//! ACP v1 compliance and feature tests, driven over raw JSON-RPC against the real binary:
//! version negotiation, auth, session persistence across a killed process, session
//! lifecycle, audio blocks, MCP servers, and the built-in stem separator.

mod common;

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use common::{BASE_TOOL_COUNT, MODEL, MockStats, Script, Step, TEST_KEY, start_mock_llm, temp_dir};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

const BIN: &str = env!("CARGO_BIN_EXE_splitfire-agent");
const STUB: &str = env!("CARGO_BIN_EXE_mcp_stdio_stub");
const TIMEOUT: Duration = Duration::from_secs(30);

/// What the fake client does when the agent asks for permission.
#[derive(Clone, Copy)]
enum Perm {
    Allow,
    #[allow(dead_code)]
    Reject,
    RejectAlways,
}

struct Raw {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: tokio::io::Lines<BufReader<ChildStdout>>,
    next_id: u64,
    /// Notifications (`session/update` etc.), in arrival order.
    notes: Vec<Value>,
    /// Permission requests seen, as the full JSON-RPC `params`.
    permissions: Vec<Value>,
    perm: Perm,
    queued: VecDeque<Value>,
}

struct Env {
    vars: Vec<(String, String)>,
}

impl Env {
    fn new(data_dir: &Path, base_url: &str) -> Self {
        let mut e = Env { vars: Vec::new() };
        e.set("SPLITFIRE_DATA_DIR", data_dir.display().to_string());
        e.set("SPLITFIRE_BASE_URL", base_url);
        e.set("SPLITFIRE_MODEL", MODEL);
        e.set("ONDE_API_KEY", TEST_KEY);
        e.set("SPLITFIRE_MCP_DEMUCS", "off");
        // No `demucs` on PATH, wherever the tests run.
        e.set("PATH", data_dir.display().to_string());
        e
    }
    fn set(&mut self, k: &str, v: impl Into<String>) -> &mut Self {
        self.vars.retain(|(n, _)| n != k);
        self.vars.push((k.to_string(), v.into()));
        self
    }
    fn unset(&mut self, k: &str) -> &mut Self {
        self.vars.retain(|(n, _)| n != k);
        self
    }
}

impl Raw {
    fn spawn(env: &Env) -> Raw {
        let mut cmd = Command::new(BIN);
        cmd.arg("--acp")
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true);
        for (k, v) in &env.vars {
            cmd.env(k, v);
        }
        let mut child = cmd.spawn().expect("spawn agent");
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().unwrap();
        Raw {
            child,
            stdin,
            lines: BufReader::new(stdout).lines(),
            next_id: 1,
            notes: Vec::new(),
            permissions: Vec::new(),
            perm: Perm::Allow,
            queued: VecDeque::new(),
        }
    }

    async fn send(&mut self, msg: Value) {
        let stdin = self.stdin.as_mut().expect("stdin closed");
        stdin
            .write_all(format!("{msg}\n").as_bytes())
            .await
            .unwrap();
        stdin.flush().await.unwrap();
    }

    async fn notify(&mut self, method: &str, params: Value) {
        self.send(json!({"jsonrpc": "2.0", "method": method, "params": params}))
            .await;
    }

    async fn next_message(&mut self) -> Value {
        if let Some(m) = self.queued.pop_front() {
            return m;
        }
        let line = tokio::time::timeout(TIMEOUT, self.lines.next_line())
            .await
            .expect("timed out waiting for the agent")
            .unwrap()
            .expect("agent closed stdout");
        serde_json::from_str(&line).unwrap_or_else(|e| panic!("non-JSON on stdout ({e}): {line}"))
    }

    /// Send a request and wait for its response, serving agent→client requests meanwhile.
    async fn call(&mut self, method: &str, params: Value) -> Result<Value, Value> {
        let id = self.next_id;
        self.next_id += 1;
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))
            .await;
        loop {
            let msg = self.next_message().await;
            if msg.get("method").is_some() {
                if msg.get("id").is_some() {
                    self.serve(&msg).await;
                } else {
                    self.notes.push(msg);
                }
                continue;
            }
            if msg["id"] == json!(id) {
                return match msg.get("error") {
                    Some(e) => Err(e.clone()),
                    None => Ok(msg["result"].clone()),
                };
            }
        }
    }

    async fn serve(&mut self, req: &Value) {
        let id = req["id"].clone();
        let reply = match req["method"].as_str().unwrap() {
            "session/request_permission" => {
                self.permissions.push(req["params"].clone());
                match self.perm {
                    Perm::Allow => {
                        json!({"outcome": {"outcome": "selected", "optionId": "allow_once"}})
                    }
                    Perm::Reject => {
                        json!({"outcome": {"outcome": "selected", "optionId": "reject_once"}})
                    }
                    Perm::RejectAlways => {
                        json!({"outcome": {"outcome": "selected", "optionId": "reject_always"}})
                    }
                }
            }
            other => {
                self.send(json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": format!("unsupported {other}")}})).await;
                return;
            }
        };
        self.send(json!({"jsonrpc": "2.0", "id": id, "result": reply}))
            .await;
    }

    /// Read whatever the agent sends right after a response (e.g. the commands update that
    /// follows `session/new`) until it has been quiet for a moment.
    async fn settle(&mut self) {
        while let Ok(line) =
            tokio::time::timeout(Duration::from_millis(300), self.lines.next_line()).await
        {
            let Ok(Some(line)) = line else { return };
            let msg: Value = serde_json::from_str(&line).unwrap();
            if msg.get("id").is_some() && msg.get("method").is_some() {
                self.serve(&msg).await;
            } else {
                self.notes.push(msg);
            }
        }
    }

    async fn ok(&mut self, method: &str, params: Value) -> Value {
        self.call(method, params)
            .await
            .unwrap_or_else(|e| panic!("{method} failed: {e}"))
    }

    async fn initialize(&mut self) -> Value {
        self.ok(
            "initialize",
            json!({"protocolVersion": 1, "clientCapabilities": {}}),
        )
        .await
    }

    async fn new_session(&mut self, cwd: &Path, mcp: Value) -> String {
        let r = self
            .ok("session/new", json!({"cwd": cwd, "mcpServers": mcp}))
            .await;
        self.settle().await;
        r["sessionId"].as_str().unwrap().to_string()
    }

    async fn prompt(&mut self, sid: &str, blocks: Value) -> Value {
        self.ok(
            "session/prompt",
            json!({"sessionId": sid, "prompt": blocks}),
        )
        .await
    }

    async fn text_prompt(&mut self, sid: &str, text: &str) -> Value {
        self.prompt(sid, json!([{"type": "text", "text": text}]))
            .await
    }

    /// Notifications for a session update kind, e.g. `tool_call_update`.
    fn updates(&self, kind: &str) -> Vec<Value> {
        self.notes
            .iter()
            .filter(|n| {
                n["method"] == "session/update" && n["params"]["update"]["sessionUpdate"] == kind
            })
            .map(|n| n["params"]["update"].clone())
            .collect()
    }

    /// Close stdin and wait for the agent to exit on its own.
    async fn finish(mut self) {
        drop(self.stdin.take());
        let _ = tokio::time::timeout(TIMEOUT, self.child.wait())
            .await
            .expect("agent did not exit");
    }
}

fn error_code(e: &Value) -> i64 {
    e["code"].as_i64().unwrap()
}

async fn mock(steps: Vec<Step>) -> (String, Arc<MockStats>) {
    let stats = Arc::new(MockStats::default());
    let url = start_mock_llm(Script(steps), stats.clone()).await;
    (url, stats)
}

fn tool_text(update: &Value) -> String {
    update["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| c.pointer("/content/text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n")
}

fn alive(pid: u32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

async fn wait_dead(pid: u32) -> bool {
    for _ in 0..100 {
        if !alive(pid) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    false
}

fn read_pid(file: &Path) -> u32 {
    std::fs::read_to_string(file)
        .expect("stub pid file")
        .trim()
        .parse()
        .unwrap()
}

fn wav_bytes() -> Vec<u8> {
    let mut cursor = std::io::Cursor::new(Vec::new());
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 22_050,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut w = hound::WavWriter::new(&mut cursor, spec).unwrap();
    for i in 0..22_050 {
        let s = (i as f32 * 2.0 * std::f32::consts::PI * 440.0 / 22_050.0).sin();
        w.write_sample((s * 0.5 * i16::MAX as f32) as i16).unwrap();
    }
    w.finalize().unwrap();
    cursor.into_inner()
}

fn stub_server(name: &str, pid_file: Option<&Path>) -> Value {
    let env: Vec<Value> = pid_file
        .map(|p| vec![json!({"name": "STUB_PID_FILE", "value": p})])
        .unwrap_or_default();
    json!({"name": name, "command": STUB, "args": [], "env": env})
}

// ---------------------------------------------------------------------------
// initialize / auth
// ---------------------------------------------------------------------------

#[tokio::test]
async fn initialize_negotiates_v1_and_advertises_capabilities() {
    let data = temp_dir("sf-data");
    let (url, _) = mock(vec![Step::Final("x")]).await;
    let mut agent = Raw::spawn(&Env::new(&data, &url));
    // A client from the future gets v1 back, not an echo of its own version.
    let r = agent
        .ok(
            "initialize",
            json!({"protocolVersion": 99, "clientCapabilities": {"auth": {"terminal": true}}}),
        )
        .await;
    assert_eq!(r["protocolVersion"], 1);
    let caps = &r["agentCapabilities"];
    assert_eq!(caps["loadSession"], true);
    assert_eq!(caps["promptCapabilities"]["audio"], true);
    assert_eq!(caps["promptCapabilities"]["image"], true);
    assert_eq!(caps["promptCapabilities"]["embeddedContext"], true);
    let sc = &caps["sessionCapabilities"];
    for k in ["list", "resume", "close", "delete", "additionalDirectories"] {
        assert!(sc.get(k).is_some(), "sessionCapabilities.{k} missing: {sc}");
    }
    assert_eq!(r["agentInfo"]["name"], "splitfire-agent");
    assert!(
        caps["auth"].get("logout").is_some(),
        "auth.logout missing: {caps}"
    );
    // Terminal auth is offered to clients that declare they can run it...
    assert_eq!(r["authMethods"][0]["id"], "terminal-setup");
    assert_eq!(r["authMethods"][0]["args"][0], "--setup");
    agent.finish().await;

    let mut agent = Raw::spawn(&Env::new(&data, &url));
    let r = agent
        .ok(
            "initialize",
            json!({"protocolVersion": 1, "clientCapabilities": {}}),
        )
        .await;
    assert_eq!(r["protocolVersion"], 1);
    // ...and not to clients that can't.
    assert!(
        r.get("authMethods")
            .and_then(Value::as_array)
            .is_none_or(|m| m.is_empty()),
        "{r}"
    );
    agent.finish().await;
}

#[tokio::test]
async fn auth_required_then_authenticate_reloads_config() {
    let data = temp_dir("sf-data");
    let (url, _) = mock(vec![Step::Final("hello")]).await;
    let mut env = Env::new(&data, &url);
    env.unset("ONDE_API_KEY");
    let mut agent = Raw::spawn(&env);
    agent.initialize().await;
    let cwd = temp_dir("sf-work");

    // No key anywhere: session/new and session/prompt are refused with auth_required (-32000).
    let e = agent
        .call("session/new", json!({"cwd": cwd, "mcpServers": []}))
        .await
        .unwrap_err();
    assert_eq!(error_code(&e), -32000, "{e}");
    let e = agent
        .call("session/prompt", json!({"sessionId": "x", "prompt": []}))
        .await
        .unwrap_err();
    assert_eq!(error_code(&e), -32000, "{e}");
    // authenticate fails while there is still no key, and rejects unknown methods.
    let e = agent
        .call("authenticate", json!({"methodId": "terminal-setup"}))
        .await
        .unwrap_err();
    assert_eq!(error_code(&e), -32000, "{e}");
    let e = agent
        .call("authenticate", json!({"methodId": "nope"}))
        .await
        .unwrap_err();
    assert_eq!(error_code(&e), -32602, "{e}");

    // load/resume also answer auth_required, so an editor reopening a thread shows sign-in.
    let e = agent
        .call(
            "session/load",
            json!({"sessionId": "x", "cwd": cwd, "mcpServers": []}),
        )
        .await
        .unwrap_err();
    assert_eq!(error_code(&e), -32000, "{e}");

    // `--setup` stored a key in the config file; authenticate picks it up without a restart.
    std::fs::write(data.join("env"), format!("ONDE_API_KEY={TEST_KEY}\n")).unwrap();
    agent
        .ok("authenticate", json!({"methodId": "terminal-setup"}))
        .await;
    let sid = agent.new_session(&cwd, json!([])).await;
    let r = agent.text_prompt(&sid, "hi").await;
    assert_eq!(r["stopReason"], "end_turn");

    // logout removes the stored key; the agent is signed out without a restart.
    agent.ok("logout", json!({})).await;
    assert!(!data.join("env").exists());
    let e = agent
        .call("session/new", json!({"cwd": cwd, "mcpServers": []}))
        .await
        .unwrap_err();
    assert_eq!(error_code(&e), -32000, "{e}");

    // Zed's flow: terminal auth runs `--setup` in another process, then retries session/new
    // on this one without calling authenticate. The stored key must be picked up anyway.
    std::fs::write(data.join("env"), format!("ONDE_API_KEY={TEST_KEY}\n")).unwrap();
    agent.new_session(&cwd, json!([])).await;
    agent.finish().await;
}

// ---------------------------------------------------------------------------
// sessions: persistence, replay, lifecycle
// ---------------------------------------------------------------------------

#[tokio::test]
async fn sessions_survive_a_killed_process_and_replay_tool_output() {
    let data = temp_dir("sf-data");
    let work = temp_dir("sf-work");
    std::fs::write(work.join("notes.txt"), "chorus in Dm").unwrap();
    let (url, _) = mock(vec![
        Step::ToolCall {
            name: "read_file",
            arguments: json!({"path": "notes.txt"}),
        },
        Step::ToolCall {
            name: "read_file",
            arguments: json!({"path": "missing.txt"}),
        },
        Step::Final("The chorus is in Dm."),
    ])
    .await;
    let env = Env::new(&data, &url);

    let mut agent = Raw::spawn(&env);
    agent.initialize().await;
    let sid = agent.new_session(&work, json!([])).await;
    let r = agent.text_prompt(&sid, "What key is the chorus in?").await;
    assert_eq!(r["stopReason"], "end_turn");
    // Title is pushed as a session_info_update.
    let infos = agent.updates("session_info_update");
    assert!(
        infos
            .iter()
            .any(|u| u["title"] == "What key is the chorus in?"),
        "no title update: {infos:?}"
    );
    // The process dies without a graceful close.
    agent.child.kill().await.unwrap();
    drop(agent);

    let mut agent = Raw::spawn(&env);
    agent.initialize().await;
    let list = agent.ok("session/list", json!({})).await;
    let sessions = list["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 1, "{list}");
    assert_eq!(sessions[0]["sessionId"], sid.as_str());
    assert_eq!(sessions[0]["title"], "What key is the chorus in?");
    assert!(sessions[0]["updatedAt"].as_str().unwrap().ends_with('Z'));
    // cwd filter.
    let other = agent.ok("session/list", json!({"cwd": "/nowhere"})).await;
    assert!(other["sessions"].as_array().unwrap().is_empty());

    agent.notes.clear();
    agent
        .ok(
            "session/load",
            json!({"sessionId": sid, "cwd": work, "mcpServers": []}),
        )
        .await;
    agent.settle().await;
    let user = agent.updates("user_message_chunk");
    assert_eq!(user[0]["content"]["text"], "What key is the chorus in?");
    let said: String = agent
        .updates("agent_message_chunk")
        .iter()
        .filter_map(|u| u["content"]["text"].as_str())
        .collect();
    assert!(said.contains("The chorus is in Dm."), "{said}");
    let calls = agent.updates("tool_call");
    assert_eq!(calls.len(), 2, "{calls:?}");
    assert_eq!(calls[0]["status"], "completed");
    assert!(
        tool_text(&calls[0]).contains("chorus in Dm"),
        "{}",
        calls[0]
    );
    assert_eq!(
        calls[1]["status"], "failed",
        "failed call must replay as failed: {}",
        calls[1]
    );
    assert!(
        calls[0]["locations"][0]["path"]
            .as_str()
            .unwrap()
            .ends_with("notes.txt")
    );
    // Commands are advertised after the load response.
    let cmds = agent.updates("available_commands_update");
    assert!(!cmds.is_empty());
    agent.finish().await;
}

#[tokio::test]
async fn resume_close_delete_and_unknown_sessions() {
    let data = temp_dir("sf-data");
    let work = temp_dir("sf-work");
    let work2 = temp_dir("sf-work");
    let (url, stats) = mock(vec![Step::Final("ok")]).await;
    let env = Env::new(&data, &url);
    let mut agent = Raw::spawn(&env);
    agent.initialize().await;

    let sid = agent.new_session(&work, json!([])).await;
    agent.text_prompt(&sid, "first").await;

    // Unknown sessions are invalid_params where nothing can be reopened. load/resume of a
    // well-formed id reopen it instead (covered below); a malformed id can't name a session.
    for (method, params) in [
        (
            "session/load",
            json!({"sessionId": "../nope", "cwd": work, "mcpServers": []}),
        ),
        (
            "session/resume",
            json!({"sessionId": "../nope", "cwd": work, "mcpServers": []}),
        ),
        (
            "session/load",
            json!({"sessionId": "relative-cwd", "cwd": "work", "mcpServers": []}),
        ),
        ("session/new", json!({"cwd": "work", "mcpServers": []})),
        ("session/close", json!({"sessionId": "nope"})),
        ("session/list", json!({"cursor": "stale"})),
        (
            "session/prompt",
            json!({"sessionId": "nope", "prompt": [{"type": "text", "text": "x"}]}),
        ),
        (
            "session/set_config_option",
            json!({"sessionId": "nope", "configId": "model", "value": "other-model"}),
        ),
    ] {
        let e = agent.call(method, params).await.unwrap_err();
        assert_eq!(error_code(&e), -32602, "{method}: {e}");
    }

    // Config options: model picker lists the endpoint's models; bad ids are rejected.
    let created = agent
        .ok("session/new", json!({"cwd": work, "mcpServers": []}))
        .await;
    let opts = created["configOptions"].as_array().unwrap();
    assert_eq!(opts[0]["id"], "model");
    assert_eq!(opts[0]["category"], "model");
    assert_eq!(opts[0]["currentValue"], MODEL);
    let values: Vec<&str> = opts[0]["options"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["value"].as_str().unwrap())
        .collect();
    assert!(
        values.contains(&MODEL) && values.contains(&"other-model"),
        "{values:?}"
    );
    let sid2 = created["sessionId"].as_str().unwrap().to_string();
    let e = agent
        .call(
            "session/set_config_option",
            json!({"sessionId": sid2, "configId": "model", "value": "bogus"}),
        )
        .await
        .unwrap_err();
    assert_eq!(error_code(&e), -32602, "{e}");
    let e = agent
        .call(
            "session/set_config_option",
            json!({"sessionId": sid2, "configId": "temperature", "value": "x"}),
        )
        .await
        .unwrap_err();
    assert_eq!(error_code(&e), -32602, "{e}");
    let r = agent
        .ok(
            "session/set_config_option",
            json!({"sessionId": sid2, "configId": "model", "value": "other-model"}),
        )
        .await;
    assert_eq!(r["configOptions"][0]["currentValue"], "other-model");
    agent.text_prompt(&sid2, "second").await;
    let last = stats.payloads.lock().unwrap().last().cloned().unwrap();
    assert_eq!(last["model"], "other-model");

    // Resume rebinds the cwd without replaying anything.
    agent.notes.clear();
    agent
        .ok(
            "session/resume",
            json!({"sessionId": sid, "cwd": work2, "mcpServers": []}),
        )
        .await;
    assert!(
        agent.updates("user_message_chunk").is_empty(),
        "resume must not replay"
    );
    let listed = agent.ok("session/list", json!({"cwd": work2})).await;
    assert_eq!(listed["sessions"].as_array().unwrap().len(), 1);

    // Close keeps the stored conversation; delete removes it.
    agent.ok("session/close", json!({"sessionId": sid})).await;
    let e = agent.call("session/close", json!({"sessionId": sid})).await;
    assert!(
        e.is_ok(),
        "close of a stored-only session is a no-op, not an error"
    );
    let all = agent.ok("session/list", json!({})).await;
    assert_eq!(all["sessions"].as_array().unwrap().len(), 2);
    agent.ok("session/delete", json!({"sessionId": sid})).await;
    let all = agent.ok("session/list", json!({})).await;
    assert_eq!(all["sessions"].as_array().unwrap().len(), 1);
    assert!(!data.join("sessions").join(format!("{sid}.json")).exists());
    // Delete is idempotent: a second delete, or one for a session that never existed, succeeds.
    agent.ok("session/delete", json!({"sessionId": sid})).await;
    agent
        .ok("session/delete", json!({"sessionId": "nope"}))
        .await;

    // A well-formed id with no saved history (a thread from a process that never saved it)
    // reopens empty under the same id, with a notice, instead of failing to launch.
    agent.notes.clear();
    let r = agent
        .ok(
            "session/load",
            json!({"sessionId": "lost-thread", "cwd": work, "mcpServers": []}),
        )
        .await;
    assert_eq!(r["configOptions"][0]["id"], "model");
    let notice = agent.updates("agent_message_chunk");
    assert_eq!(notice.len(), 1, "{notice:?}");
    assert!(
        notice[0]["content"]["text"]
            .as_str()
            .unwrap()
            .contains("starts fresh"),
        "{notice:?}"
    );
    assert!(notice[0]["messageId"].is_string());
    agent.text_prompt("lost-thread", "hello again").await;
    agent.finish().await;
}

#[tokio::test]
async fn streaming_reports_message_ids_usage_and_inlines_file_links() {
    let data = temp_dir("sf-data");
    let work = temp_dir("sf-work");
    std::fs::write(
        work.join("chart.md"),
        "Verse: Am F C G\nChorus: F G Em Am\nBridge: Dm E\n",
    )
    .unwrap();
    std::fs::write(work.join("take.wav"), wav_bytes()).unwrap();
    let (url, stats) = mock(vec![Step::Final("Looks like A minor.")]).await;
    let mut env = Env::new(&data, &url);
    env.set("SPLITFIRE_CONTEXT_WINDOW", "1000");
    let mut agent = Raw::spawn(&env);
    agent.initialize().await;
    let sid = agent.new_session(&work, json!([])).await;
    let chart = format!("file://{}?column=1#L2:2", work.join("chart.md").display());
    let take = format!("file://{}", work.join("take.wav").display());
    agent
        .prompt(
            &sid,
            json!([
                {"type": "text", "text": "What key is the chorus in?"},
                {"type": "resource_link", "name": "chart.md", "uri": chart},
                {"type": "resource_link", "name": "take.wav", "uri": take},
            ]),
        )
        .await;

    // The selection link is inlined as just that line; the audio link points at the analyzer.
    let payload = stats.payloads.lock().unwrap()[0].clone();
    assert!(payload["stream_options"]["include_usage"] == true);
    let user = payload["messages"].as_array().unwrap().last().unwrap()["content"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(user.contains("Chorus: F G Em Am"), "{user}");
    assert!(
        !user.contains("Verse:") && !user.contains("Bridge:"),
        "{user}"
    );
    assert!(
        user.contains("Referenced audio file") && user.contains("analyze_audio"),
        "{user}"
    );

    // Every chunk of one assistant message carries the same message id.
    let chunks = agent.updates("agent_message_chunk");
    assert!(!chunks.is_empty());
    assert!(
        chunks.iter().all(|c| c["messageId"].is_string()),
        "{chunks:?}"
    );
    // Usage from the endpoint, sized by SPLITFIRE_CONTEXT_WINDOW.
    let usage = agent.updates("usage_update");
    assert_eq!(usage.len(), 1, "{usage:?}");
    assert_eq!(
        (usage[0]["used"].as_u64(), usage[0]["size"].as_u64()),
        (Some(150), Some(1000))
    );
    agent.finish().await;
}

#[tokio::test]
async fn permissions_reject_always_raw_output_and_auto_approve() {
    let data = temp_dir("sf-data");
    let work = temp_dir("sf-work");
    let write = |path: &'static str| Step::ToolCall {
        name: "write_file",
        arguments: json!({"path": path, "content": "x"}),
    };
    let (url, _) = mock(vec![write("a.txt"), write("b.txt"), Step::Final("done")]).await;
    let mut agent = Raw::spawn(&Env::new(&data, &url));
    // A client that supports boolean config options gets the auto-approve toggle.
    let init = agent
        .ok(
            "initialize",
            json!({"protocolVersion": 1, "clientCapabilities": {"session": {"configOptions": {"boolean": {}}}}}),
        )
        .await;
    assert_eq!(init["protocolVersion"], 1);
    let created = agent
        .ok("session/new", json!({"cwd": work, "mcpServers": []}))
        .await;
    agent.settle().await;
    let sid = created["sessionId"].as_str().unwrap().to_string();
    let opts = created["configOptions"].as_array().unwrap();
    assert_eq!(opts[1]["id"], "auto_approve");
    assert_eq!(opts[1]["type"], "boolean");
    assert_eq!(opts[1]["currentValue"], false);

    // "Always reject" is offered, and once chosen the tool is refused without asking again.
    agent.perm = Perm::RejectAlways;
    agent.text_prompt(&sid, "write two files").await;
    assert_eq!(
        agent.permissions.len(),
        1,
        "asked again after reject_always"
    );
    let kinds: Vec<&str> = agent.permissions[0]["options"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["kind"].as_str().unwrap())
        .collect();
    assert!(kinds.contains(&"reject_always"), "{kinds:?}");
    assert!(!work.join("a.txt").exists() && !work.join("b.txt").exists());
    // Tool calls carry the tool name; updates carry the raw output the model saw.
    let calls = agent.updates("tool_call");
    assert!(calls.iter().all(|c| c["name"] == "write_file"), "{calls:?}");
    let done = agent.updates("tool_call_update");
    let failed: Vec<&Value> = done.iter().filter(|u| u["status"] == "failed").collect();
    assert_eq!(failed.len(), 2, "{done:?}");
    assert_eq!(failed[0]["rawOutput"]["failed"], true);
    assert!(
        failed[0]["rawOutput"]["output"]
            .as_str()
            .unwrap()
            .contains("rejected")
    );

    // Turning auto-approve on is broadcast as a config_option_update and skips the prompts.
    agent.notes.clear();
    let r = agent
        .ok(
            "session/set_config_option",
            json!({"sessionId": sid, "configId": "auto_approve", "type": "boolean", "value": true}),
        )
        .await;
    assert_eq!(r["configOptions"][1]["currentValue"], true);
    let broadcast = agent.updates("config_option_update");
    assert_eq!(broadcast.len(), 1, "{broadcast:?}");
    let e = agent
        .call(
            "session/set_config_option",
            json!({"sessionId": sid, "configId": "auto_approve", "value": "yes"}),
        )
        .await
        .unwrap_err();
    assert_eq!(error_code(&e), -32602, "{e}");

    // A fresh session (no reject_always grant) now writes without asking.
    let sid2 = agent.new_session(&work, json!([])).await;
    agent
        .ok(
            "session/set_config_option",
            json!({"sessionId": sid2, "configId": "auto_approve", "type": "boolean", "value": true}),
        )
        .await;
    agent.permissions.clear();
    agent.text_prompt(&sid2, "write two files").await;
    assert!(agent.permissions.is_empty(), "{:?}", agent.permissions);
    assert!(work.join("a.txt").exists() && work.join("b.txt").exists());
    agent.finish().await;
}

#[tokio::test]
async fn auto_approve_is_hidden_from_clients_without_boolean_options() {
    let data = temp_dir("sf-data");
    let work = temp_dir("sf-work");
    let (url, _) = mock(vec![Step::Final("x")]).await;
    let mut agent = Raw::spawn(&Env::new(&data, &url));
    agent.initialize().await;
    let created = agent
        .ok("session/new", json!({"cwd": work, "mcpServers": []}))
        .await;
    agent.settle().await;
    let opts = created["configOptions"].as_array().unwrap();
    assert_eq!(opts.len(), 1, "{opts:?}");
    let e = agent
        .call(
            "session/set_config_option",
            json!({"sessionId": created["sessionId"], "configId": "auto_approve", "type": "boolean", "value": true}),
        )
        .await
        .unwrap_err();
    assert_eq!(error_code(&e), -32602, "{e}");
    agent.finish().await;
}

#[tokio::test]
async fn content_filter_is_a_refusal_and_private_flags_stay_off_the_wire() {
    let data = temp_dir("sf-data");
    let work = temp_dir("sf-work");
    let (url, stats) = mock(vec![
        Step::ToolCall {
            name: "read_file",
            arguments: json!({"path": "missing.txt"}),
        },
        Step::ContentFilter,
    ])
    .await;
    let mut agent = Raw::spawn(&Env::new(&data, &url));
    agent.initialize().await;
    let sid = agent.new_session(&work, json!([])).await;
    let r = agent.text_prompt(&sid, "read it").await;
    assert_eq!(r["stopReason"], "refusal");
    let payloads = stats.payloads.lock().unwrap().clone();
    let second = payloads.last().unwrap()["messages"]
        .as_array()
        .unwrap()
        .clone();
    let tool = second.iter().find(|m| m["role"] == "tool").unwrap();
    assert!(
        tool["content"].as_str().unwrap().starts_with("Error:"),
        "{tool}"
    );
    assert!(
        tool.get("x_failed").is_none(),
        "private field leaked to the model: {tool}"
    );
    // The system prompt is rebuilt each turn and never stored with the history.
    assert_eq!(second[0]["role"], "system");
    let stored: Value = serde_json::from_slice(
        &std::fs::read(data.join("sessions").join(format!("{sid}.json"))).unwrap(),
    )
    .unwrap();
    assert!(
        stored["messages"]
            .as_array()
            .unwrap()
            .iter()
            .all(|m| m["role"] != "system")
    );
    agent.finish().await;
}

#[tokio::test]
async fn slash_commands_are_advertised_and_expanded() {
    let data = temp_dir("sf-data");
    let work = temp_dir("sf-work");
    let (url, stats) = mock(vec![Step::Final("ok")]).await;
    let mut agent = Raw::spawn(&Env::new(&data, &url));
    agent.initialize().await;
    let sid = agent.new_session(&work, json!([])).await;
    let cmds = agent.updates("available_commands_update");
    let names: Vec<&str> = cmds[0]["availableCommands"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        ["analyze", "chords", "library", "practice"],
        "no /stems without a separator"
    );

    agent.text_prompt(&sid, "/chords Dm7 G7 Cmaj7").await;
    let payload = stats.payloads.lock().unwrap()[0].clone();
    let msgs = payload["messages"].as_array().unwrap();
    let system = msgs[0]["content"].as_str().unwrap();
    // Disconnected: the separation tool is not offered, but the prompt says how to enable it.
    assert!(
        !system.contains("mcp__demucs__separate_stems"),
        "separation tool offered while disconnected"
    );
    assert!(
        system.contains("SPLITFIRE_DEMUCS_BIN"),
        "no hint on enabling stems"
    );
    assert!(
        system.contains("Co-Authored-By: SplitFire v")
            && system.contains("-acp <noreply@splitfire.ai>")
    );
    let user = msgs.last().unwrap()["content"].as_str().unwrap();
    assert!(
        user.contains("theory_progression") && user.contains("Dm7 G7 Cmaj7"),
        "{user}"
    );
    assert_eq!(payload["tools"].as_array().unwrap().len(), BASE_TOOL_COUNT);
    agent.finish().await;
}

// ---------------------------------------------------------------------------
// audio
// ---------------------------------------------------------------------------

#[tokio::test]
async fn audio_blocks_are_saved_to_scratch_and_cleaned_on_close() {
    use base64::Engine;
    let data = temp_dir("sf-data");
    let work = temp_dir("sf-work");
    let (url, stats) = mock(vec![Step::Final("heard it")]).await;
    let mut agent = Raw::spawn(&Env::new(&data, &url));
    agent.initialize().await;
    let sid = agent.new_session(&work, json!([])).await;
    let b64 = base64::engine::general_purpose::STANDARD.encode(wav_bytes());
    let r = agent
        .prompt(
            &sid,
            json!([
                {"type": "text", "text": "what is this?"},
                {"type": "audio", "data": b64, "mimeType": "audio/wav"}
            ]),
        )
        .await;
    assert_eq!(r["stopReason"], "end_turn");
    let payload = stats.payloads.lock().unwrap()[0].clone();
    let user = payload["messages"].as_array().unwrap().last().unwrap()["content"]
        .as_str()
        .unwrap()
        .to_string();
    let scratch = data.join("scratch").join(&sid);
    let saved: Vec<PathBuf> = std::fs::read_dir(&scratch)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(
        saved.len(),
        1,
        "one scratch file expected in {}",
        scratch.display()
    );
    assert!(
        user.contains(&saved[0].display().to_string()),
        "model must see the path: {user}"
    );
    assert_eq!(std::fs::read(&saved[0]).unwrap(), wav_bytes());
    agent.ok("session/close", json!({"sessionId": sid})).await;
    assert!(!scratch.exists(), "scratch dir must be removed on close");
    agent.finish().await;
}

#[tokio::test]
async fn analyze_audio_runs_through_the_agent() {
    let data = temp_dir("sf-data");
    let work = temp_dir("sf-work");
    std::fs::write(work.join("tone.wav"), wav_bytes()).unwrap();
    let (url, stats) = mock(vec![
        Step::ToolCall {
            name: "analyze_audio",
            arguments: json!({"path": "tone.wav"}),
        },
        Step::Final("done"),
    ])
    .await;
    let mut agent = Raw::spawn(&Env::new(&data, &url));
    agent.initialize().await;
    let sid = agent.new_session(&work, json!([])).await;
    agent.text_prompt(&sid, "analyze tone.wav").await;
    assert!(
        agent.permissions.is_empty(),
        "read-only analysis must not prompt"
    );
    let calls = agent.updates("tool_call");
    assert_eq!(calls[0]["kind"], "read");
    assert!(
        calls[0]["locations"][0]["path"]
            .as_str()
            .unwrap()
            .ends_with("tone.wav")
    );
    let payloads = stats.payloads.lock().unwrap().clone();
    let tool = payloads[1]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["role"] == "tool")
        .unwrap()
        .clone();
    let text = tool["content"].as_str().unwrap();
    assert!(
        text.contains("22050 Hz") && text.contains("Peak:") && text.contains("estimate"),
        "{text}"
    );
    agent.finish().await;
}

#[tokio::test]
async fn theory_tools_are_ungated_and_exact() {
    let data = temp_dir("sf-data");
    let work = temp_dir("sf-work");
    let (url, stats) = mock(vec![
        Step::ToolCall {
            name: "theory_scale",
            arguments: json!({"root": "C#", "scale": "major"}),
        },
        Step::Final("done"),
    ])
    .await;
    let mut agent = Raw::spawn(&Env::new(&data, &url));
    agent.initialize().await;
    let sid = agent.new_session(&work, json!([])).await;
    agent.text_prompt(&sid, "C# major scale?").await;
    assert!(agent.permissions.is_empty());
    let calls = agent.updates("tool_call");
    assert_eq!(calls[0]["kind"], "think");
    let payloads = stats.payloads.lock().unwrap().clone();
    let tool = payloads[1]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["role"] == "tool")
        .unwrap()
        .clone();
    assert!(
        tool["content"]
            .as_str()
            .unwrap()
            .contains("C# D# E# F# G# A# B#")
    );
    agent.finish().await;
}

// ---------------------------------------------------------------------------
// MCP
// ---------------------------------------------------------------------------

#[tokio::test]
async fn client_supplied_mcp_server_is_offered_gated_and_streams_progress() {
    let data = temp_dir("sf-data");
    let work = temp_dir("sf-work");
    let pid_file = data.join("stub.pid");
    let (url, stats) = mock(vec![
        Step::ToolCall {
            name: "mcp__stub__echo",
            arguments: json!({"text": "hi"}),
        },
        Step::ToolCall {
            name: "mcp__stub__slow_progress",
            arguments: json!({}),
        },
        Step::Final("all done"),
    ])
    .await;
    let mut agent = Raw::spawn(&Env::new(&data, &url));
    agent.initialize().await;
    let sid = agent
        .new_session(&work, json!([stub_server("stub", Some(&pid_file))]))
        .await;
    let r = agent.text_prompt(&sid, "use the stub").await;
    assert_eq!(r["stopReason"], "end_turn");

    // Offered: the namespaced tools ride along with the built-ins.
    let payload = stats.payloads.lock().unwrap()[0].clone();
    let names: Vec<String> = payload["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["function"]["name"].as_str().unwrap().to_string())
        .collect();
    assert!(names.contains(&"mcp__stub__echo".to_string()), "{names:?}");
    assert_eq!(names.len(), BASE_TOOL_COUNT + 5, "{names:?}");

    // Gated: only the tool without readOnlyHint asked for permission.
    assert_eq!(agent.permissions.len(), 1, "{:?}", agent.permissions);
    let perm = &agent.permissions[0];
    assert_eq!(perm["toolCall"]["toolCallId"], "call_2");

    // Callable, with the result fed back to the model.
    let payloads = stats.payloads.lock().unwrap().clone();
    let tools: Vec<&Value> = payloads[1]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["role"] == "tool")
        .collect();
    assert_eq!(tools[0]["content"], "echo: hi");

    // Progress: MCP notifications/progress became tool_call_update content.
    let progress: Vec<String> = agent
        .updates("tool_call_update")
        .iter()
        .filter(|u| u["toolCallId"] == "call_2")
        .map(tool_text)
        .collect();
    assert!(
        progress.iter().any(|t| t == "halfway (50%)"),
        "{progress:?}"
    );
    let done = agent
        .updates("tool_call_update")
        .into_iter()
        .rfind(|u| u["toolCallId"] == "call_2")
        .unwrap();
    assert_eq!(done["status"], "completed");

    // Children are killed when the session closes.
    let pid = read_pid(&pid_file);
    assert!(alive(pid));
    agent.ok("session/close", json!({"sessionId": sid})).await;
    assert!(
        wait_dead(pid).await,
        "stub server still running after session/close"
    );
    agent.finish().await;
}

#[tokio::test]
async fn mcp_children_die_when_the_agent_exits() {
    let data = temp_dir("sf-data");
    let work = temp_dir("sf-work");
    let pid_file = data.join("stub.pid");
    let (url, _) = mock(vec![Step::Final("x")]).await;
    let mut agent = Raw::spawn(&Env::new(&data, &url));
    agent.initialize().await;
    agent
        .new_session(&work, json!([stub_server("stub", Some(&pid_file))]))
        .await;
    let pid = read_pid(&pid_file);
    assert!(alive(pid));
    agent.finish().await;
    assert!(wait_dead(pid).await, "stub server outlived the agent");
}

#[tokio::test]
async fn broken_mcp_server_is_not_fatal() {
    let data = temp_dir("sf-data");
    let work = temp_dir("sf-work");
    let (url, stats) = mock(vec![Step::Final("fine")]).await;
    let mut agent = Raw::spawn(&Env::new(&data, &url));
    agent.initialize().await;
    let sid = agent
        .new_session(
            &work,
            json!([
                {"name": "ghost", "command": "/definitely/not/here", "args": [], "env": []},
                {"name": "http", "type": "http", "url": "http://127.0.0.1:1/mcp", "headers": []}
            ]),
        )
        .await;
    let r = agent.text_prompt(&sid, "hello").await;
    assert_eq!(r["stopReason"], "end_turn");
    let payload = stats.payloads.lock().unwrap()[0].clone();
    assert_eq!(payload["tools"].as_array().unwrap().len(), BASE_TOOL_COUNT);
    agent.finish().await;
}

#[tokio::test]
async fn cancelling_a_turn_cancels_the_mcp_call() {
    let data = temp_dir("sf-data");
    let work = temp_dir("sf-work");
    let (url, _) = mock(vec![
        Step::ToolCall {
            name: "mcp__stub__wait_for_cancel",
            arguments: json!({}),
        },
        Step::Final("should not get here"),
    ])
    .await;
    let mut agent = Raw::spawn(&Env::new(&data, &url));
    agent.initialize().await;
    let sid = agent
        .new_session(&work, json!([stub_server("stub", None)]))
        .await;
    // Fire the prompt without waiting, then cancel once the call is in flight.
    let id = agent.next_id;
    agent.next_id += 1;
    agent
        .send(
            json!({"jsonrpc": "2.0", "id": id, "method": "session/prompt",
            "params": {"sessionId": sid, "prompt": [{"type": "text", "text": "wait"}]}}),
        )
        .await;
    let mut cancelled = false;
    let stop = loop {
        let msg = agent.next_message().await;
        if msg.get("method").is_some() {
            if msg.get("id").is_some() {
                agent.serve(&msg).await;
                // The permission request means the tool call is about to start.
                tokio::time::sleep(Duration::from_millis(300)).await;
                agent
                    .notify("session/cancel", json!({"sessionId": sid}))
                    .await;
                cancelled = true;
            } else {
                agent.notes.push(msg);
            }
        } else if msg["id"] == json!(id) {
            break msg["result"]["stopReason"].clone();
        }
    };
    assert!(cancelled, "the call should have asked for permission first");
    assert_eq!(stop, "cancelled");
    let last = agent
        .updates("tool_call_update")
        .into_iter()
        .last()
        .unwrap();
    assert_eq!(last["status"], "failed");
    agent.finish().await;
}

// ---------------------------------------------------------------------------
// built-in demucs
// ---------------------------------------------------------------------------

#[tokio::test]
async fn builtin_demucs_appears_only_when_configured() {
    let data = temp_dir("sf-data");
    let work = temp_dir("sf-work");
    let pid_file = data.join("stub.pid");
    let input = work.join("song.wav");
    std::fs::write(&input, wav_bytes()).unwrap();
    let (url, stats) = mock(vec![
        Step::ToolCall {
            name: "mcp__demucs__separate_stems",
            arguments: json!({"input": input}),
        },
        Step::Final("separated"),
    ])
    .await;

    // Configured: SPLITFIRE_DEMUCS_BIN points at a server speaking `demucs --mcp`.
    let mut env = Env::new(&data, &url);
    env.set("SPLITFIRE_MCP_DEMUCS", "on");
    env.set("SPLITFIRE_DEMUCS_BIN", STUB);
    env.set("STUB_PID_FILE", pid_file.display().to_string());
    let mut agent = Raw::spawn(&env);
    agent.initialize().await;
    let sid = agent.new_session(&work, json!([])).await;
    let cmds = agent.updates("available_commands_update");
    let names: Vec<&str> = cmds[0]["availableCommands"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"stems"), "{names:?}");
    let r = agent.text_prompt(&sid, "split song.wav").await;
    assert_eq!(r["stopReason"], "end_turn");
    let payload = stats.payloads.lock().unwrap()[0].clone();
    let system = payload["messages"][0]["content"].as_str().unwrap();
    assert!(system.contains("mcp__demucs__separate_stems"));
    let tool_names: Vec<&str> = payload["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["function"]["name"].as_str().unwrap())
        .collect();
    assert!(
        tool_names.contains(&"mcp__demucs__separate_stems")
            && tool_names.contains(&"mcp__demucs__list_models")
    );

    // The permission prompt states the download size from list_models.
    assert_eq!(agent.permissions.len(), 1);
    let prompt_text = agent.permissions[0]["toolCall"]["content"][0]["content"]["text"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(prompt_text.contains("80 MB"), "{prompt_text}");
    assert!(prompt_text.contains("song.wav"), "{prompt_text}");

    // Stem paths become tool-call locations.
    let update = agent
        .updates("tool_call_update")
        .into_iter()
        .rfind(|u| u["status"] == "completed")
        .unwrap();
    let locs: Vec<&str> = update["locations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["path"].as_str().unwrap())
        .collect();
    assert_eq!(locs.len(), 4, "{update}");
    assert!(
        locs.iter()
            .all(|p| Path::new(p).is_absolute() && p.ends_with(".wav"))
    );

    // One demucs per process: a second session shares it.
    let pid = read_pid(&pid_file);
    agent.new_session(&work, json!([])).await;
    assert_eq!(read_pid(&pid_file), pid);
    agent.finish().await;
    assert!(wait_dead(pid).await, "demucs outlived the agent");

    // Unset (no binary, nothing on PATH): no tools, no /stems, and the prompt offers no
    // separation tool, only how to enable it.
    for mode in ["off", "on"] {
        let mut env = Env::new(&data, &url);
        env.set("SPLITFIRE_MCP_DEMUCS", mode);
        let (url2, stats2) = mock(vec![Step::Final("x")]).await;
        env.set("SPLITFIRE_BASE_URL", url2);
        let mut agent = Raw::spawn(&env);
        agent.initialize().await;
        let sid = agent.new_session(&work, json!([])).await;
        let cmds = agent.updates("available_commands_update");
        assert!(!cmds[0]["availableCommands"].to_string().contains("stems"));
        agent.text_prompt(&sid, "hi").await;
        let payload = stats2.payloads.lock().unwrap()[0].clone();
        assert_eq!(
            payload["tools"].as_array().unwrap().len(),
            BASE_TOOL_COUNT,
            "mode {mode}"
        );
        let system = payload["messages"][0]["content"].as_str().unwrap();
        assert!(!system.contains("mcp__demucs__"), "mode {mode}");
        assert!(system.contains("SPLITFIRE_DEMUCS_BIN"), "mode {mode}");
        agent.finish().await;
    }
}

#[tokio::test]
async fn client_supplied_demucs_wins_over_the_builtin() {
    let data = temp_dir("sf-data");
    let work = temp_dir("sf-work");
    let builtin_pid = data.join("builtin.pid");
    let client_pid = data.join("client.pid");
    let (url, _) = mock(vec![Step::Final("x")]).await;
    let mut env = Env::new(&data, &url);
    env.set("SPLITFIRE_MCP_DEMUCS", "on");
    env.set("SPLITFIRE_DEMUCS_BIN", STUB);
    env.set("STUB_PID_FILE", builtin_pid.display().to_string());
    let mut agent = Raw::spawn(&env);
    agent.initialize().await;
    agent
        .new_session(&work, json!([stub_server("demucs", Some(&client_pid))]))
        .await;
    assert!(client_pid.exists(), "the client's server should run");
    assert!(
        !builtin_pid.exists(),
        "the built-in must not start when the client supplies demucs"
    );
    agent.finish().await;
}

// ---------------------------------------------------------------------------
// Real demucs (opt-in)
// ---------------------------------------------------------------------------

/// End to end with a real `demucs --mcp`. Skipped unless `SPLITFIRE_E2E_DEMUCS_BIN` points at a
/// demucs binary (model weights are downloaded on first use):
/// `SPLITFIRE_E2E_DEMUCS_BIN=$(which demucs) cargo test --test acp_compliance real_demucs -- --nocapture`
#[tokio::test]
async fn real_demucs_end_to_end() {
    let Some(bin) = std::env::var("SPLITFIRE_E2E_DEMUCS_BIN")
        .ok()
        .filter(|b| !b.is_empty())
    else {
        eprintln!("skipping: SPLITFIRE_E2E_DEMUCS_BIN not set");
        return;
    };
    let data = temp_dir("sf-data");
    let work = temp_dir("sf-work");
    // Four seconds of stereo 44.1 kHz: a low note plus a decaying high one.
    let input = work.join("short.wav");
    let spec = hound::WavSpec {
        channels: 2,
        sample_rate: 44_100,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut w = hound::WavWriter::create(&input, spec).unwrap();
    for i in 0..44_100 * 4 {
        let t = i as f32 / 44_100.0;
        let s = 0.3 * (2.0 * std::f32::consts::PI * 110.0 * t).sin()
            + 0.2 * (2.0 * std::f32::consts::PI * 1_320.0 * t).sin() * (-(t % 0.5) * 8.0).exp();
        let v = (s * i16::MAX as f32) as i16;
        w.write_sample(v).unwrap();
        w.write_sample(v).unwrap();
    }
    w.finalize().unwrap();

    let (url, stats) = mock(vec![
        Step::ToolCall {
            name: "mcp__demucs__separate_stems",
            arguments: json!({"input": input}),
        },
        Step::Final("separated"),
    ])
    .await;
    let mut env = Env::new(&data, &url);
    env.set("SPLITFIRE_MCP_DEMUCS", "on");
    env.set("SPLITFIRE_DEMUCS_BIN", bin);
    let mut agent = Raw::spawn(&env);
    agent.initialize().await;
    let sid = agent.new_session(&work, json!([])).await;
    let r = agent.text_prompt(&sid, "/stems short.wav").await;
    assert_eq!(r["stopReason"], "end_turn");
    let prompt_text = agent.permissions[0]["toolCall"]["content"][0]["content"]["text"]
        .as_str()
        .unwrap()
        .to_string();
    eprintln!("permission prompt: {prompt_text}");
    let updates = agent.updates("tool_call_update");
    let progress: Vec<String> = updates
        .iter()
        .map(tool_text)
        .filter(|t| t.contains('%'))
        .collect();
    eprintln!("progress: {progress:?}");
    assert!(!progress.is_empty(), "no progress reached the tool call");
    let done = updates
        .iter()
        .rev()
        .find(|u| u["status"] == "completed")
        .expect("completed update");
    let paths: Vec<&str> = done["locations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["path"].as_str().unwrap())
        .collect();
    assert_eq!(paths.len(), 4, "{done}");
    for p in &paths {
        assert!(Path::new(p).is_file(), "missing stem {p}");
    }
    let payloads = stats.payloads.lock().unwrap().clone();
    let tool = payloads[1]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["role"] == "tool")
        .unwrap()
        .clone();
    eprintln!("model saw: {}", tool["content"]);
    agent.finish().await;
}

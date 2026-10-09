//! SplitFire's ACP tests over raw JSON-RPC against the real binary. The protocol itself (version
//! negotiation, auth, sessions, streaming, approval, MCP) is covered by `ed-acp-testkit`'s
//! conformance suite, run here through [`ed_acp_testkit::conformance_suite!`]. The rest are
//! SplitFire's own: slash commands, audio, the music tools and the built-in stem separator.

mod common;

use std::path::{Path, PathBuf};

use ed_acp_testkit::{Raw, Step, mock, read_pid, stub_server, temp_dir, tool_text, wait_dead};
use common::{BASE_TOOL_COUNT, target};
use serde_json::json;

ed_acp_testkit::conformance_suite!(target);

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

/// Audio links never reach the model as text to read; SplitFire points it at `analyze_audio`.
#[tokio::test]
async fn audio_links_point_the_model_at_the_analyzer() {
    let data = temp_dir("sf-data");
    let work = temp_dir("sf-work");
    std::fs::write(work.join("take.wav"), wav_bytes()).unwrap();
    let (url, stats) = mock(vec![Step::Final("ok")]).await;
    let mut agent = Raw::spawn(&target().env(&data, &url));
    agent.initialize().await;
    let sid = agent.new_session(&work, json!([])).await;
    let take = format!("file://{}", work.join("take.wav").display());
    agent
        .prompt(
            &sid,
            json!([
                {"type": "text", "text": "What key is this take in?"},
                {"type": "resource_link", "name": "take.wav", "uri": take},
            ]),
        )
        .await;
    let payload = stats.payloads.lock().unwrap()[0].clone();
    let user = payload["messages"].as_array().unwrap().last().unwrap()["content"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        user.contains("Referenced audio file") && user.contains("analyze_audio"),
        "{user}"
    );
    agent.finish().await;
}

#[tokio::test]
async fn slash_commands_are_advertised_and_expanded() {
    let data = temp_dir("sf-data");
    let work = temp_dir("sf-work");
    let (url, stats) = mock(vec![Step::Final("ok")]).await;
    let mut agent = Raw::spawn(&target().env(&data, &url));
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
    let mut agent = Raw::spawn(&target().env(&data, &url));
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
    let mut agent = Raw::spawn(&target().env(&data, &url));
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
    let mut agent = Raw::spawn(&target().env(&data, &url));
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
    let mut env = target().env(&data, &url);
    env.set("SPLITFIRE_MCP_DEMUCS", "on");
    env.set(
        "SPLITFIRE_DEMUCS_BIN",
        target().mcp_stub.display().to_string(),
    );
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
        let mut env = target().env(&data, &url);
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
    let mut env = target().env(&data, &url);
    env.set("SPLITFIRE_MCP_DEMUCS", "on");
    env.set(
        "SPLITFIRE_DEMUCS_BIN",
        target().mcp_stub.display().to_string(),
    );
    env.set("STUB_PID_FILE", builtin_pid.display().to_string());
    let mut agent = Raw::spawn(&env);
    agent.initialize().await;
    agent
        .new_session(
            &work,
            json!([stub_server(&target().mcp_stub, "demucs", Some(&client_pid))]),
        )
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
    let mut env = target().env(&data, &url);
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

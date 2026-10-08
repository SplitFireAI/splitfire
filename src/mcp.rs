//! MCP client over stdio (rmcp): servers supplied by the ACP client in `session/new|load|resume`,
//! plus the optional built-in `demucs --mcp` stem-separation server.
//!
//! Tools are exposed to the model as `mcp__<server>__<tool>`. A failing server is never fatal:
//! it is logged and its tools are simply absent.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use agent_client_protocol::schema::v1::{McpServer, ToolCallContent, ToolCallUpdateFields};
use anyhow::{Context, Result, anyhow, bail};
use futures::StreamExt;
use rmcp::handler::client::progress::ProgressDispatcher;
use rmcp::model::{
    CallToolRequestParams, ClientCapabilities, ClientRequest, ContentBlock, Implementation,
    ProgressNotificationParam, Request, ServerResult, Tool,
};
use rmcp::service::{NotificationContext, PeerRequestOptions, RunningService};
use rmcp::transport::TokioChildProcess;
use rmcp::{ClientHandler, RoleClient, ServiceExt};
use serde_json::{Map, Value, json};
use tokio::sync::Mutex;

use crate::tools::{ToolCtx, ToolOutcome, absolutize, function_def, truncate};

const PREFIX: &str = "mcp__";
/// Server name of the built-in stem separator; a client-supplied server of this name wins.
pub const DEMUCS: &str = "demucs";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
/// Upper bound for one tool call: separation of a long track can take many minutes.
const CALL_TIMEOUT: Duration = Duration::from_secs(60 * 60);
const MAX_TOOL_NAME: usize = 64;

/// Split `mcp__<server>__<tool>` into its parts.
pub fn split_qualified(name: &str) -> Option<(&str, &str)> {
    let rest = name.strip_prefix(PREFIX)?;
    let (server, tool) = rest.split_once("__")?;
    (!server.is_empty() && !tool.is_empty()).then_some((server, tool))
}

fn sanitize(part: &str) -> String {
    let mut out: String = part
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    while out.contains("__") {
        out = out.replace("__", "_");
    }
    out.trim_matches('_').to_string()
}

fn qualified(server: &str, tool: &str) -> String {
    let mut name = format!("{PREFIX}{}__{}", sanitize(server), sanitize(tool));
    name.truncate(MAX_TOOL_NAME);
    name
}

struct McpClient {
    progress: ProgressDispatcher,
}

impl ClientHandler for McpClient {
    async fn on_progress(
        &self,
        params: ProgressNotificationParam,
        _context: NotificationContext<RoleClient>,
    ) {
        self.progress.handle_notification(params).await;
    }

    fn get_info(&self) -> rmcp::model::ClientConfig {
        rmcp::model::ClientConfig::new(
            ClientCapabilities::default(),
            Implementation::new("splitfire-agent", env!("CARGO_PKG_VERSION")),
        )
    }
}

/// One live connection to an MCP server process.
pub struct McpConn {
    name: String,
    service: RunningService<RoleClient, McpClient>,
    tools: Vec<Tool>,
}

impl McpConn {
    async fn connect(
        name: &str,
        command: &Path,
        args: &[String],
        env: &[(String, String)],
    ) -> Result<McpConn> {
        let mut cmd = tokio::process::Command::new(command);
        cmd.args(args).envs(env.iter().map(|(k, v)| (k, v)));
        let transport = TokioChildProcess::new(cmd)
            .with_context(|| format!("starting {}", command.display()))?;
        let client = McpClient {
            progress: ProgressDispatcher::new(),
        };
        let service = tokio::time::timeout(CONNECT_TIMEOUT, client.serve(transport))
            .await
            .map_err(|_| anyhow!("no MCP handshake within {}s", CONNECT_TIMEOUT.as_secs()))?
            .map_err(|e| anyhow!("MCP handshake failed: {e}"))?;
        let tools = tokio::time::timeout(CONNECT_TIMEOUT, service.peer().list_all_tools())
            .await
            .map_err(|_| anyhow!("tools/list timed out"))?
            .map_err(|e| anyhow!("tools/list failed: {e}"))?;
        Ok(McpConn {
            name: name.to_string(),
            service,
            tools,
        })
    }

    async fn close(mut self) {
        let _ = self
            .service
            .close_with_timeout(Duration::from_secs(2))
            .await;
    }
}

struct Entry {
    conn: Arc<McpConn>,
    raw: String,
    description: String,
    schema: Value,
    read_only: bool,
}

/// The MCP tools available to one session.
#[derive(Default)]
pub struct McpToolset {
    tools: HashMap<String, Entry>,
    conns: Vec<Arc<McpConn>>,
}

/// The process-wide `demucs --mcp` server: started at most once, on first need.
enum DemucsState {
    NotStarted,
    Unavailable,
    Running(Arc<McpConn>),
}

static DEMUCS_CONN: Mutex<DemucsState> = Mutex::const_new(DemucsState::NotStarted);

/// `SPLITFIRE_DEMUCS_BIN`, else `demucs` on `PATH`.
fn demucs_binary() -> Option<PathBuf> {
    if let Some(bin) = std::env::var_os("SPLITFIRE_DEMUCS_BIN").filter(|v| !v.is_empty()) {
        return Some(bin.into());
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join("demucs"))
        .find(|candidate| candidate.is_file())
}

fn demucs_disabled() -> bool {
    std::env::var("SPLITFIRE_MCP_DEMUCS").is_ok_and(|v| {
        matches!(
            v.to_ascii_lowercase().as_str(),
            "off" | "0" | "false" | "no"
        )
    })
}

/// Start `demucs --mcp` once per process, on first need. `None` (logged once) when it is
/// disabled, missing, or fails its handshake.
async fn demucs() -> Option<Arc<McpConn>> {
    let mut state = DEMUCS_CONN.lock().await;
    match &*state {
        DemucsState::Running(conn) => return Some(conn.clone()),
        DemucsState::Unavailable => return None,
        DemucsState::NotStarted => {}
    }
    *state = DemucsState::Unavailable;
    if demucs_disabled() {
        tracing::info!("stem separation disabled (SPLITFIRE_MCP_DEMUCS=off)");
        return None;
    }
    let Some(bin) = demucs_binary() else {
        tracing::info!(
            "stem separation unavailable: `demucs` not found (install it or set SPLITFIRE_DEMUCS_BIN)"
        );
        return None;
    };
    match McpConn::connect(DEMUCS, &bin, &["--mcp".to_string()], &[]).await {
        Ok(conn) => {
            tracing::info!("stem separation available via {}", bin.display());
            let conn = Arc::new(conn);
            *state = DemucsState::Running(conn.clone());
            Some(conn)
        }
        Err(e) => {
            tracing::info!(
                "stem separation unavailable: {} --mcp failed: {e:#}",
                bin.display()
            );
            None
        }
    }
}

/// Stop the process-wide demucs server, if one was started. Call after every session's
/// toolset has been shut down so this is the last owner.
pub async fn shutdown_demucs() {
    let state = std::mem::replace(&mut *DEMUCS_CONN.lock().await, DemucsState::Unavailable);
    if let DemucsState::Running(conn) = state
        && let Some(conn) = Arc::into_inner(conn)
    {
        conn.close().await;
    }
}

impl McpToolset {
    /// Connect the client-supplied stdio servers (HTTP/SSE are not advertised, so ignored)
    /// and, unless the client supplied its own `demucs`, the built-in separator.
    pub async fn connect(servers: &[McpServer]) -> Self {
        let mut set = Self::default();
        let mut supplied_demucs = false;
        for server in servers {
            let McpServer::Stdio(s) = server else {
                tracing::info!("ignoring non-stdio MCP server");
                continue;
            };
            supplied_demucs |= s.name == DEMUCS;
            let env: Vec<(String, String)> = s
                .env
                .iter()
                .map(|e| (e.name.clone(), e.value.clone()))
                .collect();
            match McpConn::connect(&s.name, &s.command, &s.args, &env).await {
                Ok(conn) => set.add(Arc::new(conn)),
                Err(e) => tracing::warn!("MCP server `{}` unavailable: {e:#}", s.name),
            }
        }
        if !supplied_demucs && let Some(conn) = demucs().await {
            set.add(conn);
        }
        set
    }

    fn add(&mut self, conn: Arc<McpConn>) {
        for tool in &conn.tools {
            let name = qualified(&conn.name, &tool.name);
            if self.tools.contains_key(&name) {
                tracing::warn!("duplicate MCP tool name {name}; keeping the first");
                continue;
            }
            let mut schema = Value::Object((*tool.input_schema).clone());
            if schema.get("type").is_none() {
                schema["type"] = json!("object");
            }
            let read_only = tool
                .annotations
                .as_ref()
                .and_then(|a| a.read_only_hint)
                .unwrap_or(false);
            self.tools.insert(
                name,
                Entry {
                    conn: conn.clone(),
                    raw: tool.name.to_string(),
                    description: format!(
                        "[{} MCP server] {}",
                        conn.name,
                        tool.description.as_deref().unwrap_or("")
                    ),
                    schema,
                    read_only,
                },
            );
        }
        self.conns.push(conn);
    }

    pub fn definitions(&self) -> Vec<Value> {
        let mut names: Vec<&String> = self.tools.keys().collect();
        names.sort();
        names
            .into_iter()
            .map(|n| {
                let e = &self.tools[n];
                function_def(n, &e.description, e.schema.clone())
            })
            .collect()
    }

    pub fn has(&self, name: &str) -> bool {
        self.tools.contains_key(name)
    }

    pub fn is_read_only(&self, name: &str) -> bool {
        self.tools.get(name).is_some_and(|e| e.read_only)
    }

    /// Whether the built-in (or a client-supplied) stem separator is connected.
    pub fn has_stem_separation(&self) -> bool {
        self.tools
            .contains_key(&qualified(DEMUCS, "separate_stems"))
    }

    /// What the permission prompt shows for a call. For stem separation this consults
    /// `list_models` so the prompt can state the download size of uncached weights.
    pub async fn permission_preview(&self, name: &str, args: &Value) -> Vec<ToolCallContent> {
        let Some(entry) = self.tools.get(name) else {
            return Vec::new();
        };
        let mut text = format!("Run `{}` on the {} MCP server.", entry.raw, entry.conn.name);
        if entry.raw == "separate_stems" && entry.conn.name == DEMUCS {
            let input = args.get("input").and_then(Value::as_str).unwrap_or("?");
            let model = args
                .get("model")
                .and_then(Value::as_str)
                .unwrap_or("htdemucs");
            text = format!(
                "Separate {input} into stems with {model}. Runs locally and can take minutes."
            );
            match model_download_note(entry, model).await {
                Some(note) => text.push_str(&format!("\n{note}")),
                None => text.push_str("\nCould not check whether the model weights are cached."),
            }
        } else if args.as_object().is_some_and(|o| !o.is_empty()) {
            let shown = serde_json::to_string_pretty(args).unwrap_or_default();
            text.push_str(&format!("\nArguments:\n```json\n{}\n```", truncate(shown)));
        }
        vec![ToolCallContent::from(text)]
    }

    /// Call a tool, streaming MCP progress into the tool-call card and forwarding
    /// session cancellation as `notifications/cancelled`.
    pub async fn call(
        &self,
        ctx: &ToolCtx,
        tool_call_id: &str,
        name: &str,
        args: Value,
    ) -> Result<ToolOutcome> {
        let entry = self
            .tools
            .get(name)
            .ok_or_else(|| anyhow!("unknown tool `{name}`"))?;
        let arguments: Map<String, Value> = match args {
            Value::Object(o) => o,
            Value::Null => Map::new(),
            _ => bail!("tool arguments must be a JSON object"),
        };
        let params = CallToolRequestParams::new(entry.raw.clone()).with_arguments(arguments);
        let mut handle = entry
            .conn
            .service
            .peer()
            .send_cancellable_request(
                ClientRequest::CallToolRequest(Request::new(params)),
                PeerRequestOptions::no_options(),
            )
            .await
            .map_err(|e| anyhow!("MCP server `{}` is not responding: {e}", entry.conn.name))?;
        let mut progress = entry
            .conn
            .service
            .service()
            .progress
            .subscribe(handle.progress_token.clone())
            .await;
        let deadline = tokio::time::sleep(CALL_TIMEOUT);
        tokio::pin!(deadline);

        enum End {
            Done(Box<Result<ServerResult, rmcp::ServiceError>>),
            Cancelled,
            TimedOut,
        }
        let end = loop {
            tokio::select! {
                r = &mut handle.rx => {
                    break End::Done(Box::new(r.unwrap_or(Err(rmcp::ServiceError::TransportClosed))));
                }
                Some(p) = progress.next() => {
                    let line = progress_line(&p);
                    let _ = ctx.update(
                        tool_call_id,
                        ToolCallUpdateFields::new().content(vec![ToolCallContent::from(line)]),
                    );
                }
                () = ctx.cancel.cancelled() => break End::Cancelled,
                () = &mut deadline => break End::TimedOut,
            }
        };
        match end {
            End::Done(done) => match *done {
                Ok(ServerResult::CallToolResult(r)) => Ok(outcome_from(r)),
                Ok(_) => Ok(ToolOutcome::err(
                    "The MCP server sent an unexpected response.",
                )),
                Err(e) => Ok(ToolOutcome::err(format!("MCP call failed: {e}"))),
            },
            End::Cancelled => {
                let _ = handle.cancel(Some("cancelled by user".into())).await;
                Ok(ToolOutcome::err("Cancelled by user."))
            }
            End::TimedOut => {
                let _ = handle.cancel(Some("timed out".into())).await;
                Ok(ToolOutcome::err(format!(
                    "Timed out after {} minutes.",
                    CALL_TIMEOUT.as_secs() / 60
                )))
            }
        }
    }

    /// Close this session's servers. Shared servers (the built-in demucs) stay up.
    pub async fn shutdown(self) {
        drop(self.tools);
        for conn in self.conns {
            if let Some(conn) = Arc::into_inner(conn) {
                conn.close().await;
            }
        }
    }
}

fn progress_line(p: &ProgressNotificationParam) -> String {
    let pct = p
        .total
        .filter(|t| *t > 0.0)
        .map(|t| format!(" ({:.0}%)", 100.0 * p.progress / t));
    format!(
        "{}{}",
        p.message.as_deref().unwrap_or("Working…"),
        pct.unwrap_or_default()
    )
}

fn outcome_from(r: rmcp::model::CallToolResult) -> ToolOutcome {
    let mut parts = Vec::new();
    for block in &r.content {
        match block {
            ContentBlock::Text(t) => parts.push(t.text.clone()),
            ContentBlock::Image(i) => parts.push(format!("[image: {}]", i.mime_type)),
            ContentBlock::Audio(a) => parts.push(format!("[audio: {}]", a.mime_type)),
            ContentBlock::ResourceLink(l) => parts.push(format!("[resource: {}]", l.uri)),
            ContentBlock::Resource(_) => parts.push("[embedded resource]".into()),
            _ => parts.push("[unsupported content]".into()),
        }
    }
    let mut text = parts.join("\n");
    if text.is_empty() {
        text = r
            .structured_content
            .as_ref()
            .map(|v| v.to_string())
            .unwrap_or_else(|| "(no output)".into());
    }
    let mut out = if r.is_error == Some(true) {
        ToolOutcome::err(truncate(text))
    } else {
        ToolOutcome::ok(truncate(text))
    };
    out.locations = stem_paths(r.structured_content.as_ref());
    out
}

/// `structuredContent.stems[].path`, absolute paths only.
fn stem_paths(structured: Option<&Value>) -> Vec<PathBuf> {
    structured
        .and_then(|v| v.get("stems"))
        .and_then(Value::as_array)
        .map(|stems| {
            stems
                .iter()
                .filter_map(|s| s.get("path").and_then(Value::as_str))
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
                .map(|p| absolutize(&p))
                .collect()
        })
        .unwrap_or_default()
}

/// "Downloads N MB…" when `model` is not cached, per demucs's `list_models`.
async fn model_download_note(entry: &Entry, model: &str) -> Option<String> {
    let params = CallToolRequestParams::new("list_models");
    let result = tokio::time::timeout(
        Duration::from_secs(10),
        entry.conn.service.peer().call_tool(params),
    )
    .await
    .ok()?
    .ok()?;
    let models = result
        .structured_content?
        .get("models")?
        .as_array()?
        .clone();
    let m = models
        .iter()
        .find(|m| m.get("id").and_then(Value::as_str) == Some(model))?;
    let size = m.get("size_mb").and_then(Value::as_u64);
    Some(match (m.get("cached").and_then(Value::as_bool), size) {
        (Some(true), _) => "The model weights are already downloaded.".to_string(),
        (_, Some(mb)) => format!("First use downloads about {mb} MB of model weights."),
        _ => "The model weights may need to be downloaded first.".to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qualified_names_round_trip() {
        let q = qualified("My Server!", "do.thing");
        assert_eq!(q, "mcp__My_Server__do_thing");
        assert_eq!(split_qualified(&q), Some(("My_Server", "do_thing")));
        assert_eq!(split_qualified("read_file"), None);
        assert_eq!(split_qualified("mcp__x"), None);
        assert_eq!(split_qualified("mcp____t"), None);
        assert!(qualified("s", &"t".repeat(100)).len() <= MAX_TOOL_NAME);
    }

    #[test]
    fn stems_become_absolute_locations() {
        let v = json!({"stems": [
            {"id": "drums", "path": "/tmp/a/drums.wav"},
            {"id": "bass", "path": "relative/bass.wav"},
            {"id": "x"}
        ]});
        let paths = stem_paths(Some(&v));
        assert_eq!(paths.len(), 1);
        assert!(paths[0].ends_with("drums.wav"));
        assert!(stem_paths(None).is_empty());
    }

    #[test]
    fn progress_text() {
        let p = ProgressNotificationParam::new(
            rmcp::model::ProgressToken(rmcp::model::NumberOrString::Number(1)),
            25.0,
        )
        .with_total(100.0)
        .with_message("Separating");
        assert_eq!(progress_line(&p), "Separating (25%)");
    }
}

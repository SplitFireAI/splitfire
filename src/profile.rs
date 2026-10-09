//! SplitFire's [`Profile`] for `ed-acp`: who the agent is, its prompt and tools, its slash
//! commands, and the optional built-in `demucs --mcp` stem separator.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use agent_client_protocol::schema::v1::{AvailableCommand, PromptCapabilities};
use async_trait::async_trait;
use ed_acp::ed_mcp::{McpTool, McpToolset, ServerSpec, SharedServer, find_binary, switched_off};
use ed_acp::{
    AgentInfo, AudioHints, LlmEnv, McpCall, Profile, PromptCtx, SessionCtx, Toolset,
    WorkspaceTools, llm,
};
use serde_json::{Value, json};

use crate::prompt;
use crate::tools::MusicTools;

pub const INFO: AgentInfo = AgentInfo {
    name: "splitfire-agent",
    display_name: "SplitFire",
    version: env!("CARGO_PKG_VERSION"),
    env: LlmEnv {
        prefix: "SPLITFIRE",
        dir_name: "splitfire-agent",
        default_model: llm::DEFAULT_MODEL,
    },
};

/// Server name of the built-in stem separator; a client-supplied server of this name wins.
pub const DEMUCS: &str = "demucs";

/// `demucs --mcp`, started once per process on first need: `SPLITFIRE_DEMUCS_BIN`, else
/// `demucs` on `PATH`. `SPLITFIRE_MCP_DEMUCS=off` turns it off.
static DEMUCS_SERVER: SharedServer = SharedServer::new(DEMUCS, demucs_spec);

fn demucs_spec() -> Option<ServerSpec> {
    if switched_off("SPLITFIRE_MCP_DEMUCS") {
        tracing::info!("stem separation disabled (SPLITFIRE_MCP_DEMUCS=off)");
        return None;
    }
    let Some(command) = find_binary("SPLITFIRE_DEMUCS_BIN", "demucs") else {
        tracing::info!(
            "stem separation unavailable: `demucs` not found (install it or set SPLITFIRE_DEMUCS_BIN)"
        );
        return None;
    };
    Some(ServerSpec::stdio(
        DEMUCS,
        command,
        vec!["--mcp".to_string()],
        Vec::new(),
    ))
}

/// Whether a stem separator (built-in or client-supplied) is connected.
fn has_stems(mcp: &McpToolset) -> bool {
    mcp.has_tool(DEMUCS, "separate_stems")
}

fn is_separation(tool: &McpTool) -> bool {
    tool.server == DEMUCS && tool.tool == "separate_stems"
}

pub struct SplitFire;

#[async_trait]
impl Profile for SplitFire {
    fn info(&self) -> AgentInfo {
        INFO
    }

    fn system_prompt(&self, ctx: &PromptCtx<'_>) -> String {
        prompt::system_prompt(ctx.surface, ctx.cwd, ctx.roots, has_stems(ctx.mcp))
    }

    fn toolsets(&self) -> Vec<Arc<dyn Toolset>> {
        vec![Arc::new(WorkspaceTools), Arc::new(MusicTools)]
    }

    fn slash_commands(&self, ctx: &SessionCtx<'_>) -> Vec<AvailableCommand> {
        prompt::slash_commands(has_stems(ctx.mcp))
    }

    fn expand_slash(&self, text: &str, ctx: &SessionCtx<'_>) -> Option<String> {
        prompt::expand_slash(text, has_stems(ctx.mcp))
    }

    fn builtin_mcp_servers(&self) -> Vec<&'static SharedServer> {
        vec![&DEMUCS_SERVER]
    }

    fn prompt_capabilities(&self) -> PromptCapabilities {
        PromptCapabilities::new()
            .embedded_context(true)
            .image(true)
            .audio(true)
    }

    fn audio_hints(&self) -> AudioHints {
        AudioHints {
            link: |path| {
                format!(
                    "[Referenced audio file: {}. Use analyze_audio on that path to inspect it.]",
                    path.display()
                )
            },
            attachment: |mime, path| {
                format!(
                    "[Attached audio ({mime}) saved at {}. Use analyze_audio on that path to inspect it.]",
                    path.display()
                )
            },
        }
    }

    /// For stem separation, say what will be separated and whether the model weights still
    /// have to be downloaded, per the server's `list_models`.
    async fn mcp_permission_preview(&self, call: McpCall<'_>) -> Option<String> {
        if !is_separation(call.tool) {
            return None;
        }
        let input = call
            .args
            .get("input")
            .and_then(Value::as_str)
            .unwrap_or("?");
        let model = call
            .args
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or("htdemucs");
        let mut text =
            format!("Separate {input} into stems with {model}. Runs locally and can take minutes.");
        let note = match call.connection {
            Some(conn) => {
                let result = conn
                    .call_raw("list_models", json!({}), Duration::from_secs(10))
                    .await;
                result.and_then(|r| download_note(&r, model))
            }
            None => None,
        };
        text.push('\n');
        text.push_str(
            note.as_deref()
                .unwrap_or("Could not check whether the model weights are cached."),
        );
        Some(text)
    }

    fn mcp_result_locations(&self, tool: &McpTool, structured: &Value) -> Vec<PathBuf> {
        if is_separation(tool) {
            stem_paths(structured)
        } else {
            Vec::new()
        }
    }
}

/// "Downloads N MB…" when `model` is not cached, from a `list_models` result.
fn download_note(result: &Value, model: &str) -> Option<String> {
    let models = result.pointer("/structuredContent/models")?.as_array()?;
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

/// `structuredContent.stems[].path`, absolute paths only.
fn stem_paths(structured: &Value) -> Vec<PathBuf> {
    structured
        .get("stems")
        .and_then(Value::as_array)
        .map(|stems| {
            stems
                .iter()
                .filter_map(|s| s.get("path").and_then(Value::as_str))
                .map(PathBuf::from)
                .filter(|p| Path::new(p).is_absolute())
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stems_become_absolute_locations() {
        let v = json!({"stems": [
            {"id": "drums", "path": "/tmp/a/drums.wav"},
            {"id": "bass", "path": "relative/bass.wav"},
            {"id": "x"}
        ]});
        let paths = stem_paths(&v);
        assert_eq!(paths, vec![PathBuf::from("/tmp/a/drums.wav")]);
        assert!(stem_paths(&json!({})).is_empty());
    }

    #[test]
    fn download_note_reads_list_models() {
        let r = json!({"structuredContent": {"models": [
            {"id": "htdemucs", "size_mb": 80, "cached": false},
            {"id": "htdemucs_ft", "size_mb": 320, "cached": true}
        ]}});
        assert_eq!(
            download_note(&r, "htdemucs").as_deref(),
            Some("First use downloads about 80 MB of model weights.")
        );
        assert_eq!(
            download_note(&r, "htdemucs_ft").as_deref(),
            Some("The model weights are already downloaded.")
        );
        assert_eq!(download_note(&r, "nope"), None);
    }
}

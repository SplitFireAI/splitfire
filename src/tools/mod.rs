//! SplitFire's tools: the music theory engine, audio analysis and the separation library.
//! Workspace files and shell, MCP and approval come from `ed-acp`.

pub mod audio;
pub mod library;
pub mod theory;

use agent_client_protocol::schema::v1::{ToolCallLocation, ToolKind};
use anyhow::{Result, anyhow};
use async_trait::async_trait;
use serde_json::Value;

pub use ed_acp::{
    DescribeCtx, ToolCtx, ToolOutcome, Toolset, absolutize, function_def, resolve_in,
};

/// The music tools as one [`Toolset`]: six `theory_*` tools, `analyze_audio` and
/// `list_separations`. All are read-only and never ask for approval.
#[derive(Debug, Default, Clone, Copy)]
pub struct MusicTools;

#[async_trait]
impl Toolset for MusicTools {
    fn definitions(&self) -> Vec<Value> {
        let mut defs = theory::definitions();
        defs.push(audio::definition());
        defs.push(library::definition());
        defs
    }

    fn handles(&self, name: &str) -> bool {
        name.starts_with("theory_") || name == audio::NAME || name == library::NAME
    }

    fn describe(
        &self,
        name: &str,
        args: &Value,
        ctx: &DescribeCtx<'_>,
    ) -> (String, ToolKind, Vec<ToolCallLocation>) {
        if name.starts_with("theory_") {
            return (theory::title(name, args), ToolKind::Think, vec![]);
        }
        if name == library::NAME {
            return ("List separated songs".to_string(), ToolKind::Search, vec![]);
        }
        let path = args.get("path").and_then(Value::as_str);
        let loc = path
            .map(|p| absolutize(&resolve_in(ctx.cwd, p)))
            .map(ToolCallLocation::new)
            .into_iter()
            .collect();
        (
            format!("Analyze {}", path.unwrap_or("audio file")),
            ToolKind::Read,
            loc,
        )
    }

    async fn execute(
        &self,
        ctx: &ToolCtx,
        _tool_call_id: &str,
        name: &str,
        args: Value,
    ) -> Result<ToolOutcome> {
        if name.starts_with("theory_") {
            theory::execute(name, args)
        } else if name == audio::NAME {
            audio::execute(ctx, args).await
        } else if name == library::NAME {
            library::execute(ctx, args).await
        } else {
            Err(anyhow!("unknown tool `{name}`"))
        }
    }
}

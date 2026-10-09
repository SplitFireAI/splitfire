//! `analyze_audio`: format, levels, loudness, and tempo/key estimates for a local audio file.

use anyhow::{Result, bail};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{ToolCtx, ToolOutcome, absolutize, function_def};
use crate::audio::analysis;

pub const NAME: &str = "analyze_audio";
const MAX_FILE_BYTES: u64 = 1 << 30;

pub fn definition() -> Value {
    function_def(
        NAME,
        "Analyze a local audio file (WAV, FLAC, MP3, OGG, M4A/AAC, AIFF): duration, sample rate, channels, peak/RMS, integrated loudness (LUFS), and estimated tempo (with half/double-time readings) and key. Tempo and key are estimates; say so.",
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Audio file path; relative paths resolve against the working directory" },
                "root": { "type": "string", "description": "Workspace root to resolve a relative path against" }
            },
            "required": ["path"]
        }),
    )
}

#[derive(Deserialize)]
struct Args {
    path: String,
    root: Option<String>,
}

pub async fn execute(ctx: &ToolCtx, args: Value) -> Result<ToolOutcome> {
    let a: Args = serde_json::from_value(args)?;
    let path = absolutize(&ctx.resolve_with(a.root.as_deref(), &a.path)?);
    let meta = tokio::fs::metadata(&path).await;
    match meta {
        Ok(m) if m.is_file() && m.len() <= MAX_FILE_BYTES => {}
        Ok(m) if m.is_file() => bail!("{} is larger than 1 GiB", path.display()),
        Ok(_) => bail!("{} is not a file", path.display()),
        Err(e) => bail!("cannot read {}: {e}", path.display()),
    }
    let work = {
        let path = path.clone();
        tokio::task::spawn_blocking(move || analysis::analyze_and_report(&path))
    };
    let text = tokio::select! {
        r = work => r??,
        () = ctx.cancel.cancelled() => return Ok(ToolOutcome::err("Analysis cancelled.")),
    };
    let mut out = ToolOutcome::ok(text);
    out.locations.push(path);
    Ok(out)
}

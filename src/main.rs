//! splitfire-agent: a music-domain ACP agent (theory, production, industry, culture, live shows)
//! backed by Onde Inference, built on the Onde Agent Platform crates: `ed-acp` (the ACP server),
//! `ed-acp-tui` (the terminal UI) and `ed-mcp`. This crate holds only the music: its tools,
//! prompt, slash commands and the [`profile::SplitFire`] that wires them in.
//!
//! Run from a terminal it opens a TUI; launched by an editor (stdin not a TTY) or with `--acp`
//! it speaks ACP over stdio. Set ONDE_API_KEY (`app-id:app-secret`) or run `--setup`.
//! SPLITFIRE_YOLO=1 skips permission prompts. Logs go to stderr (RUST_LOG).

mod audio;
mod profile;
mod prompt;
mod theory;
mod tools;

use std::io::IsTerminal;
use std::path::PathBuf;
use std::sync::Arc;

use ed_acp::{LlmConfig, ServeOptions, cli};
use ed_acp_tui::TuiConfig;

use profile::{INFO, SplitFire};

const USAGE: &str = "\
Usage: splitfire-agent [--acp] [--yolo] [--list-models] [--root <path>...] [--setup] [--version]

  (no args)      interactive terminal UI (when run from a terminal)
  --acp          speak ACP over stdio for an editor (default when stdin is not a terminal)
  --yolo         approve file edits and commands without asking
  --list-models  list the models the Onde endpoint serves, then exit
  --root <path>  add an extra workspace root (repeatable; TUI mode only)
  --setup        interactive first-run setup: store your Onde API key (app-id:app-secret)
  --version      print the version

Environment: ONDE_API_KEY (required unless stored by --setup), SPLITFIRE_BASE_URL,
SPLITFIRE_MODEL, SPLITFIRE_MODELS, SPLITFIRE_YOLO, SPLITFIRE_DATA_DIR, SPLITFIRE_CONTEXT_WINDOW,
SPLITFIRE_DEMUCS_BIN, SPLITFIRE_MCP_DEMUCS=off, SPLITFIRE_STEMS_DIR
";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let has = |flag: &str| args.iter().any(|a| a == flag);
    if has("-h") || has("--help") {
        print!("{USAGE}");
        return Ok(());
    }
    if has("-V") || has("--version") {
        println!("{} {}", INFO.name, INFO.version);
        return Ok(());
    }
    // Collect `--root <path>` pairs; validate everything else is a known flag.
    let mut roots: Vec<PathBuf> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--root" => {
                i += 1;
                let Some(path) = args.get(i) else {
                    anyhow::bail!("--root requires a path\n\n{USAGE}");
                };
                let path = PathBuf::from(path);
                let path = if path.is_absolute() {
                    path
                } else {
                    std::env::current_dir()?.join(path)
                };
                if !path.is_dir() {
                    anyhow::bail!("--root {} is not a directory", path.display());
                }
                roots.push(path);
            }
            "--acp" | "--yolo" | "--list-models" | "--setup" => {}
            bad => anyhow::bail!("unknown argument {bad}\n\n{USAGE}"),
        }
        i += 1;
    }
    let mut opts = ServeOptions::from_env(&INFO);
    opts.yolo |= has("--yolo");
    if has("--setup") {
        cli::setup(&INFO).await?;
    } else if has("--list-models") {
        cli::list_models(&INFO).await?;
    } else if has("--acp") || !std::io::stdin().is_terminal() {
        if !roots.is_empty() {
            anyhow::bail!("--root is only supported in the interactive TUI\n\n{USAGE}");
        }
        cli::init_logging();
        ed_acp::serve_stdio(Arc::new(SplitFire), opts).await?;
    } else {
        let llm = LlmConfig::from_env(&INFO.env);
        if llm.api_key.is_none() {
            anyhow::bail!(
                "No Onde API key configured. Set ONDE_API_KEY or run `{} --setup`.",
                INFO.name
            );
        }
        let mut tui = TuiConfig::current_exe(INFO.name, opts.yolo)?;
        tui.env.push((INFO.env.var("SURFACE"), "tui".to_string()));
        tui.model_label = llm.model;
        tui.extra_roots = roots;
        ed_acp_tui::run(tui).await?;
    }
    Ok(())
}

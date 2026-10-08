//! splitfire-agent: a music-domain ACP agent (theory, production, industry, culture, live shows)
//! backed by Onde Inference.
//!
//! Run from a terminal it opens a TUI; launched by an editor (stdin not a TTY) or with `--acp`
//! it speaks ACP over stdio. Set ONDE_API_KEY (`app-id:app-secret`) or run `--setup`.
//! SPLITFIRE_YOLO=1 skips permission prompts. Logs go to stderr (RUST_LOG).

mod agent;
mod audio;
mod llm;
mod mcp;
mod prompt;
mod session_store;
mod theory;
mod tools;
mod tui;

use std::io::IsTerminal;
use std::path::PathBuf;

use agent_client_protocol::schema::v1::{
    AgentAuthCapabilities, AgentCapabilities, AuthenticateRequest, CancelNotification,
    CloseSessionRequest, DeleteSessionRequest, Implementation, InitializeRequest,
    InitializeResponse, ListSessionsRequest, LoadSessionRequest, LogoutCapabilities, LogoutRequest,
    NewSessionRequest, PromptCapabilities, PromptRequest, ResumeSessionRequest,
    SessionAdditionalDirectoriesCapabilities, SessionCapabilities, SessionCloseCapabilities,
    SessionDeleteCapabilities, SessionListCapabilities, SessionResumeCapabilities,
    SetSessionConfigOptionRequest,
};
use agent_client_protocol::{Agent, Stdio};
use anyhow::Context;

use agent::SplitFireAgent;
use llm::{LlmClient, LlmConfig};

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
        println!("splitfire-agent {}", env!("CARGO_PKG_VERSION"));
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
    let yolo = has("--yolo") || env_flag("SPLITFIRE_YOLO");
    if has("--setup") {
        setup().await?;
    } else if has("--list-models") {
        list_models().await?;
    } else if has("--acp") || !std::io::stdin().is_terminal() {
        if !roots.is_empty() {
            anyhow::bail!("--root is only supported in the interactive TUI\n\n{USAGE}");
        }
        run_agent(yolo).await?;
    } else {
        if !LlmConfig::from_env().api_key.is_some() {
            anyhow::bail!(
                "No Onde API key configured. Set ONDE_API_KEY or run `splitfire-agent --setup`."
            );
        }
        tui::run(yolo, roots).await?;
    }
    Ok(())
}

fn env_flag(name: &str) -> bool {
    std::env::var(name).is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true"))
}

/// Onde credentials are `app-id:app-secret`: exactly one colon, both halves non-empty.
fn valid_onde_key(key: &str) -> bool {
    let mut parts = key.split(':');
    matches!(
        (parts.next(), parts.next(), parts.next()),
        (Some(id), Some(secret), None) if !id.is_empty() && !secret.is_empty()
    )
}

/// `env` file content with `ONDE_API_KEY` set, keeping any other lines.
fn with_api_key(existing: &str, key: &str) -> String {
    let mut lines: Vec<String> = existing
        .lines()
        .filter(|l| {
            let l = l.trim().strip_prefix("export ").unwrap_or(l.trim());
            !l.starts_with("ONDE_API_KEY=")
        })
        .map(String::from)
        .collect();
    lines.push(format!("ONDE_API_KEY={key}"));
    lines.join("\n") + "\n"
}

/// Interactive first-run setup (`--setup`): prompt for the Onde key, verify it against Onde
/// Cloud, and store it in the platform config dir (`config_dir()/env`, mode 0600).
async fn setup() -> anyhow::Result<()> {
    use std::io::Write;

    if !std::io::stdin().is_terminal() {
        anyhow::bail!("--setup needs an interactive terminal");
    }
    println!("splitfire-agent setup\n");
    println!("Get credentials: sign in at https://ondeinference.com/root/login,");
    println!("register an app and assign a model. Your key is \"app-id:app-secret\".\n");
    let key = loop {
        print!("Paste your ONDE_API_KEY: ");
        std::io::stdout().flush()?;
        let mut line = String::new();
        if std::io::stdin().read_line(&mut line)? == 0 {
            anyhow::bail!("no input");
        }
        let key = line.trim().to_string();
        if valid_onde_key(&key) {
            break key;
        }
        println!("Onde credentials look like \"app-id:app-secret\" (exactly one colon).");
    };

    // Verify before writing anything.
    let mut config = LlmConfig::from_env();
    config.api_key = Some(key.clone());
    print!("Verifying the key with Onde Cloud… ");
    std::io::stdout().flush()?;
    match LlmClient::new(config).check_auth().await {
        Ok(()) => println!("OK"),
        Err(e) => anyhow::bail!("\nKey check failed: {e:#}\nNothing was written."),
    }

    let dir = llm::config_dir().context("no config directory available")?;
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("env");
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    std::fs::write(&path, with_api_key(&existing, &key))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    }
    println!("\nWrote {}. You're all set.", path.display());
    Ok(())
}

async fn list_models() -> anyhow::Result<()> {
    let client = LlmClient::new(LlmConfig::from_env());
    for m in client.models().await? {
        match m.owned_by {
            Some(owner) => println!("{}\t{}", m.id, owner),
            None => println!("{}", m.id),
        }
    }
    Ok(())
}

/// How the agent was launched: `tui` (interactive terminal UI) or `acp` (editor).
/// The TUI sets `SPLITFIRE_SURFACE=tui` on its subprocess; editors launch `--acp`
/// directly so the default is `acp`.
fn surface() -> &'static str {
    match std::env::var("SPLITFIRE_SURFACE") {
        Ok(s) if s == "tui" => "tui",
        _ => "acp",
    }
}

async fn run_agent(yolo: bool) -> agent_client_protocol::Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let agent = SplitFireAgent::new(LlmConfig::from_env(), yolo, surface(), llm::data_dir());
    tracing::info!("splitfire-agent starting with model {}", agent.model());

    let result = Agent
        .builder()
        .name("splitfire-agent")
        .on_receive_request(
            {
                let agent = agent.clone();
                async move |req: InitializeRequest, responder, _cx| {
                    agent.set_client_caps(req.client_capabilities.clone());
                    // Terminal auth is only usable by clients that can run it.
                    let methods = if req.client_capabilities.auth.terminal {
                        agent::auth_methods()
                    } else {
                        Vec::new()
                    };
                    responder.respond(
                        InitializeResponse::new(agent::negotiate_version(req.protocol_version))
                            .auth_methods(methods)
                            .agent_capabilities(
                                AgentCapabilities::new()
                                    .prompt_capabilities(
                                        PromptCapabilities::new()
                                            .embedded_context(true)
                                            .image(true)
                                            .audio(true),
                                    )
                                    .session_capabilities(
                                        SessionCapabilities::new()
                                            .list(SessionListCapabilities::new())
                                            .additional_directories(
                                                SessionAdditionalDirectoriesCapabilities::new(),
                                            )
                                            .resume(SessionResumeCapabilities::new())
                                            .close(SessionCloseCapabilities::new())
                                            .delete(SessionDeleteCapabilities::new()),
                                    )
                                    .auth(
                                        AgentAuthCapabilities::new()
                                            .logout(LogoutCapabilities::new()),
                                    )
                                    .load_session(true),
                            )
                            .agent_info(Implementation::new(
                                "splitfire-agent",
                                env!("CARGO_PKG_VERSION"),
                            )),
                    )
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            {
                let agent = agent.clone();
                async move |req: AuthenticateRequest, responder, cx| {
                    let agent = agent.clone();
                    cx.spawn(async move {
                        match agent.authenticate(req).await {
                            Ok(resp) => responder.respond(resp),
                            Err(e) => responder.respond_with_error(e),
                        }
                    })
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            {
                let agent = agent.clone();
                async move |_req: LogoutRequest, responder, cx| {
                    let agent = agent.clone();
                    cx.spawn(async move {
                        match agent.logout().await {
                            Ok(resp) => responder.respond(resp),
                            Err(e) => responder.respond_with_error(e),
                        }
                    })
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            {
                let agent = agent.clone();
                async move |req: NewSessionRequest, responder, cx| {
                    // Listing models and starting MCP servers are slow; keep them off the
                    // dispatch loop.
                    let agent = agent.clone();
                    let connection = cx.clone();
                    cx.spawn(async move {
                        // Picks up a key stored by `--setup` since this process started.
                        if !agent.refresh_credentials().await {
                            return responder.respond_with_error(agent::auth_required_error());
                        }
                        match agent.new_session(req).await {
                            Ok(resp) => {
                                let id = resp.session_id.clone();
                                responder.respond(resp)?;
                                agent.send_commands(&connection, &id)
                            }
                            Err(e) => responder.respond_with_error(e),
                        }
                    })
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            {
                let agent = agent.clone();
                async move |req: ListSessionsRequest, responder, cx| {
                    // Reads the session directory from disk.
                    let agent = agent.clone();
                    cx.spawn(async move {
                        match tokio::task::spawn_blocking(move || agent.list_sessions(req)).await {
                            Ok(Ok(resp)) => responder.respond(resp),
                            Ok(Err(e)) => responder.respond_with_error(e),
                            Err(e) => responder.respond_with_internal_error(e.to_string()),
                        }
                    })
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            {
                let agent = agent.clone();
                async move |req: LoadSessionRequest, responder, cx| {
                    let agent = agent.clone();
                    let connection = cx.clone();
                    cx.spawn(async move {
                        // Editors reopen a thread with session/load after terminal auth, so
                        // AUTH_REQUIRED here shows sign-in instead of a dead thread.
                        if !agent.refresh_credentials().await {
                            return responder.respond_with_error(agent::auth_required_error());
                        }
                        let id = req.session_id.clone();
                        match agent.load_session(req, &connection).await {
                            Ok(resp) => {
                                responder.respond(resp)?;
                                agent.send_commands(&connection, &id)
                            }
                            Err(e) => responder.respond_with_error(e),
                        }
                    })
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            {
                let agent = agent.clone();
                async move |req: ResumeSessionRequest, responder, cx| {
                    let agent = agent.clone();
                    let connection = cx.clone();
                    cx.spawn(async move {
                        if !agent.refresh_credentials().await {
                            return responder.respond_with_error(agent::auth_required_error());
                        }
                        let id = req.session_id.clone();
                        match agent.resume_session(req, &connection).await {
                            Ok(resp) => {
                                responder.respond(resp)?;
                                agent.send_commands(&connection, &id)
                            }
                            Err(e) => responder.respond_with_error(e),
                        }
                    })
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            {
                let agent = agent.clone();
                async move |req: CloseSessionRequest, responder, _cx| match agent.close_session(req)
                {
                    Ok(resp) => responder.respond(resp),
                    Err(e) => responder.respond_with_error(e),
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            {
                let agent = agent.clone();
                async move |req: DeleteSessionRequest, responder, _cx| match agent
                    .delete_session(req)
                {
                    Ok(resp) => responder.respond(resp),
                    Err(e) => responder.respond_with_error(e),
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            {
                let agent = agent.clone();
                async move |req: SetSessionConfigOptionRequest, responder, cx| {
                    let agent = agent.clone();
                    let connection = cx.clone();
                    cx.spawn(async move {
                        match agent.set_config_option(req, &connection).await {
                            Ok(resp) => responder.respond(resp),
                            Err(e) => responder.respond_with_error(e),
                        }
                    })
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            {
                let agent = agent.clone();
                async move |req: PromptRequest, responder, cx| {
                    // Run the turn off the dispatch loop so it can make requests to the client.
                    let agent = agent.clone();
                    let connection = cx.clone();
                    cx.spawn(async move {
                        if !agent.has_api_key() && !agent.refresh_credentials().await {
                            return responder.respond_with_error(agent::auth_required_error());
                        }
                        agent.prompt(req, responder, connection).await
                    })
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_notification(
            {
                let agent = agent.clone();
                async move |n: CancelNotification, _cx| {
                    agent.cancel(&n.session_id);
                    Ok(())
                }
            },
            agent_client_protocol::on_receive_notification!(),
        )
        .connect_to(Stdio::new())
        .await;
    // The editor went away: stop MCP servers instead of leaving them to be orphaned.
    agent.shutdown().await;
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn onde_key_shape() {
        assert!(valid_onde_key("app:secret"));
        assert!(!valid_onde_key("secret"));
        assert!(!valid_onde_key("a:b:c"));
        assert!(!valid_onde_key(":secret"));
        assert!(!valid_onde_key("app:"));
        assert!(!valid_onde_key(""));
    }

    #[test]
    fn env_file_keeps_other_lines_and_replaces_key() {
        let out = with_api_key(
            "# hi\nSPLITFIRE_MODEL=m\nexport ONDE_API_KEY=old:old\n",
            "new:key",
        );
        assert_eq!(out, "# hi\nSPLITFIRE_MODEL=m\nONDE_API_KEY=new:key\n");
        assert_eq!(with_api_key("", "a:b"), "ONDE_API_KEY=a:b\n");
    }
}

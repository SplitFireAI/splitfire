//! Live smoke tests against the real Onde Cloud endpoint (cloud.ondeinference.com).
//!
//! Skipped unless `ONDE_API_KEY` (format `app-id:app-secret`) is set: they hit the production
//! service and cost real tokens, so they are for manual or scheduled verification.
//!
//! Run with: ONDE_API_KEY=app-id:app-secret cargo test --test live -- --nocapture

use std::sync::{Arc, Mutex};

const BIN: &str = env!("CARGO_BIN_EXE_splitfire-agent");

fn live_key() -> Option<String> {
    std::env::var("ONDE_API_KEY").ok().filter(|k| !k.is_empty())
}

/// A clean command: no stored config, no overrides leaking in from the developer's shell.
fn clean_cmd(data_dir: &std::path::Path) -> std::process::Command {
    let mut cmd = std::process::Command::new(BIN);
    cmd.env("SPLITFIRE_DATA_DIR", data_dir)
        .env("SPLITFIRE_MCP_DEMUCS", "off")
        .env_remove("SPLITFIRE_BASE_URL")
        .env_remove("SPLITFIRE_MODEL")
        .env_remove("SPLITFIRE_MODELS");
    cmd
}

#[test]
fn onde_lists_models() {
    if live_key().is_none() {
        eprintln!("skipping: ONDE_API_KEY not set");
        return;
    }
    let dir = std::env::temp_dir().join(format!("sf-live-{}", uuid::Uuid::new_v4()));
    let out = clean_cmd(&dir)
        .arg("--list-models")
        .output()
        .expect("failed to run splitfire-agent --list-models");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "--list-models failed\nstdout: {stdout}\nstderr: {stderr}"
    );
    let ids: Vec<&str> = stdout
        .lines()
        .map(|l| l.split('\t').next().unwrap())
        .collect();
    assert!(!ids.is_empty(), "endpoint returned no models");
    assert!(
        ids.iter().any(|id| id.starts_with("onde")),
        "expected at least one onde-* model, got: {ids:?}"
    );
}

/// One completion through the ACP path that must use a theory tool: ask for the C# major
/// scale and expect the exact spelling (E# and B#) in the answer.
#[tokio::test(flavor = "multi_thread")]
async fn onde_answers_with_theory_tool() {
    use agent_client_protocol::schema::ProtocolVersion;
    use agent_client_protocol::schema::v1::{
        ContentBlock, InitializeRequest, NewSessionRequest, PromptRequest,
        RequestPermissionRequest, SessionNotification, SessionUpdate, StopReason, TextContent,
    };
    use agent_client_protocol::{AcpAgent, AcpAgentConfig, Agent, Client, ConnectionTo};

    if live_key().is_none() {
        eprintln!("skipping: ONDE_API_KEY not set");
        return;
    }
    let dir = std::env::temp_dir().join(format!("sf-live-{}", uuid::Uuid::new_v4()));
    let text: Arc<Mutex<String>> = Arc::default();
    let text_notify = text.clone();
    let tools: Arc<Mutex<Vec<String>>> = Arc::default();
    let tools_notify = tools.clone();

    let stop_reason = Client
        .builder()
        .on_receive_notification(
            async move |n: SessionNotification, _cx| {
                match n.update {
                    SessionUpdate::AgentMessageChunk(chunk) => {
                        if let ContentBlock::Text(t) = chunk.content {
                            text_notify.lock().unwrap().push_str(&t.text);
                        }
                    }
                    SessionUpdate::ToolCall(tc) => tools_notify.lock().unwrap().push(tc.title),
                    _ => {}
                }
                Ok(())
            },
            agent_client_protocol::on_receive_notification!(),
        )
        .on_receive_request(
            // Theory tools are read-only; a permission request means something else ran.
            async move |_req: RequestPermissionRequest, responder, _cx| {
                responder.respond_with_error(agent_client_protocol::Error::internal_error())
            },
            agent_client_protocol::on_receive_request!(),
        )
        .connect_with(
            AcpAgent::new(
                AcpAgentConfig::new(BIN)
                    .env("SPLITFIRE_DATA_DIR", dir.display().to_string())
                    .env("SPLITFIRE_MCP_DEMUCS", "off"),
            ),
            |connection: ConnectionTo<Agent>| async move {
                connection
                    .send_request(InitializeRequest::new(ProtocolVersion::V1))
                    .block_task()
                    .await?;
                let session = connection
                    .send_request(NewSessionRequest::new(std::env::current_dir().unwrap()))
                    .block_task()
                    .await?;
                let resp = connection
                    .send_request(PromptRequest::new(
                        session.session_id,
                        vec![ContentBlock::Text(TextContent::new(
                            "Spell the C# major scale. Use your theory tool and list the seven notes.",
                        ))],
                    ))
                    .block_task()
                    .await?;
                Ok(resp.stop_reason)
            },
        )
        .await
        .expect("ACP session failed");

    assert_eq!(stop_reason, StopReason::EndTurn);
    let text = text.lock().unwrap();
    eprintln!(
        "tool calls: {:?}\nmodel replied: {}",
        tools.lock().unwrap(),
        text.trim()
    );
    assert!(
        text.contains("E#") && text.contains("B#"),
        "expected exact spelling: {text}"
    );
}

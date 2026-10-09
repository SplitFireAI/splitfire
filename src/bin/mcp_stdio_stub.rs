//! Test MCP server for the integration tests: `ed_mcp::test_server` as a binary, so tests can
//! hand its path to the agent as a client-supplied server or as `SPLITFIRE_DEMUCS_BIN`.

#[tokio::main]
async fn main() {
    ed_mcp::test_server::run().await;
}

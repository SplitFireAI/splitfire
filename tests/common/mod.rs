//! SplitFire's side of the test setup: the conformance [`target`], and launch config for the
//! SDK-driven integration tests. The mock endpoint and the raw client come from `ed-acp-testkit`.
#![allow(dead_code)]

use std::path::Path;

use agent_client_protocol::AcpAgentConfig;

#[allow(unused_imports)]
pub use ed_acp_testkit::{MODEL, MockStats, Script, Step, TEST_KEY, Target, start_mock_llm, temp_dir};

/// 5 workspace tools + 6 theory tools + analyze_audio + list_separations.
pub const BASE_TOOL_COUNT: usize = 13;

/// splitfire-agent as the conformance suite sees it: built-in demucs off unless a test opts in.
pub fn target() -> Target {
    Target {
        bin: env!("CARGO_BIN_EXE_splitfire-agent").into(),
        name: "splitfire-agent",
        env_prefix: "SPLITFIRE",
        mcp_stub: env!("CARGO_BIN_EXE_mcp_stdio_stub").into(),
        base_tool_count: BASE_TOOL_COUNT,
        env: vec![("SPLITFIRE_MCP_DEMUCS".into(), "off".into())],
    }
}

/// Launch config for the real binary: mock endpoint, dummy key, isolated data dir, and no
/// built-in demucs unless a test opts in.
pub fn agent_config(binary: &str, base_url: &str, data_dir: &Path) -> AcpAgentConfig {
    AcpAgentConfig::new(binary)
        .env("SPLITFIRE_BASE_URL", base_url)
        .env("SPLITFIRE_MODEL", MODEL)
        .env("ONDE_API_KEY", TEST_KEY)
        .env("SPLITFIRE_DATA_DIR", data_dir.display().to_string())
        .env("SPLITFIRE_MCP_DEMUCS", "off")
}

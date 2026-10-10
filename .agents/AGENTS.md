# AGENTS.md

This file provides guidance to coding agents when working with code in this repository.

## Commands

CI (`.github/workflows/ci.yml`, ubuntu + macOS) runs exactly these:

```sh
cargo fmt -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
```

- Single test: `cargo test --test acp_integration <name>` (or `--lib <name>` for unit tests in `src/`).
- `tests/live.rs` hits real Onde Cloud and silently skips unless `ONDE_API_KEY=app-id:app-secret` is set: `ONDE_API_KEY=... cargo test --test live -- --nocapture`.
- Enable the tracked pre-commit hook (runs `cargo fmt --check` on staged `.rs` files) once per clone: `git config core.hooksPath .githooks`.
- Run locally: `cargo run -- --setup` (store API key), then `cargo run` (TUI) or `cargo run -- --acp` (ACP over stdio). `SPLITFIRE_YOLO=1`/`--yolo` skips permission prompts; logs go to stderr via `RUST_LOG`.

## Architecture

This crate holds only the *music* part of the agent. The ACP server, session/turn loop, approval, workspace tools (`read_file`, `write_file`, `run_command`, ...), terminal UI and MCP layer all come from the Onde Agent Platform crates (`ed-acp`, `ed-acp-tui`, `ed-mcp`, from the `onde-ed` workspace). Look there for anything protocol- or session-related; the README says it is checked out next to this repo.

- `src/profile.rs`: `SplitFire` implements `ed_acp::Profile`. It is the single wiring point: identity (`INFO`, env prefix `SPLITFIRE`), system prompt, toolsets (`WorkspaceTools` + `MusicTools`), slash commands, and the lazily started built-in `demucs --mcp` server (`SharedServer`; a client-supplied MCP server named `demucs` wins).
- `src/prompt.rs`: system prompt and slash commands (`/analyze`, `/chords`, `/practice`, `/stems` only when separation is available).
- `src/tools/`: `MusicTools` is one `Toolset` exposing six `theory_*` tools, `analyze_audio` and `list_separations`. All are read-only and never prompt for approval. New tools need an entry in `definitions()`, `handles()` and `describe()`.
- `src/theory/` (pitch, interval, scale, chord, key, tempo) and `src/audio/` (symphonia decode, loudness/tempo/key analysis) are the pure computation behind the tools. The design principle is that the model calls these instead of recalling answers, so results must be exact (correct enharmonic spelling, e.g. C# major has E# and B#).
- `src/lib.rs` vs `src/main.rs`: the crate is both a library (`Profile` for hosts that embed the agent over an in-memory channel with `ed_acp::serve`) and the `splitfire-agent` binary. The `cli` feature (default) pulls in `ed-acp-tui` and the MCP test stub; build with `--no-default-features` to check the embeddable library alone.
- The binary has three modes: TUI when stdin is a terminal; ACP over stdio with `--acp` or when stdin is not a terminal; plus `--setup` / `--list-models`. The TUI is itself an ACP client that spawns `splitfire-agent --acp` as a subprocess.

## Tests

Integration tests run the real binary against a scripted mock model and a test MCP server (`src/bin/mcp_stdio_stub.rs`, built from `ed-mcp`'s `test-server` feature, hence it requires `cli`). `tests/acp_compliance.rs` runs the platform conformance suite from `ed-acp-testkit` plus SplitFire-specific cases; shared helpers are in `tests/common/`.

## Conventions

- Edition 2024. Per `.agents/skills/rust-coding-standard`: no `.unwrap()`/`.expect()` outside `#[cfg(test)]`, don't silently discard errors with `let _ =`.
- `registry/` holds the ACP registry submission (`agent.json`) (its `version` is currently 1.0.0, behind `Cargo.toml`'s 1.1.0, so check it when releasing).

## Remembered notes

- Maven Central `search.maven.org/solrsearch` is search-index-lagged and often returns 0 results for newly published artifacts (e.g. `com.ondeinference:onde-inference` 1.3.2 returned `numFound=0` for multiple query forms, but the artifact was live). Authoritative sources for a "is X published on Maven Central?" check, in order of truth: (1) `https://central.sonatype.com/api/internal/browse/component/versions?filter=namespace:<g>,name:<a>` — the modern API, returns `version` + `publishedEpochMillis`; (2) `https://repo1.maven.org/maven2/<group-path>/<artifact>/maven-metadata.xml` — has `<latest>`/`<release>`/`<lastUpdated>`, definitive; (3) `curl -I https://repo1.maven.org/maven2/<group-path>/<artifact>/<version>/<artifact>-<version>.pom` — confirms the POM is live. Always use `User-Agent: Mozilla/5.0` because Xcode-shipped Python 3.9's `urllib` can JSON-decode-error on some endpoints.

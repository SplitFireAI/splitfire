# SplitFire AI Agent

Play like you mean it.

A music assistant that speaks the [Agent Client Protocol](https://agentclientprotocol.com) and
ships its own terminal UI. It covers music theory, production and DAW workflow, the music
industry, culture and history, and live shows, and it does the arithmetic with tools instead of
guessing: note spelling, chords, keys, tempo maths and audio measurements are computed, not
recalled by the model.

It runs on [Onde Inference](https://ondeinference.com) (Onde Cloud). Built on the official
[Rust SDK](https://github.com/agentclientprotocol/rust-sdk).

## What it can do

**Music theory tools** (exact, read-only, no permission prompts)

| Tool | Does |
|------|------|
| `theory_scale` | Spell a scale or mode from any root, with intervals, key signature and the diatonic chords (C# major really has E# and B#) |
| `theory_chord` | Spell a chord symbol (`F#m7b5`, `Bb13#11/D`, `C6/9`) or identify chords from notes |
| `theory_transpose` | Transpose chords or notes by semitones or between keys, with correct enharmonic spelling |
| `theory_progression` | Roman numerals, functions, secondary dominants, borrowed chords and ranked key candidates; or realize numerals (`ii7 V7 Imaj7`) in a key |
| `theory_tempo` | Note values (straight, dotted, triplet) in ms and Hz, bar length, sample counts, bpm from a loop length |
| `theory_pitch` | Note name, MIDI number and frequency conversion, with cents offset and adjustable A4 |

**Audio analysis**: `analyze_audio` reads WAV, FLAC, MP3, OGG, M4A/AAC and AIFF and reports
duration, sample rate, channels, peak, RMS, integrated loudness (LUFS), and estimated tempo
(including half and double time) and key with a confidence. Tempo and key are signal-analysis
estimates, and the agent says so. Audio attached to a prompt is saved to a scratch file and the
model is given its path.

**Project files**: `read_file`, `write_file`, `edit_file`, `list_directory`, `run_command`, with
permission prompts for writes and commands, multi-root workspaces, and the client's own
`fs/*` and `terminal/*` methods when the editor offers them.

**Slash commands**: `/analyze <file>`, `/chords <progression>`, `/practice <goal>`, and
`/stems <file>` when stem separation is installed.

**Sessions** persist across restarts: `session/list`, `load` (with tool calls and their output
replayed), `resume`, `close` and `delete`.

### Stem separation (optional)

Stem separation is not built in. Install [`demucs`](https://github.com/splitfireai/demucs-rs) and
put it on your `PATH` (or set `SPLITFIRE_DEMUCS_BIN`). SplitFire starts `demucs --mcp` the first
time a session begins, and offers `separate_stems` and `list_models` as tools. Separation runs
locally and can take minutes; the first use of a model downloads its weights, and the permission
prompt tells you how large the download is. If `demucs` is missing or fails to start, SplitFire
logs one line and carries on without it. An MCP server named `demucs` supplied by your editor
takes precedence. `SPLITFIRE_MCP_DEMUCS=off` disables it.

Separate only music you have the right to work with.

### Other MCP servers

Stdio MCP servers configured in your editor are connected for each session. Their tools show up
as `mcp__<server>__<tool>`, ask for permission unless the server marks them read-only, stream
progress into the tool call, and are cancelled when you cancel the turn. A server that fails to
start is skipped.

## Install and set up

```sh
cargo build --release   # -> target/release/splitfire-agent
splitfire-agent --setup
```

`--setup` asks for your Onde credentials (`app-id:app-secret`, from
[ondeinference.com](https://ondeinference.com/root/login): register an app and assign a model),
checks them against Onde Cloud, and stores them in the platform config directory
(`~/Library/Application Support/splitfire-agent/env` on macOS, `~/.config/splitfire-agent/env` on
Linux, `%APPDATA%\splitfire-agent\env` on Windows). Alternatively set `ONDE_API_KEY`.

Editors that can run terminal auth (`clientCapabilities.auth.terminal`) are offered `--setup` as
the authentication method. Until a key exists the agent answers `session/new`, `session/load`,
`session/resume` and `session/prompt` with `AUTH_REQUIRED`. A key stored by `--setup` is picked up
without a restart, whether or not the editor calls `authenticate` afterwards. `logout` removes the
stored key (a key exported as `ONDE_API_KEY` stays in effect).

## Use from the terminal

```sh
cd my-project
splitfire-agent            # asks before edits and commands
splitfire-agent --yolo     # approves everything
```

Enter sends, Esc cancels a running turn, Up/Down and PgUp/PgDn scroll, Ctrl-C or `/quit` exits.
Approval prompts take `y` (allow), `a` (always allow this tool), `n` (reject) or `r` (always reject this tool). The TUI is itself
an ACP client: it starts `splitfire-agent --acp` as a subprocess.

## Use with Zed

Editors launch the agent with piped stdio, which selects ACP mode (or pass `--acp`).

```json
{
  "agent_servers": {
    "splitfire-agent": {
      "command": "/path/to/splitfire-agent",
      "args": ["--acp"]
    }
  }
}
```

The model picker lists the models Onde Cloud serves for your app. Editors that support boolean
config options also get an "Auto-approve actions" toggle per thread. Threads are saved to disk, so
Zed can reopen and import them after a restart; a thread whose history is gone reopens empty with
a short notice instead of failing. Linked files (`@`-mentions and selections) are inlined, only
the selected lines for a selection; links to audio files are handed to `analyze_audio` instead.
Token usage is reported against `SPLITFIRE_CONTEXT_WINDOW`.

## Configuration

| Env var | Default | |
|---------|---------|---|
| `ONDE_API_KEY` | from the config `env` file | `app-id:app-secret`; required |
| `SPLITFIRE_BASE_URL` | `https://cloud.ondeinference.com/v1` | override the OpenAI-compatible endpoint |
| `SPLITFIRE_MODEL` | `onde-kkk` | default model |
| `SPLITFIRE_MODELS` | listed from `/models` | comma-separated list for the model picker |
| `SPLITFIRE_YOLO` | unset | `1` skips permission prompts (the default for each thread's auto-approve toggle) |
| `SPLITFIRE_CONTEXT_WINDOW` | `128000` | context size reported in `usage_update` |
| `SPLITFIRE_STEMS_DIR` | the app's iCloud and local folders | where `list_separations` looks for separated songs |
| `SPLITFIRE_DATA_DIR` | platform config dir | where `env`, sessions and scratch audio live |
| `SPLITFIRE_SURFACE` | `acp` | `tui` when launched by the terminal UI; used in the commit trailer |
| `SPLITFIRE_DEMUCS_BIN` | `demucs` on `PATH` | stem separation binary |
| `SPLITFIRE_MCP_DEMUCS` | on | `off` disables stem separation |
| `RUST_LOG` | unset | logs go to stderr |

`splitfire-agent --list-models` prints the models your endpoint serves.

## Development

```sh
cargo fmt -- --check
cargo clippy --all-targets -- -D warnings
cargo test --locked
```

SplitFire is built on the Onde Agent Platform crates in the
[`onde-ed`](https://github.com/ondeinference/ed) workspace, checked out next to this repository:
`ed-acp` (the ACP server: sessions, auth, the turn loop, approval, workspace tools),
`ed-acp-tui` (the terminal UI) and `ed-mcp` (MCP over `rmcp`). This crate holds the music.

The integration tests run the real binary against a scripted mock model and a test MCP server
(`src/bin/mcp_stdio_stub.rs`, from `ed-mcp`). `tests/acp_compliance.rs` runs the platform's ACP
conformance suite from `ed-acp-testkit` plus SplitFire's own tests. `tests/live.rs` talks to Onde
Cloud and is skipped unless `ONDE_API_KEY` is set.

```
src/main.rs            flags and mode dispatch (ACP, TUI, --setup, --list-models)
src/profile.rs         the ed-acp Profile: identity, tools, commands, built-in demucs
src/prompt.rs          system prompt and slash commands
src/tools/             theory, audio analysis and separation-library tools
src/theory/            pitch, interval, scale, chord, key, tempo
src/audio/             decoding and analysis
registry/              ACP registry submission
```

## Copyright

© 2026 [Splitfire AB](https://5mb.app) ([SplitFire AI](https://splitfire.ai)). Licensed under the
Apache License, Version 2.0; see [LICENSE](LICENSE).

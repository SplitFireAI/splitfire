//! The system prompt, slash commands, and the conversion of ACP prompt content into
//! model content.

use std::path::{Path, PathBuf};

use agent_client_protocol::schema::v1::{
    AvailableCommand, AvailableCommandInput, ClientCapabilities, ContentBlock, EmbeddedResource,
    EmbeddedResourceResource, ReadTextFileRequest, ResourceLink, SessionId, TextResourceContents,
    UnstructuredCommandInput,
};
use agent_client_protocol::{Client, ConnectionTo};
use anyhow::{Context, Result};
use base64::Engine;
use serde_json::{Value, json};

pub fn system_prompt(
    surface: &str,
    cwd: &Path,
    roots: &[PathBuf],
    stems_available: bool,
) -> String {
    let mut extra: Vec<&PathBuf> = roots.iter().filter(|r| *r != cwd).collect();
    extra.sort();
    extra.dedup();
    let roots_section = if extra.is_empty() {
        String::new()
    } else {
        let list = extra
            .iter()
            .map(|r| format!("- {}", r.display()))
            .collect::<Vec<_>>()
            .join("\n");
        format!(
            "\nAdditional workspace roots:\n{list}\n\
             Pass a root's absolute path as the `root` parameter of a file tool to work there."
        )
    };
    let stems_section = if stems_available {
        "\nStem separation: `mcp__demucs__separate_stems` splits a track into stems (drums, bass, \
         vocals, other; guitar and piano with the 6-stem model). It runs locally and can take \
         minutes. When it finishes, tell the user where the stem files are. Separate only music \
         the user says they have the right to work with."
    } else {
        "\nStem separation is not available in this session. If the user asks for stems, tell \
         them it needs the `demucs` command installed and on PATH (or SPLITFIRE_DEMUCS_BIN \
         pointing at it), and that the session must be restarted afterwards. Do not pretend to \
         separate anything, and do not offer a workaround with the file or shell tools."
    };
    format!(
        "You are SplitFire, a music assistant for musicians, producers, engineers, songwriters \
         and fans. You cover music theory, production and DAW workflow, the music industry, \
         music culture and history, and live shows.\n\
         \n\
         Working rules:\n\
         - Use the theory tools (theory_scale, theory_chord, theory_transpose, \
         theory_progression, theory_tempo, theory_pitch) for note spelling, chords, keys, \
         transposition, delay and tempo maths and pitch conversion. Do not compute these from \
         memory; quote the tool's result.\n\
         - Use analyze_audio for audio files. Its tempo and key are signal-analysis estimates: \
         call them estimates, mention the confidence, and mention half/double-time readings \
         when they matter.\n\
         - Use list_separations to see what the SplitFire app has already separated, instead of \
         guessing at stem paths. It is read-only: never edit or delete anything inside those \
         song folders, because the app rebuilds its history from that tree.\n\
         - Be specific and practical: give settings, ranges and reasoning, and say when \
         something is taste rather than rule. Match the user's level.\n\
         - Do not reproduce song lyrics beyond a short quotation; discuss, summarise or \
         analyse instead.\n\
         - On sampling, covers, licensing, splits and royalties explain how things generally \
         work and what to ask a lawyer or collecting society; this is not legal advice.\n\
         - Do not invent release dates, chart positions, credits, gear specs or tour dates. \
         Say so when you are unsure or when facts may have changed.\n\
         - Keep answers concise and use Markdown.\n\
         {stems_section}\n\
         \n\
         Working directory: {cwd}{roots_section}\n\
         File tools are available for the user's project notes, chord charts and lyric sheets. \
         Read before editing and prefer edit_file for small changes.\n\
         When creating git commits, include the trailer:\n\
         Co-Authored-By: SplitFire v{version}-{surface} <noreply@splitfire.ai>",
        cwd = cwd.display(),
        version = env!("CARGO_PKG_VERSION"),
    )
}

struct SlashCommand {
    name: &'static str,
    description: &'static str,
    hint: &'static str,
    /// Needs the stem separator to be connected.
    needs_stems: bool,
}

const COMMANDS: &[SlashCommand] = &[
    SlashCommand {
        name: "analyze",
        description: "Analyze an audio file: loudness, tempo and key estimates",
        hint: "path to an audio file",
        needs_stems: false,
    },
    SlashCommand {
        name: "chords",
        description: "Analyze a chord progression: key, Roman numerals, borrowed chords",
        hint: "chords, e.g. Dm7 G7 Cmaj7",
        needs_stems: false,
    },
    SlashCommand {
        name: "library",
        description: "List the songs the SplitFire app has already separated",
        hint: "optional song name to filter by",
        needs_stems: false,
    },
    SlashCommand {
        name: "practice",
        description: "Build a focused practice plan with tempo targets",
        hint: "instrument and goal",
        needs_stems: false,
    },
    SlashCommand {
        name: "stems",
        description: "Separate a track into drums, bass, vocals and other stems",
        hint: "path to an audio file",
        needs_stems: true,
    },
];

/// Commands to advertise; `/stems` only when the separator is connected.
pub fn slash_commands(stems_available: bool) -> Vec<AvailableCommand> {
    COMMANDS
        .iter()
        .filter(|c| stems_available || !c.needs_stems)
        .map(|c| {
            AvailableCommand::new(c.name, c.description).input(AvailableCommandInput::Unstructured(
                UnstructuredCommandInput::new(c.hint),
            ))
        })
        .collect()
}

/// If `text` starts with a known slash command, the instruction the model should follow.
pub fn expand_slash(text: &str, stems_available: bool) -> Option<String> {
    let rest = text.trim_start().strip_prefix('/')?;
    let (name, arg) = rest
        .split_once(char::is_whitespace)
        .map_or((rest, ""), |(n, a)| (n, a.trim()));
    let missing = |what: &str| format!("The user ran /{name} without {what}. Ask them for it.");
    Some(match name {
        "analyze" if arg.is_empty() => missing("a file path"),
        "analyze" => format!(
            "Analyze the audio file {arg} with analyze_audio, then summarise it: duration, \
             loudness and headroom, tempo and key (labelled as estimates, with confidence), and \
             anything worth knowing before mixing or mastering."
        ),
        "chords" if arg.is_empty() => missing("a progression"),
        "chords" => format!(
            "Analyze this progression with theory_progression: {arg}\n\
             Give the key, Roman numerals and functions, point out borrowed chords and \
             secondary dominants, and suggest a few substitutions or voicing ideas."
        ),
        "library" => {
            let scope = if arg.is_empty() {
                "Call list_separations".to_string()
            } else {
                format!("Call list_separations with song={arg}")
            };
            format!(
                "{scope} and summarise the user's separation library: which songs, the model \
                 each was separated with, when, and which stems are on this device. Mention any \
                 stem that is listed but not downloaded. If it is empty, say how to separate a \
                 track rather than guessing at paths."
            )
        }
        "practice" if arg.is_empty() => missing("an instrument and goal"),
        "practice" => format!(
            "Build a focused practice plan for: {arg}\n\
             Include a warm-up, two or three targeted exercises, and tempo targets worked out \
             with theory_tempo. Keep it to one session the user can finish in under an hour."
        ),
        "stems" if !stems_available => {
            "The user ran /stems, but stem separation is not available in this session. Tell \
             them it needs the `demucs` command installed (see the SplitFire README) and to \
             restart the session afterwards."
                .to_string()
        }
        "stems" if arg.is_empty() => missing("a file path"),
        "stems" => format!(
            "Separate {arg} into stems with mcp__demucs__separate_stems, then list the stem \
             files that were written."
        ),
        _ => return None,
    })
}

fn embedded_text(res: &EmbeddedResource) -> Option<String> {
    match &res.resource {
        EmbeddedResourceResource::TextResourceContents(r) => {
            Some(format!("<file uri=\"{}\">\n{}\n</file>", r.uri, r.text))
        }
        // Describe binary resources rather than dropping them, so the model knows they exist.
        EmbeddedResourceResource::BlobResourceContents(b) => Some(format!(
            "[Attached binary resource: {} ({})]",
            b.uri,
            b.mime_type.as_deref().unwrap_or("unknown type")
        )),
        _ => None,
    }
}

/// How a resource link is shown to the model. Audio files are pointed at `analyze_audio`
/// (and the separator) instead of being presented as text to read.
fn link_text(link: &ResourceLink) -> String {
    match parse_file_link(&link.uri) {
        Some(FileLink { path, .. }) if is_audio(&path, link.mime_type.as_deref()) => format!(
            "[Referenced audio file: {}. Use analyze_audio on that path to inspect it.]",
            path.display()
        ),
        _ => format!("[Referenced: {}]", link.uri),
    }
}

/// File extensions the SplitFire app accepts, plus the other formats symphonia decodes.
const AUDIO_EXTENSIONS: &[&str] = &[
    "wav", "wave", "mp3", "flac", "m4a", "aac", "mp4", "ogg", "oga", "opus", "aif", "aiff", "aifc",
    "caf", "webm", "mkv",
];

fn is_audio(path: &Path, mime: Option<&str>) -> bool {
    mime.is_some_and(|m| m.starts_with("audio/"))
        || path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| AUDIO_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
}

/// Largest file a `file://` resource link is inlined for; bigger ones stay as references.
const MAX_LINKED_FILE_BYTES: usize = 256 * 1024;

/// Replace `file://` resource links to text files with their embedded contents, so the model
/// sees the file. Reads through the client when it supports `fs/read_text_file` (unsaved
/// buffers), else from disk. A link to a selection (`#L10:20`) is inlined as just those
/// lines. Audio files, links that can't be read and oversized files stay as references.
pub async fn resolve_resource_links(
    blocks: &[ContentBlock],
    caps: &ClientCapabilities,
    connection: &ConnectionTo<Client>,
    session_id: &SessionId,
) -> Vec<ContentBlock> {
    let mut out = Vec::with_capacity(blocks.len());
    for block in blocks {
        if let ContentBlock::ResourceLink(link) = block
            && let Some(FileLink { path, lines }) = parse_file_link(&link.uri)
            && !is_audio(&path, link.mime_type.as_deref())
        {
            let text = if caps.fs.read_text_file {
                connection
                    .send_request(ReadTextFileRequest::new(session_id.clone(), path.clone()))
                    .block_task()
                    .await
                    .map(|r| r.content)
                    .map_err(|e| e.to_string())
            } else {
                tokio::fs::read_to_string(&path)
                    .await
                    .map_err(|e| e.to_string())
            };
            let text = match lines {
                Some(range) => text.map(|t| slice_lines(&t, range)),
                None => text,
            };
            match text {
                Ok(text) if lines.is_some() && text.is_empty() => {
                    tracing::debug!("{} selects no lines", link.uri)
                }
                Ok(text) if text.len() <= MAX_LINKED_FILE_BYTES => {
                    out.push(ContentBlock::Resource(EmbeddedResource::new(
                        EmbeddedResourceResource::TextResourceContents(
                            TextResourceContents::new(text, link.uri.clone())
                                .mime_type(link.mime_type.clone()),
                        ),
                    )));
                    continue;
                }
                Ok(_) => tracing::debug!("{} too large to inline", link.uri),
                Err(e) => tracing::debug!("could not read {}: {e}", link.uri),
            }
        }
        out.push(block.clone());
    }
    out
}

/// A `file://` resource link: the absolute path, and the lines it selects, if any.
#[derive(Debug, PartialEq)]
struct FileLink {
    path: PathBuf,
    /// 1-based, inclusive.
    lines: Option<(usize, usize)>,
}

/// Parse a `file://` URI, percent-decoding the path. Editors put more than the path in it:
/// Zed links a selection as `file:///a.md?column=5#L10:20` and a symbol as
/// `file:///a.rs?symbol=main#L3:9`. The query is dropped and the fragment read as a line
/// range; a literal `?` or `#` in a file name arrives percent-encoded, so splitting first is
/// safe.
fn parse_file_link(uri: &str) -> Option<FileLink> {
    let rest = uri.strip_prefix("file://")?;
    let (rest, fragment) = match rest.split_once('#') {
        Some((rest, fragment)) => (rest, Some(fragment)),
        None => (rest, None),
    };
    let rest = rest.split_once('?').map_or(rest, |(rest, _)| rest);
    // Allow an authority of "" or "localhost"; reject other hosts.
    let path = if rest.starts_with('/') {
        rest
    } else {
        rest.strip_prefix("localhost")?
    };
    let bytes = path.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Ok(b) = u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 3]).ok()?, 16)
        {
            decoded.push(b);
            i += 3;
        } else {
            decoded.push(bytes[i]);
            i += 1;
        }
    }
    let path = PathBuf::from(String::from_utf8(decoded).ok()?);
    path.is_absolute().then(|| FileLink {
        path,
        lines: fragment.and_then(parse_line_range),
    })
}

/// A `#L10:20` fragment as a 1-based inclusive range. Accepts `L10:20`, `L10-20`, `L10-L20`
/// and a single line `L10`.
fn parse_line_range(fragment: &str) -> Option<(usize, usize)> {
    let range = fragment.strip_prefix('L')?;
    let (start, end) = range
        .split_once(':')
        .or_else(|| range.split_once('-'))
        .unwrap_or((range, range));
    let end = end.strip_prefix('L').unwrap_or(end);
    let (start, end) = (start.parse::<usize>().ok()?, end.parse::<usize>().ok()?);
    (start >= 1 && end >= start).then_some((start, end))
}

/// Lines `start..=end` (1-based) of `text`, keeping their line endings.
fn slice_lines(text: &str, (start, end): (usize, usize)) -> String {
    text.split_inclusive('\n')
        .skip(start - 1)
        .take(end - start + 1)
        .collect()
}

/// Flatten text-like prompt blocks into one string (used for titles and slash commands).
pub fn prompt_to_text(blocks: &[ContentBlock]) -> String {
    let mut parts = Vec::new();
    for block in blocks {
        match block {
            ContentBlock::Text(t) => parts.push(t.text.clone()),
            ContentBlock::ResourceLink(link) => parts.push(link_text(link)),
            ContentBlock::Resource(res) => parts.extend(embedded_text(res)),
            _ => {}
        }
    }
    parts.join("\n\n")
}

fn audio_extension(mime: &str) -> &'static str {
    match mime.split(';').next().unwrap_or("").trim() {
        "audio/wav" | "audio/x-wav" | "audio/wave" | "audio/vnd.wave" => "wav",
        "audio/mpeg" | "audio/mp3" => "mp3",
        "audio/flac" | "audio/x-flac" => "flac",
        "audio/ogg" | "audio/vorbis" | "application/ogg" => "ogg",
        "audio/mp4" | "audio/m4a" | "audio/x-m4a" | "audio/aac" => "m4a",
        "audio/aiff" | "audio/x-aiff" => "aiff",
        "audio/webm" | "video/webm" => "webm",
        _ => "audio",
    }
}

/// Write each audio block under `dir`; returns the saved path per audio block, in order.
pub async fn save_audio_blocks(blocks: &[ContentBlock], dir: &Path) -> Result<Vec<PathBuf>> {
    let mut saved = Vec::new();
    for block in blocks {
        let ContentBlock::Audio(audio) = block else {
            continue;
        };
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(audio.data.trim())
            .context("audio block is not valid base64")?;
        tokio::fs::create_dir_all(dir).await?;
        let path = dir.join(format!(
            "audio-{}.{}",
            uuid::Uuid::new_v4().simple(),
            audio_extension(&audio.mime_type)
        ));
        tokio::fs::write(&path, bytes).await?;
        saved.push(path);
    }
    Ok(saved)
}

/// Convert prompt blocks to an OpenAI `content` value: a string for text only, or parts when
/// images are present. Audio is referenced by the path it was saved to; the model reads it
/// with `analyze_audio`. `slash` replaces the first text block (an expanded slash command).
pub fn prompt_to_content(
    blocks: &[ContentBlock],
    audio_paths: &[PathBuf],
    slash: Option<&str>,
) -> Value {
    let mut audio = audio_paths.iter();
    let mut parts: Vec<Value> = Vec::new();
    let mut slash = slash;
    let mut has_image = false;
    let text = |t: String| json!({ "type": "text", "text": t });
    for block in blocks {
        match block {
            ContentBlock::Text(t) => match slash.take() {
                Some(expanded) => parts.push(text(expanded.to_string())),
                None => parts.push(text(t.text.clone())),
            },
            ContentBlock::ResourceLink(link) => parts.push(text(link_text(link))),
            ContentBlock::Resource(res) => {
                if let Some(t) = embedded_text(res) {
                    parts.push(text(t));
                }
            }
            ContentBlock::Image(img) => {
                has_image = true;
                let url = format!("data:{};base64,{}", img.mime_type, img.data);
                parts.push(json!({ "type": "image_url", "image_url": { "url": url } }));
            }
            ContentBlock::Audio(a) => match audio.next() {
                Some(path) => parts.push(text(format!(
                    "[Attached audio ({}) saved at {}. Use analyze_audio on that path to inspect it.]",
                    a.mime_type,
                    path.display()
                ))),
                None => parts.push(text("[Attached audio could not be saved]".into())),
            },
            _ => {}
        }
    }
    if has_image {
        return Value::Array(parts);
    }
    let joined = parts
        .iter()
        .filter_map(|p| p.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n\n");
    Value::String(joined)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link(uri: &str) -> Option<FileLink> {
        parse_file_link(uri)
    }

    #[test]
    fn plain_paths_decode_and_must_be_absolute() {
        assert_eq!(
            link("file:///a%20b/c.md"),
            Some(FileLink {
                path: "/a b/c.md".into(),
                lines: None
            })
        );
        assert_eq!(
            link("file://localhost/x.md").unwrap().path,
            Path::new("/x.md")
        );
        assert_eq!(link("file://other-host/x.md"), None);
        assert_eq!(link("https://example.com/x.md"), None);
    }

    #[test]
    fn zed_selection_and_symbol_links() {
        let sel = link("file:///notes/song.md?column=5#L10:20").unwrap();
        assert_eq!(sel.path, Path::new("/notes/song.md"));
        assert_eq!(sel.lines, Some((10, 20)));
        assert_eq!(
            link("file:///a.rs?symbol=main#L3:9").unwrap().lines,
            Some((3, 9))
        );
    }

    #[test]
    fn line_range_forms() {
        assert_eq!(parse_line_range("L10:20"), Some((10, 20)));
        assert_eq!(parse_line_range("L10-20"), Some((10, 20)));
        assert_eq!(parse_line_range("L10-L20"), Some((10, 20)));
        assert_eq!(parse_line_range("L7"), Some((7, 7)));
        assert_eq!(parse_line_range("L0"), None);
        assert_eq!(parse_line_range("L5:2"), None);
        assert_eq!(parse_line_range("x"), None);
    }

    #[test]
    fn slices_inclusive_lines() {
        assert_eq!(slice_lines("a\nb\nc\nd\n", (2, 3)), "b\nc\n");
        assert_eq!(slice_lines("a\nb", (2, 9)), "b");
        assert_eq!(slice_lines("a\n", (5, 6)), "");
    }

    #[test]
    fn audio_links_point_at_the_analyzer() {
        let wav = ResourceLink::new("mix.wav", "file:///music/mix%20v2.WAV");
        assert_eq!(
            link_text(&wav),
            "[Referenced audio file: /music/mix v2.WAV. Use analyze_audio on that path to inspect it.]"
        );
        let by_mime =
            ResourceLink::new("take", "file:///t/take").mime_type("audio/flac".to_string());
        assert!(link_text(&by_mime).contains("analyze_audio"));
        let md = ResourceLink::new("notes", "file:///t/notes.md");
        assert_eq!(link_text(&md), "[Referenced: file:///t/notes.md]");
    }
    use agent_client_protocol::schema::v1::{AudioContent, ImageContent, TextContent};

    #[test]
    fn system_prompt_mentions_stems_only_when_connected() {
        let cwd = Path::new("/work");
        let off = system_prompt("acp", cwd, &[], false);
        let on = system_prompt("acp", cwd, &[], true);
        // Without the separator the prompt must not offer the tool, but must say how to
        // enable it rather than going silent.
        assert!(!off.contains("mcp__demucs__separate_stems"));
        assert!(off.contains("not available") && off.contains("SPLITFIRE_DEMUCS_BIN"));
        assert!(on.contains("mcp__demucs__separate_stems"));
        // The read-only library tool is offered either way.
        assert!(off.contains("list_separations") && on.contains("list_separations"));
        assert!(off.contains("theory_scale") && off.contains("analyze_audio"));
        assert!(
            off.contains("Co-Authored-By: SplitFire v")
                && off.contains("-acp <noreply@splitfire.ai>")
        );
        let tui = system_prompt("tui", cwd, &[PathBuf::from("/a")], false);
        assert!(tui.contains("-tui <") && tui.contains("- /a"));
    }

    #[test]
    fn commands_depend_on_stems() {
        let names = |s| {
            slash_commands(s)
                .into_iter()
                .map(|c| c.name)
                .collect::<Vec<_>>()
        };
        // `/library` only reads the app's sidecars, so it is offered either way.
        assert_eq!(names(false), ["analyze", "chords", "library", "practice"]);
        assert_eq!(
            names(true),
            ["analyze", "chords", "library", "practice", "stems"]
        );
    }

    #[test]
    fn slash_expansion() {
        assert!(
            expand_slash("/analyze /a/b.wav", false)
                .unwrap()
                .contains("/a/b.wav")
        );
        assert!(
            expand_slash("/chords Dm7 G7", false)
                .unwrap()
                .contains("theory_progression")
        );
        assert!(
            expand_slash("/analyze", false)
                .unwrap()
                .contains("without a file path")
        );
        assert!(
            expand_slash("/stems x.wav", true)
                .unwrap()
                .contains("mcp__demucs__separate_stems")
        );
        assert!(
            expand_slash("/stems x.wav", false)
                .unwrap()
                .contains("not available")
        );
        assert!(expand_slash("/unknown x", false).is_none());
        assert!(expand_slash("hello /analyze", false).is_none());
    }

    #[tokio::test]
    async fn audio_is_saved_and_referenced() {
        let dir = std::env::temp_dir().join(format!("sf-prompt-{}", uuid::Uuid::new_v4()));
        let data = base64::engine::general_purpose::STANDARD.encode(b"RIFFfake");
        let blocks = vec![
            ContentBlock::Text(TextContent::new("what key?")),
            ContentBlock::Audio(AudioContent::new(data, "audio/wav")),
        ];
        let saved = save_audio_blocks(&blocks, &dir).await.unwrap();
        assert_eq!(saved.len(), 1);
        assert_eq!(saved[0].extension().unwrap(), "wav");
        assert_eq!(std::fs::read(&saved[0]).unwrap(), b"RIFFfake");
        let content = prompt_to_content(&blocks, &saved, None);
        let s = content.as_str().unwrap();
        assert!(s.contains("what key?") && s.contains(&saved[0].display().to_string()));
        std::fs::remove_dir_all(dir).ok();

        let bad = vec![ContentBlock::Audio(AudioContent::new("!!!", "audio/wav"))];
        assert!(
            save_audio_blocks(&bad, &std::env::temp_dir())
                .await
                .is_err()
        );
    }

    #[test]
    fn images_use_parts_and_slash_replaces_text() {
        let blocks = vec![
            ContentBlock::Text(TextContent::new("/chords Am F")),
            ContentBlock::Image(ImageContent::new("AAAA", "image/png")),
        ];
        let v = prompt_to_content(&blocks, &[], Some("EXPANDED"));
        let parts = v.as_array().unwrap();
        assert_eq!(parts[0]["text"], "EXPANDED");
        assert_eq!(parts[1]["type"], "image_url");
        let plain = prompt_to_content(&blocks[..1], &[], None);
        assert_eq!(plain, json!("/chords Am F"));
    }
}

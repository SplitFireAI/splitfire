//! SplitFire's system prompt and slash commands. Converting prompt content is `ed-acp`'s job.

use std::path::{Path, PathBuf};

use agent_client_protocol::schema::v1::{
    AvailableCommand, AvailableCommandInput, UnstructuredCommandInput,
};

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

#[cfg(test)]
mod tests {
    use super::*;

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
}

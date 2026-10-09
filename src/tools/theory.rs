//! Music theory tools: thin wrappers that format the deterministic engine in `crate::theory`.
//! All are read-only and run inline, so they never prompt for permission.

use anyhow::{Result, anyhow, bail};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{ToolOutcome, function_def};
use crate::theory::chord::{self, Chord};
use crate::theory::interval::Interval;
use crate::theory::key::{self, Key, Kind};
use crate::theory::pitch::{Note, Pitch, cents_between, hz_to_midi, midi_to_hz, spell_pc};
use crate::theory::scale;
use crate::theory::tempo::{self, Feel};

pub fn definitions() -> Vec<Value> {
    vec![
        function_def(
            "theory_scale",
            "Spell a scale or mode from a root note, with intervals, key signature and the diatonic chords. Exact; use instead of computing from memory. Omit `scale` to list the supported scales.",
            json!({
                "type": "object",
                "properties": {
                    "root": { "type": "string", "description": "Root note, e.g. C#, Bb" },
                    "scale": { "type": "string", "description": "e.g. major, dorian, harmonic minor, minor pentatonic, blues, altered" }
                },
                "required": ["root"]
            }),
        ),
        function_def(
            "theory_chord",
            "Parse a chord symbol (F#m7b5, Bb13#11/D, C6/9) into spelled notes, formula, quality and inversion, OR identify chords from notes (first note is treated as the bass). Exact.",
            json!({
                "type": "object",
                "properties": {
                    "symbol": { "type": "string", "description": "Chord symbol to spell" },
                    "notes": { "type": "array", "items": { "type": "string" }, "description": "Note names to identify, bass first" }
                }
            }),
        ),
        function_def(
            "theory_transpose",
            "Transpose chord symbols or note names with correct enharmonic spelling, by semitones or from one key to another.",
            json!({
                "type": "object",
                "properties": {
                    "items": { "type": "string", "description": "Chord symbols or notes separated by spaces or bars, e.g. 'Dm7 G7 Cmaj7'" },
                    "semitones": { "type": "integer", "description": "Positive up, negative down" },
                    "from_key": { "type": "string", "description": "e.g. C major (use with to_key)" },
                    "to_key": { "type": "string", "description": "e.g. Db major" },
                    "prefer": { "type": "string", "enum": ["sharps", "flats"], "description": "Force accidental style when using semitones" }
                },
                "required": ["items"]
            }),
        ),
        function_def(
            "theory_progression",
            "Analyse a chord progression (Roman numerals, function, secondary dominants, borrowed chords, ranked key candidates) or realize Roman numerals in a key as chord symbols.",
            json!({
                "type": "object",
                "properties": {
                    "chords": { "type": "string", "description": "Chord symbols separated by spaces, bars or commas" },
                    "key": { "type": "string", "description": "Key to analyse in (optional for chords; required for numerals), e.g. C major, F# minor" },
                    "numerals": { "type": "string", "description": "Roman numerals to realize, e.g. 'ii7 V7 Imaj7' or 'V7/V'" }
                }
            }),
        ),
        function_def(
            "theory_tempo",
            "Tempo maths: note-value durations (straight, dotted, triplet) in ms and Hz for delays and LFOs, bar length for a time signature, sample counts, or the bpm of a loop from its length.",
            json!({
                "type": "object",
                "properties": {
                    "bpm": { "type": "number" },
                    "time_signature": { "type": "string", "description": "e.g. 4/4, 6/8, 7/8 (default 4/4)" },
                    "sample_rate": { "type": "number", "description": "Adds sample counts, e.g. 44100 or 48000" },
                    "bars": { "type": "number", "description": "Loop length in bars (with duration_seconds, to derive bpm)" },
                    "duration_seconds": { "type": "number" },
                    "quarter_note_ms": { "type": "number", "description": "Derive bpm from a quarter-note time in ms" },
                    "note_value": { "type": "string", "description": "A single duration to answer, e.g. 1/8., 1/4t, 1/16 (. dotted, t triplet)" }
                }
            }),
        ),
        function_def(
            "theory_pitch",
            "Convert between note names, MIDI numbers and frequencies (A4 reference adjustable). For a frequency, returns the nearest note and cents offset.",
            json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "e.g. A4, C#3, Bb2" },
                    "midi": { "type": "number" },
                    "hz": { "type": "number" },
                    "a4": { "type": "number", "description": "Reference tuning, default 440" }
                }
            }),
        ),
    ]
}

pub fn title(name: &str, args: &Value) -> String {
    let s = |k: &str| args.get(k).and_then(Value::as_str);
    match name {
        "theory_scale" => format!(
            "Scale: {} {}",
            s("root").unwrap_or("?"),
            s("scale").unwrap_or("")
        )
        .trim()
        .to_string(),
        "theory_chord" => match s("symbol") {
            Some(c) => format!("Chord: {c}"),
            None => "Identify chord".into(),
        },
        "theory_transpose" => format!("Transpose {}", s("items").unwrap_or("")),
        "theory_progression" => match s("numerals") {
            Some(n) => format!("Realize {n}"),
            None => format!("Analyse {}", s("chords").unwrap_or("progression")),
        },
        "theory_tempo" => match args.get("bpm").and_then(Value::as_f64) {
            Some(b) => format!("Tempo: {b} bpm"),
            None => "Tempo".into(),
        },
        "theory_pitch" => "Pitch conversion".into(),
        _ => name.to_string(),
    }
}

pub fn execute(name: &str, args: Value) -> Result<ToolOutcome> {
    let text = match name {
        "theory_scale" => scale_tool(serde_json::from_value(args)?)?,
        "theory_chord" => chord_tool(serde_json::from_value(args)?)?,
        "theory_transpose" => transpose_tool(serde_json::from_value(args)?)?,
        "theory_progression" => progression_tool(serde_json::from_value(args)?)?,
        "theory_tempo" => tempo_tool(serde_json::from_value(args)?)?,
        "theory_pitch" => pitch_tool(serde_json::from_value(args)?)?,
        other => bail!("unknown tool `{other}`"),
    };
    Ok(ToolOutcome::ok(text))
}

fn join_names(notes: &[Note]) -> String {
    notes.iter().map(|n| n.name()).collect::<Vec<_>>().join(" ")
}

#[derive(Deserialize)]
struct ScaleArgs {
    root: String,
    scale: Option<String>,
}

fn scale_tool(a: ScaleArgs) -> Result<String> {
    let root = Note::parse(&a.root)?;
    let Some(name) = a.scale else {
        return Ok(format!("Supported scales: {}", scale::names().join(", ")));
    };
    let def = scale::parse_scale(&name)?;
    let notes = def.spell(root);
    let intervals = def.intervals();
    let mut out = format!(
        "{} {}\nNotes: {}\nFormula: {}\nIntervals: {}",
        root.name(),
        def.name,
        join_names(&notes),
        def.formula,
        intervals
            .iter()
            .map(|i| i.name())
            .collect::<Vec<_>>()
            .join(" "),
    );
    if !def.aliases.is_empty() {
        out.push_str(&format!("\nAlso called: {}", def.aliases.join(", ")));
    }
    if matches!(def.name, "major" | "natural minor") {
        out.push_str(&format!(
            "\nKey signature: {}",
            scale::signature_text(scale::signature(&notes))
        ));
    }
    if notes.len() == 7 {
        let stack = |n: usize| -> Vec<String> {
            (0..7)
                .map(|i| {
                    let tones: Vec<Note> = (0..n).map(|k| notes[(i + 2 * k) % 7]).collect();
                    chord::identify(&tones)
                        .into_iter()
                        .find(|c| c.chord.bass.is_none() && c.chord.root.pc() == tones[0].pc())
                        .map_or_else(|| "n/a".into(), |c| c.chord.name())
                })
                .collect()
        };
        out.push_str(&format!("\nDiatonic triads: {}", stack(3).join(" ")));
        out.push_str(&format!("\nDiatonic sevenths: {}", stack(4).join(" ")));
    }
    Ok(out)
}

#[derive(Deserialize)]
struct ChordArgs {
    symbol: Option<String>,
    notes: Option<Vec<String>>,
}

fn chord_report(c: &Chord) -> String {
    let notes = c.notes();
    let mut out = format!(
        "{}\nNotes: {}\nFormula: {}\nQuality: {}",
        c.name(),
        join_names(&notes),
        c.formula(),
        c.quality(),
    );
    if let Some(b) = c.bass {
        out.push_str(&format!(
            "\nBass: {} ({})",
            b.name(),
            c.bass_description().unwrap_or_default()
        ));
    }
    out
}

fn chord_tool(a: ChordArgs) -> Result<String> {
    match (a.symbol, a.notes) {
        (Some(sym), _) => Ok(chord_report(&Chord::parse(&sym)?)),
        (None, Some(notes)) if !notes.is_empty() => {
            let parsed = notes
                .iter()
                .map(|n| Note::parse(n))
                .collect::<Result<Vec<_>>>()?;
            let found = chord::identify(&parsed);
            if found.is_empty() {
                return Ok(format!(
                    "No common chord matches {}. It may be a cluster, a polychord or an incomplete voicing.",
                    join_names(&parsed)
                ));
            }
            let lines: Vec<String> = found
                .iter()
                .take(6)
                .enumerate()
                .map(|(i, c)| format!("{}. {} ({})", i + 1, c.chord.name(), c.chord.quality()))
                .collect();
            Ok(format!(
                "Notes: {} (bass {})\nCandidates, most likely first:\n{}",
                join_names(&parsed),
                parsed[0].name(),
                lines.join("\n")
            ))
        }
        _ => bail!("give either `symbol` or a non-empty `notes` list"),
    }
}

#[derive(Deserialize)]
struct TransposeArgs {
    items: String,
    semitones: Option<i32>,
    from_key: Option<String>,
    to_key: Option<String>,
    prefer: Option<String>,
}

/// Default spelling of an upward interval of `n` semitones (0..12): minor seconds, thirds,
/// sixths and sevenths, a perfect fourth/fifth, and a diminished fifth for the tritone.
fn default_interval(n: i32) -> (usize, i32) {
    const TABLE: [(usize, i32); 12] = [
        (0, 0),
        (1, 1),
        (1, 2),
        (2, 3),
        (2, 4),
        (3, 5),
        (4, 6),
        (4, 7),
        (5, 8),
        (5, 9),
        (6, 10),
        (6, 11),
    ];
    TABLE[n.rem_euclid(12) as usize]
}

fn transpose_note(n: Note, steps: usize, semis: i32, prefer_flats: Option<bool>) -> Note {
    if let Some(flats) = prefer_flats {
        return spell_pc(n.pc() + semis, flats);
    }
    let t = n.up(steps, semis);
    if t.acc.abs() > 1 {
        spell_pc(t.pc(), n.acc < 0)
    } else {
        t
    }
}

fn transpose_tool(a: TransposeArgs) -> Result<String> {
    let (steps, semis, to_key) = match (&a.from_key, &a.to_key, a.semitones) {
        (Some(f), Some(t), _) => {
            let (from, to) = (Key::parse(f)?, Key::parse(t)?);
            let steps = (to.tonic.letter as usize + 7 - from.tonic.letter as usize) % 7;
            (steps, from.tonic.semis_to(to.tonic), Some(to))
        }
        (None, None, Some(n)) => {
            let (steps, semis) = default_interval(n);
            (steps, semis, None)
        }
        _ => bail!("give `semitones`, or both `from_key` and `to_key`"),
    };
    let prefer_flats = match a.prefer.as_deref() {
        Some("flats") => Some(true),
        Some("sharps") => Some(false),
        None => None,
        Some(other) => bail!("prefer must be sharps or flats, not `{other}`"),
    };
    let tokens = key::split_progression(&a.items);
    if tokens.is_empty() {
        bail!("`items` is empty");
    }
    let prefer = if to_key.is_some() { None } else { prefer_flats };
    let out: Vec<String> = tokens
        .iter()
        .map(|t| {
            let c = Chord::parse(t)?;
            let root = transpose_note(c.root, steps, semis, prefer);
            let bass = c.bass.map(|b| transpose_note(b, steps, semis, prefer));
            Ok(c.with_root(root, bass).name())
        })
        .collect::<Result<_>>()?;
    let interval = Interval::new(steps, semis).name();
    let mut text = format!(
        "Transposed up a {interval} ({semis} semitones):\n{}",
        out.join(" ")
    );
    if let Some(k) = to_key {
        text.push_str(&format!(
            "\n{} key signature: {}",
            k.name(),
            scale::signature_text(k.signature())
        ));
    }
    Ok(text)
}

#[derive(Deserialize)]
struct ProgressionArgs {
    chords: Option<String>,
    key: Option<String>,
    numerals: Option<String>,
}

fn kind_note(k: Kind) -> &'static str {
    match k {
        Kind::Diatonic | Kind::DiatonicHarmonic => "",
        _ => " *",
    }
}

fn progression_tool(a: ProgressionArgs) -> Result<String> {
    if let Some(numerals) = a.numerals {
        let key = Key::parse(
            a.key
                .as_deref()
                .ok_or_else(|| anyhow!("`key` is required with `numerals`"))?,
        )?;
        let tokens = key::split_progression(&numerals);
        let rows: Vec<String> = tokens
            .iter()
            .map(|n| {
                let c = key::realize(&key, n)?;
                Ok(format!(
                    "| {n} | {} | {} |",
                    c.name(),
                    join_names(&c.notes())
                ))
            })
            .collect::<Result<_>>()?;
        return Ok(format!(
            "In {} ({}):\n| Numeral | Chord | Notes |\n|---|---|---|\n{}",
            key.name(),
            scale::signature_text(key.signature()),
            rows.join("\n")
        ));
    }
    let Some(chords) = a.chords else {
        bail!("give `chords` to analyse or `numerals` (with `key`) to realize");
    };
    let parsed: Vec<Chord> = key::split_progression(&chords)
        .iter()
        .map(|c| Chord::parse(c))
        .collect::<Result<_>>()?;
    if parsed.is_empty() {
        bail!("`chords` is empty");
    }
    let guesses = key::detect(&parsed);
    let mut out = String::new();
    let used = match a.key {
        Some(k) => {
            let k = Key::parse(&k)?;
            out.push_str(&format!("Analysing in {} (as requested).\n", k.name()));
            k
        }
        None => {
            let top = &guesses[0];
            out.push_str(&format!(
                "Most likely key: {} (confidence {:.0}%, {}/{} chords diatonic).\n",
                top.key.name(),
                top.confidence * 100.0,
                top.diatonic,
                parsed.len()
            ));
            let alts: Vec<String> = guesses[1..]
                .iter()
                .take(3)
                .filter(|g| g.confidence > top.confidence - 0.35)
                .map(|g| format!("{} ({:.0}%)", g.key.name(), g.confidence * 100.0))
                .collect();
            if !alts.is_empty() {
                out.push_str(&format!("Alternatives: {}.\n", alts.join(", ")));
            }
            if guesses[1].key == top.key.relative() && top.confidence - guesses[1].confidence < 0.2
            {
                out.push_str(&format!(
                    "Note: {} and {} share the same notes; the choice rests on where the progression resolves.\n",
                    top.key.name(),
                    guesses[1].key.name()
                ));
            }
            top.key
        }
    };
    out.push_str("| Chord | Numeral | Function |\n|---|---|---|\n");
    let mut any_outside = false;
    for c in &parsed {
        let an = key::analyze(&used, c);
        any_outside |= !kind_note(an.kind).is_empty();
        out.push_str(&format!(
            "| {} | {} | {}{} |\n",
            c.name(),
            an.numeral,
            an.role,
            kind_note(an.kind)
        ));
    }
    if any_outside {
        out.push_str("\\* outside the plain diatonic scale of the key.\n");
    }
    Ok(out.trim_end().to_string())
}

#[derive(Deserialize)]
struct TempoArgs {
    bpm: Option<f64>,
    time_signature: Option<String>,
    sample_rate: Option<f64>,
    bars: Option<f64>,
    duration_seconds: Option<f64>,
    quarter_note_ms: Option<f64>,
    note_value: Option<String>,
}

fn positive(name: &str, v: f64) -> Result<f64> {
    if v.is_finite() && v > 0.0 {
        Ok(v)
    } else {
        bail!("{name} must be a positive number")
    }
}

fn tempo_tool(a: TempoArgs) -> Result<String> {
    let (num, den) = tempo::parse_time_signature(a.time_signature.as_deref().unwrap_or("4/4"))?;
    let mut derived = None;
    let bpm = match (a.bpm, a.bars, a.duration_seconds, a.quarter_note_ms) {
        (Some(b), ..) => positive("bpm", b)?,
        (None, Some(bars), Some(secs), _) => {
            let b = tempo::bpm_from_duration(
                positive("bars", bars)?,
                num,
                den,
                positive("duration_seconds", secs)?,
            );
            derived = Some(format!(
                "{bars} bars of {num}/{den} in {secs} s is {b:.3} bpm (quarter-note beats)."
            ));
            b
        }
        (None, _, _, Some(ms)) => {
            let b = 60_000.0 / positive("quarter_note_ms", ms)?;
            derived = Some(format!("A {ms} ms quarter note is {b:.3} bpm."));
            b
        }
        _ => bail!("give `bpm`, or `bars` with `duration_seconds`, or `quarter_note_ms`"),
    };
    let mut out = String::new();
    if let Some(d) = derived {
        out.push_str(&d);
        out.push('\n');
    }
    let bar = tempo::bar_ms(bpm, num, den);
    out.push_str(&format!(
        "{bpm:.3} bpm in {num}/{den}: one bar = {bar:.2} ms ({:.3} s)",
        bar / 1000.0
    ));
    if let Some(sr) = a.sample_rate {
        let sr = positive("sample_rate", sr)?;
        out.push_str(&format!(
            " = {:.0} samples at {sr:.0} Hz",
            tempo::samples(bar, sr)
        ));
    }
    if let Some(nv) = &a.note_value {
        let (den, feel) = tempo::parse_note_value(nv)?;
        let ms = tempo::note_ms(bpm, den, feel);
        out.push_str(&format!(
            "\n{nv} at {bpm:.3} bpm = {ms:.2} ms ({:.3} Hz)",
            tempo::hz_from_ms(ms)
        ));
        if let Some(sr) = a.sample_rate {
            out.push_str(&format!(
                " = {:.0} samples at {sr:.0} Hz",
                tempo::samples(ms, sr)
            ));
        }
    }
    out.push_str("\n| Note | Straight | Dotted | Triplet |\n|---|---|---|---|\n");
    for (label, den) in tempo::NOTE_VALUES {
        let cell = |feel: Feel| {
            let ms = tempo::note_ms(bpm, den, feel);
            let mut s = format!("{ms:.2} ms ({:.3} Hz)", tempo::hz_from_ms(ms));
            if let Some(sr) = a.sample_rate {
                s.push_str(&format!(", {:.0} smp", tempo::samples(ms, sr)));
            }
            s
        };
        out.push_str(&format!(
            "| {label} | {} | {} | {} |\n",
            cell(Feel::Straight),
            cell(Feel::Dotted),
            cell(Feel::Triplet)
        ));
    }
    Ok(out.trim_end().to_string())
}

#[derive(Deserialize)]
struct PitchArgs {
    name: Option<String>,
    midi: Option<f64>,
    hz: Option<f64>,
    a4: Option<f64>,
}

fn pitch_tool(a: PitchArgs) -> Result<String> {
    let a4 = positive("a4", a.a4.unwrap_or(440.0))?;
    let (midi, exact) = match (&a.name, a.midi, a.hz) {
        (Some(n), ..) => (Pitch::parse(n)?.midi() as f64, true),
        (None, Some(m), _) if m.is_finite() => (m, m.fract() == 0.0),
        (None, None, Some(h)) => (hz_to_midi(positive("hz", h)?, a4), false),
        _ => bail!("give one of `name`, `midi` or `hz`"),
    };
    let nearest = midi.round() as i32;
    let hz = midi_to_hz(midi, a4);
    let sharp = Pitch::from_midi(nearest, false);
    let flat = Pitch::from_midi(nearest, true);
    let names = if sharp.name() == flat.name() {
        sharp.name()
    } else {
        format!("{} / {}", sharp.name(), flat.name())
    };
    let mut out = format!(
        "MIDI {nearest} = {names}, {:.3} Hz (A4 = {a4} Hz)",
        midi_to_hz(nearest as f64, a4)
    );
    if !exact {
        let cents = cents_between(midi_to_hz(nearest as f64, a4), hz);
        out =
            format!("{hz:.3} Hz is nearest to {names} (MIDI {nearest}), {cents:+.1} cents. {out}");
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(name: &str, args: Value) -> String {
        execute(name, args).unwrap().text
    }

    #[test]
    fn scale_output() {
        let t = run("theory_scale", json!({"root": "C#", "scale": "major"}));
        assert!(t.contains("Notes: C# D# E# F# G# A# B#"), "{t}");
        assert!(t.contains("7 sharps"), "{t}");
        assert!(t.contains("Diatonic triads: C# D#m E#m F# G# A#m"), "{t}");
        assert!(
            t.contains("Diatonic sevenths: C#maj7 D#m7 E#m7 F#maj7 G#7 A#m7"),
            "{t}"
        );
        assert!(run("theory_scale", json!({"root": "C"})).contains("dorian"));
        assert!(execute("theory_scale", json!({"root": "C", "scale": "nope"})).is_err());
    }

    #[test]
    fn chord_output() {
        let t = run("theory_chord", json!({"symbol": "Bb13#11/D"}));
        assert!(t.contains("Notes: Bb D F Ab C E G"), "{t}");
        assert!(t.contains("Formula: 1 3 5 b7 9 #11 13"), "{t}");
        assert!(t.contains("first inversion"), "{t}");
        let t = run("theory_chord", json!({"notes": ["C", "E", "G", "Bb"]}));
        assert!(t.contains("1. C7"), "{t}");
        assert!(execute("theory_chord", json!({})).is_err());
    }

    #[test]
    fn transpose_by_key_keeps_spelling() {
        let t = run(
            "theory_transpose",
            json!({"items": "Dm7 G7 Cmaj7", "from_key": "C major", "to_key": "Db major"}),
        );
        assert!(t.contains("Ebm7 Ab7 Dbmaj7"), "{t}");
        assert!(t.contains("5 flats"), "{t}");
        let t = run(
            "theory_transpose",
            json!({"items": "C F G7/B", "semitones": 2}),
        );
        assert!(t.contains("D G A7/C#"), "{t}");
        let t = run(
            "theory_transpose",
            json!({"items": "Am F", "semitones": -3}),
        );
        assert!(t.contains("F#m D"), "{t}");
        let t = run(
            "theory_transpose",
            json!({"items": "C", "semitones": 1, "prefer": "sharps"}),
        );
        assert!(t.contains("C#"), "{t}");
        assert!(execute("theory_transpose", json!({"items": "C"})).is_err());
    }

    #[test]
    fn progression_analysis_and_realization() {
        let t = run("theory_progression", json!({"chords": "Dm7 G7 Cmaj7"}));
        assert!(t.contains("Most likely key: C major"), "{t}");
        assert!(t.contains("| Dm7 | ii7 |"), "{t}");
        assert!(t.contains("| G7 | V7 | dominant |"), "{t}");
        let t = run(
            "theory_progression",
            json!({"chords": "C A7 Dm G7", "key": "C"}),
        );
        assert!(t.contains("V7/ii"), "{t}");
        let t = run(
            "theory_progression",
            json!({"numerals": "ii7 V7 Imaj7", "key": "Eb major"}),
        );
        assert!(
            t.contains("Fm7") && t.contains("Bb7") && t.contains("Ebmaj7"),
            "{t}"
        );
        assert!(execute("theory_progression", json!({"numerals": "V"})).is_err());
    }

    #[test]
    fn tempo_output() {
        let t = run("theory_tempo", json!({"bpm": 120, "sample_rate": 48000}));
        assert!(t.contains("one bar = 2000.00 ms"), "{t}");
        assert!(
            t.contains("| 1/8 | 250.00 ms (4.000 Hz), 12000 smp | 375.00 ms"),
            "{t}"
        );
        let t = run("theory_tempo", json!({"bars": 4, "duration_seconds": 8}));
        assert!(t.contains("120.000 bpm"), "{t}");
        let t = run("theory_tempo", json!({"bpm": 120, "note_value": "1/8."}));
        assert!(t.contains("1/8. at 120.000 bpm = 375.00 ms"), "{t}");
        assert!(execute("theory_tempo", json!({"bpm": 120, "note_value": "1/3"})).is_err());
        assert!(execute("theory_tempo", json!({})).is_err());
        assert!(execute("theory_tempo", json!({"bpm": -4})).is_err());
    }

    #[test]
    fn pitch_output() {
        let t = run("theory_pitch", json!({"name": "A4"}));
        assert!(t.contains("MIDI 69 = A4, 440.000 Hz"), "{t}");
        let t = run("theory_pitch", json!({"hz": 450}));
        assert!(t.contains("A4") && t.contains("+38.9 cents"), "{t}");
        let t = run("theory_pitch", json!({"midi": 61}));
        assert!(t.contains("C#4 / Db4"), "{t}");
        let t = run("theory_pitch", json!({"name": "A4", "a4": 432}));
        assert!(t.contains("432.000 Hz"), "{t}");
        assert!(execute("theory_pitch", json!({})).is_err());
    }

    #[test]
    fn titles() {
        assert_eq!(
            title("theory_chord", &json!({"symbol": "Cm7"})),
            "Chord: Cm7"
        );
        assert_eq!(
            title("theory_scale", &json!({"root": "D", "scale": "dorian"})),
            "Scale: D dorian"
        );
    }
}

//! Scales and modes, spelled diatonically from a formula of degree tokens.

use anyhow::{Result, bail};

use super::interval::Interval;
use super::pitch::Note;

pub struct ScaleDef {
    pub name: &'static str,
    pub aliases: &'static [&'static str],
    /// Degree tokens relative to the major scale.
    pub formula: &'static str,
}

const fn def(
    name: &'static str,
    aliases: &'static [&'static str],
    formula: &'static str,
) -> ScaleDef {
    ScaleDef {
        name,
        aliases,
        formula,
    }
}

pub const SCALES: &[ScaleDef] = &[
    def("major", &["ionian", "maj"], "1 2 3 4 5 6 7"),
    def(
        "natural minor",
        &["minor", "aeolian", "min"],
        "1 2 b3 4 5 b6 b7",
    ),
    def("dorian", &[], "1 2 b3 4 5 6 b7"),
    def("phrygian", &[], "1 b2 b3 4 5 b6 b7"),
    def("lydian", &[], "1 2 3 #4 5 6 7"),
    def("mixolydian", &["dominant"], "1 2 3 4 5 6 b7"),
    def("locrian", &[], "1 b2 b3 4 b5 b6 b7"),
    def("harmonic minor", &[], "1 2 b3 4 5 b6 7"),
    def("melodic minor", &["jazz minor"], "1 2 b3 4 5 6 7"),
    def("harmonic major", &[], "1 2 3 4 5 b6 7"),
    def("dorian b2", &["phrygian #6"], "1 b2 b3 4 5 6 b7"),
    def("lydian augmented", &[], "1 2 3 #4 #5 6 7"),
    def(
        "lydian dominant",
        &["overtone", "acoustic"],
        "1 2 3 #4 5 6 b7",
    ),
    def(
        "mixolydian b6",
        &["hindu", "melodic major"],
        "1 2 3 4 5 b6 b7",
    ),
    def(
        "locrian #2",
        &["half-diminished", "aeolian b5"],
        "1 2 b3 4 b5 b6 b7",
    ),
    def(
        "altered",
        &["super locrian", "diminished whole tone"],
        "1 b2 b3 b4 b5 b6 b7",
    ),
    def(
        "phrygian dominant",
        &["spanish", "freygish"],
        "1 b2 3 4 5 b6 b7",
    ),
    def("major pentatonic", &["pentatonic major"], "1 2 3 5 6"),
    def(
        "minor pentatonic",
        &["pentatonic minor", "pentatonic"],
        "1 b3 4 5 b7",
    ),
    def("blues", &["minor blues"], "1 b3 4 b5 5 b7"),
    def("major blues", &[], "1 2 b3 3 5 6"),
    def("whole tone", &[], "1 2 3 #4 #5 b7"),
    def(
        "diminished whole-half",
        &["octatonic whole-half", "diminished"],
        "1 2 b3 4 b5 b6 6 7",
    ),
    def(
        "diminished half-whole",
        &["octatonic half-whole", "dominant diminished"],
        "1 b2 b3 3 #4 5 6 b7",
    ),
    def("bebop dominant", &[], "1 2 3 4 5 6 b7 7"),
];

fn norm(s: &str) -> String {
    s.trim()
        .to_lowercase()
        .replace(['_', '-'], " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn find(name: &str) -> Option<&'static ScaleDef> {
    let n = norm(name);
    SCALES
        .iter()
        .find(|d| norm(d.name) == n || d.aliases.iter().any(|a| norm(a) == n))
}

pub fn names() -> Vec<&'static str> {
    SCALES.iter().map(|d| d.name).collect()
}

impl ScaleDef {
    pub fn intervals(&self) -> Vec<Interval> {
        self.formula
            .split_whitespace()
            .filter_map(Interval::from_token)
            .collect()
    }

    pub fn spell(&self, root: Note) -> Vec<Note> {
        self.intervals()
            .into_iter()
            .map(|i| root.up(i.steps, i.semis))
            .collect()
    }
}

pub fn parse_scale(name: &str) -> Result<&'static ScaleDef> {
    match find(name) {
        Some(d) => Ok(d),
        None => bail!("unknown scale `{name}`; known: {}", names().join(", ")),
    }
}

/// Signed accidental count of a diatonic scale's key signature (+ sharps, - flats).
pub fn signature(notes: &[Note]) -> i32 {
    notes.iter().map(|n| n.acc as i32).sum()
}

/// Names the accidentals of a key signature in order of appearance, e.g. `F# C#`.
pub fn signature_text(count: i32) -> String {
    const SHARPS: [&str; 7] = ["F#", "C#", "G#", "D#", "A#", "E#", "B#"];
    const FLATS: [&str; 7] = ["Bb", "Eb", "Ab", "Db", "Gb", "Cb", "Fb"];
    match count {
        0 => "no sharps or flats".into(),
        n if n > 0 && n <= 7 => format!(
            "{n} sharp{} ({})",
            plural(n),
            SHARPS[..n as usize].join(" ")
        ),
        n if (-7..0).contains(&n) => {
            let n = -n;
            format!("{n} flat{} ({})", plural(n), FLATS[..n as usize].join(" "))
        }
        n => format!("{n} accidentals (theoretical key with double accidentals)"),
    }
}

fn plural(n: i32) -> &'static str {
    if n == 1 { "" } else { "s" }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spelled(root: &str, scale: &str) -> String {
        let notes = parse_scale(scale)
            .unwrap()
            .spell(Note::parse(root).unwrap());
        notes.iter().map(|n| n.name()).collect::<Vec<_>>().join(" ")
    }

    #[test]
    fn c_sharp_major_has_e_sharp_and_b_sharp() {
        assert_eq!(spelled("C#", "major"), "C# D# E# F# G# A# B#");
        let notes = parse_scale("major")
            .unwrap()
            .spell(Note::parse("C#").unwrap());
        assert_eq!(signature(&notes), 7);
    }

    #[test]
    fn flat_keys_and_modes() {
        assert_eq!(spelled("Gb", "major"), "Gb Ab Bb Cb Db Eb F");
        assert_eq!(spelled("D", "dorian"), "D E F G A B C");
        assert_eq!(spelled("F#", "locrian"), "F# G A B C D E");
        assert_eq!(spelled("A", "harmonic minor"), "A B C D E F G#");
        assert_eq!(spelled("C", "lydian"), "C D E F# G A B");
        assert_eq!(spelled("C", "altered"), "C Db Eb Fb Gb Ab Bb");
    }

    #[test]
    fn pentatonic_and_blues() {
        assert_eq!(spelled("A", "minor pentatonic"), "A C D E G");
        assert_eq!(spelled("C", "major pentatonic"), "C D E G A");
        assert_eq!(spelled("E", "blues"), "E G A Bb B D");
        assert_eq!(spelled("C", "whole tone"), "C D E F# G# Bb");
    }

    #[test]
    fn aliases_and_unknown() {
        assert!(find("Aeolian").is_some());
        assert!(find("natural-minor").is_some());
        assert!(parse_scale("klingon").is_err());
    }

    #[test]
    fn signature_text_names_accidentals() {
        assert_eq!(signature_text(2), "2 sharps (F# C#)");
        assert_eq!(signature_text(-1), "1 flat (Bb)");
        assert_eq!(signature_text(0), "no sharps or flats");
    }
}

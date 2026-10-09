//! Spelled notes, MIDI numbers and frequencies.

use anyhow::{Result, bail};

/// Pitch class of each natural letter, C D E F G A B.
pub const NATURAL_PC: [i32; 7] = [0, 2, 4, 5, 7, 9, 11];
const LETTERS: [char; 7] = ['C', 'D', 'E', 'F', 'G', 'A', 'B'];

/// A spelled pitch class: a letter plus an accidental (+1 sharp, -1 flat, ...).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Note {
    /// 0 = C ... 6 = B.
    pub letter: u8,
    pub acc: i8,
}

impl Note {
    pub fn new(letter: u8, acc: i8) -> Self {
        Self {
            letter: letter % 7,
            acc,
        }
    }

    pub fn pc(self) -> i32 {
        (NATURAL_PC[self.letter as usize] + self.acc as i32).rem_euclid(12)
    }

    pub fn name(self) -> String {
        let mut s = LETTERS[self.letter as usize].to_string();
        match self.acc {
            0 => {}
            n if n > 0 => s.push_str(&"#".repeat(n as usize)),
            n => s.push_str(&"b".repeat((-n) as usize)),
        }
        s
    }

    /// Parse `C`, `F#`, `Bb`, `Ebb`, `C##`, `Fx`, `B♭`. Returns the note and the unparsed rest.
    pub fn parse_prefix(s: &str) -> Option<(Note, &str)> {
        let mut chars = s.chars();
        let first = chars.next()?;
        let letter = LETTERS
            .iter()
            .position(|&l| l == first.to_ascii_uppercase())?;
        // Lowercase letters are only accepted by the caller when it asks for them.
        if !first.is_ascii_uppercase() {
            return None;
        }
        let mut acc = 0i8;
        let mut rest = &s[first.len_utf8()..];
        while let Some(c) = rest.chars().next() {
            let delta = match c {
                '#' | '♯' => 1,
                'x' | '𝄪' => 2,
                // `b` is an accidental here; the caller decides whether a trailing `b` after a
                // root could instead be part of a quality (it never is for Bb/Ebb style roots).
                'b' | '♭' => -1,
                _ => break,
            };
            acc += delta;
            rest = &rest[c.len_utf8()..];
        }
        Some((Note::new(letter as u8, acc), rest))
    }

    pub fn parse(s: &str) -> Result<Note> {
        let s = s.trim();
        match Note::parse_prefix(s) {
            Some((n, "")) => Ok(n),
            _ => bail!("not a note name: `{s}` (use e.g. C, F#, Bb)"),
        }
    }

    /// Move up by `steps` letters and `semitones` semitones, spelling the result diatonically.
    pub fn up(self, steps: usize, semitones: i32) -> Note {
        let letter = (self.letter as usize + steps) % 7;
        let target = (self.pc() + semitones).rem_euclid(12);
        let mut acc = (target - NATURAL_PC[letter]).rem_euclid(12);
        if acc > 6 {
            acc -= 12;
        }
        Note {
            letter: letter as u8,
            acc: acc as i8,
        }
    }

    /// Semitones from `self` up to `other`, 0..12.
    pub fn semis_to(self, other: Note) -> i32 {
        (other.pc() - self.pc()).rem_euclid(12)
    }
}

/// Spell a pitch class using the conventional sharp or flat set.
pub fn spell_pc(pc: i32, prefer_flats: bool) -> Note {
    const SHARP: [(u8, i8); 12] = [
        (0, 0),
        (0, 1),
        (1, 0),
        (1, 1),
        (2, 0),
        (3, 0),
        (3, 1),
        (4, 0),
        (4, 1),
        (5, 0),
        (5, 1),
        (6, 0),
    ];
    const FLAT: [(u8, i8); 12] = [
        (0, 0),
        (1, -1),
        (1, 0),
        (2, -1),
        (2, 0),
        (3, 0),
        (4, -1),
        (4, 0),
        (5, -1),
        (5, 0),
        (6, -1),
        (6, 0),
    ];
    let (l, a) = if prefer_flats { FLAT } else { SHARP }[pc.rem_euclid(12) as usize];
    Note { letter: l, acc: a }
}

/// A note with an octave in scientific pitch notation (C4 = middle C = MIDI 60).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pitch {
    pub note: Note,
    pub octave: i32,
}

impl Pitch {
    pub fn parse(s: &str) -> Result<Pitch> {
        let s = s.trim();
        let Some((note, rest)) = Note::parse_prefix(s) else {
            bail!("not a pitch: `{s}` (use e.g. A4, C#3, Bb2)");
        };
        let octave: i32 = rest
            .parse()
            .map_err(|_| anyhow::anyhow!("not a pitch: `{s}` (octave number missing, e.g. A4)"))?;
        Ok(Pitch { note, octave })
    }

    pub fn midi(self) -> i32 {
        let base = NATURAL_PC[self.note.letter as usize] + self.note.acc as i32;
        (self.octave + 1) * 12 + base
    }

    pub fn name(self) -> String {
        format!("{}{}", self.note.name(), self.octave)
    }

    pub fn from_midi(midi: i32, prefer_flats: bool) -> Pitch {
        let note = spell_pc(midi, prefer_flats);
        // Octave follows the letter, so B#3 / Cb4 style spellings keep the right octave.
        let octave = (midi - NATURAL_PC[note.letter as usize] - note.acc as i32).div_euclid(12) - 1;
        Pitch { note, octave }
    }
}

pub fn midi_to_hz(midi: f64, a4: f64) -> f64 {
    a4 * 2f64.powf((midi - 69.0) / 12.0)
}

pub fn hz_to_midi(hz: f64, a4: f64) -> f64 {
    69.0 + 12.0 * (hz / a4).log2()
}

pub fn cents_between(hz_a: f64, hz_b: f64) -> f64 {
    1200.0 * (hz_b / hz_a).log2()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a4_is_midi_69_and_440() {
        let p = Pitch::parse("A4").unwrap();
        assert_eq!(p.midi(), 69);
        assert!((midi_to_hz(69.0, 440.0) - 440.0).abs() < 1e-9);
        assert!((hz_to_midi(440.0, 440.0) - 69.0).abs() < 1e-9);
        assert_eq!(Pitch::parse("C4").unwrap().midi(), 60);
        assert_eq!(Pitch::parse("C-1").unwrap().midi(), 0);
    }

    #[test]
    fn a4_432_shifts_frequencies() {
        assert!((midi_to_hz(69.0, 432.0) - 432.0).abs() < 1e-9);
        assert!((midi_to_hz(60.0, 432.0) - 256.87).abs() < 0.01);
    }

    #[test]
    fn enharmonic_octaves() {
        assert_eq!(Pitch::parse("Cb4").unwrap().midi(), 59);
        assert_eq!(Pitch::parse("B#3").unwrap().midi(), 60);
        assert_eq!(Pitch::from_midi(61, false).name(), "C#4");
        assert_eq!(Pitch::from_midi(61, true).name(), "Db4");
    }

    #[test]
    fn spelling_up() {
        let c_sharp = Note::parse("C#").unwrap();
        // major third above C# is E#, not F.
        assert_eq!(c_sharp.up(2, 4).name(), "E#");
        assert_eq!(c_sharp.up(6, 11).name(), "B#");
        assert_eq!(Note::parse("Bb").unwrap().up(2, 4).name(), "D");
        assert_eq!(Note::parse("Fb").unwrap().up(4, 7).name(), "Cb");
    }

    #[test]
    fn cents() {
        assert!((cents_between(440.0, 440.0 * 2f64.powf(1.0 / 12.0)) - 100.0).abs() < 1e-6);
    }
}

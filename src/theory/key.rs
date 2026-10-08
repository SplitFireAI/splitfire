//! Keys, Roman-numeral analysis, key detection from a progression, and realizing numerals.

use anyhow::{Result, bail};

use super::chord::Chord;
use super::pitch::Note;
use super::scale;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Major,
    Minor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Key {
    pub tonic: Note,
    pub mode: Mode,
}

const MAJOR_REF: [i32; 7] = [0, 2, 4, 5, 7, 9, 11];
const MINOR_REF: [i32; 7] = [0, 2, 3, 5, 7, 8, 10];
const ROMAN: [&str; 7] = ["I", "II", "III", "IV", "V", "VI", "VII"];

impl Key {
    /// `C`, `C major`, `Am`, `A minor`, `F#m`, `Bb`, `Ebmin`.
    pub fn parse(s: &str) -> Result<Key> {
        let s = s.trim();
        let mut chars = s.chars();
        let Some(first) = chars.next().filter(|c| c.is_ascii_alphabetic()) else {
            bail!("not a key: `{s}` (use e.g. C, Am, F# minor, Bb major)");
        };
        let mut rest = &s[1..];
        let mut note = first.to_ascii_uppercase().to_string();
        if let Some(c) = rest
            .chars()
            .next()
            .filter(|c| matches!(c, '#' | '♯' | 'b' | '♭'))
        {
            // `Bb`, `F#`; but not a bare mode word like `b minor` → handled since `b` then space.
            note.push(if matches!(c, '#' | '♯') { '#' } else { 'b' });
            rest = &rest[c.len_utf8()..];
        }
        let tonic = Note::parse(&note)?;
        let mode = match rest.trim().to_lowercase().as_str() {
            "" | "major" | "maj" | "ionian" => Mode::Major,
            "m" | "min" | "minor" | "aeolian" => Mode::Minor,
            other => bail!("unknown key mode `{other}` in `{s}` (use major or minor)"),
        };
        Ok(Key { tonic, mode })
    }

    pub fn name(&self) -> String {
        format!(
            "{} {}",
            self.tonic.name(),
            match self.mode {
                Mode::Major => "major",
                Mode::Minor => "minor",
            }
        )
    }

    fn reference(&self) -> &'static [i32; 7] {
        match self.mode {
            Mode::Major => &MAJOR_REF,
            Mode::Minor => &MINOR_REF,
        }
    }

    /// The seven scale notes (major or natural minor), spelled.
    pub fn notes(&self) -> Vec<Note> {
        let r = self.reference();
        (0..7).map(|i| self.tonic.up(i, r[i])).collect()
    }

    pub fn pcs(&self) -> Vec<i32> {
        self.notes().iter().map(|n| n.pc()).collect()
    }

    pub fn signature(&self) -> i32 {
        scale::signature(&self.notes())
    }

    /// The note on `degree` (0-based) raised/lowered by `acc` semitones from the key's scale.
    fn degree_note(&self, degree: usize, acc: i32) -> Note {
        self.tonic.up(degree, self.reference()[degree] + acc)
    }

    pub fn relative(&self) -> Key {
        match self.mode {
            Mode::Major => Key {
                tonic: self.tonic.up(5, 9),
                mode: Mode::Minor,
            },
            Mode::Minor => Key {
                tonic: self.tonic.up(2, 3),
                mode: Mode::Major,
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Diatonic,
    DiatonicHarmonic,
    SecondaryDominant,
    SecondaryLeadingTone,
    TritoneSub,
    Neapolitan,
    Borrowed,
    Chromatic,
}

#[derive(Debug, Clone)]
pub struct ChordAnalysis {
    pub numeral: String,
    pub kind: Kind,
    pub role: String,
}

fn semis_diff(semis: i32, reference: i32) -> i32 {
    (semis - reference + 6).rem_euclid(12) - 6
}

fn accidental_prefix(diff: i32) -> String {
    if diff > 0 {
        "#".repeat(diff as usize)
    } else {
        "b".repeat((-diff) as usize)
    }
}

/// Numeral decoration derived from the canonical chord suffix (`ii7`, `viiø7`, `V7#9`, `I+`).
fn numeral_suffix(suffix: &str) -> String {
    if let Some(rest) = suffix.strip_prefix("m7b5") {
        format!("ø7{rest}")
    } else if let Some(rest) = suffix.strip_prefix("dim7") {
        format!("°7{rest}")
    } else if let Some(rest) = suffix.strip_prefix("dim") {
        format!("°{rest}")
    } else if let Some(rest) = suffix.strip_prefix("aug") {
        format!("+{rest}")
    } else if let Some(rest) = suffix.strip_prefix("mMaj") {
        format!("(maj{rest})")
    } else if suffix.starts_with("maj") {
        suffix.to_string()
    } else if let Some(rest) = suffix.strip_prefix('m') {
        rest.to_string()
    } else {
        suffix.to_string()
    }
}

fn numeral_for(key: &Key, chord: &Chord) -> (String, usize) {
    let steps = (chord.root.letter as usize + 7 - key.tonic.letter as usize) % 7;
    let semis = key.tonic.semis_to(chord.root);
    let mut diff = semis_diff(semis, key.reference()[steps]);
    if key.mode == Mode::Minor && steps == 6 && diff == 1 {
        diff = 0; // leading-tone vii° of harmonic minor
    }
    let minor_like = chord.is_minor_third();
    let base = if minor_like {
        ROMAN[steps].to_lowercase()
    } else {
        ROMAN[steps].to_string()
    };
    let mut numeral = format!(
        "{}{}{}",
        accidental_prefix(diff),
        base,
        numeral_suffix(&chord.suffix())
    );
    if let Some(b) = chord.bass.filter(|b| b.pc() != chord.root.pc()) {
        numeral.push('/');
        numeral.push_str(&b.name());
    }
    (numeral, steps)
}

/// Triad numeral of the diatonic chord on scale degree `idx` (`ii`, `V`, `vii°`).
fn diatonic_numeral(key: &Key, idx: usize) -> String {
    let notes = key.notes();
    let third = notes[idx].semis_to(notes[(idx + 2) % 7]);
    let fifth = notes[idx].semis_to(notes[(idx + 4) % 7]);
    match (third, fifth) {
        (3, 6) => format!("{}°", ROMAN[idx].to_lowercase()),
        (3, _) => ROMAN[idx].to_lowercase(),
        _ => ROMAN[idx].to_string(),
    }
}

fn mode_pcs(tonic: Note, name: &str) -> Vec<i32> {
    scale::find(name)
        .map(|d| d.spell(tonic).iter().map(|n| n.pc()).collect())
        .unwrap_or_default()
}

fn function_name(key: &Key, steps: usize) -> &'static str {
    match steps {
        0 => "tonic",
        1 => "supertonic (predominant)",
        2 => match key.mode {
            Mode::Major => "mediant (tonic function)",
            Mode::Minor => "mediant (relative major, tonic function)",
        },
        3 => "subdominant (predominant)",
        4 => "dominant",
        5 => match key.mode {
            Mode::Major => "submediant (tonic function)",
            Mode::Minor => "submediant (predominant function)",
        },
        _ => match key.mode {
            Mode::Major => "leading-tone (dominant function)",
            Mode::Minor => "subtonic (dominant function)",
        },
    }
}

pub fn analyze(key: &Key, chord: &Chord) -> ChordAnalysis {
    let (numeral, steps) = numeral_for(key, chord);
    let done = |kind, role: String| ChordAnalysis {
        numeral: numeral.clone(),
        kind,
        role,
    };
    let scale_pcs = key.pcs();
    let chord_pcs = chord.pcs();
    let in_scale = |pcs: &[i32], scale: &[i32]| pcs.iter().all(|p| scale.contains(p));

    if in_scale(&chord_pcs, &scale_pcs) {
        let in_scale_root = scale_pcs.contains(&chord.root.pc());
        let role = if in_scale_root {
            function_name(key, steps).to_string()
        } else {
            "diatonic".to_string()
        };
        return done(Kind::Diatonic, role);
    }
    if key.mode == Mode::Minor && matches!(steps, 4 | 6) {
        let harmonic = mode_pcs(key.tonic, "harmonic minor");
        if in_scale(&chord_pcs, &harmonic) {
            return done(
                Kind::DiatonicHarmonic,
                format!(
                    "{} (raised leading tone, harmonic minor)",
                    function_name(key, steps)
                ),
            );
        }
    }

    let rel = key.tonic.semis_to(chord.root);
    let notes = key.notes();
    let target_for = |target_pc: i32| -> Option<usize> {
        notes
            .iter()
            .position(|n| n.pc() == target_pc)
            .filter(|&i| i != 0 && !diatonic_numeral(key, i).ends_with('°'))
    };
    let dominant_type = chord.has(2, 4) && (chord.seventh().is_none() || chord.has(6, 10));
    let suffix = chord.suffix();
    if dominant_type && let Some(t) = target_for((chord.root.pc() + 5) % 12) {
        return done(
            Kind::SecondaryDominant,
            format!(
                "secondary dominant (V{suffix}/{})",
                diatonic_numeral(key, t)
            ),
        );
    }
    let dim_type = chord.is_minor_third() && chord.has(4, 6);
    if dim_type && let Some(t) = target_for((chord.root.pc() + 1) % 12) {
        let kind = if chord.has(6, 9) {
            "°7"
        } else if chord.has(6, 10) {
            "ø7"
        } else {
            "°"
        };
        return done(
            Kind::SecondaryLeadingTone,
            format!(
                "secondary leading-tone chord (vii{kind}/{})",
                diatonic_numeral(key, t)
            ),
        );
    }
    if chord.is_dominant() {
        let down = (chord.root.pc() + 11) % 12;
        if down == key.tonic.pc() {
            return done(
                Kind::TritoneSub,
                "tritone substitution of V7 (subV7), resolves down by half step to the tonic"
                    .into(),
            );
        }
        if let Some(t) = target_for(down) {
            return done(
                Kind::TritoneSub,
                format!(
                    "tritone substitution of V7/{} (subV7/{})",
                    diatonic_numeral(key, t),
                    diatonic_numeral(key, t)
                ),
            );
        }
    }
    if rel == 1 && chord.has(2, 4) && chord.seventh().is_none() {
        return done(
            Kind::Neapolitan,
            "Neapolitan chord (bII), usually in first inversion before V".into(),
        );
    }
    let parallel: &[(&str, &str)] = match key.mode {
        Mode::Major => &[
            ("natural minor", "parallel minor"),
            ("dorian", "dorian"),
            ("mixolydian", "mixolydian"),
            ("lydian", "lydian"),
            ("phrygian", "phrygian"),
            ("harmonic minor", "parallel harmonic minor"),
            ("melodic minor", "parallel melodic minor"),
        ],
        Mode::Minor => &[
            ("major", "parallel major"),
            ("dorian", "dorian"),
            ("mixolydian", "mixolydian"),
            ("lydian", "lydian"),
            ("phrygian", "phrygian"),
            ("melodic minor", "melodic minor"),
        ],
    };
    for (scale_name, label) in parallel {
        if in_scale(&chord_pcs, &mode_pcs(key.tonic, scale_name)) {
            return done(
                Kind::Borrowed,
                format!(
                    "borrowed chord (modal interchange from {} {label})",
                    key.tonic.name()
                ),
            );
        }
    }
    done(
        Kind::Chromatic,
        "chromatic: outside the key and not a common function".into(),
    )
}

#[derive(Debug, Clone)]
pub struct KeyGuess {
    pub key: Key,
    pub score: f64,
    pub confidence: f64,
    pub diatonic: usize,
}

const MAJOR_TONICS: [&str; 15] = [
    "C", "G", "D", "A", "E", "B", "F#", "C#", "F", "Bb", "Eb", "Ab", "Db", "Gb", "Cb",
];
const MINOR_TONICS: [&str; 15] = [
    "A", "E", "B", "F#", "C#", "G#", "D#", "A#", "D", "G", "C", "F", "Bb", "Eb", "Ab",
];

/// Rank candidate keys for a progression. Scores reward diatonic fit, a tonic at the start
/// and end, and V→I motion; borrowed chords and secondary dominants count for less.
pub fn detect(chords: &[Chord]) -> Vec<KeyGuess> {
    if chords.is_empty() {
        return Vec::new();
    }
    let mut guesses = Vec::new();
    for (tonics, mode) in [(MAJOR_TONICS, Mode::Major), (MINOR_TONICS, Mode::Minor)] {
        for t in tonics {
            let key = Key {
                tonic: Note::parse(t).unwrap(),
                mode,
            };
            let analyses: Vec<ChordAnalysis> = chords.iter().map(|c| analyze(&key, c)).collect();
            let mut score = 0.0;
            let mut diatonic = 0;
            for a in &analyses {
                score += match a.kind {
                    Kind::Diatonic | Kind::DiatonicHarmonic => {
                        diatonic += 1;
                        1.0
                    }
                    Kind::SecondaryDominant | Kind::SecondaryLeadingTone => 0.5,
                    Kind::TritoneSub | Kind::Neapolitan => 0.4,
                    Kind::Borrowed => 0.3,
                    Kind::Chromatic => 0.0,
                };
            }
            let is_tonic = |c: &Chord| {
                c.root.pc() == key.tonic.pc()
                    && (c.is_minor_third() == (mode == Mode::Minor))
                    && c.third().is_some()
            };
            if is_tonic(&chords[0]) {
                score += 0.9;
            }
            if chords.len() > 1 && is_tonic(&chords[chords.len() - 1]) {
                score += 0.75;
            }
            let has_cadence = chords.windows(2).any(|w| {
                w[0].root.pc() == (key.tonic.pc() + 7) % 12 && w[0].has(2, 4) && is_tonic(&w[1])
            });
            if has_cadence {
                score += 0.5;
            }
            // Prefer the spelling with fewer accidentals when keys are enharmonic.
            score -= key.signature().unsigned_abs() as f64 * 0.001;
            let max = chords.len() as f64 + 2.15;
            guesses.push(KeyGuess {
                key,
                score,
                confidence: (score / max).clamp(0.0, 1.0),
                diatonic,
            });
        }
    }
    guesses.sort_by(|a, b| b.score.total_cmp(&a.score));
    guesses
}

fn parse_roman(s: &str) -> Option<(usize, &str)> {
    let upper = s.to_ascii_uppercase();
    // Longest match first so IV isn't read as I + V.
    for (i, name) in ROMAN.iter().enumerate().rev() {
        if upper.starts_with(name) {
            return Some((i, &s[name.len()..]));
        }
    }
    None
}

/// Realize a Roman numeral (`ii7`, `V7`, `Imaj7`, `bVII`, `vii°7`, `V7/V`) in `key`.
pub fn realize(key: &Key, numeral: &str) -> Result<Chord> {
    let numeral = numeral.trim();
    if let Some((head, target)) = numeral.split_once('/') {
        let target_chord = realize(key, target)?;
        let sub_key = Key {
            tonic: target_chord.root,
            mode: Mode::Major,
        };
        return realize(&sub_key, head);
    }
    let body = numeral.trim_start_matches(['#', 'b']);
    let acc: i32 = numeral[..numeral.len() - body.len()]
        .chars()
        .map(|c| if c == '#' { 1 } else { -1 })
        .sum();
    let Some((degree, suffix)) = parse_roman(body) else {
        bail!("not a Roman numeral: `{numeral}` (use e.g. ii7, V7, Imaj7, bVII, vii°7)");
    };
    let letters = &body[..body.len() - suffix.len()];
    let lower = letters.chars().all(|c| c.is_ascii_lowercase());
    let root = key.degree_note(degree, acc);
    let quality = if let Some(r) = suffix
        .strip_prefix(['°', 'º', 'o'])
        .or_else(|| suffix.strip_prefix("dim"))
    {
        format!("dim{r}")
    } else if let Some(r) = suffix.strip_prefix(['ø', 'Ø']) {
        format!("m7b5{}", r.strip_prefix('7').unwrap_or(r))
    } else if let Some(r) = suffix
        .strip_prefix('+')
        .or_else(|| suffix.strip_prefix("aug"))
    {
        format!("aug{r}")
    } else if lower {
        match suffix.strip_prefix("maj") {
            Some(r) => format!("mMaj{r}"),
            None => format!("m{suffix}"),
        }
    } else {
        suffix.to_string()
    };
    Chord::parse(&format!("{}{quality}", root.name()))
}

/// Split a progression string into chord symbols (whitespace, `|` and `,` separate).
pub fn split_progression(s: &str) -> Vec<String> {
    s.split(|c: char| c.is_whitespace() || matches!(c, '|' | ','))
        .map(str::trim)
        .filter(|t| !t.is_empty() && t.chars().any(|c| c.is_alphanumeric()))
        .map(String::from)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chords(s: &str) -> Vec<Chord> {
        split_progression(s)
            .iter()
            .map(|c| Chord::parse(c).unwrap())
            .collect()
    }

    fn numerals(key: &str, s: &str) -> Vec<String> {
        let key = Key::parse(key).unwrap();
        chords(s).iter().map(|c| analyze(&key, c).numeral).collect()
    }

    #[test]
    fn parse_keys() {
        assert_eq!(Key::parse("Am").unwrap().name(), "A minor");
        assert_eq!(Key::parse("F# minor").unwrap().name(), "F# minor");
        assert_eq!(Key::parse("Bb").unwrap().name(), "Bb major");
        assert_eq!(Key::parse("c major").unwrap().name(), "C major");
        assert_eq!(Key::parse("Ebmin").unwrap().name(), "Eb minor");
        assert!(Key::parse("H").is_err());
        assert!(Key::parse("C lydian").is_err());
    }

    #[test]
    fn roman_numerals_in_major() {
        assert_eq!(numerals("C", "Dm7 G7 Cmaj7"), ["ii7", "V7", "Imaj7"]);
        assert_eq!(numerals("C", "C Am F G7"), ["I", "vi", "IV", "V7"]);
        assert_eq!(
            numerals("C", "Bdim Bm7b5 Bdim7"),
            ["vii°", "viiø7", "vii°7"]
        );
        assert_eq!(numerals("C", "Bb Ab Fm"), ["bVII", "bVI", "iv"]);
    }

    #[test]
    fn roman_numerals_in_minor() {
        assert_eq!(numerals("Am", "Am Dm E7 Am"), ["i", "iv", "V7", "i"]);
        assert_eq!(numerals("Am", "Am G F E"), ["i", "VII", "VI", "V"]);
        assert_eq!(numerals("Am", "G#dim7"), ["vii°7"]);
    }

    #[test]
    fn secondary_dominants_and_borrowed() {
        let key = Key::parse("C").unwrap();
        let a = analyze(&key, &Chord::parse("A7").unwrap());
        assert_eq!(a.kind, Kind::SecondaryDominant);
        assert!(a.role.contains("V7/ii"), "{}", a.role);
        let a = analyze(&key, &Chord::parse("D7").unwrap());
        assert!(a.role.contains("V7/V"), "{}", a.role);
        let a = analyze(&key, &Chord::parse("F#dim7").unwrap());
        assert_eq!(a.kind, Kind::SecondaryLeadingTone);
        assert!(a.role.contains("vii°7/V"), "{}", a.role);
        let a = analyze(&key, &Chord::parse("Fm").unwrap());
        assert_eq!(a.kind, Kind::Borrowed);
        assert!(a.role.contains("parallel minor"));
        assert_eq!(
            analyze(&key, &Chord::parse("Db7").unwrap()).kind,
            Kind::TritoneSub
        );
        assert_eq!(
            analyze(&key, &Chord::parse("Db").unwrap()).kind,
            Kind::Neapolitan
        );
        assert_eq!(
            analyze(&key, &Chord::parse("F#maj7").unwrap()).kind,
            Kind::Chromatic
        );
    }

    #[test]
    fn detects_keys() {
        let top = |s: &str| detect(&chords(s))[0].key.name();
        assert_eq!(top("Dm7 G7 Cmaj7"), "C major");
        assert_eq!(top("C F G7 C"), "C major");
        assert_eq!(top("Am Dm E7 Am"), "A minor");
        assert_eq!(top("F#m7 B7 Emaj7"), "E major");
        assert_eq!(top("Bbmaj7 Gm7 Cm7 F7"), "Bb major");
        let guesses = detect(&chords("Dm7 G7 Cmaj7"));
        assert!(guesses[0].confidence > 0.8);
        assert!(guesses[0].confidence > guesses[1].confidence);
        assert!(detect(&[]).is_empty());
    }

    #[test]
    fn realizes_ii_v_i_in_all_keys() {
        let expected = [
            ("C", "Dm7 G7 Cmaj7"),
            ("G", "Am7 D7 Gmaj7"),
            ("D", "Em7 A7 Dmaj7"),
            ("A", "Bm7 E7 Amaj7"),
            ("E", "F#m7 B7 Emaj7"),
            ("B", "C#m7 F#7 Bmaj7"),
            ("F#", "G#m7 C#7 F#maj7"),
            ("C#", "D#m7 G#7 C#maj7"),
            ("F", "Gm7 C7 Fmaj7"),
            ("Bb", "Cm7 F7 Bbmaj7"),
            ("Eb", "Fm7 Bb7 Ebmaj7"),
            ("Ab", "Bbm7 Eb7 Abmaj7"),
            ("Db", "Ebm7 Ab7 Dbmaj7"),
            ("Gb", "Abm7 Db7 Gbmaj7"),
            ("Cb", "Dbm7 Gb7 Cbmaj7"),
        ];
        for (key, want) in expected {
            let k = Key::parse(key).unwrap();
            let got: Vec<String> = ["ii7", "V7", "Imaj7"]
                .iter()
                .map(|n| realize(&k, n).unwrap().name())
                .collect();
            assert_eq!(got.join(" "), want, "key {key}");
        }
    }

    #[test]
    fn realizes_borrowed_secondary_and_minor() {
        let c = Key::parse("C").unwrap();
        let name = |k: &Key, n: &str| realize(k, n).unwrap().name();
        assert_eq!(name(&c, "bVII"), "Bb");
        assert_eq!(name(&c, "V7/V"), "D7");
        assert_eq!(name(&c, "vii°7/ii"), "C#dim7");
        assert_eq!(name(&c, "viiø7"), "Bm7b5");
        assert_eq!(name(&c, "IVmaj7"), "Fmaj7");
        let am = Key::parse("Am").unwrap();
        assert_eq!(name(&am, "i7"), "Am7");
        assert_eq!(name(&am, "iv"), "Dm");
        assert_eq!(name(&am, "VI"), "F");
        assert_eq!(name(&am, "V7"), "E7");
        assert!(realize(&c, "IX").is_err());
    }

    #[test]
    fn splits_progressions() {
        assert_eq!(
            split_progression("| Dm7 | G7, Cmaj7 - |"),
            ["Dm7", "G7", "Cmaj7"]
        );
    }
}

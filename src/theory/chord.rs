//! Chord symbols: parsing (`F#m7b5`, `Bb13#11/D`, `C6/9`), spelling, and identification
//! from a set of notes.

use anyhow::{Result, bail};

use super::interval::Interval;
use super::pitch::Note;

#[derive(Debug, Clone, PartialEq)]
pub struct Chord {
    pub root: Note,
    pub bass: Option<Note>,
    /// Chord tones above the root, sorted by degree, root (`1`) included.
    pub intervals: Vec<Interval>,
    pub symbol: String,
}

#[derive(Default)]
struct Spec {
    major: bool,
    minor: bool,
    dim: bool,
    half_dim: bool,
    aug: bool,
    sus: Option<u8>,
    power: bool,
    sixth: bool,
    seventh: bool,
    nine: bool,
    eleven: bool,
    thirteen: bool,
    adds: Vec<Interval>,
    no3: bool,
    no5: bool,
    alt: bool,
    /// (accidental, degree number) from tokens such as `b5`, `#9`, `#11`, `b13`.
    alterations: Vec<(i32, u32)>,
    seen_number: bool,
}

fn take<'a>(r: &'a str, options: &[&str]) -> Option<&'a str> {
    options.iter().find_map(|o| r.strip_prefix(o))
}

impl Chord {
    pub fn parse(symbol: &str) -> Result<Chord> {
        let original = symbol.trim();
        let s = original
            .replace("6/9", "69")
            .replace(['(', ')', ',', ' '], "");
        let mut chars = s.chars();
        let letter = chars.next().filter(|c| matches!(c, 'A'..='G'));
        let Some(letter) = letter else {
            bail!(
                "not a chord symbol: `{original}` (must start with a note A-G, e.g. Cmaj7, F#m7b5)"
            );
        };
        let mut rest = &s[1..];
        let mut acc = 0i8;
        if let Some(c) = rest.chars().next() {
            match c {
                '#' | '♯' => acc = 1,
                'b' | '♭' => acc = -1,
                _ => {}
            }
            if acc != 0 {
                rest = &rest[c.len_utf8()..];
            }
        }
        let root = Note::parse(&format!(
            "{letter}{}",
            match acc {
                1 => "#",
                -1 => "b",
                _ => "",
            }
        ))?;

        let mut bass = None;
        if let Some((head, tail)) = rest.rsplit_once('/') {
            match Note::parse(tail) {
                Ok(n) => bass = Some(n),
                Err(_) => bail!("bad bass note `{tail}` in `{original}`"),
            }
            rest = head;
        }

        let spec = parse_suffix(rest, original)?;
        Ok(Chord {
            root,
            bass,
            intervals: build_intervals(&spec),
            symbol: original.to_string(),
        })
    }

    pub fn notes(&self) -> Vec<Note> {
        self.intervals
            .iter()
            .map(|i| self.root.up(i.steps, i.semis))
            .collect()
    }

    pub fn formula(&self) -> String {
        self.intervals
            .iter()
            .map(|i| i.degree_token())
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Pitch classes of the chord tones, sorted and de-duplicated.
    pub fn pcs(&self) -> Vec<i32> {
        let mut v: Vec<i32> = self.notes().iter().map(|n| n.pc()).collect();
        v.sort_unstable();
        v.dedup();
        v
    }

    pub fn has(&self, steps: usize, semis: i32) -> bool {
        self.intervals
            .iter()
            .any(|i| i.steps == steps && i.semis == semis)
    }

    pub fn third(&self) -> Option<Interval> {
        self.intervals.iter().copied().find(|i| i.steps == 2)
    }

    pub fn fifth(&self) -> Option<Interval> {
        self.intervals.iter().copied().find(|i| i.steps == 4)
    }

    pub fn seventh(&self) -> Option<Interval> {
        self.intervals.iter().copied().find(|i| i.steps == 6)
    }

    pub fn is_minor_third(&self) -> bool {
        self.has(2, 3)
    }

    /// A major third plus a minor seventh: functions as a dominant.
    pub fn is_dominant(&self) -> bool {
        self.has(2, 4) && self.has(6, 10)
    }

    /// Re-spell the chord on a different root (used by transposition).
    pub fn with_root(&self, root: Note, bass: Option<Note>) -> Chord {
        Chord {
            root,
            bass,
            intervals: self.intervals.clone(),
            symbol: String::new(),
        }
    }

    /// Canonical symbol, e.g. `Bb13#11/D`.
    pub fn name(&self) -> String {
        let mut s = format!("{}{}", self.root.name(), self.suffix());
        if let Some(b) = self.bass {
            s.push('/');
            s.push_str(&b.name());
        }
        s
    }

    /// The quality suffix in a compact canonical form (`m7b5`, `maj9`, `7#9`, `sus4`, ...).
    pub fn suffix(&self) -> String {
        let third = self.third().map(|i| i.semis);
        let fifth = self.fifth().map(|i| i.semis);
        let seventh = self.seventh().map(|i| i.semis);
        let nat = |steps: usize, semis: i32| self.has(steps, semis);
        let sus2 = self.intervals.iter().any(|i| i.steps == 1 && i.semis == 2) && third.is_none();
        let sus4 = nat(3, 5) && third.is_none();
        let mut out = String::new();
        let mut alts: Vec<String> = Vec::new();

        // Highest stacked natural extension replaces the plain 7.
        let ext = if nat(12, 21) {
            Some(13)
        } else if nat(10, 17) && nat(8, 14) {
            Some(11)
        } else if nat(8, 14) {
            Some(9)
        } else {
            None
        };
        let dim_triad = third == Some(3) && fifth == Some(6);
        match (third, fifth, seventh) {
            (Some(3), Some(6), Some(10)) => out.push_str("m7b5"),
            (Some(3), Some(6), Some(9)) => out.push_str("dim7"),
            (Some(3), Some(6), None) => out.push_str("dim"),
            (Some(4), Some(8), None) => out.push_str("aug"),
            (Some(4), _, Some(11)) => {
                out.push_str("maj");
                out.push_str(&ext.map_or("7".into(), |e| e.to_string()));
            }
            (Some(3), _, Some(11)) => {
                out.push_str("mMaj");
                out.push_str(&ext.map_or("7".into(), |e| e.to_string()));
            }
            (Some(3), _, Some(10)) => {
                out.push('m');
                out.push_str(&ext.map_or("7".into(), |e| e.to_string()));
            }
            (Some(3), _, Some(9)) => out.push_str("dim7"),
            (Some(3), _, None) => out.push('m'),
            (Some(4), _, Some(10)) => out.push_str(&ext.map_or("7".into(), |e| e.to_string())),
            (None, _, Some(10)) if sus4 || sus2 => {
                out.push_str(&ext.map_or("7".into(), |e| e.to_string()));
            }
            (Some(4), _, Some(9)) => out.push('7'),
            _ => {}
        }
        if third.is_none() {
            if sus4 {
                out.push_str("sus4");
            } else if sus2 {
                out.push_str("sus2");
            } else if fifth.is_some() && self.intervals.len() == 2 {
                out.push('5');
            }
        }
        if matches!(third, Some(3 | 4)) && seventh.is_none() && nat(5, 9) {
            out.push_str(if nat(8, 14) { "69" } else { "6" });
        }
        if seventh.is_none() && !nat(5, 9) {
            for (steps, semis, n) in [(1, 2, 2), (8, 14, 9), (3, 5, 4), (10, 17, 11), (12, 21, 13)]
            {
                if nat(steps, semis) && !(n == 4 && sus4) && !(n == 2 && sus2) {
                    alts.push(format!("add{n}"));
                }
            }
        }
        // Altered tones.
        match fifth {
            Some(6) if !dim_triad => alts.push("b5".into()),
            Some(8) if !(third == Some(4) && seventh.is_none()) => alts.push("#5".into()),
            _ => {}
        }
        for i in &self.intervals {
            if i.steps == 8 && i.semis == 13 {
                alts.push("b9".into());
            }
            if i.steps == 8 && i.semis == 15 {
                alts.push("#9".into());
            }
            if i.steps == 10 && i.semis == 18 {
                alts.push("#11".into());
            }
            if i.steps == 12 && i.semis == 20 {
                alts.push("b13".into());
            }
        }
        if third == Some(4)
            && seventh == Some(10)
            && fifth.is_none()
            && ["b9", "#9", "#11", "b13"]
                .iter()
                .all(|a| alts.iter().any(|x| x == a))
        {
            return "7alt".into();
        }
        for a in alts {
            out.push_str(&a);
        }
        out
    }

    /// Plain-language description of the chord's quality.
    pub fn quality(&self) -> String {
        let third = self.third().map(|i| i.semis);
        let fifth = self.fifth().map(|i| i.semis);
        let seventh = self.seventh().map(|i| i.semis);
        let ext = if self.has(12, 21) {
            "13th"
        } else if self.has(10, 17) && self.has(8, 14) {
            "11th"
        } else if self.has(8, 14) {
            "9th"
        } else {
            "7th"
        };
        let sus = if self.has(3, 5) && third.is_none() {
            Some("sus4")
        } else if self.has(1, 2) && third.is_none() {
            Some("sus2")
        } else {
            None
        };
        let base = match (third, fifth, seventh, sus) {
            (_, _, Some(10), Some(s)) => format!("dominant {ext} ({s})"),
            (_, _, None, Some(s)) => s.to_string(),
            (None, Some(_), None, None) => "power chord (no third)".into(),
            (Some(3), Some(6), Some(10), _) => "half-diminished (minor 7 flat 5)".into(),
            (Some(3), Some(6), Some(9), _) => "diminished 7th".into(),
            (Some(3), Some(6), None, _) => "diminished triad".into(),
            (Some(4), Some(8), None, _) => "augmented triad".into(),
            (Some(4), Some(8), Some(10), _) => "augmented 7th (dominant 7 sharp 5)".into(),
            (Some(4), _, Some(11), _) => format!("major {ext}"),
            (Some(3), _, Some(11), _) => format!("minor-major {ext}"),
            (Some(3), _, Some(10), _) => format!("minor {ext}"),
            (Some(4), _, Some(10), _) => format!("dominant {ext}"),
            (Some(3), _, None, _) => "minor triad".into(),
            (Some(4), _, None, _) => "major triad".into(),
            _ => "non-tertian or incomplete chord".into(),
        };
        let mut extras: Vec<&str> = Vec::new();
        if seventh.is_none() && self.has(5, 9) {
            extras.push("added 6th");
        }
        let alts: Vec<String> = self
            .intervals
            .iter()
            .filter(|i| matches!((i.steps, i.semis), (8, 13) | (8, 15) | (10, 18) | (12, 20)))
            .map(|i| i.degree_token())
            .collect();
        let mut text = base;
        for e in extras {
            text.push_str(&format!(", {e}"));
        }
        if !alts.is_empty() {
            text.push_str(&format!(", altered tones: {}", alts.join(" ")));
        }
        text
    }

    /// Inversion / slash-bass description, if the chord has a bass note.
    pub fn bass_description(&self) -> Option<String> {
        let bass = self.bass?;
        if bass == self.root {
            return Some("root position".into());
        }
        let pos = self.notes().iter().position(|n| n.pc() == bass.pc());
        Some(match pos.map(|p| self.intervals[p]) {
            Some(i) if i.steps == 2 => "first inversion (third in the bass)".into(),
            Some(i) if i.steps == 4 => "second inversion (fifth in the bass)".into(),
            Some(i) if i.steps == 6 => "third inversion (seventh in the bass)".into(),
            Some(i) => format!("{} in the bass", i.name()),
            None => "slash chord (bass note is not a chord tone)".into(),
        })
    }
}

fn parse_suffix(rest: &str, original: &str) -> Result<Spec> {
    let mut st = Spec::default();
    let mut r = rest;
    while !r.is_empty() {
        // Alterations like b5, #9, #11, b13 (and +5 / -9 once a number has been seen).
        let alt_marks: &[char] = if st.seen_number {
            &['#', 'b', '+', '-', '♯', '♭']
        } else {
            &['#', 'b', '♯', '♭']
        };
        if r.starts_with(alt_marks) {
            let body = r.trim_start_matches(alt_marks);
            let marks = &r[..r.len() - body.len()];
            let digits: String = body.chars().take_while(char::is_ascii_digit).collect();
            if !digits.is_empty() {
                let acc: i32 = marks
                    .chars()
                    .map(|c| {
                        if matches!(c, '#' | '+' | '♯') {
                            1
                        } else {
                            -1
                        }
                    })
                    .sum();
                let n: u32 = digits.parse()?;
                if !matches!(n, 5 | 9 | 11 | 13) {
                    bail!("unsupported alteration `{marks}{digits}` in `{original}`");
                }
                st.alterations.push((acc, n));
                r = &body[digits.len()..];
                continue;
            }
        }
        if let Some(t) = take(r, &["maj", "Maj", "MAJ", "M", "Δ", "△"]) {
            st.major = true;
            r = t;
        } else if let Some(t) = take(r, &["min", "mi", "m", "-", "−"]) {
            st.minor = true;
            r = t;
        } else if let Some(t) = take(r, &["half-dim", "halfdim", "ø", "Ø"]) {
            st.half_dim = true;
            st.seventh = true;
            r = t;
        } else if let Some(t) = take(r, &["dim", "°", "º", "o"]) {
            st.dim = true;
            r = t;
        } else if let Some(t) = take(r, &["aug", "+"]) {
            st.aug = true;
            r = t;
        } else if let Some(t) = take(r, &["sus2"]) {
            st.sus = Some(2);
            r = t;
        } else if let Some(t) = take(r, &["sus4", "sus"]) {
            st.sus = Some(4);
            r = t;
        } else if let Some(t) = take(r, &["add", "Add"]) {
            let digits: String = t.chars().take_while(char::is_ascii_digit).collect();
            let interval = match digits.as_str() {
                "2" => Interval::new(1, 2),
                "4" => Interval::new(3, 5),
                "6" => Interval::new(5, 9),
                "9" => Interval::new(8, 14),
                "11" => Interval::new(10, 17),
                "13" => Interval::new(12, 21),
                _ => bail!("unsupported `add{digits}` in `{original}`"),
            };
            st.adds.push(interval);
            r = &t[digits.len()..];
        } else if let Some(t) = take(r, &["no3", "omit3"]) {
            st.no3 = true;
            r = t;
        } else if let Some(t) = take(r, &["no5", "omit5"]) {
            st.no5 = true;
            r = t;
        } else if let Some(t) = take(r, &["alt"]) {
            st.alt = true;
            st.seventh = true;
            r = t;
        } else if let Some(t) = take(r, &["69"]) {
            st.sixth = true;
            st.adds.push(Interval::new(8, 14));
            st.seen_number = true;
            r = t;
        } else if let Some(t) = take(r, &["13"]) {
            st.seventh = true;
            st.nine = true;
            st.thirteen = true;
            st.seen_number = true;
            r = t;
        } else if let Some(t) = take(r, &["11"]) {
            st.seventh = true;
            st.nine = true;
            st.eleven = true;
            st.seen_number = true;
            r = t;
        } else if let Some(t) = take(r, &["9"]) {
            st.seventh = true;
            st.nine = true;
            st.seen_number = true;
            r = t;
        } else if let Some(t) = take(r, &["7"]) {
            st.seventh = true;
            st.seen_number = true;
            r = t;
        } else if let Some(t) = take(r, &["6"]) {
            st.sixth = true;
            st.seen_number = true;
            r = t;
        } else if let Some(t) = take(r, &["5"]) {
            st.power = true;
            st.seen_number = true;
            r = t;
        } else if let Some(t) = take(r, &["4"]) {
            st.adds.push(Interval::new(3, 5));
            st.seen_number = true;
            r = t;
        } else if let Some(t) = take(r, &["2"]) {
            st.adds.push(Interval::new(1, 2));
            st.seen_number = true;
            r = t;
        } else {
            bail!("unrecognized chord suffix `{r}` in `{original}`");
        }
    }
    if st.minor && st.major && !st.seventh {
        // "mM" with no number: treat as minor-major 7th.
        st.seventh = true;
    }
    Ok(st)
}

fn build_intervals(st: &Spec) -> Vec<Interval> {
    let mut v = vec![Interval::new(0, 0)];
    let minor_third = st.minor || st.dim || st.half_dim;
    let half_dim_or_dim = st.dim || st.half_dim;
    if st.power && !st.seventh && !st.sixth && !st.major && !st.minor && !st.dim {
        v.push(Interval::new(4, 7));
    } else {
        if !st.no3 {
            match st.sus {
                Some(2) => v.push(Interval::new(1, 2)),
                Some(_) => v.push(Interval::new(3, 5)),
                None => v.push(Interval::new(2, if minor_third { 3 } else { 4 })),
            }
        }
        let mut fifth = if half_dim_or_dim {
            6
        } else if st.aug {
            8
        } else {
            7
        };
        for (acc, n) in &st.alterations {
            if *n == 5 {
                fifth = 7 + acc;
            }
        }
        if !st.no5 && !st.alt {
            v.push(Interval::new(4, fifth));
        }
    }
    if st.sixth {
        v.push(Interval::new(5, 9));
    }
    if st.seventh {
        let semis = if st.dim && !st.half_dim {
            9
        } else if st.major && !st.half_dim {
            11
        } else {
            10
        };
        v.push(Interval::new(6, semis));
    }
    let mut nine = st.nine;
    let mut eleven = st.eleven;
    let mut thirteen = st.thirteen;
    if st.thirteen && minor_third && !st.no3 {
        eleven = true;
    }
    for a in &st.adds {
        match (a.steps, a.semis) {
            (8, 14) => nine = true,
            (10, 17) => eleven = true,
            (12, 21) => thirteen = true,
            _ => v.push(*a),
        }
    }
    let mut altered = |steps: usize, base: i32, flag: &mut bool, n: u32| {
        for (acc, num) in &st.alterations {
            if *num == n {
                v.push(Interval::new(steps, base + acc));
                *flag = false;
            }
        }
    };
    altered(8, 14, &mut nine, 9);
    altered(10, 17, &mut eleven, 11);
    altered(12, 21, &mut thirteen, 13);
    if st.alt {
        v.push(Interval::new(8, 13));
        v.push(Interval::new(8, 15));
        v.push(Interval::new(10, 18));
        v.push(Interval::new(12, 20));
    }
    if nine {
        v.push(Interval::new(8, 14));
    }
    if eleven {
        v.push(Interval::new(10, 17));
    }
    if thirteen {
        v.push(Interval::new(12, 21));
    }
    v.sort_by_key(|i| (i.steps, i.semis));
    v.dedup();
    v
}

/// Suffixes tried when identifying a chord from notes, most common first.
const IDENTIFY_SUFFIXES: &[&str] = &[
    "", "m", "7", "maj7", "m7", "dim", "aug", "sus4", "sus2", "5", "6", "m6", "dim7", "m7b5",
    "mMaj7", "add9", "madd9", "9", "maj9", "m9", "7sus4", "69", "7b9", "7#9", "7#5", "7b5", "11",
    "m11", "13", "maj7#11", "maj13", "m13", "7#11", "7b13", "7alt", "maj7#5", "maj7b5",
];

#[derive(Debug, Clone)]
pub struct Candidate {
    pub chord: Chord,
    /// 0 is best: root in the bass first, then simpler chord types.
    pub rank: usize,
}

/// Identify chords from note names. The first note is taken as the bass.
pub fn identify(notes: &[Note]) -> Vec<Candidate> {
    let Some(&bass) = notes.first() else {
        return Vec::new();
    };
    let mut pcs: Vec<i32> = notes.iter().map(|n| n.pc()).collect();
    pcs.sort_unstable();
    pcs.dedup();
    let mut out: Vec<Candidate> = Vec::new();
    let mut seen_roots: Vec<Note> = Vec::new();
    for &root in notes {
        if seen_roots.iter().any(|r| r.pc() == root.pc()) {
            continue;
        }
        seen_roots.push(root);
        for (idx, suffix) in IDENTIFY_SUFFIXES.iter().enumerate() {
            let Ok(mut chord) = Chord::parse(&format!("{}{suffix}", root.name())) else {
                continue;
            };
            let relative: Vec<i32> = {
                let mut v: Vec<i32> = chord.notes().iter().map(|n| n.pc()).collect();
                v.sort_unstable();
                v.dedup();
                v
            };
            if relative != pcs {
                continue;
            }
            if bass.pc() != root.pc() {
                chord.bass = Some(bass);
            }
            let rank = if bass.pc() == root.pc() { 0 } else { 100 } + idx;
            out.push(Candidate { chord, rank });
        }
    }
    out.sort_by_key(|c| c.rank);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(symbol: &str) -> String {
        Chord::parse(symbol)
            .unwrap()
            .notes()
            .iter()
            .map(|n| n.name())
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[test]
    fn triads_and_sevenths() {
        assert_eq!(names("C"), "C E G");
        assert_eq!(names("Cm"), "C Eb G");
        assert_eq!(names("Cdim"), "C Eb Gb");
        assert_eq!(names("Caug"), "C E G#");
        assert_eq!(names("C7"), "C E G Bb");
        assert_eq!(names("Cmaj7"), "C E G B");
        assert_eq!(names("Dm7"), "D F A C");
        assert_eq!(names("Cdim7"), "C Eb Gb Bbb");
        assert_eq!(names("CmMaj7"), "C Eb G B");
        assert_eq!(names("C5"), "C G");
    }

    #[test]
    fn half_diminished_spelling() {
        assert_eq!(names("F#m7b5"), "F# A C E");
        assert_eq!(names("F#ø7"), "F# A C E");
        assert_eq!(Chord::parse("F#m7b5").unwrap().name(), "F#m7b5");
    }

    #[test]
    fn extended_and_altered() {
        assert_eq!(names("Bb13#11/D"), "Bb D F Ab C E G");
        assert_eq!(names("C9"), "C E G Bb D");
        assert_eq!(names("C13"), "C E G Bb D A");
        assert_eq!(names("Cm13"), "C Eb G Bb D F A");
        assert_eq!(names("G7b9"), "G B D F Ab");
        assert_eq!(names("G7#9"), "G B D F A#");
        assert_eq!(names("C7(b5)"), "C E Gb Bb");
        assert_eq!(names("C7alt"), "C E Bb Db D# F# Ab");
        assert_eq!(names("Cmaj9"), "C E G B D");
    }

    #[test]
    fn sus_add_and_sixth() {
        assert_eq!(names("Csus4"), "C F G");
        assert_eq!(names("C7sus4"), "C F G Bb");
        assert_eq!(names("Cadd9"), "C E G D");
        assert_eq!(names("C6/9"), "C E G A D");
        assert_eq!(names("Am6"), "A C E F#");
    }

    #[test]
    fn slash_bass_and_inversions() {
        let c = Chord::parse("Bb13#11/D").unwrap();
        assert_eq!(c.bass.unwrap().name(), "D");
        assert_eq!(
            c.bass_description().unwrap(),
            "first inversion (third in the bass)"
        );
        assert_eq!(c.name(), "Bb13#11/D");
        let s = Chord::parse("C/F").unwrap();
        assert!(s.bass_description().unwrap().starts_with("slash chord"));
    }

    #[test]
    fn canonical_names() {
        for sym in [
            "Cmaj7", "Dm7", "G7", "Bdim7", "Ebm9", "A7#9", "C6", "Csus4", "F#7b9",
        ] {
            assert_eq!(Chord::parse(sym).unwrap().name(), sym, "{sym}");
        }
        assert_eq!(Chord::parse("C-7").unwrap().name(), "Cm7");
        assert_eq!(Chord::parse("CΔ7").unwrap().name(), "Cmaj7");
        assert_eq!(Chord::parse("C6/9").unwrap().name(), "C69");
    }

    #[test]
    fn rejects_garbage() {
        assert!(Chord::parse("H7").is_err());
        assert!(Chord::parse("Cxyz").is_err());
        assert!(Chord::parse("C/Q").is_err());
        assert!(Chord::parse("").is_err());
    }

    #[test]
    fn identifies_from_notes() {
        let n = |s: &str| {
            s.split_whitespace()
                .map(|x| Note::parse(x).unwrap())
                .collect::<Vec<_>>()
        };
        let c = identify(&n("C E G Bb"));
        assert_eq!(c[0].chord.name(), "C7");
        let c = identify(&n("E C G"));
        assert_eq!(c[0].chord.name(), "C/E");
        // C6 and Am7/C are the same notes; both are reported.
        let all: Vec<String> = identify(&n("C E G A"))
            .iter()
            .map(|c| c.chord.name())
            .collect();
        assert!(
            all.contains(&"C6".to_string()) && all.contains(&"Am7/C".to_string()),
            "{all:?}"
        );
        assert!(identify(&n("C Db D")).is_empty());
    }
}

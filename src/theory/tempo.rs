//! Tempo maths: note-value durations, delay times, bar length, sample counts.

use anyhow::{Result, bail};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Feel {
    Straight,
    Dotted,
    Triplet,
}

impl Feel {
    fn factor(self) -> f64 {
        match self {
            Feel::Straight => 1.0,
            Feel::Dotted => 1.5,
            Feel::Triplet => 2.0 / 3.0,
        }
    }
}

/// (name, denominator): 1/1 ... 1/64.
pub const NOTE_VALUES: [(&str, u32); 7] = [
    ("1/1", 1),
    ("1/2", 2),
    ("1/4", 4),
    ("1/8", 8),
    ("1/16", 16),
    ("1/32", 32),
    ("1/64", 64),
];

pub fn quarter_ms(bpm: f64) -> f64 {
    60_000.0 / bpm
}

/// Duration in milliseconds of `1/denominator` of a whole note at `bpm` (quarter-note beats).
pub fn note_ms(bpm: f64, denominator: u32, feel: Feel) -> f64 {
    quarter_ms(bpm) * 4.0 / denominator as f64 * feel.factor()
}

pub fn hz_from_ms(ms: f64) -> f64 {
    1000.0 / ms
}

pub fn samples(ms: f64, sample_rate: f64) -> f64 {
    ms * sample_rate / 1000.0
}

/// A bar of `numerator/denominator`, in milliseconds. Compound meters (6/8, 12/8) are
/// measured in notated note values, so 6/8 is six eighth notes.
pub fn bar_ms(bpm: f64, numerator: u32, denominator: u32) -> f64 {
    numerator as f64 * note_ms(bpm, denominator, Feel::Straight)
}

pub fn parse_time_signature(s: &str) -> Result<(u32, u32)> {
    let parsed = s.split_once('/').and_then(|(n, d)| {
        let n: u32 = n.trim().parse().ok()?;
        let d: u32 = d.trim().parse().ok()?;
        (n > 0 && d.is_power_of_two() && d <= 64).then_some((n, d))
    });
    match parsed {
        Some(ts) => Ok(ts),
        None => bail!("bad time signature `{s}` (use e.g. 4/4, 6/8, 7/8)"),
    }
}

pub fn parse_note_value(s: &str) -> Result<(u32, Feel)> {
    let t = s.trim().to_lowercase();
    let (body, feel) = if let Some(b) = t.strip_suffix('.').or_else(|| t.strip_suffix("d")) {
        (b.to_string(), Feel::Dotted)
    } else if let Some(b) = t.strip_suffix('t') {
        (b.to_string(), Feel::Triplet)
    } else {
        (t, Feel::Straight)
    };
    let denominator = body
        .strip_prefix("1/")
        .unwrap_or(&body)
        .parse::<u32>()
        .ok()
        .filter(|d| NOTE_VALUES.iter().any(|(_, n)| n == d));
    match denominator {
        Some(d) => Ok((d, feel)),
        None => bail!("bad note value `{s}` (use 1/4, 1/8., 1/8t, 1/16)"),
    }
}

/// Tempo from a loop or section of `bars` bars lasting `seconds` seconds.
pub fn bpm_from_duration(bars: f64, numerator: u32, denominator: u32, seconds: f64) -> f64 {
    let quarter_beats = bars * numerator as f64 * 4.0 / denominator as f64;
    quarter_beats * 60.0 / seconds
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn dotted_eighth_at_120_is_375_ms() {
        assert!(close(note_ms(120.0, 8, Feel::Dotted), 375.0));
        assert!(close(note_ms(120.0, 4, Feel::Straight), 500.0));
        assert!(close(note_ms(120.0, 4, Feel::Triplet), 1000.0 / 3.0));
        assert!(close(note_ms(120.0, 16, Feel::Straight), 125.0));
        assert!(close(note_ms(60.0, 1, Feel::Straight), 4000.0));
    }

    #[test]
    fn bars_and_samples() {
        assert!(close(bar_ms(120.0, 4, 4), 2000.0));
        assert!(close(bar_ms(120.0, 3, 4), 1500.0));
        assert!(close(bar_ms(120.0, 6, 8), 1500.0));
        assert!(close(bar_ms(140.0, 7, 8), 7.0 * 60_000.0 / 140.0 / 2.0));
        assert!(close(samples(375.0, 48_000.0), 18_000.0));
        assert!(close(hz_from_ms(500.0), 2.0));
    }

    #[test]
    fn parsing() {
        assert_eq!(parse_time_signature("7/8").unwrap(), (7, 8));
        assert!(parse_time_signature("4/3").is_err());
        assert!(parse_time_signature("x").is_err());
        assert_eq!(parse_note_value("1/8.").unwrap(), (8, Feel::Dotted));
        assert_eq!(parse_note_value("1/4t").unwrap(), (4, Feel::Triplet));
        assert_eq!(parse_note_value("16").unwrap(), (16, Feel::Straight));
        assert!(parse_note_value("1/3").is_err());
    }

    #[test]
    fn tempo_from_loop_length() {
        // 4 bars of 4/4 in 8 seconds is 120 bpm.
        assert!(close(bpm_from_duration(4.0, 4, 4, 8.0), 120.0));
        assert!(close(bpm_from_duration(2.0, 3, 4, 3.0), 120.0));
    }
}

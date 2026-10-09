//! Diatonic intervals: a letter distance plus a semitone count.

/// An interval measured as `steps` letters (0 = unison, 2 = third, 8 = ninth) and `semis`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Interval {
    pub steps: usize,
    pub semis: i32,
}

/// Semitones of the major-scale degree for a letter distance (compound steps fold down).
fn major_semis(steps: usize) -> i32 {
    const MAJOR: [i32; 7] = [0, 2, 4, 5, 7, 9, 11];
    MAJOR[steps % 7] + 12 * (steps / 7) as i32
}

impl Interval {
    pub fn new(steps: usize, semis: i32) -> Self {
        Self { steps, semis }
    }

    /// Parse a degree token relative to the major scale: `1`, `b3`, `#4`, `bb7`, `b9`, `13`.
    pub fn from_token(token: &str) -> Option<Self> {
        let number = token.trim_start_matches(['#', 'b']);
        let acc = token[..token.len() - number.len()]
            .chars()
            .map(|c| if c == '#' { 1 } else { -1 })
            .sum::<i32>();
        let n: usize = number.parse().ok().filter(|n| *n >= 1)?;
        Some(Self::new(n - 1, major_semis(n - 1) + acc))
    }

    /// Short name such as `P5`, `M3`, `m7`, `A4`, `d5`, `M9`, `b13` style qualities included.
    pub fn name(self) -> String {
        let number = self.steps + 1;
        let perfect_type = matches!(self.steps % 7, 0 | 3 | 4);
        let diff = self.semis - major_semis(self.steps);
        let quality = if perfect_type {
            match diff {
                0 => "P".to_string(),
                d if d > 0 => "A".repeat(d as usize),
                d => "d".repeat((-d) as usize),
            }
        } else {
            match diff {
                0 => "M".to_string(),
                -1 => "m".to_string(),
                d if d > 0 => "A".repeat(d as usize),
                d => "d".repeat((-d - 1) as usize),
            }
        };
        format!("{quality}{number}")
    }

    /// Scale-degree formula token relative to the major scale: `1`, `b3`, `#4`, `b9`.
    pub fn degree_token(self) -> String {
        let diff = self.semis - major_semis(self.steps);
        let acc = match diff {
            0 => String::new(),
            d if d > 0 => "#".repeat(d as usize),
            d => "b".repeat((-d) as usize),
        };
        format!("{acc}{}", self.steps + 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert_eq!(Interval::new(4, 7).name(), "P5");
        assert_eq!(Interval::new(2, 4).name(), "M3");
        assert_eq!(Interval::new(2, 3).name(), "m3");
        assert_eq!(Interval::new(6, 10).name(), "m7");
        assert_eq!(Interval::new(3, 6).name(), "A4");
        assert_eq!(Interval::new(4, 6).name(), "d5");
        assert_eq!(Interval::new(6, 9).name(), "d7");
        assert_eq!(Interval::new(8, 14).name(), "M9");
        assert_eq!(Interval::new(8, 13).name(), "m9");
        assert_eq!(Interval::new(0, 0).name(), "P1");
    }

    #[test]
    fn parse_tokens_round_trip() {
        for t in ["1", "b3", "#4", "b9", "b13", "13", "bb7", "#11"] {
            assert_eq!(Interval::from_token(t).unwrap().degree_token(), t);
        }
        assert_eq!(Interval::from_token("bb7"), Some(Interval::new(6, 9)));
        assert!(Interval::from_token("x").is_none());
        assert!(Interval::from_token("0").is_none());
    }

    #[test]
    fn tokens() {
        assert_eq!(Interval::new(2, 3).degree_token(), "b3");
        assert_eq!(Interval::new(3, 6).degree_token(), "#4");
        assert_eq!(Interval::new(8, 13).degree_token(), "b9");
        assert_eq!(Interval::new(12, 20).degree_token(), "b13");
    }
}

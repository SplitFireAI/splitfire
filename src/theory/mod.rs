//! Deterministic music theory: pure functions, no I/O. Spelling is diatonic (letter plus
//! accidental), never bare pitch classes, so C# major has E# and B#.

pub mod chord;
pub mod interval;
pub mod key;
pub mod pitch;
pub mod scale;
pub mod tempo;

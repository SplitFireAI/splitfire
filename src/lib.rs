//! SplitFire as a library: the music agent's [`Profile`](ed_acp::Profile), for hosts that
//! embed it instead of launching the `splitfire-agent` binary.
//!
//! A host serves [`SplitFire`] with `ed_acp::serve` over an in-memory channel and runs an ACP
//! client on the other end, which keeps the agent inside an app sandbox that can't spawn it as
//! a subprocess. Build with `default-features = false` to leave out the terminal UI.

mod audio;
pub mod profile;
mod prompt;
mod theory;
mod tools;

pub use ed_acp;
pub use profile::{DEMUCS, INFO, SplitFire};

//! Pure-Rust voice input core: `/ws/voice` protocol & stream state machine,
//! controller & transcript commit logic, dictation shortcut & submission gating,
//! consent & microphone priority preferences, and PCM resampling / capture / VAD.

pub mod contracts;
pub mod controller;
pub mod pcm;
pub mod preferences;
pub mod stream;

#[cfg(test)]
mod tests;

pub use contracts::*;
pub use controller::*;
pub use pcm::*;
pub use preferences::*;
pub use stream::*;

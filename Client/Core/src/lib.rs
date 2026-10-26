//! Helpers every in-game (x86) crate uses: GTAIV.exe memory access and the log file.

mod modules;
pub use modules::{hook, log, memory};

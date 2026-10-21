//! The IVMP server as the game's session host (docs/onesync-plan.md, "Server as the session
//! host"). Every game joins the server; none of them hosts, so the host never leaves.
//!
//! * [`bits`]: bit writer matching rage's bit buffers (most significant bit first).
//! * [`frame`]: packets, frames and chunks of the engine's connection layer.
//! * [`session`]: the host itself: per client connection, reliable channels, join replies.
//! * [`templates`]: the host's join messages, from a recording.

pub mod bits;
pub mod frame;
pub mod session;
pub mod templates;

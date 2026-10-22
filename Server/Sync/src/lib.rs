//! Server side of entity sync (docs/onesync-plan.md).
//!
//! The clients send the engine's packets through the server unencrypted (client
//! `Networking/src/crypt.rs`). [`packet`] splits a packet into frames and finds the game's
//! messages in them, [`registry`] keeps track of every networked entity, [`ids`] gives every client its own
//! object id block.

pub mod bits;
pub mod events;
pub mod ids;
pub mod nodes;
pub mod packet;
pub mod registry;

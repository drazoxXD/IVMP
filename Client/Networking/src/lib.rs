//! In-game networking (x86, used by the hooks DLL).
//!
//! * [`crypt`]: game packets unencrypted, so the server can read them.
//! * [`net`]: connection to the IVMP server and the tunnel for the engine's UDP traffic.
//! * [`peer`]: peer identity without Rockstar services (LibertyM port), find-game answer.
//! * [`relay`]: opening the engine's game socket and feeding it packets every frame.
//! * [`hostsys`]: hosting or joining the session the server picked.
//! * [`session`]: network session patches (LAN flag, rline join skip) and tracing.
//! * [`lobby`]: Free Mode host arguments.
//! * [`instance`]: second client on one PC (`-cl2`).
//! * [`netobjects`]: network object fixes (no duplicate player registration).
//! * [`spawner`]: creates the entities the server owns, when it asks us to.
//! * [`jointrace`]: diagnostics for the join path.
//! * [`controltrace`]: diagnostics for entity handovers (GIVE_CONTROL_EVENT).
//! * [`synctrace`]: diagnostics for received clone syncs (applied or why not).

use std::sync::OnceLock;

pub use ivmp_core::{log, memory};
pub use ivmp_natives::natives;

pub mod controltrace;
pub mod crypt;
pub mod hostsys;
pub mod instance;
pub mod jointrace;
pub mod lobby;
pub mod net;
pub mod netobjects;
pub mod peer;
pub mod relay;
pub mod session;
pub mod spawner;
pub mod synctrace;

static NICKNAME: OnceLock<String> = OnceLock::new();

/// The player's nickname, as passed to [`net::start`].
pub fn nickname() -> &'static str {
    NICKNAME.get().map(String::as_str).unwrap_or("Player")
}

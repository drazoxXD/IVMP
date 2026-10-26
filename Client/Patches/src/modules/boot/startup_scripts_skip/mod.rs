//! Selectively skips the single-player `startup` script.
//!
//! Direct boot first disables the normal startup-script launch in
//! `multiplayer::script_stop`. When the host wants the game's multiplayer
//! lobby, that patch is partially restored: `network_startup` is allowed to
//! run, while the story `startup` script remains skipped.

use ivmp_core::{log, memory};

/// Signature after `multiplayer::script_stop` has changed the first branch
/// from `jnz` to `jmp`.
pub const STARTUP_PATTERN: &str =
    "80 7D 08 00 C6 05 ?? ?? ?? ?? 00 EB 64 E8 ?? ?? ?? ?? 84 C0 74 07";

/// Changes `jz startup` to `jz exit`, leaving `network_startup` enabled.
const STORY_ONLY_BYTES: &[u8] = &[0x74, 0x5B];

/// Restores the normal branch and keeps only the story `startup` script
/// disabled.
pub fn restore_story_only(site: usize) -> bool {
    unsafe {
        if let Err(e) = memory::patch(site + 11, &[0x75]) {
            log!("boot: lobby branch restore failed: {e}");
            return false;
        }
        if let Err(e) = memory::patch(site + 20, STORY_ONLY_BYTES) {
            log!("boot: story startup skip failed: {e}");
            return false;
        }
        true
    }
}

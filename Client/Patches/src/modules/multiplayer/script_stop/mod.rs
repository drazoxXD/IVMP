//! Stops the game's normal startup script from launching.

use crate::modules::boot::support::{apply, Patch};

pub fn apply_patch() {
    apply(Patch {
        name: "no startup script",
        pattern: "80 7D 08 00 C6 05 ?? ?? ?? ?? 00 75 64 E8",
        offset: 11,
        original: &[0x75, 0x64],
        bytes: &[0xEB, 0x64],
    });
}

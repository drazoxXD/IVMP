use crate::modules::boot::support::{apply, Patch};

pub fn apply_patch() {
    apply(Patch {
        name: "world keeps running in the pause menu",
        pattern: "C6 05 ?? ?? ?? ?? 00 84 DB 0F 84 85 00 00 00 8B 35",
        offset: 6,
        original: &[0x00],
        bytes: &[0x01],
    });
}

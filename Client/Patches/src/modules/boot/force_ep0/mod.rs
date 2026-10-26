use crate::modules::boot::support::{apply, Patch};

pub fn apply_patch() {
    apply(Patch {
        name: "force episode 0",
        pattern: "80 3D ?? ?? ?? ?? 00 8B 0D ?? ?? ?? ?? 0F 45 0D ?? ?? ?? ?? 6A 00 6A 01 B2 01",
        offset: 0,
        original: &[0x80, 0x3D],
        bytes: &[
            0x33, 0xC9, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90,
            0x90, 0x90, 0x90, 0x90, 0x90,
        ],
    });
}

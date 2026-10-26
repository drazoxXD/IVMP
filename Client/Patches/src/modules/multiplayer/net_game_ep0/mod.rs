use crate::modules::boot::support::{apply, Patch};

pub fn apply_patch() {
    apply(Patch {
        name: "network game in episode 0",
        pattern: "E8 ?? ?? ?? ?? 8B 35 ?? ?? ?? ?? 8B F8 8B CE 89 3D",
        offset: 0,
        original: &[0xE8],
        bytes: &[0x31, 0xC0, 0x90, 0x90, 0x90],
    });
}

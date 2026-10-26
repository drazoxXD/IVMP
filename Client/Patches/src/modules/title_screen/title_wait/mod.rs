use crate::modules::boot::support::{apply, Patch};

pub fn apply_patch() {
    apply(Patch {
        name: "skip title screen wait",
        pattern: "83 FE 01 0F 85 64 FF FF FF 6A 00 B9",
        offset: 3,
        original: &[0x0F, 0x85, 0x64, 0xFF, 0xFF, 0xFF],
        bytes: &[0x90; 6],
    });
}

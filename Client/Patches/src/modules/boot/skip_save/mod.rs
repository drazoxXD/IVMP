use crate::modules::boot::support::{apply, Patch};

pub fn apply_patch() {
    apply(Patch {
        name: "skip boot profile/save states",
        pattern: "6A 01 FF D3 E8 ?? ?? ?? ?? 83 FE 06 0F 87",
        offset: 0,
        original: &[0x6A, 0x01, 0xFF, 0xD3, 0xE8],
        bytes: &[0xE9, 0xAE, 0x00, 0x00, 0x00],
    });
}

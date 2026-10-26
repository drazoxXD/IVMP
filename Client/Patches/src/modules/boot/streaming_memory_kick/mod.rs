use crate::modules::boot::support::{apply, Patch};

pub fn apply_patch() {
    apply(Patch {
        name: "no too-slow / streaming memory kicks",
        pattern: "51 A1 ?? ?? ?? ?? 53 55 8B EA 56 8A F1 57",
        offset: 0,
        original: &[0x51],
        bytes: &[0xC3],
    });
}

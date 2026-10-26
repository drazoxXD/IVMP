use crate::modules::boot::support::{apply, Patch};

pub fn apply_patch() {
    apply(Patch {
        name: "no title screen action",
        pattern: "33 F6 6A 0E 89 74 24 14 E8",
        offset: 2,
        original: &[0x6A, 0x0E],
        bytes: &[0x6A, 0x00],
    });
}

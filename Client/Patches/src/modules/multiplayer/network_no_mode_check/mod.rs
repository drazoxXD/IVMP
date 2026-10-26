use crate::modules::boot::support::{apply, Patch};

pub fn apply_patch() {
    apply(Patch {
        name: "network game start without the mode check",
        pattern: "83 3D ?? ?? ?? ?? 02 75 55 E8 ?? ?? ?? ?? E8 ?? ?? ?? ?? 8B 35",
        offset: 7,
        original: &[0x75, 0x55],
        bytes: &[0x90, 0x90],
    });
}

use ivmp_core::{log, memory};

pub struct Patch {
    pub name: &'static str,
    pub pattern: &'static str,
    pub offset: usize,
    pub original: &'static [u8],
    pub bytes: &'static [u8],
}

pub fn apply(patch: Patch) {
    let Some(site) = memory::find(patch.pattern).map(|a| a + patch.offset) else {
        log!("patch: {}: signature not found, skipped", patch.name);
        return;
    };
    let current = unsafe { std::slice::from_raw_parts(site as *const u8, patch.original.len()) };
    if current != patch.original {
        log!("patch: {} at {site:#x}: unexpected bytes {current:02X?}, skipped", patch.name);
        return;
    }
    match unsafe { memory::patch(site, patch.bytes) } {
        Ok(()) => log!("patch: {} at {site:#x}", patch.name),
        Err(e) => log!("patch: {} at {site:#x} failed: {e}", patch.name),
    }
}

//! Writes the running, decrypted GTAIV.exe image to `GTAIV_dump.exe` next to the DLL, so Ghidra
//! can read code that is encrypted on disk. The dump is un-relocated back to the Ghidra base
//! (0x400000) and its sections are laid out with file offset == RVA, so addresses match the
//! existing Ghidra project.

use std::path::Path;

use crate::{log, memory};

pub const DUMP_FILE: &str = "GTAIV_dump.exe";

fn u16_at(b: &[u8], at: usize) -> usize {
    u16::from_le_bytes([b[at], b[at + 1]]) as usize
}

fn u32_at(b: &[u8], at: usize) -> usize {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap()) as usize
}

fn put_u32(b: &mut [u8], at: usize, value: usize) {
    b[at..at + 4].copy_from_slice(&(value as u32).to_le_bytes());
}

/// Dumps once; does nothing if the file already exists.
pub fn dump_once(dir: &Path) {
    let path = dir.join(DUMP_FILE);
    if path.exists() {
        return;
    }
    let image = memory::game_image();
    let base = image.as_ptr() as usize;
    let mut out = vec![0u8; image.len()];
    for (start, run) in memory::snapshot(image) {
        let offset = start - base;
        out[offset..offset + run.len()].copy_from_slice(&run);
    }

    let pe = u32_at(&out, 0x3C);
    let optional = pe + 24;
    // Undo the loader's relocations so the dump sits at the Ghidra base.
    let delta = base.wrapping_sub(memory::GHIDRA_BASE);
    let (reloc_rva, reloc_size) = (u32_at(&out, optional + 96 + 5 * 8), u32_at(&out, optional + 96 + 5 * 8 + 4));
    let mut fixed = 0usize;
    let mut block = reloc_rva;
    while reloc_size != 0 && block + 8 <= reloc_rva + reloc_size && block + 8 <= out.len() {
        let (page, size) = (u32_at(&out, block), u32_at(&out, block + 4));
        if size < 8 {
            break;
        }
        for entry in (block + 8..block + size).step_by(2) {
            let value = u16_at(&out, entry);
            // IMAGE_REL_BASED_HIGHLOW
            if value >> 12 == 3 {
                let at = page + (value & 0xFFF);
                if at + 4 <= out.len() {
                    let v = u32_at(&out, at);
                    put_u32(&mut out, at, v.wrapping_sub(delta));
                    fixed += 1;
                }
            }
        }
        block += size;
    }

    // Headers: Ghidra base, and raw layout == virtual layout.
    put_u32(&mut out, optional + 28, memory::GHIDRA_BASE);
    let section_alignment = u32_at(&out, optional + 32);
    put_u32(&mut out, optional + 36, section_alignment);
    let sections = optional + u16_at(&out, pe + 20);
    for i in 0..u16_at(&out, pe + 6) {
        let header = sections + i * 40;
        let (virtual_size, rva) = (u32_at(&out, header + 8), u32_at(&out, header + 12));
        let size = virtual_size.min(out.len() - rva);
        put_u32(&mut out, header + 16, size);
        put_u32(&mut out, header + 20, rva);
    }

    match std::fs::write(&path, &out) {
        Ok(()) => log!("dump: wrote {} ({} MB, {fixed} relocations undone)", path.display(), out.len() / (1024 * 1024)),
        Err(e) => log!("dump: writing {} failed: {e}", path.display()),
    }
}

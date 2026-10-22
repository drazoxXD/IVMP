//! Episode content in the base game's world (`dlc` 2 and 3, docs/dlc.md).
//!
//! The game describes an episode's content in a `content.dat` (files by kind: IDE, HANDLING, IMGLIST,
//! IPL, ...) and an `images.txt` (its IMG archives). With the episodes enabled in their `setup2.xml`
//! the game reads both of them at start, next to the base game's own data, but they hold the
//! episodes' whole maps, interiors, cutscene animations and scripts, which don't fit the base game's
//! streaming tables and would overwrite its map. So the lines the game reads from them are filtered:
//! what stays is what the base world can use, the vehicles, peds and weapons and their data.
//!
//! Two hooks do it: the game's `fopen` (which file a handle is) and its line reader (called again
//! for the next line while the current one is dropped).

use std::collections::HashMap;
use std::ffi::{CStr, c_char};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use ivmp_core::memory;

use crate::log;

/// `FILE* fopen(path, mode)`-like, cdecl.
const FILE_OPEN: usize = 0x008C_4CF0;
/// `char* read_line(handle, flag)`: the next line of a data file, in a static buffer, 0 at the end.
const READ_LINE: usize = 0x008C_4E70;

static OPEN_ORIGINAL: AtomicUsize = AtomicUsize::new(0);
static READ_ORIGINAL: AtomicUsize = AtomicUsize::new(0);
static TRACE: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Copy, PartialEq, Debug)]
enum Kind {
    /// An episode's `content.dat`.
    Content,
    /// An episode's `images.txt` (its IMG archives).
    Images,
}

/// The episode files being read: handle -> kind. Everything else (the base game's data, the IDE
/// files) isn't looked at.
static FILES: Mutex<Option<HashMap<u32, Kind>>> = Mutex::new(None);

/// Which filtered file `path` is. The base game's files have a device prefix (`common:/`), the
/// episode's are relative to its own folder.
fn kind_of(path: &str) -> Option<Kind> {
    let path = path.replace('\\', "/").to_ascii_lowercase();
    if path.contains(':') {
        return None;
    }
    if path == "content.dat" {
        Some(Kind::Content)
    } else if path.ends_with("/images.txt") || path == "images.txt" {
        Some(Kind::Images)
    } else {
        None
    }
}

/// Whether a line of an episode's `content.dat` stays: vehicles, peds, weapons and their data.
fn keep_content_line(line: &str) -> bool {
    let line = line.trim();
    let mut words = line.split_whitespace();
    let Some(key) = words.next() else { return true };
    if key.starts_with('#') {
        return true;
    }
    let argument = words.next().unwrap_or("").replace('\\', "/").to_ascii_lowercase();
    match key.to_ascii_uppercase().as_str() {
        // The image archives (filtered themselves) and the data tables of vehicles, peds, weapons.
        "IMGLIST" | "HANDLING" | "CARCOLS" | "VEHICLEEXTRAS" | "CARGRP" | "PEDGRP" | "WEAPONINFO" | "THROWNWEAPONINFO" => true,
        // Model definitions: the episode's own (common/data/*.ide), not those of its maps.
        "IDE" => argument.starts_with("common/data/") || argument.starts_with("common:/data/"),
        _ => false,
    }
}

/// Whether a line of an episode's `images.txt` stays: the model archives (vehicles, weapons, ped
/// components and props), not maps, interiors, cutscene animations, scripts or paths.
fn keep_image_line(line: &str) -> bool {
    let line = line.trim().replace('\\', "/").to_ascii_lowercase();
    line.is_empty() || line.starts_with('#') || line.starts_with("pc/models/cdimages/")
}

extern "C" fn file_open(path: *const c_char, mode: *const c_char) -> u32 {
    let original: extern "C" fn(*const c_char, *const c_char) -> u32 = unsafe { std::mem::transmute(OPEN_ORIGINAL.load(Ordering::SeqCst)) };
    let handle = original(path, mode);
    if !path.is_null() && !mode.is_null() {
        let (path, mode) = unsafe { (CStr::from_ptr(path).to_string_lossy(), CStr::from_ptr(mode).to_string_lossy()) };
        if TRACE.load(Ordering::Relaxed) {
            log!("datfile: {mode} {path}");
        }
        if mode.starts_with('r') && handle != 0 {
            let mut files = FILES.lock().unwrap();
            let files = files.get_or_insert_with(HashMap::new);
            match kind_of(&path) {
                Some(kind) => {
                    files.insert(handle, kind);
                }
                None => {
                    files.remove(&handle);
                }
            }
        }
    }
    handle
}

extern "C" fn read_line(handle: u32, flag: i32) -> *const c_char {
    let original: extern "C" fn(u32, i32) -> *const c_char = unsafe { std::mem::transmute(READ_ORIGINAL.load(Ordering::SeqCst)) };
    let kind = FILES.lock().unwrap().as_ref().and_then(|files| files.get(&handle).copied());
    loop {
        let line = original(handle, flag);
        let Some(kind) = kind else { return line };
        if line.is_null() {
            return line;
        }
        let text = unsafe { CStr::from_ptr(line) }.to_string_lossy();
        let keep = match kind {
            Kind::Content => keep_content_line(&text),
            Kind::Images => keep_image_line(&text),
        };
        if keep {
            return line;
        }
        if TRACE.load(Ordering::Relaxed) {
            log!("datfile: skipped {kind:?} line: {}", text.trim());
        }
    }
}

// ---- The vehicle handling table ------------------------------------------------------------------
//
// handling.dat fills a static table of 160 entries of 0x110 bytes (0x016DD6A0..0x016E80A0), the
// game's 125 vehicles first. The episodes add about 41 more, so it has to grow. And an entry
// the loader appends (FUN_00C73FC0, by name) isn't like the game's own: those carry three
// pointers (+0xF8 boat, +0xFC bike, +0x100 flying) to the start of their category's slot array, which
// the `!`, `$` and `%` lines check and then fill a slot behind; an appended entry has 0 there, so the
// first bike line of a new vehicle writes through a null pointer.

const HANDLING_TABLE: usize = 0x016D_D6A0;
const HANDLING_TABLE_END: usize = 0x016E_80A0;
const HANDLING_ENTRY_SIZE: usize = 0x110;
/// The category markers an entry starts with: (offset in the entry, start of the slot array).
const HANDLING_MARKERS: [(usize, usize); 3] = [(0xF8, 0x016E_80A0), (0xFC, 0x016E_8AA0), (0x100, 0x016E_99A0)];
/// `int append_or_find(const char* name)`, cdecl: the index of the named entry, new ones appended.
const HANDLING_FIND_OR_ADD: usize = 0x00C7_3FC0;
const HANDLING_COUNT: usize = 0x016D_D690;

/// Where the table lives now (0 = it was not moved).
static HANDLING_BASE: AtomicUsize = AtomicUsize::new(0);
static HANDLING_FIND_ORIGINAL: AtomicUsize = AtomicUsize::new(0);

/// Moves the handling table into a buffer twice as big, before the game uses it: the game's
/// entries are copied, and every instruction that holds an address inside the old table is pointed
/// into the new one (found by decoding the code). Call early (with the model pools), from the first thread after the unpack.
pub fn relocate_handling() {
    use windows::Win32::System::Memory::{MEM_COMMIT, MEM_RESERVE, PAGE_READWRITE, VirtualAlloc};
    if crate::config().dlc < 2 {
        return;
    }
    let (old_start, old_end) = (memory::from_ghidra(HANDLING_TABLE), memory::from_ghidra(HANDLING_TABLE_END));
    let size = old_end - old_start;
    let new_start = unsafe { VirtualAlloc(None, size * 2, MEM_COMMIT | MEM_RESERVE, PAGE_READWRITE) } as usize;
    if new_start == 0 || new_start > u32::MAX as usize {
        log!("handling: no memory for a bigger table, the game keeps its own");
        return;
    }
    let mut table = vec![0u8; size];
    if !memory::read(old_start, &mut table) {
        log!("handling: cannot read the table");
        return;
    }
    unsafe { std::ptr::copy_nonoverlapping(table.as_ptr(), new_start as *mut u8, size) };
    // The address operands in the code that point into the old table, found by decoding the
    // instructions (a search of the raw bytes also finds bytes of unrelated instructions).
    let mut patched = 0;
    for field in ivmp_core::hook::find_address_operands(old_start..old_end) {
        let mut current = [0u8; 4];
        if !memory::read(field, &mut current) {
            continue;
        }
        let moved = (new_start + (u32::from_le_bytes(current) as usize - old_start)) as u32;
        if unsafe { memory::patch(field, &moved.to_le_bytes()) }.is_ok() {
            patched += 1;
        }
    }
    HANDLING_BASE.store(new_start, Ordering::SeqCst);
    log!("handling: the table (160 entries) moved to {new_start:#x} with room for 320, {patched} address(es) in the code updated");
}

/// A name the handling loader looks up or appends: a new entry gets the category markers the game's
/// own entries have.
extern "C" fn handling_find_or_add(name: *const c_char) -> i32 {
    let original: extern "C" fn(*const c_char) -> i32 = unsafe { std::mem::transmute(HANDLING_FIND_ORIGINAL.load(Ordering::SeqCst)) };
    let count = unsafe { *(memory::from_ghidra(HANDLING_COUNT) as *const i32) };
    let index = original(name);
    let base = HANDLING_BASE.load(Ordering::SeqCst);
    if index >= count && base != 0 {
        // Appended just now: the markers.
        let entry = base + index as usize * HANDLING_ENTRY_SIZE;
        for (offset, marker) in HANDLING_MARKERS {
            unsafe { *((entry + offset) as *mut u32) = memory::from_ghidra(marker) as u32 };
        }
    }
    index
}

/// Installs the hooks. With `dlctrace.txt` next to the DLL every opened file and every dropped line is logged.
pub fn install() {
    TRACE.store(crate::dll_dir().join("dlctrace.txt").exists(), Ordering::Relaxed);
    for (name, address, hook, original) in [
        ("file open", FILE_OPEN, file_open as usize, &OPEN_ORIGINAL),
        ("line reader", READ_LINE, read_line as usize, &READ_ORIGINAL),
    ] {
        match unsafe { ivmp_core::hook::install(memory::from_ghidra(address), hook) } {
            Ok(installed) => original.store(installed.trampoline, Ordering::SeqCst),
            Err(e) => {
                log!("dlc: the {name} hook failed: {e}: the episode content stays unfiltered");
                return;
            }
        }
    }
    match unsafe { ivmp_core::hook::install(memory::from_ghidra(HANDLING_FIND_OR_ADD), handling_find_or_add as usize) } {
        Ok(installed) => HANDLING_FIND_ORIGINAL.store(installed.trampoline, Ordering::SeqCst),
        Err(e) => log!("dlc: the handling hook failed: {e}"),
    }
    log!("dlc: episode content is filtered to vehicles, peds and weapons");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn episode_files_are_told_from_the_base_games() {
        assert_eq!(kind_of("content.dat"), Some(Kind::Content));
        assert_eq!(kind_of("common/data/images.txt"), Some(Kind::Images));
        assert_eq!(kind_of("common\\data\\IMAGES.TXT"), Some(Kind::Images));
        assert_eq!(kind_of("common:/data/default.dat"), None);
        assert_eq!(kind_of("e1:/content.dat"), None);
        assert_eq!(kind_of("common/data/default.ide"), None);
    }

    #[test]
    fn content_lines_keep_vehicles_peds_and_weapons() {
        for keep in [
            "# data files",
            "",
            "HANDLING common/data/handling.dat",
            "CARCOLS common/data/carcols.dat",
            "WEAPONINFO common/data/weaponinfo.xml",
            "IMGLIST common/data/images.txt",
            "IDE common/data/vehicles.ide",
            "IDE common/data/peds.ide",
            "IDE common/data/default.ide",
            "PEDGRP common/data/pedgrp.dat",
        ] {
            assert!(keep_content_line(keep), "{keep}");
        }
        for drop in [
            "GXT common/text",
            "IPL common/data/maps/SCENARIO2E1.ipl",
            "IPL pc/data/maps/jersey/nj_01.ipl",
            "IDE pc/data/maps/jersey/nj_03_extra.ide",
            "DISABLE_FILE common:/DATA/MAPS/SCENARIO2.IPL",
            "TIMECYCLE pc/data/timecyc.dat",
            "PLAYER pc/models/cdimages/playerped.rpf",
            "CREDITS common/data/credits_pc.dat",
            "FONTTXD pc/textures/fonts.wtd",
        ] {
            assert!(!keep_content_line(drop), "{drop}");
        }
    }

    #[test]
    fn image_lines_keep_only_the_model_archives() {
        assert!(keep_image_line("pc/models/cdimages/vehicles"));
        assert!(keep_image_line("pc/models/cdimages/componentpeds"));
        assert!(keep_image_line(""));
        for drop in ["pc/data/maps/jersey/nj_01", "pc/anim/cuts", "common/data/cdimages/script", "pc/data/maps/interiors/Int_1"] {
            assert!(!keep_image_line(drop), "{drop}");
        }
    }
}

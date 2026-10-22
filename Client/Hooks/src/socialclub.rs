//! No Social Club in IVMP: the game never loads `socialclub.dll`, so there is no Social Club
//! sign-in, no Social Club window or hotkey, and the local gamer name is IVMP's nickname.
//!
//! The game's rgsc loader (0x6C19F0, called from 0x6C2390) finds `socialclub.dll`, loads it, asks
//! it for its interfaces and starts it. Every way it can fail leaves the same state: the rgsc
//! pointers (0x19F262C..0x19F2670) zero, an error code in 0x19F269C, and a result of 0; the game
//! then simply runs without Social Club, as it does on a machine that has none installed. We take
//! that "not installed" path (error 1) ourselves, before anything is loaded.

use crate::{log, memory};

/// The call to the rgsc loader inside 0x6C2390 (`call 0x6C19F0`).
const CALL_SITE: usize = 0x006C_2396;
const LOADER: usize = 0x006C_19F0;
/// The rgsc interface pointers and the module handle the loader fills (17 dwords).
const STATE: usize = 0x019F_262C;
const STATE_DWORDS: usize = 17;
/// The loader's error code (0 = none, 1 = socialclub.dll not found).
const ERROR_CODE: usize = 0x019F_269C;

/// Replaces the loader: reports "Social Club is not installed".
extern "C" fn no_social_club() -> u32 {
    unsafe {
        let state = memory::from_ghidra(STATE) as *mut u32;
        for i in 0..STATE_DWORDS {
            *state.add(i) = 0;
        }
        let error = memory::from_ghidra(ERROR_CODE) as *mut u32;
        if *error == 0 {
            *error = 1;
        }
    }
    static LOGGED: std::sync::Once = std::sync::Once::new();
    LOGGED.call_once(|| log!("socialclub: the game asked to load Social Club; reported it as not installed"));
    0
}

/// Skips the game's Social Club loader. Call before the game starts its online services.
pub fn install() {
    let site = memory::from_ghidra(CALL_SITE);
    let mut call = [0u8; 5];
    if !memory::read(site, &mut call) || call[0] != 0xE8 {
        log!("socialclub: unexpected code at {site:#x}, Social Club is left alone");
        return;
    }
    let target = (site as isize + 5 + i32::from_le_bytes([call[1], call[2], call[3], call[4]]) as isize) as usize;
    if target != memory::from_ghidra(LOADER) {
        log!("socialclub: the call at {site:#x} goes to {target:#x}, not the rgsc loader; Social Club is left alone");
        return;
    }
    // Already loaded before we got here? Then it is too late to keep it out.
    let loaded = unsafe { *(memory::from_ghidra(STATE + 0x40) as *const usize) } != 0;
    match unsafe { memory::redirect_call(site, no_social_club as usize) } {
        Ok(_) if loaded => log!("socialclub: hooked the loader, but socialclub.dll was already loaded"),
        Ok(_) => log!("socialclub: the game's Social Club loader is skipped"),
        Err(e) => log!("socialclub: could not hook the loader: {e}"),
    }
}

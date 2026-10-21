//! Logs which script natives get called, to find the ones the multiplayer lobby script uses
//! (e.g. what "start game" does). Each native is logged the first time it is called after
//! [`enable`], with its hash.
//!
//! The script VM's native opcode (CTheScripts thread run, 0x86D0A0) ends in
//! `push eax; mov [esp+0xA0], 0; call ecx` with ECX = the native handler. The `mov` + `call ecx`
//! are replaced by a call to [`native_thunk`], which logs and then jumps to the handler.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

use crate::{log, memory};

const CALL_SITE_PATTERN: &str = "50 C7 84 24 A0 00 00 00 00 00 00 00 FF D1 8B 47 0C";
/// Native registrations look like `push handler; push hash; call register`.
const REGISTRATION_PATTERN: [u8; 1] = [0x68];

static ENABLED: AtomicBool = AtomicBool::new(false);
static HASHES: OnceLock<HashMap<usize, u32>> = OnceLock::new();
static HANDLERS: OnceLock<HashMap<u32, usize>> = OnceLock::new();

/// Runtime address of the handler for native `hash` (after [`install`]).
pub fn handler_for(hash: u32) -> Option<usize> {
    HANDLERS.get().and_then(|h| h.get(&hash).copied())
}
static SEEN: Mutex<Option<HashSet<usize>>> = Mutex::new(None);

/// Builds the handler -> hash table and patches the VM. Call from a background thread at startup.
pub fn install() {
    let mut hashes = HashMap::new();
    let code = memory::code_section();
    let (lo, hi) = (code.as_ptr() as usize, code.as_ptr() as usize + code.len());
    for (start, run) in memory::snapshot(code) {
        let _ = start;
        for i in 0..run.len().saturating_sub(15) {
            if run[i] == REGISTRATION_PATTERN[0] && run[i + 5] == 0x68 && run[i + 10] == 0xE8 {
                let handler = u32::from_le_bytes(run[i + 1..i + 5].try_into().unwrap()) as usize;
                let hash = u32::from_le_bytes(run[i + 6..i + 10].try_into().unwrap());
                if (lo..hi).contains(&handler) {
                    hashes.insert(handler, hash);
                }
            }
        }
    }
    log!("natives: {} registrations found", hashes.len());
    let _ = HANDLERS.set(hashes.iter().map(|(handler, hash)| (*hash, *handler)).collect());
    let _ = HASHES.set(hashes);

    let Some(site) = memory::find(CALL_SITE_PATTERN).map(|a| a + 1) else {
        log!("natives: VM call site not found, native logging off");
        return;
    };
    let rel = (native_thunk as usize as isize - (site as isize + 5)) as i32;
    let mut bytes = [0x90u8; 13];
    bytes[0] = 0xE8;
    bytes[1..5].copy_from_slice(&rel.to_le_bytes());
    match unsafe { memory::patch(site, &bytes) } {
        Ok(()) => log!("natives: logging hook at {site:#x}"),
        Err(e) => log!("natives: hook at {site:#x} failed: {e}"),
    }
}

/// Starts logging (first call of each native from now on).
pub fn enable() {
    *SEEN.lock().unwrap() = Some(HashSet::new());
    ENABLED.store(true, Ordering::SeqCst);
    log!("natives: logging enabled");
}

/// Natives scripts call that go to our function instead: (game handler, replacement).
static OVERRIDES: [(AtomicUsize, AtomicUsize); 4] = [const { (AtomicUsize::new(0), AtomicUsize::new(0)) }; 4];

/// Script calls of native `hash` go to `replacement` (`extern "C" fn(context)`, the handler's
/// signature) from now on; it can call the game's handler, which this returns. Our own
/// [`crate::natives::invoke`] calls aren't redirected.
pub fn set_override(hash: u32, replacement: usize) -> Option<usize> {
    let handler = handler_for(hash)?;
    for (from, to) in &OVERRIDES {
        if from.load(Ordering::SeqCst) == 0 || from.load(Ordering::SeqCst) == handler {
            to.store(replacement, Ordering::SeqCst);
            from.store(handler, Ordering::SeqCst);
            log!("natives: script calls of {hash:#010x} are overridden");
            return Some(handler);
        }
    }
    log!("natives: no room to override {hash:#010x}");
    None
}

/// Logs the first call of each native; returns the handler to run.
extern "C" fn record(handler: usize) -> usize {
    let handler_to_run = OVERRIDES
        .iter()
        .find(|(from, _)| from.load(Ordering::Relaxed) == handler)
        .map_or(handler, |(_, to)| to.load(Ordering::Relaxed));
    if !ENABLED.load(Ordering::Relaxed) {
        return handler_to_run;
    }
    let mut seen = SEEN.lock().unwrap();
    let Some(seen) = seen.as_mut() else { return handler_to_run };
    if seen.insert(handler) {
        let ghidra = handler - memory::game_image().as_ptr() as usize + memory::GHIDRA_BASE;
        match HASHES.get().and_then(|h| h.get(&handler)) {
            Some(hash) => log!("natives: first call {hash:#010x} (handler {ghidra:#x})"),
            None => log!("natives: first call ? (handler {ghidra:#x})"),
        }
    }
    handler_to_run
}

/// Entered by `call` from the VM with ECX = native handler and the native context pushed.
#[unsafe(naked)]
extern "C" fn native_thunk() {
    core::arch::naked_asm!(
        // The instruction we replaced: mov dword ptr [esp+0xA0], 0 (one return address deeper now).
        "mov dword ptr [esp + 0xA4], 0",
        "pushad",
        "pushfd",
        "push ecx",
        "call {record}",
        "add esp, 4",
        // ECX in the saved registers (flags at [esp], then EDI..EAX) = the handler to run.
        "mov [esp + 0x1C], eax",
        "popfd",
        "popad",
        // Tail-jump: the handler returns straight to the VM.
        "jmp ecx",
        record = sym record,
    );
}

// --- Script names --------------------------------------------------------------------------------
// CTheScripts_FindScriptIndexByName (0x94B5A0, cdecl `int (const char* name)`) is how scripts are
// requested and started by name. Logging its calls shows which scripts the lobby and the game mode
// use.

const FIND_SCRIPT_BY_NAME: usize = 0x0094_B5A0;
static ORIGINAL_FIND_SCRIPT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static SEEN_SCRIPTS: Mutex<Option<HashSet<String>>> = Mutex::new(None);

pub fn install_script_names() {
    let target = memory::from_ghidra(FIND_SCRIPT_BY_NAME);
    ORIGINAL_FIND_SCRIPT.store(target, Ordering::SeqCst);
    let calls = memory::find_calls_to(target);
    for call in &calls {
        if let Err(e) = unsafe { memory::redirect_call(*call, find_script_by_name as usize) } {
            log!("natives: script name hook at {call:#x} failed: {e}");
        }
    }
    log!("natives: script name hook at {} call sites", calls.len());
}

extern "C" fn find_script_by_name(name: *const std::ffi::c_char) -> i32 {
    let original: extern "C" fn(*const std::ffi::c_char) -> i32 =
        unsafe { std::mem::transmute(ORIGINAL_FIND_SCRIPT.load(Ordering::SeqCst)) };
    let index = original(name);
    if ENABLED.load(Ordering::Relaxed) && !name.is_null() {
        let name = unsafe { std::ffi::CStr::from_ptr(name) }.to_string_lossy().into_owned();
        let mut seen = SEEN_SCRIPTS.lock().unwrap();
        if seen.get_or_insert_with(HashSet::new).insert(name.clone()) {
            log!("natives: script {name:?} (index {index})");
        }
    }
    index
}

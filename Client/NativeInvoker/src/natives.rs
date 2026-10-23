//! Calling script natives from our code.
//!
//! A native handler is `void __cdecl handler(NativeContext*)`; the script VM passes
//! `{ return slot pointer, argument count, argument array }` (see the native opcode at 0x86D84A,
//! and e.g. NETWORK_HOST_GAME's handler 0xBD8630 reading `ctx+8` and writing `*ctx`).
//! Handlers are looked up by hash in the registration table that [`crate::nativelog`] builds.

use std::sync::atomic::{AtomicUsize, Ordering};

use crate::{log, memory, nativelog};

#[repr(C)]
struct NativeContext {
    ret: *mut u32,
    argc: u32,
    args: *mut u32,
    /// The VM zeroes the field at +0x10; keep spare room.
    reserved: [u32; 4],
}

/// The script thread the VM is running (0 outside of a script): natives that register what a script
/// owns (model requests, entities it creates) read it, and crash on a read at offset 0x94 of it when
/// they are called without one, as from our own Lua runtime.
const CURRENT_THREAD: usize = 0x01BB_54DC;
/// The game's script threads: a pointer array and its count (a u16). A live thread has an id at +4
/// and a state at +0xC (2 and 3 are over).
const THREAD_TABLE: usize = 0x01B4_AF04;
const THREAD_COUNT: usize = 0x01B4_AF08;

/// The script thread the VM ran last (0 until one has run): [`install_thread_capture`] keeps it.
/// The thread objects are a fixed pool, so the pointer stays valid memory.
static LAST_THREAD: AtomicUsize = AtomicUsize::new(0);
/// The VM's run function (thiscall, `ecx` = the thread), the original behind our hook.
const VM_RUN: usize = 0x0086_D0A0;
static VM_RUN_ORIGINAL: AtomicUsize = AtomicUsize::new(0);
/// How many times the hook ran (a check that it does).
static VM_RUNS: AtomicUsize = AtomicUsize::new(0);

/// Entered instead of the VM's run function: remembers the thread (the VM only runs threads that
/// are not over, states 2 and 3) and carries on with the original, with everything as it was.
#[unsafe(naked)]
extern "C" fn vm_run_hook() {
    core::arch::naked_asm!(
        "inc dword ptr [{runs}]",
        "cmp dword ptr [ecx + 0xC], 2",
        "je 2f",
        "cmp dword ptr [ecx + 0xC], 3",
        "je 2f",
        "mov dword ptr [{last}], ecx",
        "2:",
        "jmp dword ptr [{original}]",
        last = sym LAST_THREAD,
        original = sym VM_RUN_ORIGINAL,
        runs = sym VM_RUNS,
    );
}

/// Hooks the script VM to remember the thread it runs, for [`call_handler`]. Call once at start.
pub fn install_thread_capture() {
    match unsafe { ivmp_core::hook::install(memory::from_ghidra(VM_RUN), vm_run_hook as usize) } {
        Ok(hook) => {
            VM_RUN_ORIGINAL.store(hook.trampoline, Ordering::SeqCst);
            let mut head = [0u8; 8];
            memory::read(memory::from_ghidra(VM_RUN), &mut head);
            log!("natives: the script VM is hooked to know its current thread (first bytes now {head:02x?}, trampoline {:#x})", hook.trampoline);
        }
        Err(e) => log!("natives: could not hook the script VM: {e}"),
    }
}

fn live_thread() -> usize {
    let last = LAST_THREAD.load(Ordering::Relaxed);
    if last != 0 {
        return last;
    }
    unsafe {
        let table = *(memory::from_ghidra(THREAD_TABLE) as *const usize);
        let count = *(memory::from_ghidra(THREAD_COUNT) as *const u16) as usize;
        if table == 0 {
            return 0;
        }
        for i in 0..count {
            let thread = *((table + i * 4) as *const usize);
            if thread != 0 && *((thread + 4) as *const u32) != 0 && !matches!(*((thread + 0xC) as *const u32), 2 | 3) {
                return thread;
            }
        }
        // No script runs (IVMP skips the game's startup scripts in its own modes): the game's pool of
        // thread objects is all unused, real objects with their vtables. The first one is the
        // "script" our calls run as; natives only read their fields and record it as an owner.
        if count > 0 {
            let first = *(table as *const usize);
            if first != 0 {
                static SAID: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
                if !SAID.swap(true, Ordering::Relaxed) {
                    log!("natives: no script runs, calls use the thread object {first:#x} of the game's pool");
                }
                return first;
            }
        }
    }
    0
}

/// Calls the native handler with a script thread current (a live one of the game's, when the
/// caller is not inside a script already) and puts the previous one back.
fn call_handler(handler: usize, context: &mut NativeContext) {
    unsafe {
        let current = memory::from_ghidra(CURRENT_THREAD) as *mut usize;
        let saved = *current;
        if saved == 0 {
            *current = live_thread();
        }
        let handler: extern "C" fn(*mut NativeContext) = std::mem::transmute(handler);
        handler(context);
        *current = saved;
    }
}

/// Calls the native `hash` with 32-bit arguments. Returns the first return slot, or `None` if
/// the native isn't known. Game thread only.
pub fn invoke(hash: u32, args: &[u32]) -> Option<u32> {
    let Some(handler) = nativelog::handler_for(hash) else {
        log!("natives: no handler for {hash:#010x}");
        return None;
    };
    let mut ret = [0u32; 4];
    let mut argv = [0u32; 16];
    argv[..args.len()].copy_from_slice(args);
    let mut context =
        NativeContext { ret: ret.as_mut_ptr(), argc: args.len() as u32, args: argv.as_mut_ptr(), reserved: [0; 4] };
    call_handler(handler, &mut context);
    Some(ret[0])
}

/// Calls the native `hash` and returns all its return slots (vectors come back as three floats in
/// the first three). Game thread only; `None` if the native isn't known.
pub fn invoke_slots(hash: u32, args: &[u32]) -> Option<[u32; 4]> {
    let handler = nativelog::handler_for(hash)?;
    let mut ret = [0u32; 4];
    let mut argv = [0u32; 16];
    let count = args.len().min(16);
    argv[..count].copy_from_slice(&args[..count]);
    let mut context = NativeContext { ret: ret.as_mut_ptr(), argc: count as u32, args: argv.as_mut_ptr(), reserved: [0; 4] };
    call_handler(handler, &mut context);
    Some(ret)
}

/// What the invoker sees of the script threads, for the log: (the VM's current thread now, the
/// last one the VM ran, how often the VM was run).
pub fn thread_state() -> (usize, usize, usize) {
    let current = unsafe { *(memory::from_ghidra(CURRENT_THREAD) as *const usize) };
    (current, LAST_THREAD.load(Ordering::Relaxed), VM_RUNS.load(Ordering::Relaxed))
}

/// Where the local player is, from the game (GET_PLAYER_CHAR, GET_CHAR_COORDINATES), or None before
/// there is a player. Game thread only.
pub fn player_position() -> Option<[f32; 3]> {
    const GET_PLAYER_CHAR: u32 = 0x5114_54A9;
    const GET_CHAR_COORDINATES: u32 = 0x5145_7269;
    let mut ped = [0u32; 4];
    invoke_slots(GET_PLAYER_CHAR, &[0, ped.as_mut_ptr() as usize as u32])?;
    if ped[0] == 0 || ped[0] == u32::MAX {
        return None;
    }
    let (mut x, mut y, mut z) = ([0u32; 4], [0u32; 4], [0u32; 4]);
    invoke_slots(GET_CHAR_COORDINATES, &[ped[0], x.as_mut_ptr() as usize as u32, y.as_mut_ptr() as usize as u32, z.as_mut_ptr() as usize as u32])?;
    let position = [f32::from_bits(x[0]), f32::from_bits(y[0]), f32::from_bits(z[0])];
    position.iter().all(|v| v.is_finite()).then_some(position)
}

/// Whether this game build has a native with this hash.
pub fn exists(hash: u32) -> bool {
    nativelog::handler_for(hash).is_some()
}

/// [`invoke`] for natives returning a bool.
pub fn invoke_bool(hash: u32, args: &[u32]) -> bool {
    invoke(hash, args).is_some_and(|r| r & 0xFF != 0)
}

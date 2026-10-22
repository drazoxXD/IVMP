//! Calling script natives from our code.
//!
//! A native handler is `void __cdecl handler(NativeContext*)`; the script VM passes
//! `{ return slot pointer, argument count, argument array }` (see the native opcode at 0x86D84A,
//! and e.g. NETWORK_HOST_GAME's handler 0xBD8630 reading `ctx+8` and writing `*ctx`).
//! Handlers are looked up by hash in the registration table that [`crate::nativelog`] builds.

use crate::{log, nativelog};

#[repr(C)]
struct NativeContext {
    ret: *mut u32,
    argc: u32,
    args: *mut u32,
    /// The VM zeroes the field at +0x10; keep spare room.
    reserved: [u32; 4],
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
    unsafe {
        let handler: extern "C" fn(*mut NativeContext) = std::mem::transmute(handler);
        handler(&mut context);
    }
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
    unsafe {
        let handler: extern "C" fn(*mut NativeContext) = std::mem::transmute(handler);
        handler(&mut context);
    }
    Some(ret)
}

/// Whether this game build has a native with this hash.
pub fn exists(hash: u32) -> bool {
    nativelog::handler_for(hash).is_some()
}

/// [`invoke`] for natives returning a bool.
pub fn invoke_bool(hash: u32, args: &[u32]) -> bool {
    invoke(hash, args).is_some_and(|r| r & 0xFF != 0)
}

//! Game packets in the clear, so the IVMP server can read them (docs/onesync-plan.md, phase 1).
//!
//! The engine encrypts every packet with RC4 (`rage::netConnection`): the sender (0x6D2020)
//! builds `[flags u8][nonce u32][payload][mac u32]` and RC4s the payload at 0x6D2200; the
//! receiver (0x6D2320) RC4s it back at 0x6D25B6. Both calls go to the same keystream function
//! (thiscall `rc4(state, data, len)`, 0x6F1530). Pointing just those two calls at a no-op leaves
//! the payload unencrypted; the MAC stays and still matches, since both sides now see the same
//! bytes. Every client must run this, or packets between them are garbage.

use crate::{log, memory};

const ENCRYPT_CALL: usize = 0x006D_2200;
const DECRYPT_CALL: usize = 0x006D_25B6;
const RC4: usize = 0x006F_1530;

/// thiscall with two stack arguments: return and pop them.
#[unsafe(naked)]
extern "C" fn no_rc4() {
    core::arch::naked_asm!("ret 8");
}

pub fn install() {
    let rc4 = memory::from_ghidra(RC4);
    for (site, name) in [(ENCRYPT_CALL, "encryption"), (DECRYPT_CALL, "decryption")] {
        let site = memory::from_ghidra(site);
        let rel = unsafe { std::ptr::read_unaligned((site + 1) as *const i32) };
        let target = (site as isize + 5 + rel as isize) as usize;
        if unsafe { *(site as *const u8) } != 0xE8 || target != rc4 {
            log!("crypt: packet {name} call not found at {site:#x}, packets stay encrypted");
            continue;
        }
        match unsafe { memory::redirect_call(site, no_rc4 as usize) } {
            Ok(_) => log!("crypt: packet {name} off"),
            Err(e) => log!("crypt: packet {name} patch failed: {e}"),
        }
    }
}

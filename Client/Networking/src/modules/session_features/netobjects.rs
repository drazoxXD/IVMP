//! Network object fixes.
//!
//! No client-only entities: the ped and vehicle factories (CPedFactoryNY 0x5C3770,
//! CVehicleFactoryNY 0x5DA990) register new entities as network objects in a network game and
//! delete them when that fails, except "forced" peds and mission (type 2) vehicles, which they
//! keep as local-only. Those exceptions are removed, so an entity that can't be networked is
//! never spawned.
//!
//! Peds waiting to become network objects sit in a 10-slot queue (0x18B6788, 12 bytes each, ped
//! first; added by 0x4B52C0). The function doesn't check for duplicates, so while a copy has no
//! object ids its player was queued again and again, and once ids arrived every entry became its
//! own network object: the host then showed ten copies of the second player. Only queue a ped
//! that isn't already waiting.

use std::sync::atomic::{AtomicUsize, Ordering};

use crate::{log, memory};

const QUEUE_PED: usize = 0x004B_52C0;
const QUEUE: usize = 0x018B_6788;
const QUEUE_SLOTS: usize = 10;
const QUEUE_SLOT_SIZE: usize = 12;

static ORIGINAL_QUEUE_PED: AtomicUsize = AtomicUsize::new(0);

extern "C" fn queue_ped(ped: usize, a: u32, b: u32) {
    let queue = memory::from_ghidra(QUEUE);
    let already = (0..QUEUE_SLOTS).any(|slot| unsafe { *((queue + slot * QUEUE_SLOT_SIZE) as *const usize) } == ped);
    if already {
        return;
    }
    let original: extern "C" fn(usize, u32, u32) = unsafe { std::mem::transmute(ORIGINAL_QUEUE_PED.load(Ordering::SeqCst)) };
    original(ped, a, b);
}

/// (entity, pattern, offset of the jump, its original bytes); the jump becomes two nops.
const NO_LOCAL_ONLY: [(&str, &str, usize, [u8; 2]); 2] = [
    // `call register; test al, al; jnz ok; test bl, bl (forced); jnz ok` -> drop the second jnz.
    ("ped", "84 C0 75 10 84 DB 75 0C 8B 07 56 8B CF FF 50 18", 6, [0x75, 0x0C]),
    // `test al, al; jnz ok; cmp edi, 2 (mission); jz ok` -> drop the jz.
    ("vehicle", "84 C0 75 10 83 FF 02 74 0B 8B 45 00 56 8B CD FF 50 08", 7, [0x74, 0x0B]),
];

fn forbid_local_only_entities() {
    for (name, pattern, offset, original) in NO_LOCAL_ONLY {
        let Some(site) = memory::find(pattern).map(|a| a + offset) else {
            log!("netobjects: {name} factory check not found");
            continue;
        };
        if unsafe { std::slice::from_raw_parts(site as *const u8, 2) } != original {
            log!("netobjects: {name} factory check has unexpected bytes");
            continue;
        }
        match unsafe { memory::patch(site, &[0x90, 0x90]) } {
            Ok(()) => log!("netobjects: a {name} that can't be networked is deleted, never kept local-only"),
            Err(e) => log!("netobjects: {name} patch failed: {e}"),
        }
    }
}

pub fn install() {
    forbid_local_only_entities();
    let target = memory::from_ghidra(QUEUE_PED);
    ORIGINAL_QUEUE_PED.store(target, Ordering::SeqCst);
    let calls = memory::find_calls_to(target);
    for call in &calls {
        let _ = unsafe { memory::redirect_call(*call, queue_ped as usize) };
    }
    log!("netobjects: peds are queued for registration only once ({} call sites)", calls.len());
}

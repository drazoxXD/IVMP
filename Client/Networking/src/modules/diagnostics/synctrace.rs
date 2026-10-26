//! Diagnostics: what this game does with the clone syncs it receives.
//!
//! CMsgCloneSync (0x8A) entries are applied one by one (0x492820 calls 0x493440 at 0x492A2F:
//! thiscall on the object manager, stack (sender, object type, object id, ...), returns a result).
//! The result goes back to the sender in CMsgPackedCloneSyncAcks (writer 0x4EDDC0): 5 = applied
//! (no item), 3 = no such object here, 2 = the sender may not sync it (0x48E000, or the object
//! is being removed), 1 = the sync didn't apply (node read, vtable +0xB4) or a ped <-> dummy ped
//! switch failed (0x495FE0), or the thread flags lack bit 1.
//!
//! Logged: every failure (rate-limited per object and result) with the local net object's
//! type, owner and clone flag, and every 30 s a summary of results per object type.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::time::Instant;

use crate::{log, memory};

const APPLY_CALL: usize = 0x0049_2A2F;
/// rage::netObjectMgr (CNetworkObjectMgr) and its lookup by object id (thiscall (id, 1)).
const OBJECT_MGR: usize = 0x0190_8EF0;
const OBJECT_BY_ID: usize = 0x0049_13C0;
/// Object types (0x48EB30).
const TYPES: [&str; 10] = ["PLAYER", "PED", "DUMMYPED", "AUTOMOBILE", "OBJECT", "BIKE", "TRAIN", "BOAT", "HELI", "PLANE"];

static ORIGINAL: AtomicUsize = AtomicUsize::new(0);
static RETURN: AtomicUsize = AtomicUsize::new(0);
static SENDER: AtomicU32 = AtomicU32::new(0);
static KIND: AtomicU32 = AtomicU32::new(0);
static OBJECT: AtomicU32 = AtomicU32::new(0);

struct Stats {
    started: Instant,
    last_summary: Instant,
    /// (type, result) -> count since the last summary.
    results: HashMap<(u32, u32), u32>,
    /// (object, result) -> failures seen.
    failures: HashMap<(u16, u32), u32>,
}

static STATS: Mutex<Option<Stats>> = Mutex::new(None);

fn type_name(kind: u32) -> &'static str {
    TYPES.get(kind as usize).copied().unwrap_or("?")
}

/// The local net object with `id`, if there is one.
fn local_object(id: u16) -> Option<usize> {
    let find: extern "thiscall" fn(usize, u32, u32) -> usize = unsafe { std::mem::transmute(memory::from_ghidra(OBJECT_BY_ID)) };
    let object = find(memory::from_ghidra(OBJECT_MGR), id as u32, 1);
    (object != 0).then_some(object)
}

fn describe_local(id: u16) -> String {
    let Some(object) = local_object(id) else { return "not here".into() };
    unsafe {
        let kind = std::ptr::read_unaligned((object + 8) as *const u32);
        let clone = *((object + 0xE) as *const u8);
        let owner = *((object + 0xF) as *const i8);
        let pending = *((object + 0x10) as *const u8);
        format!("here as {} owner {owner} {}{}", type_name(kind), if clone != 0 { "clone" } else { "ours" }, if pending != 0xFF { format!(" pending owner {pending}") } else { String::new() })
    }
}

extern "C" fn record(result: u32) {
    let (sender, kind, id) = (SENDER.load(Ordering::Relaxed), KIND.load(Ordering::Relaxed), OBJECT.load(Ordering::Relaxed) as u16);
    let mut guard = STATS.lock().unwrap();
    let stats = guard.get_or_insert_with(|| Stats {
        started: Instant::now(),
        last_summary: Instant::now(),
        results: HashMap::new(),
        failures: HashMap::new(),
    });
    *stats.results.entry((kind, result)).or_default() += 1;
    if result != 5 {
        let seen = stats.failures.entry((id, result)).or_default();
        *seen += 1;
        if *seen <= 3 || seen.is_power_of_two() {
            log!(
                "sync in: {} {id} from {sender:#x} -> result {result} (seen {seen}); {} ({:.1}s)",
                type_name(kind),
                describe_local(id),
                stats.started.elapsed().as_secs_f32()
            );
        }
    }
    if stats.last_summary.elapsed().as_secs() >= 30 {
        stats.last_summary = Instant::now();
        let mut rows: Vec<_> = stats.results.drain().collect();
        rows.sort();
        let text: Vec<String> = rows.iter().map(|((kind, result), n)| format!("{} r{result} x{n}", type_name(*kind))).collect();
        log!("sync in: last 30 s: {}", text.join(", "));
    }
}

/// Parks the return address, keeps the sender, type and object id (the first three stack
/// arguments), calls the original with the caller's arguments in place (it pops them), then
/// records the result. Game thread only.
#[unsafe(naked)]
extern "C" fn apply_thunk() {
    core::arch::naked_asm!(
        "pop dword ptr [{ret}]",
        "mov eax, [esp]",
        "mov [{sender}], eax",
        "mov eax, [esp + 4]",
        "mov [{kind}], eax",
        "mov eax, [esp + 8]",
        "mov [{object}], eax",
        "call dword ptr [{orig}]",
        "push dword ptr [{ret}]",
        "pushad",
        "pushfd",
        "push eax",
        "call {record}",
        "add esp, 4",
        "popfd",
        "popad",
        "ret",
        ret = sym RETURN,
        sender = sym SENDER,
        kind = sym KIND,
        object = sym OBJECT,
        orig = sym ORIGINAL,
        record = sym record,
    );
}

pub fn install() {
    let site = memory::from_ghidra(APPLY_CALL);
    if unsafe { *(site as *const u8) } != 0xE8 {
        log!("sync trace: no call at {APPLY_CALL:#x}");
        return;
    }
    match unsafe { memory::redirect_call(site, apply_thunk as usize) } {
        Ok(original) => {
            ORIGINAL.store(original, Ordering::SeqCst);
            log!("sync trace: logging clone sync results");
        }
        Err(e) => log!("sync trace: hook failed: {e}"),
    }
}

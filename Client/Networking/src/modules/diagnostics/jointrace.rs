//! Diagnostics: logs what each check inside JOIN_GAME's worker (0x47D470) returns, so we can see
//! which one refuses the join. Each traced call site is pointed at a thunk that calls the
//! original and records AL.

use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

use crate::{log, memory};

/// How many results to log per call site.
const LOG_LIMIT: u32 = 40;

struct Site {
    address: usize,
    name: &'static str,
}

const SITES: [Site; 20] = [
    Site { address: 0x0047_D5FB, name: "signed-in handle (1)" },
    Site { address: 0x0047_D62F, name: "gamer online" },
    Site { address: 0x0047_D654, name: "signed-in handle (2)" },
    Site { address: 0x0047_D682, name: "session active" },
    Site { address: 0x0047_D698, name: "init sessions" },
    Site { address: 0x0047_D6EA, name: "join request built" },
    Site { address: 0x0047_D71A, name: "join session" },
    // snJoinSessionTask step 2 (0x662BF0): build the join request, then send it to the host.
    Site { address: 0x0066_2DDC, name: "join task: request serialised" },
    Site { address: 0x0066_2E04, name: "join task: request sent (connection)" },
    // snMsgAddGamerToSessionCmd handling (0x6245C0), when the IVMP server introduces players.
    Site { address: 0x0062_47A0, name: "add gamer: parsed (622B90)" },
    Site { address: 0x0062_47F0, name: "add gamer: join task" },
    Site { address: 0x0062_482C, name: "add gamer: gamers added" },
    Site { address: 0x0062_48C8, name: "add gamer: task started" },
    // The gamer block reader (0x626E70 -> 0x627640), field by field.
    Site { address: 0x0062_7647, name: "gamer block: session id" },
    Site { address: 0x0062_7655, name: "gamer block: gamer info (0x628030)" },
    Site { address: 0x0062_7663, name: "gamer block: peer address (0x628110)" },
    Site { address: 0x0062_7676, name: "gamer block: 0x48AC50" },
    Site { address: 0x0062_768A, name: "gamer block: data length" },
    Site { address: 0x0062_6E8D, name: "gamer block: count" },
    Site { address: 0x0062_6EB4, name: "gamer block: per gamer (0x4E0800)" },
];

static COUNTS: [AtomicU32; 20] = [const { AtomicU32::new(0) }; 20];

extern "C" fn record(site: u32, eax: u32) {
    let index = site as usize;
    if COUNTS[index].fetch_add(1, Ordering::Relaxed) < LOG_LIMIT {
        log!("join trace: {} -> {} (eax {eax:#x})", SITES[index].name, eax & 0xFF);
    }
}

macro_rules! thunks {
    ($($index:literal => $thunk:ident, $original:ident, $return:ident;)*) => {
        $(
            static $original: AtomicUsize = AtomicUsize::new(0);
            static $return: AtomicUsize = AtomicUsize::new(0);

            /// Calls the original with the caller's stack arguments in place (our return address
            /// is parked), then records AL. Game thread only.
            #[unsafe(naked)]
            extern "C" fn $thunk() {
                core::arch::naked_asm!(
                    "pop dword ptr [{ret}]",
                    "call dword ptr [{orig}]",
                    "push dword ptr [{ret}]",
                    "pushad",
                    "pushfd",
                    "push eax",
                    "push {index}",
                    "call {record}",
                    "add esp, 8",
                    "popfd",
                    "popad",
                    "ret",
                    ret = sym $return,
                    orig = sym $original,
                    index = const $index,
                    record = sym record,
                );
            }
        )*

        fn thunk_for(index: usize) -> (usize, &'static AtomicUsize) {
            match index {
                $($index => ($thunk as usize, &$original),)*
                _ => unreachable!(),
            }
        }
    };
}

thunks! {
    0 => thunk0, ORIGINAL0, RETURN0;
    1 => thunk1, ORIGINAL1, RETURN1;
    2 => thunk2, ORIGINAL2, RETURN2;
    3 => thunk3, ORIGINAL3, RETURN3;
    4 => thunk4, ORIGINAL4, RETURN4;
    5 => thunk5, ORIGINAL5, RETURN5;
    6 => thunk6, ORIGINAL6, RETURN6;
    7 => thunk7, ORIGINAL7, RETURN7;
    8 => thunk8, ORIGINAL8, RETURN8;
    9 => thunk9, ORIGINAL9, RETURN9;
    10 => thunk10, ORIGINAL10, RETURN10;
    11 => thunk11, ORIGINAL11, RETURN11;
    12 => thunk12, ORIGINAL12, RETURN12;
    13 => thunk13, ORIGINAL13, RETURN13;
    14 => thunk14, ORIGINAL14, RETURN14;
    15 => thunk15, ORIGINAL15, RETURN15;
    16 => thunk16, ORIGINAL16, RETURN16;
    17 => thunk17, ORIGINAL17, RETURN17;
    18 => thunk18, ORIGINAL18, RETURN18;
    19 => thunk19, ORIGINAL19, RETURN19;
}

pub fn install() {
    for (index, site) in SITES.iter().enumerate() {
        let address = memory::from_ghidra(site.address);
        if unsafe { *(address as *const u8) } != 0xE8 {
            log!("join trace: no call at {:#x} ({})", site.address, site.name);
            continue;
        }
        let (thunk, original) = thunk_for(index);
        match unsafe { memory::redirect_call(address, thunk) } {
            Ok(old) => original.store(old, Ordering::SeqCst),
            Err(e) => log!("join trace: hook at {:#x} failed: {e}", site.address),
        }
    }
    log!("join trace: installed");
}

// --- Clone create results ----------------------------------------------------------------------
// The handler for a received clone create (0x498430) answers with an ack carrying a result code
// through `ackSender->Set(objectId, result)` (thiscall 0x4EDF70, called at 0x498854). 0 means
// the clone was created; anything else is a refusal (1 = not allowed / failed, 2 = bad data,
// 4 = over the object budget, ...). Log the codes.

const CLONE_CREATE_ACK_CALL: usize = 0x0049_8854;

static ORIGINAL_ACK: AtomicUsize = AtomicUsize::new(0);
static ACK_RESULTS: [AtomicU32; 8] = [const { AtomicU32::new(0) }; 8];

/// Per object: (refusals, last successful create). A create for an object we created shortly
/// before means it went away in between (the "despawns and respawns" seen in game).
static CREATES: std::sync::Mutex<Option<std::collections::HashMap<u16, (u32, Option<std::time::Instant>)>>> =
    std::sync::Mutex::new(None);

extern "C" fn record_ack(args: *const u32) {
    let (object_id, result) = unsafe { (*args as u16, *args.add(1)) };
    let slot = (result as usize).min(ACK_RESULTS.len() - 1);
    let seen = ACK_RESULTS[slot].fetch_add(1, Ordering::Relaxed);
    if seen < 5 || seen.is_power_of_two() {
        log!(
            "clone create: result {result} seen {} times, thread {}, thread flags {:#x}",
            seen + 1,
            unsafe { windows::Win32::System::Threading::GetCurrentThreadId() },
            game_thread_flags()
        );
    }
    let mut guard = CREATES.lock().unwrap();
    let (refused, last) = guard.get_or_insert_with(Default::default).entry(object_id).or_default();
    if result != 0 {
        *refused += 1;
        if *refused <= 3 || refused.is_power_of_two() {
            log!("clone create: object {object_id} refused with result {result} ({refused} times)");
        }
    } else {
        if let Some(previous) = last.filter(|t| t.elapsed().as_secs() < 60) {
            log!("clone create: object {object_id} created again {:.1}s after the last time", previous.elapsed().as_secs_f32());
        }
        *last = Some(std::time::Instant::now());
    }
}

#[unsafe(naked)]
extern "C" fn ack_thunk() {
    core::arch::naked_asm!(
        "pushad",
        "pushfd",
        "lea eax, [esp + 40]",
        "push eax",
        "call {record}",
        "add esp, 4",
        "popfd",
        "popad",
        "jmp dword ptr [{orig}]",
        record = sym record_ack,
        orig = sym ORIGINAL_ACK,
    );
}

pub fn install_clone_trace() {
    let site = memory::from_ghidra(CLONE_CREATE_ACK_CALL);
    if unsafe { *(site as *const u8) } != 0xE8 {
        log!("clone trace: no call at {CLONE_CREATE_ACK_CALL:#x}");
        return;
    }
    let rel = unsafe { std::ptr::read_unaligned((site + 1) as *const i32) };
    ORIGINAL_ACK.store((site as isize + 5 + rel as isize) as usize, Ordering::SeqCst);
    match unsafe { memory::redirect_call(site, ack_thunk as usize) } {
        Ok(_) => log!("clone trace: logging clone create results"),
        Err(e) => log!("clone trace: hook failed: {e}"),
    }
}

/// The game's per-thread flags (TLS slot at 0x17ABA14, +0x8D0). The clone create handler
/// refuses with result 1 unless bit 1 is set; the main thread sets 2 and 4 at start (0x966C30).
fn game_thread_flags() -> u32 {
    unsafe {
        let tls: *const usize;
        core::arch::asm!("mov {}, fs:[0x2c]", out(reg) tls);
        let index = *(memory::from_ghidra(0x017A_BA14) as *const u32) as usize;
        let data = *tls.add(index);
        if data == 0 { 0 } else { *((data + 0x8D0) as *const u32) }
    }
}

// --- AddGamer validation (0x623D80) ------------------------------------------------------------
// thiscall(session, received message, parsed message): session state (+0x50) must be 2 or 3, the
// message's connection (+0x18) must be the host's (looked up by the host peer id at +0xC30 with
// 0x622EA0), the message must parse, and its session id must match +0x540/+0x544. Logs the values.

const ADD_GAMER_VALID_CALL: usize = 0x0062_47B0;
const FIND_PEER: usize = 0x0062_2EA0;
const ADD_GAMER_PARSE: usize = 0x0061_D300;

static ORIGINAL_VALID: AtomicUsize = AtomicUsize::new(0);
static VALID_LOGS: AtomicU32 = AtomicU32::new(0);

extern "C" fn record_valid(session: usize, message: usize) {
    if VALID_LOGS.fetch_add(1, Ordering::Relaxed) >= 8 {
        return;
    }
    unsafe {
        let read = |a: usize| *(a as *const u32);
        let find: extern "thiscall" fn(usize, u32, u32) -> usize = std::mem::transmute(memory::from_ghidra(FIND_PEER));
        let host = find(session, read(session + 0xC30), read(session + 0xC34));
        let host_cxn = if host == 0 { u32::MAX } else { read(host) };
        // Parse it ourselves into scratch space (0x61D300: thiscall(out, data, length, 0)).
        let mut scratch = vec![0u32; 0x1000];
        let parse: extern "thiscall" fn(*mut u32, u32, u32, u32) -> u32 =
            std::mem::transmute(memory::from_ghidra(ADD_GAMER_PARSE));
        let parsed = parse(scratch.as_mut_ptr(), read(message + 0x1C), read(message + 0x20), 0) & 0xFF;
        let data = std::slice::from_raw_parts(read(message + 0x1C) as *const u8, read(message + 0x20).min(160) as usize);
        log!(
            "add gamer check: parse -> {parsed}, parsed session id {:#x}:{:#x}, bytes {:02x?}",
            scratch[0],
            scratch[1],
            data
        );
        log!(
            "add gamer check: state {}, message cxn {:#x}, host peer {:#x}:{:#x} cxn {:#x}, session id {:#x}:{:#x}, message +0x1c {:#x} +0x20 {:#x}",
            read(session + 0x50),
            read(message + 0x18),
            read(session + 0xC30),
            read(session + 0xC34),
            host_cxn,
            read(session + 0x540),
            read(session + 0x544),
            read(message + 0x1C),
            read(message + 0x20)
        );
    }
}

/// Entered by `call` with ECX = session and the two arguments on the stack.
#[unsafe(naked)]
extern "C" fn valid_thunk() {
    core::arch::naked_asm!(
        "pushad",
        "pushfd",
        "push dword ptr [esp + 40]",
        "push ecx",
        "call {record}",
        "add esp, 8",
        "popfd",
        "popad",
        "jmp dword ptr [{orig}]",
        record = sym record_valid,
        orig = sym ORIGINAL_VALID,
    );
}

pub fn install_add_gamer_trace() {
    let site = memory::from_ghidra(ADD_GAMER_VALID_CALL);
    if unsafe { *(site as *const u8) } != 0xE8 {
        return log!("add gamer trace: no call at {ADD_GAMER_VALID_CALL:#x}");
    }
    let rel = unsafe { std::ptr::read_unaligned((site + 1) as *const i32) };
    ORIGINAL_VALID.store((site as isize + 5 + rel as isize) as usize, Ordering::SeqCst);
    match unsafe { memory::redirect_call(site, valid_thunk as usize) } {
        Ok(_) => log!("add gamer trace: installed"),
        Err(e) => log!("add gamer trace: hook failed: {e}"),
    }
}

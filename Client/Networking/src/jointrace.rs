//! Diagnostics: logs what each check inside JOIN_GAME's worker (0x47D470) returns, so we can see
//! which one refuses the join. Each traced call site is pointed at a thunk that calls the
//! original and records AL.

use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

use crate::{log, memory};

/// How many results to log per call site.
const LOG_LIMIT: u32 = 6;

struct Site {
    address: usize,
    name: &'static str,
}

const SITES: [Site; 9] = [
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
];

static COUNTS: [AtomicU32; 9] = [const { AtomicU32::new(0) }; 9];

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

extern "C" fn record_ack(args: *const u32) {
    let (object_id, result) = unsafe { (*args as u16, *args.add(1)) };
    let slot = (result as usize).min(ACK_RESULTS.len() - 1);
    let seen = ACK_RESULTS[slot].fetch_add(1, Ordering::Relaxed);
    if seen < 5 || seen.is_power_of_two() {
        log!(
            "clone create: object {object_id} -> result {result} (seen {} times), thread {}, thread flags {:#x}",
            seen + 1,
            unsafe { windows::Win32::System::Threading::GetCurrentThreadId() },
            game_thread_flags()
        );
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

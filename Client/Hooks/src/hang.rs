//! Freeze detector: if the game thread stops reaching CGame_Process, log where it is stuck
//! (instruction pointer, registers and the game return addresses on its stack).

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use windows::Win32::Foundation::CloseHandle;
use windows::Win32::System::Diagnostics::Debug::{CONTEXT, CONTEXT_FLAGS, GetThreadContext, ReadProcessMemory};
use windows::Win32::System::Threading::{
    GetCurrentProcess, GetCurrentThreadId, OpenThread, ResumeThread, SuspendThread, THREAD_GET_CONTEXT,
    THREAD_QUERY_INFORMATION, THREAD_SUSPEND_RESUME,
};

use crate::{log, memory};

/// The game thread is considered frozen after this long without a frame.
const FROZEN_AFTER: Duration = Duration::from_secs(8);
/// Reports per freeze (one every FROZEN_AFTER), to see whether it moves.
const REPORTS: u32 = 3;
/// CONTEXT_i386 | CONTROL | INTEGER.
const CONTEXT_CONTROL_INTEGER: u32 = 0x0001_0000 | 0x1 | 0x2;

static GAME_THREAD: AtomicU32 = AtomicU32::new(0);
static LAST_FRAME_MS: AtomicU64 = AtomicU64::new(0);
static STARTED: AtomicBool = AtomicBool::new(false);

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// Call once per game frame from the game thread. Starts the watchdog on the first call.
pub fn heartbeat() {
    LAST_FRAME_MS.store(now_ms(), Ordering::Relaxed);
    if !STARTED.swap(true, Ordering::Relaxed) {
        GAME_THREAD.store(unsafe { GetCurrentThreadId() }, Ordering::Relaxed);
        std::thread::spawn(watchdog);
    }
}

fn watchdog() {
    let mut reported = 0;
    loop {
        std::thread::sleep(FROZEN_AFTER);
        let stalled = now_ms().saturating_sub(LAST_FRAME_MS.load(Ordering::Relaxed));
        if stalled < FROZEN_AFTER.as_millis() as u64 {
            reported = 0;
            continue;
        }
        if reported < REPORTS {
            reported += 1;
            log!("hang: game thread has not run a frame for {} s", stalled / 1000);
            report(GAME_THREAD.load(Ordering::Relaxed));
        }
    }
}

fn report(thread_id: u32) {
    unsafe {
        let Ok(thread) = OpenThread(THREAD_SUSPEND_RESUME | THREAD_GET_CONTEXT | THREAD_QUERY_INFORMATION, false, thread_id)
        else {
            log!("hang: cannot open the game thread");
            return;
        };
        if SuspendThread(thread) == u32::MAX {
            log!("hang: cannot suspend the game thread");
            let _ = CloseHandle(thread);
            return;
        }
        let mut context = CONTEXT { ContextFlags: CONTEXT_FLAGS(CONTEXT_CONTROL_INTEGER), ..Default::default() };
        let got = GetThreadContext(thread, &mut context).is_ok();
        let mut stack = [0u32; 512];
        let mut read = 0usize;
        if got {
            let _ = ReadProcessMemory(
                GetCurrentProcess(),
                context.Esp as *const _,
                stack.as_mut_ptr() as *mut _,
                size_of_val(&stack),
                Some(&mut read),
            );
        }
        ResumeThread(thread);
        let _ = CloseHandle(thread);
        if !got {
            log!("hang: cannot read the game thread's registers");
            return;
        }
        log!("hang:   eip {}", crate::crash::describe(context.Eip as usize));
        log!(
            "hang:   eax={:08x} ebx={:08x} ecx={:08x} edx={:08x} esi={:08x} edi={:08x} ebp={:08x} esp={:08x}",
            context.Eax, context.Ebx, context.Ecx, context.Edx, context.Esi, context.Edi, context.Ebp, context.Esp
        );
        let code = memory::code_section();
        let (start, end) = (code.as_ptr() as usize, code.as_ptr() as usize + code.len());
        let mut found = 0;
        for (slot, value) in stack[..read / 4].iter().enumerate() {
            let value = *value as usize;
            if (start..end).contains(&value) {
                log!("hang:   [esp+{:#05x}] {}", slot * 4, crate::crash::describe(value));
                found += 1;
                if found == 24 {
                    break;
                }
            }
        }
    }
}

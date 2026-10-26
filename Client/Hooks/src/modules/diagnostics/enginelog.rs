//! The engine's own diagnostics. Rockstar's network code still calls its printf-style logger
//! (`void log(channel, level, const char* fmt, ...)`, e.g. "\tRECEIVED_EVENT\t%s_%d_%d\r\n"),
//! but in the retail build that function is an empty `ret` at 0x4016A0. We put our logger
//! there and write the lines to `IVMP_Engine.log` (`IVMP_Engine_cl2.log`).
//!
//! That `ret` is also shared (identical code folding) by unrelated empty virtual methods, so
//! every call is checked: the format string must be readable text inside the game image, and
//! `%s` arguments are only read when their memory is readable.

use std::fs::File;
use std::io::Write;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};

use crate::{log, memory};

const EMPTY_LOGGER: usize = 0x0040_16A0;
/// Stop writing after this many lines so a chatty session can't fill the disk.
const MAX_LINES: u32 = 200_000;

static FILE: Mutex<Option<File>> = Mutex::new(None);
static LINES: AtomicU32 = AtomicU32::new(0);

pub fn install(path: &std::path::Path) {
    let site = memory::from_ghidra(EMPTY_LOGGER);
    if unsafe { *(site as *const u8) } != 0xC3 {
        log!("engine log: {site:#x} is not the empty logger, skipped");
        return;
    }
    *FILE.lock().unwrap() = File::create(path).ok();
    let rel = (thunk as usize as isize - (site as isize + 5)) as i32;
    let mut jump = [0xE9u8, 0, 0, 0, 0];
    jump[1..].copy_from_slice(&rel.to_le_bytes());
    match unsafe { memory::patch(site, &jump) } {
        Ok(()) => log!("engine log: writing the game's own diagnostics to {}", path.display()),
        Err(e) => log!("engine log: patch failed: {e}"),
    }
}

/// Saves everything, hands `record` a pointer to the caller's arguments, returns like `ret`.
#[unsafe(naked)]
extern "C" fn thunk() {
    core::arch::naked_asm!(
        "pushad",
        "pushfd",
        // 32 bytes of registers + 4 of flags + the return address.
        "lea eax, [esp + 40]",
        "push eax",
        "call {record}",
        "add esp, 4",
        "popfd",
        "popad",
        "ret",
        record = sym record,
    );
}

/// Reads a NUL-terminated string of printable text, if the memory is readable.
fn read_text(address: usize, max: usize, require_image: bool) -> Option<String> {
    if address < 0x10000 {
        return None;
    }
    if require_image {
        let image = memory::game_image();
        let start = image.as_ptr() as usize;
        if address < start || address >= start + image.len() {
            return None;
        }
    }
    let mut out = Vec::new();
    let mut chunk = [0u8; 64];
    while out.len() < max {
        if !memory::read(address + out.len(), &mut chunk) {
            return None;
        }
        for &b in &chunk {
            if b == 0 {
                return Some(String::from_utf8_lossy(&out).into_owned());
            }
            if !(b == b'\t' || b == b'\r' || b == b'\n' || (0x20..0x7F).contains(&b)) {
                return None;
            }
            out.push(b);
        }
    }
    None
}

/// A small printf: %d %i %u %x %X %c %s %p %f %g %e %%, flags/width/precision, h/l/ll/I64.
fn format(fmt: &str, mut args: *const u32) -> String {
    let mut out = String::new();
    let mut chars = fmt.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        let mut spec = String::new();
        while let Some(&n) = chars.peek() {
            if "-+ #0123456789.".contains(n) {
                spec.push(n);
                chars.next();
            } else {
                break;
            }
        }
        let mut wide = false;
        loop {
            match chars.peek() {
                Some('l') | Some('h') => {
                    if chars.next() == Some('l') && chars.peek() == Some(&'l') {
                        chars.next();
                        wide = true;
                    }
                }
                Some('I') => {
                    chars.next();
                    if chars.peek() == Some(&'6') {
                        chars.next();
                        chars.next();
                        wide = true;
                    }
                }
                _ => break,
            }
        }
        let Some(conv) = chars.next() else { break };
        unsafe {
            let take32 = |args: &mut *const u32| {
                let v = **args;
                *args = args.add(1);
                v
            };
            match conv {
                '%' => out.push('%'),
                'd' | 'i' if wide => {
                    let lo = take32(&mut args) as u64;
                    let hi = take32(&mut args) as u64;
                    out.push_str(&((hi << 32 | lo) as i64).to_string());
                }
                'u' | 'x' | 'X' if wide => {
                    let lo = take32(&mut args) as u64;
                    let hi = take32(&mut args) as u64;
                    let v = hi << 32 | lo;
                    out.push_str(&if conv == 'u' { v.to_string() } else { format!("{v:x}") });
                }
                'd' | 'i' => out.push_str(&(take32(&mut args) as i32).to_string()),
                'u' => out.push_str(&take32(&mut args).to_string()),
                'x' => out.push_str(&format!("{:x}", take32(&mut args))),
                'X' => out.push_str(&format!("{:X}", take32(&mut args))),
                'p' => out.push_str(&format!("{:#010x}", take32(&mut args))),
                'c' => out.push((take32(&mut args) as u8) as char),
                's' => {
                    let p = take32(&mut args) as usize;
                    out.push_str(&read_text(p, 256, false).unwrap_or_else(|| format!("<{p:#x}>")));
                }
                'f' | 'g' | 'e' | 'F' | 'G' | 'E' => {
                    let lo = take32(&mut args) as u64;
                    let hi = take32(&mut args) as u64;
                    out.push_str(&format!("{:.3}", f64::from_bits(hi << 32 | lo)));
                }
                other => {
                    out.push('%');
                    out.push(other);
                }
            }
        }
    }
    out
}

extern "C" fn record(args: *const u32) {
    if LINES.load(Ordering::Relaxed) >= MAX_LINES {
        return;
    }
    unsafe {
        let fmt_address = *args.add(2) as usize;
        // Only real log calls: a format string of text inside the game image.
        let Some(fmt) = read_text(fmt_address, 512, true) else { return };
        if fmt.len() < 3 {
            return;
        }
        let line = format(&fmt, args.add(3));
        let line = line.trim_end_matches(['\r', '\n']);
        if let Some(file) = FILE.lock().unwrap().as_mut() {
            let _ = writeln!(file, "{line}");
            LINES.fetch_add(1, Ordering::Relaxed);
        }
    }
}

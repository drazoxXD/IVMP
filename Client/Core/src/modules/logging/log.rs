//! Minimal file logger. The hooks DLL opens `IVMP_Client.log` (or `IVMP_Client_cl2.log`) next to itself.

use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

static FILE: Mutex<Option<File>> = Mutex::new(None);

/// The last lines written, for the in-game console: (number of the next line, lines).
static RECENT: Mutex<(u64, VecDeque<String>)> = Mutex::new((0, VecDeque::new()));
const RECENT_LINES: usize = 500;

/// The lines written since `after` (the number returned by the previous call, 0 at first), and
/// the number to pass next time.
pub fn tail(after: u64) -> (u64, Vec<String>) {
    let recent = RECENT.lock().unwrap();
    let (next, lines) = &*recent;
    let first = next - lines.len() as u64;
    let skip = after.saturating_sub(first) as usize;
    (*next, lines.iter().skip(skip).cloned().collect())
}

pub fn init(path: &Path) {
    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)
        .ok();
    *FILE.lock().unwrap() = file;
}

pub fn write(args: std::fmt::Arguments) {
    let millis = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
    if let Some(file) = FILE.lock().unwrap().as_mut() {
        let _ = writeln!(file, "[{millis}] {args}");
        let _ = file.flush();
    }
    let mut recent = RECENT.lock().unwrap();
    if recent.1.len() == RECENT_LINES {
        recent.1.pop_front();
    }
    recent.1.push_back(args.to_string());
    recent.0 += 1;
}

#[macro_export]
macro_rules! log {
    ($($arg:tt)*) => {
        $crate::log::write(format_args!($($arg)*))
    };
}

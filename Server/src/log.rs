//! Console output that is also written to `ivmp_server.log` next to the executable, so what the
//! console showed can be read later.

use std::fs::File;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;

pub static FILE: Mutex<Option<File>> = Mutex::new(None);

/// `verbose` in server.cfg (or `--verbose`): also log the debug lines (entities, game events).
pub static VERBOSE: AtomicBool = AtomicBool::new(false);

pub fn verbose() -> bool {
    VERBOSE.load(std::sync::atomic::Ordering::Relaxed)
}

pub fn open() {
    let path = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("ivmp_server.log")))
        .unwrap_or_else(|| "ivmp_server.log".into());
    *FILE.lock().unwrap() = File::create(path).ok();
}

/// Like `say!`, but only with `verbose = 1`: debugging detail (entities, game events, summaries).
macro_rules! debug {
    ($($arg:tt)*) => {{
        if $crate::log::verbose() {
            say!($($arg)*);
        }
    }};
}

/// Prints a line to the console and the log file.
macro_rules! say {
    ($($arg:tt)*) => {{
        let line = format!($($arg)*);
        println!("{line}");
        if let Some(file) = $crate::log::FILE.lock().unwrap().as_mut() {
            let _ = std::io::Write::write_all(file, format!("{line}\n").as_bytes());
        }
    }};
}

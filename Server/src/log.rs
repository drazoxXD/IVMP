//! Console output that is also written to `ivmp_server.log` next to the executable, so what the
//! console showed can be read later.

use std::fs::File;
use std::sync::Mutex;

pub static FILE: Mutex<Option<File>> = Mutex::new(None);

pub fn open() {
    let path = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("ivmp_server.log")))
        .unwrap_or_else(|| "ivmp_server.log".into());
    *FILE.lock().unwrap() = File::create(path).ok();
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

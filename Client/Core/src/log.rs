//! Minimal file logger. The hooks DLL opens `IVMP_Client.log` (or `IVMP_Client_cl2.log`) next to itself.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

static FILE: Mutex<Option<File>> = Mutex::new(None);

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
}

#[macro_export]
macro_rules! log {
    ($($arg:tt)*) => {
        $crate::log::write(format_args!($($arg)*))
    };
}

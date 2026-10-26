//! `--capture`: every tunnelled game packet as `u64 milliseconds since start, u8 from id, u8 to
//! id (0 = broadcast), u16 length, payload`, little endian (docs/onesync-plan.md;
//! `cargo run -p Sync --example decode_capture -- file`).

use std::fs::File;
use std::io::{BufWriter, Write};
use std::time::Instant;

use NetProtocol::ClientId;

pub struct Capture {
    file: BufWriter<File>,
    started: Instant,
}

impl Capture {
    pub fn create(path: &str) -> std::io::Result<Self> {
        Ok(Capture { file: BufWriter::new(File::create(path)?), started: Instant::now() })
    }

    pub fn record(&mut self, from: ClientId, to: ClientId, payload: &[u8]) {
        let mut record = Vec::with_capacity(12 + payload.len());
        record.extend_from_slice(&(self.started.elapsed().as_millis() as u64).to_le_bytes());
        record.extend_from_slice(&[from, to]);
        record.extend_from_slice(&(payload.len() as u16).to_le_bytes());
        record.extend_from_slice(payload);
        let _ = self.file.write_all(&record);
        let _ = self.file.flush();
    }
}

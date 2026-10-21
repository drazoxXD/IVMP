//! Bit writer matching rage's bit buffers: most significant bit first (the reader is
//! `ivmp_sync::bits::BitReader`).

#[derive(Default, Clone)]
pub struct BitWriter {
    bytes: Vec<u8>,
    bits: usize,
}

impl BitWriter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Writes the low `bits` bits of `value`.
    pub fn write(&mut self, value: u32, bits: usize) -> &mut Self {
        for i in (0..bits).rev() {
            if self.bits % 8 == 0 {
                self.bytes.push(0);
            }
            if (value >> i) & 1 != 0 {
                let last = self.bytes.len() - 1;
                self.bytes[last] |= 0x80 >> (self.bits % 8);
            }
            self.bits += 1;
        }
        self
    }

    pub fn bytes(&mut self, data: &[u8]) -> &mut Self {
        for byte in data {
            self.write(*byte as u32, 8);
        }
        self
    }

    pub fn len_bits(&self) -> usize {
        self.bits
    }

    /// The bytes written; the last one is zero-padded.
    pub fn finish(self) -> Vec<u8> {
        self.bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_msb_first() {
        let mut w = BitWriter::new();
        w.write(0x3246, 14).write(0b01, 2);
        assert_eq!(w.finish(), [0b1100_1001, 0b0001_1001]);
    }
}

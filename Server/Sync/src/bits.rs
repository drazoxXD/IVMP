//! Bit reader matching rage's bit buffers: most significant bit first.

#[derive(Clone)]
pub struct BitReader<'a> {
    data: &'a [u8],
    pos: usize,
    end: usize,
}

impl<'a> BitReader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0, end: data.len() * 8 }
    }

    /// A reader over the next `bits` bits (which this reader then skips).
    pub fn sub(&mut self, bits: usize) -> Option<BitReader<'a>> {
        if self.pos + bits > self.end {
            return None;
        }
        let sub = BitReader { data: self.data, pos: self.pos, end: self.pos + bits };
        self.pos += bits;
        Some(sub)
    }

    /// Limits the reader to the first `bits` bits.
    pub fn truncate(&mut self, bits: usize) {
        self.end = self.end.min(bits);
    }

    pub fn seek(&mut self, pos: usize) {
        self.pos = pos.min(self.end);
    }

    /// Bit position from the start of the data.
    pub fn position(&self) -> usize {
        self.pos
    }

    pub fn remaining(&self) -> usize {
        self.end - self.pos
    }

    pub fn skip(&mut self, bits: usize) -> Option<()> {
        (self.pos + bits <= self.end).then(|| self.pos += bits)
    }

    pub fn read(&mut self, bits: usize) -> Option<u32> {
        if bits > 32 || self.pos + bits > self.end {
            return None;
        }
        let mut value = 0u32;
        for i in 0..bits {
            let bit = self.pos + i;
            value = (value << 1) | ((self.data[bit >> 3] >> (7 - (bit & 7))) & 1) as u32;
        }
        self.pos += bits;
        Some(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_msb_first() {
        let mut r = BitReader::new(&[0b1100_1001, 0b0001_1001]);
        assert_eq!(r.read(14), Some(0x3246));
        assert_eq!(r.read(2), Some(0b01));
        assert_eq!(r.read(1), None);
    }
}

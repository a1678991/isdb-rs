//! MSB-first bit reader/writer matching the `uimsbf`/`bslbf` mnemonics of
//! H.222.0 §2.2.6 and ARIB STD-B10 §5.1 ("most significant bit first").

use crate::error::{Error, Result};

/// Reads big-endian bit fields from a byte slice.
#[derive(Debug, Clone)]
pub struct BitReader<'a> {
    data: &'a [u8],
    bit: usize,
    what: &'static str,
}

impl<'a> BitReader<'a> {
    /// New reader; `what` labels errors with the syntax element being read.
    pub fn new(data: &'a [u8], what: &'static str) -> Self {
        Self { data, bit: 0, what }
    }

    /// Bits not yet consumed.
    pub fn remaining_bits(&self) -> usize {
        self.data.len() * 8 - self.bit
    }

    /// Whole bytes not yet consumed (reader must be byte aligned).
    pub fn remaining_bytes(&self) -> usize {
        self.remaining_bits() / 8
    }

    /// Current position in bits from the start.
    pub fn position_bits(&self) -> usize {
        self.bit
    }

    /// True when positioned on a byte boundary.
    pub fn is_aligned(&self) -> bool {
        self.bit & 7 == 0
    }

    /// Read an `n`-bit unsigned field, `n <= 64`.
    pub fn read(&mut self, n: u32) -> Result<u64> {
        assert!(n <= 64);
        if (n as usize) > self.remaining_bits() {
            return Err(Error::Truncated {
                what: self.what,
                need: (self.bit + n as usize).div_ceil(8),
                have: self.data.len(),
            });
        }
        let mut v = 0u64;
        for _ in 0..n {
            let byte = self.data[self.bit / 8];
            let b = (byte >> (7 - (self.bit % 8))) & 1;
            v = (v << 1) | b as u64;
            self.bit += 1;
        }
        Ok(v)
    }

    /// Read a 1-bit flag.
    pub fn bool(&mut self) -> Result<bool> {
        Ok(self.read(1)? == 1)
    }

    /// Read an 8-bit field.
    pub fn u8(&mut self) -> Result<u8> {
        Ok(self.read(8)? as u8)
    }

    /// Read a 16-bit field.
    pub fn u16(&mut self) -> Result<u16> {
        Ok(self.read(16)? as u16)
    }

    /// Read a 32-bit field.
    pub fn u32(&mut self) -> Result<u32> {
        Ok(self.read(32)? as u32)
    }

    /// Take `n` whole bytes; the reader must be byte aligned.
    pub fn bytes(&mut self, n: usize) -> Result<&'a [u8]> {
        if !self.is_aligned() {
            return Err(Error::invalid(self.what, "byte read at unaligned position"));
        }
        let start = self.bit / 8;
        if start + n > self.data.len() {
            return Err(Error::Truncated {
                what: self.what,
                need: start + n,
                have: self.data.len(),
            });
        }
        self.bit += n * 8;
        Ok(&self.data[start..start + n])
    }

    /// Take all remaining bytes (byte aligned).
    pub fn rest(&mut self) -> Result<&'a [u8]> {
        let n = self.remaining_bytes();
        self.bytes(n)
    }

    /// Error unless every byte was consumed.
    pub fn finish(&self) -> Result<()> {
        if self.remaining_bits() != 0 {
            return Err(Error::invalid(
                self.what,
                format!(
                    "{} trailing bits not described by the syntax",
                    self.remaining_bits()
                ),
            ));
        }
        Ok(())
    }
}

/// Writes big-endian bit fields into a growable buffer.
#[derive(Debug, Clone, Default)]
pub struct BitWriter {
    buf: Vec<u8>,
    bit: usize,
}

impl BitWriter {
    /// Empty writer.
    pub fn new() -> Self {
        Self::default()
    }

    /// Write the low `n` bits of `v`; errors if `v` needs more than `n` bits.
    pub fn write(&mut self, what: &'static str, n: u32, v: u64) -> Result<()> {
        assert!(n <= 64);
        if n < 64 && v >> n != 0 {
            return Err(Error::Overflow {
                what,
                value: v,
                bits: n,
            });
        }
        for i in (0..n).rev() {
            if self.bit & 7 == 0 {
                self.buf.push(0);
            }
            let b = ((v >> i) & 1) as u8;
            let last = self.buf.len() - 1;
            self.buf[last] |= b << (7 - (self.bit % 8));
            self.bit += 1;
        }
        Ok(())
    }

    /// Write a 1-bit flag.
    pub fn bool(&mut self, v: bool) {
        self.write("flag", 1, v as u64).expect("1-bit flag fits");
    }

    /// Append whole bytes; the writer must be byte aligned.
    pub fn bytes(&mut self, data: &[u8]) {
        assert!(self.bit & 7 == 0, "BitWriter::bytes at unaligned position");
        self.buf.extend_from_slice(data);
        self.bit += data.len() * 8;
    }

    /// Bits written so far.
    pub fn len_bits(&self) -> usize {
        self.bit
    }

    /// Finish; panics unless byte aligned (every ARIB/MPEG syntax is).
    pub fn into_bytes(self) -> Vec<u8> {
        assert!(
            self.bit & 7 == 0,
            "BitWriter finished at unaligned position"
        );
        self.buf
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_fields() {
        let mut w = BitWriter::new();
        w.write("a", 3, 0b101).unwrap();
        w.write("b", 13, 0x1ABC).unwrap();
        w.write("c", 33, 0x1_2345_6789).unwrap();
        w.write("d", 7, 0x55).unwrap();
        let v = w.into_bytes();
        let mut r = BitReader::new(&v, "t");
        assert_eq!(r.read(3).unwrap(), 0b101);
        assert_eq!(r.read(13).unwrap(), 0x1ABC);
        assert_eq!(r.read(33).unwrap(), 0x1_2345_6789);
        assert_eq!(r.read(7).unwrap(), 0x55);
        r.finish().unwrap();
    }

    #[test]
    fn overflow_rejected() {
        let mut w = BitWriter::new();
        assert!(w.write("pid", 13, 0x2000).is_err());
    }
}

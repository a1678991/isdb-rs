//! MSB-first bit reader/writer matching the `uimsbf`/`bslbf` mnemonics of
//! H.222.0 §2.2.6 and ARIB STD-B10 §5.1 ("most significant bit first").

use crate::error::{Error, Result};
use crate::pid::Pid;

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
    ///
    /// # Panics
    ///
    /// If `n > 64`: the width is the decoder's constant from the syntax
    /// table, not input.
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
            v = (v << 1) | u64::from(b);
            self.bit += 1;
        }
        Ok(v)
    }

    /// Read a 1-bit flag.
    pub fn bool(&mut self) -> Result<bool> {
        Ok(self.read(1)? == 1)
    }

    /// Read an `n`-bit field into a `u8`, `n <= 8`.
    ///
    /// The width-typed readers (`read_u8`, [`read_u16`](Self::read_u16),
    /// [`read_u32`](Self::read_u32), [`read_usize`](Self::read_usize)) are
    /// how a decoder takes a field narrower than 64 bits without an `as`
    /// cast: the width is asserted here, once, so the narrowing is proven
    /// where it happens and the field lands in a type that fits it.
    ///
    /// # Panics
    ///
    /// If `n` exceeds the type's width (8 here, 16, 32 and 32 for the
    /// others): the width is the decoder's constant, not input.
    pub fn read_u8(&mut self, n: u32) -> Result<u8> {
        assert!(n <= 8, "read_u8: {n} bits");
        // n <= 8 is asserted above, so the value fits.
        #[allow(clippy::cast_possible_truncation)]
        Ok(self.read(n)? as u8)
    }

    /// Read an `n`-bit field into a `u16`, `n <= 16`.
    ///
    /// # Panics
    ///
    /// If `n > 16` (see [`read_u8`](Self::read_u8)).
    pub fn read_u16(&mut self, n: u32) -> Result<u16> {
        assert!(n <= 16, "read_u16: {n} bits");
        // n <= 16 is asserted above, so the value fits.
        #[allow(clippy::cast_possible_truncation)]
        Ok(self.read(n)? as u16)
    }

    /// Read an `n`-bit field into a `u32`, `n <= 32`.
    ///
    /// # Panics
    ///
    /// If `n > 32` (see [`read_u8`](Self::read_u8)).
    pub fn read_u32(&mut self, n: u32) -> Result<u32> {
        assert!(n <= 32, "read_u32: {n} bits");
        // n <= 32 is asserted above, so the value fits.
        #[allow(clippy::cast_possible_truncation)]
        Ok(self.read(n)? as u32)
    }

    /// Read an `n`-bit field into a `usize`, `n <= 32` (a length or count).
    ///
    /// # Panics
    ///
    /// If `n > 32` (see [`read_u8`](Self::read_u8)).
    pub fn read_usize(&mut self, n: u32) -> Result<usize> {
        assert!(n <= 32, "read_usize: {n} bits");
        // n <= 32 is asserted above and usize is at least 32 bits on every
        // target this workspace builds for, so the value fits.
        #[allow(clippy::cast_possible_truncation)]
        Ok(self.read(n)? as usize)
    }

    /// Read an 8-bit field.
    pub fn u8(&mut self) -> Result<u8> {
        self.read_u8(8)
    }

    /// Read a 16-bit field.
    pub fn u16(&mut self) -> Result<u16> {
        self.read_u16(16)
    }

    /// Read a 32-bit field.
    pub fn u32(&mut self) -> Result<u32> {
        self.read_u32(32)
    }

    /// Read a 13-bit PID field (H.222.0 Table 2-3).
    pub fn pid(&mut self) -> Result<Pid> {
        Ok(Pid::from_wire(self.read_u16(13)?))
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
    ///
    /// # Panics
    ///
    /// If `n > 64`: the width is the encoder's constant from the syntax
    /// table, not input.
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
            self.bit_out((v >> i) & 1 == 1);
        }
        Ok(())
    }

    /// Append one bit.
    fn bit_out(&mut self, b: bool) {
        if self.bit & 7 == 0 {
            self.buf.push(0);
        }
        if let Some(last) = self.buf.last_mut() {
            *last |= u8::from(b) << (7 - (self.bit % 8));
        }
        self.bit += 1;
    }

    /// Write a 1-bit flag.
    pub fn bool(&mut self, v: bool) {
        self.bit_out(v);
    }

    /// Append whole bytes; the writer must be byte aligned.
    ///
    /// # Panics
    ///
    /// If the writer is not byte aligned: every ARIB/MPEG syntax puts its
    /// byte fields on byte boundaries, so this is an encoder bug.
    pub fn bytes(&mut self, data: &[u8]) {
        assert!(self.bit & 7 == 0, "BitWriter::bytes at unaligned position");
        self.buf.extend_from_slice(data);
        self.bit += data.len() * 8;
    }

    /// Bits written so far.
    pub fn len_bits(&self) -> usize {
        self.bit
    }

    /// Finish.
    ///
    /// # Panics
    ///
    /// Unless byte aligned (every ARIB/MPEG syntax is, so this is an
    /// encoder bug).
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

    /// The width-typed readers return exactly the field, and refuse a
    /// width their type cannot hold.
    #[test]
    fn width_typed_readers() {
        let v = [0xAB, 0xCD, 0xEF, 0x12, 0x34, 0x56, 0x78, 0x9A];
        let mut r = BitReader::new(&v, "t");
        assert_eq!(r.read_u8(4).unwrap(), 0xA);
        assert_eq!(r.read_u8(8).unwrap(), 0xBC);
        assert_eq!(r.read_u16(12).unwrap(), 0xDEF);
        assert_eq!(r.read_u32(24).unwrap(), 0x123456);
        assert_eq!(r.read_usize(16).unwrap(), 0x789A);
        r.finish().unwrap();
        assert!(std::panic::catch_unwind(|| BitReader::new(&v, "t").read_u8(9)).is_err());
    }

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

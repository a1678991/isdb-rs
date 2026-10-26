//! Program clock reference and 33-bit marker-coded timestamps.
//!
//! * PCR/OPCR (H.222.0 §2.4.2.2 equations 2-1..2-3, §2.4.3.4 Table 2-6,
//!   §2.4.3.5 equations 2-8..2-10): `program_clock_reference_base` (33 bits,
//!   units of 300/27 MHz = 90 kHz), 6 reserved bits,
//!   `program_clock_reference_extension` (9 bits, units of 1/27 MHz);
//!   `PCR = PCR_base × 300 + PCR_ext`, `PCR_base = (27 MHz·t DIV 300) % 2^33`,
//!   `PCR_ext = (27 MHz·t) % 300`.
//! * [`Timestamp`]: the 5-byte coding shared by PTS/DTS (§2.4.3.6
//!   Table 2-21) and DTS_next_AU (Table 2-6): a 4-bit prefix, then the 33-bit
//!   value split `[32..30] marker [29..15] marker [14..0] marker`.

use crate::bits::{BitReader, BitWriter};
use crate::error::Result;

/// Nominal system_clock_frequency in Hz (H.222.0 §2.4.2.1: "27 000 000 –
/// 810 ≤ system_clock_frequency ≤ 27 000 000 + 810").
pub const SYSTEM_CLOCK_FREQUENCY: u64 = 27_000_000;

/// Modulus of 33-bit fields in units of system_clock_frequency/300
/// (PCR_base, PTS, DTS, ESCR_base: "% 2^33", equations 2-2, 2-11, 2-12).
pub const BASE_MODULUS: u64 = 1 << 33;

/// Modulus of the full 27 MHz PCR value: the base wraps at 2^33 and the
/// extension at 300 (equations 2-1..2-3), so `PCR` ∈ [0, 2^33·300).
pub const PCR_MODULUS: u64 = BASE_MODULUS * 300;

/// A PCR or OPCR field (H.222.0 §2.4.3.4 Table 2-6, 48 bits on the wire).
/// All three parts are kept verbatim, so `extension` may hold the
/// out-of-range values 300..=511 and `reserved` non-'1' bits; see
/// [`crate::packet::check`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Pcr {
    /// program_clock_reference_base, 33 bits (equation 2-2).
    pub base: u64,
    /// The 6 reserved bits between base and extension (spec value '111111',
    /// §2.1.64).
    pub reserved: u8,
    /// program_clock_reference_extension, 9 bits (equation 2-3, 0..300).
    pub extension: u16,
}

impl Pcr {
    /// Encoded size in bytes (33 + 6 + 9 bits).
    pub const LEN: usize = 6;

    /// PCR for a 27 MHz tick count `ticks` (taken modulo [`PCR_MODULUS`]),
    /// split per equations 2-2/2-3, reserved bits '111111'.
    pub fn from_ticks(ticks: u64) -> Self {
        let t = ticks % PCR_MODULUS;
        Self {
            base: t / 300,
            reserved: 0x3F,
            extension: (t % 300) as u16,
        }
    }

    /// `PCR_base × 300 + PCR_ext` (equation 2-1), in 27 MHz ticks.
    pub fn ticks(&self) -> u64 {
        self.base * 300 + u64::from(self.extension)
    }

    /// Seconds represented by [`Pcr::ticks`] at the nominal 27 MHz.
    pub fn seconds(&self) -> f64 {
        self.ticks() as f64 / SYSTEM_CLOCK_FREQUENCY as f64
    }

    /// Decode the 6-byte field.
    pub fn decode(b: &[u8]) -> Result<Self> {
        let mut r = BitReader::new(b, "program_clock_reference");
        let base = r.read(33)?;
        let reserved = r.read(6)? as u8;
        let extension = r.read(9)? as u16;
        Ok(Self {
            base,
            reserved,
            extension,
        })
    }

    /// Append the 6-byte field.
    pub fn write(&self, w: &mut BitWriter) -> Result<()> {
        w.write("program_clock_reference_base", 33, self.base)?;
        w.write("PCR reserved", 6, u64::from(self.reserved))?;
        w.write(
            "program_clock_reference_extension",
            9,
            u64::from(self.extension),
        )
    }

    /// Encode to 6 bytes.
    pub fn encode(&self) -> Result<[u8; 6]> {
        let mut w = BitWriter::new();
        self.write(&mut w)?;
        let v = w.into_bytes();
        let mut out = [0u8; 6];
        out.copy_from_slice(&v);
        Ok(out)
    }
}

/// H.222.0 eq. 2-1..2-3 wrap: `(later − earlier) mod PCR_MODULUS`: elapsed 27 MHz ticks from `earlier`
/// to `later` assuming at most one wrap of the 2^33·300 PCR range.
pub fn pcr_wrap_diff(later: u64, earlier: u64) -> u64 {
    (later % PCR_MODULUS + PCR_MODULUS - earlier % PCR_MODULUS) % PCR_MODULUS
}

/// `(t + d) mod PCR_MODULUS` (H.222.0 eq. 2-1..2-3).
pub fn pcr_wrap_add(t: u64, d: u64) -> u64 {
    ((t % PCR_MODULUS) + (d % PCR_MODULUS)) % PCR_MODULUS
}

/// Signed shortest distance `a − b` on the PCR circle (H.222.0 eq. 2-1), in
/// `(−PCR_MODULUS/2, PCR_MODULUS/2]`.
pub fn pcr_signed_diff(a: u64, b: u64) -> i64 {
    signed(pcr_wrap_diff(a, b), PCR_MODULUS)
}

/// `(later − earlier) mod 2^33` for 90 kHz values (PTS/DTS/PCR_base,
/// H.222.0 eq. 2-2, 2-11, 2-12).
pub fn ts90k_wrap_diff(later: u64, earlier: u64) -> u64 {
    (later.wrapping_sub(earlier)) & (BASE_MODULUS - 1)
}

/// `(t + d) mod 2^33` for 90 kHz values (H.222.0 eq. 2-11/2-12).
pub fn ts90k_wrap_add(t: u64, d: u64) -> u64 {
    t.wrapping_add(d) & (BASE_MODULUS - 1)
}

/// Signed shortest distance `a − b` on the 2^33 circle of 90 kHz values
/// (H.222.0 eq. 2-11/2-12).
pub fn ts90k_signed_diff(a: u64, b: u64) -> i64 {
    signed(ts90k_wrap_diff(a, b), BASE_MODULUS)
}

fn signed(d: u64, m: u64) -> i64 {
    if d > m / 2 {
        d as i64 - m as i64
    } else {
        d as i64
    }
}

/// A 33-bit value coded as in PTS/DTS (H.222.0 §2.4.3.6 Table 2-21) and
/// DTS_next_AU (§2.4.3.4 Table 2-6): `prefix(4) value[32..30](3) marker(1)
/// value[29..15](15) marker(1) value[14..0](15) marker(1)` = 40 bits.
///
/// `prefix` is '0010' (PTS only), '0011' (PTS of a PTS+DTS pair), '0001'
/// (DTS), or `splice_type` for DTS_next_AU.  The three marker bits are kept
/// verbatim (`markers` bit 2 = first marker); their spec value is '1'
/// (§2.4.3.7 "marker_bit … has the value '1'").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Timestamp {
    /// The 4 bits preceding the value.
    pub prefix: u8,
    /// The 33-bit value in 90 kHz units (equations 2-11/2-12).
    pub value: u64,
    /// The three marker bits, first marker in bit 2 (spec value 0b111).
    pub markers: u8,
}

impl Timestamp {
    /// Encoded size in bytes.
    pub const LEN: usize = 5;
    /// Prefix of a lone PTS (PTS_DTS_flags = '10'), Table 2-21.
    pub const PREFIX_PTS_ONLY: u8 = 0b0010;
    /// Prefix of the PTS when PTS_DTS_flags = '11', Table 2-21.
    pub const PREFIX_PTS_WITH_DTS: u8 = 0b0011;
    /// Prefix of the DTS when PTS_DTS_flags = '11', Table 2-21.
    pub const PREFIX_DTS: u8 = 0b0001;

    /// Timestamp with the given prefix, value reduced modulo 2^33, markers '1'.
    pub fn new(prefix: u8, value: u64) -> Self {
        Self {
            prefix,
            value: value & (BASE_MODULUS - 1),
            markers: 0b111,
        }
    }

    /// Read the 40-bit field.
    pub fn read(r: &mut BitReader<'_>) -> Result<Self> {
        let prefix = r.read(4)? as u8;
        let hi = r.read(3)?;
        let m0 = r.read(1)?;
        let mid = r.read(15)?;
        let m1 = r.read(1)?;
        let lo = r.read(15)?;
        let m2 = r.read(1)?;
        Ok(Self {
            prefix,
            value: (hi << 30) | (mid << 15) | lo,
            markers: ((m0 << 2) | (m1 << 1) | m2) as u8,
        })
    }

    /// Write the 40-bit field.
    pub fn write(&self, w: &mut BitWriter) -> Result<()> {
        w.write("timestamp prefix", 4, u64::from(self.prefix))?;
        if self.value >> 33 != 0 {
            return Err(crate::Error::Overflow {
                what: "33-bit timestamp",
                value: self.value,
                bits: 33,
            });
        }
        w.write("timestamp[32..30]", 3, self.value >> 30)?;
        w.write("marker_bit", 1, (u64::from(self.markers) >> 2) & 1)?;
        w.write("timestamp[29..15]", 15, (self.value >> 15) & 0x7FFF)?;
        w.write("marker_bit", 1, (u64::from(self.markers) >> 1) & 1)?;
        w.write("timestamp[14..0]", 15, self.value & 0x7FFF)?;
        w.write("marker_bit", 1, u64::from(self.markers) & 1)?;
        if self.markers >> 3 != 0 {
            return Err(crate::Error::Overflow {
                what: "timestamp markers",
                value: u64::from(self.markers),
                bits: 3,
            });
        }
        Ok(())
    }

    /// Seconds at 90 kHz.
    pub fn seconds(&self) -> f64 {
        self.value as f64 / 90_000.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// H.222.0 §2.4.2.2 eq. 2-1..2-3: PCR = base·300 + ext, base = ticks DIV
    /// 300 mod 2^33, ext = ticks mod 300; 33 + 6 + 9 bit layout of Table 2-6.
    #[test]
    fn spec_eq2_1_pcr_formula_and_layout() {
        let t = 27_000_000u64 * 3600 + 299; // one hour and 299 ticks
        let p = Pcr::from_ticks(t);
        assert_eq!(p.base, 324_000_000);
        assert_eq!(p.extension, 299);
        assert_eq!(p.ticks(), t);
        // base = 1, reserved = '111111', ext = 0x1FF: bit layout check.
        let raw = Pcr {
            base: 1,
            reserved: 0x3F,
            extension: 0x1FF,
        }
        .encode()
        .unwrap();
        // 33 bits of base: 0..0 1, then 6 ones, then 9 ones = low 16 bits all set
        assert_eq!(raw, [0x00, 0x00, 0x00, 0x00, 0xFF, 0xFF]);
        let raw = Pcr {
            base: (1 << 33) - 1,
            reserved: 0,
            extension: 0,
        }
        .encode()
        .unwrap();
        assert_eq!(raw, [0xFF, 0xFF, 0xFF, 0xFF, 0x80, 0x00]);
        assert_eq!(Pcr::decode(&raw).unwrap().base, (1 << 33) - 1);
        // modulus: base wraps at 2^33 (eq. 2-2)
        assert_eq!(PCR_MODULUS, 2_576_980_377_600);
        assert_eq!(Pcr::from_ticks(PCR_MODULUS + 5), Pcr::from_ticks(5));
    }

    #[test]
    fn wrap_helpers() {
        assert_eq!(pcr_wrap_diff(10, PCR_MODULUS - 5), 15);
        assert_eq!(pcr_signed_diff(PCR_MODULUS - 5, 10), -15);
        assert_eq!(pcr_wrap_add(PCR_MODULUS - 1, 2), 1);
        assert_eq!(ts90k_wrap_diff(3, BASE_MODULUS - 2), 5);
        assert_eq!(ts90k_signed_diff(BASE_MODULUS - 2, 3), -5);
        assert_eq!(ts90k_wrap_add(BASE_MODULUS - 1, 1), 0);
    }

    /// Table 2-21: PTS coded as '0010' PTS[32..30] m PTS[29..15] m PTS[14..0] m.
    #[test]
    fn spec_table2_21_pts_bit_layout() {
        let ts = Timestamp::new(Timestamp::PREFIX_PTS_ONLY, 0);
        let mut w = BitWriter::new();
        ts.write(&mut w).unwrap();
        // 0010 000 1 | 000000000000000 1 | 000000000000000 1
        assert_eq!(w.into_bytes(), [0x21, 0x00, 0x01, 0x00, 0x01]);
        let ts = Timestamp::new(Timestamp::PREFIX_DTS, (1 << 33) - 1);
        let mut w = BitWriter::new();
        ts.write(&mut w).unwrap();
        assert_eq!(w.into_bytes(), [0x1F, 0xFF, 0xFF, 0xFF, 0xFF]);
        let mut r = BitReader::new(&[0x31, 0x00, 0x03, 0x00, 0x05], "t");
        let ts = Timestamp::read(&mut r).unwrap();
        assert_eq!(ts.prefix, 0b0011);
        assert_eq!(ts.value, (1 << 15) | 2);
        assert_eq!(ts.markers, 0b111);
    }
}

//! Section envelope (H.222.0 §2.4.4.10-11 Tables 2-30/2-32/2-33/2-40,
//! ARIB STD-B10 §5.1 / Fig. 5-1 "section format").
//!
//! Two section forms exist:
//!
//! * **long** (section_syntax_indicator = 1): `table_id(8) ssi(1)
//!   private_indicator(1) reserved(2) section_length(12)
//!   table_id_extension(16) reserved(2) version_number(5)
//!   current_next_indicator(1) section_number(8) last_section_number(8)
//!   body… CRC_32(32)`;
//! * **short** (section_syntax_indicator = 0): `table_id(8) ssi(1)
//!   private_indicator/reserved_future_use(1) reserved(2) section_length(12)
//!   body…` optionally followed by CRC_32 (e.g. B10 TOT has one, TDT not).
//!
//! Every header field is kept verbatim — including reserved bits — so
//! decoding then encoding a valid section reproduces it byte for byte.  Spec
//! *values* of reserved bits are checked by validators, not here.

use tracing::debug;

use crate::bits::{BitReader, BitWriter};
use crate::crc::crc32;
use crate::error::{Error, Result};

/// Largest `section_length` representable by the 12-bit field.  Table
/// specific maxima (1021 for PSI, H.222.0 §2.4.4.11; 4093 for SI, B10 §5.1)
/// are checked by validators.
pub const MAX_SECTION_LENGTH: usize = 4093;

/// Total size in bytes of the section starting at `buf[0]`, from its first
/// three bytes (3 + section_length), or `None` if fewer than 3 bytes.
pub fn total_len(buf: &[u8]) -> Option<usize> {
    if buf.len() < 3 {
        return None;
    }
    Some(3 + ((((buf[1] & 0x0F) as usize) << 8) | buf[2] as usize))
}

/// Header of a long-form section.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LongHeader {
    /// table_id.
    pub table_id: u8,
    /// Bit following section_syntax_indicator ('0' in PSI; ARIB SI
    /// defines it as reserved_future_use).
    pub private_indicator: bool,
    /// The 2 reserved bits before section_length (spec value '11').
    pub reserved1: u8,
    /// table_id_extension (transport_stream_id, program_number, service_id…).
    pub table_id_extension: u16,
    /// The 2 reserved bits before version_number (spec value '11').
    pub reserved2: u8,
    /// version_number (5 bits).
    pub version_number: u8,
    /// current_next_indicator.
    pub current_next_indicator: bool,
    /// section_number.
    pub section_number: u8,
    /// last_section_number.
    pub last_section_number: u8,
}

impl LongHeader {
    /// Header with spec reserved values ('11'), current, section 0 of 0.
    pub fn new(table_id: u8, table_id_extension: u16, version_number: u8) -> Self {
        Self {
            table_id,
            private_indicator: false,
            reserved1: 0b11,
            table_id_extension,
            reserved2: 0b11,
            version_number,
            current_next_indicator: true,
            section_number: 0,
            last_section_number: 0,
        }
    }
}

/// Header of a short-form section.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ShortHeader {
    /// table_id.
    pub table_id: u8,
    /// Bit following section_syntax_indicator.
    pub private_indicator: bool,
    /// The 2 reserved bits before section_length.
    pub reserved1: u8,
}

/// Split a complete long section into header and body (bytes between
/// last_section_number and CRC_32), verifying length and CRC_32.  A CRC
/// failure is also a `tracing` event at `debug` level (`table_id`,
/// `table_id_extension`, `residue`).
pub fn split_long(sec: &[u8]) -> Result<(LongHeader, &[u8])> {
    let (h, body, residue) = split_long_unchecked(sec)?;
    if residue != 0 {
        debug!(
            table_id = h.table_id,
            table_id_extension = h.table_id_extension,
            section_number = h.section_number,
            residue,
            "table_id {:#04x} (extension {:#06x}, section {}): CRC_32 mismatch (residue {residue:#010x})",
            h.table_id,
            h.table_id_extension,
            h.section_number
        );
        return Err(Error::Crc {
            what: "long section",
            residue,
        });
    }
    Ok((h, body))
}

/// Like [`split_long`] but returns the CRC residue instead of failing, for
/// forensic inspection of corrupted captures.
pub fn split_long_unchecked(sec: &[u8]) -> Result<(LongHeader, &[u8], u32)> {
    let mut r = BitReader::new(sec, "long section header");
    let table_id = r.u8()?;
    let ssi = r.bool()?;
    if !ssi {
        return Err(Error::Mismatch {
            what: "section_syntax_indicator",
            value: 0,
            expected: 1,
        });
    }
    let private_indicator = r.bool()?;
    let reserved1 = r.read_u8(2)?;
    let section_length = r.read_usize(12)?;
    if section_length < 9 {
        // Five header bytes after section_length and the CRC_32.
        return Err(Error::Range {
            what: "section_length",
            value: section_length as u64,
            min: 9,
            max: 0xFFF,
        });
    }
    if sec.len() != 3 + section_length {
        return Err(Error::invalid(
            "long section header",
            format!(
                "buffer is {} bytes, section_length says {}",
                sec.len(),
                3 + section_length
            ),
        ));
    }
    let table_id_extension = r.u16()?;
    let reserved2 = r.read_u8(2)?;
    let version_number = r.read_u8(5)?;
    let current_next_indicator = r.bool()?;
    let section_number = r.u8()?;
    let last_section_number = r.u8()?;
    let h = LongHeader {
        table_id,
        private_indicator,
        reserved1,
        table_id_extension,
        reserved2,
        version_number,
        current_next_indicator,
        section_number,
        last_section_number,
    };
    Ok((h, &sec[8..sec.len() - 4], crc32(sec)))
}

/// Serialise a long section: computes section_length and CRC_32.
pub fn build_long(h: &LongHeader, body: &[u8]) -> Result<Vec<u8>> {
    let section_length = 5 + body.len() + 4;
    if section_length > MAX_SECTION_LENGTH {
        return Err(Error::Overflow {
            what: "section_length",
            value: section_length as u64,
            bits: 12,
        });
    }
    let mut w = BitWriter::new();
    w.write("table_id", 8, u64::from(h.table_id))?;
    w.bool(true);
    w.bool(h.private_indicator);
    w.write("reserved", 2, u64::from(h.reserved1))?;
    w.write("section_length", 12, section_length as u64)?;
    w.write("table_id_extension", 16, u64::from(h.table_id_extension))?;
    w.write("reserved", 2, u64::from(h.reserved2))?;
    w.write("version_number", 5, u64::from(h.version_number))?;
    w.bool(h.current_next_indicator);
    w.write("section_number", 8, u64::from(h.section_number))?;
    w.write("last_section_number", 8, u64::from(h.last_section_number))?;
    w.bytes(body);
    let mut v = w.into_bytes();
    let c = crc32(&v);
    v.extend_from_slice(&c.to_be_bytes());
    Ok(v)
}

/// Split a complete short section into header and body.  With `has_crc`
/// the last four bytes are CRC_32 and are verified and excluded from body;
/// a CRC failure is also a `tracing` event at `debug` level.
pub fn split_short(sec: &[u8], has_crc: bool) -> Result<(ShortHeader, &[u8])> {
    let mut r = BitReader::new(sec, "short section header");
    let table_id = r.u8()?;
    if r.bool()? {
        return Err(Error::Mismatch {
            what: "section_syntax_indicator",
            value: 1,
            expected: 0,
        });
    }
    let private_indicator = r.bool()?;
    let reserved1 = r.read_u8(2)?;
    let section_length = r.read_usize(12)?;
    if sec.len() != 3 + section_length {
        return Err(Error::invalid(
            "short section header",
            format!(
                "buffer is {} bytes, section_length says {}",
                sec.len(),
                3 + section_length
            ),
        ));
    }
    let h = ShortHeader {
        table_id,
        private_indicator,
        reserved1,
    };
    if has_crc {
        if section_length < 4 {
            // Too short for CRC_32.
            return Err(Error::Range {
                what: "section_length",
                value: section_length as u64,
                min: 4,
                max: 0xFFF,
            });
        }
        let residue = crc32(sec);
        if residue != 0 {
            debug!(
                table_id,
                residue, "table_id {table_id:#04x}: CRC_32 mismatch (residue {residue:#010x})"
            );
            return Err(Error::Crc {
                what: "short section",
                residue,
            });
        }
        Ok((h, &sec[3..sec.len() - 4]))
    } else {
        Ok((h, &sec[3..]))
    }
}

/// Serialise a short section, appending CRC_32 when `has_crc`.
pub fn build_short(h: &ShortHeader, body: &[u8], has_crc: bool) -> Result<Vec<u8>> {
    let section_length = body.len() + if has_crc { 4 } else { 0 };
    if section_length > MAX_SECTION_LENGTH {
        return Err(Error::Overflow {
            what: "section_length",
            value: section_length as u64,
            bits: 12,
        });
    }
    let mut w = BitWriter::new();
    w.write("table_id", 8, u64::from(h.table_id))?;
    w.bool(false);
    w.bool(h.private_indicator);
    w.write("reserved", 2, u64::from(h.reserved1))?;
    w.write("section_length", 12, section_length as u64)?;
    w.bytes(body);
    let mut v = w.into_bytes();
    if has_crc {
        let c = crc32(&v);
        v.extend_from_slice(&c.to_be_bytes());
    }
    Ok(v)
}

/// Body of a long-form table: everything between last_section_number and
/// CRC_32.  Implementations must be lossless: `encode(decode(b)) == b` for
/// every `b` they accept.
pub trait LongBody: Sized {
    /// table_id values this body is defined for.
    fn accepts_table_id(table_id: u8) -> bool;
    /// Decode the body bytes; the header is supplied for context.
    fn decode(header: &LongHeader, body: &[u8]) -> Result<Self>;
    /// Append the body bytes.
    fn encode(&self, out: &mut Vec<u8>) -> Result<()>;
}

/// Body of a short-form table.
pub trait ShortBody: Sized {
    /// Whether the section ends with CRC_32.
    const HAS_CRC: bool;
    /// table_id values this body is defined for.
    fn accepts_table_id(table_id: u8) -> bool;
    /// Decode the body bytes (CRC excluded).
    fn decode(header: &ShortHeader, body: &[u8]) -> Result<Self>;
    /// Append the body bytes (CRC excluded).
    fn encode(&self, out: &mut Vec<u8>) -> Result<()>;
}

/// A typed long-form section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Long<B> {
    /// Envelope header.
    pub header: LongHeader,
    /// Typed body.
    pub body: B,
}

impl<B: LongBody> Long<B> {
    /// Decode a complete section (verifies length, CRC and table_id).
    pub fn decode(sec: &[u8]) -> Result<Self> {
        let (header, body) = split_long(sec)?;
        if !B::accepts_table_id(header.table_id) {
            return Err(Error::NotAccepted {
                what: "table_id",
                value: u64::from(header.table_id),
            });
        }
        Ok(Self {
            body: B::decode(&header, body)?,
            header,
        })
    }

    /// Encode to complete section bytes with fresh section_length/CRC_32.
    pub fn encode(&self) -> Result<Vec<u8>> {
        let mut body = Vec::new();
        self.body.encode(&mut body)?;
        build_long(&self.header, &body)
    }
}

/// A typed short-form section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Short<B> {
    /// Envelope header.
    pub header: ShortHeader,
    /// Typed body.
    pub body: B,
}

impl<B: ShortBody> Short<B> {
    /// Decode a complete section (verifies length, CRC if any, table_id).
    pub fn decode(sec: &[u8]) -> Result<Self> {
        let (header, body) = split_short(sec, B::HAS_CRC)?;
        if !B::accepts_table_id(header.table_id) {
            return Err(Error::NotAccepted {
                what: "table_id",
                value: u64::from(header.table_id),
            });
        }
        Ok(Self {
            body: B::decode(&header, body)?,
            header,
        })
    }

    /// Encode to complete section bytes.
    pub fn encode(&self) -> Result<Vec<u8>> {
        let mut body = Vec::new();
        self.body.encode(&mut body)?;
        build_short(&self.header, &body, B::HAS_CRC)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_roundtrip_preserves_reserved_bits() {
        let mut h = LongHeader::new(0x42, 0x79EE, 17);
        h.private_indicator = true;
        h.reserved1 = 0b10;
        h.section_number = 1;
        h.last_section_number = 3;
        let s = build_long(&h, &[1, 2, 3]).unwrap();
        assert_eq!(total_len(&s), Some(s.len()));
        let (h2, b) = split_long(&s).unwrap();
        assert_eq!(h, h2);
        assert_eq!(b, &[1, 2, 3]);
        assert_eq!(build_long(&h2, b).unwrap(), s);
    }

    #[test]
    fn crc_error_detected() {
        let mut s = build_long(&LongHeader::new(0, 1, 0), &[0, 0, 0xE0, 0x10]).unwrap();
        s[9] ^= 1;
        assert!(matches!(split_long(&s), Err(Error::Crc { .. })));
    }

    #[test]
    fn short_roundtrip() {
        let h = ShortHeader {
            table_id: 0x73,
            private_indicator: true,
            reserved1: 3,
        };
        let s = build_short(&h, &[0xC0, 0x79, 0x12, 0x45, 0, 0xF0, 0], true).unwrap();
        let (h2, b) = split_short(&s, true).unwrap();
        assert_eq!(h, h2);
        assert_eq!(build_short(&h2, b, true).unwrap(), s);
    }
}

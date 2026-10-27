//! TLV-SI in transmission-control TLV packets (type FEh): TLV-NIT (table
//! id 40h actual / 41h other network) and AMT (FEh), section extension
//! format with CRC-32 (ARIB STD-B32 v3.11 Part 3 §3.2, §3.10 (5)–(6);
//! ARIB STD-B60 v1.14 §5, Tables 4-2, 5-1).
//!
//! The layout of the FEh packet's data around the section is in a MIC
//! notification and ARIB STD-B63, neither local. [`sections`] finds
//! sections by their header and CRC: a section starts at `offset` when its
//! table id is known, the section syntax indicator is set, and the CRC-32
//! over the section is valid (SPEC-NOTES "Control packet layout" records
//! the offset seen on air).

use isdb_ts::crc::crc32;

/// A section found in a control packet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    /// Byte offset of the section in the packet's data.
    pub offset: usize,
    /// Table id.
    pub table_id: u8,
    /// Table id extension (network id for TLV-NIT).
    pub extension: u16,
    /// Version number.
    pub version: u8,
    /// The whole section, CRC included.
    pub bytes: Vec<u8>,
}

/// Name of a TLV-SI table id (B60 Table 4-2).
pub fn table_name(t: u8) -> &'static str {
    match t {
        0x40 => "TLV-NIT (actual)",
        0x41 => "TLV-NIT (other)",
        0xFE => "AMT",
        _ => "other",
    }
}

/// The CRC-valid long-form sections in `data`, scanning every offset.
pub fn sections(data: &[u8]) -> Vec<Section> {
    let mut out = Vec::new();
    let mut i = 0;
    while i + 3 <= data.len() {
        let t = data[i];
        if matches!(t, 0x40 | 0x41 | 0xFE) && data[i + 1] & 0x80 != 0 {
            let len = (usize::from(data[i + 1] & 0x0F) << 8) | usize::from(data[i + 2]);
            let end = i + 3 + len;
            if len >= 9 && end <= data.len() && crc32(&data[i..end]) == 0 {
                out.push(Section {
                    offset: i,
                    table_id: t,
                    extension: u16::from_be_bytes([data[i + 3], data[i + 4]]),
                    version: (data[i + 5] >> 1) & 0x1F,
                    bytes: data[i..end].to_vec(),
                });
                i = end;
                continue;
            }
        }
        i += 1;
    }
    out
}

/// The TLV stream entries of a TLV-NIT section (B32 §3.10 (5)): (TLV
/// stream id, original network id).
pub fn nit_streams(s: &Section) -> Vec<(u16, u16)> {
    let b = &s.bytes;
    let mut out = Vec::new();
    if b.len() < 12 {
        return out;
    }
    let ndl = (usize::from(b[8] & 0x0F) << 8) | usize::from(b[9]);
    let mut p = 10 + ndl;
    let Some(h) = b.get(p..p + 2) else { return out };
    let loop_len = (usize::from(h[0] & 0x0F) << 8) | usize::from(h[1]);
    p += 2;
    let end = (p + loop_len).min(b.len() - 4);
    while p + 6 <= end {
        let id = u16::from_be_bytes([b[p], b[p + 1]]);
        let onid = u16::from_be_bytes([b[p + 2], b[p + 3]]);
        let dl = (usize::from(b[p + 4] & 0x0F) << 8) | usize::from(b[p + 5]);
        out.push((id, onid));
        p += 6 + dl;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A TLV-NIT section with one TLV stream entry and no descriptors.
    pub(crate) fn nit() -> Vec<u8> {
        let mut b = vec![0x40, 0xF0, 0, 0x00, 0x04, 0xC1, 0, 0, 0xF0, 0x00];
        b.extend([0xF0, 0x06, 0xB0, 0xB0, 0x00, 0x04, 0xF0, 0x00]);
        b[2] = u8::try_from(b.len() - 3 + 4).expect("section_length fits");
        let c = crc32(&b);
        b.extend(c.to_be_bytes());
        b
    }

    #[test]
    fn finds_a_nit_at_any_offset() {
        for pad in [0usize, 1, 3] {
            let mut d = vec![0xAAu8; pad];
            d.extend(nit());
            let s = sections(&d);
            assert_eq!(s.len(), 1);
            assert_eq!(s[0].offset, pad);
            assert_eq!(s[0].extension, 4);
            assert_eq!(nit_streams(&s[0]), vec![(0xB0B0, 4)]);
        }
    }
}

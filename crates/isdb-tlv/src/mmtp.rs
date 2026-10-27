//! MMTP packets (ARIB STD-B32 v3.11 Part 3 §3.9) and the MMT-SI messages
//! they carry (ARIB STD-B60 v1.14 §4.2, §7.2.3, Tables 4-6, 4-8, 4-11).
//!
//! Header (B60 §6.4.1.1 Table 6-4): version (2) `00`, packet counter flag
//! (1), FEC type (2, Table 6-5), reserved (1), extension header flag (1),
//! RAP flag (1), reserved (2), payload type (6, Table 6-6), packet id
//! (16), delivery time stamp (32), packet sequence number (32), [packet
//! counter (32)], [extension type (16), length (16), field]. Payload type
//! 00h MPU, 02h control messages ([`PayloadType`]). Packet id 0000h PA
//! message, 0001h CA message, others per Table 4-11 (8000h MH-EIT, 8002h
//! MH-BIT, 8004h MH-SDT, 8005h MH-TOT, …) or as the MPT designates.
//!
//! A control-message payload (Figure No. 2): division index (2), reserved
//! (4), length extension flag (1), aggregation flag (1), division number
//! counter (8), then the message (with a 16- or 32-bit length prefix per
//! message when aggregated). Only undivided messages are decoded here.
//!
//! ISO/IEC 23008-1 (MMT) is not available (SPECS.md §1.2): everything here
//! is taken from B32 Part 3 and B60.

use std::fmt;

/// `FEC_type` of the MMTP header: ARIB STD-B60 v1.14 §6.4.1.1, Table 6-5
/// (2 bits).
///
/// A lossless coded enum: every code of the field has a variant, so a
/// decoded header re-encodes bit-exactly; code 3 is "reserved for future
/// use".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FecType {
    /// 0: "MMTP Packet which is not protected by AL-FEC".
    NotProtected,
    /// 1: "Source packet, among MMTP Packets which are protected by
    /// AL-FEC".
    Source,
    /// 2: "Repair packet, among MMTP Packets which are protected by
    /// AL-FEC".
    Repair,
    /// 3: "reserved for future use".
    Reserved,
}

impl FecType {
    /// The three assigned codes, in Table 6-5 order.
    pub const ASSIGNED: [FecType; 3] = [FecType::NotProtected, FecType::Source, FecType::Repair];

    /// From the 2-bit field (`code` is masked to the width; total).
    pub const fn from_code(code: u8) -> Self {
        match code & 0x03 {
            0 => FecType::NotProtected,
            1 => FecType::Source,
            2 => FecType::Repair,
            _ => FecType::Reserved,
        }
    }

    /// The 2-bit code.
    pub const fn code(self) -> u8 {
        match self {
            FecType::NotProtected => 0,
            FecType::Source => 1,
            FecType::Repair => 2,
            FecType::Reserved => 3,
        }
    }
}

impl fmt::Display for FecType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.code())
    }
}

/// `payload_type` of the MMTP header: ARIB STD-B60 v1.14 §6.4.1.1, Table
/// 6-6 (6 bits).
///
/// A lossless coded enum: the four assigned codes are named, every other
/// code is kept in [`PayloadType::Reserved`] (0x04–0x1F "reserved for
/// ISO/IEC", 0x20–0x3F "provided by the Ministry or private
/// standardization organization"), so a decoded header re-encodes
/// bit-exactly. The `Reserved` payload is a [`ReservedCode`] only
/// `from_code` can build, so every value is canonical and `==` is wire
/// equality. On air only [`Mpu`](PayloadType::Mpu) and
/// [`Messages`](PayloadType::Messages) occur.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PayloadType {
    /// 0x00: "MPU. This involves fragment of MPU considering media."
    Mpu,
    /// 0x01: "Generic object. This involves general object such as a
    /// complete MPU." Table 6-6 note 1: "provided in MMT Standard, but it
    /// is not used in this standard".
    GenericObject,
    /// 0x02: "This involves more than or equal to one control message."
    Messages,
    /// 0x03: "This involves repair symbol of AL-FEC."
    FecRepair,
    /// 0x04–0x3F: reserved for ISO/IEC (–0x1F) or provided by the
    /// Ministry or a private standardization organization (0x20–); the
    /// 6-bit code as decoded.
    Reserved(ReservedCode),
}

/// A payload type Table 6-6 does not assign (0x04–0x3F), as decoded.
///
/// Only [`PayloadType::from_code`] builds one, after it has excluded the
/// four assigned codes: the field is private, so a
/// [`Reserved`](PayloadType::Reserved) value is canonical by construction
/// (one value per wire code) and `==` on the enum is wire equality.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ReservedCode(u8);

impl ReservedCode {
    /// `c` must be a reserved code of Table 6-6, already masked to 6 bits;
    /// only `from_code` upholds this.
    pub(crate) const fn new_unchecked(c: u8) -> Self {
        Self(c)
    }

    /// The 6-bit code.
    pub const fn get(self) -> u8 {
        self.0
    }
}

impl fmt::Display for ReservedCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

impl fmt::Binary for ReservedCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Binary::fmt(&self.0, f)
    }
}

impl fmt::LowerHex for ReservedCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::LowerHex::fmt(&self.0, f)
    }
}

/// A reserved code cannot be spelled as a [`PayloadType`] outside this
/// crate; the only way in is [`PayloadType::from_code`], so a decoded value
/// is canonical:
///
/// ```compile_fail,E0624
/// use isdb_tlv::mmtp::{PayloadType, ReservedCode};
/// let _ = PayloadType::Reserved(ReservedCode::new_unchecked(0x2A));
/// ```
///
/// ```
/// use isdb_tlv::mmtp::PayloadType;
/// assert_eq!(PayloadType::from_code(0x2A), PayloadType::from_code(0xEA));
/// assert_eq!(PayloadType::from_code(0x2A).code(), 0x2A);
/// ```
#[doc(hidden)]
pub const _PAYLOAD_TYPE_CANONICAL: () = ();

impl PayloadType {
    /// The four assigned codes, in Table 6-6 order.
    pub const ASSIGNED: [PayloadType; 4] = [
        PayloadType::Mpu,
        PayloadType::GenericObject,
        PayloadType::Messages,
        PayloadType::FecRepair,
    ];

    /// From the 6-bit field (`code` is masked to the width; total).
    pub const fn from_code(code: u8) -> Self {
        match code & 0x3F {
            0x00 => PayloadType::Mpu,
            0x01 => PayloadType::GenericObject,
            0x02 => PayloadType::Messages,
            0x03 => PayloadType::FecRepair,
            c => PayloadType::Reserved(ReservedCode::new_unchecked(c)),
        }
    }

    /// The 6-bit code.
    pub const fn code(self) -> u8 {
        match self {
            PayloadType::Mpu => 0x00,
            PayloadType::GenericObject => 0x01,
            PayloadType::Messages => 0x02,
            PayloadType::FecRepair => 0x03,
            PayloadType::Reserved(c) => c.get(),
        }
    }
}

impl fmt::Display for PayloadType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:#04x}", self.code())
    }
}

/// The MMTP header fields this crate uses (Table 6-4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    /// `FEC_type` (Table 6-5).
    pub fec_type: FecType,
    /// Random access point flag.
    pub rap: bool,
    /// `payload_type` (Table 6-6: 00h MPU, 02h control messages).
    pub payload_type: PayloadType,
    /// Packet id.
    pub packet_id: u16,
    /// Packet sequence number.
    pub sequence: u32,
    /// The header extension carries a multi-type header extension of type
    /// 0001h, "information related to scramble specified in ARIB STD-B61"
    /// (B60 Tables 6-7 to 6-9). STD-B61 is not available: its content is
    /// not interpreted.
    pub scramble_info: bool,
}

/// Parse an MMTP packet: its header and payload.
pub fn parse(b: &[u8]) -> Option<(Header, &[u8])> {
    if b.len() < 12 {
        return None;
    }
    if b[0] >> 6 != 0 {
        return None;
    }
    let counter = b[0] & 0x20 != 0;
    let fec_type = FecType::from_code(b[0] >> 3);
    let ext = b[0] & 0x02 != 0;
    let rap = b[0] & 0x01 != 0;
    let payload_type = PayloadType::from_code(b[1]);
    let packet_id = u16::from_be_bytes([b[2], b[3]]);
    let sequence = u32::from_be_bytes([b[8], b[9], b[10], b[11]]);
    let mut pos = 12 + if counter { 4 } else { 0 };
    let mut scramble_info = false;
    if ext {
        let h = b.get(pos..pos + 4)?;
        let ext_type = u16::from_be_bytes([h[0], h[1]]);
        let len = usize::from(u16::from_be_bytes([h[2], h[3]]));
        let body = b.get(pos + 4..pos + 4 + len)?;
        if ext_type == 0x0000 {
            // multi-type header extension: (end flag 1, type 15, length
            // 16, bytes) …
            let mut q = 0;
            while let Some(e) = body.get(q..q + 4) {
                let t = u16::from_be_bytes([e[0], e[1]]) & 0x7FFF;
                scramble_info |= t == 0x0001;
                q += 4 + usize::from(u16::from_be_bytes([e[2], e[3]]));
                if e[0] & 0x80 != 0 {
                    break;
                }
            }
        }
        pos += 4 + len;
    }
    Some((
        Header {
            fec_type,
            rap,
            payload_type,
            packet_id,
            sequence,
            scramble_info,
        },
        b.get(pos..)?,
    ))
}

/// One MMT-SI message found in a control-message payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    /// Message id (Table 4-6).
    pub id: u16,
    /// Table ids it carries (PA message: from its table list; M2 section /
    /// short section message: its section's table_id).
    pub tables: Vec<u8>,
}

/// The messages of an undivided control-message payload; `None` when the
/// payload is a fragment of a divided message or malformed.
pub fn messages(p: &[u8]) -> Option<Vec<Message>> {
    if p.len() < 2 {
        return None;
    }
    let division = p[0] >> 6;
    let long_len = p[0] & 0x02 != 0;
    let aggregated = p[0] & 0x01 != 0;
    if division != 0 {
        return None;
    }
    let mut rest = &p[2..];
    let mut out = Vec::new();
    loop {
        let msg = if aggregated {
            let (n, l) = if long_len {
                let h = rest.get(..4)?;
                (u32::from_be_bytes([h[0], h[1], h[2], h[3]]) as usize, 4)
            } else {
                let h = rest.get(..2)?;
                (usize::from(u16::from_be_bytes([h[0], h[1]])), 2)
            };
            let m = rest.get(l..l + n)?;
            rest = &rest[l + n..];
            m
        } else {
            std::mem::take(&mut rest)
        };
        out.push(message(msg)?);
        if rest.is_empty() {
            break;
        }
    }
    Some(out)
}

/// Identify one message.
fn message(m: &[u8]) -> Option<Message> {
    let id = u16::from_be_bytes([*m.first()?, *m.get(1)?]);
    let tables = match id {
        // PA message: id, version, length (32), number_of_tables N, then
        // (table_id, version, length (16)) × N, then the tables. On air
        // N is 0 and the payload still holds tables (SPEC-NOTES "PA
        // message"), so the payload is walked as a sequence of tables,
        // each table_id (8), version (8), length (16), body.
        0x0000 => {
            let len = u32::from_be_bytes([*m.get(3)?, *m.get(4)?, *m.get(5)?, *m.get(6)?]) as usize;
            let body = m.get(7..(7 + len).min(m.len()))?;
            let n = usize::from(*body.first()?);
            let mut ids: Vec<u8> = (0..n)
                .filter_map(|i| body.get(1 + 4 * i).copied())
                .collect();
            let mut p = 1 + 4 * n;
            while let Some(h) = body.get(p..p + 4) {
                ids.push(h[0]);
                p += 4 + usize::from(u16::from_be_bytes([h[2], h[3]]));
            }
            ids.sort_unstable();
            ids.dedup();
            ids
        }
        // M2 section message (length 16) and M2 short section message:
        // the section's table_id follows id, version, length
        0x8000 | 0x8002 => vec![*m.get(5)?],
        // CA message: id, version, length (16), then the table
        0x8001 => vec![*m.get(5)?],
        _ => Vec::new(),
    };
    Some(Message { id, tables })
}

/// The name of an MMT-SI table id (B60 Table 4-8).
pub fn table_name(t: u8) -> &'static str {
    match t {
        0x00 => "PA table",
        0x10 => "complete MPI table",
        0x11..=0x1F => "MP table (subset)",
        0x20 => "MPT",
        0x80 => "PLT",
        0x81 => "LCT",
        0x82 | 0x83 => "ECM",
        0x84 | 0x85 => "EMM",
        0x86 => "CAT (MH)",
        0x8B => "MH-EIT p/f",
        0x8C..=0x9B => "MH-EIT schedule",
        0x9C => "MH-AIT",
        0x9D => "MH-BIT",
        0x9E => "MH-SDTT",
        0x9F => "MH-SDT (actual)",
        0xA0 => "MH-SDT (other)",
        0xA1 => "MH-TOT",
        0xA2 => "MH-CDT",
        0xA3 => "DDM table",
        0xA4 => "DAM table",
        0xA5 => "DCC table",
        0xA6 => "EMT",
        0xA7 => "MH-DIT",
        0xA8 => "MH-SIT",
        0xE0..=0xFF => "broadcaster table",
        _ => "other",
    }
}

/// The name of a message id (B60 Table 4-6).
pub fn message_name(id: u16) -> &'static str {
    match id {
        0x0000 => "PA message",
        0x0001..=0x000F => "MPI message",
        0x0010..=0x001F => "MPT message",
        0x8000 => "M2 section message",
        0x8001 => "CA message",
        0x8002 => "M2 short section message",
        0x8003 => "data transmission message",
        _ => "other message",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spec_pa_message_lists_its_tables() {
        // PA message as seen on air: control-message header 3C 00
        // (undivided, reserved ones, not aggregated), message id 0000,
        // version 00, length 13, number_of_tables 0, then a PLT (80h,
        // version 4, 8 bytes)
        let payload = [
            0x3C, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x0D, 0x00, 0x80, 0x04, 0x00, 0x08,
            0x01, 0x02, 0x00, 0x97, 0x00, 0xA0, 0x01, 0x00,
        ];
        let mut pkt = vec![
            0x00,
            PayloadType::Messages.code(),
            0x00,
            0x00,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            7,
        ];
        pkt.extend(payload);
        let (h, p) = parse(&pkt).unwrap();
        assert_eq!(
            (h.packet_id, h.payload_type, h.sequence),
            (0, PayloadType::Messages, 7)
        );
        let m = messages(p).unwrap();
        assert_eq!(m[0].id, 0);
        assert_eq!(m[0].tables, vec![0x80]);
    }

    /// Table 6-5: the three assigned FEC types and the reserved code 3
    /// round-trip through `from_code` / `code` over the whole 2-bit width.
    #[test]
    fn spec_b60_table_6_5_fec_types() {
        assert_eq!(
            FecType::ASSIGNED.map(FecType::code),
            [0, 1, 2],
            "Table 6-5 order"
        );
        for c in 0..=3u8 {
            let t = FecType::from_code(c);
            assert_eq!(t.code(), c);
            assert_eq!(t == FecType::Reserved, c == 3);
        }
        // masked to the field width
        assert_eq!(FecType::from_code(0xFE), FecType::Repair);
    }

    /// Table 6-6: every 6-bit payload type round-trips through
    /// `from_code` / `code`; the four assigned codes are named and
    /// 0x04–0x3F are `Reserved`.
    #[test]
    fn spec_b60_payload_types() {
        assert_eq!(
            PayloadType::ASSIGNED.map(PayloadType::code),
            [0x00, 0x01, 0x02, 0x03],
            "Table 6-6 order"
        );
        for c in 0..=0x3Fu8 {
            let t = PayloadType::from_code(c);
            assert_eq!(t.code(), c, "{t:?}");
            match t {
                PayloadType::Reserved(r) => {
                    assert!((0x04..=0x3F).contains(&r.get()) && r.get() == c);
                }
                named => assert!(PayloadType::ASSIGNED.contains(&named)),
            }
        }
        assert_eq!(PayloadType::from_code(0x00), PayloadType::Mpu);
        assert_eq!(PayloadType::from_code(0x02), PayloadType::Messages);
        // masked to the field width (the two bits above are reserved
        // header bits, Table 6-4)
        assert_eq!(PayloadType::from_code(0xC2), PayloadType::Messages);
        assert_eq!(PayloadType::from_code(0xFF), PayloadType::from_code(0x3F));
        assert_eq!(PayloadType::from_code(0xFF).code(), 0x3F);
    }

    /// `from_code` is total and masks to the 6-bit field: every `u8`
    /// decodes to the canonical value of its low bits (the `Reserved`
    /// payload can only come from `from_code`, so `==` is wire equality).
    #[test]
    fn prop_payload_type_from_code_masks_to_width() {
        for c in 0..=255u8 {
            assert_eq!(
                PayloadType::from_code(c),
                PayloadType::from_code(c & 0x3F),
                "{c:#04x}"
            );
            assert_eq!(PayloadType::from_code(c).code(), c & 0x3F, "{c:#04x}");
        }
    }

    /// A parsed header's codes are the bytes they came from, for every
    /// FEC type and payload type (lossless; VERIFICATION.md rung 2).
    #[test]
    fn prop_header_codes_round_trip() {
        for fec in 0..=3u8 {
            for pt in 0..=0x3Fu8 {
                let pkt = [fec << 3, pt, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
                let (h, _) = parse(&pkt).unwrap();
                assert_eq!((h.fec_type.code(), h.payload_type.code()), (fec, pt));
            }
        }
    }
}

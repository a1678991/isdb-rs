//! MMTP packets (ARIB STD-B32 v3.11 Part 3 §3.9) and the MMT-SI messages
//! they carry (ARIB STD-B60 v1.14 §4.2, §7.2.3, Tables 4-6, 4-8, 4-11).
//!
//! Header: version (2) `00`, packet counter flag (1), FEC type (2),
//! reserved (1), extension header flag (1), RAP flag (1), reserved (2),
//! payload type (6), packet id (16), delivery time stamp (32), packet
//! sequence number (32), [packet counter (32)], [extension type (16),
//! length (16), field]. Payload type 00h MPU, 02h control messages.
//! Packet id 0000h PA message, 0001h CA message, others per Table 4-11
//! (8000h MH-EIT, 8002h MH-BIT, 8004h MH-SDT, 8005h MH-TOT, …) or as the
//! MPT designates.
//!
//! A control-message payload (Figure No. 2): division index (2), reserved
//! (4), length extension flag (1), aggregation flag (1), division number
//! counter (8), then the message (with a 16- or 32-bit length prefix per
//! message when aggregated). Only undivided messages are decoded here.
//!
//! ISO/IEC 23008-1 (MMT) is not available (SPECS.md §1.2): everything here
//! is taken from B32 Part 3 and B60.

/// The MMTP header fields this crate uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    /// FEC type (0 = not protected).
    pub fec_type: u8,
    /// Random access point flag.
    pub rap: bool,
    /// Payload type (00h MPU, 02h control messages).
    pub payload_type: u8,
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

/// Payload type: MPU.
pub const MPU: u8 = 0x00;
/// Payload type: control messages.
pub const MESSAGES: u8 = 0x02;

/// Parse an MMTP packet: its header and payload.
pub fn parse(b: &[u8]) -> Option<(Header, &[u8])> {
    if b.len() < 12 {
        return None;
    }
    if b[0] >> 6 != 0 {
        return None;
    }
    let counter = b[0] & 0x20 != 0;
    let fec_type = (b[0] >> 3) & 3;
    let ext = b[0] & 0x02 != 0;
    let rap = b[0] & 0x01 != 0;
    let payload_type = b[1] & 0x3F;
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
        let mut pkt = vec![0x00, MESSAGES, 0x00, 0x00, 0, 0, 0, 0, 0, 0, 0, 7];
        pkt.extend(payload);
        let (h, p) = parse(&pkt).unwrap();
        assert_eq!((h.packet_id, h.payload_type, h.sequence), (0, MESSAGES, 7));
        let m = messages(p).unwrap();
        assert_eq!(m[0].id, 0);
        assert_eq!(m[0].tables, vec![0x80]);
    }
}

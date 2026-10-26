//! MMT-SI messages (ARIB STD-B60 v1.14 §7.2, Tables 7-1 to 7-4) from
//! control-message MMTP payloads (payload type 02h, Table 6-1): divided
//! messages are reassembled per packet id from their fragments
//! (fragmentation indicator 01, 10…, 11), aggregated payloads are split by
//! their length prefixes, and each message is cut into the tables it
//! carries.

use std::collections::BTreeMap;

/// Largest message kept while reassembling (a PA message's length field
/// is 32 bits; tables are at most 64 KiB, sections 4 KiB).
pub const MAX_MESSAGE: usize = 1 << 20;

/// A table found in a message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableBytes {
    /// The MMTP packet id it came on.
    pub packet_id: u16,
    /// The message id (Table 4-6).
    pub message_id: u16,
    /// The table: PA tables from `table_id` (8), `version` (8), `length`
    /// (16) on; sections from `table_id` on, CRC included.
    pub bytes: Vec<u8>,
}

/// What went wrong reassembling or splitting messages.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MessageErrors {
    /// Fragments that did not continue a message being reassembled (a
    /// lost fragment, or a stream joined mid-message).
    pub orphan_fragments: u64,
    /// Messages dropped for exceeding [`MAX_MESSAGE`].
    pub oversize: u64,
    /// Messages or tables whose lengths do not fit.
    pub malformed: u64,
}

#[derive(Debug, Default)]
struct Partial {
    buf: Vec<u8>,
    long_len: bool,
    aggregated: bool,
    next_counter: u8,
}

/// Reassembles messages per packet id.
#[derive(Debug, Default)]
pub struct Messages {
    partial: BTreeMap<u16, Partial>,
    /// Error counters.
    pub errors: MessageErrors,
}

impl Messages {
    /// A reassembler with nothing pending.
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed one control-message payload of `packet_id` (the MMTP payload,
    /// from the fragmentation indicator byte on). Lost MMTP packets on the
    /// id (`gap`) drop the message being reassembled.
    pub fn push(&mut self, packet_id: u16, payload: &[u8], gap: bool, out: &mut Vec<TableBytes>) {
        let (Some(&f), Some(&counter)) = (payload.first(), payload.get(1)) else {
            self.errors.malformed += 1;
            return;
        };
        let division = f >> 6;
        let long_len = f & 0x02 != 0;
        let aggregated = f & 0x01 != 0;
        let data = &payload[2..];
        if gap {
            self.partial.remove(&packet_id);
        }
        let whole = match division {
            0b00 => {
                self.partial.remove(&packet_id);
                Some((data.to_vec(), long_len, aggregated))
            }
            0b01 => {
                self.partial.insert(
                    packet_id,
                    Partial {
                        buf: data.to_vec(),
                        long_len,
                        aggregated,
                        next_counter: counter.wrapping_sub(1),
                    },
                );
                None
            }
            _ => match self.partial.get_mut(&packet_id) {
                // fragment_counter counts the fragments after this one
                Some(p) if p.next_counter == counter => {
                    if p.buf.len() + data.len() > MAX_MESSAGE {
                        self.partial.remove(&packet_id);
                        self.errors.oversize += 1;
                        None
                    } else {
                        p.buf.extend_from_slice(data);
                        p.next_counter = counter.wrapping_sub(1);
                        if division == 0b11 {
                            self.partial
                                .remove(&packet_id)
                                .map(|p| (p.buf, p.long_len, p.aggregated))
                        } else {
                            None
                        }
                    }
                }
                _ => {
                    self.partial.remove(&packet_id);
                    self.errors.orphan_fragments += 1;
                    None
                }
            },
        };
        if let Some((bytes, long_len, aggregated)) = whole {
            for m in split_aggregated(&bytes, long_len, aggregated, &mut self.errors) {
                tables_of(packet_id, m, out, &mut self.errors);
            }
        }
    }
}

/// The messages of a reassembled payload.
fn split_aggregated<'a>(
    b: &'a [u8],
    long_len: bool,
    aggregated: bool,
    e: &mut MessageErrors,
) -> Vec<&'a [u8]> {
    if !aggregated {
        return vec![b];
    }
    let mut out = Vec::new();
    let mut p = 0;
    let w = if long_len { 4 } else { 2 };
    while p + w <= b.len() {
        let n = b[p..p + w]
            .iter()
            .fold(0usize, |a, &x| (a << 8) | usize::from(x));
        let Some(m) = b.get(p + w..p + w + n) else {
            e.malformed += 1;
            break;
        };
        out.push(m);
        p += w + n;
    }
    out
}

/// Cut one message into its tables.
fn tables_of(packet_id: u16, m: &[u8], out: &mut Vec<TableBytes>, e: &mut MessageErrors) {
    let Some(id) = m.get(..2).map(|x| u16::from_be_bytes([x[0], x[1]])) else {
        e.malformed += 1;
        return;
    };
    let mut push = |bytes: &[u8]| {
        out.push(TableBytes {
            packet_id,
            message_id: id,
            bytes: bytes.to_vec(),
        });
    };
    match id {
        // PA message: id 16, version 8, length 32, extension (N, table
        // entries), payload of tables, each table_id 8, version 8, length
        // 16, body. The payload is walked as tables whatever N says
        // (isdb-tlv SPEC-NOTES "pa-message").
        0x0000 => {
            let Some(h) = m.get(3..7) else {
                e.malformed += 1;
                return;
            };
            let len = u32::from_be_bytes([h[0], h[1], h[2], h[3]]) as usize;
            let body = &m[7..(7 + len).min(m.len())];
            let Some(&n) = body.first() else { return };
            let mut p = 1 + 4 * usize::from(n);
            while let Some(t) = body.get(p..p + 4) {
                let tl = usize::from(u16::from_be_bytes([t[2], t[3]]));
                let Some(table) = body.get(p..p + 4 + tl) else {
                    e.malformed += 1;
                    break;
                };
                push(table);
                p += 4 + tl;
            }
        }
        // M2 section message, CA message, M2 short section message:
        // id 16, version 8, length 16, then one table / section
        0x8000..=0x8002 => {
            let Some(h) = m.get(3..5) else {
                e.malformed += 1;
                return;
            };
            let len = usize::from(u16::from_be_bytes([h[0], h[1]]));
            match m.get(5..5 + len) {
                Some(t) => push(t),
                None => e.malformed += 1,
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spec_pa_message_on_air_yields_its_plt() {
        // 3C 00 | 0000 00 0000000D | 00 | 80 04 0008 01 02 0097 00 A0 01 00
        let p = [
            0x3C, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x0D, 0x00, 0x80, 0x04, 0x00, 0x08,
            0x01, 0x02, 0x00, 0x97, 0x00, 0xA0, 0x01, 0x00,
        ];
        let mut out = Vec::new();
        Messages::new().push(0, &p, false, &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].bytes[0], 0x80);
        assert_eq!(out[0].bytes.len(), 12);
    }

    fn m2(section: &[u8]) -> Vec<u8> {
        let mut m = vec![0x80, 0x00, 0x00];
        m.extend((section.len() as u16).to_be_bytes());
        m.extend(section);
        m
    }

    #[test]
    fn divided_message_reassembles() {
        let msg = m2(&[0x8B; 40]);
        let (a, b) = msg.split_at(17);
        let mut r = Messages::new();
        let mut out = Vec::new();
        r.push(0x8000, &[[0x7C, 1].as_slice(), a].concat(), false, &mut out);
        assert!(out.is_empty());
        r.push(0x8000, &[[0xFC, 0].as_slice(), b].concat(), false, &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].bytes, vec![0x8B; 40]);
        // a lost middle fragment drops the message
        let mut out = Vec::new();
        r.push(0x8000, &[[0x7C, 2].as_slice(), a].concat(), false, &mut out);
        r.push(0x8000, &[[0xFC, 0].as_slice(), b].concat(), false, &mut out);
        assert!(out.is_empty());
        assert_eq!(r.errors.orphan_fragments, 1);
    }

    #[test]
    fn aggregated_messages_split() {
        let a = m2(&[0x01; 3]);
        let b = m2(&[0x02; 5]);
        let mut p = vec![0x3D, 0x00];
        for m in [&a, &b] {
            p.extend((m.len() as u16).to_be_bytes());
            p.extend(m);
        }
        let mut out = Vec::new();
        Messages::new().push(0x8000, &p, false, &mut out);
        assert_eq!(out.len(), 2);
        assert_eq!(out[1].bytes, vec![0x02; 5]);
    }

    proptest::proptest! {
        #[test]
        fn prop_any_division_reassembles(len in 1usize..3000, cuts in proptest::collection::vec(1usize..400, 0..12)) {
            let section: Vec<u8> = (0..len).map(|i| (i * 7) as u8).collect();
            let msg = m2(&section);
            let mut pieces = Vec::new();
            let mut p = 0;
            for c in cuts {
                if p + c >= msg.len() { break; }
                pieces.push(&msg[p..p + c]);
                p += c;
            }
            pieces.push(&msg[p..]);
            let n = pieces.len();
            let mut r = Messages::new();
            let mut out = Vec::new();
            for (i, piece) in pieces.iter().enumerate() {
                let div = if n == 1 { 0 } else if i == 0 { 1 } else if i == n - 1 { 3 } else { 2 };
                let hdr = [(div << 6) | 0x3C, (n - 1 - i) as u8];
                r.push(0x8000, &[hdr.as_slice(), piece].concat(), false, &mut out);
            }
            proptest::prop_assert_eq!(out.len(), 1);
            proptest::prop_assert_eq!(&out[0].bytes, &section);
        }
    }
}

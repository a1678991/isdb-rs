//! Sections → Transport Stream packets (H.222.0 §2.4.4, §2.4.4.1–2.4.4.2).
//!
//! * A packet in which at least one section begins has
//!   payload_unit_start_indicator '1' and starts its payload with the
//!   pointer_field, the number of bytes before the first section start
//!   (§2.4.4.2); other packets have PUSI '0' and no pointer_field.
//! * After the last byte of a section the rest of the packet may be filled
//!   with 0xFF stuffing bytes, and then "all bytes until the end of the
//!   Transport Stream packet shall also be stuffing bytes" (§2.4.4).
//! * Packets carry payload only (adaptation_field_control '01'), so every
//!   packet increments the continuity_counter (§2.4.3.3).

use crate::cc::Counter;
use crate::error::{Error, Result};
use crate::packet::{PACKET_SIZE, PAYLOAD_CAPACITY, Packet};
use crate::pid::Pid;

/// How sections share packets (H.222.0 §2.4.4.2 pointer_field, §2.4.4
/// stuffing).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Packing {
    /// Every section starts in a new packet (pointer_field 0); the packet
    /// holding a section's last byte is filled with 0xFF stuffing.
    OnePerPacket,
    /// A section may start in the packet where the previous one ends
    /// (pointer_field = bytes of the previous section in that packet).
    Packed,
}

/// Packetize `sections` (each a complete section, `3 + section_length`
/// bytes) onto `pid`, drawing continuity_counter values from `cc`
/// (H.222.0 §2.4.4.1–2.4.4.2, §2.4.3.3).
pub fn packetize_sections(
    pid: Pid,
    sections: &[Vec<u8>],
    packing: Packing,
    cc: &mut Counter,
) -> Result<Vec<[u8; PACKET_SIZE]>> {
    for s in sections {
        if crate::section::total_len(s) != Some(s.len()) {
            return Err(Error::invalid(
                "section_length",
                "buffer is not one complete section",
            ));
        }
    }
    let mut out = Vec::new();
    let mut idx = 0; // next section to start
    let mut cur: &[u8] = &[]; // remaining bytes of the section in progress
    while idx < sections.len() || !cur.is_empty() {
        let mut payload = Vec::with_capacity(PAYLOAD_CAPACITY);
        // A new section can begin in this packet if one is pending and either
        // nothing is in progress, or (packed) the tail leaves room for at
        // least one byte of it after the pointer_field.
        let starts_here = idx < sections.len()
            && (cur.is_empty() || (packing == Packing::Packed && cur.len() < PAYLOAD_CAPACITY - 1));
        if starts_here {
            payload.push(cur.len() as u8); // pointer_field
            payload.extend_from_slice(cur);
            cur = &[];
            loop {
                let room = PAYLOAD_CAPACITY - payload.len();
                let s = &sections[idx];
                idx += 1;
                if s.len() <= room {
                    payload.extend_from_slice(s);
                    let more = packing == Packing::Packed
                        && idx < sections.len()
                        && payload.len() < PAYLOAD_CAPACITY;
                    if !more {
                        break;
                    }
                } else {
                    payload.extend_from_slice(&s[..room]);
                    cur = &s[room..];
                    break;
                }
            }
        } else {
            let n = cur.len().min(PAYLOAD_CAPACITY);
            payload.extend_from_slice(&cur[..n]);
            cur = &cur[n..];
        }
        payload.resize(PAYLOAD_CAPACITY, 0xFF); // §2.4.4 stuffing
        let p = Packet::with_payload(pid, starts_here, cc.next(true), &payload)?;
        out.push(p.encode()?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::demux::{Assembled, SectionAssembler};
    use crate::section::{LongHeader, build_long};

    fn sec(tid: u8, n: usize) -> Vec<u8> {
        let body: Vec<u8> = (0..n).map(|i| (i * 13 + tid as usize) as u8).collect();
        build_long(&LongHeader::new(tid, 7, 1), &body).unwrap()
    }

    fn reassemble(pkts: &[[u8; PACKET_SIZE]], pid: Pid) -> Vec<Vec<u8>> {
        let mut a = SectionAssembler::new(pid);
        let mut v = Vec::new();
        for (i, p) in pkts.iter().enumerate() {
            for o in a.push(i as u64, p) {
                match o {
                    Assembled::Section(s) => v.push(s.data),
                    Assembled::Event(e) => panic!("unexpected event {e:?}"),
                }
            }
        }
        v
    }

    #[test]
    fn one_per_packet_layout() {
        let s = vec![sec(0x42, 300), sec(0x42, 5)];
        let mut cc = Counter::new(15);
        let p = packetize_sections(Pid::new(0x11).unwrap(), &s, Packing::OnePerPacket, &mut cc)
            .unwrap();
        assert_eq!(p.len(), 3);
        assert_eq!(p[0][1] & 0x40, 0x40); // PUSI
        assert_eq!(p[0][4], 0); // pointer_field
        assert_eq!(p[1][1] & 0x40, 0);
        assert_eq!(p[2][4], 0);
        assert_eq!(
            p.iter().map(|x| x[3] & 0xF).collect::<Vec<_>>(),
            vec![15, 0, 1]
        );
        assert!(p[1][4 + 312 - 183..].iter().all(|&b| b == 0xFF));
        assert_eq!(reassemble(&p, Pid::new(0x11).unwrap()), s);
    }

    #[test]
    fn packed_layout_uses_pointer() {
        let s = vec![sec(0x42, 200), sec(0x43, 20), sec(0x44, 400), sec(0x45, 1)];
        let mut cc = Counter::new(0);
        let p = packetize_sections(Pid::new(0x12).unwrap(), &s, Packing::Packed, &mut cc).unwrap();
        // second packet: pointer = 212 - 183 = 29
        assert_eq!(p[1][4], 29);
        assert_eq!(reassemble(&p, Pid::new(0x12).unwrap()), s);
        let total: usize = s.iter().map(Vec::len).sum();
        assert!(p.len() <= total.div_ceil(183) + 1);
    }

    #[test]
    fn boundary_lengths_roundtrip() {
        for n in 170..200 {
            for packing in [Packing::Packed, Packing::OnePerPacket] {
                let s = vec![
                    sec(0x42, n),
                    sec(0x42, n + 1),
                    sec(0x50, 3),
                    sec(0x51, n * 2),
                ];
                let mut cc = Counter::new(3);
                let p = packetize_sections(Pid::new(0x13).unwrap(), &s, packing, &mut cc).unwrap();
                assert_eq!(
                    reassemble(&p, Pid::new(0x13).unwrap()),
                    s,
                    "n={n} {packing:?}"
                );
            }
        }
    }
}

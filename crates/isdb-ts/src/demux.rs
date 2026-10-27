//! Section reassembly from Transport Stream packets (H.222.0 §2.4.4,
//! §2.4.4.1–2.4.4.2 pointer_field, §2.4.3.3 payload_unit_start_indicator).
//!
//! Rules implemented, quoting the spec:
//! * PUSI for PSI: "if the Transport Stream packet carries the first byte of
//!   a PSI section, the payload_unit_start_indicator value shall be '1',
//!   indicating that the first byte of the payload … carries the
//!   pointer_field" (§2.4.3.3); without PUSI there is no pointer_field.
//! * pointer_field: "the number of bytes, immediately following the
//!   pointer_field until the first byte of the first section that is present
//!   in the payload" (§2.4.4.2) — so the bytes before it finish the section
//!   in progress.
//! * Stuffing: "packet stuffing bytes of value 0xFF may be found in the
//!   payload … only after the last byte of a section. In this case all bytes
//!   until the end of the Transport Stream packet shall also be stuffing
//!   bytes" (§2.4.4); table_id 0xFF is "Forbidden" (Table 2-31), so a 0xFF
//!   where a table_id would be is stuffing.
//! * Section size is `3 + section_length` (Table 2-30 etc.); sections may
//!   span packets and several may follow each other in one packet.
//!
//! Partial sections are discarded on a continuity break, a
//! transport_error_indicator, or an inconsistent pointer_field.  Nothing is
//! assembled before the first packet with PUSI.
//!
//! Every [`DemuxEvent`] the assembler returns, a reset, and a section
//! whose section_length exceeds [`MAX_SECTION_LENGTH`] are also emitted
//! as `tracing` events at `debug` level with `pid`, `packet_index` and
//! (where known) `table_id` fields; a packet that simply continues a
//! section emits nothing.

use std::collections::HashMap;
use std::fmt;

use tracing::debug;

use crate::cc::Tracker;
use crate::packet::{PACKET_SIZE, View};
use crate::pid::Pid;
use crate::section::MAX_SECTION_LENGTH;

/// A complete section as carried on one PID (H.222.0 §2.4.4; size
/// `3 + section_length`, Table 2-30/2-35).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    /// PID it was carried on.
    pub pid: Pid,
    /// Index of the packet holding its first byte.
    pub start_packet: u64,
    /// Index of the packet holding its last byte.
    pub end_packet: u64,
    /// The section bytes, `3 + section_length` long.
    pub data: Vec<u8>,
}

/// Why the assembler dropped data or saw something unexpected
/// (H.222.0 §2.4.4, §2.4.4.2, §2.4.3.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DemuxEventKind {
    /// A partial section was dropped (`bytes` collected so far).
    Discarded {
        /// Bytes of the partial section that were dropped.
        bytes: usize,
        /// Reason.
        reason: DiscardReason,
    },
    /// pointer_field points beyond the payload (§2.4.4.2); packet ignored.
    PointerOutOfRange {
        /// pointer_field value.
        pointer: u8,
        /// payload size after the pointer_field.
        available: usize,
    },
    /// Bytes after the end of a section that are neither a new section
    /// (no PUSI) nor 0xFF stuffing (§2.4.4).
    UnexpectedBytes {
        /// Number of bytes ignored.
        bytes: usize,
    },
    /// Stuffing (0xFF) started but non-0xFF bytes follow in the same packet
    /// (§2.4.4 "all bytes until the end … shall also be stuffing bytes").
    BadStuffing,
}

/// Reason a partial section was dropped (H.222.0 §2.4.3.3 continuity,
/// §2.4.4.2 pointer_field).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum DiscardReason {
    /// continuity_counter break (packet loss).
    ContinuityBreak,
    /// transport_error_indicator '1'.
    TransportError,
    /// The pointer_field of the next PUSI packet says a new section starts
    /// before (or after) the partial section's declared end.
    PointerMismatch,
    /// [`SectionAssembler::reset`] was called.
    Reset,
}

impl fmt::Display for DemuxEventKind {
    /// One sentence naming the deviation and its numbers, as the `tracing`
    /// events print it: `partial section dropped (183 bytes): continuity
    /// break`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DemuxEventKind::Discarded { bytes, reason } => {
                write!(f, "partial section dropped ({bytes} bytes): {reason}")
            }
            DemuxEventKind::PointerOutOfRange { pointer, available } => write!(
                f,
                "pointer_field {pointer} points past the {available}-byte payload; packet ignored"
            ),
            DemuxEventKind::UnexpectedBytes { bytes } => {
                write!(
                    f,
                    "{bytes} bytes after the end of a section are not stuffing"
                )
            }
            DemuxEventKind::BadStuffing => {
                write!(f, "non-0xFF bytes follow stuffing in the same packet")
            }
        }
    }
}

impl fmt::Display for DiscardReason {
    /// Short reason text: `continuity break`, `transport_error_indicator`,
    /// `pointer_field mismatch`, `reset`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            DiscardReason::ContinuityBreak => "continuity break",
            DiscardReason::TransportError => "transport_error_indicator",
            DiscardReason::PointerMismatch => "pointer_field mismatch",
            DiscardReason::Reset => "reset",
        })
    }
}

/// An event of the section assembler (H.222.0 §2.4.4) with the index of
/// the packet that caused it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DemuxEvent {
    /// PID.
    pub pid: Pid,
    /// Packet index.
    pub packet_index: u64,
    /// What happened.
    pub kind: DemuxEventKind,
}

/// Output of [`SectionAssembler::push`] (H.222.0 §2.4.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Assembled {
    /// A complete section.
    Section(Section),
    /// A diagnostic event.
    Event(DemuxEvent),
}

#[derive(Debug, Clone)]
struct Partial {
    start_packet: u64,
    data: Vec<u8>,
}

/// Per-PID section reassembler (H.222.0 §2.4.4.1–2.4.4.2).
#[derive(Debug, Clone)]
pub struct SectionAssembler {
    pid: Pid,
    cc: Tracker,
    partial: Option<Partial>,
}

impl SectionAssembler {
    /// Assembler for `pid`.
    pub fn new(pid: Pid) -> Self {
        Self {
            pid,
            cc: Tracker::new(),
            partial: None,
        }
    }

    /// PID served.
    pub fn pid(&self) -> Pid {
        self.pid
    }

    /// Drop any partial section and the continuity expectation.
    pub fn reset(&mut self, index: u64, out: &mut Vec<Assembled>) {
        debug!(
            pid = %self.pid,
            packet_index = index,
            "PID {}: section assembler reset at packet {index}",
            self.pid
        );
        self.discard(index, DiscardReason::Reset, out);
        self.cc.reset();
    }

    fn event(&self, index: u64, kind: DemuxEventKind, out: &mut Vec<Assembled>) {
        debug!(
            pid = %self.pid,
            packet_index = index,
            "PID {}: {kind} (packet {index})",
            self.pid
        );
        out.push(Assembled::Event(DemuxEvent {
            pid: self.pid,
            packet_index: index,
            kind,
        }));
    }

    fn discard(&mut self, index: u64, reason: DiscardReason, out: &mut Vec<Assembled>) {
        if let Some(p) = self.partial.take() {
            let kind = DemuxEventKind::Discarded {
                bytes: p.data.len(),
                reason,
            };
            // The first byte of a partial section is its table_id (the
            // assembler keeps at least one byte).
            let table_id = p.data.first().copied().unwrap_or(0xFF);
            debug!(
                pid = %self.pid,
                packet_index = index,
                table_id,
                "PID {}: table_id {table_id:#04x} {kind} (packet {index})",
                self.pid
            );
            out.push(Assembled::Event(DemuxEvent {
                pid: self.pid,
                packet_index: index,
                kind,
            }));
        }
    }

    /// Feed packet `index` (any PID; other PIDs are ignored) and collect
    /// completed sections and events into a new vector.
    pub fn push(&mut self, index: u64, pkt: &[u8; PACKET_SIZE]) -> Vec<Assembled> {
        let mut out = Vec::new();
        self.push_into(index, pkt, &mut out);
        out
    }

    /// Like [`SectionAssembler::push`] but appends to `out`.
    pub fn push_into(&mut self, index: u64, pkt: &[u8; PACKET_SIZE], out: &mut Vec<Assembled>) {
        let Ok(v) = View::parse(pkt) else {
            // Structurally broken packet: treat like a transport error.
            if crate::packet::Header::decode(pkt).is_ok_and(|h| h.pid == self.pid) {
                self.discard(index, DiscardReason::TransportError, out);
                self.cc.reset();
            }
            return;
        };
        if v.header.pid != self.pid {
            return;
        }
        if v.header.transport_error_indicator {
            self.discard(index, DiscardReason::TransportError, out);
            self.cc.reset();
            return;
        }
        let c = self.cc.update(&v.header, v.discontinuity_indicator());
        if c.is_repeat() {
            return;
        }
        if c.is_loss() {
            self.discard(index, DiscardReason::ContinuityBreak, out);
        }
        let Some(payload) = v.payload else { return };
        if v.header.payload_unit_start_indicator {
            let Some((&ptr, rest)) = payload.split_first() else {
                return;
            };
            let ptr_us = ptr as usize;
            if ptr_us > rest.len() {
                self.event(
                    index,
                    DemuxEventKind::PointerOutOfRange {
                        pointer: ptr,
                        available: rest.len(),
                    },
                    out,
                );
                self.discard(index, DiscardReason::PointerMismatch, out);
                return;
            }
            let (tail, start) = rest.split_at(ptr_us);
            if let Some(mut p) = self.partial.take() {
                // The bytes before the pointed-to start finish the section in
                // progress (§2.4.4.2).
                p.data.extend_from_slice(tail);
                match crate::section::total_len(&p.data) {
                    Some(t) if t <= p.data.len() => self.complete(index, p, t, out),
                    _ => {
                        self.partial = Some(p);
                        self.discard(index, DiscardReason::PointerMismatch, out);
                    }
                }
            }
            self.parse_sections(index, start, out);
        } else if let Some(mut p) = self.partial.take() {
            p.data.extend_from_slice(payload);
            match crate::section::total_len(&p.data) {
                Some(t) if t <= p.data.len() => self.complete(index, p, t, out),
                _ => self.partial = Some(p),
            }
        }
        // else: not synchronised to a section start yet — ignore.
    }

    /// Emit the first `t` bytes of `p` as a section; the bytes after it in
    /// this packet (before the pointed-to start, or to the end of a non-PUSI
    /// payload) shall be 0xFF stuffing (§2.4.4).
    fn complete(&mut self, index: u64, mut p: Partial, t: usize, out: &mut Vec<Assembled>) {
        let extra = p.data.split_off(t);
        out.push(Assembled::Section(Section {
            pid: self.pid,
            start_packet: p.start_packet,
            end_packet: index,
            data: p.data,
        }));
        if !extra.iter().all(|&b| b == 0xFF) {
            self.event(
                index,
                DemuxEventKind::UnexpectedBytes { bytes: extra.len() },
                out,
            );
        }
    }

    /// Parse sections starting at `b[0]` of a PUSI packet's payload.
    fn parse_sections(&mut self, index: u64, mut b: &[u8], out: &mut Vec<Assembled>) {
        while let Some(&first) = b.first() {
            if first == 0xFF {
                if !b.iter().all(|&x| x == 0xFF) {
                    self.event(index, DemuxEventKind::BadStuffing, out);
                }
                return;
            }
            let total = crate::section::total_len(b);
            if let Some(t) = total
                && t > 3 + MAX_SECTION_LENGTH
            {
                // §2.4.4.11 / ARIB STD-B10 §5.1: section_length never
                // exceeds 4093; the section is still assembled and left to
                // the decoder to reject.
                debug!(
                    pid = %self.pid,
                    packet_index = index,
                    table_id = first,
                    section_length = t - 3,
                    "PID {}: table_id {first:#04x} section_length {} exceeds the maximum {MAX_SECTION_LENGTH} (packet {index})",
                    self.pid,
                    t - 3
                );
            }
            match total {
                Some(t) if t <= b.len() => {
                    out.push(Assembled::Section(Section {
                        pid: self.pid,
                        start_packet: index,
                        end_packet: index,
                        data: b[..t].to_vec(),
                    }));
                    b = &b[t..];
                }
                _ => {
                    self.partial = Some(Partial {
                        start_packet: index,
                        data: b.to_vec(),
                    });
                    return;
                }
            }
        }
    }
}

/// Section assemblers for a set of PIDs (H.222.0 §2.4.4, one
/// [`SectionAssembler`] per PID).
#[derive(Debug, Clone, Default)]
pub struct SectionDemux {
    assemblers: HashMap<Pid, SectionAssembler>,
}

impl SectionDemux {
    /// Empty demux.
    pub fn new() -> Self {
        Self::default()
    }

    /// Start assembling sections on `pid` (no-op if already present).
    pub fn add_pid(&mut self, pid: Pid) {
        self.assemblers
            .entry(pid)
            .or_insert_with(|| SectionAssembler::new(pid));
    }

    /// True if `pid` is being assembled.
    pub fn has_pid(&self, pid: Pid) -> bool {
        self.assemblers.contains_key(&pid)
    }

    /// Feed one packet; returns completed sections and events.
    pub fn push(&mut self, index: u64, pkt: &[u8; PACKET_SIZE]) -> Vec<Assembled> {
        let pid = Pid::from_wire((u16::from(pkt[1] & 0x1F) << 8) | u16::from(pkt[2]));
        let mut out = Vec::new();
        if let Some(a) = self.assemblers.get_mut(&pid) {
            a.push_into(index, pkt, &mut out);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cc::ContinuityCounter;
    use crate::packet::Packet;
    use crate::section::{LongHeader, build_long};

    fn sec(tid: u8, n: usize) -> Vec<u8> {
        let body: Vec<u8> = std::iter::successors(Some(tid), |b| Some(b.wrapping_add(7)))
            .take(n)
            .collect();
        build_long(&LongHeader::new(tid, 1, 0), &body).unwrap()
    }

    fn pkt(pid: Pid, pusi: bool, cc: u8, payload: &[u8]) -> [u8; PACKET_SIZE] {
        let cc = ContinuityCounter::new(cc).unwrap();
        let mut p = payload.to_vec();
        p.resize(184, 0xFF);
        Packet::with_payload(pid, pusi, cc, &p)
            .unwrap()
            .encode()
            .unwrap()
    }

    fn sections(out: &[Assembled]) -> Vec<&Section> {
        out.iter()
            .filter_map(|a| {
                if let Assembled::Section(s) = a {
                    Some(s)
                } else {
                    None
                }
            })
            .collect()
    }

    #[test]
    fn spec_2_4_4_section_spanning_three_packets() {
        let s = sec(0x42, 400); // 412 bytes
        assert_eq!(s.len(), 412);
        let mut a = SectionAssembler::new(Pid::new(0x11).unwrap());
        let mut p0 = vec![0u8];
        p0.extend_from_slice(&s[..183]);
        let mut out = a.push(10, &pkt(Pid::new(0x11).unwrap(), true, 0, &p0));
        out.extend(a.push(11, &pkt(Pid::new(0x11).unwrap(), false, 1, &s[183..367])));
        out.extend(a.push(12, &pkt(Pid::new(0x11).unwrap(), false, 2, &s[367..])));
        let ss = sections(&out);
        assert_eq!(ss.len(), 1);
        assert_eq!((ss[0].start_packet, ss[0].end_packet), (10, 12));
        assert_eq!(ss[0].data, s);
        assert_eq!(out.len(), 1, "{out:?}");
    }

    #[test]
    fn spec_2_4_4_two_sections_in_one_packet_then_stuffing() {
        let s1 = sec(0x42, 20);
        let s2 = sec(0x46, 30);
        let mut p = vec![0u8];
        p.extend_from_slice(&s1);
        p.extend_from_slice(&s2);
        let mut a = SectionAssembler::new(Pid::new(0x11).unwrap());
        let out = a.push(0, &pkt(Pid::new(0x11).unwrap(), true, 0, &p));
        let ss = sections(&out);
        assert_eq!(ss.len(), 2);
        assert_eq!(ss[0].data, s1);
        assert_eq!(ss[1].data, s2);
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn spec_2_4_4_2_pointer_field_completes_previous_section() {
        let s1 = sec(0x42, 200); // 212 bytes
        let s2 = sec(0x4E, 10);
        let mut a = SectionAssembler::new(Pid::new(0x12).unwrap());
        let mut p0 = vec![0u8];
        p0.extend_from_slice(&s1[..183]);
        let mut out = a.push(0, &pkt(Pid::new(0x12).unwrap(), true, 5, &p0));
        let rem = &s1[183..];
        let mut p1 = vec![u8::try_from(rem.len()).unwrap()];
        p1.extend_from_slice(rem);
        p1.extend_from_slice(&s2);
        out.extend(a.push(1, &pkt(Pid::new(0x12).unwrap(), true, 6, &p1)));
        let ss = sections(&out);
        assert_eq!(ss.len(), 2);
        assert_eq!(ss[0].data, s1);
        assert_eq!((ss[0].start_packet, ss[0].end_packet), (0, 1));
        assert_eq!(ss[1].data, s2);
        assert_eq!((ss[1].start_packet, ss[1].end_packet), (1, 1));
    }

    #[test]
    fn section_header_split_across_packets() {
        let s1 = sec(0x42, 200);
        let mut p0 = vec![0u8];
        let s0 = sec(0x40, 170); // 182 bytes; leaves 1 byte for s1 header
        p0.extend_from_slice(&s0);
        p0.push(s1[0]);
        let mut a = SectionAssembler::new(Pid::new(0x12).unwrap());
        let mut out = a.push(0, &pkt(Pid::new(0x12).unwrap(), true, 0, &p0));
        out.extend(a.push(1, &pkt(Pid::new(0x12).unwrap(), false, 1, &s1[1..185])));
        out.extend(a.push(2, &pkt(Pid::new(0x12).unwrap(), false, 2, &s1[185..])));
        let ss = sections(&out);
        assert_eq!(ss.len(), 2);
        assert_eq!(ss[1].data, s1);
        assert_eq!(out.len(), 2, "{out:?}");
    }

    #[test]
    fn spec_2_4_3_3_cc_break_discards_partial() {
        let s1 = sec(0x42, 400);
        let s2 = sec(0x42, 10);
        let mut a = SectionAssembler::new(Pid::new(0x11).unwrap());
        let mut p0 = vec![0u8];
        p0.extend_from_slice(&s1[..183]);
        let mut out = a.push(0, &pkt(Pid::new(0x11).unwrap(), true, 0, &p0));
        // packet with cc=1 lost; cc=2 arrives
        out.extend(a.push(2, &pkt(Pid::new(0x11).unwrap(), false, 2, &s1[367..])));
        assert!(sections(&out).is_empty());
        assert!(out.iter().any(|e| matches!(
            e,
            Assembled::Event(DemuxEvent {
                kind: DemuxEventKind::Discarded {
                    reason: DiscardReason::ContinuityBreak,
                    bytes: 183
                },
                ..
            })
        )));
        let mut p3 = vec![0u8];
        p3.extend_from_slice(&s2);
        let out = a.push(3, &pkt(Pid::new(0x11).unwrap(), true, 3, &p3));
        assert_eq!(sections(&out)[0].data, s2);
        // a duplicate packet is ignored
        assert!(
            a.push(4, &pkt(Pid::new(0x11).unwrap(), true, 3, &p3))
                .is_empty()
        );
    }

    #[test]
    fn tei_discards_partial() {
        let s1 = sec(0x42, 400);
        let mut a = SectionAssembler::new(Pid::new(0x11).unwrap());
        let mut p0 = vec![0u8];
        p0.extend_from_slice(&s1[..183]);
        a.push(0, &pkt(Pid::new(0x11).unwrap(), true, 0, &p0));
        let mut bad = pkt(Pid::new(0x11).unwrap(), false, 1, &s1[183..367]);
        bad[1] |= 0x80;
        let out = a.push(1, &bad);
        assert!(matches!(
            &out[..],
            [Assembled::Event(DemuxEvent {
                kind: DemuxEventKind::Discarded {
                    reason: DiscardReason::TransportError,
                    ..
                },
                ..
            })]
        ));
        assert!(
            a.push(2, &pkt(Pid::new(0x11).unwrap(), false, 2, &s1[367..]))
                .is_empty()
        );
    }

    #[test]
    fn spec_2_4_4_2_join_mid_stream_waits_for_pusi() {
        let s1 = sec(0x42, 400);
        let s2 = sec(0x43, 50);
        let mut a = SectionAssembler::new(Pid::new(0x11).unwrap());
        // Joined at the 2nd packet of s1: no PUSI → nothing, no events.
        assert!(
            a.push(0, &pkt(Pid::new(0x11).unwrap(), false, 1, &s1[183..367]))
                .is_empty()
        );
        let mut p = vec![u8::try_from(s1.len() - 367).unwrap()];
        p.extend_from_slice(&s1[367..]);
        p.extend_from_slice(&s2);
        let out = a.push(1, &pkt(Pid::new(0x11).unwrap(), true, 2, &p));
        let ss = sections(&out);
        assert_eq!(ss.len(), 1, "tail of s1 must not be emitted");
        assert_eq!(ss[0].data, s2);
    }

    #[test]
    fn pointer_mismatch_discards() {
        let s1 = sec(0x42, 300);
        let s2 = sec(0x43, 10);
        let mut a = SectionAssembler::new(Pid::new(0x11).unwrap());
        let mut p0 = vec![0u8];
        p0.extend_from_slice(&s1[..183]);
        a.push(0, &pkt(Pid::new(0x11).unwrap(), true, 0, &p0));
        // pointer 0 though s1 still needs bytes
        let mut p1 = vec![0u8];
        p1.extend_from_slice(&s2);
        let out = a.push(1, &pkt(Pid::new(0x11).unwrap(), true, 1, &p1));
        assert!(out.iter().any(|e| matches!(
            e,
            Assembled::Event(DemuxEvent {
                kind: DemuxEventKind::Discarded {
                    reason: DiscardReason::PointerMismatch,
                    ..
                },
                ..
            })
        )));
        assert_eq!(sections(&out)[0].data, s2);
    }
}

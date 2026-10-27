//! Per-PID packet statistics for analyzers: counts of packets,
//! transport_error_indicator (§2.4.3.3), scrambled packets
//! (transport_scrambling_control ≠ '00', Table 2-4), PUSI, adaptation-only
//! packets, PCRs (§2.4.3.4) with packet index, discontinuity indicators and
//! continuity events (§2.4.3.3 via [`crate::cc::CcChecker`]).

use std::collections::BTreeMap;

use crate::cc::{CcChecker, CcEvent};
use crate::packet::{AdaptationFieldControl, PACKET_SIZE, View};
use crate::pcr::Pcr;
use crate::pid::Pid;

/// Counters of one PID (fields of H.222.0 Table 2-2 / Table 2-6).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PidStats {
    /// Packets seen.
    pub packets: u64,
    /// Packets with transport_error_indicator '1'.
    pub tei: u64,
    /// Packets with transport_scrambling_control ≠ '00'.
    pub scrambled: u64,
    /// Packets with payload_unit_start_indicator '1'.
    pub pusi: u64,
    /// Packets with adaptation_field_control '10' (no payload).
    pub adaptation_only: u64,
    /// Packets with discontinuity_indicator '1'.
    pub discontinuity: u64,
    /// Packets with random_access_indicator '1'.
    pub random_access: u64,
    /// Packets whose adaptation field runs past the packet.
    pub malformed: u64,
    /// Continuity events on this PID.
    pub cc_events: u64,
    /// `(packet index, PCR)` of every PCR, in stream order.
    pub pcrs: Vec<(u64, Pcr)>,
}

/// Statistics over a whole stream (H.222.0 §2.4.3; continuity per §2.4.3.3).
#[derive(Debug, Clone, Default)]
pub struct Stats {
    /// Packets seen.
    pub packets: u64,
    /// Per-PID counters.
    pub pids: BTreeMap<Pid, PidStats>,
    /// Every continuity event, in stream order.
    pub cc_events: Vec<CcEvent>,
    cc: CcChecker,
}

impl Stats {
    /// Empty statistics.
    pub fn new() -> Self {
        Self::default()
    }

    /// Account packet `index`.
    pub fn push(&mut self, index: u64, pkt: &[u8; PACKET_SIZE]) {
        self.packets += 1;
        let pid = Pid::from_wire((u16::from(pkt[1] & 0x1F) << 8) | u16::from(pkt[2]));
        let s = self.pids.entry(pid).or_default();
        s.packets += 1;
        let h = crate::packet::Header::decode(pkt).expect("188 bytes hold a header");
        s.tei += u64::from(h.transport_error_indicator);
        s.scrambled += u64::from(h.transport_scrambling_control.is_scrambled());
        s.pusi += u64::from(h.payload_unit_start_indicator);
        s.adaptation_only +=
            u64::from(h.adaptation_field_control == AdaptationFieldControl::AdaptationOnly);
        match View::parse(pkt) {
            Ok(v) => {
                s.discontinuity += u64::from(v.discontinuity_indicator());
                s.random_access += u64::from(v.random_access_indicator());
                if let Some(p) = v.pcr() {
                    s.pcrs.push((index, p));
                }
            }
            Err(_) => s.malformed += 1,
        }
        if let Some(e) = self.cc.push(index, pkt) {
            s.cc_events += 1;
            self.cc_events.push(e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cc::ContinuityCounter;
    use crate::packet::Packet;

    #[test]
    fn counts() {
        let mut st = Stats::new();
        let a = Packet::with_payload(
            Pid::new(0x100).unwrap(),
            true,
            ContinuityCounter::ZERO,
            &[0; 184],
        )
        .unwrap()
        .encode()
        .unwrap();
        let b = Packet::pcr_only(
            Pid::new(0x100).unwrap(),
            ContinuityCounter::ZERO,
            Pcr::from_ticks(27_000_000),
        )
        .encode()
        .unwrap();
        let c = Packet::with_payload(
            Pid::new(0x100).unwrap(),
            false,
            ContinuityCounter::new(2).unwrap(),
            &[0; 184],
        )
        .unwrap()
        .encode()
        .unwrap();
        let mut d = Packet::null().encode().unwrap();
        d[1] |= 0x80;
        for (i, p) in [a, b, c, d].iter().enumerate() {
            st.push(i as u64, p);
        }
        let s = &st.pids[&Pid::new(0x100).unwrap()];
        assert_eq!(
            (s.packets, s.pusi, s.adaptation_only, s.cc_events),
            (3, 1, 1, 1)
        );
        assert_eq!(s.pcrs, vec![(1, Pcr::from_ticks(27_000_000))]);
        assert_eq!(st.pids[&Pid::new(0x1FFF).unwrap()].tei, 1);
        assert_eq!(st.cc_events.len(), 1);
    }
}

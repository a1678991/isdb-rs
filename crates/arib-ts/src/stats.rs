//! Per-PID packet statistics for analyzers: counts of packets,
//! transport_error_indicator (§2.4.3.3), scrambled packets
//! (transport_scrambling_control ≠ '00', Table 2-4), PUSI, adaptation-only
//! packets, PCRs (§2.4.3.4) with packet index, discontinuity indicators and
//! continuity events (§2.4.3.3 via [`crate::cc::CcChecker`]).

use std::collections::BTreeMap;

use crate::cc::{CcChecker, CcEvent};
use crate::packet::{PACKET_SIZE, View};
use crate::pcr::Pcr;

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
    pub pids: BTreeMap<u16, PidStats>,
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
        let pid = (((pkt[1] & 0x1F) as u16) << 8) | pkt[2] as u16;
        let s = self.pids.entry(pid).or_default();
        s.packets += 1;
        let h = crate::packet::Header::decode(pkt).expect("188 bytes hold a header");
        s.tei += h.transport_error_indicator as u64;
        s.scrambled += (h.transport_scrambling_control != 0) as u64;
        s.pusi += h.payload_unit_start_indicator as u64;
        s.adaptation_only +=
            (h.adaptation_field_control == crate::packet::afc::ADAPTATION_ONLY) as u64;
        match View::parse(pkt) {
            Ok(v) => {
                s.discontinuity += v.discontinuity_indicator() as u64;
                s.random_access += v.random_access_indicator() as u64;
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
    use crate::packet::Packet;

    #[test]
    fn counts() {
        let mut st = Stats::new();
        let a = Packet::with_payload(0x100, true, 0, &[0; 184])
            .unwrap()
            .encode()
            .unwrap();
        let b = Packet::pcr_only(0x100, 0, Pcr::from_ticks(27_000_000))
            .encode()
            .unwrap();
        let c = Packet::with_payload(0x100, false, 2, &[0; 184])
            .unwrap()
            .encode()
            .unwrap();
        let mut d = Packet::null().encode().unwrap();
        d[1] |= 0x80;
        for (i, p) in [a, b, c, d].iter().enumerate() {
            st.push(i as u64, p);
        }
        let s = &st.pids[&0x100];
        assert_eq!(
            (s.packets, s.pusi, s.adaptation_only, s.cc_events),
            (3, 1, 1, 1)
        );
        assert_eq!(s.pcrs, vec![(1, Pcr::from_ticks(27_000_000))]);
        assert_eq!(st.pids[&0x1FFF].tei, 1);
        assert_eq!(st.cc_events.len(), 1);
    }
}

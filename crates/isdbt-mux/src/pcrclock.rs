//! Packet arrival times of a recorded transport stream, interpolated from
//! its PCR (H.222.0 §2.4.2.2).
//!
//! H.222.0 §2.4.2.2: "The value encoded in the PCR field indicates the
//! time t(i), where i is the index of the byte containing the last bit of
//! the program_clock_reference_base field", and eq. 2-4 gives the arrival
//! time of every other byte by linear interpolation between two PCRs:
//! `t(i) = PCR(i″)/system_clock_frequency + (i − i″)/transport_rate(i)`.
//! [`PcrClock`] applies this per packet: the packets between two PCR
//! packets of the reference PID get times spaced evenly between the two
//! PCR values (a file recorded from a partial TS has lost the packets of
//! other services, so the interpolation spreads the remaining ones evenly —
//! the rate is the program's own between each PCR pair).
//!
//! The times are on a *continuous* 27 MHz time line starting at 0 at the
//! first PCR: PCR wrap-around (2^33 · 300, ≈ 26.5 h) is unwrapped, and a
//! discontinuity (discontinuity_indicator, §2.4.3.5, or a PCR step outside
//! `0 < Δ ≤` [`MAX_PCR_GAP`], e.g. a looped or spliced file) continues the
//! time line at the rate of the previous PCR interval instead of jumping.
//! Every packet carries the [`Anchor`] (raw PCR ↔ continuous time) of the
//! last PCR before it, so the PTS/DTS it carries (which refer to the time
//! base of that PCR) map onto the same continuous time line.

use isdb_ts::Pid;
use std::collections::VecDeque;

use isdb_ts::packet::View;
use isdb_ts::pcr::{pcr_signed_diff, pcr_wrap_diff};

/// System clock frequency (H.222.0 §2.4.2.1), Hz.
pub const HZ: u64 = 27_000_000;
/// Largest PCR step taken as continuous: H.222.0 §2.7.2 requires PCRs at
/// most 0.1 s apart; steps up to 0.7 s (a damaged recording) are still
/// interpolated, anything larger is a discontinuity.
pub const MAX_PCR_GAP: u64 = HZ * 7 / 10;
/// Ticks per packet assumed before two PCRs have been seen (20 Mbit/s).
const DEFAULT_TICKS_PER_PACKET: f64 = 188.0 * 8.0 * HZ as f64 / 20e6;
/// Packets held while waiting for the next PCR before they are timed by
/// extrapolation (≈ 37 MB: a stream that long without PCR is treated as
/// having none).
const MAX_PENDING: usize = 200_000;

/// PCR of a raw packet with the discontinuity_indicator, if its adaptation
/// field carries one (H.222.0 Table 2-6).
pub fn packet_pcr(p: &[u8; 188]) -> Option<(u64, bool)> {
    let v = View::parse(p).ok()?;
    Some((v.pcr()?.ticks(), v.discontinuity_indicator()))
}

/// PID of a raw packet.
pub fn packet_pid(p: &[u8; 188]) -> Pid {
    Pid::from_wire((u16::from(p[1] & 0x1F) << 8) | u16::from(p[2]))
}

/// Correspondence of a raw PCR value and the continuous time line, valid
/// for the time stamps that follow that PCR (until the next discontinuity).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Anchor {
    /// Continuous time (27 MHz ticks) of the PCR.
    pub t: u64,
    /// Raw PCR value (27 MHz ticks modulo 2^33 · 300).
    pub pcr: u64,
}

impl Anchor {
    /// Continuous time of a raw 27 MHz value of the same time base (wrap
    /// handled: the nearest representative within ±13 h).
    pub fn map27(&self, raw: u64) -> i64 {
        self.t as i64 + pcr_signed_diff(raw % isdb_ts::pcr::PCR_MODULUS, self.pcr)
    }

    /// Continuous time (27 MHz) of a 90 kHz PTS/DTS (33 bits).
    pub fn map90(&self, ts: u64) -> i64 {
        self.map27((ts % isdb_ts::pcr::BASE_MODULUS) * 300)
    }
}

/// An item with its arrival time.
#[derive(Debug, Clone)]
pub struct Timed<T> {
    /// Continuous arrival time (27 MHz ticks; 0 = first PCR).
    pub t: u64,
    /// Anchor of the last PCR before the packet (`None` before the first).
    pub anchor: Option<Anchor>,
    /// The item pushed with the packet.
    pub item: T,
}

/// Counters.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ClockStats {
    /// PCRs used.
    pub pcrs: u64,
    /// Discontinuities bridged (indicator or out-of-range step).
    pub discontinuities: u64,
    /// Packets timed by extrapolation (before the first / after the last PCR).
    pub extrapolated: u64,
}

/// Assigns arrival times to packets pushed in stream order; see the module
/// documentation. Items come out of [`PcrClock::pop`] once the next PCR
/// (or [`PcrClock::finish`]) has fixed their time.
#[derive(Debug)]
pub struct PcrClock<T> {
    pid: Option<Pid>,
    pending: VecDeque<(Option<Anchor>, T)>,
    ready: VecDeque<Timed<T>>,
    anchor: Option<Anchor>,
    ticks_per_packet: f64,
    measured: bool,
    stats: ClockStats,
}

impl<T> PcrClock<T> {
    /// New clock on the PCRs of `pid`, or of the first PID carrying a PCR.
    pub fn new(pid: Option<Pid>) -> Self {
        Self {
            pid,
            pending: VecDeque::new(),
            ready: VecDeque::new(),
            anchor: None,
            ticks_per_packet: DEFAULT_TICKS_PER_PACKET,
            measured: false,
            stats: ClockStats::default(),
        }
    }

    /// Reference PCR PID (once known).
    pub fn pcr_pid(&self) -> Option<Pid> {
        self.pid
    }

    /// Counters so far.
    pub fn stats(&self) -> ClockStats {
        self.stats
    }

    /// Rate of the last PCR interval (bit/s of 188-byte packets), once two
    /// PCRs were seen.
    pub fn rate_bps(&self) -> Option<f64> {
        self.measured
            .then(|| 188.0 * 8.0 * HZ as f64 / self.ticks_per_packet)
    }

    /// Anchor of the last PCR seen.
    pub fn anchor(&self) -> Option<Anchor> {
        self.anchor
    }

    /// Packets waiting for the next PCR.
    pub fn pending(&self) -> usize {
        self.pending.len()
    }

    /// Push the next packet of the stream with an item to time.
    pub fn push(&mut self, p: &[u8; 188], item: T) {
        let pcr = packet_pcr(p).filter(|_| self.pid.is_none_or(|x| x == packet_pid(p)));
        let Some((pcr, disc)) = pcr else {
            self.pending.push_back((self.anchor, item));
            if self.pending.len() >= MAX_PENDING {
                self.extrapolate();
            }
            return;
        };
        self.pid.get_or_insert(packet_pid(p));
        self.stats.pcrs += 1;
        let n = self.pending.len() as u64 + 1; // packets since the last PCR packet, this one included
        let t_new = match self.anchor {
            None => {
                // First PCR: packets before it are timed backwards at the
                // default rate (they precede the time line's origin, so
                // they get 0).
                self.stats.extrapolated += self.pending.len() as u64;
                for (a, it) in self.pending.drain(..) {
                    self.ready.push_back(Timed {
                        t: 0,
                        anchor: a,
                        item: it,
                    });
                }
                0
            }
            Some(a) => {
                let d = pcr_wrap_diff(pcr, a.pcr);
                let t_new = if !disc && d > 0 && d <= MAX_PCR_GAP {
                    self.ticks_per_packet = d as f64 / n as f64;
                    self.measured = true;
                    a.t + d
                } else {
                    self.stats.discontinuities += 1;
                    a.t + (n as f64 * self.ticks_per_packet).round().max(1.0) as u64
                };
                for (i, (anc, it)) in self.pending.drain(..).enumerate() {
                    let k = i as u64 + 1;
                    self.ready.push_back(Timed {
                        t: a.t + (t_new - a.t) * k / n,
                        anchor: anc,
                        item: it,
                    });
                }
                t_new
            }
        };
        let anchor = Anchor { t: t_new, pcr };
        self.anchor = Some(anchor);
        self.ready.push_back(Timed {
            t: t_new,
            anchor: Some(anchor),
            item,
        });
    }

    fn extrapolate(&mut self) {
        let base = self.anchor.map_or(0, |a| a.t);
        let pending: Vec<_> = self.pending.drain(..).collect();
        self.stats.extrapolated += pending.len() as u64;
        let mut last = base;
        for (i, (anc, it)) in pending.into_iter().enumerate() {
            last = base + ((i as f64 + 1.0) * self.ticks_per_packet).round() as u64;
            self.ready.push_back(Timed {
                t: last,
                anchor: anc,
                item: it,
            });
        }
        // Continue the time line from the last extrapolated packet.
        if let Some(a) = self.anchor.as_mut() {
            let adv = last - a.t;
            a.t = last;
            a.pcr = isdb_ts::pcr::pcr_wrap_add(a.pcr, adv);
        }
    }

    /// End of stream: time the packets after the last PCR by extrapolation
    /// at the last measured rate.
    pub fn finish(&mut self) {
        self.extrapolate();
    }

    /// Next timed item in stream order.
    pub fn pop(&mut self) -> Option<Timed<T>> {
        self.ready.pop_front()
    }

    /// Time of the next item ready to pop.
    pub fn peek_time(&self) -> Option<u64> {
        self.ready.front().map(|x| x.t)
    }

    /// Items ready to pop.
    pub fn ready(&self) -> usize {
        self.ready.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use isdb_ts::pcr::{PCR_MODULUS, Pcr};

    fn pcr_pkt(pid: u16, ticks: u64, disc: bool) -> [u8; 188] {
        let pid = Pid::new(pid).unwrap();
        let mut f = isdb_ts::packet::AdaptationFields {
            pcr: Some(Pcr::from_ticks(ticks)),
            ..Default::default()
        };
        f.discontinuity_indicator = disc;
        isdb_ts::packet::Packet::with_adaptation(
            pid,
            false,
            isdb_ts::ContinuityCounter::ZERO,
            Some(f),
            &[],
        )
        .unwrap()
        .encode()
        .unwrap()
    }

    fn data_pkt(pid: u16) -> [u8; 188] {
        isdb_ts::packet::Packet::with_payload(
            Pid::new(pid).unwrap(),
            false,
            isdb_ts::ContinuityCounter::ZERO,
            &[0u8; 184],
        )
        .unwrap()
        .encode()
        .unwrap()
    }

    fn run(pkts: &[[u8; 188]]) -> (Vec<u64>, ClockStats) {
        let mut c = PcrClock::new(None);
        let mut out = vec![];
        for (i, p) in pkts.iter().enumerate() {
            c.push(p, i);
            while let Some(x) = c.pop() {
                assert_eq!(x.item, out.len());
                out.push(x.t);
            }
        }
        c.finish();
        while let Some(x) = c.pop() {
            out.push(x.t);
        }
        (out, c.stats())
    }

    #[test]
    fn spec_eq_2_4_interpolation() {
        // PCR, 3 packets, PCR 4000 ticks later: 1000 ticks per packet.
        let p = [
            pcr_pkt(0x100, 50_000, false),
            data_pkt(0x200),
            data_pkt(0x200),
            data_pkt(0x200),
            pcr_pkt(0x100, 54_000, false),
            data_pkt(0x200),
        ];
        let (t, st) = run(&p);
        assert_eq!(t, vec![0, 1000, 2000, 3000, 4000, 5000]);
        assert_eq!(st.discontinuities, 0);
        assert_eq!(st.extrapolated, 1);
    }

    #[test]
    fn wrap_is_continuous_and_anchor_maps_timestamps() {
        let near = PCR_MODULUS - 1500;
        let p = [
            pcr_pkt(0x100, near, false),
            data_pkt(0x200),
            pcr_pkt(0x100, 1500, false), // wrapped: +3000 ticks
        ];
        let mut c = PcrClock::new(Some(Pid::new(0x100).unwrap()));
        for (i, x) in p.iter().enumerate() {
            c.push(x, i);
        }
        let v: Vec<_> = std::iter::from_fn(|| c.pop()).collect();
        assert_eq!(
            v.iter().map(|x| x.t).collect::<Vec<_>>(),
            vec![0, 1500, 3000]
        );
        // A PTS 0.1 s after the wrapped PCR maps 0.1 s later on the line.
        let a = v[2].anchor.unwrap();
        assert_eq!(a.map90(1500 / 300 + 9000), 3000 + 2_700_000);
        // and one just before the wrap maps before it
        let pts_before = (near / 300) - 90;
        assert_eq!(
            a.map90(pts_before),
            3000 + (pts_before * 300) as i64 - 1500 - PCR_MODULUS as i64
        );
    }

    #[test]
    fn discontinuity_continues_at_previous_rate() {
        let p = [
            pcr_pkt(0x100, 1_000_000, false),
            data_pkt(0x200),
            pcr_pkt(0x100, 1_002_000, false), // 1000 ticks/packet
            data_pkt(0x200),
            pcr_pkt(0x100, 77, true), // new time base (loop)
            data_pkt(0x200),
            pcr_pkt(0x100, 2077, false),
        ];
        let (t, st) = run(&p);
        assert_eq!(t, vec![0, 1000, 2000, 3000, 4000, 5000, 6000]);
        assert_eq!(st.discontinuities, 1);
        // a backwards step without indicator is a discontinuity too
        let p = [
            pcr_pkt(0x100, 10_000_000, false),
            data_pkt(0x200),
            pcr_pkt(0x100, 10_002_000, false),
            pcr_pkt(0x100, 5, false),
        ];
        let (t, st) = run(&p);
        assert_eq!(t, vec![0, 1000, 2000, 3000]);
        assert_eq!(st.discontinuities, 1);
    }

    #[test]
    fn other_pcr_pids_are_ignored_once_locked() {
        let p = [
            pcr_pkt(0x100, 0, false),
            pcr_pkt(0x101, 999_999, false),
            pcr_pkt(0x100, 2000, false),
        ];
        let (t, _) = run(&p);
        assert_eq!(t, vec![0, 1000, 2000]);
    }
}

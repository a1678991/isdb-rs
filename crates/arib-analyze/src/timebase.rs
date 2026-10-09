//! Packet index → time, from the PCRs of one PID (H.222.0 §2.4.2.2
//! eq. 2-4: byte arrival times are interpolated linearly between PCRs).
//!
//! Interpolating between PCR samples keeps times right even when packets
//! were removed (e.g. a capture with null packets stripped), because the
//! PCR values themselves carry the elapsed time.  Without PCRs a nominal
//! bit rate may be supplied; otherwise times are unknown.

use arib_ts::pcr::{PCR_MODULUS, pcr_wrap_diff};
use arib_ts::stats::Stats;

/// Packet-index → seconds mapping.
#[derive(Debug, Clone, Default)]
pub struct Timebase {
    /// PID whose PCRs define the mapping.
    pub pcr_pid: Option<u16>,
    /// (packet index, seconds since first sample), increasing.
    samples: Vec<(u64, f64)>,
    /// Fallback constant rate (bits/s) when there are no PCRs.
    pub nominal_bps: Option<f64>,
    /// PCR jumps treated as discontinuities (> 5 s or backwards).
    pub discontinuities: u64,
    /// PCR value (27 MHz ticks) of the first sample.
    pub t0_ticks: u64,
}

impl Timebase {
    /// Build from the PCRs of `pid` (or the PID with most PCRs if `None`).
    pub fn from_stats(stats: &Stats, pid: Option<u16>, nominal_bps: Option<f64>) -> Self {
        let pid = pid
            .filter(|p| stats.pids.get(p).is_some_and(|s| s.pcrs.len() >= 2))
            .or_else(|| {
                stats
                    .pids
                    .iter()
                    .filter(|(_, s)| s.pcrs.len() >= 2)
                    .max_by_key(|(_, s)| s.pcrs.len())
                    .map(|(p, _)| *p)
            });
        let mut tb = Timebase {
            pcr_pid: pid,
            nominal_bps,
            ..Default::default()
        };
        let Some(pid) = pid else { return tb };
        let pcrs = &stats.pids[&pid].pcrs;
        let mut t = 0f64;
        let mut prev: Option<(u64, u64)> = None;
        let mut per_packet = None::<f64>;
        for &(idx, p) in pcrs {
            let ticks = p.ticks() % PCR_MODULUS;
            if let Some((pi, pt)) = prev {
                if idx <= pi {
                    continue;
                }
                let d = pcr_wrap_diff(ticks, pt);
                let dt = d as f64 / 27e6;
                if dt > 5.0 {
                    tb.discontinuities += 1;
                    t += per_packet.unwrap_or(0.0) * (idx - pi) as f64;
                } else {
                    t += dt;
                    per_packet = Some(t / (idx - tb.samples[0].0).max(1) as f64);
                }
            }
            if tb.samples.is_empty() {
                tb.t0_ticks = ticks;
            }
            tb.samples.push((idx, t));
            prev = Some((idx, ticks));
        }
        tb
    }

    /// Interpolated PCR value (27 MHz ticks, modulo 2^33·300) at packet
    /// `idx` (H.222.0 eq. 2-4), if PCRs are available.
    pub fn pcr_at(&self, idx: u64) -> Option<u64> {
        if self.samples.len() < 2 {
            return None;
        }
        let t = self.time(idx)?;
        let d = (t * 27e6).round();
        let m = PCR_MODULUS as f64;
        let v = (self.t0_ticks as f64 + d).rem_euclid(m);
        Some(v as u64 % PCR_MODULUS)
    }

    /// The (packet index, seconds) samples.
    pub fn samples(&self) -> &[(u64, f64)] {
        &self.samples
    }

    /// Whether times are available.
    pub fn known(&self) -> bool {
        self.samples.len() >= 2 || self.nominal_bps.is_some()
    }

    /// Seconds at packet `idx` (relative to the first PCR, or to packet 0
    /// with a nominal rate).
    pub fn time(&self, idx: u64) -> Option<f64> {
        if self.samples.len() < 2 {
            return self.nominal_bps.map(|r| idx as f64 * 188.0 * 8.0 / r);
        }
        let s = &self.samples;
        let i = s.partition_point(|&(x, _)| x <= idx);
        let (a, b) = if i == 0 {
            (s[0], s[1])
        } else if i >= s.len() {
            (s[s.len() - 2], s[s.len() - 1])
        } else {
            (s[i - 1], s[i])
        };
        let span = (b.0 - a.0) as f64;
        if span == 0.0 {
            return Some(a.1);
        }
        Some(a.1 + (idx as f64 - a.0 as f64) * (b.1 - a.1) / span)
    }

    /// First packet index at or after time `t` (inverse of [`Self::time`]).
    pub fn index_at(&self, t: f64) -> Option<u64> {
        if self.samples.len() < 2 {
            return self
                .nominal_bps
                .map(|r| (t * r / (188.0 * 8.0)).ceil().max(0.0) as u64);
        }
        let s = &self.samples;
        let i = s.partition_point(|&(_, x)| x < t);
        let (a, b) = if i == 0 {
            (s[0], s[1])
        } else if i >= s.len() {
            (s[s.len() - 2], s[s.len() - 1])
        } else {
            (s[i - 1], s[i])
        };
        if b.1 == a.1 {
            return Some(a.0);
        }
        let x = a.0 as f64 + (t - a.1) * (b.0 - a.0) as f64 / (b.1 - a.1);
        Some(x.ceil().max(0.0) as u64)
    }

    /// Seconds covered by `packets` packets (from packet 0 to the last).
    pub fn duration(&self, packets: u64) -> Option<f64> {
        if packets == 0 {
            return Some(0.0);
        }
        Some(
            self.time(packets - 1)? - self.time(0)?
                + self.packet_seconds(packets - 1).unwrap_or(0.0),
        )
    }

    /// Approximate duration of one packet around `idx`.
    pub fn packet_seconds(&self, idx: u64) -> Option<f64> {
        Some(self.time(idx + 1)? - self.time(idx)?)
    }

    /// Mean transport rate in bits/s implied by the PCR samples
    /// (packets × 188 × 8 / elapsed PCR time).
    pub fn rate_bps(&self) -> Option<f64> {
        if self.samples.len() < 2 {
            return self.nominal_bps;
        }
        let (a, b) = (self.samples[0], *self.samples.last().unwrap());
        (b.1 > a.1).then(|| (b.0 - a.0) as f64 * 188.0 * 8.0 / (b.1 - a.1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arib_ts::packet::Packet;
    use arib_ts::pcr::Pcr;

    #[test]
    fn interpolates_between_pcrs_and_inverts() {
        let mut st = Stats::new();
        // PCR every 10 packets, 1 ms apart; a wrap in the middle.
        let base = PCR_MODULUS - 27_000 * 3;
        for i in 0..100u64 {
            let p = if i % 10 == 0 {
                Packet::pcr_only(0x100, 0, Pcr::from_ticks(base + i / 10 * 27_000))
                    .encode()
                    .unwrap()
            } else {
                Packet::null().encode().unwrap()
            };
            st.push(i, &p);
        }
        let tb = Timebase::from_stats(&st, None, None);
        assert_eq!(tb.pcr_pid, Some(0x100));
        assert!((tb.time(0).unwrap()).abs() < 1e-12);
        assert!((tb.time(55).unwrap() - 0.0055).abs() < 1e-9);
        assert!((tb.time(95).unwrap() - 0.0095).abs() < 1e-9); // extrapolated
        assert_eq!(tb.index_at(0.0055), Some(55));
        assert!((tb.rate_bps().unwrap() - 90.0 * 1504.0 / 0.009).abs() < 1.0);
    }
}

//! The rate a `ts-program` relay needs, measured over the whole recording.
//!
//! The start-up probe ([`crate::relay::probe`]) reads only the first
//! `probe_seconds` of the source. A recording's rate is not uniform (the
//! tail of the previous programme, commercials, scene changes), so its
//! mean over that window can be well below what the rest of the file
//! needs. The relay keeps the source's pace and never re-times
//! ([`crate::relay`]): on a layer that carries less than the source's
//! sustained rate it falls further behind its PCR schedule for as long as
//! the rate stays above the capacity, and its PES arrive after their
//! decoding time — audio first, since its arrival→decode lead is the
//! shortest. Receivers drop such access units.
//!
//! [`scan`] reads the whole file once and returns a [`RateProfile`]. Its
//! [`RateProfile::required_bps`] is the smallest constant rate at which the
//! relayed packets, served first-in first-out in source order (as the
//! relay sends them), are each delayed by at most the time left until
//! the decoding time of their PES (DTS, else PTS; H.222.0 §2.4.2.3), plus
//! [`TOLERANCE_MS`]. A packet with no decoding time (ECM / EMM, carousel
//! sections, PES without PTS) may be delayed by up to [`UNTIMED_MS`]. The
//! time line is binned by [`BIN_MS`].

use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use isdb_ts::reader::StreamReader;

use crate::error::{MuxError, Result};
use crate::pcrclock::{HZ, PcrClock, packet_pid};

/// Width of a time bin (ms of source PCR time).
pub const BIN_MS: u64 = 1;
/// Delay a relayed packet may take beyond its PES's decoding time: the
/// relay's slot jitter, as [`crate::relay::RETIME_HOLD`].
pub const TOLERANCE_MS: f64 = 2.0;
/// Largest delay of a packet that has no decoding time (H.222.0 §2.4.2.6
/// bounds a PES's bytes to 1 s ahead of its decoding).
pub const UNTIMED_MS: f64 = 1000.0;
/// Precision of [`RateProfile::required_bps`] (bit/s).
const PRECISION_BPS: f64 = 1000.0;
const TICKS_PER_BIN: u64 = HZ / 1000 * BIN_MS;
/// Slack value of a bin without timed packets.
const NO_SLACK: i16 = i16::MAX;

/// A relayed source PID for [`scan`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ScanPid {
    /// Source PID.
    pub pid: u16,
    /// Its packets carry PES whose PTS/DTS give each packet a deadline.
    pub pes: bool,
}

/// Rates of the relayed packets over the whole source file.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RateProfile {
    /// PCR time covered (s).
    pub span_s: f64,
    /// Relayed packets.
    pub packets: u64,
    /// Mean rate (bit/s of 188-byte packets).
    pub mean_bps: f64,
    /// Largest rate over a 1 s window (bit/s).
    pub peak_1s_bps: f64,
    /// Smallest constant rate that delivers every relayed packet in time
    /// (bit/s; see the module documentation).
    pub required_bps: f64,
    /// Shortest arrival→decoding distance of a relayed packet (s), if any
    /// packet had a decoding time.
    pub min_lead_s: Option<f64>,
}

impl RateProfile {
    /// One-line description for the probe report.
    pub fn describe(&self) -> String {
        format!(
            "whole file: {:.1} s of PCR, {:.2} Mbit/s mean, {:.2} Mbit/s peak (1 s); needs {:.2} Mbit/s to deliver every packet by its decoding time (shortest arrival→decode lead {})",
            self.span_s,
            self.mean_bps / 1e6,
            self.peak_1s_bps / 1e6,
            self.required_bps / 1e6,
            self.min_lead_s
                .map_or("none".to_string(), |s| format!("{:.3} s", s))
        )
    }
}

/// Per-bin packet counts and tightest deadlines of a source.
#[derive(Debug, Clone, Default)]
pub struct Bins {
    /// Relayed packets per bin.
    pub packets: Vec<u16>,
    /// Smallest (decoding time − arrival) of a timed packet per bin, in
    /// whole ms rounded down ([`i16::MAX`] = no timed packet).
    pub slack_ms: Vec<i16>,
}

impl Bins {
    fn add(&mut self, t: u64, slack_ticks: Option<i64>) {
        let k = (t / TICKS_PER_BIN) as usize;
        if k >= self.packets.len() {
            self.packets.resize(k + 1, 0);
            self.slack_ms.resize(k + 1, NO_SLACK);
        }
        self.packets[k] = self.packets[k].saturating_add(1);
        if let Some(s) = slack_ticks {
            let ms = s.div_euclid((HZ / 1000) as i64);
            let ms = ms.clamp(i16::MIN as i64, NO_SLACK as i64 - 1) as i16;
            self.slack_ms[k] = self.slack_ms[k].min(ms);
        }
    }

    /// The profile of these bins.
    pub fn profile(&self) -> RateProfile {
        let bin_s = BIN_MS as f64 / 1000.0;
        let packets: u64 = self.packets.iter().map(|&n| n as u64).sum();
        let span_s = self.packets.len() as f64 * bin_s;
        let mean_bps = if span_s > 0.0 {
            packets as f64 * 1504.0 / span_s
        } else {
            0.0
        };
        let w = (1000 / BIN_MS) as usize;
        let peak_1s_bps = if self.packets.len() < w {
            mean_bps
        } else {
            let mut sum: u64 = self.packets[..w].iter().map(|&n| n as u64).sum();
            let mut best = sum;
            for k in w..self.packets.len() {
                sum = sum + self.packets[k] as u64 - self.packets[k - w] as u64;
                best = best.max(sum);
            }
            best as f64 * 1504.0
        };
        let min_lead_s = self
            .slack_ms
            .iter()
            .filter(|&&s| s != NO_SLACK)
            .min()
            .map(|&s| s as f64 / 1000.0);
        RateProfile {
            span_s,
            packets,
            mean_bps,
            peak_1s_bps,
            required_bps: self.required_bps(),
            min_lead_s,
        }
    }

    /// Whether a FIFO server of `bps` delivers every packet in time.
    pub fn feasible(&self, bps: f64) -> bool {
        let bin_s = BIN_MS as f64 / 1000.0;
        let drain = bps * bin_s;
        let mut queue = 0.0f64; // bits waiting at the start of the bin, its own included
        for (&n, &s) in self.packets.iter().zip(&self.slack_ms) {
            queue = (queue - drain).max(0.0) + n as f64 * 1504.0;
            if n == 0 {
                continue;
            }
            let delay_ms = queue / bps * 1000.0;
            let allowed = if s == NO_SLACK {
                UNTIMED_MS
            } else {
                s.max(0) as f64 + TOLERANCE_MS
            };
            if delay_ms > allowed {
                return false;
            }
        }
        true
    }

    /// Smallest feasible rate (bit/s, to 1 kbit/s). A rate that
    /// sends the fullest bin within one bin is always feasible, so the
    /// search is bounded.
    pub fn required_bps(&self) -> f64 {
        let max = self.packets.iter().copied().max().unwrap_or(0);
        if max == 0 {
            return 0.0;
        }
        let (mut lo, mut hi) = (0.0f64, max as f64 * 1504.0 * 1000.0 / BIN_MS as f64);
        while hi - lo > PRECISION_BPS {
            let mid = (lo + hi) / 2.0;
            if self.feasible(mid) {
                hi = mid;
            } else {
                lo = mid;
            }
        }
        hi
    }
}

/// Read the whole of `path` and bin the packets of `pids`, timed by the
/// PCRs of `pcr_pid` ([`PcrClock`]).
pub fn bin_file(path: &Path, pcr_pid: u16, pids: &[ScanPid]) -> Result<Bins> {
    let media = |msg: String| MuxError::Media {
        path: path.display().to_string(),
        msg,
    };
    let f = std::fs::File::open(path).map_err(|e| media(e.to_string()))?;
    let mut r = StreamReader::new(BufReader::with_capacity(1 << 20, f));
    let mut slot = vec![u16::MAX; 8192];
    for (i, p) in pids.iter().enumerate() {
        slot[p.pid as usize & 0x1FFF] = i as u16;
    }
    // per relayed PID: the decoding time (27 MHz, continuous) of the PES
    // being received
    let mut decode: Vec<Option<i64>> = vec![None; pids.len()];
    // item: (slot, PTS/DTS of a PES start: Some(Some(ts)), a start without
    // one: Some(None), a continuation: None)
    let mut clock = PcrClock::<(u16, Option<Option<u64>>)>::new(Some(pcr_pid));
    let mut bins = Bins::default();
    let mut drain = |clock: &mut PcrClock<(u16, Option<Option<u64>>)>, bins: &mut Bins| {
        while let Some(x) = clock.pop() {
            let (s, start) = x.item;
            if s == u16::MAX {
                continue;
            }
            let s = s as usize;
            if let Some(ts) = start {
                decode[s] = ts.zip(x.anchor).map(|(v, a)| a.map90(v));
            }
            let slack = decode[s].map(|d| d - x.t as i64);
            bins.add(x.t, slack);
        }
    };
    while let Some(p) = r.next_packet().map_err(MuxError::Io)? {
        let p = *p.packet();
        let s = slot[packet_pid(&p) as usize];
        let start =
            (s != u16::MAX && pids[s as usize].pes && p[1] & 0x40 != 0).then(|| pes_decode_ts(&p));
        clock.push(&p, (s, start));
        drain(&mut clock, &mut bins);
    }
    clock.finish();
    drain(&mut clock, &mut bins);
    Ok(bins)
}

/// DTS, else PTS, of the PES header starting in this packet's payload.
fn pes_decode_ts(p: &[u8; 188]) -> Option<u64> {
    let afc = (p[3] >> 4) & 3;
    if afc & 1 == 0 {
        return None;
    }
    let start = if afc & 2 != 0 { 5 + p[4] as usize } else { 4 };
    let pl = p.get(start..)?;
    if pl.len() < 19 || pl[..3] != [0, 0, 1] || !isdb_ts::pes::has_optional_header(pl[3]) {
        return None;
    }
    let ts = |b: &[u8]| {
        ((b[0] as u64 >> 1) & 7) << 30
            | (b[1] as u64) << 22
            | ((b[2] as u64) >> 1) << 15
            | (b[3] as u64) << 7
            | (b[4] as u64) >> 1
    };
    match pl[7] >> 6 {
        3 => Some(ts(&pl[14..19])),
        2 => Some(ts(&pl[9..14])),
        _ => None,
    }
}

type CacheKey = (PathBuf, u64, Option<SystemTime>, u16, Vec<ScanPid>);

/// Profiles already measured in this process: building a multiplexer
/// twice (`--auto-layout` retries with another layout) reads the file once.
static CACHE: Mutex<Vec<(CacheKey, RateProfile)>> = Mutex::new(Vec::new());

/// [`RateProfile`] of the packets of `pids` over the whole of `path`
/// (memoised per file, size and modification time).
pub fn scan(path: &Path, pcr_pid: u16, pids: &[ScanPid]) -> Result<RateProfile> {
    let meta = std::fs::metadata(path).map_err(|e| MuxError::Media {
        path: path.display().to_string(),
        msg: e.to_string(),
    })?;
    let key: CacheKey = (
        std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf()),
        meta.len(),
        meta.modified().ok(),
        pcr_pid,
        pids.to_vec(),
    );
    if let Some((_, p)) = CACHE
        .lock()
        .ok()
        .and_then(|c| c.iter().find(|x| x.0 == key).cloned())
    {
        return Ok(p);
    }
    let p = bin_file(path, pcr_pid, pids)?.profile();
    if let Ok(mut c) = CACHE.lock() {
        c.push((key, p));
    }
    Ok(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `n` packets per bin for `ms` bins, each with `slack` ms of lead.
    fn constant(bins: &mut Bins, n: u16, ms: usize, slack: i16) {
        bins.packets.extend(std::iter::repeat_n(n, ms));
        bins.slack_ms.extend(std::iter::repeat_n(slack, ms));
    }

    fn mbps(n: u16) -> f64 {
        n as f64 * 1504.0 * 1000.0 / BIN_MS as f64
    }

    #[test]
    fn constant_rate_needs_its_own_rate() {
        let mut b = Bins::default();
        constant(&mut b, 12, 10_000, 100);
        let p = b.profile();
        assert!((p.mean_bps - mbps(12)).abs() < 1.0, "{p:?}");
        assert!((p.peak_1s_bps - mbps(12)).abs() < 1.0, "{p:?}");
        // a lead absorbs a slightly slower server only for a while: over
        // 10 s, 100 ms of lead allows 1 % less
        assert!(p.required_bps <= mbps(12), "{p:?}");
        assert!(p.required_bps >= mbps(12) * 0.985, "{p:?}");
        assert_eq!(p.min_lead_s, Some(0.1));
    }

    #[test]
    fn a_light_start_does_not_hide_a_heavy_rest() {
        // 30 s at 12 packets/ms, then 120 s at 13: the relay needs 13
        let mut b = Bins::default();
        constant(&mut b, 12, 30_000, 150);
        constant(&mut b, 13, 120_000, 150);
        let p = b.profile();
        let need = p.required_bps;
        assert!(need > mbps(13) * 0.998 && need <= mbps(13), "{need}");
        assert!(!b.feasible(mbps(13) * 0.99));
    }

    #[test]
    fn a_short_burst_is_absorbed_by_the_lead() {
        // 50 ms at twice the rate, with 200 ms of lead: a rate a little
        // above the base delivers the burst in time
        let mut b = Bins::default();
        constant(&mut b, 10, 5_000, 200);
        constant(&mut b, 20, 50, 200);
        constant(&mut b, 10, 5_000, 200);
        let need = b.required_bps();
        assert!(need < mbps(11), "{need}");
        // with no lead, the burst's own rate is needed
        let mut tight = Bins::default();
        constant(&mut tight, 10, 5_000, 0);
        constant(&mut tight, 20, 50, 0);
        constant(&mut tight, 10, 5_000, 0);
        assert!(tight.required_bps() > mbps(19), "{}", tight.required_bps());
    }

    #[test]
    fn untimed_packets_may_wait_a_second() {
        let mut b = Bins::default();
        constant(&mut b, 10, 1_000, NO_SLACK);
        constant(&mut b, 0, 2_000, NO_SLACK);
        // 1 s of 10 packets/ms may be sent over 2 s
        let need = b.required_bps();
        assert!(need < mbps(6) && need > mbps(4), "{need}");
    }

    #[test]
    fn late_source_packets_count_as_no_lead() {
        let mut b = Bins::default();
        constant(&mut b, 4, 100, -50);
        assert!(b.required_bps() <= mbps(4) * 1.01);
        assert!(b.required_bps() > 0.0);
        assert_eq!(b.profile().min_lead_s, Some(-0.05));
    }
}

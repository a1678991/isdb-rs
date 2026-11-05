//! The exact clock of the decoded media, per asset: the MPTs' timestamp
//! descriptors resolved to the 90 kHz PTS/DTS of MPEG-TS.
//!
//! The descriptor clock (B60 §7.4.3.5, §7.4.3.35): the *MPU timestamp*
//! names the NTP presentation time `PT` of the MPU's first access unit
//! in **presentation** order; the *MPU extended timestamp* names, per
//! MPU, a `decoding_time_offset` (the first AU's decode before `PT`) and,
//! per access unit in **decode** order, a `(dts_pts_offset, pts_offset)`
//! pair in timescale ticks. Access unit *i* of the MPU then has
//!
//! ```text
//! DTS(i) = PT − decoding_time_offset + Σ_{k<i} pts_offset(k)
//! PTS(i) = DTS(i) + dts_pts_offset(i)
//! ```
//!
//! — the pts offset is the AU's *duration*, which steps the decode clock.
//! On the BS recordings (timescale 180000) the video's rows repeat the
//! cycle `30030 15014 6006 0 3004 9008 3004 6006` with `pts_offset` 3003
//! (1/59.94 s) and `decoding_time_offset` 9010: the PTSs of a cycle land
//! on display frames `7 3 1 0 2 5 4 6` (a CRA/P then its hierarchical
//! B pictures), whose earliest is `PT` itself. The AAC's rows are
//! `(0, 3840)` (1024 samples at 48 kHz) with offset 0: DTS = PTS.
//!
//! An MPT version only covers the current and the next few MPUs, so the
//! [tracker](crate::signal::AssetTiming) accumulates them; an MPU it never
//! saw falls back to the nearest row and anchor (the patterns are
//! constant on air, and the anchor steps by a row's total duration).

use crate::signal::AssetTiming;

/// The MPEG-TS system clock base (PTS/DTS/PCR base units per second).
const TS_HZ: i128 = 90_000;
/// The 33-bit PTS/DTS field.
pub const TS_MASK: u64 = 0x1_ffff_ffff;
/// How far (in MPUs) an anchor or row is extrapolated to an MPU the
/// tracker did not see.
const MAX_EXTRAPOLATION: u32 = 64;

/// The `(pts, dts)` of access unit `index` (decode order) of `mpu`, in
/// 90 kHz ticks modulo 2^33; `None` when the descriptors do not time it
/// (no timescale yet, no anchor or row near the MPU, an index past the
/// row's AUs).
#[must_use]
pub fn au_times(t: &AssetTiming, mpu: u32, index: usize) -> Option<(u64, u64)> {
    let (pts, dts, scale) = au_units(t, mpu, index)?;
    Some((to_90k(pts, scale), to_90k(dts, scale)))
}

/// The presentation time of access unit `index` (decode order) of `mpu` as
/// the NTP timestamp the descriptors state it in; `None` as [`au_times`].
#[must_use]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the NTP timestamp's own 64-bit ring"
)]
pub fn au_ntp(t: &AssetTiming, mpu: u32, index: usize) -> Option<u64> {
    let (pts, _, scale) = au_units(t, mpu, index)?;
    Some(pts.div_euclid(scale) as u64)
}

/// One instant on both of the recorder's clocks: the unwrapped 90 kHz PTS
/// (whose 33 bits say nothing of the date) and the NTP time it stands for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Clock {
    /// The unwrapped PTS.
    pub ticks: i64,
    /// Its NTP timestamp.
    pub ntp: u64,
}

impl Clock {
    /// The unwrapped PTS of the whole NTP second `seconds`, the nearest way
    /// round the NTP ring (136 years), so an NTP era change costs nothing.
    #[must_use]
    #[allow(
        clippy::cast_possible_wrap,
        clippy::cast_possible_truncation,
        reason = "a signed distance on the ring; ±68 years of ticks fit i64"
    )]
    pub fn ticks_at(self, seconds: u32) -> i64 {
        let distance = (u64::from(seconds) << 32).wrapping_sub(self.ntp) as i64;
        let ticks = (i128::from(distance) * TS_HZ + (1 << 31)) >> 32;
        self.ticks + ticks as i64
    }
}

/// The `(pts, dts, scale)` of an access unit, the times in units of
/// 1/(2^32 · `scale`) s.
fn au_units(t: &AssetTiming, mpu: u32, index: usize) -> Option<(i128, i128, i128)> {
    let scale = i128::from(t.timescale.filter(|&s| s != 0)?);
    let row = nearest(&t.rows, mpu)?.1;
    let offsets = row
        .access_units
        .iter()
        .map(|&(dpo, po)| po.map(|po| (i128::from(dpo), i128::from(po))))
        .collect::<Option<Vec<_>>>()?;
    let &(dts_pts, _) = offsets.get(index)?;
    let before: i128 = offsets[..index].iter().map(|&(_, po)| po).sum();
    // The anchor in units of 1/(2^32 · timescale) s, extrapolated by
    // whole row durations when the MPU itself was never announced.
    let (anchor_mpu, &ntp) = nearest(&t.mpu_times, mpu)?;
    let steps = i128::from(mpu.wrapping_sub(anchor_mpu) as i32);
    let duration: i128 = offsets.iter().map(|&(_, po)| po).sum();
    let anchor = i128::from(ntp) * scale + ((steps * duration) << 32);
    let dts = anchor + ((before - i128::from(row.decoding_time_offset)) << 32);
    let pts = dts + (dts_pts << 32);
    Some((pts, dts, scale))
}

/// The number of access units of `mpu` (its row's, or the nearest row's
/// within 64 MPUs on the sequence-number ring).
#[must_use]
pub fn au_count(t: &AssetTiming, mpu: u32) -> Option<usize> {
    Some(nearest(&t.rows, mpu)?.1.access_units.len())
}

/// The mean access unit duration of `mpu`'s row (a frame of the video)
/// in 90 kHz ticks.
#[must_use]
#[allow(clippy::cast_precision_loss, reason = "a row's few ticks")]
pub fn frame_ticks(t: &AssetTiming, mpu: u32) -> Option<f64> {
    let scale = t.timescale.filter(|&s| s != 0)?;
    let row = nearest(&t.rows, mpu)?.1;
    let total: u64 = row
        .access_units
        .iter()
        .map(|&(_, po)| po.map(u64::from))
        .sum::<Option<u64>>()?;
    let n = row.access_units.len();
    (n > 0).then(|| total as f64 / n as f64 * TS_HZ as f64 / f64::from(scale))
}

/// The entry for `mpu`, else the nearest one on the sequence-number ring
/// within [`MAX_EXTRAPOLATION`].
fn nearest<V>(m: &std::collections::BTreeMap<u32, V>, mpu: u32) -> Option<(u32, &V)> {
    if let Some(v) = m.get(&mpu) {
        return Some((mpu, v));
    }
    m.iter()
        .map(|(&k, v)| (k, v, mpu.wrapping_sub(k).min(k.wrapping_sub(mpu))))
        .filter(|&(.., d)| d <= MAX_EXTRAPOLATION)
        .min_by_key(|&(.., d)| d)
        .map(|(k, v, _)| (k, v))
}

/// A time in 1/(2^32 · `scale`) s to rounded 90 kHz ticks, 33 bits.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "masked to 33 bits"
)]
fn to_90k(units: i128, scale: i128) -> u64 {
    let den = scale << 32;
    let ticks = (units * TS_HZ + den / 2).div_euclid(den);
    (ticks as u64) & TS_MASK
}

#[cfg(test)]
mod tests {
    use super::*;
    use isdb_mmt::desc::MpuExtended;

    /// The BS7 video's on-air row: the 8-picture cycle ×4.
    fn video(pt: u64) -> AssetTiming {
        let cycle = [30030u16, 15014, 6006, 0, 3004, 9008, 3004, 6006];
        let mut t = AssetTiming {
            timescale: Some(180_000),
            ..AssetTiming::default()
        };
        t.mpu_times.insert(100, pt);
        t.rows.insert(
            100,
            MpuExtended {
                mpu_sequence_number: 100,
                leap_indicator: 0,
                decoding_time_offset: 9010,
                access_units: cycle
                    .iter()
                    .cycle()
                    .take(32)
                    .map(|&d| (d, Some(3003)))
                    .collect(),
            },
        );
        t
    }

    #[test]
    fn video_cycle_presents_in_display_order() {
        // PT = 1000 s exactly: 90 000 000 ticks.
        let t = video(1000 << 32);
        let times: Vec<_> = (0..32).map(|i| au_times(&t, 100, i).unwrap()).collect();
        // DTS strictly increasing by one frame (1501.5 ticks, rounded).
        for w in times.windows(2) {
            let step = w[1].1 - w[0].1;
            assert!(step == 1501 || step == 1502, "dts step {step}");
        }
        // The PTSs are the 32 display frames from PT on (±1 tick of the
        // encoder's 180 kHz rounding), each exactly once.
        let mut frames: Vec<u64> = times
            .iter()
            .map(|&(p, _)| ((p - 90_000_000) * 2 + 1501) / 3003)
            .collect();
        frames.sort_unstable();
        assert_eq!(frames, (0..32).collect::<Vec<_>>());
        assert_eq!(times[3].0, 90_000_000, "decode 3 is the first displayed");
        assert_eq!(times[0].0 - times[0].1, 15015, "the CRA's reorder delay");
        assert!(times.iter().all(|&(p, d)| p >= d));
    }

    #[test]
    fn uncovered_mpu_extrapolates_by_row_duration() {
        let t = video(1000 << 32);
        // Two MPUs on: 2 × 32 × 3003 / 180000 s later.
        let (p, _) = au_times(&t, 102, 3).unwrap();
        assert_eq!(p, 90_000_000 + 2 * 32 * 1501 + 32);
        assert_eq!(au_times(&t, 100 + MAX_EXTRAPOLATION + 1, 0), None);
        assert_eq!(au_times(&t, 100, 32), None);
        assert_eq!(au_count(&t, 102), Some(32));
        assert_eq!(frame_ticks(&t, 102), Some(1501.5));
        assert_eq!(au_count(&t, 100 + MAX_EXTRAPOLATION + 1), None);
    }

    #[test]
    fn audio_rows_have_dts_equal_pts() {
        let mut t = AssetTiming {
            timescale: Some(180_000),
            ..AssetTiming::default()
        };
        t.mpu_times.insert(7, 5 << 32);
        t.rows.insert(
            7,
            MpuExtended {
                mpu_sequence_number: 7,
                leap_indicator: 0,
                decoding_time_offset: 0,
                access_units: vec![(0, Some(3840)); 4],
            },
        );
        for i in 0..4 {
            let (p, d) = au_times(&t, 7, i).unwrap();
            assert_eq!(p, d);
            assert_eq!(p, 450_000 + 1920 * i as u64);
        }
    }

    #[test]
    fn au_ntp_is_the_time_au_times_wraps() {
        let t = video(4_001_996_504u64 << 32);
        for i in 0..32 {
            let ntp = au_ntp(&t, 100, i).unwrap();
            let (pts, _) = au_times(&t, 100, i).unwrap();
            let ticks = (i128::from(ntp) * TS_HZ) >> 32;
            let ring = u64::try_from(ticks & i128::from(TS_MASK)).unwrap();
            let diff = ring.wrapping_sub(pts) & TS_MASK;
            assert!(diff <= 1 || diff == TS_MASK, "{i}: off by {diff}");
        }
        // The first displayed picture is `PT`, but for the encoder's one
        // 180 kHz tick of rounding (dts 9009 − 9010, offset 0).
        assert_eq!(
            au_ntp(&t, 100, 3),
            Some((4_001_996_504u64 << 32) - (1u64 << 32) / 180_000 - 1)
        );
    }

    #[test]
    fn clock_places_seconds_beyond_the_33_bit_ring() {
        // Half a second past a whole NTP second.
        let c = Clock {
            ticks: 1_000,
            ntp: (4_001_996_504u64 << 32) | (1 << 31),
        };
        assert_eq!(c.ticks_at(4_001_996_505), 1_000 + 45_000);
        assert_eq!(c.ticks_at(4_001_996_504), 1_000 - 45_000);
        // Two days on: past the ring of ≈ 26.5 h.
        assert_eq!(
            c.ticks_at(4_001_996_504 + 172_800),
            1_000 - 45_000 + 172_800 * 90_000
        );
    }

    #[test]
    fn clock_crosses_the_ntp_era() {
        let c = Clock {
            ticks: 0,
            ntp: u64::from(u32::MAX) << 32,
        };
        assert_eq!(c.ticks_at(9), 10 * 90_000);
    }

    #[test]
    fn wraps_at_33_bits() {
        // NTP seconds far past 2^33 / 90000 (≈ 26.5 h).
        let t = video(4_001_996_504u64 << 32);
        let (p, d) = au_times(&t, 100, 0).unwrap();
        assert!(p <= TS_MASK && d <= TS_MASK);
    }
}

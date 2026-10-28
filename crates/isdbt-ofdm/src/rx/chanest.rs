//! Pilot-based channel estimation for coherent segments (general DSP on
//! the ARIB STD-B31 pilot grid of §3.12.2/§3.13).
//!
//! Scattered pilots sit at `c ≡ 3·(n mod 4) (mod 12)` of every coherent
//! segment.  Since C (108/216/432) is a multiple of 12, in band-carrier
//! numbers the SP of symbol phase `p` are `k ≡ 3p (mod 12)` and each
//! carrier `k ≡ 0 (mod 3)` of a coherent segment carries an SP every 4
//! symbols (phase `(k/3) mod 4`).  The estimator
//!
//! 1. interpolates in time: least-squares line through the `2w` nearest
//!    SP occurrences of each grid carrier (`w = 1`: linear interpolation
//!    between the two nearest, 3 symbols of look-ahead);
//! 2. uses the continual pilot that closes each coherent segment at its
//!    upper edge — carrier 0 of a differential neighbour (§3.12.1: "the CP
//!    … serves as the SP of a coherent modulation's segment") or the band
//!    edge carrier (§3.14) — averaged over a symmetric window;
//! 3. interpolates in frequency between grid carriers (spacing 3) with a
//!    4-point (cubic) Lagrange interpolator inside each run of contiguous
//!    coherent segments (linear at run edges), after removing the mean
//!    phase slope (timing offset) of the grid.

use crate::C32;
use crate::frame::{FrameLayout, SegmentKind, pilot_value};
use isdbt_core::SEGMENTS;
use isdbt_core::ofdm_params::segment_at_position;

/// A grid carrier whose estimate comes from a continual pilot.
#[derive(Debug, Clone, Copy)]
struct ContinualGrid {
    k: usize,
}

/// Channel estimator for one layout.
#[derive(Debug, Clone)]
pub struct ChannelEstimator {
    /// For each coherent segment: (first carrier k0, C).
    segments: Vec<usize>,
    c: usize,
    /// Grid carriers at a coherent segment's upper edge that are continual.
    continual: Vec<ContinualGrid>,
    /// Runs of contiguous coherent segments: (first grid index, last grid
    /// index inclusive), grid index = k/3.
    runs: Vec<(usize, usize)>,
    /// Inverse pilot values 1/P_k for every band carrier (BPSK ±4/3).
    inv_pilot: Vec<f32>,
    /// Grid values for the current symbol (indexed by k/3 within band).
    grid: Vec<C32>,
    /// Output channel estimate per band carrier.
    pub h: Vec<C32>,
}

impl ChannelEstimator {
    /// Estimator for `layout`.
    pub fn new(layout: &FrameLayout) -> Self {
        let c = layout.mode.carriers_per_segment();
        let k_total = layout.k_total;
        let mut segments = Vec::new();
        let mut continual = Vec::new();
        for pos in 0..SEGMENTS {
            let seg = segment_at_position(pos);
            if layout.kinds[seg] != SegmentKind::Coherent {
                continue;
            }
            let k0 = pos * c;
            segments.push(k0);
            let upper = k0 + c;
            let upper_is_sp = pos + 1 < SEGMENTS
                && layout.kinds[segment_at_position(pos + 1)] == SegmentKind::Coherent;
            if !upper_is_sp {
                continual.push(ContinualGrid { k: upper });
            }
        }
        let mut runs: Vec<(usize, usize)> = Vec::new();
        for &k0 in &segments {
            let (a, b) = (k0 / 3, (k0 + c) / 3);
            match runs.last_mut() {
                Some(r) if r.1 == a => r.1 = b,
                _ => runs.push((a, b)),
            }
        }
        let inv_pilot = layout.w.iter().map(|&w| 1.0 / pilot_value(w).re).collect();
        Self {
            segments,
            c,
            continual,
            runs,
            inv_pilot,
            grid: vec![C32::default(); k_total / 3 + 2],
            h: vec![C32::default(); k_total],
        }
    }

    /// True if there is no coherent segment.
    pub fn is_empty(&self) -> bool {
        self.segments.is_empty()
    }

    /// Estimate the channel of symbol `t`.
    ///
    /// `current` is the spectrum of symbol `t`, `ring(d)` the spectrum of
    /// symbol `t + d` if available (|d| ≤ [`lookahead`]`(w)`; `ring(0)` is
    /// `current`); `phase_t` is the SP phase (frame symbol
    /// index mod 4) of symbol `t`.  `w` ≥ 1 is the number of SP
    /// occurrences used on each side: each grid carrier's estimate is the
    /// least-squares straight line through its `2w` nearest SP
    /// occurrences evaluated at `t` (`w = 1`: linear interpolation between
    /// the two nearest; larger `w` trades Doppler tolerance for noise).
    pub fn estimate<'a>(
        &mut self,
        current: &[C32],
        ring: impl Fn(i32) -> Option<&'a [C32]>,
        phase_t: usize,
        w: usize,
    ) {
        if self.segments.is_empty() {
            return;
        }
        let y0 = current;
        let c = self.c;
        // `w` is DemodConfig::sp_time_window, a handful of symbols (the ring
        // holds 2·lookahead(w) + 1 of them), so it fits an i32.
        #[allow(clippy::cast_possible_truncation)]
        let w = w.max(1) as i32;
        // 1. SP grid with time interpolation.
        for &k0 in &self.segments {
            for g in (k0..k0 + c).step_by(3) {
                let p = (g / 3) % 4;
                // Offset back to the latest SP occurrence (0..=3).
                let back: i32 = [0, 1, 2, 3][(phase_t + 4 - p) % 4];
                let pts = (-(w - 1)..=w).filter_map(|i| {
                    let d = -back + 4 * i;
                    ring(d).map(|y| (d as f32, y[g] * self.inv_pilot[g]))
                });
                self.grid[g / 3] = ls_line_at_zero(pts).unwrap_or(y0[g]);
            }
        }
        // 2. Continual pilots closing a coherent segment (CP of a
        //    differential neighbour or the band edge): mean over the
        //    symmetric window ±(2w−1) symbols.
        let span = 2 * w - 1;
        for cg in &self.continual {
            let k = cg.k;
            let pts = (-span..=span)
                .filter_map(|d| ring(d).map(|y| (d as f32, y[k] * self.inv_pilot[k])));
            self.grid[k / 3] = ls_line_at_zero(pts).unwrap_or(y0[k]);
        }
        // 3. Mean phase slope over grid pairs, removed before interpolation.
        let mut acc = C32::default();
        for &(a, b) in &self.runs {
            for i in a..b {
                acc += self.grid[i + 1] * self.grid[i].conj();
            }
        }
        let slope = acc.arg();
        for &(a, b) in &self.runs {
            // Derotate the run: G'[i] = G[i]·e^{−jσ(i−a)}.
            let step = C32::from_polar(1.0, -slope);
            let mut r = C32::new(1.0, 0.0);
            let g: Vec<C32> = (a..=b)
                .map(|i| {
                    let v = self.grid[i] * r;
                    r *= step;
                    v
                })
                .collect();
            let last = g.len() - 1;
            let sub = C32::from_polar(1.0, slope / 3.0);
            for (i, &gi) in g.iter().enumerate() {
                let k = (a + i) * 3;
                if k < self.h.len() {
                    self.h[k] = self.grid[a + i];
                }
                if i == last {
                    break;
                }
                // Back-rotation for the sub-carriers of interval i.
                let mut rot = C32::from_polar(1.0, slope * i as f32);
                for u in 1..3 {
                    rot *= sub;
                    let x = u as f32 / 3.0;
                    let v = if i >= 1 && i + 2 <= last {
                        let (p0, p1, p2, p3) = (g[i - 1], gi, g[i + 1], g[i + 2]);
                        p0 * (-x * (x - 1.0) * (x - 2.0) / 6.0)
                            + p1 * ((x + 1.0) * (x - 1.0) * (x - 2.0) / 2.0)
                            + p2 * (-(x + 1.0) * x * (x - 2.0) / 2.0)
                            + p3 * ((x + 1.0) * x * (x - 1.0) / 6.0)
                    } else {
                        gi * (1.0 - x) + g[i + 1] * x
                    };
                    self.h[k + u] = v * rot;
                }
            }
        }
    }
}

/// Symbols of look-ahead (and look-back) needed by
/// [`ChannelEstimator::estimate`] with `w` occurrences per side.
pub fn lookahead(w: usize) -> usize {
    4 * w.max(1) - 1
}

/// Least-squares straight line through `(x, v)` evaluated at x = 0
/// (single point: its value).
fn ls_line_at_zero(pts: impl Iterator<Item = (f32, C32)>) -> Option<C32> {
    let pts: Vec<(f32, C32)> = pts.collect();
    match pts.len() {
        0 => None,
        1 => Some(pts[0].1),
        n => {
            let nf = n as f32;
            let xm = pts.iter().map(|p| p.0).sum::<f32>() / nf;
            let vm = pts.iter().map(|p| p.1).sum::<C32>() / nf;
            let sxx: f32 = pts.iter().map(|p| (p.0 - xm) * (p.0 - xm)).sum();
            if sxx <= 0.0 {
                return Some(vm);
            }
            let sxv: C32 = pts.iter().map(|p| (p.1 - vm) * (p.0 - xm)).sum();
            Some(vm + sxv / sxx * (0.0 - xm))
        }
    }
}

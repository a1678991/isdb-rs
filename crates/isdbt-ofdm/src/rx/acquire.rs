//! Acquisition: mode / guard-interval detection, symbol timing and
//! fractional CFO from guard-interval autocorrelation; integer CFO from
//! the continual (AC1 + band-edge) carriers; segment-kind detection; TMCC
//! frame synchronisation.  ISDB-T specific positions come from
//! [`crate::frame`] (Tables 3-14/3-15) and [`crate::tmcc`] (Table 3-20).

use crate::C32;
use crate::frame::{FrameLayout, SegmentKind, ac1_positions, ac2_positions, tmcc_positions};
use crate::ofdm::carrier_bin;
use isdbt_core::ofdm_params::segment_position;
use isdbt_core::{GuardInterval, Mode, SEGMENTS};

/// Result of guard-interval correlation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ModeGuess {
    /// Mode.
    pub mode: Mode,
    /// Guard interval.
    pub guard: GuardInterval,
    /// Offset in the analysed buffer of a guard-interval start (0 ≤ offset < symbol length).
    pub offset: usize,
    /// Fractional CFO in carrier spacings, (−0.5, 0.5].
    pub cfo_frac: f64,
    /// Normalised correlation (≈ SNR/(1+SNR) for the right candidate).
    pub metric: f64,
}

/// All modes.
pub const MODES: [Mode; 3] = [Mode::M1, Mode::M2, Mode::M3];
/// All guard intervals.
pub const GUARDS: [GuardInterval; 4] = [
    GuardInterval::G1_4,
    GuardInterval::G1_8,
    GuardInterval::G1_16,
    GuardInterval::G1_32,
];

/// Samples needed by [`detect_mode`] to test every candidate with at
/// least `symbols` symbols.
pub fn samples_needed(modes: &[Mode], guards: &[GuardInterval], symbols: usize) -> usize {
    modes
        .iter()
        .flat_map(|m| {
            guards
                .iter()
                .map(move |g| (symbols + 1) * (m.fft_size() + g.samples(*m)) + m.fft_size())
        })
        .max()
        .unwrap_or(0)
}

/// Guard-interval autocorrelation over `buf` for each (mode, guard)
/// candidate: `Γ(d) = Σ_s Σ_{m<Ng} x[d+sP+m]·x*[d+sP+m+N]` folded over the
/// symbol period `P = N + Ng`, normalised by the mean energy.  The
/// candidate with the largest `|Γ|/Φ` wins; `d` at the maximum is a GI
/// start and the fractional CFO is `−arg Γ / 2π` (the useful part's tail
/// equals the GI rotated by `2π·ε`).
pub fn detect_mode(buf: &[C32], modes: &[Mode], guards: &[GuardInterval]) -> Option<ModeGuess> {
    let mut best: Option<ModeGuess> = None;
    for &mode in modes {
        let n = mode.fft_size();
        if buf.len() <= n {
            continue;
        }
        let len = buf.len() - n;
        // Prefix sums of the lag-N product and energy.
        let mut cp = Vec::with_capacity(len + 1);
        let mut ce = Vec::with_capacity(len + 1);
        let (mut sp, mut se) = (num_complex::Complex64::default(), 0f64);
        cp.push(sp);
        ce.push(se);
        for m in 0..len {
            let a = buf[m];
            let b = buf[m + n];
            let p = a * b.conj();
            sp += num_complex::Complex64::new(p.re as f64, p.im as f64);
            se += (a.norm_sqr() + b.norm_sqr()) as f64 * 0.5;
            cp.push(sp);
            ce.push(se);
        }
        for &guard in guards {
            let ng = guard.samples(mode);
            let p = n + ng;
            if len < ng + 4 * p {
                continue;
            }
            let s_count = (len - ng) / p;
            for d in 0..p {
                let mut g = num_complex::Complex64::default();
                let mut e = 0f64;
                for s in 0..s_count {
                    let a = d + s * p;
                    if a + ng > len {
                        break;
                    }
                    g += cp[a + ng] - cp[a];
                    e += ce[a + ng] - ce[a];
                }
                if e <= 0.0 {
                    continue;
                }
                let metric = g.norm() / e;
                if best.is_none_or(|b| metric > b.metric) {
                    best = Some(ModeGuess {
                        mode,
                        guard,
                        offset: d,
                        cfo_frac: -g.arg() / (2.0 * std::f64::consts::PI),
                        metric,
                    });
                }
            }
        }
    }
    best
}

/// Continual-carrier coherence per bin: `|Σ_j p_j²| / Σ_j |p_j|²` with
/// `p_j = X_j·X*_{j−1}`.  Squaring removes the DBPSK ±1 of TMCC/AC, so
/// continual carriers score ≈ 1 and data carriers ≈ 1/√J.
pub fn continual_metric(spectra: &[Vec<C32>]) -> Vec<f32> {
    let nb = spectra[0].len();
    let mut num = vec![C32::default(); nb];
    let mut den = vec![0f32; nb];
    for w in spectra.windows(2) {
        for b in 0..nb {
            let p = w[1][b] * w[0][b].conj();
            num[b] += p * p;
            den[b] += p.norm_sqr();
        }
    }
    num.iter()
        .zip(&den)
        .map(|(n, &d)| if d > 0.0 { n.norm() / d } else { 0.0 })
        .collect()
}

/// Integer CFO in carrier spacings from full-band FFT spectra (natural
/// bin order, N bins): correlate the continual metric with the AC1
/// positions of all 13 segments (identical for both segment kinds, §3.12)
/// plus the band-edge carrier, over shifts `−max..=max`.
pub fn integer_cfo(mode: Mode, spectra: &[Vec<C32>], max_shift: usize) -> (i64, f32) {
    let n = mode.fft_size() as i64;
    let metric = continual_metric(spectra);
    let c = mode.carriers_per_segment();
    let mut pattern: Vec<i64> = Vec::new();
    for seg in 0..SEGMENTS {
        let base = segment_position(seg) * c;
        for &p in ac1_positions(mode, seg, SegmentKind::Coherent) {
            pattern.push(carrier_bin(mode, base + p as usize) as i64);
        }
    }
    pattern.push(carrier_bin(mode, mode.total_carriers() - 1) as i64);
    let mut best = (0i64, f32::MIN);
    let max = max_shift as i64;
    for d in -max..=max {
        let score: f32 = pattern
            .iter()
            .map(|&b| metric[(b + d).rem_euclid(n) as usize])
            .sum();
        if score > best.1 {
            best = (d, score);
        }
    }
    (best.0, best.1 / pattern.len() as f32)
}

/// Segment kinds from the continual metric of band-carrier spectra (K
/// carriers): a segment is differential if the positions that are
/// continual only in a differential segment (CP, AC2, differential TMCC;
/// Table 3-14) are coherent over time.
pub fn detect_kinds(
    mode: Mode,
    spectra: &[Vec<C32>],
) -> ([SegmentKind; SEGMENTS], [f32; SEGMENTS]) {
    let metric = continual_metric(spectra);
    let c = mode.carriers_per_segment();
    let mut kinds = [SegmentKind::Coherent; SEGMENTS];
    let mut scores = [0f32; SEGMENTS];
    for seg in 0..SEGMENTS {
        let base = segment_position(seg) * c;
        let excl: Vec<u16> = tmcc_positions(mode, seg, SegmentKind::Coherent)
            .iter()
            .chain(ac1_positions(mode, seg, SegmentKind::Coherent))
            .copied()
            .collect();
        let pos: Vec<u16> = std::iter::once(0u16)
            .chain(
                ac2_positions(mode, seg, SegmentKind::Differential)
                    .iter()
                    .copied(),
            )
            .chain(
                tmcc_positions(mode, seg, SegmentKind::Differential)
                    .iter()
                    .copied(),
            )
            .filter(|p| !excl.contains(p))
            .collect();
        let s = pos.iter().map(|&p| metric[base + p as usize]).sum::<f32>() / pos.len() as f32;
        scores[seg] = s;
        if s > 0.5 {
            kinds[seg] = SegmentKind::Differential;
        }
    }
    (kinds, scores)
}

/// Soft DBPSK value of the TMCC carriers between two consecutive
/// symbols: `Σ_c Re(y_j[c]·y*_{j−1}[c])`, optionally restricted to one
/// segment kind (negative ⇒ bit 1).
pub fn tmcc_soft(
    layout: &FrameLayout,
    y: &[C32],
    y_prev: &[C32],
    kind: Option<SegmentKind>,
) -> f32 {
    layout
        .tmcc
        .iter()
        .filter(|t| kind.is_none_or(|k| t.kind == k))
        .map(|t| (y[t.k as usize] * y_prev[t.k as usize].conj()).re)
        .sum()
}

/// Scattered-pilot phase detection (§3.12.2: SP at `c ≡ 3·(n mod 4) mod
/// 12`).  For every symbol `j` and hypothesis `p`, the products of
/// adjacent SPs (12 carriers apart) after removing the known BPSK values
/// add coherently only for the right `p`.  Returns `c` with
/// `n mod 4 = (j + c) mod 4` for the stored symbols and the fraction of
/// symbols agreeing, or `None` without coherent segments.
pub fn detect_sp_phase(layout: &FrameLayout, spectra: &[Vec<C32>]) -> Option<(usize, f32)> {
    if layout.sp[0].is_empty() {
        return None;
    }
    let mut votes = [0usize; 4];
    for (j, y) in spectra.iter().enumerate() {
        let mut best = (0usize, -1f32);
        for p in 0..4 {
            let sp = &layout.sp[p];
            let mut acc = C32::default();
            let mut energy = 0f32;
            for w in sp.windows(2) {
                let (a, b) = (w[0] as usize, w[1] as usize);
                if b != a + 12 {
                    continue;
                }
                let va = y[a] * crate::frame::pilot_value(layout.w[a]).re;
                let vb = y[b] * crate::frame::pilot_value(layout.w[b]).re;
                acc += vb * va.conj();
                energy += va.norm() * vb.norm();
            }
            let m = if energy > 0.0 { acc.re / energy } else { 0.0 };
            if m > best.1 {
                best = (p, m);
            }
        }
        // n mod 4 = p = (j + c) mod 4  ⇒  c = (p − j) mod 4.
        votes[(best.0 + 4 - j % 4) % 4] += 1;
    }
    let (c, n) = votes
        .iter()
        .enumerate()
        .max_by_key(|e| e.1)
        .map(|(c, &n)| (c, n))?;
    Some((c, n as f32 / spectra.len().max(1) as f32))
}

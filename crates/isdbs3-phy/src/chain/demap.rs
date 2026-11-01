//! Stage 6: the max-log demapper with the constellation and noise of the
//! previous frame (module docs of [`crate::chain`], "Stage 6").

use super::fx::{div_round, round_shift, sat127};
use isdbs3_core::constellation::points;
use isdbs3_core::params::{CodeRate, Modulation, PILOT_SYMBOLS};
use isdbs3_core::slots::Slot;

/// Fractional bits of a point and a symbol (Q.10, 1.0 = the sync
/// amplitude, as stage 3's output).
pub const POINT_FRAC: u32 = 10;
/// Shift of the LLR scale `K = round(2^LLR_SHIFT / n0)`.
pub const LLR_SHIFT: u32 = 32;
/// Smallest n0 the scale is computed for (Q.20: 256 = 2.4·10⁻⁴, 36 dB).
pub const N0_MIN: i64 = 256;
/// n0 before the first estimate: `round(2²⁰ · 10^(−13/10))` (Es/N0
/// 13 dB, the lab signal's sync Es/N0 before equalisation).
pub const N0_DEFAULT: i64 = 52_556;
/// Every this many symbols of a codeword (from its first) is a noise
/// sample (`rx::noise_of`).
pub const NOISE_STEP: usize = 16;

/// The five modulations in [`modulation_index`] order.
pub const MODULATIONS: [Modulation; 5] = [
    Modulation::Bpsk,
    Modulation::Qpsk,
    Modulation::Psk8,
    Modulation::Apsk16,
    Modulation::Apsk32,
];

/// Index of `m` in [`MODULATIONS`] (bits − 1).
pub fn modulation_index(m: Modulation) -> usize {
    m.bits() - 1
}

/// The nominal points of `m` at rate `r` (§3.8) in Q.10: `round(1 024 ·
/// p)` per component, label order.
pub fn nominal(m: Modulation, r: CodeRate) -> Vec<[i16; 2]> {
    let q = |v: f64| -> i16 {
        // |p| < 2 (unit power), so 1 024·p fits i16
        #[allow(clippy::cast_possible_truncation)]
        let x = (v * f64::from(1u32 << POINT_FRAC)).round() as i16;
        x
    };
    points(m, r)
        .into_iter()
        .map(|(i, q_)| [q(i), q(q_)])
        .collect()
}

/// The demapper's metric of point `p` for symbol `y`: `|p|² − 2(y_i p_i +
/// y_q p_q)` (= |y − p|² − |y|², Q.20; |y| ≤ 2¹⁵, |p| < 2¹¹: 30 bits
/// signed).
pub fn metric(y: [i16; 2], p: [i16; 2]) -> i64 {
    let (yi, yq) = (i64::from(y[0]), i64::from(y[1]));
    let (pi, pq) = (i64::from(p[0]), i64::from(p[1]));
    pi * pi + pq * pq - 2 * (yi * pi + yq * pq)
}

/// The LLRs of `y` against `pts` (label order, `bits` per label) into
/// `out[0..bits]`, the label's MSB first: for each bit b, `Δ = min_{label
/// bit 1} m − min_{label bit 0} m` and `sat127(round_shift(Δ · k, 32))`.
/// Returns the smallest metric (for the noise estimate).
pub fn demap(y: [i16; 2], pts: &[[i16; 2]], bits: usize, k: i64, out: &mut [i8]) -> i64 {
    let mut m = [0i64; 32];
    let mut best = i64::MAX;
    for (mi, &p) in m.iter_mut().zip(pts) {
        *mi = metric(y, p);
        best = best.min(*mi);
    }
    for (o, b) in out[..bits].iter_mut().zip((0..bits).rev()) {
        let (mut m0, mut m1) = (i64::MAX, i64::MAX);
        for (lab, &mi) in m[..pts.len()].iter().enumerate() {
            if (lab >> b) & 1 == 0 {
                m0 = m0.min(mi);
            } else {
                m1 = m1.min(mi);
            }
        }
        *o = sat127(round_shift((m1 - m0) * k, LLR_SHIFT));
    }
    best
}

/// The LLR scale of noise `n0` (Q.20): `round(2³² / max(n0, 256))`
/// (the frame's one reciprocal; 24 bits).
pub fn scale_of(n0: i64) -> i64 {
    div_round(1i64 << LLR_SHIFT, n0.max(N0_MIN))
}

/// The pilot and noise accumulators of one frame, and what they give the
/// next.
#[derive(Debug, Clone)]
pub struct Learn {
    /// Per modulation, per label: Σ pilot (Q.10, sign removed), 32 bits
    /// signed per component, and the count.
    sum: [Vec<[i64; 2]>; 5],
    count: [Vec<u32>; 5],
    /// Σ (|y|² + min metric) over the noise samples (Q.20, 48 bits) and
    /// their number.
    noise: i64,
    samples: u32,
}

impl Default for Learn {
    fn default() -> Self {
        Self::new()
    }
}

impl Learn {
    /// Empty accumulators.
    pub fn new() -> Self {
        Self {
            sum: std::array::from_fn(|i| vec![[0; 2]; 1 << (i + 1)]),
            count: std::array::from_fn(|i| vec![0; 1 << (i + 1)]),
            noise: 0,
            samples: 0,
        }
    }

    /// Pilot `t` (0 … 31) of a slot of modulation `m`, `z` with the PRBS
    /// sign `s` (+1 / −1) removed by the caller: label `t mod 2^bits`
    /// (§3.9).
    pub fn pilot(&mut self, m: Modulation, t: usize, z: [i16; 2]) {
        debug_assert!(t < PILOT_SYMBOLS);
        let mi = modulation_index(m);
        let lab = t % (1 << m.bits());
        self.sum[mi][lab][0] += i64::from(z[0]);
        self.sum[mi][lab][1] += i64::from(z[1]);
        self.count[mi][lab] += 1;
    }

    /// A noise sample: symbol `y` with smallest metric `best`.
    pub fn noise(&mut self, y: [i16; 2], best: i64) {
        let (yi, yq) = (i64::from(y[0]), i64::from(y[1]));
        self.noise += yi * yi + yq * yq + best;
        self.samples += 1;
    }

    /// The learned points of `m`: per label `round(Σ / count)` (one
    /// division per label at the frame's end), `None` unless every label
    /// had a pilot.
    pub fn points(&self, m: Modulation) -> Option<Vec<[i16; 2]>> {
        let mi = modulation_index(m);
        if self.count[mi].contains(&0) {
            return None;
        }
        Some(
            self.sum[mi]
                .iter()
                .zip(&self.count[mi])
                .map(|(s, &c)| {
                    let c = i64::from(c);
                    [
                        super::fx::sat16(div_round(s[0], c)),
                        super::fx::sat16(div_round(s[1], c)),
                    ]
                })
                .collect(),
        )
    }

    /// The noise estimate `round(Σ / samples)` (Q.20), `None` without a
    /// sample.
    pub fn n0(&self) -> Option<i64> {
        (self.samples > 0).then(|| div_round(self.noise, i64::from(self.samples)))
    }
}

/// The constellation and scale a frame is demapped with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InForce {
    /// Learned points per modulation (from the previous frame), `None`:
    /// the nominal points.
    pub learned: [Option<Vec<[i16; 2]>>; 5],
    /// n0 (Q.20) and its scale.
    pub n0: i64,
    /// See `n0`.
    pub k: i64,
    /// Frame counter of the frame they were learned on (`None`: the
    /// defaults).
    pub from: Option<u32>,
}

impl Default for InForce {
    fn default() -> Self {
        Self {
            learned: std::array::from_fn(|_| None),
            n0: N0_DEFAULT,
            k: scale_of(N0_DEFAULT),
            from: None,
        }
    }
}

impl InForce {
    /// The points for a slot.
    pub fn points(&self, s: &Slot) -> Vec<[i16; 2]> {
        self.learned[modulation_index(s.modulation)]
            .clone()
            .unwrap_or_else(|| nominal(s.modulation, s.rate))
    }

    /// What frame `frame`'s accumulators give the next frame: its learned
    /// points (a modulation without them keeps nominal) and its n0 (none:
    /// the current n0 stays).
    #[must_use]
    pub fn next(&self, l: &Learn, frame: u32) -> Self {
        let n0 = l.n0().unwrap_or(self.n0);
        Self {
            learned: std::array::from_fn(|i| l.points(MODULATIONS[i])),
            n0,
            k: scale_of(n0),
            from: Some(frame),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// On the nominal points themselves every LLR has the label's sign.
    #[test]
    fn demap_reads_the_labels_of_the_points() {
        let r = CodeRate::new(93).unwrap();
        for m in [
            Modulation::Qpsk,
            Modulation::Psk8,
            Modulation::Apsk16,
            Modulation::Apsk32,
        ] {
            let pts = nominal(m, r);
            for (lab, &p) in pts.iter().enumerate() {
                let mut out = [0i8; 5];
                demap(p, &pts, m.bits(), scale_of(N0_DEFAULT), &mut out);
                for (b, &l) in out[..m.bits()].iter().enumerate() {
                    let bit = (lab >> (m.bits() - 1 - b)) & 1;
                    assert_eq!(l < 0, bit == 1, "{m:?} {lab} bit {b}: {l}");
                    assert_ne!(l, 0);
                }
            }
        }
    }

    /// The fixed-point LLR is the float max-log LLR within rounding.
    #[test]
    fn demap_tracks_the_float_demapper() {
        let r = CodeRate::new(93).unwrap();
        let ptsf = crate::main_signal::nominal_points(Modulation::Apsk16, r);
        let pts = nominal(Modulation::Apsk16, r);
        let n0 = 0.03;
        let n0q = (n0 * f64::from(1u32 << 20)).round();
        // f64 → i64 of a small rounded value
        #[allow(clippy::cast_possible_truncation)]
        let k = scale_of(n0q as i64);
        let (mut eq, mut tot, mut maxd) = (0, 0, 0i32);
        for a in 0..40 {
            for b in 0..40 {
                let y = crate::Cplx::new(f64::from(a - 20) / 15.0, f64::from(b - 20) / 15.0);
                // |y| ≤ 1.4: 1 024·y rounds into i16
                #[allow(clippy::cast_possible_truncation)]
                let yq = [
                    (y.re * 1024.0).round() as i16,
                    (y.im * 1024.0).round() as i16,
                ];
                let mut f = Vec::new();
                crate::main_signal::demap(&[y], &ptsf, 4, n0q / f64::from(1u32 << 20), &mut f);
                let mut q = Vec::new();
                isdb_fec::ldpc::fixed::quantise(&f, 1.0, &mut q);
                let mut o = [0i8; 4];
                demap(yq, &pts, 4, k, &mut o);
                for (x, y) in q.iter().zip(&o) {
                    tot += 1;
                    eq += usize::from(x == y);
                    maxd = maxd.max((i32::from(*x) - i32::from(*y)).abs());
                }
            }
        }
        assert!(maxd <= 1, "max difference {maxd}");
        assert!(eq * 100 > tot * 90, "{eq} of {tot} equal");
    }
}

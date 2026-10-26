//! Measurements on front-end output symbols, for choosing widths and
//! checking the fixed-point model against the floating-point reference.
//! They need no knowledge of the broadcast frame:
//!
//! * [`ring_stats`]: the amplitude rings of a PSK / APSK constellation
//!   (1-D k-means on |z|) and a ring MER from the radial spread, which is
//!   independent of the carrier phase (no carrier recovery needed);
//! * [`psk_mer`]: MER after a decision-directed M-PSK phase-locked loop
//!   (all of ISDB-S's BPSK, QPSK and 8PSK points lie on the 8PSK grid);
//! * [`sqnr_db`]: signal to quantisation noise of one sequence against
//!   another.

use std::f64::consts::TAU;

/// Ring statistics.
#[derive(Debug, Clone, PartialEq)]
pub struct RingStats {
    /// Ring radii, ascending.
    pub radii: Vec<f64>,
    /// Fraction of the symbols on each ring.
    pub share: Vec<f64>,
    /// Mean |z|² over radial error variance, dB: the MER of a circularly
    /// symmetric noise (its radial part carries half the noise power).
    pub mer_db: f64,
}

/// Fit `rings` amplitude rings to `z`.
pub fn ring_stats(z: &[[f64; 2]], rings: usize) -> RingStats {
    let a: Vec<f64> = z.iter().map(|v| v[0].hypot(v[1])).collect();
    let mut sorted = a.clone();
    sorted.sort_by(f64::total_cmp);
    let n = sorted.len().max(1);
    let mut radii: Vec<f64> = (0..rings)
        .map(|k| sorted[((2 * k + 1) * n / (2 * rings)).min(n - 1)])
        .collect();
    let mut assign = vec![0usize; a.len()];
    for _ in 0..30 {
        for (i, &v) in a.iter().enumerate() {
            assign[i] = (0..rings)
                .min_by(|&x, &y| (v - radii[x]).abs().total_cmp(&(v - radii[y]).abs()))
                .unwrap_or(0);
        }
        let mut sum = vec![0.0; rings];
        let mut cnt = vec![0usize; rings];
        for (i, &v) in a.iter().enumerate() {
            sum[assign[i]] += v;
            cnt[assign[i]] += 1;
        }
        for k in 0..rings {
            if cnt[k] > 0 {
                radii[k] = sum[k] / cnt[k] as f64;
            }
        }
    }
    let mut cnt = vec![0usize; rings];
    let mut radial = 0.0;
    for (i, &v) in a.iter().enumerate() {
        cnt[assign[i]] += 1;
        radial += (v - radii[assign[i]]).powi(2);
    }
    let power = z.iter().map(|v| v[0] * v[0] + v[1] * v[1]).sum::<f64>();
    RingStats {
        radii,
        share: cnt.iter().map(|&c| c as f64 / n as f64).collect(),
        mer_db: 10.0 * (power / (2.0 * radial.max(1e-30))).log10(),
    }
}

/// MER after a decision-directed M-PSK loop.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PskMer {
    /// Mean |z|² over mean |z − decision|², dB.
    pub mer_db: f64,
    /// RMS of the loop's phase error, rad.
    pub phase_rms: f64,
    /// Mean frequency the loop tracked, cycles per symbol.
    pub freq: f64,
}

/// MER of `z` against the M-PSK grid (phases 2πk/M) after a second-order
/// decision-directed PLL of noise bandwidth `bn` (fraction of the symbol
/// rate); the first `settle` symbols are not counted.
pub fn psk_mer(z: &[[f64; 2]], m: usize, bn: f64, settle: usize) -> PskMer {
    let zeta = std::f64::consts::FRAC_1_SQRT_2;
    let theta = bn / (zeta + 1.0 / (4.0 * zeta));
    let d = 1.0 + 2.0 * zeta * theta + theta * theta;
    let (kp, ki) = (4.0 * zeta * theta / d, 4.0 * theta * theta / d);
    let rms =
        (z.iter().map(|v| v[0] * v[0] + v[1] * v[1]).sum::<f64>() / z.len().max(1) as f64).sqrt();
    let (mut ph, mut fr) = (0.0f64, 0.0f64);
    let (mut sig, mut err, mut perr, mut fsum, mut n) = (0.0, 0.0, 0.0, 0.0, 0usize);
    let step = TAU / m as f64;
    for (k, v) in z.iter().enumerate() {
        let (s, c) = ph.sin_cos();
        let r = [v[0] * c + v[1] * s, v[1] * c - v[0] * s];
        let ang = r[1].atan2(r[0]);
        let dec = (ang / step).round() * step;
        let e = ang - dec;
        let dpt = [rms * dec.cos(), rms * dec.sin()];
        fr += ki * e;
        ph += kp * e + fr;
        if k >= settle {
            sig += dpt[0] * dpt[0] + dpt[1] * dpt[1];
            err += (r[0] - dpt[0]).powi(2) + (r[1] - dpt[1]).powi(2);
            perr += e * e;
            fsum += fr;
            n += 1;
        }
    }
    let n = n.max(1) as f64;
    PskMer {
        mer_db: 10.0 * (sig / err.max(1e-30)).log10(),
        phase_rms: (perr / n).sqrt(),
        freq: fsum / n / TAU,
    }
}

/// Signal to quantisation noise, dB: power of `reference` over the power
/// of `reference − scale·test`.
pub fn sqnr_db(reference: &[[f64; 2]], test: &[[f64; 2]], scale: f64) -> f64 {
    let (mut s, mut e) = (0.0, 0.0);
    for (r, t) in reference.iter().zip(test) {
        s += r[0] * r[0] + r[1] * r[1];
        e += (r[0] - scale * t[0]).powi(2) + (r[1] - scale * t[1]).powi(2);
    }
    10.0 * (s / e.max(1e-30)).log10()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rings_of_a_clean_apsk() {
        // 4 + 12 points, ratio 2.6: two rings with shares 1/4, 3/4.
        let z: Vec<[f64; 2]> = (0..1600)
            .map(|i| {
                let (r, k, m) = if i % 4 == 0 {
                    (1.0, i / 4, 4)
                } else {
                    (2.6, i, 12)
                };
                let a = TAU * f64::from(k % m) / f64::from(m);
                [r * a.cos(), r * a.sin()]
            })
            .collect();
        let s = ring_stats(&z, 2);
        assert!((s.radii[0] - 1.0).abs() < 1e-9 && (s.radii[1] - 2.6).abs() < 1e-9);
        assert!((s.share[0] - 0.25).abs() < 1e-9);
        assert!(s.mer_db > 100.0);
    }

    #[test]
    fn psk_mer_tracks_a_rotating_8psk() {
        let mut s = 7u32;
        let z: Vec<[f64; 2]> = (0..20000)
            .map(|i| {
                s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let a = TAU * f64::from(s >> 29) / 8.0 + 1e-3 * f64::from(i);
                [a.cos(), a.sin()]
            })
            .collect();
        let r = psk_mer(&z, 8, 1e-2, 2000);
        assert!(r.mer_db > 40.0, "{r:?}");
        assert!((r.freq - 1e-3 / TAU).abs() < 1e-5, "{r:?}");
    }
}

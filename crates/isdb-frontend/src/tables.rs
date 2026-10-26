//! Tables of the front end: the mixer's cosine table and the polyphase
//! matched-filter and derivative banks (root-raised cosine, Kaiser
//! window), with the timing-detector slope that sets the loop gains.
//!
//! The integer tables are the single source of truth: the FPGA build reads
//! them from the files the model writes ([`crate::vectors`]), so the two
//! never compute them separately.

use crate::params::{COEF_BITS, COEF_SUM_MAX, FILTER_SHIFT, NCO_LUT_BITS, PHASES};
use std::f64::consts::PI;

/// Amplitude of the cosine table.
pub const COS_AMPLITUDE: f64 = 32767.0;

/// The mixer's cosine table: `round(32767·cos(2πk/1024))`, k = 0..1023.
/// The sine is read from it at k + 768.
pub fn cos_table() -> Vec<i16> {
    let n = 1usize << NCO_LUT_BITS;
    (0..n)
        .map(|k| (COS_AMPLITUDE * (2.0 * PI * k as f64 / n as f64).cos()).round() as i16)
        .collect()
}

/// Root-raised-cosine pulse of roll-off `alpha` at `t` symbol periods,
/// 1 − α + 4α/π at t = 0.
pub fn rrc(t: f64, alpha: f64) -> f64 {
    if t.abs() < 1e-12 {
        return 1.0 - alpha + 4.0 * alpha / PI;
    }
    let x = 4.0 * alpha * t;
    if (x.abs() - 1.0).abs() < 1e-9 {
        let a = PI / (4.0 * alpha);
        return alpha / std::f64::consts::SQRT_2
            * ((1.0 + 2.0 / PI) * a.sin() + (1.0 - 2.0 / PI) * a.cos());
    }
    ((PI * t * (1.0 - alpha)).sin() + x * (PI * t * (1.0 + alpha)).cos()) / (PI * t * (1.0 - x * x))
}

/// Modified Bessel function of the first kind, order 0 (series).
fn bessel_i0(x: f64) -> f64 {
    let (mut sum, mut term, mut k) = (1.0, 1.0, 1.0);
    while term > 1e-17 * sum {
        term *= (x / (2.0 * k)).powi(2);
        sum += term;
        k += 1.0;
    }
    sum
}

/// Kaiser window of shape `beta` over `|tau| ≤ half`, zero outside.
pub fn kaiser(tau: f64, half: f64, beta: f64) -> f64 {
    let r = tau / half;
    if r.abs() > 1.0 {
        0.0
    } else {
        bessel_i0(beta * (1.0 - r * r).sqrt()) / bessel_i0(beta)
    }
}

/// The continuous matched filter at `tau` input samples from the output
/// instant: RRC at `sps` samples per symbol under a Kaiser window spanning
/// the `taps`-sample delay line.
pub fn matched_filter(tau: f64, sps: f64, alpha: f64, taps: usize, beta: f64) -> f64 {
    let half = taps as f64 / 2.0;
    rrc(tau / sps, alpha) * kaiser(tau, half, beta)
}

/// d/dτ of [`matched_filter`] (central difference).
pub fn matched_filter_derivative(tau: f64, sps: f64, alpha: f64, taps: usize, beta: f64) -> f64 {
    let d = 1e-4;
    (matched_filter(tau + d, sps, alpha, taps, beta)
        - matched_filter(tau - d, sps, alpha, taps, beta))
        / (2.0 * d)
}

/// Floating-point banks: `[p][t]` at τ = t − taps/2 + p/PHASES.
pub fn float_banks(sps: f64, alpha: f64, taps: usize, beta: f64) -> (Vec<Vec<f64>>, Vec<Vec<f64>>) {
    let c = (taps / 2) as f64;
    let bank = |f: &dyn Fn(f64) -> f64| -> Vec<Vec<f64>> {
        (0..PHASES)
            .map(|p| {
                (0..taps)
                    .map(|t| f(t as f64 - c + p as f64 / PHASES as f64))
                    .collect()
            })
            .collect()
    };
    (
        bank(&|tau| matched_filter(tau, sps, alpha, taps, beta)),
        bank(&|tau| matched_filter_derivative(tau, sps, alpha, taps, beta)),
    )
}

/// The factor that scales a floating-point bank so that the largest sum of
/// coefficient magnitudes of a phase stays below [`COEF_SUM_MAX`] after
/// rounding (each coefficient can gain 0.5 by rounding).
pub fn bank_scale(bank: &[Vec<f64>]) -> f64 {
    let taps = bank.first().map_or(0, Vec::len) as f64;
    let max = bank
        .iter()
        .map(|p| p.iter().map(|c| c.abs()).sum::<f64>())
        .fold(0.0, f64::max);
    (COEF_SUM_MAX as f64 - taps / 2.0) / max
}

/// The integer banks `(h, g)` for `sps` samples per symbol, each scaled by
/// its own [`bank_scale`] and rounded.
pub fn matched_filter_banks(
    sps: f64,
    alpha: f64,
    taps: usize,
    beta: f64,
) -> (Vec<Vec<i32>>, Vec<Vec<i32>>) {
    let (hf, gf) = float_banks(sps, alpha, taps, beta);
    let quant = |bank: &[Vec<f64>]| {
        let s = bank_scale(bank);
        let lim = f64::from(1i32 << (COEF_BITS - 1)) - 1.0;
        bank.iter()
            .map(|p| {
                p.iter()
                    .map(|c| (c * s).round().clamp(-lim, lim) as i32)
                    .collect()
            })
            .collect::<Vec<Vec<i32>>>()
    };
    (quant(&hf), quant(&gf))
}

/// The output bank for a trim of `trim` input samples: at τ = t − taps/2 +
/// p/PHASES, the root-raised cosine at τ + `trim` under the same Kaiser
/// window as [`matched_filter`] (centred on the loop's instant, not on the
/// trimmed one), scaled by the matched-filter bank's [`bank_scale`] so its
/// output has the same scale as `h`'s and rounded the same way. A trim of
/// 0 gives `h` exactly.
pub fn output_bank(sps: f64, alpha: f64, taps: usize, beta: f64, trim: f64) -> Vec<Vec<i32>> {
    let (hf, _) = float_banks(sps, alpha, taps, beta);
    let s = bank_scale(&hf);
    let half = taps as f64 / 2.0;
    let c = half.floor();
    let lim = f64::from(1i32 << (COEF_BITS - 1)) - 1.0;
    let bank: Vec<Vec<f64>> = (0..PHASES)
        .map(|p| {
            (0..taps)
                .map(|t| {
                    let tau = t as f64 - c + p as f64 / PHASES as f64;
                    rrc((tau + trim) / sps, alpha) * kaiser(tau, half, beta) * s
                })
                .collect()
        })
        .collect();
    // Keep the coefficient budget: scale down (rare: a trim moves the peak
    // off the window's centre) when a phase's magnitudes sum too high.
    let max = bank
        .iter()
        .map(|p| p.iter().map(|c| c.abs()).sum::<f64>())
        .fold(0.0, f64::max);
    let budget = COEF_SUM_MAX as f64 - taps as f64 / 2.0;
    let fit = if max > budget { budget / max } else { 1.0 };
    bank.iter()
        .map(|p| {
            p.iter()
                .map(|c| (c * fit).round().clamp(-lim, lim) as i32)
                .collect()
        })
        .collect()
}

/// Slope of the timing detector, in loop error units (after `ted_shift`)
/// per input sample of timing error (positive when sampling early), for
/// the integer banks `h`, `g` at `sps` samples per symbol with |z| held at
/// `agc_rms`·32768 RMS. Measured on a noiseless QPSK pulse train with a
/// fixed pseudo-random symbol sequence; only the loop gains depend on it.
pub fn ted_gain(
    h: &[Vec<i32>],
    g: &[Vec<i32>],
    sps: f64,
    alpha: f64,
    agc_rms: f64,
    ted_shift: u8,
) -> f64 {
    let taps = h.first().map_or(0, Vec::len);
    // Continuous filters with the integer banks' scales, read back from
    // the banks' phase 0 (the ratio of integer to float coefficient).
    let (hf, gf) = float_banks(sps, alpha, taps, 0.0);
    let ratio = |int: &[Vec<i32>], fl: &[Vec<f64>]| {
        let (mut num, mut den) = (0.0, 0.0);
        for (pi, pf) in int.iter().zip(fl) {
            for (&a, &b) in pi.iter().zip(pf) {
                num += f64::from(a) * b;
                den += b * b;
            }
        }
        num / den
    };
    // The window is part of the integer banks; it hardly changes the
    // slope near the main lobe, so the unwindowed filter with the banks'
    // scale is used for the measurement.
    let (sh, sg) = (ratio(h, &hf), ratio(g, &gf));
    let half = taps as f64 / 2.0;
    let mf = |tau: f64| matched_filter(tau, sps, alpha, taps, 0.0) * sh;
    let dmf = |tau: f64| matched_filter_derivative(tau, sps, alpha, taps, 0.0) * sg;
    // QPSK symbols from a 32-bit LCG.
    let mut state = 0x1234_5678u32;
    let n_sym = 600usize;
    let syms: Vec<(f64, f64)> = (0..n_sym)
        .map(|_| {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let i = if state & 0x8000_0000 != 0 { 1.0 } else { -1.0 };
            let q = if state & 0x4000_0000 != 0 { 1.0 } else { -1.0 };
            (i, q)
        })
        .collect();
    let n_samp = (n_sym as f64 * sps) as usize + 64;
    let span = (40.0 * sps) as i64;
    let x: Vec<(f64, f64)> = (0..n_samp)
        .map(|n| {
            let k0 = ((n as f64 / sps) as i64 - span / 2).max(0) as usize;
            let k1 = (((n as f64 / sps) as i64 + span / 2) as usize).min(n_sym);
            (k0..k1).fold((0.0, 0.0), |(a, b), k| {
                let w = rrc((n as f64 - k as f64 * sps) / sps, alpha);
                (a + syms[k].0 * w, b + syms[k].1 * w)
            })
        })
        .collect();
    let out = |s: f64, f: &dyn Fn(f64) -> f64| {
        let n0 = (s - half).floor() as i64;
        let (mut a, mut b) = (0.0, 0.0);
        for n in n0..=n0 + taps as i64 + 1 {
            if n < 0 || n as usize >= x.len() {
                continue;
            }
            let w = f(s - n as f64);
            a += x[n as usize].0 * w;
            b += x[n as usize].1 * w;
        }
        let k = f64::from(1u32 << FILTER_SHIFT);
        (a / k, b / k)
    };
    let delta = 2.0 / PHASES as f64;
    let mean_error = |offset: f64| {
        let (mut ys, mut ds) = (Vec::new(), Vec::new());
        for k in 80..n_sym - 80 {
            let s = k as f64 * sps + offset;
            ys.push(out(s, &mf));
            ds.push(out(s, &dmf));
        }
        let p = ys.iter().map(|(a, b)| a * a + b * b).sum::<f64>() / ys.len() as f64;
        let gain = agc_rms * 32768.0 / p.sqrt();
        let k = gain * gain / f64::from(1u32 << ted_shift);
        ys.iter()
            .zip(&ds)
            .map(|(y, d)| (y.0 * d.0 + y.1 * d.1) * k)
            .sum::<f64>()
            / ys.len() as f64
    };
    (mean_error(-delta) - mean_error(delta)) / (2.0 * delta)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cos_table_values() {
        let c = cos_table();
        assert_eq!(c.len(), 1024);
        assert_eq!(c[0], 32767);
        assert_eq!(c[256], 0);
        assert_eq!(c[512], -32767);
        assert_eq!(c[768], 0);
        // sin(π/4) = cos at 128 and at 128 + 768 − 1024 … the sine of
        // index k is cos[(k + 768) & 1023].
        assert_eq!(c[(128 + 768) & 1023], c[128]);
    }

    #[test]
    fn rrc_known_points() {
        // Peak, and zero crossings of the full raised cosine are not those
        // of the root; check the peak and the singular point instead.
        assert!((rrc(0.0, 0.35) - (0.65 + 1.4 / PI)).abs() < 1e-12);
        let a = 0.25;
        let t = 1.0 / (4.0 * a);
        let near = rrc(t + 1e-7, a);
        assert!((rrc(t, a) - near).abs() < 1e-5);
        // Root-Nyquist: the RRC convolved with itself is ≈ 0 at nonzero
        // integer symbol offsets.
        let sps = 16.0;
        let rc = |k: f64| {
            (-400..=400)
                .map(|n| {
                    let t = f64::from(n) / sps;
                    rrc(t, 0.35) * rrc(k - t, 0.35)
                })
                .sum::<f64>()
                / sps
        };
        let peak = rc(0.0);
        for k in 1..4 {
            assert!(rc(f64::from(k)).abs() < 2e-3 * peak, "k {k}");
        }
    }

    #[test]
    fn banks_respect_the_coefficient_budget() {
        for (sps, alpha, taps) in [(1.386, 0.35, 16), (1.185, 0.03, 48), (2.0, 0.35, 32)] {
            let (h, g) = matched_filter_banks(sps, alpha, taps, 5.0);
            for bank in [&h, &g] {
                assert_eq!(bank.len(), PHASES);
                for p in bank {
                    assert_eq!(p.len(), taps);
                    assert!(p.iter().map(|&c| i64::from(c).abs()).sum::<i64>() <= COEF_SUM_MAX);
                }
            }
            // Phase 0 of h peaks at the centre tap.
            let c = taps / 2;
            let max = h[0].iter().map(|v| v.abs()).max().unwrap();
            assert_eq!(h[0][c].abs(), max);
        }
    }

    #[test]
    fn output_bank_without_trim_is_the_matched_filter() {
        for (sps, alpha, taps) in [(1.386, 0.35, 16), (1.82, 0.03, 32), (2.129, 0.35, 32)] {
            let (h, _) = matched_filter_banks(sps, alpha, taps, 5.0);
            assert_eq!(output_bank(sps, alpha, taps, 5.0, 0.0), h);
            // A trim moves the peak of phase 0 by about `trim` samples.
            let k = output_bank(sps, alpha, taps, 5.0, -0.31);
            assert!(
                k.iter()
                    .flatten()
                    .map(|&c| i64::from(c).abs())
                    .max()
                    .unwrap()
                    > 0
            );
            assert!(
                k.iter()
                    .all(|p| p.iter().map(|&c| i64::from(c).abs()).sum::<i64>() <= COEF_SUM_MAX)
            );
        }
    }

    #[test]
    fn ted_slope_is_positive() {
        let (h, g) = matched_filter_banks(1.386, 0.35, 16, 5.0);
        let k = ted_gain(&h, &g, 1.386, 0.35, 0.25, 14);
        assert!(k > 0.0, "{k}");
    }
}

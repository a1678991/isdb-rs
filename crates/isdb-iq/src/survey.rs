//! What a single-carrier signal in a recording looks like, without
//! demodulating it.
//!
//! * **Power spectrum** (Welch, Hann window): the band's centre and its
//!   widths 3 dB and 20 dB below the in-band level. Their difference
//!   shows the roll-off: a raised-cosine spectrum of roll-off α at symbol
//!   rate R falls from −3 dB to −20 dB over roughly 0.3·α·R per edge.
//! * **Symbol-rate line**: |z|² of a linearly modulated signal is
//!   cyclostationary with period 1/R, so its spectrum has a line at R
//!   whose strength grows with the roll-off. The line is searched between
//!   [`Options::min_symbol_rate_hz`] and fs/2; because |z|² is real, a
//!   rate above fs/2 shows up at fs − R ([`Survey::system`] tries both).
//! * **Carrier lines**: z^k removes M-PSK modulation for k a multiple of
//!   M, leaving a line at k·Δf (Δf: carrier offset from the tuned centre).
//!   k = 2 (BPSK parts), 4 (QPSK), 8 (8PSK) are searched. Cyclostationarity
//!   also puts copies at k·Δf ± R, which can be as strong as the line
//!   itself, so once a symbol rate R is known (the matched system's, else
//!   the |z|² line if it stands clear) the search is limited to
//!   |f| < R/2 (z⁴, z⁸) or R/4 (z²): Δf is then found within ±R/(2k)
//!   (±1.8 MHz for 8PSK at 28.86 Mbaud), ±R/8 for z². π/2-shift BPSK
//!   (ISDB-S3's TMCC) puts its z² line at 2Δf ± R/2 instead; that line is
//!   searched separately (k = 2, shift ±R/2, within R/4 of it, less when
//!   the two windows would meet across fs/2).
//!
//! Each line is reported with its level above the median of the bins
//! searched, in dB; a line that does not stand clear of the median (a few
//! dB) is noise. The spectra use [`Options::fft_len`] samples (fewer if
//! the input is shorter), so the frequency resolution is fs/`fft_len`.

use crate::C32;
use rustfft::FftPlanner;
use rustfft::num_complex::Complex32;

/// A transmission system recognised by its symbol rate.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct KnownSystem {
    /// Name, e.g. `ISDB-S`.
    pub name: &'static str,
    /// Nominal symbol rate in baud.
    pub symbol_rate_hz: f64,
    /// Roll-off of its spectrum shaping.
    pub roll_off: f64,
    /// Where the values are specified.
    pub source: &'static str,
}

/// The systems [`Survey::system`] recognises.
pub const KNOWN_SYSTEMS: &[KnownSystem] = &[
    KnownSystem {
        name: "ISDB-S",
        symbol_rate_hz: 28.860e6,
        roll_off: 0.35,
        source: "MIC Ordinance No. 87 of 2011 Art. 52(3), annexed table 47; ITU-R BO.1408-1 Annex 1 Table 3",
    },
    KnownSystem {
        name: "ISDB-S3",
        symbol_rate_hz: 33.7561e6,
        roll_off: 0.03,
        source: "ARIB STD-B44 2.1 Annex B.3 (±20 ppm, B.4); ITU-R BO.2098-0 Annex 1",
    },
];

/// Analysis sizes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Options {
    /// Samples of the cyclic (|z|², z^k) spectra: a power of two; the
    /// input's largest power of two is used when it is shorter.
    pub fft_len: usize,
    /// FFT length of the Welch power spectrum (a power of two).
    pub psd_len: usize,
    /// Lowest symbol rate searched, Hz (keeps the search off DC and
    /// slow power variations).
    pub min_symbol_rate_hz: f64,
    /// Largest relative deviation in ppm of a measured symbol rate from a
    /// [`KnownSystem`] rate for a match.
    pub match_ppm: f64,
}

impl Default for Options {
    /// 2²² samples (73 ms at 57.72 Msps, 14 Hz resolution), 4096-point
    /// power spectrum, rates from 1 MHz, 200 ppm.
    fn default() -> Self {
        Self {
            fft_len: 1 << 22,
            psd_len: 4096,
            min_symbol_rate_hz: 1e6,
            match_ppm: 200.0,
        }
    }
}

/// A spectral line.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Line {
    /// Frequency in Hz (interpolated between bins).
    pub freq_hz: f64,
    /// Level above the median of the bins searched, dB.
    pub level_db: f64,
}

/// The z^k carrier line.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CarrierLine {
    /// The power k.
    pub k: u32,
    /// Where the line sits relative to k·Δf: 0, or ±R/2 for the
    /// π/2-shift BPSK z² line.
    pub shift_hz: f64,
    /// The line of z^k (at k·Δf + shift).
    pub line: Line,
    /// The carrier offset Δf it gives, Hz: (line − shift)/k.
    pub offset_hz: f64,
}

/// Band edges from the power spectrum, relative to the tuned centre.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Band {
    /// In-band level: the highest power-spectrum value after smoothing,
    /// dB relative to full scale per bin.
    pub level_db: f64,
    /// Midpoint of the −3 dB edges, Hz from the tuned centre.
    pub center_hz: f64,
    /// Width between the −3 dB edges, Hz.
    pub width_3db_hz: f64,
    /// Width between the −20 dB edges, Hz (`None` if the spectrum does not
    /// fall 20 dB below the in-band level inside the sampled band).
    pub width_20db_hz: Option<f64>,
}

/// A [`KnownSystem`] matched by the symbol-rate line.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SystemMatch {
    /// The system.
    pub system: &'static KnownSystem,
    /// Symbol rate measured (the line, or fs minus the line).
    pub symbol_rate_hz: f64,
    /// Deviation from the nominal rate, ppm.
    pub ppm: f64,
}

/// The survey of one stretch of samples.
#[derive(Debug, Clone, PartialEq)]
pub struct Survey {
    /// Sample rate, Hz.
    pub sample_rate_hz: f64,
    /// Samples of the cyclic spectra.
    pub samples: usize,
    /// Mean power, dB relative to full scale (|z| = 1).
    pub power_dbfs: f64,
    /// Band edges.
    pub band: Band,
    /// The |z|² line between the minimum rate and fs/2.
    pub symbol_line: Line,
    /// The z², z⁴ and z⁸ lines (shift 0), and with a known symbol rate
    /// the π/2-BPSK z² lines (shift ±R/2).
    pub carrier_lines: Vec<CarrierLine>,
    /// The known system whose symbol rate matches the line.
    pub system: Option<SystemMatch>,
}

impl Survey {
    /// The carrier line with the highest level.
    pub fn best_carrier(&self) -> Option<&CarrierLine> {
        self.carrier_lines
            .iter()
            .max_by(|a, b| a.line.level_db.total_cmp(&b.line.level_db))
    }
}

// The window is computed in f64 and rounded to the f32 of the samples.
#[allow(clippy::cast_possible_truncation)]
fn hann(n: usize) -> Vec<f32> {
    (0..n)
        .map(|i| {
            let s = (std::f64::consts::PI * i as f64 / n as f64).sin();
            (s * s) as f32
        })
        .collect()
}

fn median(v: &[f64]) -> f64 {
    let mut s = v.to_vec();
    let mid = s.len() / 2;
    *s.select_nth_unstable_by(mid, f64::total_cmp).1
}

/// The index and value of the largest of `v` in `total_cmp` order, the
/// last of equal ones (as `Iterator::max_by`); `(0, −∞)` for an empty `v`
/// (no caller passes one: an inclusive range, a spectrum of
/// `psd_len` ≥ 1 bins).
fn last_max(v: &[f64]) -> (usize, f64) {
    let Some((&first, rest)) = v.split_first() else {
        return (0, f64::NEG_INFINITY);
    };
    rest.iter().enumerate().fold((0, first), |best, (i, &x)| {
        if best.1.total_cmp(&x).is_gt() {
            best
        } else {
            (i + 1, x)
        }
    })
}

/// Peak of `p[lo..=hi]` (power per bin) with parabolic interpolation on
/// the dB values: (fractional bin, level above the median of the range).
fn peak(p: &[f64], lo: usize, hi: usize) -> (f64, f64) {
    let range = &p[lo..=hi];
    let (i, max) = last_max(range);
    let k = lo + i;
    let db = |x: f64| 10.0 * x.max(1e-300).log10();
    let mut frac = 0.0;
    if k > 0 && k + 1 < p.len() {
        let (a, b, c) = (db(p[k - 1]), db(p[k]), db(p[k + 1]));
        let d = a - 2.0 * b + c;
        if d < 0.0 {
            frac = (0.5 * (a - c) / d).clamp(-0.5, 0.5);
        }
    }
    (k as f64 + frac, db(max) - db(median(range)))
}

/// Power spectrum |FFT|² of `x` (Hann-windowed).
fn power_spectrum(
    planner: &mut FftPlanner<f32>,
    x: impl Iterator<Item = C32>,
    n: usize,
) -> Vec<f64> {
    let w = hann(n);
    let mut buf: Vec<Complex32> = x.zip(&w).map(|(v, &w)| v * w).collect();
    buf.resize(n, Complex32::new(0.0, 0.0));
    planner.plan_fft_forward(n).process(&mut buf);
    buf.iter().map(|c| f64::from(c.norm_sqr())).collect()
}

/// Welch power spectrum, fft-shifted (bin 0 = −fs/2), dB per bin
/// relative to a full-scale (|z| = 1) carrier.
fn welch(planner: &mut FftPlanner<f32>, x: &[C32], n: usize) -> Vec<f64> {
    let w = hann(n);
    let w2: f64 = w.iter().map(|&v| f64::from(v * v)).sum();
    let fft = planner.plan_fft_forward(n);
    let mut acc = vec![0.0f64; n];
    let mut count = 0usize;
    let mut buf = vec![Complex32::new(0.0, 0.0); n];
    // At most 4096 half-overlapping segments, spread over the input.
    let starts = x.len().saturating_sub(n) / (n / 2) + 1;
    let step = (starts / 4096).max(1) * (n / 2);
    let mut s = 0;
    while s + n <= x.len() {
        for ((b, v), &w) in buf.iter_mut().zip(&x[s..s + n]).zip(&w) {
            *b = v * w;
        }
        fft.process(&mut buf);
        for (a, b) in acc.iter_mut().zip(&buf) {
            *a += f64::from(b.norm_sqr());
        }
        count += 1;
        s += step;
    }
    let norm = 1.0 / (count.max(1) as f64 * w2 * n as f64);
    (0..n)
        .map(|i| 10.0 * (acc[(i + n / 2) % n] * norm).max(1e-30).log10())
        .collect()
}

/// Band edges of a shifted dB spectrum `psd` at `fs`.
fn band(psd: &[f64], fs: f64) -> Band {
    let n = psd.len();
    // 15-bin moving average against the noise-like ripple of a PSD estimate.
    let h = 7usize;
    let smooth: Vec<f64> = (0..n)
        .map(|i| {
            let (a, b) = (i.saturating_sub(h), (i + h).min(n - 1));
            psd[a..=b].iter().sum::<f64>() / (b - a + 1) as f64
        })
        .collect();
    let (top, level) = last_max(&smooth);
    // Walk out from the top while the spectrum stays above level − drop;
    // the edge is interpolated between the last bin above and the first
    // below. None if the band edge of the sampled spectrum is reached.
    let edge = |drop: f64, dir: isize| -> Option<f64> {
        let t = level - drop;
        let mut i = top as isize;
        loop {
            let j = i + dir;
            if j < 0 || j >= n as isize {
                return None;
            }
            let (a, b) = (smooth[i as usize], smooth[j as usize]);
            if b < t {
                return Some(i as f64 + dir as f64 * (a - t) / (a - b));
            }
            i = j;
        }
    };
    let bin_hz = fs / n as f64;
    let hz = |bin: f64| (bin - (n / 2) as f64) * bin_hz;
    let (lo3, hi3) = (edge(3.0, -1), edge(3.0, 1));
    let (lo3, hi3) = (lo3.unwrap_or(0.0), hi3.unwrap_or((n - 1) as f64));
    let width_20db_hz = match (edge(20.0, -1), edge(20.0, 1)) {
        (Some(a), Some(b)) => Some((b - a) * bin_hz),
        _ => None,
    };
    Band {
        level_db: level,
        center_hz: hz(0.5 * (lo3 + hi3)),
        width_3db_hz: (hi3 - lo3) * bin_hz,
        width_20db_hz,
    }
}

/// The known system whose rate matches a |z|² line at `line_hz` (or its
/// alias fs − line).
pub fn match_system(line_hz: f64, fs: f64, match_ppm: f64) -> Option<SystemMatch> {
    KNOWN_SYSTEMS
        .iter()
        .flat_map(|s| {
            [line_hz, fs - line_hz, fs + line_hz].map(|r| SystemMatch {
                system: s,
                symbol_rate_hz: r,
                ppm: (r / s.symbol_rate_hz - 1.0) * 1e6,
            })
        })
        .filter(|m| m.ppm.abs() <= match_ppm)
        .min_by(|a, b| a.ppm.abs().total_cmp(&b.ppm.abs()))
}

/// Survey `x` sampled at `fs` Hz.
///
/// # Panics
///
/// If `x` has fewer than `opts.psd_len` samples or the lengths are not
/// powers of two.
pub fn survey(x: &[C32], fs: f64, opts: &Options) -> Survey {
    assert!(opts.psd_len.is_power_of_two() && opts.fft_len.is_power_of_two());
    assert!(
        x.len() >= opts.psd_len,
        "need at least {} samples",
        opts.psd_len
    );
    let n = opts.fft_len.min(1 << x.len().ilog2());
    let x = &x[..n];
    let mut planner = FftPlanner::new();
    let power = x.iter().map(|c| f64::from(c.norm_sqr())).sum::<f64>() / n as f64;
    // The f64 power sum rounded to the f32 of the samples it normalises.
    #[allow(clippy::cast_possible_truncation)]
    let (rms, mean) = (power.sqrt().max(1e-30) as f32, power as f32);

    let psd = welch(&mut planner, x, opts.psd_len);
    let band = band(&psd, fs);

    // |z|² without its mean.
    let p = power_spectrum(
        &mut planner,
        x.iter()
            .map(|c| C32::new(c.norm_sqr() / (rms * rms) - mean / (rms * rms), 0.0)),
        n,
    );
    let bin_hz = fs / n as f64;
    // A bin index: `as` saturates (NaN to 0) and the clamp bounds it.
    #[allow(clippy::cast_possible_truncation)]
    let lo = ((opts.min_symbol_rate_hz / bin_hz).ceil() as usize).clamp(1, n / 2);
    let (bin, level_db) = peak(&p, lo, n / 2);
    let symbol_line = Line {
        freq_hz: bin * bin_hz,
        level_db,
    };

    let system = match_system(symbol_line.freq_hz, fs, opts.match_ppm);
    // The symbol rate bounding the carrier search: the matched system's,
    // else the line's if it stands clear of the noise.
    let rate = system
        .map(|m| m.symbol_rate_hz)
        .or((symbol_line.level_db > 10.0).then_some(symbol_line.freq_hz));
    let mut carrier_lines = Vec::new();
    for k in [2u32, 4, 8] {
        let p = power_spectrum(&mut planner, x.iter().map(|c| (c / rms).powu(k)), n);
        // Peak of the bins within `half` Hz of `centre` (wrapped).
        let line_near = |centre: f64, half: f64| -> Line {
            let mut q = vec![0.0; n];
            // Shift the window to the middle of a scratch spectrum so the
            // peak search sees it as one contiguous range: bin c + d of `p`
            // (wrapped) goes to n/2 + d of `q`, |d| <= w.
            let c = (centre / bin_hz).round();
            // Bin counts: `as` saturates (NaN to 0), the clamp bounds the
            // half width and rem_euclid by n puts the centre bin in [0, n).
            #[allow(clippy::cast_possible_truncation)]
            let (w, c_bin) = (
                ((half / bin_hz) as usize).clamp(1, n / 2 - 1),
                c.rem_euclid(n as f64) as usize,
            );
            let (lo, hi) = (n / 2 - w, n / 2 + w);
            for dst in lo..=hi {
                q[dst] = p[(c_bin + n + dst - n / 2) % n];
            }
            let (bin, level_db) = peak(&q, lo, hi);
            Line {
                freq_hz: (c + bin - (n / 2) as f64) * bin_hz,
                level_db,
            }
        };
        // z² keeps clear of the π/2-BPSK lines at ±R/2.
        let span = if k == 2 { 4.0 } else { 2.0 };
        let half = rate.map_or(fs / 2.0, |r| (r / span).min(fs / 2.0));
        let line = line_near(0.0, half);
        carrier_lines.push(CarrierLine {
            k,
            shift_hz: 0.0,
            line,
            offset_hz: line.freq_hz / f64::from(k),
        });
        if let (2, Some(r)) = (k, rate) {
            // The windows at ±R/2 wrap around fs; keep them apart, or at
            // fs < 2R both see both lines (at 40 Msps and R = 33.76 Mbaud
            // the +R/2 window would reach the −R/2 line through fs/2).
            let apart = r.rem_euclid(fs);
            let half = (r / 4.0).min(0.49 * apart.min(fs - apart));
            for shift in [r / 2.0, -r / 2.0] {
                let line = line_near(shift, half);
                carrier_lines.push(CarrierLine {
                    k,
                    shift_hz: shift,
                    line,
                    offset_hz: (line.freq_hz - shift) / 2.0,
                });
            }
        }
    }

    Survey {
        sample_rate_hz: fs,
        samples: n,
        power_dbfs: 10.0 * power.max(1e-30).log10(),
        band,
        symbol_line,
        carrier_lines,
        system,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn last_max_is_max_by() {
        let nan = f64::NAN;
        for v in [
            &[][..],
            &[1.0],
            &[1.0, 3.0, 3.0, 2.0],
            &[3.0, 1.0, 3.0],
            &[-nan, -nan],
            &[1.0, nan, 2.0, -nan],
            &[f64::NEG_INFINITY, -1.0, f64::INFINITY, f64::INFINITY],
        ] {
            let want = v
                .iter()
                .enumerate()
                .max_by(|a, b| a.1.total_cmp(b.1))
                .map_or((0, f64::NEG_INFINITY), |(i, &x)| (i, x));
            let got = last_max(v);
            assert_eq!(got.0, want.0, "{v:?}");
            assert_eq!(got.1.to_bits(), want.1.to_bits(), "{v:?}");
        }
    }

    /// A QPSK (or 8PSK) signal at `sps` samples per symbol with a
    /// raised-cosine-like smooth pulse, offset `cfo` cycles per sample.
    // Test signal synthesis: f64 symbol positions and phases taken to the
    // f32 samples and usize symbol indices of the generated signal.
    #[allow(clippy::cast_possible_truncation)]
    fn psk(m: u32, sps: f64, cfo: f64, n: usize) -> Vec<C32> {
        let mut rng = 0x1234_5678_9abc_def1u64;
        let mut next = || {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            rng
        };
        let symbols: Vec<C32> = (0..(n as f64 / sps) as usize + 8)
            .map(|_| {
                let ph = 2.0 * std::f32::consts::PI * (next() % u64::from(m)) as f32 / m as f32;
                C32::new(ph.cos(), ph.sin())
            })
            .collect();
        (0..n)
            .map(|i| {
                // Linear interpolation between symbols with a raised-cosine
                // transition: excess bandwidth ~1, a strong |z|² line.
                let t = i as f64 / sps;
                let k = t.floor() as usize;
                let f = (t - k as f64) as f32;
                let w = 0.5 - 0.5 * (std::f32::consts::PI * f).cos();
                let s = symbols[k] * (1.0 - w) + symbols[k + 1] * w;
                let ph = 2.0 * std::f64::consts::PI * cfo * i as f64;
                s * C32::new(ph.cos() as f32, ph.sin() as f32) * 0.3
            })
            .collect()
    }

    #[test]
    fn isdb_s_like_signal() {
        // 8PSK at the ISDB-S rate sampled at 2.1 samples per symbol,
        // offset -107 kHz.
        let r = 28.86e6;
        let fs = 2.1 * r;
        let x = psk(8, 2.1, -107e3 / fs, 1 << 18);
        let s = survey(&x, fs, &Options::default());
        assert_eq!(s.samples, 1 << 18);
        let res = fs / s.samples as f64;
        assert!((s.symbol_line.freq_hz - r).abs() < res, "{s:?}");
        assert!(s.symbol_line.level_db > 20.0, "{s:?}");
        let m = s.system.expect("ISDB-S");
        assert_eq!(m.system.name, "ISDB-S");
        let c8 = s.carrier_lines.iter().find(|c| c.k == 8).unwrap();
        assert_eq!(s.carrier_lines.len(), 5); // z², z² ±R/2, z⁴, z⁸
        assert!((c8.offset_hz + 107e3).abs() < res, "{c8:?}");
        assert!(c8.line.level_db > 20.0);
        assert_eq!(s.best_carrier().unwrap().k, 8);
        assert!(
            (s.power_dbfs - 20.0 * 0.3f64.log10()).abs() < 1.5,
            "{}",
            s.power_dbfs
        );
    }

    #[test]
    fn symbol_rate_above_half_the_sample_rate_aliases() {
        // ISDB-S3's rate at 61.44 Msps (1.82 samples per symbol): the
        // |z|² line appears at fs - R = 27.68 MHz.
        let r = 33.7561e6;
        let fs = 61.44e6;
        let x = psk(4, fs / r, 0.0, 1 << 18);
        let s = survey(&x, fs, &Options::default());
        assert!(
            (s.symbol_line.freq_hz - (fs - r)).abs() < fs / s.samples as f64,
            "{s:?}"
        );
        let m = s.system.expect("ISDB-S3");
        assert_eq!(m.system.name, "ISDB-S3");
        assert!(m.ppm.abs() < 5.0, "{m:?}");
        assert_eq!(s.best_carrier().unwrap().k, 4);
    }

    #[test]
    fn band_edges_of_a_flat_band() {
        // White noise filtered to 0.5·fs by zeroing half the FFT bins.
        let n = 1 << 16;
        let mut rng = 7u64;
        let mut buf: Vec<Complex32> = (0..n)
            .map(|_| {
                rng = rng.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
                let a = ((rng >> 40) as f32 / (1u64 << 24) as f32) - 0.5;
                rng = rng.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
                let b = ((rng >> 40) as f32 / (1u64 << 24) as f32) - 0.5;
                Complex32::new(a, b)
            })
            .collect();
        let mut planner = FftPlanner::<f32>::new();
        planner.plan_fft_forward(n).process(&mut buf);
        for (i, v) in buf.iter_mut().enumerate() {
            // keep −0.25 fs .. +0.25 fs
            if i > n / 4 && i < 3 * n / 4 {
                *v = Complex32::new(0.0, 0.0);
            }
        }
        planner.plan_fft_inverse(n).process(&mut buf);
        let s = survey(&buf, 1e6, &Options::default());
        assert!((s.band.width_3db_hz - 0.5e6).abs() < 0.02e6, "{:?}", s.band);
        assert!(s.band.center_hz.abs() < 0.01e6, "{:?}", s.band);
        let w20 = s.band.width_20db_hz.expect("steep edges");
        assert!(w20 - s.band.width_3db_hz < 0.02e6, "{:?}", s.band);
        // No modulation: no known system.
        assert!(s.system.is_none() || s.symbol_line.level_db < 10.0);
    }

    #[test]
    fn pi2_bpsk_carrier_line() {
        // π/2-shift BPSK (s_n = j^n b_n) at the ISDB-S3 rate, offset +60 kHz,
        // at the AD936x maximum and at 40 Msps (where the ±R/2 windows
        // would otherwise meet across fs/2).
        for fs in [61.44e6, 40e6] {
            pi2_bpsk_at(fs);
        }
    }

    // Test signal synthesis: f64 symbol index and phase taken to the u64
    // index and f32 rotation of the generated signal.
    #[allow(clippy::cast_possible_truncation)]
    fn pi2_bpsk_at(fs: f64) {
        let r = 33.7561e6;
        let sps = fs / r;
        let mut x = psk(2, sps, 0.0, 1 << 18);
        for (i, v) in x.iter_mut().enumerate() {
            let n = (i as f64 / sps + 0.5).floor() as u64;
            let rot = [
                C32::new(1.0, 0.0),
                C32::new(0.0, 1.0),
                C32::new(-1.0, 0.0),
                C32::new(0.0, -1.0),
            ];
            let ph = 2.0 * std::f64::consts::PI * 60e3 / fs * i as f64;
            *v = *v * rot[(n % 4) as usize] * C32::new(ph.cos() as f32, ph.sin() as f32);
        }
        let s = survey(&x, fs, &Options::default());
        assert_eq!(s.system.unwrap().system.name, "ISDB-S3");
        let res = 2.0 * fs / s.samples as f64;
        let best = s.best_carrier().unwrap();
        assert!((best.offset_hz - 60e3).abs() < res, "{best:?}");
        // The z² line sits at 2Δf ± R/2, not at 2Δf.
        let pi2 = s
            .carrier_lines
            .iter()
            .filter(|c| c.shift_hz != 0.0)
            .max_by(|a, b| a.line.level_db.total_cmp(&b.line.level_db))
            .unwrap();
        assert!((pi2.offset_hz - 60e3).abs() < res, "{pi2:?}");
        // Each window holds its own line: both give the offset.
        for c in s.carrier_lines.iter().filter(|c| c.shift_hz != 0.0) {
            assert!((c.offset_hz - 60e3).abs() < res, "fs {fs}: {c:?}");
        }
        let plain = s
            .carrier_lines
            .iter()
            .find(|c| c.k == 2 && c.shift_hz == 0.0)
            .unwrap();
        assert!(
            pi2.line.level_db > plain.line.level_db + 10.0,
            "{pi2:?} {plain:?}"
        );
    }

    #[test]
    fn rate_matching() {
        let fs = 57.72e6;
        let m = match_system(28.86e6 * (1.0 + 50e-6), fs, 200.0).unwrap();
        assert_eq!(m.system.name, "ISDB-S");
        assert!((m.ppm - 50.0).abs() < 1e-6);
        let m = match_system(fs - 33.7561e6, fs, 200.0).unwrap();
        assert_eq!(m.system.name, "ISDB-S3");
        assert!(match_system(30.0e6, fs, 200.0).is_none());
    }
}

//! The transmit pulse shaper: symbols at one sample per symbol in, IQ
//! samples at the radio's rate out, root-raised-cosine shaped. It is the
//! inverse of the receive front end ([`crate::fixed`], [`crate::fast`]):
//! the receiver's matched filter is the same pulse
//! ([`tables::rrc`](crate::tables::rrc)), so the cascade is a raised
//! cosine and free of intersymbol interference.
//!
//! ITU-R BO.1408-1 Annex 1 §9 (and MIC Ordinance No. 87 of 2011 Art.
//! 52(5), annexed table 47) prescribe the square-root raised cosine
//! filter, roll-off 0.35, ahead of the ISDB-S modulator; ARIB STD-B44
//! v2.1 §3.10 gives ISDB-S3 its band limitation with roll-off 0.03. The
//! shaper takes the roll-off and both rates from the caller
//! (`isdb_iq::survey::KNOWN_SYSTEMS` has them), so it is
//! system-independent.
//!
//! # Signal model
//!
//! With `sps` = sample rate / symbol rate (any real value ≥ 1 + α, so
//! fractional: ISDB-S at 40 Msps is 1.386 samples per symbol), symbols
//! `a_k` (k = 0, 1, …) and the pulse `p(τ) = rrc(τ, α)` for |τ| < `span`,
//! 0 elsewhere (τ in symbol periods), output sample `n` is
//!
//! ```text
//! y[n] = G · Σ_k a_k · p(u_n − span − k),   u_n = n / sps
//! ```
//!
//! The output is delayed by `span` symbols so that it is causal: symbol
//! k's pulse starts at u = k and peaks at u = k + span, i.e. at sample
//! (k + span)·sps. Sample n with u_n in [m, m + 1) needs symbols
//! m − 2·span + 1 ..= m only, which the delay line holds, so
//! [`Shaper::push`] of symbols k0..k1 (counted from the stream's start)
//! appends exactly the samples with u_n in [k0, k1): the interval those
//! symbols cover. [`Shaper::finish`] appends the rest, the samples of
//! 2·span − 1 zero symbols: the last symbol's pulse ends at
//! u = N − 1 + 2·span for a stream of N symbols.
//!
//! The sample position is integer arithmetic: u_n = n·step / 2⁶³ with
//! `step` = round(2⁶³ / sps), kept as the symbol count plus a 63-bit
//! fraction. It never drifts (the realised rate is within 2⁻⁴⁸ of the
//! requested one, as `sps` ≤ [`MAX_SPS`]), the samples of a stream do not
//! depend on how it is cut into calls, and [`Shaper::samples_for`] counts
//! them exactly: n·step < N·2⁶³ for a sample emitted by N symbols. A
//! superframe of ISDB-S at 40 Msps (319 488 symbols) is 442 810.81
//! samples, so consecutive superframes take 442 810 or 442 811.
//!
//! # The bank
//!
//! [`PHASES`] + 1 rows of 2·`span` taps: row q, tap t (t = 0 the newest
//! symbol) is `G·p(q / PHASES + t − span)`. A sample at fraction f of its
//! symbol uses row q = round(f·[`PHASES`]) (the nearest phase; row
//! [`PHASES`] is row 0 one tap later, so rounding up needs no look-ahead).
//! The nearest phase moves a sample by at most 1/2 048 of a symbol, a
//! timing jitter of RMS 1/(1 024·√12) symbol on a signal whose derivative
//! has about twice its RMS: near −65 dB, below the pulse's own ISI.
//! Measured with the ISI test below at 1.386 / 2.129 samples per symbol:
//! 256 phases 53.3 / 54.6 dB, 1 024 phases 57.6 / 58.8 dB, 4 096 phases
//! 58.0 / 58.8 dB (at exactly 2 every bank uses rows 0 and 512 only:
//! 59.1 dB). Linear interpolation between fewer phases would cost a second
//! dot product per sample to win the last 0.4 dB.
//!
//! # The window: none
//!
//! The pulse is the root-raised cosine cut at ±`span` symbols, without
//! the receiver's Kaiser window. A window tapers the pulse, so a pair of
//! windowed pulses is no longer a Nyquist pulse: for α = 0.35 and span 16
//! the ISI of the cascade with the same pulse as matched filter (the MER
//! bound of random symbols at the exact instants) is 58.8 dB truncated,
//! 57.1 dB under Kaiser β = 2 and 47.9 dB under the receiver's β = 5; the
//! window buys only far sidelobes (−52 dB truncated, −85 dB with β = 5,
//! beyond the band edge + R/20) that neither a loopback nor a cabled
//! bench needs (`examples/shapewin.rs`). At α = 0.03 (ISDB-S3) the tails
//! decay slowly: span 16 gives 27 dB, 32 gives 45 dB, 64 gives 57 dB
//! ([`Shaper::with_span`]).
//!
//! # Scaling
//!
//! `G = (Σ_{q < PHASES} Σ_t p(q / PHASES + t − span)² / PHASES)^(−1/2)`:
//! the mean energy of a bank row is the pulse's energy per symbol
//! period, ∫ p(τ)² dτ (equivalently the energy of the pulse sampled at
//! `sps`, Σ_n p(n / sps)², divided by `sps`), so independent zero-mean
//! symbols of unit power give an output RMS of 1 over the phases the
//! samples visit. (The power of the row of phase f is
//! Σ_j p(f − j)² = ∫p² + 2·Re Σ_{k ≥ 1} P₂(k)·e^{2πikf}, P₂ the Fourier
//! transform of p², which the truncation aside is non-zero for k < 1 + α
//! only; the k = 1 term averages out over the phases of any sps > 1.) The
//! products of different symbols average out too, and fast: their weight
//! at lag d, averaged over the phases, is the cascade p⋆p at d, which
//! is a Nyquist pulse (zero at every d ≠ 0), so the RMS of a finite
//! stream is 1 to within 10⁻⁵ rather than 1/√N. Symbols of any other
//! scale give a proportional output.
//!
//! # Arithmetic
//!
//! Coefficients (with `G`) and the delay line in `f32`, the delay line's
//! I and Q apart, each dot product in eight `f32` lanes summed in a fixed
//! order: the bits of a sample depend on the symbols and its phase only.
//! `f32` rounding (about 2⁻²⁴ per term) is some 70 dB below the ISI.
//!
//! # Measured (`tests/shape.rs`)
//!
//! Span 16, α = 0.35, random 8PSK through the shaper, then an ideal
//! matched filter (the same pulse, in `f64`) at the exact symbol
//! instants: MER 57.6 dB at 40 Msps (1.386 samples per symbol for
//! 28.86 Mbaud), 59.1 dB at 57.72 Msps (2), 58.8 dB at 61.44 Msps
//! (2.129), 57.6 dB at (1 + α)·R = 38.961 Msps itself and 58.8 dB at
//! 100 Msps (the continuous cascade's bound is 58.8 dB; the sampled
//! filter differs from it by the aliased leakage of the truncation).
//! Accumulating in `f64` instead changes none of these by 0.01 dB. Output
//! RMS 1 within 10⁻⁵. Through the real receive front end
//! ([`crate::fast::FastFrontend`], ISDB-S at 40 Msps as the receivers
//! design it, trim 0, RMS 0.3 of the 12-bit input) the 8PSK MER after
//! the loop settles is 41.2 dB before and 36.2 dB after the 6-bit output
//! quantiser, counting the receiver's outputs over the `finish` tail
//! (zero symbols, which `metrics::psk_mer` counts as errors); in the
//! steady state 44.5 and 37.0 dB (`examples/txloop.rs`).

use crate::tables::rrc;
use num_complex::Complex32;

pub mod fixed;

/// Phases of the bank (rows, plus one: row [`PHASES`] is row 0 one tap
/// later).
pub const PHASES: usize = 1024;

/// Symbols on each side of the pulse by default.
pub const DEFAULT_SPAN: usize = 16;

/// Largest span [`Shaper::with_span`] takes (a bank of 2 MiB).
pub const MAX_SPAN: usize = 256;

/// Largest samples per symbol (keeps the 63-bit step within 2⁻⁴⁸ of the
/// requested rate).
pub const MAX_SPS: f64 = 65_536.0;

/// One symbol period in the position's fraction: 2⁶³.
const UNIT: u64 = 1 << 63;

/// log2 of [`PHASES`].
const PHASE_BITS: u32 = 10;

/// The fraction's shift to a phase index.
const PHASE_SHIFT: u32 = 63 - PHASE_BITS;

/// Lanes of the dot product (the taps are padded to a multiple).
const LANES: usize = 8;

/// Relative tolerance of the rate check: a sample rate within 10⁻⁹ of
/// (1 + α)·R passes.
const RATE_TOLERANCE: f64 = 1e-9;

/// Why [`Shaper::new`] or [`Shaper::with_span`] refused its arguments.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum ShapeError {
    /// A rate that is not finite and positive, or samples per symbol
    /// below 1 + roll-off (the occupied band would alias) or above
    /// [`MAX_SPS`].
    #[error(
        "pulse shaper: {sample_rate} Hz for {symbol_rate} baud at roll-off {roll_off}: needs finite positive rates and (1 + roll-off) ≤ samples per symbol ≤ {MAX_SPS}"
    )]
    Rate {
        /// The sample rate, Hz.
        sample_rate: f64,
        /// The symbol rate, baud.
        symbol_rate: f64,
        /// The roll-off.
        roll_off: f64,
    },
    /// A roll-off outside 0 < α ≤ 1 (or not finite).
    #[error("pulse shaper: roll-off {0}: 0 < roll-off ≤ 1")]
    RollOff(f64),
    /// A span of 0 or above [`MAX_SPAN`] symbols.
    #[error("pulse shaper: span {0} symbols: 1..={MAX_SPAN}")]
    Span(usize),
}

/// The root-raised-cosine pulse shaper (module documentation; ITU-R
/// BO.1408-1 Annex 1 §9 for ISDB-S).
///
/// ```
/// use isdb_frontend::shape::Shaper;
/// use num_complex::Complex32;
///
/// let mut sh = Shaper::new(40e6, 28.86e6, 0.35)?; // ISDB-S at 40 Msps
/// let symbols = vec![Complex32::new(1.0, 0.0); 1000];
/// let mut iq = Vec::new();
/// sh.push(&symbols, &mut iq);
/// assert_eq!(iq.len(), 1387); // ⌈1000 · 40 / 28.86⌉
/// assert_eq!(sh.samples_for(1000), 1387);
/// sh.finish(&mut iq); // the last pulses' tail: 31 symbols' worth
/// assert_eq!(iq.len() as u64, sh.samples_for(1000 + 31));
/// # Ok::<(), isdb_frontend::shape::ShapeError>(())
/// ```
#[derive(Debug, Clone)]
pub struct Shaper {
    sample_rate: f64,
    symbol_rate: f64,
    roll_off: f64,
    span: usize,
    /// 2·span rounded up to [`LANES`]: the taps of a row and the length of
    /// the delay line.
    width: usize,
    /// Position step per output sample, 2⁻⁶³ symbol units.
    step: u64,
    /// `G`.
    gain: f64,
    /// [`PHASES`] + 1 rows of `width` coefficients (zero beyond 2·span).
    bank: Vec<f32>,
    /// The delay line twice over (`2·width`): the newest symbol at `head`
    /// and `head + width`, so `re[head..head + width]` is the line, newest
    /// first.
    re: Vec<f32>,
    im: Vec<f32>,
    head: usize,
    /// Fraction of the next sample's position within the newest symbol,
    /// 2⁻⁶³ units.
    frac: u64,
    /// Symbols pushed since the stream started.
    symbols: u64,
    /// Samples emitted since the stream started.
    samples: u64,
}

impl Shaper {
    /// A shaper from `symbol_rate` baud to `sample_rate` Hz with roll-off
    /// `roll_off` (0 < α ≤ 1), span [`DEFAULT_SPAN`]:
    /// `sample_rate / symbol_rate` ≥ 1 + `roll_off` (within 10⁻⁹) and ≤
    /// [`MAX_SPS`], else [`ShapeError::Rate`]. BO.1408-1 Annex 1 §9.
    pub fn new(sample_rate: f64, symbol_rate: f64, roll_off: f64) -> Result<Self, ShapeError> {
        if !(roll_off.is_finite() && roll_off > 0.0 && roll_off <= 1.0) {
            return Err(ShapeError::RollOff(roll_off));
        }
        let rate = ShapeError::Rate {
            sample_rate,
            symbol_rate,
            roll_off,
        };
        if !(sample_rate.is_finite()
            && symbol_rate.is_finite()
            && sample_rate > 0.0
            && symbol_rate > 0.0)
        {
            return Err(rate);
        }
        let sps = sample_rate / symbol_rate;
        if !(sps >= (1.0 + roll_off) * (1.0 - RATE_TOLERANCE) && sps <= MAX_SPS) {
            return Err(rate);
        }
        // 1 − 10⁻⁹ < sps ≤ 2¹⁶, so 2⁴⁷ ≤ 2⁶³ / sps < 2⁶⁴: the rounded value
        // fits u64 exactly; `min` below keeps step ≤ 2⁶³ (at most one
        // symbol per sample).
        #[allow(clippy::cast_possible_truncation)]
        let step = (UNIT as f64 / sps).round() as u64;
        Ok(Self::build(
            sample_rate,
            symbol_rate,
            roll_off,
            step.min(UNIT),
            DEFAULT_SPAN,
        ))
    }

    /// The same with `span` symbols on each side of the pulse (1..=
    /// [`MAX_SPAN`], else [`ShapeError::Span`]); the stream restarts.
    pub fn with_span(self, span: usize) -> Result<Self, ShapeError> {
        if !(1..=MAX_SPAN).contains(&span) {
            return Err(ShapeError::Span(span));
        }
        Ok(Self::build(
            self.sample_rate,
            self.symbol_rate,
            self.roll_off,
            self.step,
            span,
        ))
    }

    /// The shaper for validated settings, at the start of a stream.
    fn build(sample_rate: f64, symbol_rate: f64, roll_off: f64, step: u64, span: usize) -> Self {
        let width = (2 * span).div_ceil(LANES) * LANES;
        let rows = bank_rows(roll_off, span);
        let gain = gain_of(&rows);
        let mut bank = vec![0.0f32; (PHASES + 1) * width];
        for (dst, row) in bank.chunks_exact_mut(width).zip(&rows) {
            for (d, &c) in dst.iter_mut().zip(row) {
                // DSP coefficient: |c·G| < 2, rounded to f32 on purpose.
                #[allow(clippy::cast_possible_truncation)]
                let v = (c * gain) as f32;
                *d = v;
            }
        }
        Self {
            sample_rate,
            symbol_rate,
            roll_off,
            span,
            width,
            step,
            gain,
            bank,
            re: vec![0.0; 2 * width],
            im: vec![0.0; 2 * width],
            head: 0,
            frac: 0,
            symbols: 0,
            samples: 0,
        }
    }

    /// Output samples per symbol: sample rate / symbol rate.
    pub fn sps(&self) -> f64 {
        self.sample_rate / self.symbol_rate
    }

    /// Symbols on each side of the pulse.
    pub fn span(&self) -> usize {
        self.span
    }

    /// The roll-off.
    pub fn roll_off(&self) -> f64 {
        self.roll_off
    }

    /// The scale `G` applied to the pulse (module documentation,
    /// "Scaling").
    pub fn gain(&self) -> f64 {
        self.gain
    }

    /// The pulse before the gain at `t` symbol periods from its peak:
    /// [`tables::rrc`](crate::tables::rrc)`(t, roll_off)` for |t| < span,
    /// else 0. A receiver's ideal matched filter is this pulse.
    pub fn pulse(&self, t: f64) -> f64 {
        pulse(t, self.roll_off, self.span)
    }

    /// Symbols pushed since the stream started.
    pub fn symbols(&self) -> u64 {
        self.symbols
    }

    /// Samples emitted since the stream started.
    pub fn samples(&self) -> u64 {
        self.samples
    }

    /// Samples a stream of `n` symbols produces through [`Shaper::push`],
    /// however it is cut: the samples n' with n'·step < n·2⁶³, that is
    /// ⌈n·2⁶³ / step⌉ ≈ ⌈n·sps⌉ (saturating at `u64::MAX`). The tail of
    /// [`Shaper::finish`] is `samples_for(n + 2·span − 1) − samples_for(n)`
    /// more (none for an empty stream).
    pub fn samples_for(&self, n: u64) -> u64 {
        let total = (u128::from(n) << 63).div_ceil(u128::from(self.step));
        u64::try_from(total).unwrap_or(u64::MAX)
    }

    /// Shape `symbols` (any scale) and append the samples of the interval
    /// they cover to `out` (module documentation, "Signal model"); the
    /// fractional phase is kept across calls. Output RMS 1 for
    /// independent, zero-mean, unit-power symbols.
    pub fn push(&mut self, symbols: &[Complex32], out: &mut Vec<Complex32>) {
        // Samples of these symbols: n' ≥ 0 with frac + n'·step < k·2⁶³.
        let k = u128::try_from(symbols.len()).unwrap_or(u128::MAX);
        if k > 0 {
            let more = ((k << 63) - u128::from(self.frac)).div_ceil(u128::from(self.step));
            out.reserve(usize::try_from(more).unwrap_or(usize::MAX));
        }
        for &s in symbols {
            self.push_one(s, out);
        }
    }

    /// Append the tail (the samples of 2·span − 1 zero symbols, where the
    /// last symbols' pulses end; nothing for an empty stream) and reset to
    /// the start of a new stream.
    pub fn finish(&mut self, out: &mut Vec<Complex32>) {
        if self.symbols > 0 {
            for _ in 0..2 * self.span - 1 {
                self.push_one(Complex32::new(0.0, 0.0), out);
            }
        }
        self.reset();
    }

    /// Drop the stream (delay line, phase and counts) without its tail.
    pub fn reset(&mut self) {
        self.re.fill(0.0);
        self.im.fill(0.0);
        self.head = 0;
        self.frac = 0;
        self.symbols = 0;
        self.samples = 0;
    }

    /// One symbol in, the samples of its interval out.
    fn push_one(&mut self, s: Complex32, out: &mut Vec<Complex32>) {
        let w = self.width;
        self.head = (self.head + w - 1) % w;
        self.re[self.head] = s.re;
        self.re[self.head + w] = s.re;
        self.im[self.head] = s.im;
        self.im[self.head + w] = s.im;
        self.symbols += 1;
        let re = &self.re[self.head..self.head + w];
        let im = &self.im[self.head..self.head + w];
        // frac < 2⁶³ here and step ≤ 2⁶³: every sum below stays under 2⁶⁴.
        loop {
            // The nearest phase, 0..=PHASES: (frac + 2⁵²) >> 53 ≤ 1 024.
            let q = usize::try_from((self.frac + (1 << (PHASE_SHIFT - 1))) >> PHASE_SHIFT)
                .unwrap_or(PHASES);
            out.push(dot(&self.bank[q * w..(q + 1) * w], re, im));
            self.samples += 1;
            self.frac += self.step;
            if self.frac >= UNIT {
                self.frac -= UNIT;
                break;
            }
        }
    }
}

/// The unscaled bank: [`PHASES`] + 1 rows of `p(q / PHASES + t − span)`,
/// t = 0..2·span.
fn bank_rows(roll_off: f64, span: usize) -> Vec<Vec<f64>> {
    let half = span as f64;
    (0..=PHASES)
        .map(|q| {
            (0..2 * span)
                .map(|t| pulse(q as f64 / PHASES as f64 + t as f64 - half, roll_off, span))
                .collect()
        })
        .collect()
}

/// `G` of `rows` ([`bank_rows`]): the inverse RMS of the first
/// [`PHASES`] rows (module documentation, "Scaling").
fn gain_of(rows: &[Vec<f64>]) -> f64 {
    let energy = rows[..PHASES]
        .iter()
        .map(|r| r.iter().map(|c| c * c).sum::<f64>())
        .sum::<f64>()
        / PHASES as f64;
    energy.sqrt().recip()
}

/// [`Shaper::gain`] for `roll_off` and `span` (it depends on nothing
/// else): the gain [`fixed::TxParams::design`] folds into its banks.
pub(crate) fn rms_gain(roll_off: f64, span: usize) -> f64 {
    gain_of(&bank_rows(roll_off, span))
}

/// The pulse: [`rrc`]`(t, alpha)` for |t| < `span`, else 0.
fn pulse(t: f64, alpha: f64, span: usize) -> f64 {
    if t.abs() < span as f64 {
        rrc(t, alpha)
    } else {
        0.0
    }
}

/// `Σ_t c[t]·(re[t] + j·im[t])` in [`LANES`] lanes, summed in a fixed
/// order (the lengths are equal multiples of [`LANES`]).
#[inline]
fn dot(c: &[f32], re: &[f32], im: &[f32]) -> Complex32 {
    let mut ar = [0.0f32; LANES];
    let mut ai = [0.0f32; LANES];
    let (c, re, im) = (
        c.as_chunks::<LANES>().0,
        re.as_chunks::<LANES>().0,
        im.as_chunks::<LANES>().0,
    );
    for ((c, r), i) in c.iter().zip(re).zip(im) {
        for (((ar, ai), &c), (&r, &i)) in
            ar.iter_mut().zip(ai.iter_mut()).zip(c).zip(r.iter().zip(i))
        {
            *ar += c * r;
            *ai += c * i;
        }
    }
    let fold = |a: [f32; LANES]| ((a[0] + a[4]) + (a[2] + a[6])) + ((a[1] + a[5]) + (a[3] + a[7]));
    Complex32::new(fold(ar), fold(ai))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The gain makes the bank's mean row energy 1, and that energy is the
    /// pulse's ∫ p² dτ (a fine Riemann sum, independent of the bank grid).
    #[test]
    fn gain_is_the_inverse_rms_of_the_pulse() {
        for (alpha, span) in [(0.35, 16), (0.35, 3), (0.03, 64), (1.0, 8)] {
            let s = Shaper::new(61.44e6, 28.86e6, alpha)
                .unwrap()
                .with_span(span)
                .unwrap();
            let w = s.width;
            let mean = s.bank[..PHASES * w]
                .iter()
                .map(|&c| f64::from(c).powi(2))
                .sum::<f64>()
                / PHASES as f64;
            assert!((mean - 1.0).abs() < 1e-6, "α {alpha} span {span}: {mean}");
            let steps = 8192;
            let h = 1.0 / f64::from(steps);
            let n = 2 * span * steps as usize;
            let integral = (0..n)
                .map(|i| s.pulse((i as f64 + 0.5) * h - span as f64).powi(2))
                .sum::<f64>()
                * h;
            let g = integral.sqrt().recip();
            assert!(
                (s.gain() / g - 1.0).abs() < 1e-5,
                "α {alpha} span {span}: G {} against {g}",
                s.gain()
            );
        }
    }

    /// Row q is the pulse at q/PHASES + t − span; row PHASES is row 0 one
    /// tap on (so rounding up to it needs no newer symbol) with a zero
    /// last tap; the padding is zero.
    #[test]
    fn bank_rows() {
        let s = Shaper::new(40e6, 28.86e6, 0.35)
            .unwrap()
            .with_span(5)
            .unwrap();
        let w = s.width;
        assert_eq!(w, 16);
        let row = |q: usize| &s.bank[q * w..(q + 1) * w];
        for t in 0..10 {
            let want = s.gain() * rrc(t as f64 - 5.0 + 0.25, 0.35);
            assert!((f64::from(row(256)[t]) - want).abs() < 1e-6);
        }
        assert_eq!(row(0)[0], 0.0, "τ = −span is outside the pulse");
        assert_eq!(&row(PHASES)[..9], &row(0)[1..10]);
        assert_eq!(row(PHASES)[9], 0.0);
        assert!(
            s.bank
                .chunks_exact(w)
                .all(|r| r[10..].iter().all(|&c| c == 0.0))
        );
    }

    /// The nearest phase: a fraction just below 1 picks row PHASES, one
    /// at 1/2 048 below the midpoint of two rows picks the lower.
    #[test]
    fn phase_rounding() {
        let q = |frac: u64| (frac + (1 << (PHASE_SHIFT - 1))) >> PHASE_SHIFT;
        assert_eq!(q(0), 0);
        assert_eq!(q(UNIT - 1), PHASES as u64);
        assert_eq!(q((1 << PHASE_SHIFT) / 2 - 1), 0);
        assert_eq!(q((1 << PHASE_SHIFT) / 2), 1);
    }

    /// At exactly 2 samples per symbol the step is 2⁶², every symbol
    /// gives two samples, phases 0 and 512.
    #[test]
    fn two_samples_per_symbol_is_exact() {
        let s = Shaper::new(57.72e6, 28.86e6, 0.35).unwrap();
        assert_eq!(s.step, 1 << 62);
        assert_eq!(s.samples_for(319_488), 638_976);
        // ISDB-S at 40 Msps: a superframe is 442 810.81 samples.
        let s = Shaper::new(40e6, 28.86e6, 0.35).unwrap();
        assert_eq!(s.samples_for(319_488), 442_811);
        assert_eq!(
            s.samples_for(6 * 319_488) - s.samples_for(5 * 319_488),
            442_810
        );
    }
}

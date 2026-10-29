//! The bit-exact fixed-point model of the FPGA transmit interpolator
//! (`SymbolsTx` in `fpga/`, TX format version
//! [`txformat::FORMAT_VERSION`]): 4-bit symbol codes
//! ([`crate::txformat`]) in, 12-bit IQ at the DAC rate out, one sample per
//! clock, root-raised-cosine shaped. It is the transmit twin of
//! [`crate::fixed::Frontend`]: the HDL reproduces it bit for bit on the
//! vectors [`crate::vectors::tx_document`] writes, and a host without the
//! block (a file, a radio without it) runs the same arithmetic.
//!
//! # Signal
//!
//! With `step` = round(2²⁴·R / fs) and the codes c_j (j = 0, 1, … in the
//! order they enter), output sample n is, up to the phase grid and the
//! integer rounding,
//!
//! ```text
//! y[n] = D · G · Σ_j a(c_j) · p(u_n − span − 1 − j),   u_n = n·step / 2²⁴
//! ```
//!
//! a(c) the code's point ([`txformat::point`]), p the pulse of
//! [`Shaper`](super::Shaper) (`tables::rrc` cut at ±span, no window), G
//! the shaper's gain (unit-power symbols give RMS |y| = D) and
//! D = 2048·10^(−backoff/20) LSB of the 12-bit DAC. That is the shaper's
//! signal model one symbol later: the block starts with an empty line and
//! the first code enters at the first carry of the phase accumulator, not
//! before sample 0 (code j peaks at u = j + span + 1).
//!
//! # Parameters ([`TxParams::design`])
//!
//! - `step`: Q0.24 symbol periods per sample, 0 < step < 2²⁴ (0xB8B439 at
//!   40 Msps for 28.86 Mbaud, a rate error of −0.028 ppm; 0x800000 at
//!   57.72 Msps and 0x784000 at 61.44 Msps, exact).
//! - Two banks of P = [`PHASES`] rows of 2S taps (S = span, [`SPAN`] = 12
//!   by default), 18-bit signed. Row q, tap t (t = 0 the newest code) is
//!   at x = (q + ½)/P + t − S:
//!   `a[q][t] = round(G·p(x)·D·2^shift)` and
//!   `b[q][t] = round(G·p(x)·D·2^shift / √2)`, both rounded (half away
//!   from zero) from the same `f64` product, evaluated left to right.
//!   Rows q < P/2 are computed and mirrored, so
//!   `a[q][t] == a[P−1−q][2S−1−t]` (and `b`) holds exactly: x of the mirror
//!   is −x. The FPGA stores one copy of each tap pair, so
//!   [`TxParams::validate`] refuses a bank without the symmetry.
//! - `shift`: the largest value with max|a| ≤ 2¹⁷ − 1 (7 at 10 dB backoff,
//!   max|a| = 90 825).
//!
//! # State
//!
//! `frac` (24 bits), a line of 2S codes (newest at index 0), an 8-code
//! unpack register, the counters. START ([`Interpolator::start`]) clears
//! all of them: the line holds zero codes, frac = 0, the register is
//! empty.
//!
//! # Per output sample n, in this order
//!
//! 1. q = frac >> (24 − log2 P) (the top 9 bits at P = 512).
//! 2. For each tap t whose code is a point 8 + k (8..=15):
//!    v = b\[q\]\[t\] if k is odd, else a\[q\]\[t\];
//!    I += sI\[k\]·v, Q += sQ\[k\]·v with
//!    sI = \[1, 1, 0, −1, −1, −1, 0, 1\], sQ = \[0, 1, 1, 1, 0, −1, −1, −1\]
//!    (the point e^{jkπ/4} is (sI, sQ) for even k and (sI, sQ)/√2 for odd
//!    k, so one coefficient per tap serves both components). Zero and
//!    reserved codes add nothing. |sum| < 2S·2¹⁷ ≤ 2²², so `i32` holds
//!    it (24 bits in the FPGA).
//! 3. out = clamp(round_shift(sum, shift), −2048, 2047) per component,
//!    `round_shift` as [`crate::fixed::round_shift`] (add half, arithmetic
//!    shift). The DAC word is out << 4 (`dac_data[15:4]`).
//! 4. frac += step; on carry (frac ≥ 2²⁴): frac −= 2²⁴, the line shifts by
//!    one and the next code enters at index 0, used from sample n + 1.
//!
//! So sample 0 is 0 and the first code enters at the first carry.
//!
//! # Input: words, the unpack register, stalls
//!
//! The next code comes from the unpack register, which holds one DMA word
//! (eight codes, [`txformat::codes`]). When a code is needed and the
//! register is empty, the block requests a word. If the DMA has one, it
//! is delivered: its eight codes enter one per carry, each counted in
//! CONSUMED (zero codes included). If it has none (a stall), the register
//! takes eight zero symbols instead, each counted in MISSED as it enters;
//! the first such request latches FIRST_MISS = CONSUMED and sets the
//! sticky underrun flag. The next request asks again for the same word.
//! [`Interpolator::stall`]`(n)` makes the next n requests before the next
//! word pushed find no word: the vectors' stall `(word_index, n)`. A
//! request that finds the queue empty is a stall too, as in the hardware.
//!
//! # Measured (`tests/txfixed.rs`, `examples/txloop.rs`)
//!
//! ISDB-S at 40 Msps, 10 dB backoff, P 512, span 12: random 8PSK through
//! an ideal `f64` matched filter at the exact symbol instants of the
//! realised rate, MER 54.38 dB (the f32 shaper at span 16: 57.6 dB);
//! against the f32 shaper at span 12, SNR 56.8 dB; through
//! [`crate::fast::FastFrontend`] (the receivers' design, trim 0) 8PSK MER
//! 36.96 dB at the 6-bit output and 44.41 dB before the quantiser in the
//! steady state (the f32 shaper: 36.98 and 44.54 dB). A component is at
//! most Σ|a| / 2^shift = 1 042 LSB over the worst row (measured peak 998
//! over 200 000 symbols), so nothing clips at backoffs of 4.2 dB or more.

use crate::control::tx::TxIdentity;
use crate::fixed::round_shift;
use crate::txformat::{self, CODES_PER_WORD, CodePacker};
use isdb_iq::survey::KnownSystem;
use std::collections::VecDeque;

/// Phases of the banks by default (rows; the FPGA's).
pub const PHASES: usize = 512;
/// Most phases the register map holds (bank rows at `q·32`, 0x4000 apart).
pub const MAX_PHASES: usize = 512;
/// Symbols on each side of the pulse by default (24 taps).
pub const SPAN: usize = 12;
/// Most taps per row the register map holds (row stride 32).
pub const MAX_TAPS: usize = 32;
/// Fraction bits of the symbol-phase accumulator `frac`.
pub const FRAC_BITS: u32 = 24;
/// One symbol period in `frac` units: 2²⁴.
pub const ONE: u32 = 1 << FRAC_BITS;
/// Width of a bank coefficient (signed).
pub const COEF_BITS: u32 = 18;
/// Largest coefficient magnitude: 2¹⁷ − 1.
pub const COEF_MAX: i32 = (1 << (COEF_BITS - 1)) - 1;
/// Smallest output component (12-bit DAC).
pub const OUT_MIN: i16 = -2048;
/// Largest output component.
pub const OUT_MAX: i16 = 2047;
/// Left shift from the 12-bit output to the 16-bit DAC word.
pub const DAC_SHIFT: u32 = 4;
/// Largest `shift` (the sums are below 2²³).
pub const MAX_SHIFT: u8 = 23;
/// Output RMS of unit-power symbols at 0 dB backoff, LSB: the 12-bit full
/// scale (the CLI's convention, `input_sample` ×2048).
pub const FULL_SCALE: f64 = 2048.0;
/// Backoff of [`TxDesign::new`], dB below [`FULL_SCALE`].
pub const DEFAULT_BACKOFF_DB: f64 = 10.0;

/// sI of the module documentation: the I sign of point k.
const SIGN_I: [i32; 8] = [1, 1, 0, -1, -1, -1, 0, 1];
/// sQ: the Q sign of point k.
const SIGN_Q: [i32; 8] = [0, 1, 1, 1, 0, -1, -1, -1];

/// Relative tolerance of the rate check, as [`super::Shaper::new`]'s.
const RATE_TOLERANCE: f64 = 1e-9;

/// What [`TxParams::design`] needs.
#[derive(Debug, Clone, PartialEq)]
pub struct TxDesign {
    /// DAC sample rate, Hz.
    pub sample_rate: f64,
    /// Symbol rate, baud.
    pub symbol_rate: f64,
    /// Roll-off of the root-raised cosine.
    pub roll_off: f64,
    /// Symbols on each side of the pulse (1..=16; taps = 2·span).
    pub span: usize,
    /// Rows of the banks (a power of two, 2..=[`MAX_PHASES`]).
    pub phases: usize,
    /// RMS of |y| below the 12-bit full scale, dB (10 dB: 647.6 LSB).
    pub backoff_db: f64,
}

impl TxDesign {
    /// The FPGA's geometry ([`PHASES`], [`SPAN`]) at
    /// [`DEFAULT_BACKOFF_DB`].
    pub fn new(sample_rate: f64, symbol_rate: f64, roll_off: f64) -> Self {
        Self {
            sample_rate,
            symbol_rate,
            roll_off,
            span: SPAN,
            phases: PHASES,
            backoff_db: DEFAULT_BACKOFF_DB,
        }
    }

    /// [`TxDesign::new`] for a known system (`isdb_iq::survey`: ISDB-S is
    /// 28.86 Mbaud, roll-off 0.35, BO.1408-1 Annex 1 §9) at
    /// `sample_rate`.
    pub fn for_system(system: &KnownSystem, sample_rate: f64) -> Self {
        Self::new(sample_rate, system.symbol_rate_hz, system.roll_off)
    }

    /// Samples per symbol.
    pub fn sps(&self) -> f64 {
        self.sample_rate / self.symbol_rate
    }

    /// D: the output RMS of unit-power symbols, LSB of the 12-bit DAC.
    pub fn dac_rms(&self) -> f64 {
        FULL_SCALE * 10f64.powf(-self.backoff_db / 20.0)
    }
}

/// Why a [`TxDesign`] or [`TxParams`] is refused: the rule that failed,
/// with the values it saw.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum TxParamError {
    /// A rate that is not finite and positive, or samples per symbol
    /// below 1 + roll-off (the occupied band would alias).
    #[error(
        "transmit interpolator: {sample_rate} Hz for {symbol_rate} baud at roll-off {roll_off}: needs finite positive rates and samples per symbol ≥ 1 + roll-off"
    )]
    Rate {
        /// The sample rate, Hz.
        sample_rate: f64,
        /// The symbol rate, baud.
        symbol_rate: f64,
        /// The roll-off.
        roll_off: f64,
    },
    /// A roll-off outside 0 < α ≤ 1.
    #[error("transmit interpolator: roll-off {0}: 0 < roll-off ≤ 1")]
    RollOff(f64),
    /// A span outside 1..=16 (taps 2..=[`MAX_TAPS`]).
    #[error("transmit interpolator: span {0} symbols: 1..=16")]
    Span(usize),
    /// Phases that are not a power of two in 2..=[`MAX_PHASES`].
    #[error("transmit interpolator: {0} phases: a power of two, 2..={MAX_PHASES}")]
    Phases(usize),
    /// A backoff that is not finite, or so low that no shift keeps the
    /// coefficients within [`COEF_BITS`] bits.
    #[error("transmit interpolator: backoff {0} dB: finite, and the bank must fit 18 bits")]
    Backoff(f64),
    /// A step outside 0 < step < 2²⁴ (at most one symbol per sample).
    #[error("transmit interpolator: step {0:#x}: 0 < step < 2^24")]
    Step(u32),
    /// A shift above [`MAX_SHIFT`].
    #[error("transmit interpolator: shift {0}: 0..={MAX_SHIFT}")]
    Shift(u8),
    /// Banks whose row counts differ or are not a power of two in
    /// 2..=[`MAX_PHASES`].
    #[error(
        "transmit interpolator: banks of {a} / {b} rows: equal, a power of two, 2..={MAX_PHASES}"
    )]
    BankRows {
        /// Rows of `a`.
        a: usize,
        /// Rows of `b`.
        b: usize,
    },
    /// A first row of `a` whose tap count is odd or outside 2..=[`MAX_TAPS`].
    #[error("transmit interpolator: {0} taps: even, 2..={MAX_TAPS}")]
    Taps(usize),
    /// A row whose tap count differs from the first row of `a`.
    #[error("transmit interpolator: ragged bank: {bank}[{row}] has {len} taps, {taps} expected")]
    RaggedBank {
        /// `a` or `b`.
        bank: &'static str,
        /// The row.
        row: usize,
        /// Its taps.
        len: usize,
        /// The taps of `a[0]`.
        taps: usize,
    },
    /// A coefficient outside [`COEF_BITS`] signed bits.
    #[error("transmit interpolator: coefficient {value} at {bank}[{row}][{tap}] outside 18 bits")]
    Coefficient {
        /// `a` or `b`.
        bank: &'static str,
        /// The row.
        row: usize,
        /// The tap.
        tap: usize,
        /// The coefficient.
        value: i32,
    },
    /// A coefficient that differs from its mirror
    /// `bank[P−1−row][2S−1−tap]`: the FPGA stores one copy of each pair.
    #[error(
        "transmit interpolator: {bank}[{row}][{tap}] = {value} but its mirror {bank}[{mirror_row}][{mirror_tap}] = {mirror}: the bank must be symmetric"
    )]
    Asymmetric {
        /// `a` or `b`.
        bank: &'static str,
        /// The row.
        row: usize,
        /// The tap.
        tap: usize,
        /// The coefficient.
        value: i32,
        /// P − 1 − row.
        mirror_row: usize,
        /// 2S − 1 − tap.
        mirror_tap: usize,
        /// The mirror's coefficient.
        mirror: i32,
    },
}

/// The interpolator's registers: everything a host writes before START
/// (crate README, "The TX register map"; [`crate::control::tx`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TxParams {
    /// Symbol periods per output sample, Q0.24.
    pub step: u32,
    /// Right shift (with rounding) from the sums to the 12-bit output.
    pub shift: u8,
    /// Bank for points with even k: `a[q][t]`, P rows of 2S taps.
    pub a: Vec<Vec<i32>>,
    /// Bank for points with odd k: `b[q][t]` = `a`'s value / √2, rounded
    /// from the same `f64`.
    pub b: Vec<Vec<i32>>,
}

impl TxParams {
    /// The registers for `d` (module documentation, "Parameters").
    pub fn design(d: &TxDesign) -> Result<Self, TxParamError> {
        let (fs, rs, alpha) = (d.sample_rate, d.symbol_rate, d.roll_off);
        if !(alpha.is_finite() && alpha > 0.0 && alpha <= 1.0) {
            return Err(TxParamError::RollOff(alpha));
        }
        let rate = TxParamError::Rate {
            sample_rate: fs,
            symbol_rate: rs,
            roll_off: alpha,
        };
        if !(fs.is_finite() && rs.is_finite() && fs > 0.0 && rs > 0.0) {
            return Err(rate);
        }
        // Finite positive rates: the ratio is not NaN.
        if d.sps() < (1.0 + alpha) * (1.0 - RATE_TOLERANCE) {
            return Err(rate);
        }
        if !(1..=MAX_TAPS / 2).contains(&d.span) {
            return Err(TxParamError::Span(d.span));
        }
        if !(d.phases.is_power_of_two() && (2..=MAX_PHASES).contains(&d.phases)) {
            return Err(TxParamError::Phases(d.phases));
        }
        if !d.backoff_db.is_finite() {
            return Err(TxParamError::Backoff(d.backoff_db));
        }
        // sps ≥ 1 + α − 10⁻⁹ > 1, so 0 < 2²⁴·R/fs < 2²⁴: the rounded value
        // fits u32 (and is checked below).
        #[allow(clippy::cast_possible_truncation)]
        let step = (f64::from(ONE) * rs / fs).round() as u32;
        if step == 0 || step >= ONE {
            return Err(TxParamError::Step(step));
        }
        let (p, span) = (d.phases, d.span);
        let taps = 2 * span;
        let g = super::rms_gain(alpha, span);
        let dac = d.dac_rms();
        // G·p(x)·D of rows q < P/2; the other half is the mirror.
        let half: Vec<Vec<f64>> = (0..p / 2)
            .map(|q| {
                (0..taps)
                    .map(|t| {
                        let x = (q as f64 + 0.5) / p as f64 + t as f64 - span as f64;
                        g * super::pulse(x, alpha, span) * dac
                    })
                    .collect()
            })
            .collect();
        let peak = half.iter().flatten().fold(0.0f64, |m, v| m.max(v.abs()));
        let fits = |s: u8| (peak * f64::from(1u32 << s)).round() <= f64::from(COEF_MAX);
        let shift = (0..=MAX_SHIFT)
            .rev()
            .find(|&s| fits(s))
            .ok_or(TxParamError::Backoff(d.backoff_db))?;
        let scale = f64::from(1u32 << shift);
        // |v·2^shift| ≤ 2¹⁷ − 1 (the shift's rule), so the rounded values
        // fit i32.
        #[allow(clippy::cast_possible_truncation)]
        let quantise = |v: f64, root2: bool| {
            let x = v * scale;
            (if root2 {
                x / std::f64::consts::SQRT_2
            } else {
                x
            })
            .round() as i32
        };
        let mut a = vec![vec![0i32; taps]; p];
        let mut b = vec![vec![0i32; taps]; p];
        for (q, row) in half.iter().enumerate() {
            for (t, &v) in row.iter().enumerate() {
                let (va, vb) = (quantise(v, false), quantise(v, true));
                a[q][t] = va;
                b[q][t] = vb;
                a[p - 1 - q][taps - 1 - t] = va;
                b[p - 1 - q][taps - 1 - t] = vb;
            }
        }
        let params = Self { step, shift, a, b };
        params.validate()?;
        Ok(params)
    }

    /// Rows of the banks, P.
    pub fn phases(&self) -> usize {
        self.a.len()
    }

    /// log2 of [`TxParams::phases`].
    pub fn phase_bits(&self) -> u32 {
        self.phases().trailing_zeros()
    }

    /// Taps per row, 2S.
    pub fn taps(&self) -> usize {
        self.a.first().map_or(0, Vec::len)
    }

    /// Symbols on each side of the pulse, S.
    pub fn span(&self) -> usize {
        self.taps() / 2
    }

    /// The ID word of a block with these parameters' geometry and the
    /// model's format version ([`TxIdentity`]): 0xB5D1_1918 for P 512,
    /// 24 taps.
    pub fn id_word(&self) -> u32 {
        TxIdentity::of(self).word()
    }

    /// The realised samples per symbol, 2²⁴ / step.
    pub fn sps(&self) -> f64 {
        f64::from(ONE) / f64::from(self.step)
    }

    /// Check the invariants the arithmetic and the FPGA rely on: the
    /// geometry, 0 < step < 2²⁴, the shift, 18-bit coefficients and the
    /// symmetry of both banks.
    pub fn validate(&self) -> Result<(), TxParamError> {
        let p = self.a.len();
        if p != self.b.len() || !p.is_power_of_two() || !(2..=MAX_PHASES).contains(&p) {
            return Err(TxParamError::BankRows {
                a: p,
                b: self.b.len(),
            });
        }
        let taps = self.taps();
        if !(2..=MAX_TAPS).contains(&taps) || !taps.is_multiple_of(2) {
            return Err(TxParamError::Taps(taps));
        }
        if self.step == 0 || self.step >= ONE {
            return Err(TxParamError::Step(self.step));
        }
        if self.shift > MAX_SHIFT {
            return Err(TxParamError::Shift(self.shift));
        }
        for (bank, rows) in [("a", &self.a), ("b", &self.b)] {
            for (row, r) in rows.iter().enumerate() {
                if r.len() != taps {
                    return Err(TxParamError::RaggedBank {
                        bank,
                        row,
                        len: r.len(),
                        taps,
                    });
                }
                if let Some((tap, &value)) = r
                    .iter()
                    .enumerate()
                    .find(|&(_, &c)| !(-COEF_MAX - 1..=COEF_MAX).contains(&c))
                {
                    return Err(TxParamError::Coefficient {
                        bank,
                        row,
                        tap,
                        value,
                    });
                }
            }
            for (row, r) in rows.iter().enumerate() {
                for (tap, &value) in r.iter().enumerate() {
                    let (mirror_row, mirror_tap) = (p - 1 - row, taps - 1 - tap);
                    let mirror = rows[mirror_row][mirror_tap];
                    if mirror != value {
                        return Err(TxParamError::Asymmetric {
                            bank,
                            row,
                            tap,
                            value,
                            mirror_row,
                            mirror_tap,
                            mirror,
                        });
                    }
                }
            }
        }
        Ok(())
    }
}

/// One output sample and the internals the FPGA test compares
/// ([`Interpolator::output`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TxSample {
    /// Samples output since START before this one.
    pub n: u64,
    /// `frac` before this sample's update.
    pub frac: u32,
    /// Bank row used.
    pub q: u16,
    /// The line used, newest first (`taps` entries).
    line: [u8; MAX_TAPS],
    /// Taps of the line.
    taps: u8,
    /// The sums before rounding, [I, Q].
    pub sum: [i32; 2],
    /// The 12-bit output, [I, Q].
    pub out: [i16; 2],
    /// Whether this sample's update carried (the line shifted).
    pub wrap: bool,
    /// The code entered at that carry (0 for a missed symbol).
    pub code: Option<u8>,
}

impl TxSample {
    /// The line used for this sample, newest code first.
    pub fn line(&self) -> &[u8] {
        &self.line[..usize::from(self.taps)]
    }

    /// The 16-bit DAC words, `out << 4` ([`DAC_SHIFT`]).
    pub fn dac(&self) -> [i16; 2] {
        self.out.map(|v| v << DAC_SHIFT)
    }
}

/// The interpolator (module documentation): codes or DMA words in, 12-bit
/// samples out, with the block's counters.
///
/// ```
/// use isdb_frontend::shape::fixed::{Interpolator, TxDesign, TxParams};
///
/// let p = TxParams::design(&TxDesign::new(40e6, 28.86e6, 0.35))?; // ISDB-S
/// let mut tx = Interpolator::new(p)?;
/// tx.push_codes(&[8, 10, 12, 14, 9, 11, 13, 15]); // eight points: one word
/// let mut y = Vec::new();
/// tx.process(40, &mut y);
/// assert_eq!(y[0], [0, 0]); // the first code enters at the first carry
/// assert_eq!(tx.consumed(), 8);
/// assert!(tx.missed() > 0); // then the queue ran dry: zero symbols
/// # Ok::<(), isdb_frontend::shape::fixed::TxParamError>(())
/// ```
#[derive(Debug, Clone)]
pub struct Interpolator {
    p: TxParams,
    taps: usize,
    /// FRAC_BITS − log2 P.
    q_shift: u32,
    /// The banks row-major: `a[q·taps + t]`.
    a: Vec<i32>,
    b: Vec<i32>,
    frac: u32,
    /// The line twice over (`2·taps`): the newest code at `head` and
    /// `head + taps`, so `buf[head..head + taps]` is the line, newest
    /// first.
    buf: Vec<u8>,
    head: usize,
    /// The unpack register: the remaining codes, the next in the low
    /// nibble.
    reg: u32,
    /// Codes left in it.
    left: usize,
    /// Whether they stand for a stalled request.
    reg_missed: bool,
    /// Words delivered by the DMA and not yet requested.
    words: VecDeque<u32>,
    /// (word index, requests) of the stalls not yet served, by index.
    stalls: VecDeque<(u64, u32)>,
    /// Words pushed so far: the index of the next.
    pushed: u64,
    /// Words delivered to the unpack register so far.
    delivered: u64,
    packer: CodePacker,
    consumed: u64,
    missed: u64,
    first_miss: Option<u64>,
    samples: u64,
}

impl Interpolator {
    /// An interpolator with parameters `p`, after START; the input queue
    /// empty.
    pub fn new(p: TxParams) -> Result<Self, TxParamError> {
        p.validate()?;
        let taps = p.taps();
        let flat = |bank: &[Vec<i32>]| bank.iter().flatten().copied().collect::<Vec<i32>>();
        Ok(Self {
            taps,
            q_shift: FRAC_BITS - p.phase_bits(),
            a: flat(&p.a),
            b: flat(&p.b),
            frac: 0,
            buf: vec![txformat::ZERO; 2 * taps],
            head: 0,
            reg: 0,
            left: 0,
            reg_missed: false,
            words: VecDeque::new(),
            stalls: VecDeque::new(),
            pushed: 0,
            delivered: 0,
            packer: CodePacker::new(),
            consumed: 0,
            missed: 0,
            first_miss: None,
            samples: 0,
            p,
        })
    }

    /// The parameters.
    pub fn params(&self) -> &TxParams {
        &self.p
    }

    /// START: `frac`, the line, the unpack register and the counters to
    /// their initial values. Words queued and not yet requested stay (they
    /// are the DMA's).
    pub fn start(&mut self) {
        self.frac = 0;
        self.buf.fill(txformat::ZERO);
        self.head = 0;
        self.reg = 0;
        self.left = 0;
        self.reg_missed = false;
        self.consumed = 0;
        self.missed = 0;
        self.first_miss = None;
        self.samples = 0;
    }

    /// Queue DMA words ([`txformat::word`] layout) for delivery, in order.
    pub fn push_words(&mut self, words: &[u32]) {
        self.words.extend(words);
        self.pushed += words.len() as u64;
    }

    /// Queue codes: packed eight to a word ([`CodePacker`]); a partial word
    /// is held until more codes or [`Interpolator::flush_codes`] complete
    /// it.
    pub fn push_codes(&mut self, codes: &[u8]) {
        let mut w = Vec::new();
        self.packer.push(codes, &mut w);
        self.push_words(&w);
    }

    /// Queue the codes [`Interpolator::push_codes`] holds as a last word,
    /// padded with zero symbols.
    pub fn flush_codes(&mut self) {
        let mut w = Vec::new();
        self.packer.finish(&mut w);
        self.push_words(&w);
    }

    /// The next `n` requests for the next word pushed find no word: a DMA
    /// stall of `8·n` zero symbols before it (the vectors' stall
    /// `(word_index, n)` with `word_index` the words pushed so far).
    pub fn stall(&mut self, n: u32) {
        if n == 0 {
            return;
        }
        match self.stalls.back_mut() {
            Some((i, m)) if *i == self.pushed => *m += n,
            _ => self.stalls.push_back((self.pushed, n)),
        }
    }

    /// Words queued and not yet requested.
    pub fn queued(&self) -> usize {
        self.words.len()
    }

    /// Whether every word and stall queued has been requested.
    pub fn input_done(&self) -> bool {
        self.words.is_empty() && self.stalls.iter().all(|&(i, _)| i < self.delivered)
    }

    /// Whether the next sample's update will carry and need a word: its
    /// carry finds the unpack register empty.
    pub fn next_requests(&self) -> bool {
        self.left == 0 && self.frac + self.p.step >= ONE
    }

    /// Codes entered from delivered words since START (zero codes
    /// included): CONSUMED.
    pub fn consumed(&self) -> u64 {
        self.consumed
    }

    /// Zero symbols entered for stalled requests since START: MISSED.
    pub fn missed(&self) -> u64 {
        self.missed
    }

    /// CONSUMED at the first stalled request since START (FIRST_MISS),
    /// `None` without one.
    pub fn first_miss(&self) -> Option<u64> {
        self.first_miss
    }

    /// The sticky underrun flag (STATUS bit 2): a request has stalled
    /// since START.
    pub fn underrun(&self) -> bool {
        self.first_miss.is_some()
    }

    /// Samples output since START: SAMPLES.
    pub fn samples(&self) -> u64 {
        self.samples
    }

    /// One output sample, with its internals (module documentation, "Per
    /// output sample").
    pub fn output(&mut self) -> TxSample {
        let (frac, q) = (self.frac, self.row());
        let mut line = [0u8; MAX_TAPS];
        line[..self.taps].copy_from_slice(&self.buf[self.head..self.head + self.taps]);
        let sum = self.sums(q);
        let out = sum.map(|s| self.finish(s));
        let n = self.samples;
        let code = self.advance();
        TxSample {
            n,
            frac,
            // q < P ≤ 512.
            #[allow(clippy::cast_possible_truncation)]
            q: q as u16,
            line,
            // taps ≤ MAX_TAPS = 32.
            #[allow(clippy::cast_possible_truncation)]
            taps: self.taps as u8,
            sum,
            out,
            wrap: code.is_some(),
            code,
        }
    }

    /// `n` output samples appended to `out` (the same as `n`
    /// [`Interpolator::output`]s, without the internals).
    pub fn process(&mut self, n: usize, out: &mut Vec<[i16; 2]>) {
        out.reserve(n);
        for _ in 0..n {
            let sum = self.sums(self.row());
            out.push(sum.map(|s| self.finish(s)));
            self.advance();
        }
    }

    /// Step 1: the bank row.
    #[inline]
    fn row(&self) -> usize {
        (self.frac >> self.q_shift) as usize
    }

    /// Step 2: the sums of row `q` over the line.
    #[inline]
    fn sums(&self, q: usize) -> [i32; 2] {
        let line = &self.buf[self.head..self.head + self.taps];
        let a = &self.a[q * self.taps..(q + 1) * self.taps];
        let b = &self.b[q * self.taps..(q + 1) * self.taps];
        let (mut si, mut sq) = (0i32, 0i32);
        for ((&c, &va), &vb) in line.iter().zip(a).zip(b) {
            if let Some(k) = txformat::angle(c) {
                let k = usize::from(k);
                let v = if k & 1 == 1 { vb } else { va };
                si += SIGN_I[k] * v;
                sq += SIGN_Q[k] * v;
            }
        }
        [si, sq]
    }

    /// Step 3: round, shift and saturate to 12 bits.
    #[inline]
    fn finish(&self, s: i32) -> i16 {
        let v = round_shift(i64::from(s), u32::from(self.p.shift));
        // Clamped to the 12-bit range, so the cast is exact.
        #[allow(clippy::cast_possible_truncation)]
        let o = v.clamp(i64::from(OUT_MIN), i64::from(OUT_MAX)) as i16;
        o
    }

    /// Step 4: the phase update; on carry the line shifts and the next
    /// code enters, which is returned.
    #[inline]
    fn advance(&mut self) -> Option<u8> {
        self.samples += 1;
        self.frac += self.p.step;
        if self.frac < ONE {
            return None;
        }
        self.frac -= ONE;
        let c = self.next_code();
        let w = self.taps;
        self.head = if self.head == 0 { w - 1 } else { self.head - 1 };
        self.buf[self.head] = c;
        self.buf[self.head + w] = c;
        Some(c)
    }

    /// The next code from the unpack register, refilled by a request when
    /// empty.
    fn next_code(&mut self) -> u8 {
        if self.left == 0 {
            self.request();
        }
        let b = self.reg.to_le_bytes()[0];
        self.reg >>= txformat::CODE_BITS;
        self.left -= 1;
        if self.reg_missed {
            self.missed += 1;
        } else {
            self.consumed += 1;
        }
        b & 0xF
    }

    /// A word request: a stall due before the next word, else the next
    /// word, else (the queue empty) a stall too.
    fn request(&mut self) {
        while self
            .stalls
            .front()
            .is_some_and(|&(i, _)| i < self.delivered)
        {
            self.stalls.pop_front();
        }
        let stalled = match self.stalls.front_mut() {
            Some((i, n)) if *i == self.delivered => {
                *n -= 1;
                if *n == 0 {
                    self.stalls.pop_front();
                }
                true
            }
            _ => false,
        };
        self.left = CODES_PER_WORD;
        if !stalled && let Some(w) = self.words.pop_front() {
            self.delivered += 1;
            self.reg = w;
            self.reg_missed = false;
            return;
        }
        if self.first_miss.is_none() {
            self.first_miss = Some(self.consumed);
        }
        self.reg = 0;
        self.reg_missed = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn isdbs() -> TxParams {
        TxParams::design(&TxDesign::new(40e6, 28.86e6, 0.35)).unwrap()
    }

    /// interpolator.md §4: the Q0.24 steps at the radios' rates.
    #[test]
    fn spec_steps() {
        for (fs, step) in [
            (40e6, 0xB8_B439),
            (57.72e6, 0x80_0000),
            (61.44e6, 0x78_4000),
        ] {
            let p = TxParams::design(&TxDesign::new(fs, 28.86e6, 0.35)).unwrap();
            assert_eq!(p.step, step, "{fs}");
        }
    }

    /// Shift 7 at 10 dB: the largest that keeps the bank within 18 bits.
    #[test]
    fn shift_is_the_largest_that_fits() {
        let p = isdbs();
        assert_eq!(p.shift, 7);
        let max = p.a.iter().flatten().map(|v| v.abs()).max().unwrap();
        assert!(max <= COEF_MAX && 2 * max > COEF_MAX, "max |a| {max}");
        // the row nearest the pulse's peak (x = ±1/1024); interpolator.md's
        // 90 813 is P 64's
        assert_eq!(max, 90_825);
        // the largest component a row can give: Σ|a| / 2^shift = 1 042.2 LSB
        let worst =
            p.a.iter()
                .map(|r| r.iter().map(|v| v.abs()).sum::<i32>())
                .max()
                .unwrap();
        assert_eq!(worst, 133_401);
        assert_eq!(
            (p.phases(), p.taps(), p.span(), p.phase_bits()),
            (512, 24, 12, 9)
        );
    }

    /// The geometry and range rules of `design`.
    #[test]
    fn design_refuses() {
        let d = TxDesign::new(40e6, 28.86e6, 0.35);
        let with = |f: &dyn Fn(&mut TxDesign)| {
            let mut x = d.clone();
            f(&mut x);
            TxParams::design(&x)
        };
        assert!(matches!(
            with(&|x| x.sample_rate = 38e6),
            Err(TxParamError::Rate { .. })
        ));
        assert!(matches!(
            with(&|x| x.symbol_rate = f64::NAN),
            Err(TxParamError::Rate { .. })
        ));
        assert_eq!(with(&|x| x.roll_off = 0.0), Err(TxParamError::RollOff(0.0)));
        assert_eq!(with(&|x| x.span = 0), Err(TxParamError::Span(0)));
        assert_eq!(with(&|x| x.span = 17), Err(TxParamError::Span(17)));
        assert_eq!(with(&|x| x.phases = 384), Err(TxParamError::Phases(384)));
        assert_eq!(with(&|x| x.phases = 1024), Err(TxParamError::Phases(1024)));
        assert!(matches!(
            with(&|x| x.backoff_db = f64::INFINITY),
            Err(TxParamError::Backoff(_))
        ));
        assert!(matches!(
            with(&|x| x.backoff_db = -60.0),
            Err(TxParamError::Backoff(_))
        ));
        assert!(with(&|x| x.span = 16).is_ok());
        assert!(with(&|x| x.phases = 2).is_ok());
        assert!(with(&|x| x.sample_rate = 1.35 * 28.86e6).is_ok());
    }
}

//! The front end's fixed architecture constants (format version 2) and its
//! run-time parameters (the registers of the FPGA implementation).

use crate::tables;

/// Version of the architecture and of the [`crate::format`] it emits. Any
/// change to the arithmetic below or to the word layout increments it.
///
/// Version 2 adds the output bank [`Params::k`]: the output symbols (and
/// the AGC) come from it, the timing detector still from `h` and `g`, so
/// the output instant can sit a fixed trim away from where the loop locks
/// (ISDB-S3: crate README, "The timing trim"). The word layout is the
/// same as version 1; with `k == h` the output is version 1's.
pub const FORMAT_VERSION: u16 = 2;
/// Oldest `Symbols` format version a [`crate::format::Reader`] accepts
/// (the layout has not changed since).
pub const MIN_READ_VERSION: u16 = 1;

/// Phases of the polyphase matched-filter banks (the fractional-delay
/// resolution of the timing recovery, 1/64 sample).
pub const PHASES: usize = 64;
/// log2 of [`PHASES`].
pub const PHASE_BITS: u32 = 6;
/// Fractional bits of the timing accumulator and of the symbol period
/// (units of one input sample).
pub const FRAC_BITS: u32 = 24;
/// One input sample in timing units.
pub const ONE: i64 = 1 << FRAC_BITS;
/// Index bits of the mixer's cosine table (1024 entries).
pub const NCO_LUT_BITS: u32 = 10;
/// Coefficients of the filter banks are signed integers of this width
/// (the 18-bit B input of a DSP48E1).
pub const COEF_BITS: u32 = 18;
/// Largest sum of coefficient magnitudes of one phase: keeps every
/// accumulator below 2^29 for 13-bit mixer outputs, so the filter outputs
/// fit `i16` after [`FILTER_SHIFT`].
pub const COEF_SUM_MAX: i64 = (1 << 17) - 1;
/// Right shift (with rounding) from filter accumulator to filter output.
pub const FILTER_SHIFT: u32 = 14;
/// Fractional bits of the AGC gain `G` (Q6.10: 1024 is a gain of 1).
pub const GAIN_FRAC_BITS: u32 = 10;
/// Extra fractional bits of the AGC accumulator below `G`.
pub const GAIN_ACC_EXTRA_BITS: u32 = 8;
/// Width of the AGC accumulator (`G` is its top 16 bits).
pub const GAIN_ACC_BITS: u32 = 24;
/// Right shift from |z|² to the AGC's power measure.
pub const POWER_SHIFT: u32 = 12;
/// Fractional bits of the timing integrator below Q8.24 (a Q8.40
/// register).
pub const KI_FRAC: u32 = 16;
/// Largest `kp_shift` / `ki_shift`.
pub const MAX_GAIN_SHIFT: u8 = 40;
/// Symbols between a symbol and the loop updates that use its error
/// values: symbol k is computed with the gain and period updated by the
/// errors of symbols up to k − `LOOP_DELAY`. This is the pipeline latency
/// the FPGA implementation is allowed: at least 6 input samples (24 clock
/// cycles at 4 cycles per sample) from a symbol's decision to its errors.
pub const LOOP_DELAY: usize = 6;
/// Bound of the timing error value (24-bit signed).
pub const E_MAX: i64 = (1 << 23) - 1;
/// Largest number of taps per phase.
pub const MAX_TAPS: usize = 64;

/// The run-time parameters: everything a host writes before starting the
/// front end. All values are integers; [`Params::design`] derives them
/// from physical quantities.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Params {
    /// Mixer frequency word: the phase added per input sample, in units of
    /// 2π/2³² (wrapping). The mixer multiplies by e^{+jφ}, so removing a
    /// carrier offset Δf takes `−Δf/fs·2³²`.
    pub nco_step: i32,
    /// Matched-filter bank: [`PHASES`] phases of `taps` coefficients
    /// (`h[p][t]` at offset `t − taps/2 + p/PHASES` samples from the
    /// output instant; `t = 0` multiplies the newest sample).
    pub h: Vec<Vec<i32>>,
    /// Derivative bank, same layout (d/dτ of the matched filter, scaled).
    pub g: Vec<Vec<i32>>,
    /// Output bank, same layout: the matched filter evaluated `trim`
    /// later than `h` within the same window (equal to `h` for no trim).
    /// The output symbol and the AGC use it; the timing detector uses `h`
    /// and `g`.
    pub k: Vec<Vec<i32>>,
    /// Nominal symbol period in input samples, Q8.24.
    pub period_nominal: u32,
    /// Smallest and largest period the loop may set, Q8.24 (the smallest
    /// must exceed one sample: at most one symbol per input sample).
    pub period_min: u32,
    /// See `period_min`.
    pub period_max: u32,
    /// Right shift from the timing error `Re{z·conj(dz)}` to the loop's
    /// error value (saturated to ±[`E_MAX`]; 8 keeps every value).
    pub ted_shift: u8,
    /// Proportional gain: the period changes by (kp·e) >> `kp_shift`
    /// Q8.24 units.
    pub kp: i16,
    /// See `kp`.
    pub kp_shift: u8,
    /// Integral gain: the integrator (Q8.24 with [`KI_FRAC`] more
    /// fractional bits) moves by (ki·e) >> `ki_shift` per symbol.
    pub ki: i16,
    /// See `ki`.
    pub ki_shift: u8,
    /// Bound of the integrator, Q8.24 units.
    pub integrator_max: i32,
    /// Initial AGC accumulator (`G` = acc >> [`GAIN_ACC_EXTRA_BITS`]).
    pub agc_init: u32,
    /// AGC target: the mean of (|z|² >> 12).
    pub agc_target: u32,
    /// AGC step: the accumulator moves by error >> `agc_shift` per symbol.
    pub agc_shift: u8,
    /// Bits per output component (2..=8).
    pub bits: u8,
    /// Right shift (with rounding) from `z` (i16) to the output component.
    pub q_shift: u8,
    /// log2 of the symbols per output block (L = 2^`block_log2`, 8..=15).
    pub block_log2: u8,
}

/// What [`Params::design`] needs.
#[derive(Debug, Clone, PartialEq)]
pub struct Design {
    /// Input sample rate, Hz.
    pub sample_rate: f64,
    /// Symbol rate, baud.
    pub symbol_rate: f64,
    /// Roll-off of the root-raised-cosine shaping.
    pub roll_off: f64,
    /// Carrier offset of the input to remove, Hz.
    pub carrier_offset: f64,
    /// Taps per phase (even, ≤ [`MAX_TAPS`]).
    pub taps: usize,
    /// Output instant relative to where the timing loop locks, in symbol
    /// periods (0: none). Measured by the host from known symbols
    /// ([`crate::s3sync::best_trim`]); it shapes the output bank `k`.
    pub trim: f64,
    /// Kaiser window β applied over the filter span (0: none).
    pub kaiser_beta: f64,
    /// Timing loop noise bandwidth as a fraction of the symbol rate.
    pub loop_bandwidth: f64,
    /// Bits per output component.
    pub bits: u8,
    /// RMS of |z| the AGC holds, as a fraction of the i16 full scale
    /// (32768).
    pub agc_rms: f64,
    /// Output component full scale as a multiple of the RMS of |z|: the
    /// quantiser step is the power of two nearest to
    /// `clip`·RMS / 2^(bits−1).
    pub clip: f64,
    /// log2 of the block length in symbols.
    pub block_log2: u8,
}

impl Design {
    /// Defaults: 32 taps, Kaiser β 5, loop bandwidth 2·10⁻⁴ R, 8-bit
    /// output, blocks of 4096 symbols (see also [`Design::for_system`]).
    pub fn new(sample_rate: f64, symbol_rate: f64, roll_off: f64) -> Self {
        Self {
            sample_rate,
            symbol_rate,
            roll_off,
            carrier_offset: 0.0,
            taps: 32,
            trim: 0.0,
            kaiser_beta: 5.0,
            loop_bandwidth: 2e-4,
            bits: 8,
            agc_rms: 0.25,
            clip: 2.0,
            block_log2: 12,
        }
    }

    /// The settings chosen from the measurements on the lab's satellite
    /// IF (crate README, "Design choices") for a known system at
    /// `sample_rate`: 32 taps; loop bandwidth 2·10⁻⁴ R for roll-off ≥ 0.1
    /// and 5·10⁻⁶ R below (the timing detector's slope shrinks with the
    /// roll-off); 6 bits per component for both. No trim: the host
    /// measures it on the signal and sets [`Design::trim`].
    pub fn for_system(system: &isdb_iq::survey::KnownSystem, sample_rate: f64) -> Self {
        let mut d = Self::new(sample_rate, system.symbol_rate_hz, system.roll_off);
        if system.roll_off < 0.1 {
            d.loop_bandwidth = 5e-6;
        }
        d.bits = 6;
        d
    }

    /// Input samples per symbol.
    pub fn sps(&self) -> f64 {
        self.sample_rate / self.symbol_rate
    }
}

/// Why a [`Design`] or [`Params`] is rejected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("front end parameters: {0}")]
pub struct ParamError(pub String);

impl Params {
    /// The parameters for `d`.
    pub fn design(d: &Design) -> Result<Self, ParamError> {
        let sps = d.sps();
        if !(1.0625..4.0).contains(&sps) {
            return Err(ParamError(format!(
                "{sps:.3} samples per symbol: 1.0625..4 supported"
            )));
        }
        if !(4..=MAX_TAPS).contains(&d.taps) || !d.taps.is_multiple_of(2) {
            return Err(ParamError(format!("{} taps: even, 4..={MAX_TAPS}", d.taps)));
        }
        if !(2..=8).contains(&d.bits) {
            return Err(ParamError(format!("{} bits: 2..=8", d.bits)));
        }
        if !(-0.5..=0.5).contains(&d.trim) {
            return Err(ParamError(format!("trim {} symbols: within ±0.5", d.trim)));
        }
        let (h, g) = tables::matched_filter_banks(sps, d.roll_off, d.taps, d.kaiser_beta);
        let k = if d.trim == 0.0 {
            h.clone()
        } else {
            tables::output_bank(sps, d.roll_off, d.taps, d.kaiser_beta, d.trim * sps)
        };
        let period = (sps * ONE as f64).round() as u32;
        // TED gain: the error of the scaled banks for a unit timing offset
        // is measured on a noiseless pulse train (tables::ted_gain); the
        // PI gains then follow from the usual second-order loop with
        // damping 1/√2 and noise bandwidth `loop_bandwidth`·R.
        let ted_shift = 8u8;
        let kd = tables::ted_gain(&h, &g, sps, d.roll_off, d.agc_rms, ted_shift);
        let bn = d.loop_bandwidth;
        let zeta = std::f64::consts::FRAC_1_SQRT_2;
        let theta = bn / (zeta + 1.0 / (4.0 * zeta));
        let denom = 1.0 + 2.0 * zeta * theta + theta * theta;
        // Gains in "samples of period per error unit"; the loop's timing
        // detector slope kd is in error units per sample of offset.
        let kp_f = 4.0 * zeta * theta / denom / kd;
        let ki_f = 4.0 * theta * theta / denom / kd;
        // Each gain as a 15-bit mantissa (8192..16383) and a right shift.
        let split = |g: f64, frac: u32| -> Result<(i16, u8), ParamError> {
            let x = g * 2f64.powi(frac as i32);
            let shift = (16383.0 / x).log2().floor();
            if !(0.0..=f64::from(MAX_GAIN_SHIFT)).contains(&shift) {
                return Err(ParamError(format!(
                    "loop gain {g:.3e} outside the registers' range (TED slope {kd:.3e}); change loop_bandwidth"
                )));
            }
            Ok(((x * 2f64.powi(shift as i32)).round() as i16, shift as u8))
        };
        let (kp, kp_shift) = split(kp_f, FRAC_BITS)?;
        let (ki, ki_shift) = split(ki_f, FRAC_BITS + KI_FRAC)?;
        let agc_rms = d.agc_rms * 32768.0;
        let agc_target = ((agc_rms * agc_rms) / f64::from(1u32 << POWER_SHIFT)).round() as u32;
        // Output step: a power of two, so the quantiser is a shift.
        let fs_comp = d.clip * agc_rms;
        let step = fs_comp / f64::from(1u32 << (d.bits - 1));
        let q_shift = step.log2().round().clamp(0.0, 15.0) as u8;
        let nco_step = (-d.carrier_offset / d.sample_rate * 4_294_967_296.0).round() as i64;
        Ok(Self {
            nco_step: nco_step as u32 as i32,
            h,
            g,
            k,
            period_nominal: period,
            period_min: ((sps * 0.98).max(1.0625) * ONE as f64) as u32,
            period_max: (sps * 1.02 * ONE as f64) as u32,
            ted_shift,
            kp,
            kp_shift,
            ki,
            ki_shift,
            integrator_max: (sps * 0.01 * ONE as f64) as i32,
            // Gain 1.0 to start; the loop converges within a few thousand
            // symbols.
            agc_init: 1 << (GAIN_FRAC_BITS + GAIN_ACC_EXTRA_BITS),
            agc_target,
            agc_shift: 4,
            bits: d.bits,
            q_shift,
            block_log2: d.block_log2,
        })
    }

    /// Taps per phase.
    pub fn taps(&self) -> usize {
        self.h.first().map_or(0, Vec::len)
    }

    /// Check the invariants the arithmetic relies on.
    pub fn validate(&self) -> Result<(), ParamError> {
        let err = |s: String| Err(ParamError(s));
        let taps = self.taps();
        if self.h.len() != PHASES || self.g.len() != PHASES || self.k.len() != PHASES {
            return err(format!(
                "{} / {} / {} phases, {PHASES} expected",
                self.h.len(),
                self.g.len(),
                self.k.len()
            ));
        }
        if !(4..=MAX_TAPS).contains(&taps) || !taps.is_multiple_of(2) {
            return err(format!("{taps} taps"));
        }
        let lim = 1i32 << (COEF_BITS - 1);
        for bank in [&self.h, &self.g, &self.k] {
            for ph in bank {
                if ph.len() != taps {
                    return err("ragged coefficient bank".into());
                }
                if ph.iter().any(|&c| c < -lim || c >= lim) {
                    return err(format!("coefficient outside {COEF_BITS} bits"));
                }
                if ph.iter().map(|&c| i64::from(c).abs()).sum::<i64>() > COEF_SUM_MAX {
                    return err("coefficient magnitudes sum above 2^17 − 1".into());
                }
            }
        }
        if i64::from(self.period_min) <= ONE
            || self.period_min > self.period_nominal
            || self.period_nominal > self.period_max
            || i64::from(self.period_max) >= 4 * ONE
        {
            return err("period bounds: 1 < min ≤ nominal ≤ max < 4 samples".into());
        }
        if !(2..=8).contains(&self.bits) || self.q_shift > 15 {
            return err(format!("bits {} / q_shift {}", self.bits, self.q_shift));
        }
        if !(8..=15).contains(&self.block_log2) {
            return err(format!("block_log2 {}: 8..=15", self.block_log2));
        }
        if self.agc_init >= 1 << GAIN_ACC_BITS
            || self.integrator_max < 0
            || self.integrator_max >= 1 << 22
            || self.ted_shift > 30
            || self.kp_shift > MAX_GAIN_SHIFT
            || self.ki_shift > MAX_GAIN_SHIFT
        {
            return err("agc_init, integrator_max or ted_shift out of range".into());
        }
        Ok(())
    }

    /// Output words per symbol, header included (16-bit words).
    pub fn words_per_symbol(&self) -> f64 {
        let l = f64::from(1u32 << self.block_log2);
        (l * 2.0 * f64::from(self.bits) / 16.0
            + (crate::format::HEADER_WORDS + crate::format::TRAILER_WORDS) as f64)
            / l
    }
}

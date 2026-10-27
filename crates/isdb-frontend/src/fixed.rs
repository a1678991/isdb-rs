//! The bit-exact fixed-point front end: the reference the FPGA
//! implementation must reproduce bit for bit (format version 4).
//!
//! Per input sample `x[n]` (12-bit components in `i16`), in this order:
//!
//! 1. **Mixer.** `m = round((x · e^{jφ}) / 2¹⁵)` with `e^{jφ}` from the
//!    cosine table at `φ >> 22` (sine at index + 768), then
//!    `φ += nco_step` (wrapping `u32`). Products are summed in `i32`;
//!    rounding is `(v + 2¹⁴) >> 15` (arithmetic shift, as everywhere).
//! 2. **Delay line.** `m` enters at tap 0; tap `t` holds `m[n − t]`.
//! 3. **Symbol decision.** `t_acc` is the time from the output reference
//!    instant `n − taps/2` to the next symbol, Q8.24 samples. When
//!    `t_acc < 1.0` a symbol is computed (at most one per sample):
//!    1. phase `p = (t_acc >> 18) & 63`;
//!    2. if `k ≥ LOOP_DELAY` (k: symbols emitted so far), the loop errors
//!       of symbol `k − LOOP_DELAY` are applied: integrator
//!       `v = clamp(v + round((ki·e) / 2^ki_shift), ±integrator_max << 16)`,
//!       `period = clamp(nominal + (v >> 16) + round((kp·e) / 2^kp_shift), min, max)`,
//!       AGC `acc = clamp(acc + (err >> agc_shift), 0, 2²⁴ − 1)`;
//!    3. `y = round(Σ_t h[p][t]·m[n − t] / 2¹⁴)`, `dy` likewise with `g`,
//!       `yk` likewise with `k` (`i64` accumulators; the FPGA's are 44
//!       bits);
//!    4. AGC: `G = acc >> 8` (Q6.10), `z = sat16(round(y·G / 2¹⁰))`,
//!       `dz` and `zk` likewise;
//!    5. output `q = clamp(round(zk / 2^q_shift), ±2^(bits−1))`;
//!    6. errors: `e = clamp((zI·dzI + zQ·dzQ) >> ted_shift, ±(2²³ − 1))`
//!       (the timing detector on `h` and `g`),
//!       `err = agc_target − ((zkI² + zkQ²) >> 12)` (the AGC on the
//!       output); queued for symbol `k + LOOP_DELAY`;
//!    7. `t_acc += period`.
//! 4. `t_acc −= 1.0`.
//!
//! The loop delay of [`LOOP_DELAY`] symbols is part of the definition:
//! it is the pipeline latency the FPGA is allowed, and the model applies
//! it exactly so both agree bit for bit.
//!
//! Since format version 4 the detector banks `h` and `g` are non-zero on
//! their centre [`crate::params::TED_TAPS`] taps only and `k` is the whole
//! matched filter (format 1 was the special case `k == h`).

use crate::params::{
    E_MAX, FILTER_SHIFT, FRAC_BITS, GAIN_ACC_BITS, GAIN_ACC_EXTRA_BITS, GAIN_FRAC_BITS, KI_FRAC,
    LOOP_DELAY, NCO_LUT_BITS, ONE, PHASE_BITS, POWER_SHIFT, ParamError, Params,
};
use crate::tables::cos_table;
use std::collections::VecDeque;

/// One output symbol and the internal values the FPGA test compares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Symbol {
    /// Index of the input sample at which it was computed.
    pub sample: u64,
    /// Filter phase used (0..64).
    pub phase: u8,
    /// Matched-filter output (I, Q).
    pub y: [i16; 2],
    /// Derivative-filter output.
    pub dy: [i16; 2],
    /// Output-filter output.
    pub yk: [i16; 2],
    /// AGC gain used (Q6.10).
    pub gain: u16,
    /// Period register in force when the symbol was computed (Q8.24).
    pub period: u32,
    /// Gain-scaled symbol.
    pub z: [i16; 2],
    /// Gain-scaled derivative.
    pub dz: [i16; 2],
    /// Gain-scaled output-filter value (what `q` quantises).
    pub zk: [i16; 2],
    /// Output components (`bits` wide).
    pub q: [i8; 2],
    /// Timing error of this symbol (24 bits).
    pub e: i32,
    /// AGC error of this symbol.
    pub err: i32,
}

/// The fixed-point front end.
#[derive(Debug, Clone)]
pub struct Frontend {
    p: Params,
    cos: Vec<i16>,
    taps: usize,
    line: Vec<[i32; 2]>,
    /// Write position of the newest sample in `line` (ring buffer).
    head: usize,
    nco: u32,
    t_acc: i64,
    v: i64,
    period: i64,
    gain_acc: i64,
    pending: VecDeque<(i32, i32)>,
    samples: u64,
    symbols: u64,
}

impl Frontend {
    /// A front end with parameters `p`, delay line cleared, timing at the
    /// first sample.
    pub fn new(p: Params) -> Result<Self, ParamError> {
        p.validate()?;
        let taps = p.taps();
        Ok(Self {
            cos: cos_table(),
            taps,
            line: vec![[0, 0]; taps],
            head: 0,
            nco: 0,
            t_acc: 0,
            v: 0,
            period: i64::from(p.period_nominal),
            gain_acc: i64::from(p.agc_init),
            pending: VecDeque::with_capacity(LOOP_DELAY + 1),
            samples: 0,
            symbols: 0,
            p,
        })
    }

    /// The parameters.
    pub fn params(&self) -> &Params {
        &self.p
    }

    /// Symbols emitted so far.
    pub fn symbols(&self) -> u64 {
        self.symbols
    }

    /// The period register (Q8.24 samples).
    pub fn period(&self) -> u32 {
        period_reg(self.period)
    }

    /// The AGC gain (Q6.10).
    pub fn gain(&self) -> u16 {
        gain_of(self.gain_acc)
    }

    /// Mixer step 1: returns the rotated sample and advances the phase.
    fn mix(&mut self, x: [i16; 2]) -> [i32; 2] {
        let idx = (self.nco >> (32 - NCO_LUT_BITS)) as usize;
        let mask = (1 << NCO_LUT_BITS) - 1;
        let c = i32::from(self.cos[idx]);
        let s = i32::from(self.cos[(idx + 768) & mask]);
        let (xi, xq) = (i32::from(x[0]), i32::from(x[1]));
        let re = xi * c - xq * s;
        let im = xi * s + xq * c;
        self.nco = self.nco.wrapping_add(self.p.nco_step as u32);
        [(re + (1 << 14)) >> 15, (im + (1 << 14)) >> 15]
    }

    /// Push one input sample; returns the symbol computed at it, if any.
    pub fn push(&mut self, x: [i16; 2]) -> Option<Symbol> {
        let m = self.mix(x);
        self.head = if self.head == 0 {
            self.taps - 1
        } else {
            self.head - 1
        };
        self.line[self.head] = m;
        let out = if self.t_acc < ONE {
            Some(self.symbol())
        } else {
            None
        };
        self.t_acc -= ONE;
        self.samples += 1;
        out
    }

    /// Push a block of samples, appending the symbols to `out`.
    pub fn process(&mut self, x: &[[i16; 2]], out: &mut Vec<Symbol>) {
        out.extend(x.iter().filter_map(|&s| self.push(s)));
    }

    fn symbol(&mut self) -> Symbol {
        let p = &self.p;
        let phase = phase_of(self.t_acc);
        if self.pending.len() == LOOP_DELAY {
            let (e, err) = self
                .pending
                .pop_front()
                .expect("queue holds LOOP_DELAY entries");
            let vmax = i64::from(p.integrator_max) << KI_FRAC;
            let ki_e = round_shift(i64::from(p.ki) * i64::from(e), u32::from(p.ki_shift));
            self.v = (self.v + ki_e).clamp(-vmax, vmax);
            self.period = (i64::from(p.period_nominal)
                + (self.v >> KI_FRAC)
                + round_shift(i64::from(p.kp) * i64::from(e), u32::from(p.kp_shift)))
            .clamp(i64::from(p.period_min), i64::from(p.period_max));
            self.gain_acc = (self.gain_acc + (i64::from(err) >> p.agc_shift))
                .clamp(0, (1 << GAIN_ACC_BITS) - 1);
        }
        let (h, g, kb) = (
            &p.h[usize::from(phase)],
            &p.g[usize::from(phase)],
            &p.k[usize::from(phase)],
        );
        let (mut yi, mut yq, mut di, mut dq, mut ki, mut kq) = (0i64, 0i64, 0i64, 0i64, 0i64, 0i64);
        for t in 0..self.taps {
            let s = self.line[(self.head + t) % self.taps];
            let (si, sq) = (i64::from(s[0]), i64::from(s[1]));
            yi += i64::from(h[t]) * si;
            yq += i64::from(h[t]) * sq;
            di += i64::from(g[t]) * si;
            dq += i64::from(g[t]) * sq;
            ki += i64::from(kb[t]) * si;
            kq += i64::from(kb[t]) * sq;
        }
        let filt = |a: i64| sat16(round_shift(a, FILTER_SHIFT));
        let y = [filt(yi), filt(yq)];
        let dy = [filt(di), filt(dq)];
        let yk = [filt(ki), filt(kq)];
        let gain = gain_of(self.gain_acc);
        let scale = |v: i16| sat16(round_shift(i64::from(v) * i64::from(gain), GAIN_FRAC_BITS));
        let z = [scale(y[0]), scale(y[1])];
        let dz = [scale(dy[0]), scale(dy[1])];
        let zk = [scale(yk[0]), scale(yk[1])];
        let quant = |v: i16| quantise(v, p.q_shift, p.bits);
        let q = [quant(zk[0]), quant(zk[1])];
        let ted = i64::from(z[0]) * i64::from(dz[0]) + i64::from(z[1]) * i64::from(dz[1]);
        let e = timing_error(ted, p.ted_shift);
        let pow = (i64::from(zk[0]).pow(2) + i64::from(zk[1]).pow(2)) >> POWER_SHIFT;
        let err = agc_error(p.agc_target, pow);
        self.pending.push_back((e, err));
        let sym = Symbol {
            sample: self.samples,
            phase,
            y,
            dy,
            yk,
            gain,
            period: period_reg(self.period),
            z,
            dz,
            zk,
            q,
            e,
            err,
        };
        self.t_acc += self.period;
        self.symbols += 1;
        sym
    }
}

/// `round(v / 2^s)`: add half, arithmetic shift (s ≥ 1); `v` for s = 0.
#[inline]
pub fn round_shift(v: i64, s: u32) -> i64 {
    if s == 0 { v } else { (v + (1 << (s - 1))) >> s }
}

/// Saturate to `i16`.
#[inline]
pub fn sat16(v: i64) -> i16 {
    // Clamped to the i16 range, so the cast is exact.
    #[allow(clippy::cast_possible_truncation)]
    let s = v.clamp(i64::from(i16::MIN), i64::from(i16::MAX)) as i16;
    s
}

/// Step 3.1: the filter phase `(t_acc >> 18) & 63` of the symbol about to
/// be computed.
#[inline(always)]
pub(crate) fn phase_of(t_acc: i64) -> u8 {
    ((t_acc >> (FRAC_BITS - PHASE_BITS)) & ((1 << PHASE_BITS) - 1)) as u8
}

/// Step 3.4: the AGC gain `G = acc >> 8` (Q6.10).
#[inline(always)]
pub(crate) fn gain_of(acc: i64) -> u16 {
    // The accumulator is clamped to 0..2^GAIN_ACC_BITS (24 bits), so its
    // top 16 bits fit.
    #[allow(clippy::cast_possible_truncation)]
    let g = (acc >> GAIN_ACC_EXTRA_BITS) as u16;
    g
}

/// Step 3.5: the output component `clamp(round(v / 2^q_shift), ±2^(bits−1))`.
#[inline(always)]
pub(crate) fn quantise(v: i16, q_shift: u8, bits: u8) -> i8 {
    let lim = 1i64 << (bits - 1);
    // DSP quantiser: clamped to ±2^(bits−1) with bits ≤ 8
    // (Params::validate), so the cast is exact.
    #[allow(clippy::cast_possible_truncation)]
    let q = round_shift(i64::from(v), u32::from(q_shift)).clamp(-lim, lim - 1) as i8;
    q
}

/// Step 3.6: the timing error `clamp(ted >> ted_shift, ±E_MAX)`.
#[inline(always)]
pub(crate) fn timing_error(ted: i64, ted_shift: u8) -> i32 {
    // Clamped to ±E_MAX (24 bits), so the cast is exact.
    #[allow(clippy::cast_possible_truncation)]
    let e = (ted >> ted_shift).clamp(-E_MAX, E_MAX) as i32;
    e
}

/// Step 3.6: the AGC error `agc_target − pow`.
#[inline(always)]
pub(crate) fn agc_error(agc_target: u32, pow: i64) -> i32 {
    // pow ≤ 2·32768² >> 12 = 2^19 and a designed agc_target is at most 2^18
    // (Params::design, agc_rms ≤ 1), so the difference fits i32.
    #[allow(clippy::cast_possible_truncation)]
    let err = (i64::from(agc_target) - pow) as i32;
    err
}

/// The period register (Q8.24) from the loop's `i64` value.
#[inline(always)]
pub(crate) fn period_reg(period: i64) -> u32 {
    // The loop clamps the period to period_min..=period_max, both u32.
    #[allow(clippy::cast_possible_truncation)]
    let r = period as u32;
    r
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::params::Design;

    #[test]
    fn round_shift_is_round_half_up() {
        assert_eq!(round_shift(5, 1), 3);
        assert_eq!(round_shift(-5, 1), -2);
        assert_eq!(round_shift(-6, 2), -1);
        assert_eq!(round_shift(7, 0), 7);
    }

    #[test]
    fn mixer_rotates_by_the_table() {
        let mut d = Design::new(40e6, 28.86e6, 0.35);
        d.carrier_offset = -10e6; // +quarter turn per sample
        let mut f = Frontend::new(Params::design(&d).unwrap()).unwrap();
        assert_eq!(f.params().nco_step, 1 << 30);
        // e^{j0}, e^{jπ/2}, e^{jπ}: (1000, 0) → (1000,0), (0,1000), (−1000,0)
        assert_eq!(f.mix([1000, 0]), [1000, 0]);
        assert_eq!(f.mix([1000, 0]), [0, 1000]);
        assert_eq!(f.mix([1000, 0]), [-1000, 0]);
    }

    #[test]
    fn the_detector_uses_its_centre_taps_and_the_output_all() {
        let mut d = Design::new(40e6, 33.7561e6, 0.03);
        d.taps = 16;
        d.loop_bandwidth = 2e-4;
        let p = Params::design(&d).unwrap();
        let lo = (16 - crate::params::TED_TAPS) / 2;
        for ph in 0..crate::params::PHASES {
            for t in 0..16 {
                let centre = (lo..lo + crate::params::TED_TAPS).contains(&t);
                if !centre {
                    assert_eq!((p.h[ph][t], p.g[ph][t]), (0, 0));
                }
                if centre {
                    assert_eq!(p.h[ph][t], p.k[ph][t], "untrimmed k is h's whole filter");
                }
            }
        }
        // a coefficient outside the centre is rejected
        let mut q = p.clone();
        q.g[3][0] = 1;
        assert!(q.validate().is_err());
    }

    #[test]
    fn a_trim_changes_only_the_output() {
        // Same input, trim 0 and −0.17 T: the loop (period, e, gain path
        // through z) sees the matched filter in both; the output differs.
        let base = Design {
            taps: 16,
            loop_bandwidth: 2e-4,
            ..Design::new(40e6, 33.7561e6, 0.03)
        };
        let x: Vec<[i16; 2]> = (0..20_000)
            .map(|n| if n % 5 < 2 { [700, 100] } else { [-200, -650] })
            .collect();
        let run = |trim: f64| {
            let p = Params::design(&Design {
                trim,
                ..base.clone()
            })
            .unwrap();
            let mut f = Frontend::new(p).unwrap();
            let mut out = Vec::new();
            f.process(&x, &mut out);
            out
        };
        let (a, b) = (run(0.0), run(-0.17));
        assert_eq!(a.len(), b.len());
        // The very first symbols precede any loop update (LOOP_DELAY) and
        // use the same gain: their z agree, their zk differ.
        assert!(a.iter().zip(&b).take(LOOP_DELAY).all(|(s, t)| s.z == t.z));
        assert!(a.iter().zip(&b).any(|(s, t)| s.zk != t.zk));
    }

    #[test]
    fn at_most_one_symbol_per_sample_and_rate_matches() {
        let d = Design::new(40e6, 33.7561e6, 0.03);
        let p = Params::design(&Design { taps: 48, ..d }).unwrap();
        let mut f = Frontend::new(p).unwrap();
        let mut out = Vec::new();
        let n = 100_000;
        // A quiet input: the loop sees no error and keeps the nominal rate.
        f.process(&vec![[0, 0]; n], &mut out);
        let expect = n as f64 / (40e6 / 33.7561e6);
        assert!(
            (out.len() as f64 - expect).abs() < 2.0,
            "{} vs {expect}",
            out.len()
        );
        assert!(out.windows(2).all(|w| w[1].sample > w[0].sample));
    }
}

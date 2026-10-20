//! The bit-exact fixed-point front end: the reference the FPGA
//! implementation must reproduce bit for bit (format version 1).
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
//!    3. `y = round(Σ_t h[p][t]·m[n − t] / 2¹⁴)`, `dy` likewise with `g`
//!       (`i64` accumulators; the FPGA's are 48 bits);
//!    4. AGC: `G = acc >> 8` (Q6.10), `z = sat16(round(y·G / 2¹⁰))`,
//!       `dz` likewise;
//!    5. output `q = clamp(round(z / 2^q_shift), ±2^(bits−1))`;
//!    6. errors: `e = clamp((zI·dzI + zQ·dzQ) >> ted_shift, ±(2²³ − 1))`,
//!       `err = agc_target − ((zI² + zQ²) >> 12)`; queued for symbol
//!       `k + LOOP_DELAY`;
//!    7. `t_acc += period`.
//! 4. `t_acc −= 1.0`.
//!
//! The loop delay of [`LOOP_DELAY`] symbols is part of the definition:
//! it is the pipeline latency the FPGA is allowed, and the model applies
//! it exactly so both agree bit for bit.

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
    /// AGC gain used (Q6.10).
    pub gain: u16,
    /// Period register in force when the symbol was computed (Q8.24).
    pub period: u32,
    /// Gain-scaled symbol.
    pub z: [i16; 2],
    /// Gain-scaled derivative.
    pub dz: [i16; 2],
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
        self.period as u32
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
        let phase = ((self.t_acc >> (FRAC_BITS - PHASE_BITS)) & ((1 << PHASE_BITS) - 1)) as usize;
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
        let (h, g) = (&p.h[phase], &p.g[phase]);
        let (mut yi, mut yq, mut di, mut dq) = (0i64, 0i64, 0i64, 0i64);
        for t in 0..self.taps {
            let s = self.line[(self.head + t) % self.taps];
            let (si, sq) = (i64::from(s[0]), i64::from(s[1]));
            yi += i64::from(h[t]) * si;
            yq += i64::from(h[t]) * sq;
            di += i64::from(g[t]) * si;
            dq += i64::from(g[t]) * sq;
        }
        let filt = |a: i64| sat16(round_shift(a, FILTER_SHIFT));
        let y = [filt(yi), filt(yq)];
        let dy = [filt(di), filt(dq)];
        let gain = gain_of(self.gain_acc);
        let scale = |v: i16| sat16(round_shift(i64::from(v) * i64::from(gain), GAIN_FRAC_BITS));
        let z = [scale(y[0]), scale(y[1])];
        let dz = [scale(dy[0]), scale(dy[1])];
        let lim = 1i64 << (p.bits - 1);
        let quant =
            |v: i16| round_shift(i64::from(v), u32::from(p.q_shift)).clamp(-lim, lim - 1) as i8;
        let q = [quant(z[0]), quant(z[1])];
        let ted = i64::from(z[0]) * i64::from(dz[0]) + i64::from(z[1]) * i64::from(dz[1]);
        let e = (ted >> p.ted_shift).clamp(-E_MAX, E_MAX) as i32;
        let pow = (i64::from(z[0]).pow(2) + i64::from(z[1]).pow(2)) >> POWER_SHIFT;
        let err = (i64::from(p.agc_target) - pow) as i32;
        self.pending.push_back((e, err));
        let sym = Symbol {
            sample: self.samples,
            phase: phase as u8,
            y,
            dy,
            gain,
            period: self.period as u32,
            z,
            dz,
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
pub fn round_shift(v: i64, s: u32) -> i64 {
    if s == 0 { v } else { (v + (1 << (s - 1))) >> s }
}

/// Saturate to `i16`.
pub fn sat16(v: i64) -> i16 {
    v.clamp(i64::from(i16::MIN), i64::from(i16::MAX)) as i16
}

fn gain_of(acc: i64) -> u16 {
    (acc >> GAIN_ACC_EXTRA_BITS) as u16
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

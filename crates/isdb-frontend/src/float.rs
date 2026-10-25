//! Floating-point reference of the front end: the same structure as
//! [`crate::fixed`] (mixer, polyphase matched filter, derivative and
//! output filters, AGC, timing loop with the same delay and gains)
//! without its quantisation:
//! exact sine and cosine, unrounded coefficients at 1024 phases (instead of
//! 64), no rounding or saturation. Comparing the two measures what the
//! fixed-point arithmetic costs.

use crate::params::{
    FILTER_SHIFT, GAIN_ACC_EXTRA_BITS, GAIN_FRAC_BITS, KI_FRAC, LOOP_DELAY, ONE, POWER_SHIFT,
    Params,
};
use crate::tables::{bank_scale, float_banks};
use std::collections::VecDeque;

/// Phases of the floating-point banks.
pub const FLOAT_PHASES: usize = 1024;

/// One floating-point output symbol (same units as the fixed `z`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FloatSymbol {
    /// Input sample index.
    pub sample: u64,
    /// Fractional position of the output instant (0..1 sample).
    pub mu: f64,
    /// Gain-scaled symbol at the loop's instant (matched filter `h`).
    pub z: [f64; 2],
    /// Gain-scaled output symbol (output filter: `z` when untrimmed).
    pub zk: [f64; 2],
    /// Period in force (input samples).
    pub period: f64,
}

/// The floating-point front end.
#[derive(Debug, Clone)]
pub struct FloatFrontend {
    p: Params,
    h: Vec<Vec<f64>>,
    g: Vec<Vec<f64>>,
    k: Vec<Vec<f64>>,
    taps: usize,
    line: VecDeque<[f64; 2]>,
    nco: u32,
    t_acc: f64,
    v: f64,
    period: f64,
    gain_acc: f64,
    pending: VecDeque<(f64, f64)>,
    samples: u64,
}

impl FloatFrontend {
    /// The reference for the same parameters as a fixed front end, with
    /// banks designed from `sps`, `alpha` and `beta` (those of the fixed
    /// banks) and no trim.
    pub fn new(p: Params, sps: f64, alpha: f64, beta: f64) -> Self {
        Self::with_trim(p, sps, alpha, beta, 0.0)
    }

    /// The same with the output filter `trim` symbol periods after the
    /// loop's instant (as [`crate::tables::output_bank`]).
    pub fn with_trim(p: Params, sps: f64, alpha: f64, beta: f64, trim: f64) -> Self {
        let taps = p.taps();
        let (h64, g64) = float_banks(sps, alpha, taps, beta);
        let (sh, sg) = (bank_scale(&h64), bank_scale(&g64));
        // The same scales as the integer banks, at 1024 phases.
        let c = (taps / 2) as f64;
        let bank = |f: &dyn Fn(f64) -> f64, s: f64| -> Vec<Vec<f64>> {
            (0..FLOAT_PHASES)
                .map(|ph| {
                    (0..taps)
                        .map(|t| f(t as f64 - c + ph as f64 / FLOAT_PHASES as f64) * s)
                        .collect()
                })
                .collect()
        };
        let h = bank(
            &|tau| crate::tables::matched_filter(tau, sps, alpha, taps, beta),
            sh,
        );
        let g = bank(
            &|tau| crate::tables::matched_filter_derivative(tau, sps, alpha, taps, beta),
            sg,
        );
        let half = taps as f64 / 2.0;
        let k = bank(
            &|tau| {
                crate::tables::rrc((tau + trim * sps) / sps, alpha)
                    * crate::tables::kaiser(tau, half, beta)
            },
            sh,
        );
        Self {
            h,
            g,
            k,
            taps,
            line: VecDeque::from(vec![[0.0, 0.0]; taps]),
            nco: 0,
            t_acc: 0.0,
            v: 0.0,
            period: f64::from(p.period_nominal) / ONE as f64,
            gain_acc: f64::from(p.agc_init),
            pending: VecDeque::new(),
            samples: 0,
            p,
        }
    }

    /// Push one input sample (same units as the fixed model's input).
    pub fn push(&mut self, x: [f64; 2]) -> Option<FloatSymbol> {
        let ph = f64::from(self.nco) / 4_294_967_296.0 * std::f64::consts::TAU;
        let (s, c) = ph.sin_cos();
        // The fixed mixer scales by 32767/32768.
        let k = 32767.0 / 32768.0;
        let m = [(x[0] * c - x[1] * s) * k, (x[0] * s + x[1] * c) * k];
        self.nco = self.nco.wrapping_add(self.p.nco_step as u32);
        self.line.pop_back();
        self.line.push_front(m);
        let out = (self.t_acc < 1.0).then(|| self.symbol());
        self.t_acc -= 1.0;
        self.samples += 1;
        out
    }

    fn symbol(&mut self) -> FloatSymbol {
        let p = &self.p;
        let one = ONE as f64;
        if self.pending.len() == LOOP_DELAY {
            let (e, err) = self
                .pending
                .pop_front()
                .expect("queue holds LOOP_DELAY entries");
            let vmax = f64::from(p.integrator_max) * 2f64.powi(KI_FRAC as i32);
            self.v = (self.v + f64::from(p.ki) * e / 2f64.powi(i32::from(p.ki_shift)))
                .clamp(-vmax, vmax);
            self.period = ((f64::from(p.period_nominal)
                + self.v / 2f64.powi(KI_FRAC as i32)
                + f64::from(p.kp) * e / 2f64.powi(i32::from(p.kp_shift)))
                / one)
                .clamp(f64::from(p.period_min) / one, f64::from(p.period_max) / one);
            self.gain_acc += err / f64::from(1u32 << p.agc_shift);
        }
        let ph = ((self.t_acc * FLOAT_PHASES as f64) as usize).min(FLOAT_PHASES - 1);
        let mu = self.t_acc;
        let (h, g, kb) = (&self.h[ph], &self.g[ph], &self.k[ph]);
        let (mut y, mut dy, mut yk) = ([0.0; 2], [0.0; 2], [0.0; 2]);
        for (t, s) in self.line.iter().enumerate().take(self.taps) {
            y[0] += h[t] * s[0];
            y[1] += h[t] * s[1];
            dy[0] += g[t] * s[0];
            dy[1] += g[t] * s[1];
            yk[0] += kb[t] * s[0];
            yk[1] += kb[t] * s[1];
        }
        let k = 1.0 / f64::from(1u32 << FILTER_SHIFT);
        let gain = self.gain_acc / f64::from(1u32 << (GAIN_ACC_EXTRA_BITS + GAIN_FRAC_BITS));
        let z = [y[0] * k * gain, y[1] * k * gain];
        let dz = [dy[0] * k * gain, dy[1] * k * gain];
        let zk = [yk[0] * k * gain, yk[1] * k * gain];
        let e = (z[0] * dz[0] + z[1] * dz[1]) / f64::from(1u32 << p.ted_shift);
        let err = f64::from(p.agc_target)
            - (zk[0] * zk[0] + zk[1] * zk[1]) / f64::from(1u32 << POWER_SHIFT);
        self.pending.push_back((e, err));
        let sym = FloatSymbol {
            sample: self.samples,
            mu,
            z,
            zk,
            period: self.period,
        };
        self.t_acc += self.period;
        sym
    }
}

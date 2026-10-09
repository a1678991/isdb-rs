//! IFFT / guard interval (ARIB STD-B31 §3.14.1–3.14.2) and the receiver
//! FFT.
//!
//! §3.14.1: carrier `k` of symbol `n` is the complex exponential
//! `exp(j2π (k − Kc)/Tu · (t − Tg − n·Ts))` on `n·Ts ≤ t < (n+1)·Ts`.  At
//! the FFT sample rate 512/63 MHz (`Tu` = N samples) sample `m` of the
//! symbol (0 ≤ m < Ng + N) is
//!
//! `x[m] = g/√K · Σ_k c(n,k) · exp(j2π (k − Kc)(m − Ng)/N)`,
//!
//! i.e. the IFFT with carrier `k` in bin `(k − Kc) mod N` (Kc → bin 0, the
//! RF centre at baseband DC), output samples `Ng..Ng+N` = IFFT output and
//! samples `0..Ng` = its last `Ng` samples (§3.14.2 guard interval).
//!
//! Scaling: with the spec's normalised carrier values (data carriers of
//! unit mean power, pilots ±4/3) the factor `1/√K` gives a mean sample
//! power of `(1/K)·Σ|c|²` — exactly 1 if every active carrier had unit
//! power; ≈1.03–1.06 in practice because of the boosted pilots — times
//! `g²` (user gain, default 1).

use crate::C32;
use isdbt_core::ofdm_params::center_carrier;
use isdbt_core::{GuardInterval, Mode};
use rustfft::{Fft, FftPlanner};
use std::sync::Arc;

/// FFT bin of band carrier `k` (§3.14.1).
pub fn carrier_bin(mode: Mode, k: usize) -> usize {
    let n = mode.fft_size() as isize;
    (k as isize - center_carrier(mode) as isize).rem_euclid(n) as usize
}

/// OFDM symbol generator.
pub struct OfdmTx {
    mode: Mode,
    n: usize,
    ng: usize,
    fft: Arc<dyn Fft<f32>>,
    buf: Vec<C32>,
    scratch: Vec<C32>,
    scale: f32,
}

impl std::fmt::Debug for OfdmTx {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OfdmTx").field("mode", &self.mode).field("ng", &self.ng).finish()
    }
}

impl OfdmTx {
    /// Generator for `mode`/`guard` with unit user gain.
    pub fn new(mode: Mode, guard: GuardInterval) -> Self {
        let n = mode.fft_size();
        let fft = FftPlanner::new().plan_fft_inverse(n);
        let scratch = vec![C32::default(); fft.get_inplace_scratch_len()];
        Self {
            mode,
            n,
            ng: guard.samples(mode),
            fft,
            buf: vec![C32::default(); n],
            scratch,
            scale: 1.0 / (mode.total_carriers() as f32).sqrt(),
        }
    }

    /// Set the user gain `g` (amplitude).
    pub fn set_gain(&mut self, g: f32) {
        self.scale = g / (self.mode.total_carriers() as f32).sqrt();
    }

    /// Append one symbol (guard + useful part, `Ng + N` samples) for the
    /// `K` carrier values `carriers` to `out`.
    pub fn modulate(&mut self, carriers: &[C32], out: &mut Vec<C32>) {
        assert_eq!(carriers.len(), self.mode.total_carriers());
        self.buf.fill(C32::default());
        for (k, &c) in carriers.iter().enumerate() {
            self.buf[carrier_bin(self.mode, k)] = c * self.scale;
        }
        self.fft.process_with_scratch(&mut self.buf, &mut self.scratch);
        out.extend_from_slice(&self.buf[self.n - self.ng..]);
        out.extend_from_slice(&self.buf);
    }
}

/// Receiver FFT: `N` time samples → `N` bins scaled so that a carrier sent
/// with value `c` (and unit gain) is returned as `c` (`× √K / N`).
pub struct OfdmRx {
    fft: Arc<dyn Fft<f32>>,
    scratch: Vec<C32>,
    scale: f32,
}

impl std::fmt::Debug for OfdmRx {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OfdmRx").finish()
    }
}

impl OfdmRx {
    /// FFT for `mode`.
    pub fn new(mode: Mode) -> Self {
        let n = mode.fft_size();
        let fft = FftPlanner::new().plan_fft_forward(n);
        let scratch = vec![C32::default(); fft.get_inplace_scratch_len()];
        Self { fft, scratch, scale: (mode.total_carriers() as f32).sqrt() / n as f32 }
    }

    /// Transform `buf` (N samples) in place into scaled bins.
    pub fn transform(&mut self, buf: &mut [C32]) {
        self.fft.process_with_scratch(buf, &mut self.scratch);
        for v in buf.iter_mut() {
            *v *= self.scale;
        }
    }
}

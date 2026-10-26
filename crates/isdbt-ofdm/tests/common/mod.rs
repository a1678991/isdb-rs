//! Shared test helpers: deterministic RNG and a channel model.
#![allow(dead_code)]

use isdbt_ofdm::C32;

/// SplitMix64.
pub struct Rng(pub u64);

impl Rng {
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    pub fn bit(&mut self) -> u8 {
        (self.next_u64() >> 63) as u8
    }
    pub fn bits(&mut self, n: usize) -> Vec<u8> {
        (0..n).map(|_| self.bit()).collect()
    }
    pub fn uniform(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }
    /// Complex Gaussian with E|z|^2 = var.
    pub fn cgauss(&mut self, var: f64) -> C32 {
        let u1 = self.uniform().max(1e-300);
        let u2 = self.uniform();
        let r = (-var * u1.ln()).sqrt();
        let th = 2.0 * std::f64::consts::PI * u2;
        C32::new((r * th.cos()) as f32, (r * th.sin()) as f32)
    }
}

/// Channel impairments.
pub struct Channel {
    /// Leading samples of noise before the signal.
    pub offset: usize,
    /// CFO in carrier spacings (1/N of the sample rate).
    pub cfo_carriers: f64,
    /// FFT size N (for the CFO).
    pub fft_size: usize,
    /// Multipath taps (delay in samples, gain).
    pub taps: Vec<(usize, C32)>,
    /// Per-carrier SNR in dB: noise variance per carrier (after the
    /// receiver FFT scaling) relative to a unit-power transmitted data carrier.
    pub snr_db: f64,
    /// Total carriers K.
    pub k_total: usize,
}

impl Channel {
    /// Apply to a TX sample stream.
    pub fn apply(&self, tx: &[C32], rng: &mut Rng) -> Vec<C32> {
        let max_d = self.taps.iter().map(|t| t.0).max().unwrap_or(0);
        let mut out = vec![C32::default(); self.offset + tx.len() + max_d];
        for &(d, g) in &self.taps {
            for (i, &x) in tx.iter().enumerate() {
                out[self.offset + i + d] += x * g;
            }
        }
        let n = self.fft_size as f64;
        for (i, v) in out.iter_mut().enumerate() {
            let ph = 2.0 * std::f64::consts::PI * self.cfo_carriers * i as f64 / n;
            *v *= C32::new(ph.cos() as f32, ph.sin() as f32);
        }
        let s = 10f64.powf(self.snr_db / 10.0);
        // Receiver scaling: c_hat = FFT(x) * sqrt(K)/N; noise per carrier = sigma^2 * K / N.
        // Want noise per carrier = 1/s  =>  sigma^2 = N / (K s).
        let sigma2 = n / (self.k_total as f64 * s);
        for v in out.iter_mut() {
            *v += rng.cgauss(sigma2);
        }
        out
    }
}

/// Resample `x` at instants `i·(1 + ppm·1e-6)` (sampling-clock offset of
/// the transmitter relative to the receiver) with a Kaiser-windowed sinc
/// interpolator (±24 taps).
pub fn resample_ppm(x: &[C32], ppm: f64) -> Vec<C32> {
    let r = 1.0 + ppm * 1e-6;
    let half = 24i64;
    let beta = 8.0f64;
    let bessel_i0 = |v: f64| {
        let mut sum = 1.0;
        let mut term = 1.0;
        for k in 1..30 {
            term *= (v / 2.0) * (v / 2.0) / (f64::from(k) * f64::from(k));
            sum += term;
        }
        sum
    };
    let i0b = bessel_i0(beta);
    let n_out = ((x.len() as f64 - half as f64) / r) as usize;
    let mut out = Vec::with_capacity(n_out);
    for i in 0..n_out {
        let t = i as f64 * r;
        let c = t.floor() as i64;
        let mut acc = C32::default();
        for m in (c - half + 1)..=(c + half) {
            if m < 0 || m as usize >= x.len() {
                continue;
            }
            let d = t - m as f64;
            let sinc = if d.abs() < 1e-12 {
                1.0
            } else {
                (std::f64::consts::PI * d).sin() / (std::f64::consts::PI * d)
            };
            let u = d / half as f64;
            let w = if u.abs() >= 1.0 {
                0.0
            } else {
                bessel_i0(beta * (1.0 - u * u).sqrt()) / i0b
            };
            acc += x[m as usize] * (sinc * w) as f32;
        }
        out.push(acc);
    }
    out
}

//! Software channel for `isdb loopback`: sample offset, carrier frequency
//! offset and additive white Gaussian noise.

use isdbt_ofdm::C32;

/// Channel impairments.
#[derive(Debug, Clone)]
pub struct Channel {
    /// Noise relative to the measured signal power (None = noiseless).
    snr_db: Option<f32>,
    /// Phase increment per sample (radians) for the CFO.
    dphi: f64,
    phase: f64,
    /// Samples of noise-only lead-in still to insert.
    offset: usize,
    /// Samples of signal still to drop (receiver starts mid-stream).
    skip: usize,
    sigma: Option<f32>,
    rng: u64,
}

impl Channel {
    /// `cfo_carriers` is in carrier spacings of an `fft_size`-point symbol;
    /// `sample_offset` > 0 samples of silence (plus noise) precede the
    /// signal; < 0 drops that many samples (the receiver starts mid-stream).
    pub fn new(
        snr_db: Option<f32>,
        cfo_carriers: f64,
        fft_size: usize,
        sample_offset: i64,
        seed: u64,
    ) -> Self {
        Self {
            snr_db,
            dphi: 2.0 * std::f64::consts::PI * cfo_carriers / fft_size as f64,
            phase: 0.0,
            offset: sample_offset.max(0) as usize,
            skip: (-sample_offset).max(0) as usize,
            sigma: None,
            rng: seed | 1,
        }
    }

    fn uniform(&mut self) -> f32 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 7;
        self.rng ^= self.rng << 17;
        ((self.rng >> 40) as f32 + 0.5) / (1u64 << 24) as f32
    }

    fn gauss_pair(&mut self, sigma: f32) -> C32 {
        let (u1, u2) = (self.uniform(), self.uniform());
        let r = (-2.0 * u1.ln()).sqrt() * sigma;
        let t = 2.0 * std::f32::consts::PI * u2;
        C32::new(r * t.cos(), r * t.sin())
    }

    /// Pass a block through the channel (the first block also gets the
    /// offset lead-in prepended).
    pub fn process(&mut self, mut s: Vec<C32>) -> Vec<C32> {
        if let (Some(snr), None) = (self.snr_db, self.sigma) {
            // Noise variance per complex sample from the first block's power.
            let p = s.iter().map(|c| f64::from(c.norm_sqr())).sum::<f64>() / s.len().max(1) as f64;
            self.sigma = Some(((p / 10f64.powf(f64::from(snr) / 10.0)) / 2.0).sqrt() as f32);
        }
        if self.skip > 0 {
            let n = self.skip.min(s.len());
            s.drain(..n);
            self.skip -= n;
        }
        if self.offset > 0 {
            let mut v = vec![C32::new(0.0, 0.0); self.offset];
            v.append(&mut s);
            s = v;
            self.offset = 0;
        }
        if self.dphi != 0.0 {
            for c in s.iter_mut() {
                *c *= C32::new(self.phase.cos() as f32, self.phase.sin() as f32);
                self.phase = (self.phase + self.dphi) % (2.0 * std::f64::consts::PI);
            }
        }
        if let Some(sigma) = self.sigma {
            for c in s.iter_mut() {
                *c += self.gauss_pair(sigma);
            }
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snr_offset_and_cfo() {
        let sig = vec![C32::new(1.0, 0.0); 200_000];
        let mut ch = Channel::new(Some(10.0), 0.0, 8192, 7, 42);
        let out = ch.process(sig.clone());
        assert_eq!(out.len(), 200_007);
        let noise: f64 = out[7..]
            .iter()
            .map(|c| f64::from((c - C32::new(1.0, 0.0)).norm_sqr()))
            .sum::<f64>()
            / 200_000.0;
        assert!((10.0 * noise.log10() + 10.0).abs() < 0.1, "noise {noise}");
        // CFO of one carrier spacing: one full turn per FFT length.
        let mut ch = Channel::new(None, 1.0, 8, 0, 1);
        let out = ch.process(vec![C32::new(1.0, 0.0); 9]);
        assert!((out[2] - C32::new(0.0, 1.0)).norm() < 1e-5);
        assert!((out[8] - C32::new(1.0, 0.0)).norm() < 1e-5);
        // Negative offset drops samples across blocks.
        let mut ch = Channel::new(None, 0.0, 8, -5, 1);
        assert!(ch.process(vec![C32::new(1.0, 0.0); 3]).is_empty());
        assert_eq!(ch.process(vec![C32::new(2.0, 0.0); 4]).len(), 2);
    }
}

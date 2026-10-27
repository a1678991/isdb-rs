//! Quantisation of the unit-RMS OFDM baseband into integer samples.

use crate::C32;
use std::marker::PhantomData;

/// An interleaved integer I/Q sample format a [`Quantizer`] can produce.
pub trait IntFormat {
    /// One I or Q component (buffers of them cross to the thread feeding
    /// the device).
    type Component: Copy + Send + 'static;
    /// Bits of resolution: full scale is `2^(BITS−1) − 1`.
    const BITS: u32;
    /// Short name for descriptions (e.g. `cs8`).
    const NAME: &'static str;
    /// Write `samples · scale` into `out` (cleared first), rounding and
    /// saturating at ± full scale; returns the number of saturated
    /// components.
    fn convert(samples: &[C32], scale: f32, out: &mut Vec<Self::Component>) -> usize;
}

/// Interleaved signed 8-bit I/Q (`cs8`, I first), full scale 127; the
/// conversion is [`isdbt_phy::iq::cs8_from_samples`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cs8;

impl IntFormat for Cs8 {
    type Component = i8;
    const BITS: u32 = 8;
    const NAME: &'static str = "cs8";

    fn convert(samples: &[C32], scale: f32, out: &mut Vec<i8>) -> usize {
        isdbt_phy::iq::cs8_from_samples(samples, scale, out)
    }
}

/// Interleaved 12-bit I/Q (`cs12`, I first) left-aligned in `i16` (the
/// value times 16), full scale 2047: the AD936x DAC takes bits 15..4 of
/// each 16-bit component (`le:S16/16` channels of `cf-ad9361-dds-core-lpc`).
/// Rounding and saturation happen at 12 bits, so the clip count is exact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cs12;

impl IntFormat for Cs12 {
    type Component = i16;
    const BITS: u32 = 12;
    const NAME: &'static str = "cs12";

    fn convert(samples: &[C32], scale: f32, out: &mut Vec<i16>) -> usize {
        const FULL: f32 = 2047.0;
        out.clear();
        out.reserve(samples.len() * 2);
        let mut clipped = 0;
        for c in samples {
            for v in [c.re, c.im] {
                let x = (v * scale).round();
                if !(-FULL..=FULL).contains(&x) {
                    clipped += 1;
                }
                // DSP sample conversion: x is rounded and clamped to
                // +-2047, so the cast to i16 is exact.
                #[allow(clippy::cast_possible_truncation)]
                let x = x.clamp(-FULL, FULL) as i16;
                out.push(x << 4);
            }
        }
        clipped
    }
}

/// Scaling of the unit-mean-power OFDM signal into `F` samples: RMS
/// amplitude = full scale · 10^(−backoff/20), with saturation counting.
///
/// OFDM has a high peak-to-average ratio, so some components saturate at
/// any useful backoff; [`Quantizer::describe`] reports how many did.
#[derive(Debug, Clone)]
pub struct Quantizer<F: IntFormat> {
    backoff_db: f32,
    scale: f32,
    clipped: u64,
    components: u64,
    format: PhantomData<F>,
}

impl<F: IntFormat> Quantizer<F> {
    /// Full scale of `F`: `2^(BITS−1) − 1`.
    pub fn full_scale() -> f32 {
        ((1u32 << (F::BITS - 1)) - 1) as f32
    }

    /// Quantizer for an RMS level `backoff_db` below full scale.
    pub fn new(backoff_db: f32) -> Self {
        Self {
            backoff_db,
            scale: Self::full_scale() * 10f32.powf(-backoff_db / 20.0),
            clipped: 0,
            components: 0,
            format: PhantomData,
        }
    }

    /// Multiplier applied to the samples.
    pub fn scale(&self) -> f32 {
        self.scale
    }

    /// Saturated I or Q components so far.
    pub fn clipped(&self) -> u64 {
        self.clipped
    }

    /// Components converted so far.
    pub fn components(&self) -> u64 {
        self.components
    }

    /// Convert one block into `out` (cleared first).
    pub fn convert_into(&mut self, samples: &[C32], out: &mut Vec<F::Component>) {
        self.clipped += F::convert(samples, self.scale, out) as u64;
        self.components += 2 * samples.len() as u64;
    }

    /// Convert one block.
    pub fn convert(&mut self, samples: &[C32]) -> Vec<F::Component> {
        let mut out = Vec::new();
        self.convert_into(samples, &mut out);
        out
    }

    /// The level: `10.0 dB below cs8 full scale (RMS 40.2 of 127)`.
    pub fn describe_level(&self) -> String {
        format!(
            "{:.1} dB below {} full scale (RMS {:.1} of {})",
            self.backoff_db,
            F::NAME,
            self.scale,
            Self::full_scale()
        )
    }

    /// Clip summary: `clipped 12 of 4000000 components (3.00e-6)`.
    pub fn describe(&self) -> String {
        let r = if self.components > 0 {
            self.clipped as f64 / self.components as f64
        } else {
            0.0
        };
        format!(
            "clipped {} of {} components ({:.2e})",
            self.clipped, self.components, r
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cs8_scaling_and_clip_count() {
        assert_eq!(Quantizer::<Cs8>::full_scale(), 127.0);
        let mut s = Quantizer::<Cs8>::new(0.0);
        assert!((s.scale() - 127.0).abs() < 1e-4);
        let v = s.convert(&[C32::new(0.5, -0.5), C32::new(1.5, -2.0)]);
        assert_eq!(v, vec![64, -64, 127, -127]);
        assert_eq!((s.clipped(), s.components()), (2, 4));
        assert_eq!(s.describe(), "clipped 2 of 4 components (5.00e-1)");
        let s = Quantizer::<Cs8>::new(20.0);
        assert!((s.scale() - 12.7).abs() < 1e-4);
        assert_eq!(
            Quantizer::<Cs8>::new(10.0).describe_level(),
            "10.0 dB below cs8 full scale (RMS 40.2 of 127)"
        );
    }

    #[test]
    fn cs12_is_left_aligned_and_clips_at_12_bits() {
        assert_eq!(Quantizer::<Cs12>::full_scale(), 2047.0);
        let mut s = Quantizer::<Cs12>::new(0.0);
        let v = s.convert(&[C32::new(0.5, -0.5), C32::new(1.5, -2.0), C32::new(0.0, 1.0)]);
        // 0.5 · 2047 = 1023.5 → 1024 (rounded half away from zero), × 16.
        assert_eq!(
            v,
            vec![1024 << 4, -1024 << 4, 2047 << 4, -2047 << 4, 0, 2047 << 4]
        );
        assert_eq!((s.clipped(), s.components()), (2, 6));
        assert!(v.iter().all(|x| x % 16 == 0));
        assert_eq!(
            Quantizer::<Cs12>::new(10.0).describe_level(),
            "10.0 dB below cs12 full scale (RMS 647.3 of 2047)"
        );
    }

    #[test]
    fn same_bytes_as_the_phy_conversion() {
        // The scale must be bit-identical to 127 · 10^(−b/20) so cs8 files
        // and radio buffers do not change with the refactoring.
        let x: Vec<C32> = (0..1000)
            .map(|i| C32::new((i as f32 * 0.37).sin(), (i as f32 * 0.11).cos()))
            .collect();
        for b in [0.0f32, 3.5, 10.0, 17.25] {
            let scale = 127.0 * 10f32.powf(-b / 20.0);
            let mut want = Vec::new();
            let clipped = isdbt_phy::iq::cs8_from_samples(&x, scale, &mut want);
            let mut q = Quantizer::<Cs8>::new(b);
            assert_eq!(q.scale().to_bits(), scale.to_bits());
            assert_eq!(q.convert(&x), want);
            assert_eq!(q.clipped(), clipped as u64);
        }
    }
}

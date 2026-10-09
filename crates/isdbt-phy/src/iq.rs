//! Sample-format conversion for IQ files and the HackRF wire format.

use isdbt_ofdm::C32;

/// Interleaved little-endian f32 I/Q ("cf32", GNU Radio complex) → samples.
pub fn cf32_from_bytes(b: &[u8]) -> Vec<C32> {
    b.as_chunks::<8>()
        .0
        .iter()
        .map(|c| {
            C32::new(
                f32::from_le_bytes([c[0], c[1], c[2], c[3]]),
                f32::from_le_bytes([c[4], c[5], c[6], c[7]]),
            )
        })
        .collect()
}

/// Samples → interleaved little-endian f32 I/Q.
pub fn cf32_to_bytes(s: &[C32]) -> Vec<u8> {
    let mut v = Vec::with_capacity(s.len() * 8);
    for c in s {
        v.extend_from_slice(&c.re.to_le_bytes());
        v.extend_from_slice(&c.im.to_le_bytes());
    }
    v
}

/// Samples → interleaved signed 8-bit I/Q (HackRF "cs8"), scaled by
/// `scale` (full scale = 127) with saturation; returns the number of
/// clipped components alongside.
pub fn cs8_from_samples(s: &[C32], scale: f32, out: &mut Vec<i8>) -> usize {
    out.clear();
    out.reserve(s.len() * 2);
    let mut clipped = 0;
    for c in s {
        for v in [c.re, c.im] {
            let x = (v * scale).round();
            if !(-127.0..=127.0).contains(&x) {
                clipped += 1;
            }
            out.push(x.clamp(-127.0, 127.0) as i8);
        }
    }
    clipped
}

/// Interleaved signed 8-bit I/Q → samples scaled to ±1.
pub fn samples_from_cs8(b: &[i8]) -> Vec<C32> {
    b.as_chunks::<2>()
        .0
        .iter()
        .map(|&[i, q]| C32::new(i as f32 / 128.0, q as f32 / 128.0))
        .collect()
}

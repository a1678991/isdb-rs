//! Carrier modulation (ARIB STD-B31 §3.9): delay adjustment (Table 3-9),
//! bit interleaving (Figs. 3-12/3-14/3-16/3-18), mapping (Figs. 3-13/3-15/
//! 3-17/3-19, Table 3-10) and modulation-level normalisation (Table 3-11);
//! and the receiver inverses (max-log demapping, differential detection,
//! bit deinterleaving).
//!
//! Bit labels: a carrier symbol's bits are `b0..b(S-1)` in the order the
//! serial-to-parallel converter takes them from the input stream (b0
//! first).  As an integer label, b0 is the most significant bit, so the
//! label reads like the figures' "(b0,b1,…)".

use crate::delay::MultiDelay;
use crate::C32;
use isdbt_core::ofdm_params::{bit_delay_adjustment_bits, bit_interleave_delays, BIT_INTERLEAVE_MAX_DELAY};
use isdbt_core::{LayerConfig, Mode, Modulation};

/// Normalisation factor of Table 3-11 (`Z/√2`, `Z/√2`, `Z/√10`, `Z/√42`).
pub fn normalization(m: Modulation) -> f32 {
    match m {
        Modulation::Dqpsk | Modulation::Qpsk => 1.0 / 2f32.sqrt(),
        Modulation::Qam16 => 1.0 / 10f32.sqrt(),
        Modulation::Qam64 => 1.0 / 42f32.sqrt(),
    }
}

/// Per-axis Gray level: the bits of one axis (for 64QAM I: b0,b2,b4; Q:
/// b1,b3,b5), most significant first, to the un-normalised level.
///
/// Read off Figs. 3-15/3-17/3-19: the first axis bit selects the sign
/// (0 → +), the remaining bits the magnitude: 16QAM `0→3, 1→1`;
/// 64QAM `00→7, 01→5, 11→3, 10→1`.
fn axis_level(bits: &[u8]) -> i32 {
    let sign = if bits[0] == 0 { 1 } else { -1 };
    let mag = match bits.len() {
        1 => 1,
        2 => {
            if bits[1] == 0 {
                3
            } else {
                1
            }
        }
        3 => match (bits[1], bits[2]) {
            (0, 0) => 7,
            (0, 1) => 5,
            (1, 1) => 3,
            _ => 1,
        },
        _ => unreachable!(),
    };
    sign * mag
}

/// Un-normalised constellation point `Z = I + jQ` (integer levels) for
/// coherent modulation `m` and label (b0 = MSB of `label`).  For
/// [`Modulation::Dqpsk`] this returns the QPSK point (used for the
/// differential phase decision, which has the same labelling).
pub fn constellation_point_raw(m: Modulation, label: u8) -> (i32, i32) {
    let s = m.bits_per_symbol();
    let bit = |k: usize| (label >> (s - 1 - k)) & 1;
    let ibits: Vec<u8> = (0..s).step_by(2).map(bit).collect();
    let qbits: Vec<u8> = (1..s).step_by(2).map(bit).collect();
    (axis_level(&ibits), axis_level(&qbits))
}

/// Normalised constellation point (Table 3-11 applied).
pub fn constellation_point(m: Modulation, label: u8) -> C32 {
    let (i, q) = constellation_point_raw(m, label);
    C32::new(i as f32, q as f32) * normalization(m)
}

/// π/4-shift DQPSK phase θj for the (delayed) bit pair b0', b1'
/// (Table 3-10): 00 → π/4, 01 → −π/4, 10 → 3π/4, 11 → −3π/4.
pub fn dqpsk_phase(b0: u8, b1: u8) -> f32 {
    use std::f32::consts::FRAC_PI_4;
    match (b0, b1) {
        (0, 0) => FRAC_PI_4,
        (0, 1) => -FRAC_PI_4,
        (1, 0) => 3.0 * FRAC_PI_4,
        _ => -3.0 * FRAC_PI_4,
    }
}

/// Deterministic generator for the start-up fill of the delay lines
/// (not part of the spec; see SPEC-NOTES "start-up fill").  A fill that is
/// identical on every carrier adds up coherently in the IFFT (≈ 40 dB PAPR),
/// so the fill bits and the initial DQPSK references are pseudo-random.
#[derive(Debug, Clone)]
pub(crate) struct FillRng(u64);

impl FillRng {
    pub(crate) fn new(seed: u64) -> Self {
        Self(seed ^ 0x9E37_79B9_7F4A_7C15)
    }
    pub(crate) fn next_u32(&mut self) -> u32 {
        // SplitMix64.
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        ((z ^ (z >> 31)) >> 32) as u32
    }
    pub(crate) fn bit(&mut self) -> u8 {
        (self.next_u32() >> 31) as u8
    }
}

/// Per-axis level table (normalised level, axis bits as integer MSB-first)
/// for the max-log demapper.
fn axis_table(m: Modulation) -> Vec<(f32, u8)> {
    let nb = m.bits_per_symbol() / 2;
    (0..(1u8 << nb))
        .map(|v| {
            let bits: Vec<u8> = (0..nb).map(|k| (v >> (nb - 1 - k)) & 1).collect();
            (axis_level(&bits) as f32 * normalization(m), v)
        })
        .collect()
}

/// Transmit-side carrier modulator of one hierarchical layer (§3.9):
/// delay adjustment (Table 3-9) + bit interleaving + mapping.
///
/// Input per OFDM symbol: `nc·N·S` bits (the layer's share of the coded
/// stream for one OFDM symbol); output: `nc·N` normalised carrier symbols
/// in layer order (to [`crate::combine`]).
///
/// The delay adjustment (`X` bits, Table 3-9) is a pure delay of the
/// whole bit stream; since `X` is a multiple of `S` (asserted) it is
/// realised as `X/S` symbols added to every branch delay, which is
/// bit-exact equivalent.  The delay lines start filled with pseudo-random
/// bits (see [`CarrierMapper::with_seed`]).
#[derive(Debug, Clone)]
pub struct CarrierMapper {
    modulation: Modulation,
    per_symbol: usize,
    lines: MultiDelay<u8>,
    staged_in: Vec<u8>,
    staged_out: Vec<u8>,
    /// DQPSK: previous output (un-normalised I, Q) per layer carrier
    /// position (the "Delay" feedback of Fig. 3-12; one OFDM symbol).
    dqpsk_prev: Vec<(f32, f32)>,
}

impl CarrierMapper {
    /// Mapper for `layer` in `mode` (start-up fill seed 0).
    pub fn new(mode: Mode, layer: &LayerConfig) -> Self {
        Self::with_seed(mode, layer, 0)
    }

    /// Mapper whose delay lines start with pseudo-random bits and whose
    /// DQPSK references start at pseudo-random diagonal points (Fig. 3-13
    /// (±1, ±1)), derived from `seed`.  The spec does not define the
    /// start-up state; random content keeps the first OFDM symbols' PAPR
    /// normal.
    pub fn with_seed(mode: Mode, layer: &LayerConfig, seed: u64) -> Self {
        let mut rng = FillRng::new(seed);
        let m = layer.modulation;
        let s = m.bits_per_symbol();
        let adj_bits = bit_delay_adjustment_bits(mode, m, layer.segments);
        assert_eq!(adj_bits % s, 0);
        let adj = adj_bits / s;
        let delays: Vec<usize> = bit_interleave_delays(m).iter().map(|d| d + adj).collect();
        let per_symbol = mode.data_carriers_per_segment() * layer.segments;
        Self {
            modulation: m,
            per_symbol,
            lines: MultiDelay::with_contents(
                delays.clone(),
                delays.iter().map(|&d| (0..d).map(|_| rng.bit()).collect()).collect(),
            ),
            staged_in: vec![0; s],
            staged_out: vec![0; s],
            dqpsk_prev: if m.is_differential() {
                (0..per_symbol)
                    .map(|_| {
                        let v = rng.next_u32();
                        (if v & 1 == 0 { 1.0 } else { -1.0 }, if v & 2 == 0 { 1.0 } else { -1.0 })
                    })
                    .collect()
            } else {
                Vec::new()
            },
        }
    }

    /// DQPSK: current differential reference of layer position `p`
    /// (normalised), i.e. the previous output — before the first symbol,
    /// the start-up reference that the modulator's fill must end in.
    pub fn dqpsk_reference(&self, p: usize) -> C32 {
        let (i, q) = self.dqpsk_prev[p];
        C32::new(i, q) * normalization(self.modulation)
    }

    /// Carrier symbols per OFDM symbol (`nc·N`).
    pub fn symbols_per_ofdm_symbol(&self) -> usize {
        self.per_symbol
    }

    /// Map one OFDM symbol worth of bits (`nc·N·S`) to `nc·N` normalised
    /// symbols.
    pub fn map_symbol(&mut self, bits: &[u8], out: &mut [C32]) {
        let s = self.modulation.bits_per_symbol();
        assert_eq!(bits.len(), self.per_symbol * s);
        assert_eq!(out.len(), self.per_symbol);
        let norm = normalization(self.modulation);
        for (p, chunk) in bits.chunks_exact(s).enumerate() {
            self.staged_in.copy_from_slice(chunk);
            self.lines.push(&self.staged_in, &mut self.staged_out);
            let b = &self.staged_out;
            out[p] = if self.modulation.is_differential() {
                let th = dqpsk_phase(b[0], b[1]);
                let (c, sn) = (th.cos(), th.sin());
                let (i0, q0) = self.dqpsk_prev[p];
                let (i1, q1) = (c * i0 - sn * q0, sn * i0 + c * q0);
                // Snap to the exact lattice {0, ±1, ±√2} to avoid drift.
                let snap = |v: f32| -> f32 {
                    let r2 = std::f32::consts::SQRT_2;
                    if v.abs() < 0.5 {
                        0.0
                    } else if (v.abs() - 1.0).abs() < 0.2 {
                        v.signum()
                    } else {
                        v.signum() * r2
                    }
                };
                let (i1, q1) = (snap(i1), snap(q1));
                self.dqpsk_prev[p] = (i1, q1);
                C32::new(i1, q1) * norm
            } else {
                let label = b.iter().fold(0u8, |a, &x| (a << 1) | x);
                constellation_point(self.modulation, label)
            };
        }
    }
}

/// Max-log demapper for one modulation (precomputed per-axis levels).
#[derive(Debug, Clone)]
pub struct Demapper {
    m: Modulation,
    nb: usize,
    table: Vec<(f32, u8)>,
}

impl Demapper {
    /// Demapper for `m` (DQPSK uses the QPSK grid on the differential product).
    pub fn new(m: Modulation) -> Self {
        Self { m, nb: m.bits_per_symbol() / 2, table: axis_table(m) }
    }

    /// Modulation.
    pub fn modulation(&self) -> Modulation {
        self.m
    }

    /// Max-log LLRs for one carrier symbol.
    ///
    /// `z` is the equalised symbol on the normalised constellation (for
    /// DQPSK: the differential product, see [`dqpsk_differential`]), `csi`
    /// the reliability weight (`|H|²/N0` for coherent symbols) such that
    /// `LLR_b = csi·(min_{s:b=1}|z−s|² − min_{s:b=0}|z−s|²)`.  Writes `S`
    /// LLRs (b0..b(S-1)) to `out`.  The I axis carries b0, b2, b4 and the Q
    /// axis b1, b3, b5 (Figs. 3-15/3-17/3-19).
    pub fn demap(&self, z: C32, csi: f32, out: &mut [f32]) {
        let nb = self.nb;
        for (axis, v) in [(0usize, z.re), (1, z.im)] {
            for k in 0..nb {
                let mut d0 = f32::INFINITY;
                let mut d1 = f32::INFINITY;
                for &(lvl, bits) in &self.table {
                    let d = (v - lvl) * (v - lvl);
                    if (bits >> (nb - 1 - k)) & 1 == 0 {
                        d0 = d0.min(d);
                    } else {
                        d1 = d1.min(d);
                    }
                }
                out[2 * k + axis] = csi * (d1 - d0);
            }
        }
    }

    /// Nearest constellation point (normalised) to `z`.
    pub fn nearest(&self, z: C32) -> C32 {
        let pick = |v: f32| {
            self.table.iter().map(|&(l, _)| l).fold(f32::NAN, |best, l| {
                if best.is_nan() || (v - l).abs() < (v - best).abs() {
                    l
                } else {
                    best
                }
            })
        };
        C32::new(pick(z.re), pick(z.im))
    }
}

/// Max-log LLRs for one carrier symbol (see [`Demapper::demap`]).
pub fn demap(m: Modulation, z: C32, csi: f32, out: &mut [f32]) {
    Demapper::new(m).demap(z, csi, out)
}

/// Fast hard decision (label) of an equalised symbol.
pub fn hard_label(m: Modulation, z: C32) -> u8 {
    let mut llr = [0f32; 6];
    let s = m.bits_per_symbol();
    demap(m, z, 1.0, &mut llr[..s]);
    llr[..s].iter().fold(0u8, |a, &l| (a << 1) | u8::from(l < 0.0))
}

/// Differential detection for π/4-shift DQPSK (inverse of the Fig. 3-12
/// recursion): returns `(z, csi)` to feed [`demap`] with
/// [`Modulation::Dqpsk`].
///
/// With `y_j = H·x_j + n_j`, `|x| = 1` and complex noise variance `n0`,
/// `d = y_j·conj(y_{j-1}) ≈ |H|²·e^{jθ}`; the max-log LLR of each axis is
/// `√2·Re(d)/n0` (resp. `Im`), obtained by demapping `z = d` on the
/// normalised QPSK grid (points `(±1±j)/√2`, same labels as Table 3-10)
/// with `csi = 1/(2·n0)`.
pub fn dqpsk_differential(y: C32, y_prev: C32, n0: f32) -> (C32, f32) {
    (y * y_prev.conj(), 1.0 / (2.0 * n0.max(1e-12)))
}

/// Receive-side bit deinterleaver of one layer: delays LLR branch `k` by
/// `120 − d_k` carrier symbols so every bit sees 120 symbols of
/// interleave delay in total (§3.9.2).
#[derive(Debug, Clone)]
pub struct BitDeinterleaver {
    s: usize,
    lines: MultiDelay<f32>,
    staged_in: Vec<f32>,
    staged_out: Vec<f32>,
}

impl BitDeinterleaver {
    /// Deinterleaver for modulation `m`.
    pub fn new(m: Modulation) -> Self {
        let delays: Vec<usize> = bit_interleave_delays(m).iter().map(|d| BIT_INTERLEAVE_MAX_DELAY - d).collect();
        let s = m.bits_per_symbol();
        Self { s, lines: MultiDelay::new(delays, 0.0), staged_in: vec![0.0; s], staged_out: vec![0.0; s] }
    }

    /// Process LLRs of consecutive carrier symbols in place (`S` per symbol).
    pub fn process(&mut self, llr: &mut [f32]) {
        assert_eq!(llr.len() % self.s, 0);
        for chunk in llr.chunks_exact_mut(self.s) {
            self.staged_in.copy_from_slice(chunk);
            self.lines.push(&self.staged_in, &mut self.staged_out);
            chunk.copy_from_slice(&self.staged_out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn demap_signs_match_labels() {
        for m in [Modulation::Qpsk, Modulation::Qam16, Modulation::Qam64] {
            for label in 0..(1u8 << m.bits_per_symbol()) {
                assert_eq!(hard_label(m, constellation_point(m, label)), label, "{m:?} {label}");
            }
        }
    }
}

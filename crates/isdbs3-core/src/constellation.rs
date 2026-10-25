//! Signal points of the ISDB-S3 modulations (ARIB STD-B44 v2.1 §3.8,
//! Fig. 3.8-1, Tables 3.8-2/3), as (bit label, point) with the label's MSB
//! the first bit of the symbol.
//!
//! π/2-shift BPSK: 0 / 1 at +45° / −135° on odd symbols (the first of the
//! frame is odd), turned 90° counter-clockwise on even symbols. QPSK
//! [C1 C0], 8PSK [C2 C1 C0], 16APSK [C3 C2 C1 C0] and 32APSK
//! [C4 … C0] are read from the figure (crate SPEC-NOTES "Constellation
//! labels"). APSK rings: R1 = 1 relative, R2 = γ, R3 = γ₂; the point set is
//! scaled to unit mean power (§3.8: 4R₁² + 12R₂² = 16, 4R₁² + 12R₂² +
//! 16R₃² = 32).

use crate::params::{CodeRate, Modulation, gamma16, gamma32};
use std::f64::consts::PI;

/// A point (I, Q).
pub type Point = (f64, f64);

fn deg(r: f64, d: f64) -> Point {
    let a = d * PI / 180.0;
    (r * a.cos(), r * a.sin())
}

/// QPSK [C1 C0] → phase (Fig. 3.8-1).
const QPSK: [(u8, f64); 4] = [(0b00, 45.0), (0b10, 135.0), (0b11, 225.0), (0b01, 315.0)];
/// 8PSK [C2 C1 C0] → phase.
const PSK8: [(u8, f64); 8] = [
    (0b001, 0.0),
    (0b000, 45.0),
    (0b100, 90.0),
    (0b110, 135.0),
    (0b010, 180.0),
    (0b011, 225.0),
    (0b111, 270.0),
    (0b101, 315.0),
];
/// 16APSK inner ring (R1) [C3…C0] → phase.
const APSK16_R1: [(u8, f64); 4] = [
    (0b1100, 45.0),
    (0b1110, 135.0),
    (0b1111, 225.0),
    (0b1101, 315.0),
];
/// 16APSK outer ring (R2).
const APSK16_R2: [(u8, f64); 12] = [
    (0b0100, 15.0),
    (0b0000, 45.0),
    (0b1000, 75.0),
    (0b1010, 105.0),
    (0b0010, 135.0),
    (0b0110, 165.0),
    (0b0111, 195.0),
    (0b0011, 225.0),
    (0b1011, 255.0),
    (0b1001, 285.0),
    (0b0001, 315.0),
    (0b0101, 345.0),
];
/// 32APSK ring R1.
const APSK32_R1: [(u8, f64); 4] = [
    (0b10001, 45.0),
    (0b10101, 135.0),
    (0b10111, 225.0),
    (0b10011, 315.0),
];
/// 32APSK ring R2.
const APSK32_R2: [(u8, f64); 12] = [
    (0b10000, 15.0),
    (0b00000, 45.0),
    (0b00001, 75.0),
    (0b00101, 105.0),
    (0b00100, 135.0),
    (0b10100, 165.0),
    (0b10110, 195.0),
    (0b00110, 225.0),
    (0b00111, 255.0),
    (0b00011, 285.0),
    (0b00010, 315.0),
    (0b10010, 345.0),
];
/// 32APSK ring R3.
const APSK32_R3: [(u8, f64); 16] = [
    (0b11000, 0.0),
    (0b01000, 22.5),
    (0b11001, 45.0),
    (0b01001, 67.5),
    (0b01101, 90.0),
    (0b11101, 112.5),
    (0b01100, 135.0),
    (0b11100, 157.5),
    (0b11110, 180.0),
    (0b01110, 202.5),
    (0b11111, 225.0),
    (0b01111, 247.5),
    (0b01011, 270.0),
    (0b11011, 292.5),
    (0b01010, 315.0),
    (0b11010, 337.5),
];

/// The points of `m` at rate `r` (γ matters for the APSKs only), indexed
/// by bit label, at unit mean power. π/2-BPSK gives the odd-symbol points.
pub fn points(m: Modulation, r: CodeRate) -> Vec<Point> {
    let mut p = vec![(0.0, 0.0); 1 << m.bits()];
    match m {
        Modulation::Bpsk => {
            p[0] = deg(1.0, 45.0);
            p[1] = deg(1.0, 225.0);
        }
        Modulation::Qpsk => QPSK.iter().for_each(|&(l, d)| p[l as usize] = deg(1.0, d)),
        Modulation::Psk8 => PSK8.iter().for_each(|&(l, d)| p[l as usize] = deg(1.0, d)),
        Modulation::Apsk16 => {
            let g = gamma16(r);
            APSK16_R1
                .iter()
                .for_each(|&(l, d)| p[l as usize] = deg(1.0, d));
            APSK16_R2
                .iter()
                .for_each(|&(l, d)| p[l as usize] = deg(g, d));
        }
        Modulation::Apsk32 => {
            let (g1, g2) = gamma32(r);
            APSK32_R1
                .iter()
                .for_each(|&(l, d)| p[l as usize] = deg(1.0, d));
            APSK32_R2
                .iter()
                .for_each(|&(l, d)| p[l as usize] = deg(g1, d));
            APSK32_R3
                .iter()
                .for_each(|&(l, d)| p[l as usize] = deg(g2, d));
        }
    }
    let pw = p.iter().map(|(i, q)| i * i + q * q).sum::<f64>() / p.len() as f64;
    let s = pw.sqrt();
    p.iter().map(|(i, q)| (i / s, q / s)).collect()
}

/// Which ring each label of `m` sits on (0 = innermost); for PSKs all 0.
pub fn rings(m: Modulation) -> Vec<u8> {
    let mut r = vec![0u8; 1 << m.bits()];
    match m {
        Modulation::Apsk16 => APSK16_R2.iter().for_each(|&(l, _)| r[l as usize] = 1),
        Modulation::Apsk32 => {
            APSK32_R2.iter().for_each(|&(l, _)| r[l as usize] = 1);
            APSK32_R3.iter().for_each(|&(l, _)| r[l as usize] = 2);
        }
        _ => {}
    }
    r
}

/// The π/2-BPSK point of bit `b` for symbol number `i` of the frame
/// (1-based as in §3.8: i = 1 is odd).
pub fn bpsk(b: u8, i: usize) -> Point {
    let base = if b & 1 == 0 { 45.0 } else { 225.0 };
    let turn = if i % 2 == 1 { 0.0 } else { 90.0 };
    deg(1.0, base + turn)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rate(r: u8) -> CodeRate {
        CodeRate::new(r).unwrap()
    }

    #[test]
    fn every_label_is_placed_once_at_unit_power() {
        for m in [
            Modulation::Qpsk,
            Modulation::Psk8,
            Modulation::Apsk16,
            Modulation::Apsk32,
        ] {
            let p = points(m, rate(93));
            assert_eq!(p.len(), 1 << m.bits());
            for (i, a) in p.iter().enumerate() {
                for b in &p[i + 1..] {
                    assert!((a.0 - b.0).hypot(a.1 - b.1) > 1e-3, "{m:?} duplicate");
                }
            }
            let pw = p.iter().map(|(i, q)| i * i + q * q).sum::<f64>() / p.len() as f64;
            assert!((pw - 1.0).abs() < 1e-12);
        }
    }

    #[test]
    fn spec_apsk_power_normalisation_and_gray_neighbours() {
        // §3.8: 4R1² + 12R2² = 16 at R2 = γ·R1
        let g = gamma16(rate(93));
        let r1 = (16.0 / (4.0 + 12.0 * g * g)).sqrt();
        let p = points(Modulation::Apsk16, rate(93));
        let inner = p[0b1100];
        assert!((inner.0.hypot(inner.1) - r1).abs() < 1e-12);
        // 8PSK in the figure is Gray-coded: neighbours differ in one bit
        let p8 = PSK8;
        for i in 0..8 {
            let (a, b) = (p8[i].0, p8[(i + 1) % 8].0);
            assert_eq!((a ^ b).count_ones(), 1, "{a:03b} {b:03b}");
        }
    }

    #[test]
    fn spec_pi2_bpsk_rotation() {
        let (i1, q1) = bpsk(0, 1);
        let (i2, q2) = bpsk(0, 2);
        assert!(i1 > 0.0 && q1 > 0.0); // 1st quadrant on odd symbols
        assert!(i2 < 0.0 && q2 > 0.0); // turned 90° on even
        let (i3, q3) = bpsk(1, 1);
        assert!(i3 < 0.0 && q3 < 0.0); // 3rd quadrant
    }
}

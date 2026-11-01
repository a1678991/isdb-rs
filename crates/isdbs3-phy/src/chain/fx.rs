//! Fixed-point primitives shared by the chain's stages, each defined
//! exactly so the HDL can reproduce it: rounding is "add half, arithmetic
//! shift" everywhere (as in `isdb_frontend::fixed`), angles are binary
//! angles (`u32`, 2³² = one turn, wrapping), and every saturation is
//! stated.

/// `round(v / 2^s)`: add half, arithmetic shift (halves rounded up).
/// `s = 0` returns `v`.
pub fn round_shift(v: i64, s: u32) -> i64 {
    if s == 0 {
        v
    } else {
        (v + (1i64 << (s - 1))) >> s
    }
}

/// Saturate to `i16`.
pub fn sat16(v: i64) -> i16 {
    // clamped to the i16 range first, so the cast is exact
    #[allow(clippy::cast_possible_truncation)]
    let s = v.clamp(i64::from(i16::MIN), i64::from(i16::MAX)) as i16;
    s
}

/// Saturate to ±127 as an `i8` (the LDPC decoder's input range; −128 is
/// never produced).
pub fn sat127(v: i64) -> i8 {
    // clamped to ±127 first, so the cast is exact
    #[allow(clippy::cast_possible_truncation)]
    let s = v.clamp(-127, 127) as i8;
    s
}

/// A value known to fit `i32` by its stage's word width, as `i32`
/// (saturating, so a violated width shows as a clipped value, never as a
/// wrapped one).
pub fn sat32(v: i64) -> i32 {
    // clamped to the i32 range first, so the cast is exact
    #[allow(clippy::cast_possible_truncation)]
    let s = v.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32;
    s
}

/// The slot search's magnitude: `max(|re|, |im|) + (min(|re|, |im|) >> 1)`.
/// It overestimates |c| by at most 11.8 % (at 26.6°) and never
/// underestimates it: enough to find a correlation peak; no amplitude
/// is taken from it (the slot amplitude is the [`cordic`] magnitude).
pub fn mag_approx(re: i64, im: i64) -> i64 {
    let (a, b) = (re.abs(), im.abs());
    a.max(b) + (a.min(b) >> 1)
}

/// CORDIC iterations of [`cordic`].
pub const CORDIC_ITERATIONS: usize = 16;

/// `round(atan(2⁻ⁱ) / 2π · 2³²)`, i = 0 … 15: the CORDIC's angle table
/// in binary-angle units.
pub const CORDIC_ATAN: [u32; CORDIC_ITERATIONS] = [
    536_870_912,
    316_933_406,
    167_458_907,
    85_004_756,
    42_667_331,
    21_354_465,
    10_679_838,
    5_340_245,
    2_670_163,
    1_335_087,
    667_544,
    333_772,
    166_886,
    83_443,
    41_722,
    20_861,
];

/// `round(2¹⁶ / K)`, K = Π √(1 + 2⁻²ⁱ) over the 16 iterations
/// (1.646 760 258): the CORDIC's gain compensation, Q0.16.
pub const CORDIC_INV_GAIN: i64 = 39_797;

/// Bits the CORDIC input is scaled up by before the iterations.
pub const CORDIC_PRESCALE: u32 = 8;

/// Largest input component [`cordic`] is defined for: |re|, |im| < 2²².
pub const CORDIC_MAX_IN: i64 = 1 << 22;

/// CORDIC vectoring of `(re, im)` (|re|, |im| < [`CORDIC_MAX_IN`]): the
/// angle as a binary angle and the magnitude, both integers.
///
/// 1. `x = re << 8`, `y = im << 8`, `z = 0`; if `x < 0`: `x, y = −x, −y`,
///    `z = 2³¹` (half a turn, so the iterations start in the right
///    half-plane).
/// 2. For i = 0 … 15, with `dx = y >> i`, `dy = x >> i` (arithmetic
///    shifts of the values before the step): if `y > 0`: `x += dx`,
///    `y −= dy`, `z += atan[i]`; else `x −= dx`, `y += dy`, `z −= atan[i]`
///    (`z` wraps).
/// 3. magnitude `= round_shift(x · 39 797, 16 + 8)`.
///
/// Registers: x and y need 22 + 8 + 2 = 32 bits signed (|x| grows by K ≤
/// 1.65 and √2), z 32 bits. The angle is within 6.2·10⁻⁵ rad of `atan2`
/// of the integer input; the magnitude within ±1 + 10⁻⁴ relative.
pub fn cordic(re: i64, im: i64) -> (u32, i64) {
    debug_assert!(re.abs() < CORDIC_MAX_IN && im.abs() < CORDIC_MAX_IN);
    let (mut x, mut y) = (re << CORDIC_PRESCALE, im << CORDIC_PRESCALE);
    let mut z = 0u32;
    if x < 0 {
        x = -x;
        y = -y;
        z = 1 << 31;
    }
    for (i, &a) in CORDIC_ATAN.iter().enumerate() {
        let (dx, dy) = (y >> i, x >> i);
        if y > 0 {
            x += dx;
            y -= dy;
            z = z.wrapping_add(a);
        } else {
            x -= dx;
            y += dy;
            z = z.wrapping_sub(a);
        }
    }
    (z, round_shift(x * CORDIC_INV_GAIN, 16 + CORDIC_PRESCALE))
}

/// `(re, im) >> s` with the smallest `s ≥ 0` that brings both components
/// below [`CORDIC_MAX_IN`] in magnitude (arithmetic shifts): a
/// leading-zero count and a barrel shift in the HDL. Only for angles (the
/// shift changes the magnitude).
pub fn cordic_scale(re: i64, im: i64) -> (i64, i64) {
    let mut s = 0;
    while (re >> s).abs() >= CORDIC_MAX_IN || (im >> s).abs() >= CORDIC_MAX_IN {
        s += 1;
    }
    (re >> s, im >> s)
}

/// A binary angle as a signed value in [−2³¹, 2³¹) (units of 2⁻³² turn).
pub fn signed_angle(a: u32) -> i64 {
    i64::from(a.cast_signed())
}

/// The cosine and sine of binary angle `a` from the 1 024-entry cosine
/// table `table` (`isdb_frontend::tables::cos_table`, amplitude 32 767):
/// entry `idx = a >> 22` (truncated), sine at `idx + 768` (mod 1 024).
pub fn cos_sin_trunc(table: &[i16], a: u32) -> (i64, i64) {
    let idx = (a >> 22) as usize;
    (i64::from(table[idx]), i64::from(table[(idx + 768) & 1023]))
}

/// [`cos_sin_trunc`] at the nearest entry: `idx = ((a + 2²¹) >> 22) mod
/// 1 024` (the addition wrapping), so the phase error is at most half an
/// entry (π/1 024 rad).
pub fn cos_sin_round(table: &[i16], a: u32) -> (i64, i64) {
    cos_sin_trunc(table, a.wrapping_add(1 << 21))
}

/// `round(a / b)` for `b > 0`, halves rounded up: `floor((2a + b) / 2b)`.
///
/// # Panics
///
/// If `b ≤ 0`.
pub fn div_round(a: i64, b: i64) -> i64 {
    assert!(b > 0, "div_round by {b}");
    (2 * a + b).div_euclid(2 * b)
}

/// `round(2³⁷ / 9 296)`: one modulation slot's reciprocal, the multiplier
/// that replaces a division by 9 296 (`x / 9 296 ≈ x · R >> 37`; relative
/// error 1.6·10⁻⁸).
pub const SLOT_RECIP: i64 = 14_784_741;

/// `round(x / 9 296)` as `round_shift(x · SLOT_RECIP, 37)` (|x| < 2²⁶ ·
/// 2⁴ keeps the product inside 63 bits; the stages use |x| ≤ 2³¹).
pub fn div_slot(x: i64) -> i64 {
    round_shift(x * SLOT_RECIP, 37)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::{PI, TAU};

    // test values far inside i64 / f64's exact range
    #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
    #[test]
    fn cordic_angle_and_magnitude() {
        for k in 0..64 {
            let ang = TAU * f64::from(k) / 64.0 + 0.01;
            for r in [64.0, 100.0, 1500.0, 140_000.0, 4.0e6] {
                let (re, im) = ((r * ang.cos()).round(), (r * ang.sin()).round());
                let (a, m) = cordic(re as i64, im as i64);
                let want = im.atan2(re);
                let got = signed_angle(a) as f64 / 2f64.powi(32) * TAU;
                let d = (got - want + PI).rem_euclid(TAU) - PI;
                // the last iteration's angle (2⁻¹⁵ rad) plus the input's
                // rounding (0.7 / r rad)
                assert!(d.abs() < 6.2e-5 + 0.7 / r, "{k} {r}: {got} vs {want}");
                let mag = re.hypot(im);
                assert!(
                    (m as f64 - mag).abs() <= 1.0 + mag * 1e-4,
                    "{k} {r}: {m} vs {mag}"
                );
            }
        }
        assert_eq!(cordic(0, 0).1, 0);
        // the half-plane flip: (−r, 0) is half a turn
        assert_eq!(cordic(-1000, 0).0 >> 20, (1u32 << 31) >> 20);
    }

    // f64 holds these integers exactly
    #[allow(clippy::cast_precision_loss)]
    #[test]
    fn cordic_tables_are_the_stated_constants() {
        for (i, &a) in CORDIC_ATAN.iter().enumerate() {
            let v = 0.5f64.powi(i32::try_from(i).unwrap()).atan() / TAU * 2f64.powi(32);
            assert_eq!(f64::from(a), v.round(), "{i}");
        }
        let k: f64 = (0..CORDIC_ITERATIONS)
            .map(|i| (1.0 + 0.25f64.powi(i32::try_from(i).unwrap())).sqrt())
            .product();
        assert_eq!(CORDIC_INV_GAIN as f64, (65_536.0 / k).round());
        assert_eq!(SLOT_RECIP as f64, (2f64.powi(37) / 9_296.0).round());
    }

    #[test]
    fn rounding_helpers() {
        assert_eq!(round_shift(5, 1), 3);
        assert_eq!(round_shift(-5, 1), -2);
        assert_eq!(round_shift(7, 0), 7);
        assert_eq!(div_round(5, 2), 3);
        assert_eq!(div_round(-5, 2), -2);
        assert_eq!(div_round(-7, 3), -2);
        assert_eq!(sat127(-300), -127);
        assert_eq!(sat16(40_000), i16::MAX);
        assert_eq!(mag_approx(-10, 4), 12);
        assert_eq!(cordic_scale(1 << 30, -5), (1 << 21, -1));
        for x in [0i64, 1, 9_296, -9_296 * 5 + 4_648, 1 << 31, -(1 << 31)] {
            assert_eq!(div_slot(x), div_round(x, 9_296), "{x}");
        }
    }

    #[test]
    fn cos_sin_entries() {
        let t = isdb_frontend::tables::cos_table();
        assert_eq!(cos_sin_trunc(&t, 0), (32_767, 0));
        // just below a quarter turn: the truncated entry is 255, the
        // rounded one 256
        let a = (1 << 30) - (1 << 21) + 1;
        assert_eq!(cos_sin_round(&t, a), (0, 32_767));
        assert_ne!(cos_sin_trunc(&t, a), (0, 32_767));
    }
}

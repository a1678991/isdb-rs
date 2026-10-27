//! Float → integer conversions of tick counts, packet counts and rates.
//!
//! `as` from a float truncates toward zero and saturates (NaN → 0, ±∞ →
//! the bounds), and std has no fallible conversion. The callers pass
//! durations in 27 MHz ticks, packet counts and scaled rates that a
//! non-negative `f64` holds far below the integer bounds, so the
//! narrowing is the intended truncation. Keeping the three casts here is
//! what keeps the workspace `cast_possible_truncation` lint at one
//! `#[allow]` per conversion.

/// `x` truncated toward zero, saturating (NaN → 0).
pub(crate) fn to_u64(x: f64) -> u64 {
    // f64 -> u64 saturates; std has no fallible conversion (module doc).
    #[allow(clippy::cast_possible_truncation)]
    let v = x as u64;
    v
}

/// `x` truncated toward zero, saturating (NaN → 0).
pub(crate) fn to_i64(x: f64) -> i64 {
    // f64 -> i64 saturates; std has no fallible conversion (module doc).
    #[allow(clippy::cast_possible_truncation)]
    let v = x as i64;
    v
}

/// `x` truncated toward zero, saturating (NaN → 0).
pub(crate) fn to_usize(x: f64) -> usize {
    // f64 -> usize saturates; std has no fallible conversion (module doc).
    #[allow(clippy::cast_possible_truncation)]
    let v = x as usize;
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncates_toward_zero_and_saturates() {
        assert_eq!(to_u64(21.9), 21);
        assert_eq!(to_u64(-1.0), 0);
        assert_eq!(to_u64(f64::NAN), 0);
        assert_eq!(to_u64(f64::INFINITY), u64::MAX);
        assert_eq!(to_i64(-2.5), -2);
        assert_eq!(to_i64(f64::NEG_INFINITY), i64::MIN);
        assert_eq!(to_usize(3.999), 3);
        assert_eq!(to_usize(1e300), usize::MAX);
    }
}

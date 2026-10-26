//! Shortened difference-set cyclic code (273,191) (ARIB STD-B31
//! §3.15.6.9 for TMCC as (184,102); §3.16.6.7 for the seismic-warning AC
//! as (187,105)).
//!
//! g(x) = x^82 + x^77 + x^76 + x^71 + x^67 + x^66 + x^56 + x^52 + x^48 +
//! x^40 + x^36 + x^34 + x^24 + x^22 + x^18 + x^10 + x^4 + 1.
//!
//! Bit order (see SPEC-NOTES): the first transmitted information bit (B20
//! for TMCC, B17 for AC) is the highest-degree coefficient; the code is
//! systematic, `c(x) = m(x)·x^82 + (m(x)·x^82 mod g(x))`, and the 82
//! parity bits follow the information bits highest degree first — the
//! usual serial shift-register encoder (as drawn for the CRC of
//! Fig. 3-35).
//!
//! Decoding: syndrome check, and one-step majority-logic decoding with the
//! 17 parity checks orthogonal on each bit given by a perfect difference
//! set of the projective plane PG(2,16) (273 = 16²+16+1); the set is
//! derived at run time (Singer construction) and verified against g(x).

use std::sync::OnceLock;

/// Code length of the mother code.
pub const N: usize = 273;
/// Parity bits.
pub const PARITY: usize = 82;
/// Exponents of g(x).
pub const G_EXPONENTS: [usize; 18] = [
    82, 77, 76, 71, 67, 66, 56, 52, 48, 40, 36, 34, 24, 22, 18, 10, 4, 0,
];

fn g_low() -> u128 {
    G_EXPONENTS
        .iter()
        .filter(|&&e| e < PARITY)
        .fold(0u128, |a, &e| a | (1u128 << e))
}

const MASK: u128 = (1u128 << PARITY) - 1;

/// Parity bits (82, highest degree first) for information bits `info`
/// (first = highest degree).
pub fn parity(info: &[u8]) -> [u8; PARITY] {
    let g = g_low();
    let mut r: u128 = 0;
    for &b in info {
        let fb = u128::from(b & 1) ^ (r >> (PARITY - 1)) & 1;
        r = (r << 1) & MASK;
        if fb == 1 {
            r ^= g;
        }
    }
    std::array::from_fn(|i| ((r >> (PARITY - 1 - i)) & 1) as u8)
}

/// True if `word` (information followed by parity, first = highest
/// degree) is a codeword.
pub fn syndrome_ok(word: &[u8]) -> bool {
    assert!(word.len() > PARITY);
    let (info, par) = word.split_at(word.len() - PARITY);
    parity(info).as_slice() == par
}

/// Result of decoding a shortened codeword.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParityStatus {
    /// Syndrome zero as received.
    Ok,
    /// Majority-logic decoding corrected this many bits; syndrome now zero.
    Corrected(usize),
    /// Uncorrectable (syndrome still non-zero).
    Failed,
}

impl ParityStatus {
    /// True unless [`ParityStatus::Failed`].
    pub fn is_ok(self) -> bool {
        !matches!(self, ParityStatus::Failed)
    }
}

// --- GF(2^12) for the Singer difference set ---------------------------

fn gf_mul(a: u16, b: u16, poly: u16) -> u16 {
    let (mut a, mut b, mut r) = (u32::from(a), u32::from(b), 0u32);
    while b != 0 {
        if b & 1 != 0 {
            r ^= a;
        }
        b >>= 1;
        a <<= 1;
        if a & 0x1000 != 0 {
            a ^= 0x1000 | u32::from(poly);
        }
    }
    r as u16
}

fn gf_pow(a: u16, mut e: u32, poly: u16) -> u16 {
    let (mut base, mut r) = (a, 1u16);
    while e > 0 {
        if e & 1 != 0 {
            r = gf_mul(r, base, poly);
        }
        base = gf_mul(base, base, poly);
        e >>= 1;
    }
    r
}

/// A primitive polynomial of degree 12 (low 12 bits; x^12 implicit),
/// found by search: x is primitive iff x^(4095/p) ≠ 1 for p ∈ {3,5,7,13}.
fn primitive_poly() -> u16 {
    (1u16..0x1000)
        .step_by(2)
        .find(|&p| {
            gf_pow(2, 4095, p) == 1 && [3u32, 5, 7, 13].iter().all(|q| gf_pow(2, 4095 / q, p) != 1)
        })
        .expect("primitive polynomial")
}

fn is_dual_codeword(set: &[usize]) -> bool {
    // v ⟂ every shift of g ⇔ Σ_d g_{(d−s) mod N} = 0 (mod 2) for all s.
    let mut gv = [0u8; N];
    for &e in &G_EXPONENTS {
        gv[e] = 1;
    }
    (0..N).all(|s| set.iter().map(|&d| gv[(d + N - s) % N]).sum::<u8>() % 2 == 0)
}

/// The perfect difference set D (17 elements mod 273) whose indicator
/// polynomial is a codeword of the dual code (parity check).
pub fn difference_set() -> &'static [usize] {
    static SET: OnceLock<Vec<usize>> = OnceLock::new();
    SET.get_or_init(|| {
        let poly = primitive_poly();
        let alpha = 2u16;
        // Tr_{GF(4096)/GF(16)}(x) = x + x^16 + x^256.
        let base: Vec<usize> = (0..N)
            .filter(|&i| {
                let x = gf_pow(alpha, i as u32, poly);
                (x ^ gf_pow(x, 16, poly) ^ gf_pow(x, 256, poly)) == 0
            })
            .collect();
        assert_eq!(base.len(), 17);
        for t in 1..N {
            if gcd(t, N) != 1 {
                continue;
            }
            let mut d: Vec<usize> = base.iter().map(|&x| (x * t) % N).collect();
            d.sort_unstable();
            if is_dual_codeword(&d) {
                return d;
            }
        }
        panic!("no difference set matches g(x)");
    })
}

fn gcd(a: usize, b: usize) -> usize {
    if b == 0 { a } else { gcd(b, a % b) }
}

/// Decode a shortened codeword in place (information + 82 parity bits,
/// first = highest degree) by iterative one-step majority logic over the
/// 17 orthogonal checks per bit.  Corrects up to 8 errors.
pub fn decode(word: &mut [u8]) -> ParityStatus {
    if syndrome_ok(word) {
        return ParityStatus::Ok;
    }
    let len = word.len();
    assert!(len <= N);
    let d = difference_set();
    // Degree of transmitted index t is len−1−t; shortened degrees ≥ len are 0.
    let mut c = [0u8; N];
    for (t, &b) in word.iter().enumerate() {
        c[len - 1 - t] = b & 1;
    }
    let mut flipped = vec![false; len];
    for _ in 0..4 {
        let mut changed = false;
        let snapshot = c;
        for (deg, cd) in c.iter_mut().enumerate().take(len) {
            // Checks containing `deg`: shifts s = deg − d_m.
            let mut votes = 0;
            for &dm in d {
                let s = (deg + N - dm) % N;
                let sum: u8 = d
                    .iter()
                    .map(|&dj| snapshot[(dj + s) % N])
                    .fold(0, |a, b| a ^ b);
                votes += sum as usize;
            }
            if votes > d.len() / 2 {
                *cd ^= 1;
                let t = len - 1 - deg;
                flipped[t] = !flipped[t];
                changed = true;
            }
        }
        if !changed {
            break;
        }
        for (t, b) in word.iter_mut().enumerate() {
            *b = c[len - 1 - t];
        }
        if syndrome_ok(word) {
            return ParityStatus::Corrected(flipped.iter().filter(|&&f| f).count());
        }
    }
    ParityStatus::Failed
}

#[cfg(test)]
mod tests {
    use super::*;

    /// g(x) divides x^273 + 1 (cyclic code of length 273).
    #[test]
    fn spec_3_15_6_9_generator_divides() {
        // Long division of x^273 + 1 by g(x) over GF(2).
        let mut r = vec![0u8; N + 1];
        r[N] = 1;
        r[0] = 1;
        for deg in (PARITY..=N).rev() {
            if r[deg] == 1 {
                for &e in &G_EXPONENTS {
                    r[deg - PARITY + e] ^= 1;
                }
            }
        }
        assert!(r.iter().all(|&b| b == 0));
    }

    #[test]
    fn difference_set_is_perfect() {
        let d = difference_set();
        let mut seen = vec![0; N];
        for &a in d {
            for &b in d {
                if a != b {
                    seen[(a + N - b) % N] += 1;
                }
            }
        }
        assert_eq!(seen[0], 0);
        assert!(seen[1..].iter().all(|&c| c == 1));
    }
}

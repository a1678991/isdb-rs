//! Shortened Reed–Solomon codes of the RS(255,239) mother code over
//! GF(2⁸), as every ISDB system uses them:
//!
//! - RS(204,188): the TS outer code of ISDB-T (ARIB STD-B31 v2.2 §3.3) and
//!   of ISDB-S (ITU-R BO.1408-1 Annex 1 §2; MIC Ordinance No. 87 of 2011,
//!   annexed table 49 item 1);
//! - RS(64,48): the TMCC outer code of ISDB-S (BO.1408-1 Appendix 1 Table 3
//!   item 10; annexed table 49 item 2: "入力データバイトの前に１９１バイトの
//!   「００ｈ」を付加し、符号化後に先頭１９１バイトを除去").
//!
//! Field: GF(2⁸) with p(x) = x⁸ + x⁴ + x³ + x² + 1 (0x11D); generator
//! g(x) = (x + λ⁰)(x + λ¹)…(x + λ¹⁵), λ = 02h. Both texts give the same
//! polynomials (BO.1408-1 §2 writes the factors as (x + λⁱ), B31 §3.3 as
//! (x − λⁱ): the same in characteristic 2).
//!
//! Byte ↔ polynomial mapping: the first byte of an n-byte word is the
//! coefficient of xⁿ⁻¹ (highest degree) and the 16 parity bytes are the
//! coefficients of x¹⁵…x⁰. This is the only mapping under which prepending
//! 255 − n zero bytes "at the beginning" yields a (255,239) codeword with
//! the parity at the end (`isdbt-fec` SPEC-NOTES).
//!
//! The decoder is a textbook errors-only bounded-distance decoder
//! (syndromes, Berlekamp–Massey, Chien search restricted to the n
//! transmitted positions, Forney), correcting up to 8 byte errors.

/// Number of RS parity bytes (16 for every ISDB code).
pub const PARITY: usize = 16;
/// Correctable byte errors, t = 8.
pub const T: usize = PARITY / 2;
/// Primitive polynomial p(x) = x⁸+x⁴+x³+x²+1 including the x⁸ term.
pub const PRIMITIVE_POLY: u16 = 0x11D;
/// Primitive element λ = 02h.
pub const ALPHA: u8 = 0x02;
/// Length of the mother code.
pub const MOTHER_N: usize = 255;

/// More than t = 8 byte errors in a word.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("RS({n},{k}): uncorrectable", k = .n - PARITY)]
pub struct Uncorrectable {
    /// Codeword length of the code that failed.
    pub n: usize,
}

/// GF(2⁸) arithmetic tables for p(x) = 0x11D, λ = 2.
struct Gf {
    exp: [u8; 512],
    log: [u16; 256],
}

impl Gf {
    const fn new() -> Self {
        let mut exp = [0u8; 512];
        let mut log = [0u16; 256];
        let mut x: u16 = 1;
        let mut i = 0;
        while i < 255 {
            exp[i] = x as u8;
            exp[i + 255] = x as u8;
            log[x as usize] = i as u16;
            x <<= 1; // multiply by λ = 0x02
            if x & 0x100 != 0 {
                x ^= PRIMITIVE_POLY;
            }
            i += 1;
        }
        exp[510] = exp[0];
        exp[511] = exp[1];
        Self { exp, log }
    }
    #[inline]
    fn mul(&self, a: u8, b: u8) -> u8 {
        if a == 0 || b == 0 {
            0
        } else {
            self.exp[self.log[a as usize] as usize + self.log[b as usize] as usize]
        }
    }
    #[inline]
    fn div(&self, a: u8, b: u8) -> u8 {
        debug_assert!(b != 0);
        if a == 0 {
            0
        } else {
            self.exp[(self.log[a as usize] as usize + 255 - self.log[b as usize] as usize) % 255]
        }
    }
    /// λ^e for any integer e.
    #[inline]
    fn pow_alpha(&self, e: i64) -> u8 {
        self.exp[e.rem_euclid(255) as usize]
    }
}

static GF: Gf = Gf::new();

/// Field multiplication in GF(2⁸)/p(x).
pub fn gf_mul(a: u8, b: u8) -> u8 {
    GF.mul(a, b)
}

/// λ^e in GF(2⁸) (λ = 02h).
pub fn gf_alpha_pow(e: u32) -> u8 {
    GF.pow_alpha(i64::from(e))
}

/// Coefficients of g(x) = ∏_{i=0}^{15} (x + λ^i), highest degree first:
/// `[1, g15, …, g0]` (17 coefficients, monic).
pub fn generator_poly() -> [u8; PARITY + 1] {
    // Built in ascending-degree order then reversed.
    let mut g = [0u8; PARITY + 1];
    g[0] = 1; // ascending: g[k] = coeff of x^k
    for i in 0..PARITY {
        let root = GF.pow_alpha(i as i64);
        // multiply by (x + root); degree is i.
        let mut next = [0u8; PARITY + 1];
        for k in 0..=i {
            next[k + 1] ^= g[k];
            next[k] ^= GF.mul(g[k], root);
        }
        g = next;
    }
    let mut out = [0u8; PARITY + 1];
    for k in 0..=PARITY {
        out[k] = g[PARITY - k];
    }
    out
}

/// Generator coefficients without the leading 1, highest degree first.
fn gen_tail() -> &'static [u8; PARITY] {
    use std::sync::OnceLock;
    static G: OnceLock<[u8; PARITY]> = OnceLock::new();
    G.get_or_init(|| {
        let g = generator_poly();
        let mut t = [0u8; PARITY];
        t.copy_from_slice(&g[1..]);
        t
    })
}

/// A shortened RS(n, n − 16) code of the RS(255,239) mother code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rs {
    n: usize,
}

/// RS(204,188), the TS outer code (ISDB-T, ISDB-S).
pub const RS_204_188: Rs = Rs { n: 204 };
/// RS(64,48), the ISDB-S TMCC outer code.
pub const RS_64_48: Rs = Rs { n: 64 };

impl Rs {
    /// The code of codeword length `n` (17..=255).
    pub const fn new(n: usize) -> Self {
        assert!(n > PARITY && n <= MOTHER_N);
        Self { n }
    }

    /// Codeword length n.
    pub const fn n(&self) -> usize {
        self.n
    }

    /// Information length k = n − 16.
    pub const fn k(&self) -> usize {
        self.n - PARITY
    }

    /// Zero bytes conceptually prepended to shorten RS(255,239).
    pub const fn shortening(&self) -> usize {
        MOTHER_N - self.n
    }

    /// The 16 parity bytes r(x) = d(x)·x¹⁶ mod g(x) of the `k` data bytes.
    pub fn parity(&self, data: &[u8]) -> [u8; PARITY] {
        assert_eq!(data.len(), self.k(), "RS({},{}) data", self.n, self.k());
        let g = gen_tail();
        let mut reg = [0u8; PARITY];
        for &b in data {
            let fb = b ^ reg[0];
            reg.copy_within(1.., 0);
            reg[PARITY - 1] = 0;
            if fb != 0 {
                for (r, &gk) in reg.iter_mut().zip(g.iter()) {
                    *r ^= GF.mul(fb, gk);
                }
            }
        }
        reg
    }

    /// Systematic encoding into `word` (n bytes): its first k bytes are the
    /// data, the last 16 are overwritten with the parity.
    pub fn encode_in_place(&self, word: &mut [u8]) {
        assert_eq!(word.len(), self.n, "RS({},{}) word", self.n, self.k());
        let k = self.k();
        let p = self.parity(&word[..k]);
        word[k..].copy_from_slice(&p);
    }

    /// The 16 syndromes S_j = c(λ^j), j = 0..15, of an n-byte word (the
    /// roots of g(x)). All zero ⇔ the word is a codeword.
    pub fn syndromes(&self, word: &[u8]) -> [u8; PARITY] {
        assert_eq!(word.len(), self.n, "RS({},{}) word", self.n, self.k());
        let mut s = [0u8; PARITY];
        for (j, sj) in s.iter_mut().enumerate() {
            let a = GF.pow_alpha(j as i64);
            let mut acc = 0u8;
            for &b in word {
                acc = GF.mul(acc, a) ^ b;
            }
            *sj = acc;
        }
        s
    }

    /// Decode an n-byte word in place (t = 8). On success the word is a
    /// codeword and the number of corrected bytes is returned; on failure
    /// (more than 8 errors detected) the word is left unchanged.
    pub fn decode(&self, word: &mut [u8]) -> Result<usize, Uncorrectable> {
        let n = self.n;
        let fail = Uncorrectable { n };
        let s = self.syndromes(word);
        if s.iter().all(|&x| x == 0) {
            return Ok(0);
        }
        // Berlekamp–Massey: error locator Λ(x), ascending coefficients.
        let mut lambda = [0u8; PARITY + 1];
        let mut prev = [0u8; PARITY + 1];
        lambda[0] = 1;
        prev[0] = 1;
        let mut l = 0usize;
        let mut m = 1usize;
        let mut b = 1u8;
        for i in 0..PARITY {
            let mut d = s[i];
            for j in 1..=l {
                d ^= GF.mul(lambda[j], s[i - j]);
            }
            if d == 0 {
                m += 1;
            } else if 2 * l <= i {
                let t = lambda;
                let coef = GF.div(d, b);
                for j in m..=PARITY {
                    lambda[j] ^= GF.mul(coef, prev[j - m]);
                }
                l = i + 1 - l;
                prev = t;
                b = d;
                m = 1;
            } else {
                let coef = GF.div(d, b);
                for j in m..=PARITY {
                    lambda[j] ^= GF.mul(coef, prev[j - m]);
                }
                m += 1;
            }
        }
        let deg = (0..=PARITY).rev().find(|&i| lambda[i] != 0).unwrap_or(0);
        if l > T || deg != l {
            return Err(fail);
        }
        // Ω(x) = S(x)·Λ(x) mod x^16, S(x) = Σ S_j x^j.
        let mut omega = [0u8; PARITY];
        for i in 0..PARITY {
            let mut acc = 0u8;
            for j in 0..=i.min(l) {
                acc ^= GF.mul(lambda[j], s[i - j]);
            }
            omega[i] = acc;
        }
        // Chien search over the n transmitted positions. Byte index p has
        // degree e = n − 1 − p and locator X = λ^e; Λ(X⁻¹) = 0 at errors.
        let mut positions = [0usize; T];
        let mut values = [0u8; T];
        let mut found = 0;
        for p in 0..n {
            let e = (n - 1 - p) as i64;
            let xinv = GF.pow_alpha(-e);
            let mut acc = 0u8;
            let mut xp = 1u8;
            for &c in lambda.iter().take(l + 1) {
                acc ^= GF.mul(c, xp);
                xp = GF.mul(xp, xinv);
            }
            if acc == 0 {
                if found == T {
                    return Err(fail);
                }
                // Forney with first consecutive root λ⁰ (b = 0):
                // e = X·Ω(X⁻¹)/Λ'(X⁻¹).
                let mut om = 0u8;
                let mut xp = 1u8;
                for &c in &omega {
                    om ^= GF.mul(c, xp);
                    xp = GF.mul(xp, xinv);
                }
                // Formal derivative: odd-degree terms only.
                let mut dl = 0u8;
                let mut i = 1;
                while i <= l {
                    dl ^= GF.mul(lambda[i], GF.pow_alpha(-e * (i as i64 - 1)));
                    i += 2;
                }
                if dl == 0 {
                    return Err(fail);
                }
                let x = GF.pow_alpha(e);
                positions[found] = p;
                values[found] = GF.mul(x, GF.div(om, dl));
                found += 1;
            }
        }
        if found != l {
            // Roots outside the transmitted positions (in the shortened
            // zero region) or repeated: more than t errors.
            return Err(fail);
        }
        let mut fixed = word.to_vec();
        for k in 0..found {
            fixed[positions[k]] ^= values[k];
        }
        if self.syndromes(&fixed).iter().any(|&x| x != 0) {
            return Err(fail);
        }
        word.copy_from_slice(&fixed);
        Ok(found)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::XorShift;

    /// λ = 2 generates GF(2⁸)* under p(x) = 0x11D (order 255).
    #[test]
    fn spec_primitive_element() {
        let mut seen = [false; 256];
        for e in 0..255 {
            let v = gf_alpha_pow(e);
            assert!(!seen[v as usize], "λ^{e} repeats");
            seen[v as usize] = true;
        }
        assert_eq!(gf_alpha_pow(8), 0x1D, "λ⁸ = λ⁴+λ³+λ²+1");
        assert_eq!(gf_alpha_pow(255), 1);
    }

    /// BO.1408-1 §2 / B31 §3.3: g(x) has exactly the roots λ⁰..λ¹⁵.
    #[test]
    fn spec_generator_roots() {
        let g = generator_poly();
        assert_eq!(g[0], 1);
        let eval = |x: u8| g.iter().fold(0u8, |acc, &c| gf_mul(acc, x) ^ c);
        for i in 0..255u32 {
            let v = eval(gf_alpha_pow(i));
            assert_eq!(v == 0, i < 16, "λ^{i}");
        }
        assert_ne!(eval(0), 0);
    }

    /// Shortened encoding equals the (255,239) systematic encoding of the
    /// zero-prefixed data with the prefix removed, for both ISDB lengths.
    #[test]
    fn spec_shortening_equivalence() {
        let mut rng = XorShift::new(7);
        let g = generator_poly();
        for rs in [RS_204_188, RS_64_48] {
            assert_eq!(rs.shortening(), if rs.n() == 204 { 51 } else { 191 });
            for _ in 0..20 {
                let mut c = vec![0u8; rs.n()];
                rng.fill(&mut c[..rs.k()]);
                let d = c[..rs.k()].to_vec();
                rs.encode_in_place(&mut c);
                let mut full = vec![0u8; rs.shortening()];
                full.extend_from_slice(&c);
                assert_eq!(full.len(), 255);
                let mut rem = full.clone();
                for i in 0..(255 - PARITY) {
                    let f = rem[i];
                    if f != 0 {
                        for k in 0..=PARITY {
                            rem[i + k] ^= gf_mul(f, g[k]);
                        }
                    }
                }
                assert!(rem.iter().all(|&x| x == 0));
                assert_eq!(&c[..rs.k()], &d[..]);
            }
        }
    }

    /// ≤ 8 random byte errors are corrected in both codes.
    #[test]
    fn spec_correct_up_to_8() {
        let mut rng = XorShift::new(1);
        for rs in [RS_204_188, RS_64_48] {
            for nerr in 0..=T {
                for _ in 0..30 {
                    let mut c = vec![0u8; rs.n()];
                    rng.fill(&mut c[..rs.k()]);
                    rs.encode_in_place(&mut c);
                    assert!(rs.syndromes(&c).iter().all(|&x| x == 0));
                    let mut r = c.clone();
                    for p in rng.distinct(nerr, rs.n()) {
                        r[p] ^= (rng.next() as u8).max(1);
                    }
                    assert_eq!(rs.decode(&mut r), Ok(nerr));
                    assert_eq!(r, c);
                }
            }
        }
    }

    /// Beyond t = 8 a failed decode leaves the word unchanged.
    #[test]
    fn spec_failure_beyond_8() {
        let mut rng = XorShift::new(99);
        for rs in [RS_204_188, RS_64_48] {
            for i in 0..200 {
                let mut c = vec![0u8; rs.n()];
                rng.fill(&mut c[..rs.k()]);
                rs.encode_in_place(&mut c);
                let mut r = c.clone();
                for p in rng.distinct(9 + i % 12, rs.n()) {
                    r[p] ^= (rng.next() as u8).max(1);
                }
                let before = r.clone();
                match rs.decode(&mut r) {
                    Err(e) => {
                        assert_eq!(e.n, rs.n());
                        assert_eq!(r, before);
                    }
                    Ok(_) => {
                        assert!(rs.syndromes(&r).iter().all(|&x| x == 0));
                        assert_ne!(r, c);
                    }
                }
            }
        }
    }
}

//! Quasi-cyclic irregular-repeat-accumulate LDPC codes defined by an
//! address table, as in ARIB STD-B44 v2.1 §3.4 (2) (the inner code of
//! ISDB-S3, n = 44 880) and §3.5 (its shortened rate-61/120 code for the
//! TMCC).
//!
//! Encoding (§3.4 (2)): parity p₀…p_{P−1} (P = n − k) start at 0; each
//! information bit i_m, m = 374·g + j, is added to parity positions
//! (x + j·q) mod P for every x in row g + 1 of the table; then
//! p_n ← p_n ⊕ p_{n−1} for n = 1 … P − 1 (the accumulator). q = P / 374.
//! The codeword is i₀ … i_{k−1} followed by p₀ … p_{P−1}.
//!
//! So parity check c (0 ≤ c < P) covers the information bits whose
//! addresses hit c, plus p_c and p_{c−1} (none for c = 0): the
//! accumulator's staircase.
//!
//! Decoding: layered normalised min-sum over the P checks in order,
//! scalar `f32`. Soft input is the LLR ln P(b=0)/P(b=1) per code bit, in
//! codeword order; a known bit (shortening, §3.5) is a large LLR.

use std::sync::Arc;

/// Information bits per address-table row.
pub const GROUP: usize = 374;

/// One code: the table expanded into check-node adjacency.
#[derive(Debug, Clone)]
pub struct Code {
    n: usize,
    k: usize,
    /// For each check c: the variable indices (information bits, then the
    /// staircase parity bits), into a flat array.
    rows: Arc<[u32]>,
    row_start: Arc<[u32]>,
    /// For each information bit m: its parity positions (encoder).
    info_adj: Arc<[u32]>,
    info_start: Arc<[u32]>,
}

/// The decoder found no codeword within its iteration limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("LDPC: no codeword after {iterations} iterations ({unsatisfied} checks unsatisfied)")]
pub struct NotConverged {
    /// Iterations run.
    pub iterations: usize,
    /// Checks still unsatisfied at the end.
    pub unsatisfied: usize,
}

/// What a successful decode took.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Decoded {
    /// Iterations until every check was satisfied (0: the hard decision
    /// of the input was already a codeword).
    pub iterations: usize,
}

impl Code {
    /// The code of length `n` from `table` (rows of parity positions, one
    /// row per group of [`GROUP`] information bits): k = 374 · rows.
    pub fn new(n: usize, table: &[&[u16]]) -> Self {
        let k = GROUP * table.len();
        assert!(k < n, "k {k} ≥ n {n}");
        let p = n - k;
        assert_eq!(p % GROUP, 0, "n − k = {p} not a multiple of {GROUP}");
        let q = p / GROUP;
        let mut info_adj = Vec::new();
        let mut info_start = Vec::with_capacity(k + 1);
        let mut per_check: Vec<Vec<u32>> = vec![Vec::new(); p];
        for (g, row) in table.iter().enumerate() {
            for j in 0..GROUP {
                info_start.push(info_adj.len() as u32);
                let m = (g * GROUP + j) as u32;
                for &x in *row {
                    let c = (x as usize + j * q) % p;
                    info_adj.push(c as u32);
                    per_check[c].push(m);
                }
            }
        }
        info_start.push(info_adj.len() as u32);
        let mut rows = Vec::new();
        let mut row_start = Vec::with_capacity(p + 1);
        for (c, v) in per_check.iter().enumerate() {
            row_start.push(rows.len() as u32);
            rows.extend_from_slice(v);
            if c > 0 {
                rows.push((k + c - 1) as u32);
            }
            rows.push((k + c) as u32);
        }
        row_start.push(rows.len() as u32);
        Self {
            n,
            k,
            rows: rows.into(),
            row_start: row_start.into(),
            info_adj: info_adj.into(),
            info_start: info_start.into(),
        }
    }

    /// Code length.
    pub fn n(&self) -> usize {
        self.n
    }
    /// Information bits.
    pub fn k(&self) -> usize {
        self.k
    }

    /// Parity of the `k` information bits (0/1), `n − k` bits.
    pub fn parity(&self, info: &[u8]) -> Vec<u8> {
        assert_eq!(info.len(), self.k);
        let mut p = vec![0u8; self.n - self.k];
        for (m, &b) in info.iter().enumerate() {
            if b & 1 == 1 {
                let (s, e) = (self.info_start[m] as usize, self.info_start[m + 1] as usize);
                for &c in &self.info_adj[s..e] {
                    p[c as usize] ^= 1;
                }
            }
        }
        for i in 1..p.len() {
            p[i] ^= p[i - 1];
        }
        p
    }

    /// The codeword: `info` followed by its parity.
    pub fn encode(&self, info: &[u8]) -> Vec<u8> {
        let mut c = info.to_vec();
        c.extend(self.parity(info));
        c
    }

    /// Number of unsatisfied checks of the hard word `bits` (0/1, length n).
    pub fn syndrome_weight(&self, bits: &[u8]) -> usize {
        (0..self.n - self.k)
            .filter(|&c| {
                let (s, e) = (self.row_start[c] as usize, self.row_start[c + 1] as usize);
                self.rows[s..e]
                    .iter()
                    .fold(0u8, |a, &v| a ^ bits[v as usize])
                    & 1
                    == 1
            })
            .count()
    }

    /// Layered normalised min-sum: `llr` (length n, updated in place to the
    /// a-posteriori LLRs) → `bits` (hard decisions). `scale` multiplies the
    /// check-to-variable magnitudes (0.75–0.8 is usual).
    pub fn decode(
        &self,
        llr: &mut [f32],
        bits: &mut Vec<u8>,
        max_iter: usize,
        scale: f32,
    ) -> Result<Decoded, NotConverged> {
        assert_eq!(llr.len(), self.n);
        let hard = |llr: &[f32], bits: &mut Vec<u8>| {
            bits.clear();
            bits.extend(llr.iter().map(|&l| u8::from(l < 0.0)));
        };
        hard(llr, bits);
        if self.syndrome_weight(bits) == 0 {
            return Ok(Decoded { iterations: 0 });
        }
        // check-to-variable messages, one per edge
        let mut r = vec![0f32; self.rows.len()];
        let mut q = Vec::<f32>::with_capacity(64);
        for it in 1..=max_iter {
            for c in 0..self.n - self.k {
                let (s, e) = (self.row_start[c] as usize, self.row_start[c + 1] as usize);
                let vars = &self.rows[s..e];
                q.clear();
                let (mut min1, mut min2, mut imin, mut sign) = (f32::MAX, f32::MAX, 0usize, false);
                for (i, &v) in vars.iter().enumerate() {
                    let t = llr[v as usize] - r[s + i];
                    q.push(t);
                    let a = t.abs();
                    sign ^= t < 0.0;
                    if a < min1 {
                        min2 = min1;
                        min1 = a;
                        imin = i;
                    } else if a < min2 {
                        min2 = a;
                    }
                }
                for (i, &v) in vars.iter().enumerate() {
                    let mag = scale * if i == imin { min2 } else { min1 };
                    let sg = sign ^ (q[i] < 0.0);
                    let m = if sg { -mag } else { mag };
                    r[s + i] = m;
                    llr[v as usize] = q[i] + m;
                }
            }
            hard(llr, bits);
            let u = self.syndrome_weight(bits);
            if u == 0 {
                return Ok(Decoded { iterations: it });
            }
            if it == max_iter {
                return Err(NotConverged {
                    iterations: it,
                    unsatisfied: u,
                });
            }
        }
        unreachable!()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::XorShift;

    /// A small table of the same form (n = 374·6, k = 374·2, q = 4).
    fn toy() -> Code {
        Code::new(374 * 6, &[&[3, 300, 1000, 2000], &[7, 900, 1500]])
    }

    #[test]
    fn codewords_satisfy_every_check() {
        let code = toy();
        let mut rng = XorShift::new(3);
        for _ in 0..4 {
            let info: Vec<u8> = (0..code.k()).map(|_| (rng.next() & 1) as u8).collect();
            let c = code.encode(&info);
            assert_eq!(code.syndrome_weight(&c), 0);
        }
    }

    #[test]
    fn decodes_noisy_bpsk() {
        let code = toy();
        let mut rng = XorShift::new(5);
        let info: Vec<u8> = (0..code.k()).map(|_| (rng.next() & 1) as u8).collect();
        let c = code.encode(&info);
        // BPSK at Es/N0 ≈ 4 dB: σ² = 0.2; LLR = 2y/σ²
        let sigma = 0.2f64.sqrt();
        let mut gauss = || {
            let u1 = (rng.next() >> 11) as f64 / (1u64 << 53) as f64 + 1e-300;
            let u2 = (rng.next() >> 11) as f64 / (1u64 << 53) as f64;
            (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos()
        };
        let mut llr: Vec<f32> = c
            .iter()
            .map(|&b| {
                let y = if b == 0 { 1.0 } else { -1.0 } + sigma * gauss();
                (2.0 * y / (sigma * sigma)) as f32
            })
            .collect();
        let mut bits = Vec::new();
        let d = code.decode(&mut llr, &mut bits, 50, 0.8).expect("decodes");
        assert!(d.iterations >= 1);
        assert_eq!(bits, c);
    }
}

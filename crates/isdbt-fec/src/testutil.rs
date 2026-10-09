//! Deterministic test randomness (xorshift64*), no external dependency.

pub struct XorShift(u64);

impl XorShift {
    pub fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }
    pub fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    pub fn fill(&mut self, buf: &mut [u8]) {
        for b in buf {
            *b = self.next() as u8;
        }
    }
    pub fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    /// `k` distinct indices in 0..n.
    pub fn distinct(&mut self, k: usize, n: usize) -> Vec<usize> {
        let mut v: Vec<usize> = (0..n).collect();
        for i in 0..k {
            let j = i + self.below(n - i);
            v.swap(i, j);
        }
        v.truncate(k);
        v
    }
    /// Standard normal (Box–Muller).
    pub fn gauss(&mut self) -> f64 {
        let u1 = ((self.next() >> 11) as f64 + 1.0) / (1u64 << 53) as f64;
        let u2 = (self.next() >> 11) as f64 / (1u64 << 53) as f64;
        (-2.0 * u1.ln()).sqrt() * (2.0 * std::f64::consts::PI * u2).cos()
    }
}

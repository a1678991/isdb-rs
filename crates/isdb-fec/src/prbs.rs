//! Pseudo-random binary sequences for energy dispersal.
//!
//! - [`Prbs15`]: g(x) = X¹⁵ + X¹⁴ + 1, initial state `100101010000000`.
//!   The energy dispersal of the ISDB-T layers (ARIB STD-B31 v2.2 §3.5,
//!   Fig. 3-8, reset every OFDM frame) and of the ISDB-S main signal and
//!   TMCC (ITU-R BO.1408-1 Annex 1 §4, Fig. 5 and §6.4; MIC Ordinance
//!   No. 87 of 2011, annexed table 48 別記 1 and table 50 別記 1, reset every
//!   superframe).
//! - [`Prbs9`]: g(x) = X⁹ + X⁴ + 1, initial state `111101101`, the ISDB-S
//!   phase-reference burst (annexed table 51, restarted every frame and
//!   clocked during the burst symbols only).
//!
//! Both generator figures have the same structure: stages numbered from 1,
//! shifting from stage 1 towards the last; the outputs of the two tapped
//! stages are XORed, the sum is fed back into stage 1, and the **output**
//! is taken from that feedback node. The initial string assigns its
//! leftmost character to stage 1 (`isdbt-fec` SPEC-NOTES, "energy").

/// A Fibonacci LFSR of the figures' form, `N` stages, taps at stage `A`
/// and stage `N` (stage i in bit i − 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Lfsr<const N: u32, const A: u32> {
    reg: u16,
    init: u16,
}

impl<const N: u32, const A: u32> Lfsr<N, A> {
    const MASK: u16 = ((1u32 << N) - 1) as u16;

    const fn from_str(init: &str) -> Self {
        let b = init.as_bytes();
        assert!(b.len() == N as usize);
        let mut reg = 0u16;
        let mut i = 0;
        while i < b.len() {
            if b[i] == b'1' {
                reg |= 1 << i;
            }
            i += 1;
        }
        Self { reg, init: reg }
    }

    #[inline]
    fn next_bit(&mut self) -> u8 {
        let fb = ((self.reg >> (A - 1)) ^ (self.reg >> (N - 1))) & 1;
        self.reg = ((self.reg << 1) | fb) & Self::MASK;
        fb as u8
    }

    fn state_string(&self) -> String {
        (0..N)
            .map(|i| if self.reg >> i & 1 == 1 { '1' } else { '0' })
            .collect()
    }
}

/// Initial register contents of [`Prbs15`], stage 1 first.
pub const PRBS15_INITIAL_STATE: &str = "100101010000000";
/// Period of [`Prbs15`]: 2¹⁵ − 1.
pub const PRBS15_PERIOD: usize = (1 << 15) - 1;
/// Initial register contents of [`Prbs9`], stage 1 first (annexed table 51:
/// "初期値：１１１１０１１０１").
pub const PRBS9_INITIAL_STATE: &str = "111101101";
/// Period of [`Prbs9`]: 2⁹ − 1.
pub const PRBS9_PERIOD: usize = (1 << 9) - 1;

/// The X¹⁵ + X¹⁴ + 1 generator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Prbs15(Lfsr<15, 14>);

impl Default for Prbs15 {
    fn default() -> Self {
        Self::new()
    }
}

impl Prbs15 {
    /// Register loaded with [`PRBS15_INITIAL_STATE`].
    pub const fn new() -> Self {
        Self(Lfsr::from_str(PRBS15_INITIAL_STATE))
    }
    /// Re-initialise.
    pub fn reset(&mut self) {
        self.0.reg = self.0.init;
    }
    /// Register contents as the 15-character string, stage 1 first.
    pub fn state_string(&self) -> String {
        self.0.state_string()
    }
    /// One clock: output = stage 14 ⊕ stage 15, which also enters stage 1.
    #[inline]
    pub fn next_bit(&mut self) -> u8 {
        self.0.next_bit()
    }
    /// Eight clocks packed MSB first (the first PRBS bit meets the MSB of a
    /// byte in every ISDB text).
    #[inline]
    pub fn next_byte(&mut self) -> u8 {
        let mut b = 0u8;
        for _ in 0..8 {
            b = (b << 1) | self.next_bit();
        }
        b
    }
}

/// The X⁹ + X⁴ + 1 burst generator (annexed table 51).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Prbs9(Lfsr<9, 4>);

impl Default for Prbs9 {
    fn default() -> Self {
        Self::new()
    }
}

impl Prbs9 {
    /// Register loaded with [`PRBS9_INITIAL_STATE`].
    pub const fn new() -> Self {
        Self(Lfsr::from_str(PRBS9_INITIAL_STATE))
    }
    /// Re-initialise (at every frame start).
    pub fn reset(&mut self) {
        self.0.reg = self.0.init;
    }
    /// Register contents as the 9-character string, stage 1 first.
    pub fn state_string(&self) -> String {
        self.0.state_string()
    }
    /// One clock: output = stage 4 ⊕ stage 9, which also enters stage 1.
    #[inline]
    pub fn next_bit(&mut self) -> u8 {
        self.0.next_bit()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Independent formulation: stage i holds o_{t−i}, so o_t = o_{t−a} ⊕
    /// o_{t−n}, with the virtual past o_{−i} = stage i of the initial string.
    fn recurrence(init: &str, a: usize, n: usize, len: usize) -> Vec<u8> {
        let init: Vec<u8> = init.bytes().map(|c| c - b'0').collect();
        // hist[k] = o_{k−n}
        let mut hist: Vec<u8> = (0..n).map(|k| init[n - 1 - k]).collect();
        for t in 0..len {
            let v = hist[t + n - a] ^ hist[t];
            hist.push(v);
        }
        hist[n..].to_vec()
    }

    /// B31 Fig. 3-8 / BO.1408-1 Fig. 5: the first bytes from the stated
    /// state are 0x03, 0xF6 (`isdbt-fec` SPEC-NOTES "energy").
    #[test]
    fn spec_prbs15_first_bits() {
        let mut p = Prbs15::new();
        assert_eq!(p.state_string(), PRBS15_INITIAL_STATE);
        assert_eq!([p.next_byte(), p.next_byte()], [0x03, 0xF6]);
        let mut p = Prbs15::new();
        let circuit: Vec<u8> = (0..5000).map(|_| p.next_bit()).collect();
        assert_eq!(circuit, recurrence(PRBS15_INITIAL_STATE, 14, 15, 5000));
    }

    /// Annexed table 51: the burst generator from `111101101` (stages 1..9).
    /// By hand, with stage i holding o_{t−i}: o_t = o_{t−4} ⊕ o_{t−9}, so
    /// o_0 = s4 ⊕ s9 = 1 ⊕ 1 = 0, o_1 = s3 ⊕ s8 = 1 ⊕ 0 = 1, o_2 = s2 ⊕ s7 =
    /// 1 ⊕ 1 = 0, o_3 = s1 ⊕ s6 = 1 ⊕ 1 = 0, o_4 = o_0 ⊕ s5 = 0 ⊕ 0 = 0,
    /// o_5 = o_1 ⊕ s4 = 1 ⊕ 1 = 0.
    #[test]
    fn spec_prbs9_first_bits() {
        let mut p = Prbs9::new();
        assert_eq!(p.state_string(), PRBS9_INITIAL_STATE);
        let first: Vec<u8> = (0..6).map(|_| p.next_bit()).collect();
        assert_eq!(first, [0, 1, 0, 0, 0, 0]);
        let mut p = Prbs9::new();
        let circuit: Vec<u8> = (0..2000).map(|_| p.next_bit()).collect();
        assert_eq!(circuit, recurrence(PRBS9_INITIAL_STATE, 4, 9, 2000));
    }

    /// Both polynomials are primitive: the register returns to its initial
    /// state after exactly 2ⁿ − 1 clocks.
    #[test]
    fn spec_periods() {
        let s = Prbs15::new();
        let mut p = s;
        let mut n = 0;
        loop {
            p.next_bit();
            n += 1;
            if p == s {
                break;
            }
        }
        assert_eq!(n, PRBS15_PERIOD);
        let s = Prbs9::new();
        let mut p = s;
        let mut n = 0;
        loop {
            p.next_bit();
            n += 1;
            if p == s {
                break;
            }
        }
        assert_eq!(n, PRBS9_PERIOD);
    }
}

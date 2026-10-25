//! Synthetic ISDB-S symbols for tests: a frame header carrying a TMCC
//! superframe (inner-coded BPSK), the bursts, and a main signal of random
//! 8PSK points, at one sample per symbol, with an optional carrier offset,
//! phase and noise.

use crate::Cplx;
use isdb_fec::ConvEncoder;
use isdbs_core::burst::frame_bits;
use isdbs_core::frame::{
    BURST_SYMBOLS, BURSTS, FRAME_SYMBOLS, FRAMES, HEADER_BYTES, HEADER_SYMBOLS, MAIN_SYMBOLS,
    burst_symbol_position, main_symbol_position,
};
use isdbs_core::tmcc::{INFO_BYTES, TmccInfo, headers};

/// A deterministic generator.
pub struct Synth {
    rng: u64,
    superframes: usize,
    cfo: f64,
    noise: f64,
    phase: f64,
    /// The TMCC information sent.
    pub info: TmccInfo,
}

impl Synth {
    /// `superframes` superframes, seeded with `seed`; the TMCC carries a
    /// fixed BS-like assignment (TC8PSK 2/3, three TSs).
    pub fn new(superframes: usize, seed: u64) -> Self {
        let mut info = [0u8; INFO_BYTES];
        let h = "5BE1E0781E0000000000000124924924924A4924924924920082008A0097FFFFFFFFFFFFFFFFFFF89FFFFFFFFFFFFFFF";
        for (i, b) in info.iter_mut().enumerate() {
            *b = u8::from_str_radix(&h[2 * i..2 * i + 2], 16).unwrap();
        }
        Self {
            rng: seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1,
            superframes,
            cfo: 0.0,
            noise: 0.0,
            phase: 0.7,
            info: TmccInfo::decode(&info),
        }
    }
    /// Carrier offset, cycles per symbol.
    pub fn cfo(mut self, c: f64) -> Self {
        self.cfo = c;
        self
    }
    /// Noise standard deviation per component (signal amplitude 1).
    pub fn noise(mut self, n: f64) -> Self {
        self.noise = n;
        self
    }
    fn next(&mut self) -> u64 {
        let mut x = self.rng;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.rng = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn gauss(&mut self) -> f64 {
        let u1 = ((self.next() >> 11) as f64 + 1.0) / (1u64 << 53) as f64;
        let u2 = (self.next() >> 11) as f64 / (1u64 << 53) as f64;
        (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos()
    }

    /// The symbols, starting `lead` random symbols before the first frame,
    /// and the start index of each frame.
    pub fn symbols(mut self, lead: usize) -> (Vec<Cplx>, Vec<usize>) {
        let total = lead + self.superframes * FRAMES * FRAME_SYMBOLS;
        let mut z = vec![Cplx::new(0.0, 0.0); total];
        let pt = |k: u64| Cplx::from_polar(1.0, k as f64 * std::f64::consts::FRAC_PI_4);
        for v in z.iter_mut().take(lead) {
            *v = pt(self.next() & 7);
        }
        let hdr = headers(&self.info);
        let burst = frame_bits();
        let mut enc = ConvEncoder::new();
        let mut starts = Vec::new();
        for sf in 0..self.superframes {
            for (f, h) in hdr.iter().enumerate() {
                let s = lead + (sf * FRAMES + f) * FRAME_SYMBOLS;
                starts.push(s);
                // header: 96 bits → 192 BPSK symbols (X then Y)
                let mut k = 0;
                for &byte in h.iter().take(HEADER_BYTES) {
                    for i in (0..8).rev() {
                        let (x, y) = enc.push((byte >> i) & 1);
                        for b in [x, y] {
                            z[s + k] = Cplx::new(if b == 0 { 1.0 } else { -1.0 }, 0.0);
                            k += 1;
                        }
                    }
                }
                debug_assert_eq!(k, HEADER_SYMBOLS);
                for b in 0..BURSTS {
                    for j in 0..BURST_SYMBOLS {
                        let bit = burst[b * BURST_SYMBOLS + j];
                        z[s + burst_symbol_position(b, j)] =
                            Cplx::new(if bit == 0 { 1.0 } else { -1.0 }, 0.0);
                    }
                }
                for m in 0..MAIN_SYMBOLS {
                    z[s + main_symbol_position(m)] = pt(self.next() & 7);
                }
            }
        }
        for (n, v) in z.iter_mut().enumerate() {
            let rot = Cplx::from_polar(
                1.0,
                self.phase + std::f64::consts::TAU * self.cfo * n as f64,
            );
            let w = Cplx::new(self.gauss(), self.gauss()) * self.noise;
            *v = *v * rot + w;
        }
        (z, starts)
    }
}

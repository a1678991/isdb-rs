//! Inner code: punctured convolutional code (ARIB STD-B31 §3.8, Fig. 3-10,
//! Table 3-8) and its soft-decision Viterbi decoder.
//!
//! > "The inner code is a punctured convolutional code with a mother code
//! > having a constraint length k of 7, and a coding rate of 1/2. The
//! > generating polynomial of the mother code must be G1 = 171OCT and G2 =
//! > 133OCT. … Note that the puncturing pattern must be reset such that the
//! > patterns shown in Table 3-8 are initiated by frame synchronization."
//! > — §3.8
//!
//! Fig. 3-10 reading: six D stages; output X sums the data input and the
//! outputs of stages 1, 2, 3 and 6; output Y sums the input and stages 2,
//! 3, 5 and 6. With the 7-bit window (u_t, u_{t−1}, …, u_{t−6}) written MSB
//! first, these are the octal masks 171 = 1111001₂ and 133 = 1011011₂, the
//! MSB of each mask being the undelayed input.
//!
//! Soft input: f32 LLRs ln P(b=0)/P(b=1) (positive ⇒ 0); punctured
//! positions are given LLR 0.

use isdbt_core::CodeRate;

/// Constraint length k = 7 (§3.8).
pub const CONSTRAINT_LENGTH: usize = 7;
/// G1 = 171 octal, output X (§3.8).
pub const G1: u8 = 0o171;
/// G2 = 133 octal, output Y (§3.8).
pub const G2: u8 = 0o133;
/// Encoder states 2^(k−1).
pub const STATES: usize = 1 << (CONSTRAINT_LENGTH - 1);

/// Mother-code encoder of Fig. 3-10. The register holds the six previous
/// input bits, u_{t−1} in bit 5 … u_{t−6} in bit 0; it starts at zero and
/// runs continuously (it is not reset at frame boundaries, §3.8 resets only
/// the puncturing pattern).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ConvEncoder {
    state: u8,
}

#[inline]
fn parity(x: u8) -> u8 {
    (x.count_ones() & 1) as u8
}

/// (X, Y) for input `u` from register `state` (§3.8, Fig. 3-10).
#[inline]
pub fn branch_output(state: u8, u: u8) -> (u8, u8) {
    let w = (u << 6) | state;
    (parity(w & G1), parity(w & G2))
}

impl ConvEncoder {
    /// Zero register.
    pub fn new() -> Self {
        Self::default()
    }
    /// Current register contents (u_{t−1} in bit 5).
    pub fn state(&self) -> u8 {
        self.state
    }
    /// Encode one bit, returning the mother-code pair (X, Y).
    #[inline]
    pub fn push(&mut self, u: u8) -> (u8, u8) {
        let out = branch_output(self.state, u & 1);
        self.state = ((u & 1) << 5) | (self.state >> 1);
        out
    }
}

/// Puncturing pattern and transmission order of one code rate (Table 3-8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PunctureSpec {
    /// Code rate.
    pub rate: CodeRate,
    /// "X:" row of Table 3-8 (one character per input bit of the period).
    pub x: &'static str,
    /// "Y:" row of Table 3-8.
    pub y: &'static str,
    /// Transmission-signal sequence of Table 3-8 as (output, index): `('X',
    /// 1)` = X₁. Indices are 1-based within the puncturing period.
    pub sequence: &'static [(char, usize)],
}

/// Table 3-8, transcribed (rows 1/2, 2/3, 3/4, 5/6, 7/8).
pub const TABLE_3_8: [PunctureSpec; 5] = [
    PunctureSpec {
        rate: CodeRate::R1_2,
        x: "1",
        y: "1",
        sequence: &[('X', 1), ('Y', 1)],
    },
    PunctureSpec {
        rate: CodeRate::R2_3,
        x: "10",
        y: "11",
        sequence: &[('X', 1), ('Y', 1), ('Y', 2)],
    },
    PunctureSpec {
        rate: CodeRate::R3_4,
        x: "101",
        y: "110",
        sequence: &[('X', 1), ('Y', 1), ('Y', 2), ('X', 3)],
    },
    PunctureSpec {
        rate: CodeRate::R5_6,
        x: "10101",
        y: "11010",
        sequence: &[('X', 1), ('Y', 1), ('Y', 2), ('X', 3), ('Y', 4), ('X', 5)],
    },
    PunctureSpec {
        rate: CodeRate::R7_8,
        x: "1000101",
        y: "1111010",
        sequence: &[
            ('X', 1),
            ('Y', 1),
            ('Y', 2),
            ('Y', 3),
            ('Y', 4),
            ('X', 5),
            ('Y', 6),
            ('X', 7),
        ],
    },
];

/// The Table 3-8 row for `rate`.
pub fn puncture_spec(rate: CodeRate) -> &'static PunctureSpec {
    TABLE_3_8
        .iter()
        .find(|s| s.rate == rate)
        .expect("all rates tabulated")
}

/// Compiled puncturing: for each transmitted bit of a period, the index of
/// the mother-code bit it carries, counting mother bits as X₁,Y₁,X₂,Y₂,…
/// (index 2(i−1) for Xᵢ, 2(i−1)+1 for Yᵢ).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Puncturing {
    /// Input bits per period (the period of Table 3-8's pattern).
    pub period: usize,
    /// Mother-code bit index (within 2·period) of each transmitted bit, in
    /// transmission order.
    pub order: Vec<usize>,
}

impl Puncturing {
    /// From Table 3-8 for `rate`.
    pub fn new(rate: CodeRate) -> Self {
        let s = puncture_spec(rate);
        let order = s
            .sequence
            .iter()
            .map(|&(c, i)| 2 * (i - 1) + usize::from(c == 'Y'))
            .collect();
        Self {
            period: s.x.len(),
            order,
        }
    }
    /// Transmitted bits per period.
    pub fn out_per_period(&self) -> usize {
        self.order.len()
    }
}

/// Encode `bits` (0/1 values, length a multiple of the puncturing period)
/// and puncture them, appending the transmitted bits to `out`. The
/// puncturing pattern starts at its first position at the beginning of
/// `bits`: call once per frame to obtain the §3.8 frame-synchronous reset.
pub fn encode_punctured(enc: &mut ConvEncoder, punct: &Puncturing, bits: &[u8], out: &mut Vec<u8>) {
    assert_eq!(
        bits.len() % punct.period,
        0,
        "frame must hold whole puncturing periods"
    );
    let mut mother = [0u8; 14];
    out.reserve(bits.len() / punct.period * punct.out_per_period());
    for blk in bits.chunks_exact(punct.period) {
        for (i, &u) in blk.iter().enumerate() {
            let (x, y) = enc.push(u);
            mother[2 * i] = x;
            mother[2 * i + 1] = y;
        }
        out.extend(punct.order.iter().map(|&k| mother[k]));
    }
}

/// Inverse of puncturing on soft values: expand transmitted-order LLRs to
/// mother-code pairs (X, Y, X, Y, …) with 0.0 at punctured positions. The
/// pattern restarts at the beginning of `llrs` (§3.8 frame reset); `llrs`
/// must hold whole periods.
pub fn depuncture(punct: &Puncturing, llrs: &[f32], out: &mut Vec<f32>) {
    let n = punct.out_per_period();
    assert_eq!(
        llrs.len() % n,
        0,
        "frame must hold whole puncturing periods"
    );
    let base = out.len();
    out.resize(base + llrs.len() / n * 2 * punct.period, 0.0);
    for (blk, dst) in llrs
        .chunks_exact(n)
        .zip(out[base..].chunks_exact_mut(2 * punct.period))
    {
        for (&l, &k) in blk.iter().zip(&punct.order) {
            dst[k] = l;
        }
    }
}

const HALF: usize = STATES / 2;

/// Branch sign tables for the vectorised add-compare-select. For new state
/// `ns = (u << 5) | (s >> 1)` the predecessors are `s = 2·(ns mod 32) + b`.
struct Tables {
    // [u][b][j]: sign (+1 for code bit 0, −1 for 1) of X and Y on the branch
    // from s = 2j + b with input u into ns = 32u + j.
    sx: [[[f32; HALF]; 2]; 2],
    sy: [[[f32; HALF]; 2]; 2],
}

fn tables() -> &'static Tables {
    use std::sync::OnceLock;
    static T: OnceLock<Tables> = OnceLock::new();
    T.get_or_init(|| {
        let mut t = Tables {
            sx: [[[0.0; HALF]; 2]; 2],
            sy: [[[0.0; HALF]; 2]; 2],
        };
        for u in 0..2u8 {
            for b in 0..2u8 {
                for j in 0..HALF {
                    let s = (2 * j) as u8 | b;
                    let (x, y) = branch_output(s, u);
                    t.sx[u as usize][b as usize][j] = if x == 0 { 1.0 } else { -1.0 };
                    t.sy[u as usize][b as usize][j] = if y == 0 { 1.0 } else { -1.0 };
                }
            }
        }
        t
    })
}

/// Default traceback depth (≈ 16 × constraint length is ample for the 7/8
/// punctured code).
pub const DEFAULT_TRACEBACK: usize = 112;

/// Streaming soft-decision Viterbi decoder for the k = 7 mother code
/// (§3.8), maximising the correlation Σ ±LLR. Path metrics persist across
/// calls; decisions are released `traceback` steps late until
/// [`ViterbiDecoder::flush`] traces back from the best state and releases
/// the rest.
#[derive(Debug, Clone)]
pub struct ViterbiDecoder {
    pm: [f32; STATES],
    decisions: Vec<u64>,
    traceback: usize,
    block: usize,
}

impl ViterbiDecoder {
    /// Decoder starting in the all-zero encoder state with the given
    /// traceback depth (≥ 1).
    pub fn new(traceback: usize) -> Self {
        let mut pm = [-1.0e6f32; STATES];
        pm[0] = 0.0;
        let traceback = traceback.max(1);
        Self {
            pm,
            decisions: Vec::new(),
            traceback,
            block: 4096.max(traceback),
        }
    }

    /// Decoder that assumes an unknown start state (all metrics equal), for
    /// receivers joining mid-stream.
    pub fn new_unknown_state(traceback: usize) -> Self {
        let mut d = Self::new(traceback);
        d.pm = [0.0; STATES];
        d
    }

    /// Traceback depth.
    pub fn traceback(&self) -> usize {
        self.traceback
    }

    /// Steps buffered but not yet released.
    pub fn pending(&self) -> usize {
        self.decisions.len()
    }

    #[inline]
    fn acs(&mut self, lx: f32, ly: f32) {
        let t = tables();
        let mut ev = [0f32; HALF];
        let mut od = [0f32; HALF];
        for j in 0..HALF {
            ev[j] = self.pm[2 * j];
            od[j] = self.pm[2 * j + 1];
        }
        let mut new = [0f32; STATES];
        let mut dec: u64 = 0;
        for u in 0..2 {
            let (sx0, sy0) = (&t.sx[u][0], &t.sy[u][0]);
            let (sx1, sy1) = (&t.sx[u][1], &t.sy[u][1]);
            let mut m0 = [0f32; HALF];
            let mut m1 = [0f32; HALF];
            for j in 0..HALF {
                m0[j] = ev[j] + sx0[j] * lx + sy0[j] * ly;
                m1[j] = od[j] + sx1[j] * lx + sy1[j] * ly;
            }
            let mut d: u32 = 0;
            for j in 0..HALF {
                let c = m1[j] > m0[j];
                new[u * HALF + j] = if c { m1[j] } else { m0[j] };
                d |= (c as u32) << j;
            }
            dec |= (d as u64) << (u * HALF);
        }
        self.pm = new;
        self.decisions.push(dec);
    }

    fn renormalise(&mut self) {
        let m = self.pm.iter().copied().fold(f32::MIN, f32::max);
        for p in self.pm.iter_mut() {
            *p -= m;
        }
    }

    fn best_state(&self) -> usize {
        let mut best = 0;
        for s in 1..STATES {
            if self.pm[s] > self.pm[best] {
                best = s;
            }
        }
        best
    }

    /// Trace back over all buffered steps from `state` and append the first
    /// `release` decoded bits to `out`, dropping them from the buffer.
    fn release(&mut self, state: usize, release: usize, out: &mut Vec<u8>) {
        let n = self.decisions.len();
        let mut s = state;
        let mut bits = vec![0u8; release];
        for t in (0..n).rev() {
            let u = (s >> 5) as u8;
            if t < release {
                bits[t] = u;
            }
            let d = (self.decisions[t] >> s) & 1;
            s = ((s << 1) & (STATES - 1)) | d as usize;
        }
        out.extend_from_slice(&bits);
        self.decisions.drain(..release);
    }

    /// Feed mother-code LLR pairs `[Lx₀, Ly₀, Lx₁, Ly₁, …]` (use
    /// [`depuncture`] first); appends decided bits to `out`, lagging the
    /// input by at least `traceback` steps.
    pub fn push(&mut self, pairs: &[f32], out: &mut Vec<u8>) {
        assert_eq!(pairs.len() % 2, 0);
        for (i, p) in pairs.as_chunks::<2>().0.iter().enumerate() {
            self.acs(p[0], p[1]);
            if i % 64 == 63 {
                self.renormalise();
            }
            if self.decisions.len() >= self.block + self.traceback {
                let s = self.best_state();
                self.release(s, self.block, out);
            }
        }
        self.renormalise();
    }

    /// Release every buffered decision, tracing back from the currently best
    /// state (no future information is used for the last `traceback` bits).
    pub fn flush(&mut self, out: &mut Vec<u8>) {
        let n = self.decisions.len();
        if n > 0 {
            let s = self.best_state();
            self.release(s, n, out);
        }
    }
}

/// Decode a complete punctured block (depuncture, Viterbi from the zero
/// state, flush). Convenience for tests and one-shot use.
pub fn decode_punctured(rate: CodeRate, llrs: &[f32], traceback: usize) -> Vec<u8> {
    let p = Puncturing::new(rate);
    let mut pairs = Vec::new();
    depuncture(&p, llrs, &mut pairs);
    let mut v = ViterbiDecoder::new(traceback);
    let mut out = Vec::with_capacity(pairs.len() / 2);
    v.push(&pairs, &mut out);
    v.flush(&mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::XorShift;

    const RATES: [CodeRate; 5] = [
        CodeRate::R1_2,
        CodeRate::R2_3,
        CodeRate::R3_4,
        CodeRate::R5_6,
        CodeRate::R7_8,
    ];

    /// Fig. 3-10 / §3.8: impulse response of X and Y equals the generator
    /// taps 171₈ = 1111001₂ and 133₈ = 1011011₂ (MSB = undelayed input).
    #[test]
    fn spec_fig_3_10_impulse_response() {
        let mut e = ConvEncoder::new();
        let mut xs = Vec::new();
        let mut ys = Vec::new();
        for t in 0..CONSTRAINT_LENGTH + 3 {
            let (x, y) = e.push(u8::from(t == 0));
            xs.push(x);
            ys.push(y);
        }
        assert_eq!(xs, [1, 1, 1, 1, 0, 0, 1, 0, 0, 0]);
        assert_eq!(ys, [1, 0, 1, 1, 0, 1, 1, 0, 0, 0]);
        assert_eq!(e.state(), 0);
    }

    /// Table 3-8: patterns, rates and transmission order exactly as printed;
    /// the order is "Xᵢ (if kept) then Yᵢ (if kept)" for i = 1..period.
    #[test]
    fn spec_table_3_8_puncturing() {
        for s in TABLE_3_8 {
            let (num, den) = s.rate.ratio();
            assert_eq!(s.x.len(), s.y.len());
            assert_eq!(s.x.len(), num, "period = numerator");
            let kept = s.x.matches('1').count() + s.y.matches('1').count();
            assert_eq!(kept, den, "{:?}", s.rate);
            assert_eq!(s.sequence.len(), den);
            let mut derived = Vec::new();
            for i in 0..s.x.len() {
                if &s.x[i..=i] == "1" {
                    derived.push(('X', i + 1));
                }
                if &s.y[i..=i] == "1" {
                    derived.push(('Y', i + 1));
                }
            }
            assert_eq!(derived, s.sequence, "{:?}", s.rate);
        }
        assert_eq!(
            Puncturing::new(CodeRate::R7_8).order,
            vec![0, 1, 3, 5, 7, 8, 11, 12]
        );
        assert_eq!(Puncturing::new(CodeRate::R3_4).order, vec![0, 1, 3, 4]);
    }

    /// Puncture output for a known input: rate 3/4 of an impulse.
    #[test]
    fn spec_table_3_8_impulse_3_4() {
        let p = Puncturing::new(CodeRate::R3_4);
        let mut out = Vec::new();
        encode_punctured(&mut ConvEncoder::new(), &p, &[1, 0, 0, 0, 0, 0], &mut out);
        // mother: X=1,1,1,1,0,0  Y=1,0,1,1,0,1 ; per period X1 Y1 Y2 X3
        assert_eq!(out, [1, 1, 0, 1, /* */ 1, 1, 0, 0]);
    }

    fn noiseless(bits: &[u8]) -> Vec<f32> {
        bits.iter()
            .map(|&b| if b == 0 { 8.0 } else { -8.0 })
            .collect()
    }

    /// Viterbi decodes noiseless streams exactly for every rate.
    #[test]
    fn prop_viterbi_noiseless_all_rates() {
        let mut rng = XorShift::new(5);
        for r in RATES {
            let p = Puncturing::new(r);
            for len in [p.period * 3, p.period * 700, p.period * 3001] {
                let bits: Vec<u8> = (0..len).map(|_| (rng.next() & 1) as u8).collect();
                let mut coded = Vec::new();
                encode_punctured(&mut ConvEncoder::new(), &p, &bits, &mut coded);
                for tb in [8, DEFAULT_TRACEBACK] {
                    assert_eq!(
                        decode_punctured(r, &noiseless(&coded), tb),
                        bits,
                        "{r:?} len {len} tb {tb}"
                    );
                }
            }
        }
    }

    /// Streaming in odd chunk sizes equals one-shot decoding.
    #[test]
    fn viterbi_streaming_chunks() {
        let mut rng = XorShift::new(11);
        let bits: Vec<u8> = (0..20000).map(|_| (rng.next() & 1) as u8).collect();
        let p = Puncturing::new(CodeRate::R1_2);
        let mut coded = Vec::new();
        encode_punctured(&mut ConvEncoder::new(), &p, &bits, &mut coded);
        let llr = noiseless(&coded);
        let mut v = ViterbiDecoder::new(64);
        let mut out = Vec::new();
        for c in llr.chunks(2 * 777) {
            v.push(c, &mut out);
        }
        v.flush(&mut out);
        assert_eq!(out, bits);
    }

    /// Robustness: BPSK over AWGN, rate 1/2 at Eb/N0 = 4 dB (Es/N0 = 1 dB):
    /// coded BER must be far below the raw hard-decision channel BER.
    #[test]
    fn viterbi_awgn_improves_ber() {
        let mut rng = XorShift::new(3);
        for (rate, ebn0_db) in [
            (CodeRate::R1_2, 4.0f64),
            (CodeRate::R3_4, 5.0),
            (CodeRate::R7_8, 5.0),
        ] {
            let p = Puncturing::new(rate);
            let n = p.period * 40000;
            let bits: Vec<u8> = (0..n).map(|_| (rng.next() & 1) as u8).collect();
            let mut coded = Vec::new();
            encode_punctured(&mut ConvEncoder::new(), &p, &bits, &mut coded);
            let (num, den) = rate.ratio();
            let esn0 = 10f64.powf(ebn0_db / 10.0) * num as f64 / den as f64;
            let sigma = (1.0 / (2.0 * esn0)).sqrt();
            let mut raw_err = 0usize;
            let llr: Vec<f32> = coded
                .iter()
                .map(|&b| {
                    let s = if b == 0 { 1.0 } else { -1.0 };
                    let y = s + sigma * rng.gauss();
                    raw_err += usize::from((y < 0.0) != (b == 1));
                    (2.0 * y / (sigma * sigma)) as f32
                })
                .collect();
            let dec = decode_punctured(rate, &llr, DEFAULT_TRACEBACK);
            let err = dec.iter().zip(&bits).filter(|(a, b)| a != b).count();
            let raw_ber = raw_err as f64 / coded.len() as f64;
            let ber = err as f64 / n as f64;
            println!("{rate:?} Eb/N0 {ebn0_db} dB: raw BER {raw_ber:.2e}, decoded BER {ber:.2e}");
            assert!(raw_ber > 3e-3, "{rate:?} channel too clean: {raw_ber}");
            assert!(
                ber < raw_ber / 20.0,
                "{rate:?}: coded BER {ber} vs raw {raw_ber}"
            );
        }
    }

    /// Throughput measurement (run with `cargo test --release -p isdbt-fec
    /// -- --ignored --nocapture viterbi_throughput`).
    #[test]
    #[ignore]
    fn viterbi_throughput() {
        let mut rng = XorShift::new(9);
        let n = 20_000_000usize;
        let bits: Vec<u8> = (0..n).map(|_| (rng.next() & 1) as u8).collect();
        let p = Puncturing::new(CodeRate::R1_2);
        let mut coded = Vec::new();
        encode_punctured(&mut ConvEncoder::new(), &p, &bits, &mut coded);
        let llr: Vec<f32> = coded
            .iter()
            .map(|&b| if b == 0 { 1.0 } else { -1.0 } + 0.6 * rng.gauss() as f32)
            .collect();
        let t = std::time::Instant::now();
        let mut v = ViterbiDecoder::new(DEFAULT_TRACEBACK);
        let mut out = Vec::with_capacity(n);
        v.push(&llr, &mut out);
        v.flush(&mut out);
        let dt = t.elapsed().as_secs_f64();
        let err = out.iter().zip(&bits).filter(|(a, b)| a != b).count();
        println!(
            "viterbi: {n} bits in {dt:.3} s = {:.1} Mbit/s decoded (errors {err})",
            n as f64 / dt / 1e6
        );
    }
}

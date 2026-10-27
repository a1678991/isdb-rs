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
//! positions are given LLR 0. The mother code and the Viterbi decoder are
//! `isdb_fec::conv`, shared with ISDB-S; the Table 3-8 puncturing is here.

use isdbt_core::CodeRate;

pub use isdb_fec::conv::{
    CONSTRAINT_LENGTH, ConvEncoder, DEFAULT_TRACEBACK, G1, G2, STATES, ViterbiDecoder,
    branch_output,
};

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
                    // the test LLR rounded to the decoder's f32
                    #[allow(clippy::cast_possible_truncation)]
                    let llr = (2.0 * y / (sigma * sigma)) as f32;
                    llr
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
            .map(|&b| {
                // the test noise rounded to the decoder's f32
                #[allow(clippy::cast_possible_truncation)]
                let n = 0.6 * rng.gauss() as f32;
                (if b == 0 { 1.0 } else { -1.0 }) + n
            })
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

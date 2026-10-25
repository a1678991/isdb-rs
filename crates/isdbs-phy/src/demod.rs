//! The main signal of a frame to bytes: soft demapping, depuncturing and
//! the K = 7 Viterbi decoder, run by run (MIC Ordinance No. 87 of 2011,
//! annexed tables 46 and 49; BO.1408-1 §9, Table 2, Figs. 14–15).
//!
//! One encoder runs through the whole frame (Fig. 14: a single
//! convolutional encoder feeds every modulation), so one decoder does too:
//! it starts in the state the header's W2/W3 left (their last six bits) and
//! ends on the next frame's W1, which it knows. The puncturing pattern
//! restarts at the start of each run (Table 2 note 1; table 49 note 5).
//!
//! - BPSK r = 1/2: X then Y of each input bit on successive symbols
//!   (table 49 note 3, SPEC-NOTES "coded-tails").
//! - QPSK: the (P1, P0) pairs of [`isdbs_core::combination::Puncture`]
//!   on one symbol each; P1 → I, P0 → Q (table 46 text, SPEC-NOTES
//!   "qpsk-map").
//! - TC8PSK r = 2/3: per symbol, B1 is sent uncoded as C2 and B0 enters the
//!   code, whose outputs are C1 = 133₈ (Y) and C0 = 171₈ (X): BO.1408-1
//!   Fig. 15 labels the points (W, Y, X) (SPEC-NOTES "tc8psk-map"). The eight
//!   points of table 46 are set-partitioned: for each (C1, C0) the two
//!   values of C2 are antipodal points. The decoder therefore uses, per
//!   branch, the larger of the two points' correlations — |Re(z·p̄)| for
//!   the subset's C2 = 0 point p — and decides C2 afterwards from the
//!   decoded (C1, C0) (SPEC-NOTES "tc8psk-decoding").
//!
//! Bytes enter MSB first: into B0 for BPSK and QPSK, alternately B1, B0
//! for TC8PSK (B1 the higher bit; table 49 note 3, BO.1408-1 §9).

use crate::Cplx;
use isdb_fec::{ConvEncoder, ViterbiDecoder};
use isdbs_core::combination::{Combination, Modulation};
use isdbs_core::frame::{SLOT_BYTES, W1, W2, W3};
use isdbs_core::slots::runs;
use isdbs_core::tmcc::TmccInfo;

/// How coded bits map to constellation points. The default is the reading
/// confirmed by RS(204,188) on the lab recordings (SPEC-NOTES "qpsk-map",
/// "tc8psk-map"); the other settings exist to test the alternatives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mapping {
    /// Mirror the symbols (z → z̄) before demapping: a spectrum inversion
    /// in the down-conversion, invisible to the BPSK pilots.
    pub conjugate: bool,
    /// QPSK: P1 on Q and P0 on I (figure `-086`) instead of P1 on I and P0
    /// on Q (table 46 text).
    pub qpsk_p1_on_q: bool,
    /// TC8PSK: C1 is the 133₈ output and C0 the 171₈ one (BO.1408-1 Fig. 15
    /// labels the points (W, Y, X); the default) instead of C1 = 171₈,
    /// C0 = 133₈.
    pub tc8psk_c1_is_y: bool,
}

impl Default for Mapping {
    fn default() -> Self {
        Self {
            conjugate: false,
            qpsk_p1_on_q: false,
            tc8psk_c1_is_y: true,
        }
    }
}

/// Decoder settings.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DemodOptions {
    /// Constellation reading.
    pub mapping: Mapping,
    /// Start the decoder in the state the W2/W3 word left (else unknown).
    pub known_start: bool,
    /// Scale of the soft values (≈ 2/σ²; only ratios matter to Viterbi,
    /// this bounds the f32 range).
    pub llr_scale: f32,
}

impl Default for DemodOptions {
    fn default() -> Self {
        Self {
            mapping: Mapping::default(),
            known_start: true,
            llr_scale: 8.0,
        }
    }
}

/// The 8PSK point of table 46 for [C2 C1 C0] = `label` (0..8): 000 at 0°,
/// 001 +45°, 011 +90°, 010 +135°, 100 180°, 101 −135°, 111 −90°,
/// 110 −45°.
pub fn psk8_point(label: u8) -> Cplx {
    let k = match label & 7 {
        0b000 => 0,
        0b001 => 1,
        0b011 => 2,
        0b010 => 3,
        0b100 => 4,
        0b101 => 5,
        0b111 => 6,
        _ => 7, // 110
    };
    Cplx::from_polar(1.0, f64::from(k) * std::f64::consts::FRAC_PI_4)
}

/// The QPSK point of table 46 for (P1, P0): 00 +45°, 01 −45°, 10 +135°,
/// 11 −135° (P1 = the sign of I, P0 = the sign of Q).
pub fn qpsk_point(p1: u8, p0: u8) -> Cplx {
    let s = std::f64::consts::FRAC_1_SQRT_2;
    Cplx::new(
        if p1 & 1 == 0 { s } else { -s },
        if p0 & 1 == 0 { s } else { -s },
    )
}

/// The encoder register after a 16-bit sync word (its last six bits;
/// [`ConvEncoder::state`] holds u_{t−1} in bit 5).
pub fn state_after(word: u16) -> u8 {
    let mut e = ConvEncoder::new();
    for i in (0..16).rev() {
        e.push(((word >> i) & 1) as u8);
    }
    e.state()
}

fn word_bits(w: u16) -> [u8; 16] {
    std::array::from_fn(|i| ((w >> (15 - i)) & 1) as u8)
}

/// The decoded bytes of a frame: 203 per sent slot, in transmission order
/// (the interleaved order; [`isdbs_core::mainsig::codewords`] undoes it),
/// plus a measure of how well the symbols fitted the decoded path.
#[derive(Debug, Clone)]
pub struct FrameBytes {
    /// The bytes.
    pub bytes: Vec<u8>,
    /// MER of the main signal against its decided points, dB: TC8PSK
    /// points from the decoded path (C1, C0 re-encoded, C2 decided in its
    /// subset), QPSK and BPSK points by hard decision.
    pub mer_db: f64,
}

/// Decode the main signal of a frame (`main`: the 38 976 derotated,
/// unit-amplitude symbols of [`crate::rx::Frame::main`]) under `info`.
/// `first` tells whether the frame is frame 1 of its superframe (its header
/// ended in W2; frames 2–8 in W3).
pub fn decode_frame(main: &[Cplx], info: &TmccInfo, first: bool, o: &DemodOptions) -> FrameBytes {
    let m = o.mapping;
    let s = o.llr_scale;
    let z: Vec<Cplx> = if m.conjugate {
        main.iter().map(|v| v.conj()).collect()
    } else {
        main.to_vec()
    };
    let start = state_after(if first { W2 } else { W3 });
    let mut dec = if o.known_start {
        ViterbiDecoder::with_state(128, start)
    } else {
        ViterbiDecoder::new_unknown_state(128)
    };
    // Per trellis step: the branch metric quadruple m[2X + Y].
    let mut steps: Vec<[f32; 4]> = Vec::with_capacity(main.len() * 2);
    // TC8PSK steps: symbol index, to decide C2 after decoding.
    struct Tc {
        step: usize,
        sym: usize,
    }
    let mut tc: Vec<Tc> = Vec::new();
    let pair = |lx: f32, ly: f32| [lx + ly, lx - ly, -lx + ly, -lx - ly];
    let rs = runs(info);
    let mut bit_kinds: Vec<(Combination, usize)> = Vec::new();
    for r in &rs {
        let c = r.combination;
        let sym = &z[r.symbols.clone()];
        let info_bits = r.valid_slots * SLOT_BYTES * 8;
        bit_kinds.push((c, info_bits));
        match c.modulation() {
            Modulation::Bpsk => {
                for k in 0..info_bits {
                    let lx = (sym[2 * k].re as f32) * s;
                    let ly = (sym[2 * k + 1].re as f32) * s;
                    steps.push(pair(lx, ly));
                }
            }
            Modulation::Qpsk => {
                let soft = |v: Cplx| {
                    let (a, b) = if m.qpsk_p1_on_q {
                        (v.im, v.re)
                    } else {
                        (v.re, v.im)
                    };
                    ((a as f32) * s, (b as f32) * s)
                };
                match c.puncturing() {
                    None => {
                        for &v in sym.iter().take(info_bits) {
                            let (p1, p0) = soft(v);
                            steps.push(pair(p1, p0));
                        }
                    }
                    Some(p) => {
                        let period = p.period();
                        let pairs = p.pairs();
                        let mut k = 0usize; // symbol index in the run
                        let mut done = 0usize;
                        while done < info_bits {
                            let mut lx = vec![0f32; period];
                            let mut ly = vec![0f32; period];
                            for &(a, b) in pairs {
                                let (p1, p0) = soft(sym[k]);
                                k += 1;
                                for (mb, l) in [(a, p1), (b, p0)] {
                                    match mb.0 {
                                        'X' => lx[mb.1 - 1] = l,
                                        _ => ly[mb.1 - 1] = l,
                                    }
                                }
                            }
                            for i in 0..period.min(info_bits - done) {
                                steps.push(pair(lx[i], ly[i]));
                            }
                            done += period;
                        }
                    }
                }
            }
            Modulation::Tc8psk => {
                for (k, &v) in sym.iter().enumerate().take(info_bits / 2) {
                    let mut q = [0f32; 4];
                    for c1 in 0..2u8 {
                        for c0 in 0..2u8 {
                            let p = psk8_point((c1 << 1) | c0);
                            let corr = (v * p.conj()).re.abs() as f32 * s;
                            let (x, y) = if m.tc8psk_c1_is_y { (c0, c1) } else { (c1, c0) };
                            q[usize::from(2 * x + y)] = corr;
                        }
                    }
                    tc.push(Tc {
                        step: steps.len(),
                        sym: r.symbols.start + k,
                    });
                    steps.push(q);
                }
            }
        }
    }
    let mut b0 = Vec::with_capacity(steps.len() + 16);
    dec.push_metrics(&steps, &mut b0);
    dec.push_known(&word_bits(W1), &mut b0);
    dec.flush(&mut b0);
    b0.truncate(steps.len());
    // Re-encode for the TC8PSK C2 decisions and the quality figure.
    // The register after shifting in `start`'s bits, oldest (bit 0) first.
    let mut enc = ConvEncoder::new();
    for i in 0..6 {
        enc.push((start >> i) & 1);
    }
    debug_assert_eq!(enc.state(), start);
    let coded: Vec<(u8, u8)> = b0.iter().map(|&u| enc.push(u)).collect();
    let mut c2 = vec![0u8; steps.len()];
    let (mut sig, mut err) = (0f64, 0f64);
    for r in &rs {
        let (pts, off) = match r.combination.modulation() {
            Modulation::Tc8psk => continue,
            Modulation::Bpsk => (2.0, 0.0),
            Modulation::Qpsk => (4.0, std::f64::consts::FRAC_PI_4),
        };
        let step = std::f64::consts::TAU / pts;
        for &v in &z[r.symbols.clone()] {
            let a = ((v.arg() - off) / step).round() * step + off;
            sig += 1.0;
            err += (v - Cplx::from_polar(1.0, a)).norm_sqr();
        }
    }
    for t in &tc {
        let (x, y) = coded[t.step];
        let (c1, c0) = if m.tc8psk_c1_is_y { (y, x) } else { (x, y) };
        let p = psk8_point((c1 << 1) | c0);
        let v = z[t.sym];
        let bit = u8::from((v * p.conj()).re < 0.0);
        c2[t.step] = bit;
        let d = psk8_point((bit << 2) | (c1 << 1) | c0);
        sig += 1.0;
        err += (v - d).norm_sqr();
    }
    // Assemble bytes run by run.
    let mut bytes = Vec::new();
    let mut step = 0usize;
    for &(c, info_bits) in &bit_kinds {
        let mut acc = 0u8;
        let mut nb = 0;
        let mut push_bit = |b: u8, bytes: &mut Vec<u8>| {
            acc = (acc << 1) | (b & 1);
            nb += 1;
            if nb == 8 {
                bytes.push(acc);
                acc = 0;
                nb = 0;
            }
        };
        if c.modulation() == Modulation::Tc8psk {
            for _ in 0..info_bits / 2 {
                push_bit(c2[step], &mut bytes);
                push_bit(b0[step], &mut bytes);
                step += 1;
            }
        } else {
            for _ in 0..info_bits {
                push_bit(b0[step], &mut bytes);
                step += 1;
            }
        }
    }
    FrameBytes {
        bytes,
        mer_db: if sig > 0.0 {
            10.0 * (sig / err.max(1e-30)).log10()
        } else {
            f64::NAN
        },
    }
}

/// The transmit side of [`decode_frame`] (tests and the future
/// modulator): the 38 976 main symbols of a frame from its bytes (203 per
/// sent slot, slot order), the encoder starting in the state the header's
/// W2/W3 left. Unused symbols at the end of the frame stay 0.
pub fn encode_frame(bytes: &[u8], info: &TmccInfo, first: bool, m: Mapping) -> Vec<Cplx> {
    let mut enc = ConvEncoder::new();
    let start = state_after(if first { W2 } else { W3 });
    for i in 0..6 {
        enc.push((start >> i) & 1);
    }
    let mut out = vec![Cplx::new(0.0, 0.0); isdbs_core::frame::MAIN_SYMBOLS];
    let mut bits = bytes
        .iter()
        .flat_map(|&b| (0..8).rev().map(move |i| (b >> i) & 1));
    for r in runs(info) {
        let c = r.combination;
        let info_bits = r.valid_slots * SLOT_BYTES * 8;
        let mut k = r.symbols.start;
        match c.modulation() {
            Modulation::Bpsk => {
                for _ in 0..info_bits {
                    let (x, y) = enc.push(bits.next().unwrap_or(0));
                    for b in [x, y] {
                        out[k] = Cplx::new(if b == 0 { 1.0 } else { -1.0 }, 0.0);
                        k += 1;
                    }
                }
            }
            Modulation::Qpsk => {
                let put = |p1: u8, p0: u8| {
                    let q = qpsk_point(p1, p0);
                    if m.qpsk_p1_on_q {
                        Cplx::new(q.im, q.re)
                    } else {
                        q
                    }
                };
                match c.puncturing() {
                    None => {
                        for _ in 0..info_bits {
                            let (x, y) = enc.push(bits.next().unwrap_or(0));
                            out[k] = put(x, y);
                            k += 1;
                        }
                    }
                    Some(p) => {
                        let mut done = 0;
                        while done < info_bits {
                            let mut xy = Vec::with_capacity(p.period());
                            for _ in 0..p.period() {
                                xy.push(enc.push(bits.next().unwrap_or(0)));
                            }
                            for &(a, b) in p.pairs() {
                                let get = |mb: isdbs_core::combination::MotherBit| {
                                    let (x, y) = xy[mb.1 - 1];
                                    if mb.0 == 'X' { x } else { y }
                                };
                                out[k] = put(get(a), get(b));
                                k += 1;
                            }
                            done += p.period();
                        }
                    }
                }
            }
            Modulation::Tc8psk => {
                for _ in 0..info_bits / 2 {
                    let c2 = bits.next().unwrap_or(0);
                    let (x, y) = enc.push(bits.next().unwrap_or(0));
                    let (c1, c0) = if m.tc8psk_c1_is_y { (y, x) } else { (x, y) };
                    out[k] = psk8_point((c2 << 2) | (c1 << 1) | c0);
                    k += 1;
                }
            }
        }
    }
    if m.conjugate {
        for v in &mut out {
            *v = v.conj();
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use isdbs_core::frame::SLOTS;
    use isdbs_core::mainsig::sent_slots;
    use isdbs_core::tmcc::{CM, CombinationEntry, TM};
    use proptest::prelude::*;

    fn info(c: &[(u8, u8)]) -> TmccInfo {
        let mut combinations = [CombinationEntry {
            word: 0xF,
            slots: 0,
        }; CM];
        for (i, &(w, s)) in c.iter().enumerate() {
            combinations[i] = CombinationEntry { word: w, slots: s };
        }
        TmccInfo {
            order_of_change: 0,
            combinations,
            relative_ts: [0; SLOTS],
            ts_ids: [0xFFFF; TM],
            emergency: false,
            uplink: 0,
            reserved: 0,
        }
    }

    /// Annexed table 46: the 8PSK labels sit at the printed angles (not a
    /// Gray labelling: 110 at −45° and 000 at 0° differ in two bits) and
    /// the QPSK pairs at ±45° / ±135°.
    #[test]
    fn spec_table_46_points() {
        let deg = |p: Cplx| (p.arg().to_degrees().round() as i32 + 360) % 360;
        let want = [
            (0b000, 0),
            (0b001, 45),
            (0b011, 90),
            (0b010, 135),
            (0b100, 180),
            (0b101, 225),
            (0b111, 270),
            (0b110, 315),
        ];
        for (l, a) in want {
            assert_eq!(deg(psk8_point(l)), a, "{l:03b}");
        }
        assert_eq!(deg(qpsk_point(0, 0)), 45);
        assert_eq!(deg(qpsk_point(1, 0)), 135);
        assert_eq!(deg(qpsk_point(1, 1)), 225);
        assert_eq!(deg(qpsk_point(0, 1)), 315);
        // set partitioning: for each (C1, C0), C2 = 0 and 1 are antipodal
        for low in 0..4u8 {
            assert!((psk8_point(low) + psk8_point(4 | low)).norm() < 1e-12);
        }
    }

    /// The encoder state after W2 / W3 is their last six bits.
    #[test]
    fn spec_state_after_words() {
        assert_eq!(state_after(W2), state_after(0x0040 | (W2 & 0x3F)));
        assert_ne!(state_after(W2), state_after(W3));
    }

    proptest! {
        /// Every combination mix decodes noiselessly back to its bytes,
        /// whatever the frame's position in the superframe.
        #[test]
        fn prop_frame_round_trip(seed in any::<u64>(), mix in 0usize..5, first in any::<bool>()) {
            let inf = [
                info(&[(7, 48)]),
                info(&[(4, 48)]),
                info(&[(7, 44), (1, 4)]),
                info(&[(6, 40), (2, 8)]),
                info(&[(5, 30), (3, 18)]),
            ][mix].clone();
            let n = sent_slots(&inf).len() * SLOT_BYTES;
            let mut x = seed | 1;
            let bytes: Vec<u8> = (0..n).map(|_| { x ^= x << 13; x ^= x >> 7; x ^= x << 17; x as u8 }).collect();
            let m = Mapping::default();
            let z = encode_frame(&bytes, &inf, first, m);
            let d = decode_frame(&z, &inf, first, &DemodOptions::default());
            prop_assert_eq!(d.bytes, bytes);
        }
    }

    /// TC8PSK with noise at 12 dB SNR (about the lab's BS level) decodes
    /// with few byte errors; the reverse C1/C0 reading does not.
    #[test]
    fn tc8psk_noise_and_wrong_reading() {
        let inf = info(&[(7, 48)]);
        let n = 48 * SLOT_BYTES;
        let mut x = 99u64;
        let mut rnd = || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        let bytes: Vec<u8> = (0..n).map(|_| rnd() as u8).collect();
        let mut z = encode_frame(&bytes, &inf, false, Mapping::default());
        let sigma = (10f64.powf(-12.0 / 10.0) / 2.0).sqrt();
        for v in &mut z {
            let u1 = ((rnd() >> 11) as f64 + 1.0) / (1u64 << 53) as f64;
            let u2 = (rnd() >> 11) as f64 / (1u64 << 53) as f64;
            let r = (-2.0 * u1.ln()).sqrt() * sigma;
            *v += Cplx::from_polar(r, std::f64::consts::TAU * u2);
        }
        let good = decode_frame(&z, &inf, false, &DemodOptions::default());
        let errs = good
            .bytes
            .iter()
            .zip(&bytes)
            .filter(|(a, b)| a != b)
            .count();
        assert!(errs < n / 200, "{errs} byte errors");
        assert!((good.mer_db - 12.0).abs() < 1.0, "{}", good.mer_db);
        let o = DemodOptions {
            mapping: Mapping {
                tc8psk_c1_is_y: false,
                ..Mapping::default()
            },
            ..DemodOptions::default()
        };
        let bad = decode_frame(&z, &inf, false, &o);
        let errs = bad.bytes.iter().zip(&bytes).filter(|(a, b)| a != b).count();
        assert!(errs > n / 4, "{errs}");
    }
}

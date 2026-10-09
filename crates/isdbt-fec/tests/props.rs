//! Property tests: inverse pairs of every stage and the per-layer pipeline
//! (ARIB STD-B31 §3.3–3.8).

use isdbt_core::{CodeRate, LayerConfig, Mode, Modulation, TRANSMISSION_TSP, TSP, TimeInterleave};
use isdbt_fec::byte_interleave::{ByteDeinterleaver, ByteInterleaver, PAIR_LATENCY_BYTES};
use isdbt_fec::conv::{
    ConvEncoder, DEFAULT_TRACEBACK, Puncturing, decode_punctured, encode_punctured,
};
use isdbt_fec::{LayerDecoder, LayerEncoder, rs};
use proptest::prelude::*;

const MODS: [Modulation; 4] = [
    Modulation::Dqpsk,
    Modulation::Qpsk,
    Modulation::Qam16,
    Modulation::Qam64,
];
const RATES: [CodeRate; 5] = [
    CodeRate::R1_2,
    CodeRate::R2_3,
    CodeRate::R3_4,
    CodeRate::R5_6,
    CodeRate::R7_8,
];

fn lc(segments: usize, modulation: Modulation, code_rate: CodeRate) -> LayerConfig {
    LayerConfig {
        segments,
        modulation,
        code_rate,
        time_interleave: TimeInterleave(0),
    }
}

fn llr_of(bits: &[u8]) -> Vec<f32> {
    bits.iter()
        .map(|&b| if b == 0 { 20.0 } else { -20.0 })
        .collect()
}

/// Deterministic frame contents from a seed (splitmix64).
fn frame(seed: u64, n: usize) -> Vec<[u8; TSP]> {
    let mut s = seed;
    let mut next = move || {
        s = s.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = s;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    };
    (0..n)
        .map(|_| {
            let mut p = [0u8; TSP];
            for c in p.chunks_mut(8) {
                let v = next().to_le_bytes();
                c.copy_from_slice(&v[..c.len()]);
            }
            p[0] = 0x47;
            p
        })
        .collect()
}

/// Run `frames` frames TX → noiseless LLR → RX and check the one-frame
/// latency contract.
fn layer_roundtrip(mode: Mode, l: &LayerConfig, seed: u64, frames: usize) {
    let mut enc = LayerEncoder::new(mode, l);
    let mut dec = LayerDecoder::new(mode, l);
    let t = l.tsp_per_frame(mode);
    let mut prev: Option<Vec<[u8; TSP]>> = None;
    for k in 0..frames {
        let f = frame(seed ^ (k as u64) << 32, t);
        let bits = enc.encode_frame_188(&f).unwrap();
        assert_eq!(bits.len(), l.coded_bits_per_frame(mode));
        assert_eq!(
            bits.len(),
            mode.data_carriers_per_segment() * l.modulation.bits_per_symbol() * l.segments * 204
        );
        let out = dec.decode_frame(&llr_of(&bits)).unwrap();
        assert_eq!(out.tsps.len(), t);
        if let Some(p) = prev {
            assert_eq!(out.stats.clean, t, "{mode:?} {l:?} frame {k}");
            assert!(
                out.tsps_188() == p,
                "{mode:?} {l:?} frame {k}: payload mismatch"
            );
        }
        prev = Some(f);
    }
}

/// LayerEncoder → LayerDecoder identity for every modulation × code rate in
/// every mode (1 segment, 3 frames), latency one frame.
#[test]
fn prop_layer_roundtrip_all_combinations() {
    for mode in [Mode::M1, Mode::M2, Mode::M3] {
        for m in MODS {
            for r in RATES {
                layer_roundtrip(mode, &lc(1, m, r), 0xC0FFEE, 3);
            }
        }
    }
}

/// Full-size layers: 12-segment 64QAM 3/4 and 13-segment 64QAM 7/8 in
/// Mode 3 (the largest frame, 3276 TSPs).
#[test]
fn prop_layer_roundtrip_full_size() {
    layer_roundtrip(Mode::M3, &lc(12, Modulation::Qam64, CodeRate::R3_4), 1, 3);
    layer_roundtrip(Mode::M3, &lc(13, Modulation::Qam64, CodeRate::R7_8), 2, 3);
}

/// Receiver joining mid-stream (fresh decoder at TX frame 2, unknown
/// Viterbi start state): output slots 11..T−1 of its first call are already
/// correct, everything is correct from its second call.
#[test]
fn layer_mid_stream_join() {
    let mode = Mode::M2;
    let l = lc(3, Modulation::Qam16, CodeRate::R2_3);
    let t = l.tsp_per_frame(mode);
    let mut enc = LayerEncoder::new(mode, &l);
    let frames: Vec<_> = (0..5).map(|k| frame(77 + k, t)).collect();
    let coded: Vec<_> = frames
        .iter()
        .map(|f| enc.encode_frame_188(f).unwrap())
        .collect();
    let v = isdbt_fec::conv::ViterbiDecoder::new_unknown_state(DEFAULT_TRACEBACK);
    let mut dec = LayerDecoder::with_viterbi(mode, &l, v);
    let first = dec.decode_frame(&llr_of(&coded[2])).unwrap();
    assert_eq!(&first.tsps_188()[11..], &frames[1][11..]);
    for k in 3..5 {
        let out = dec.decode_frame(&llr_of(&coded[k])).unwrap();
        assert!(out.tsps_188() == frames[k - 1]);
    }
}

/// Channel errors: random sign flips on 0.3 % of LLRs and 3 % erasures at
/// rate 1/2 are removed by Viterbi + RS.
#[test]
fn layer_with_channel_errors() {
    let mode = Mode::M1;
    let l = lc(2, Modulation::Qam16, CodeRate::R1_2);
    let t = l.tsp_per_frame(mode);
    let mut enc = LayerEncoder::new(mode, &l);
    let mut dec = LayerDecoder::new(mode, &l);
    let mut s = 12345u64;
    let mut rnd = move || {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        s
    };
    let mut prev = None;
    for k in 0..4 {
        let f = frame(900 + k, t);
        let bits = enc.encode_frame_188(&f).unwrap();
        let llr: Vec<f32> = bits
            .iter()
            .map(|&b| {
                let r = rnd() % 1000;
                let v = if b == 0 { 4.0 } else { -4.0 };
                if r < 3 {
                    -v
                } else if r < 33 {
                    0.0
                } else {
                    v
                }
            })
            .collect();
        let out = dec.decode_frame(&llr).unwrap();
        if let Some(p) = prev {
            assert_eq!(out.stats.uncorrectable, 0);
            assert!(out.tsps_188() == p);
        }
        prev = Some(f);
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 32, ..ProptestConfig::default() })]

    /// Byte interleaver ∘ deinterleaver = delay of 11 × 204 bytes (§3.6/3.7).
    #[test]
    fn prop_byte_interleave_identity(data in proptest::collection::vec(any::<u8>(), 0..12_000), split in 1usize..5000) {
        let mut v = data.clone();
        let mut il = ByteInterleaver::new();
        let mut dl = ByteDeinterleaver::new();
        for c in v.chunks_mut(split) { il.process(c); }
        for c in v.chunks_mut(split.max(3) / 3 + 1) { dl.process(c); }
        if v.len() > PAIR_LATENCY_BYTES {
            prop_assert_eq!(&v[PAIR_LATENCY_BYTES..], &data[..data.len() - PAIR_LATENCY_BYTES]);
        }
    }

    /// RS(204,188): any ≤ 8 byte errors are corrected exactly (§3.3).
    #[test]
    fn prop_rs_corrects_up_to_8(
        data in proptest::collection::vec(any::<u8>(), TSP),
        errs in proptest::collection::btree_map(0usize..TRANSMISSION_TSP, 1u8..=255, 0..=8),
    ) {
        let d: [u8; TSP] = data.try_into().unwrap();
        let c = rs::encode(&d);
        let mut r = c;
        for (&p, &e) in &errs { r[p] ^= e; }
        let out = rs::decode(&mut r).unwrap();
        prop_assert_eq!(out.errors, errs.len());
        prop_assert_eq!(r, c);
    }

    /// Viterbi recovers noiseless punctured streams exactly (§3.8).
    #[test]
    fn prop_viterbi_noiseless(seed in any::<u64>(), periods in 1usize..600, ri in 0usize..5) {
        let r = RATES[ri];
        let p = Puncturing::new(r);
        let mut s = seed | 1;
        let bits: Vec<u8> = (0..p.period * periods).map(|_| { s ^= s << 13; s ^= s >> 7; s ^= s << 17; (s & 1) as u8 }).collect();
        let mut coded = Vec::new();
        encode_punctured(&mut ConvEncoder::new(), &p, &bits, &mut coded);
        prop_assert_eq!(decode_punctured(r, &llr_of(&coded), DEFAULT_TRACEBACK), bits);
    }

    /// LayerEncoder → LayerDecoder identity on random content for random
    /// small layers (Mode 1, 1–2 segments), several frames.
    #[test]
    fn prop_layer_roundtrip_random(seed in any::<u64>(), mi in 0usize..4, ri in 0usize..5, segs in 1usize..=2) {
        layer_roundtrip(Mode::M1, &lc(segs, MODS[mi], RATES[ri]), seed, 3);
    }
}

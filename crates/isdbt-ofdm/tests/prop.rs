//! Property tests: every TX stage composed with its RX inverse is the
//! identity (after the documented latency).

mod common;

use common::Rng;
use isdbt_core::ofdm_params::time_interleave_delayed_frames;
use isdbt_core::{
    CodeRate, GuardInterval, Layer, LayerConfig, Mode, Modulation, Params, Segments, TimeInterleave,
};
use isdbt_ofdm::ac::{AcFrame, SeismicWarning};
use isdbt_ofdm::combine::layer_segments;
use isdbt_ofdm::dsc;
use isdbt_ofdm::frame::FrameLayout;
use isdbt_ofdm::freq_interleave::{FrequencyInterleaver, RotationNumbering};
use isdbt_ofdm::latency::end_to_end_frames;
use isdbt_ofdm::mapping::{
    BitDeinterleaver, CarrierMapper, Demapper, constellation_point, dqpsk_differential, hard_label,
};
use isdbt_ofdm::time_interleave::{TimeDeinterleaver, TimeInterleaver};
use isdbt_ofdm::tmcc::{TmccInfo, decode_frame};
use isdbt_ofdm::{C32, DemodConfig, Demodulator, Modulator};
use proptest::prelude::*;

const MODS: [Modulation; 4] = [
    Modulation::Dqpsk,
    Modulation::Qpsk,
    Modulation::Qam16,
    Modulation::Qam64,
];
const MODES: [Mode; 3] = [Mode::M1, Mode::M2, Mode::M3];
const GUARDS: [GuardInterval; 4] = [
    GuardInterval::G1_4,
    GuardInterval::G1_8,
    GuardInterval::G1_16,
    GuardInterval::G1_32,
];

/// Random valid parameters (segment split, modulations respecting the
/// partial-reception → differential → coherent order).
fn arb_params(max_ti: u8) -> impl Strategy<Value = Params> {
    (
        0usize..3,
        0usize..4,
        any::<bool>(),
        1usize..=3,
        any::<u64>(),
    )
        .prop_map(move |(m, g, pr, nl, seed)| {
            let mut rng = Rng(seed);
            let mode = MODES[m];
            loop {
                let nl = if pr { nl.max(2) } else { nl };
                let mut cuts: Vec<usize> = Vec::new();
                let mut remaining = 13;
                for i in 0..nl {
                    let left = nl - i - 1;
                    let n = if i == nl - 1 {
                        remaining
                    } else if pr && i == 0 {
                        1
                    } else {
                        1 + (rng.next_u64() as usize) % (remaining - left)
                    };
                    cuts.push(n);
                    remaining -= n;
                }
                let mut layers = [None; 3];
                let mut coherent_seen = false;
                for (i, &n) in cuts.iter().enumerate() {
                    let mut md = MODS[(rng.next_u64() % 4) as usize];
                    if md.is_differential() && coherent_seen && !(pr && i == 0) {
                        md = Modulation::Qam16;
                    }
                    if !(pr && i == 0) {
                        coherent_seen |= !md.is_differential();
                    }
                    layers[i] = Some(LayerConfig {
                        segments: Segments::ALL[n - 1],
                        modulation: md,
                        code_rate: CodeRate::R1_2,
                        time_interleave: TimeInterleave::ALL
                            [(rng.next_u64() % (u64::from(max_ti) + 1)) as usize],
                    });
                }
                if let Ok(p) = Params::new(mode, GUARDS[g], pr, layers) {
                    return p;
                }
            }
        })
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 64, ..ProptestConfig::default() })]

    /// Mapper → demapper hard decisions are the identity (with small noise).
    #[test]
    fn prop_map_demap_hard(m in 1usize..4, label in 0u8..64, nr in -0.02f32..0.02, ni in -0.02f32..0.02) {
        let m = MODS[m];
        let label = label % (1 << m.bits_per_symbol());
        let z = constellation_point(m, label) + C32::new(nr, ni);
        prop_assert_eq!(hard_label(m, z), label);
    }

    /// TMCC information round trip, with up to 8 bit errors in B20..B203.
    #[test]
    fn prop_tmcc_roundtrip(seed in any::<u64>(), errors in 0usize..=8, odd in any::<bool>(), diff in any::<bool>()) {
        let mut rng = Rng(seed);
        let info_bits: [u8; 102] = rng.bits(102).try_into().unwrap();
        let info = TmccInfo::from_bits(&info_bits);
        prop_assert_eq!(info.to_bits(), info_bits);
        let mut frame = info.frame_bits(odd, diff);
        let mut flipped = std::collections::HashSet::new();
        while flipped.len() < errors {
            flipped.insert(20 + (rng.next_u64() % 184) as usize);
        }
        for &p in &flipped { frame[p] ^= 1; }
        let d = decode_frame(&frame);
        prop_assert!(d.parity.is_ok());
        prop_assert_eq!(d.info, info);
        prop_assert_eq!(d.sync_odd, Some(odd));
    }

    /// Seismic AC word round trip, with up to 8 bit errors in B17..B203.
    #[test]
    fn prop_seismic_roundtrip(seed in any::<u64>(), errors in 0usize..=8, odd in any::<bool>()) {
        let mut rng = Rng(seed);
        let w = SeismicWarning {
            start_end: (rng.next_u64() % 4) as u8,
            update: (rng.next_u64() % 4) as u8,
            signal_id: (rng.next_u64() % 8) as u8,
            detail: rng.bits(88).try_into().unwrap(),
        };
        let mut bits = w.to_bits(odd);
        let mut flipped = std::collections::HashSet::new();
        while flipped.len() < errors {
            flipped.insert(17 + (rng.next_u64() % 187) as usize);
        }
        for &p in &flipped { bits[p] ^= 1; }
        let d = SeismicWarning::decode(&bits);
        prop_assert!(d.parity.is_ok());
        prop_assert!(d.crc_ok);
        prop_assert_eq!(d.warning, w);
    }

    /// Frequency interleaver: a permutation; deinterleave ∘ interleave = id.
    #[test]
    fn prop_freq_interleave_identity(p in arb_params(3), rel in any::<bool>()) {
        let numbering = if rel { RotationNumbering::GroupRelative } else { RotationNumbering::DataSegmentNumber };
        let fi = FrequencyInterleaver::with_numbering(&p, numbering);
        let n = fi.permutation().len();
        let mut seen = vec![false; n];
        for &q in fi.permutation() { prop_assert!(!seen[q as usize]); seen[q as usize] = true; }
        let x: Vec<u32> = (0..n as u32).collect();
        let mut y = vec![0; n];
        let mut z = vec![0; n];
        fi.interleave(&x, &mut y);
        fi.deinterleave(&y, &mut z);
        prop_assert_eq!(x, z);
        // Segments never leave their group: partial reception stays in segment 0.
        let nc = p.mode().data_carriers_per_segment();
        if p.partial_reception() {
            for q in 0..nc { prop_assert!((fi.permutation()[q] as usize) < nc); }
        }
    }

    /// AC serial mapping round trip (Attachment §6.3.4.2 order).
    #[test]
    fn prop_ac_stream_roundtrip(p in arb_params(0), seed in any::<u64>()) {
        let lay = FrameLayout::from_params(&p);
        let mut rng = Rng(seed);
        let bits = rng.bits(lay.ac.len() * 203);
        let f = AcFrame::from_stream(&lay, &bits);
        prop_assert_eq!(f.to_stream(), bits);
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 12, ..ProptestConfig::default() })]

    /// Bit interleaver (with Table 3-9 adjustment) + mapping → (differential
    /// detection) → demapping → bit deinterleaver = delay of exactly 2 OFDM
    /// symbols (§3.9.2).
    #[test]
    fn prop_bit_interleave_identity(m in 0usize..4, mode in 0usize..3, n in 1usize..=13, seed in any::<u64>()) {
        let m = MODS[m];
        let mode = MODES[mode];
        let layer = LayerConfig { segments: Segments::ALL[n - 1], modulation: m, code_rate: CodeRate::R1_2, time_interleave: TimeInterleave::K0 };
        let mut mapper = CarrierMapper::new(mode, &layer);
        let per = mapper.symbols_per_ofdm_symbol();
        let s = m.bits_per_symbol();
        let mut rng = Rng(seed);
        let mut di = BitDeinterleaver::new(m);
        let dm = Demapper::new(m);
        let mut inputs = Vec::new();
        let mut outputs = Vec::new();
        // DQPSK: the start-up references of the mapper (pseudo-random).
        let mut prev: Vec<C32> =
            (0..per).map(|i| if m.is_differential() { mapper.dqpsk_reference(i) } else { C32::default() }).collect();
        let mut sym = vec![C32::default(); per];
        for _ in 0..5 {
            let bits = rng.bits(per * s);
            mapper.map_symbol(&bits, &mut sym);
            let mut llr = vec![0f32; per * s];
            for (i, z) in sym.iter().enumerate() {
                let (zz, w) = if m.is_differential() { dqpsk_differential(*z, prev[i], 0.1) } else { (*z, 1.0) };
                dm.demap(zz, w, &mut llr[i * s..(i + 1) * s]);
            }
            prev.copy_from_slice(&sym);
            di.process(&mut llr);
            inputs.push(bits);
            outputs.push(llr.iter().map(|&l| u8::from(l < 0.0)).collect::<Vec<u8>>());
        }
        for t in 2..5 {
            prop_assert_eq!(&outputs[t], &inputs[t - 2], "symbol {}", t);
        }
    }

    /// Time interleaver + deinterleaver = delay of exactly Table 3-12
    /// frames × 204 symbols on every carrier of each layer.
    #[test]
    fn prop_time_interleave_identity(p in arb_params(3), seed in any::<u64>()) {
        let nc = p.mode().data_carriers_per_segment();
        let mut ti = TimeInterleaver::new(&p, u32::MAX);
        let mut tdi = TimeDeinterleaver::new(&p, u32::MAX);
        let mut rng = Rng(seed);
        let max_f = Layer::ALL.iter().map(|&l| time_interleave_delayed_frames(p.mode(), p.time_interleave_length(l)).unwrap()).max().unwrap();
        let total = max_f * 204 + 3;
        let mut hist: Vec<Vec<u32>> = Vec::new();
        for t in 0..total {
            let x: Vec<u32> = (0..13 * nc).map(|_| rng.next_u64() as u32 & 0x7fff_ffff).collect();
            let mut y = x.clone();
            ti.process(&mut y);
            tdi.process(&mut y);
            hist.push(x);
            for (id, _) in p.active_layers() {
                let f = time_interleave_delayed_frames(p.mode(), p.time_interleave_length(id)).unwrap();
                let d = f * 204;
                let (first, cnt) = layer_segments(&p, id).unwrap();
                if t >= d {
                    prop_assert_eq!(&y[first * nc..(first + cnt) * nc], &hist[t - d][first * nc..(first + cnt) * nc]);
                }
            }
        }
    }

    /// Noise-free sample-domain loopback over random Mode 1/2 parameters
    /// (both rotation numberings): LLR hard decisions equal the input bits.
    #[test]
    fn prop_noiseless_loopback(p in arb_params(1), rel in any::<bool>(), seed in any::<u64>()) {
        prop_assume!(p.mode() != Mode::M3);
        let numbering = if rel { RotationNumbering::GroupRelative } else { RotationNumbering::DataSegmentNumber };
        let mut m = Modulator::with_numbering(p.clone(), numbering);
        let tmcc = TmccInfo::from_params(&p);
        let ac = AcFrame::stuffing(m.layout());
        let lat = end_to_end_frames(&p);
        let mut rng = Rng(seed);
        let mut inputs = Vec::new();
        let mut rx = Demodulator::new(DemodConfig { rotation: numbering, ..DemodConfig::default() });
        let mut frames = Vec::new();
        for _ in 0..lat + 3 {
            let bits = Layer::ALL.map(|l| rng.bits(m.bits_per_frame(l)));
            let s = m.modulate_frame([&bits[0], &bits[1], &bits[2]], &tmcc, &ac).unwrap();
            frames.extend(rx.push(&s));
            inputs.push(bits);
        }
        let mut checked = 0;
        for f in &frames {
            prop_assert_eq!(&f.params, &p);
            prop_assert_eq!(f.tmcc.parity, dsc::ParityStatus::Ok);
            let idx = (f.start_sample / p.samples_per_frame() as u64) as usize;
            if !f.valid || idx < lat { continue; }
            for l in Layer::ALL {
                if let Some(llr) = &f.llr[l.index()] {
                    // The very first transmitted symbol has no DQPSK reference
                    // on air: its differential bits come out as erasures (0).
                    let first_dqpsk = idx == lat
                        && p.layers()[l.index()].is_some_and(|c| c.modulation.is_differential());
                    let input = &inputs[idx - lat][l.index()];
                    let mut erasures = 0;
                    for (x, &b) in llr.iter().zip(input) {
                        if *x == 0.0 && first_dqpsk {
                            erasures += 1;
                        } else {
                            prop_assert_eq!(u8::from(*x < 0.0), b, "layer {} frame {}", l, idx);
                        }
                    }
                    prop_assert!(erasures <= llr.len() / 204, "{} erasures", erasures);
                }
            }
            checked += 1;
        }
        prop_assert!(checked >= 1);
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 10, ..ProptestConfig::default() })]

    /// Frame-alignment contract (B31 Part 2 Figs. 3-8/3-9, `latency.rs`):
    /// on the modulator's carrier output, after differential detection
    /// (DQPSK), frequency/time deinterleaving, demapping and bit
    /// deinterleaving — but without the receiver's alignment FIFO — symbol
    /// `s` of on-air frame `m` carries symbol `s` of input frame `m − F_l`
    /// for every layer; in particular bit 0 of input frame `k` is b0 of the
    /// first carrier symbol of on-air frame `k + F_l`, symbol 0.  The first
    /// emitted frame is on-air frame −1.
    #[test]
    fn prop_bit_frame_alignment(p in arb_params(3), seed in any::<u64>()) {
        use isdbt_ofdm::combine::split;
        use isdbt_ofdm::latency::ti_frames;
        let mut m = Modulator::new(p.clone());
        prop_assert_eq!(m.onair_frame_index(), -1);
        let tmcc = TmccInfo::from_params(&p);
        let ac = AcFrame::stuffing(m.layout());
        let layout = m.layout().clone();
        let nc = p.mode().data_carriers_per_segment();
        let fi = FrequencyInterleaver::new(&p);
        let mut tdi = TimeDeinterleaver::new(&p, (C32::default(), 0f32));
        let mut bdi: Vec<Option<(BitDeinterleaver, Demapper)>> =
            p.layers().iter().map(|l| l.map(|l| (BitDeinterleaver::new(l.modulation), Demapper::new(l.modulation)))).collect();
        let max_f = Layer::ALL.iter().map(|&l| ti_frames(&p, l)).max().unwrap();
        let mut rng = Rng(seed);
        let mut inputs: Vec<[Vec<u8>; 3]> = Vec::new();
        let mut prev: Option<Vec<C32>> = None;
        let mut checked = [0usize; 3];
        for e in 0..max_f + 3 {
            let bits = Layer::ALL.map(|l| rng.bits(m.bits_per_frame(l)));
            let sy = m.modulate_frame_carriers([&bits[0], &bits[1], &bits[2]], &tmcc, &ac).unwrap();
            inputs.push(bits);
            let onair = e as i64 - 1;
            for (n, y) in sy.iter().enumerate() {
                let mut data = vec![(C32::default(), 0f32); 13 * nc];
                for (q, &k) in layout.data[n % 4].iter().enumerate() {
                    let k = k as usize;
                    data[q] = if p.segment_is_differential(q / nc) {
                        match &prev { Some(pr) => dqpsk_differential(y[k], pr[k], 0.5), None => (C32::default(), 0.0) }
                    } else {
                        (y[k], 1.0)
                    };
                }
                prev = Some(y.clone());
                let mut deint = vec![(C32::default(), 0f32); 13 * nc];
                fi.deinterleave(&data, &mut deint);
                tdi.process(&mut deint);
                let parts = split(&p, &deint);
                for l in Layer::ALL {
                    let (Some(part), Some((bd, dm))) = (parts[l.index()], bdi[l.index()].as_mut()) else { continue };
                    let s = dm.modulation().bits_per_symbol();
                    let mut llr = vec![0f32; part.len() * s];
                    for (i, &(z, w)) in part.iter().enumerate() {
                        dm.demap(z, w, &mut llr[i * s..(i + 1) * s]);
                    }
                    bd.process(&mut llr);
                    let src = onair - ti_frames(&p, l) as i64;
                    if src >= 0 && (src as usize) < inputs.len() {
                        let per = llr.len();
                        let want = &inputs[src as usize][l.index()][n * per..(n + 1) * per];
                        let hard: Vec<u8> = llr.iter().map(|&x| u8::from(x < 0.0)).collect();
                        prop_assert!(hard == want, "layer {} on-air frame {} symbol {}", l, onair, n);
                        checked[l.index()] += 1;
                    }
                }
            }
        }
        for (id, _) in p.active_layers() {
            prop_assert!(checked[id.index()] >= 204, "layer {} checked {}", id, checked[id.index()]);
        }
    }
}

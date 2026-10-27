//! Spec-vector tests: values printed in ARIB STD-B31 v2.2, asserted
//! directly (tables and figures cited per test).  Figure transcriptions
//! were read from the PDF page images (pdf pages 39–42, 45–51, 63).

mod common;

use isdbt_core::ofdm_params::{segment_carrier_counts, segment_position, time_interleave_m};
use isdbt_core::{
    CodeRate, GuardInterval, LayerConfig, Mode, Modulation, Params, SEGMENTS, Segments,
    TimeInterleave,
};
use isdbt_ofdm::ac::{AcFrame, SeismicDetail, SeismicPage, SeismicWarning, crc10};
use isdbt_ofdm::dsc;
use isdbt_ofdm::frame::{
    AcKind, CarrierRole, FrameLayout, SegmentKind, edge_w, is_sp, pilot_value, prbs_init,
    prbs_segment_init, prbs_step, segment_roles,
};
use isdbt_ofdm::freq_interleave::{FrequencyInterleaver, randomize_table};
use isdbt_ofdm::mapping::{
    CarrierMapper, constellation_point, constellation_point_raw, dqpsk_phase, normalization,
};
use isdbt_ofdm::tables;
use isdbt_ofdm::tmcc::{
    InterleaveCode, ModulationCode, RateCode, SegmentsCode, TmccInfo, TmccLayer, W0, decode_frame,
    sync_word,
};
use isdbt_ofdm::{C32, Modulator};

const MODES: [Mode; 3] = [Mode::M1, Mode::M2, Mode::M3];

fn label(s: &str) -> u8 {
    s.bytes()
        .filter(|b| *b == b'0' || *b == b'1')
        .fold(0, |a, b| (a << 1) | (b - b'0'))
}

/// Fig. 3-15: QPSK constellation (b0 → I, b1 → Q).
#[test]
fn spec_fig_3_15_qpsk() {
    for (l, i, q) in [("00", 1, 1), ("10", -1, 1), ("11", -1, -1), ("01", 1, -1)] {
        assert_eq!(
            constellation_point_raw(Modulation::Qpsk, label(l)),
            (i, q),
            "{l}"
        );
    }
}

/// Fig. 3-17: 16QAM constellation, rows Q = +3..−3, columns I = −3..+3.
#[test]
fn spec_fig_3_17_16qam() {
    let rows = [
        ["1000", "1010", "0010", "0000"],
        ["1001", "1011", "0011", "0001"],
        ["1101", "1111", "0111", "0101"],
        ["1100", "1110", "0110", "0100"],
    ];
    let lv = [3, 1, -1, -3];
    let mut seen = std::collections::HashSet::new();
    for (r, row) in rows.iter().enumerate() {
        for (c, l) in row.iter().enumerate() {
            assert_eq!(
                constellation_point_raw(Modulation::Qam16, label(l)),
                (lv[3 - c], lv[r]),
                "{l}"
            );
            seen.insert(label(l));
        }
    }
    assert_eq!(seen.len(), 16);
}

/// Fig. 3-19: 64QAM constellation, rows Q = +7..−7, columns I = −7..+7.
#[test]
fn spec_fig_3_19_64qam() {
    let rows = [
        "100000 100010 101010 101000 001000 001010 000010 000000",
        "100001 100011 101011 101001 001001 001011 000011 000001",
        "100101 100111 101111 101101 001101 001111 000111 000101",
        "100100 100110 101110 101100 001100 001110 000110 000100",
        "110100 110110 111110 111100 011100 011110 010110 010100",
        "110101 110111 111111 111101 011101 011111 010111 010101",
        "110001 110011 111011 111001 011001 011011 010011 010001",
        "110000 110010 111010 111000 011000 011010 010010 010000",
    ];
    let lv = [7, 5, 3, 1, -1, -3, -5, -7];
    let mut seen = std::collections::HashSet::new();
    for (r, row) in rows.iter().enumerate() {
        for (c, l) in row.split_whitespace().enumerate() {
            assert_eq!(
                constellation_point_raw(Modulation::Qam64, label(l)),
                (lv[7 - c], lv[r]),
                "{l}"
            );
            seen.insert(label(l));
        }
    }
    assert_eq!(seen.len(), 64);
}

/// Table 3-11: normalisation gives unit average power.
#[test]
fn spec_table_3_11_unit_power() {
    for m in [Modulation::Qpsk, Modulation::Qam16, Modulation::Qam64] {
        let n = 1u32 << m.bits_per_symbol();
        let p: f32 = (0..n)
            .map(|l| constellation_point(m, l as u8).norm_sqr())
            .sum::<f32>()
            / n as f32;
        assert!((p - 1.0).abs() < 1e-6, "{m:?} {p}");
    }
    // DQPSK points of Fig. 3-13 have |Z| = √2 → 1 after Z/√2.
    assert!((2f32.sqrt() * normalization(Modulation::Dqpsk) - 1.0).abs() < 1e-6);
}

/// Table 3-10 and Fig. 3-13: π/4-shift DQPSK phases and constellation.
#[test]
fn spec_table_3_10_dqpsk() {
    use std::f32::consts::FRAC_PI_4;
    assert_eq!(dqpsk_phase(0, 0), FRAC_PI_4);
    assert_eq!(dqpsk_phase(0, 1), -FRAC_PI_4);
    assert_eq!(dqpsk_phase(1, 0), 3.0 * FRAC_PI_4);
    assert_eq!(dqpsk_phase(1, 1), -3.0 * FRAC_PI_4);
    // Recursion (Ij,Qj) = R(θj)(Ij−1,Qj−1) on one carrier: every output is a
    // Fig. 3-13 point and consecutive phases differ by θj.
    let layer = LayerConfig {
        segments: Segments::new(1).unwrap(),
        modulation: Modulation::Dqpsk,
        code_rate: CodeRate::R1_2,
        time_interleave: TimeInterleave::K0,
    };
    let mut m = CarrierMapper::new(Mode::M1, &layer);
    let mut rng = common::Rng(5);
    let r2 = 2f32.sqrt();
    let fig: Vec<(f32, f32)> = vec![
        (1., 1.),
        (1., -1.),
        (-1., 1.),
        (-1., -1.),
        (r2, 0.),
        (-r2, 0.),
        (0., r2),
        (0., -r2),
    ];
    let mut out = vec![C32::default(); 96];
    for _ in 0..10 {
        m.map_symbol(&rng.bits(192), &mut out);
        for z in &out {
            let z = z / normalization(Modulation::Dqpsk);
            assert!(
                fig.iter()
                    .any(|&(i, q)| (z.re - i).abs() < 1e-5 && (z.im - q).abs() < 1e-5),
                "{z}"
            );
        }
    }
}

/// Table 3-13: each table is a permutation; spot checks read from the PDF
/// page images (pages 41–42).
#[test]
fn spec_table_3_13_permutations() {
    for mode in MODES {
        let t = randomize_table(mode);
        let mut s: Vec<u16> = t.to_vec();
        s.sort_unstable();
        assert!(
            s.iter().enumerate().all(|(i, &v)| v as usize == i),
            "{mode:?}"
        );
        assert_eq!(t.len(), mode.data_carriers_per_segment());
    }
    let m1 = randomize_table(Mode::M1);
    for (b, a) in [(0, 80), (23, 13), (32, 0), (47, 36), (71, 84), (95, 24)] {
        assert_eq!(m1[b], a);
    }
    let m2 = randomize_table(Mode::M2);
    for (b, a) in [(3, 116), (9, 168), (78, 0), (120, 1), (167, 103), (191, 59)] {
        assert_eq!(m2[b], a);
    }
    let m3 = randomize_table(Mode::M3);
    for (b, a) in [
        (0, 62),
        (2, 371),
        (137, 3),
        (146, 383),
        (249, 0),
        (272, 1),
        (383, 51),
    ] {
        assert_eq!(m3[b], a);
    }
}

/// Fig. 3-28 (b): Mode 1, segment 0, I = 8 — number of delayed symbols per
/// carrier after time interleaving and carrier randomising.  Read off the
/// plot: carrier 0 ≈ 515, carrier 1 ≈ 270, carrier 80 = 0.  This fixes the
/// direction of Table 3-13 (data at "before" i goes to carrier "after" T[i]).
#[test]
fn spec_fig_3_28b_randomize_direction() {
    let t = randomize_table(Mode::M1);
    let delay_at_carrier = |c: u16| {
        let i = t.iter().position(|&a| a == c).unwrap();
        8 * time_interleave_m(i)
    };
    assert_eq!(delay_at_carrier(0), 512);
    assert_eq!(delay_at_carrier(1), 272);
    assert_eq!(delay_at_carrier(80), 0);
    // Fig. 3-24 (Mode 1, I = 8): carrier 19 → 760, carrier 20 → 32.
    assert_eq!(8 * time_interleave_m(19), 760);
    assert_eq!(8 * time_interleave_m(20), 32);
}

fn params(mode: Mode, pr: bool, layers: [Option<(u8, Modulation)>; 3]) -> Params {
    let l = layers.map(|o| {
        o.map(|(segments, modulation)| LayerConfig {
            segments: Segments::new(segments).unwrap(),
            modulation,
            code_rate: CodeRate::R1_2,
            time_interleave: TimeInterleave::K0,
        })
    });
    Params::new(mode, GuardInterval::G1_8, pr, l).unwrap()
}

/// Figs. 3-26/3-27 + Table 3-13 composed, hand-derived from the figures.
#[test]
fn spec_fig_3_26_3_27_frequency_interleave() {
    // 13 coherent segments, Mode 1: S1 = (seg 0, i 1) → inter-segment
    // (n = 13): segment 1, position 0 → rotation by 1: position 95 →
    // randomising: carrier T[95] = 24 of data segment 1.
    let p = params(Mode::M1, false, [Some((13, Modulation::Qpsk)), None, None]);
    let fi = FrequencyInterleaver::new(&p);
    assert_eq!(fi.permutation()[0], 80);
    assert_eq!(fi.permutation()[1], 96 + 24);
    // S13 = (seg 0, i 13) → segment 0, position 1 → rotation 0 → T[1] = 93.
    assert_eq!(fi.permutation()[13], 93);
    // Partial reception: segment 0 only rotated (k = 0) and randomised.
    let p = params(
        Mode::M1,
        true,
        [
            Some((1, Modulation::Qpsk)),
            Some((12, Modulation::Qam64)),
            None,
        ],
    );
    let fi = FrequencyInterleaver::new(&p);
    assert_eq!(fi.permutation()[1], 93);
    // Coherent group = data segments 1..12 (n = 12): S'_0 of the group
    // (segment 1, i 0) → group segment 0 = data segment 1, position 0 →
    // rotation by data-segment number 1 → position 95 → T[95] = 24.
    assert_eq!(fi.permutation()[96], 96 + 24);
}

/// Table 3-1 / 3-2: carrier counts per segment and in the band.
#[test]
fn spec_table_3_1_carrier_counts() {
    for mode in MODES {
        let k = mode.total_carriers();
        assert_eq!(k, [1405, 2809, 5617][mode.number() as usize - 1]);
        for kind in [SegmentKind::Coherent, SegmentKind::Differential] {
            let want = segment_carrier_counts(mode, kind == SegmentKind::Differential);
            for seg in 0..SEGMENTS {
                for phase in 0..4 {
                    let r = segment_roles(mode, seg, kind, phase);
                    let count =
                        |f: &dyn Fn(&CarrierRole) -> bool| r.iter().filter(|x| f(x)).count();
                    assert_eq!(
                        count(&|x| *x == CarrierRole::Data),
                        want.data,
                        "{mode:?} {seg} {kind:?}"
                    );
                    assert_eq!(count(&|x| *x == CarrierRole::Sp), want.sp);
                    assert_eq!(count(&|x| *x == CarrierRole::Cp), want.cp);
                    assert_eq!(count(&|x| *x == CarrierRole::Tmcc), want.tmcc);
                    assert_eq!(count(&|x| *x == CarrierRole::Ac(AcKind::Ac1)), want.ac1);
                    assert_eq!(count(&|x| *x == CarrierRole::Ac(AcKind::Ac2)), want.ac2);
                }
            }
        }
        // Expected per-mode values of Table 3-1.
        let s = mode.scale();
        let c = segment_carrier_counts(mode, false);
        assert_eq!((c.data, c.sp, c.tmcc, c.ac1), (96 * s, 9 * s, s, 2 * s));
        let d = segment_carrier_counts(mode, true);
        assert_eq!(
            (d.cp, d.tmcc, d.ac2),
            (1, 5 * s, [4, 9, 19][mode.number() as usize - 1])
        );
        // Whole band, mixed kinds (Table 3-2): nd = 5 differential segments.
        let p = params(
            mode,
            false,
            [
                Some((5, Modulation::Dqpsk)),
                Some((8, Modulation::Qam16)),
                None,
            ],
        );
        let lay = FrameLayout::from_params(&p);
        let (ns, nd) = (8, 5);
        assert_eq!(lay.w.len(), k);
        assert_eq!(lay.sp[1].len(), 9 * s * ns);
        assert_eq!(lay.cp.len() + 1, nd + 1);
        assert_eq!(lay.tmcc.len(), s * ns + 5 * s * nd);
        assert_eq!(
            lay.ac.iter().filter(|a| a.kind == AcKind::Ac1).count(),
            2 * s * 13
        );
        assert_eq!(
            lay.ac.iter().filter(|a| a.kind == AcKind::Ac2).count(),
            d.ac2 * nd
        );
        let roles = lay.roles(2);
        assert_eq!(
            roles.iter().filter(|r| **r == CarrierRole::Data).count(),
            96 * s * 13
        );
        assert_eq!(roles[k - 1], CarrierRole::EdgeCp);
    }
}

/// Tables 3-14 vs 3-15: AC1 positions identical for both segment kinds
/// (§3.12.1 "AC1 carriers … arranged at the same positions"), and spot
/// checks of both tables against the PDF images (pages 45–49).
#[test]
fn spec_tables_3_14_3_15() {
    assert_eq!(tables::DIFF_AC1_M1, tables::COH_AC1_M1);
    assert_eq!(tables::DIFF_AC1_M2, tables::COH_AC1_M2);
    assert_eq!(tables::DIFF_AC1_M3, tables::COH_AC1_M3);
    assert_eq!(tables::DIFF_TMCC_M1[0], [49, 61, 96, 99, 104]);
    assert_eq!(tables::DIFF_AC2_M1[11], [3, 45, 59, 77]);
    assert_eq!(tables::DIFF_AC2_M2[3][8], 213);
    assert_eq!(tables::DIFF_TMCC_M3[12][19], 429);
    assert_eq!(tables::DIFF_AC2_M3[1][18], 398);
    assert_eq!(tables::DIFF_AC2_M3[0][9], 216);
    assert_eq!(
        tables::COH_TMCC_M1,
        [
            [49],
            [47],
            [31],
            [44],
            [83],
            [86],
            [61],
            [17],
            [85],
            [25],
            [101],
            [70],
            [23]
        ]
    );
    assert_eq!(tables::COH_TMCC_M3[0], [101, 131, 286, 349]);
    assert_eq!(tables::COH_AC1_M3[6][7], 352);
    assert_eq!(tables::COH_TMCC_M2[10], [61, 193]);
}

/// Fig. 3-30: SP every 12 carriers shifting by 3 per symbol; data carriers
/// fill the rest in ascending order. Fig. 3-29: CP at carrier 0.
#[test]
fn spec_fig_3_29_3_30_segment_structure() {
    assert!(
        is_sp(0, 0) && is_sp(12, 0) && is_sp(3, 1) && is_sp(6, 2) && is_sp(9, 3) && is_sp(0, 4)
    );
    assert!(!is_sp(3, 0) && !is_sp(0, 1));
    let p = params(Mode::M1, false, [Some((13, Modulation::Qpsk)), None, None]);
    let lay = FrameLayout::from_params(&p);
    // Segment 11 is at the lowest frequency (k = c).
    let seg11 = 11 * 96;
    assert_eq!(lay.data[0][seg11], 1); // S0,0 at carrier 1
    assert_eq!(lay.data[1][seg11], 0); // S0,1 at carrier 0
    assert_eq!(lay.data[1][seg11 + 3], 4); // S3,1 at carrier 4 (SP at 3)
    let p = params(Mode::M1, false, [Some((13, Modulation::Dqpsk)), None, None]);
    let lay = FrameLayout::from_params(&p);
    assert_eq!(lay.cp[0], 0);
    assert_eq!(lay.data[0][seg11], 1);
}

/// §3.13.1, Fig. 3-31, Table 3-16 note: the per-segment initial values
/// equal the register state reached by running the PRBS from all ones
/// across the whole band from carrier 0 of segment 11; Table 3-19 gives
/// the value for the rightmost carrier.
#[test]
fn spec_table_3_16_prbs_initial_values() {
    // First outputs from all-ones: the eleven register 1s, then eight
    // feedback bits D9⊕D11 = 0 (steps 0–8), then 1 once the first fed-back
    // 0 has reached D9 (step 9).
    let mut st = [1u8; 11];
    let first: Vec<u8> = (0..21).map(|_| prbs_step(&mut st)).collect();
    assert_eq!(
        first,
        [
            1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1
        ]
    );
    for mode in MODES {
        let c = mode.carriers_per_segment();
        let mut st = [1u8; 11];
        let mut w_all = Vec::new();
        for pos in 0..13 {
            let seg = isdbt_core::ofdm_params::segment_at_position(pos);
            assert_eq!(st, prbs_segment_init(mode, seg), "{mode:?} segment {seg}");
            for _ in 0..c {
                w_all.push(prbs_step(&mut st));
            }
        }
        // Table 3-19: rightmost carrier.
        let w_edge = prbs_step(&mut st);
        assert_eq!(w_edge, edge_w(mode), "{mode:?} edge");
        let p = params(mode, false, [Some((13, Modulation::Qpsk)), None, None]);
        let lay = FrameLayout::from_params(&p);
        assert_eq!(&lay.w[..13 * c], &w_all[..]);
    }
    assert_eq!(prbs_init(tables::PRBS_INIT_M1[9]), prbs_init("11011001111"));
    assert_eq!(
        prbs_init(tables::PRBS_INIT_M3[12]),
        prbs_init("00010011100")
    );
    // Table 3-17 / 3-19 amplitudes.
    assert_eq!(pilot_value(1), C32::new(-4.0 / 3.0, 0.0));
    assert_eq!(pilot_value(0), C32::new(4.0 / 3.0, 0.0));
    assert_eq!(
        [edge_w(Mode::M1), edge_w(Mode::M2), edge_w(Mode::M3)],
        [1, 0, 0]
    );
    let _ = segment_position(0);
}

/// §3.15.4, Table 3-21: w0/w1 and their alternation; §3.15.5 type bits;
/// decoded from the modulator's TMCC carriers (DBPSK, §3.13.3).
#[test]
fn spec_table_3_21_sync_alternation() {
    assert_eq!(W0, [0, 0, 1, 1, 0, 1, 0, 1, 1, 1, 1, 0, 1, 1, 1, 0]);
    assert_eq!(
        sync_word(true),
        [1, 1, 0, 0, 1, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, 1]
    );
    let p = params(
        Mode::M1,
        true,
        [
            Some((1, Modulation::Dqpsk)),
            Some((12, Modulation::Qam64)),
            None,
        ],
    );
    let mut m = Modulator::new(p.clone());
    let tmcc = TmccInfo::from_params(&p);
    let ac = AcFrame::stuffing(m.layout());
    let zero: Vec<Vec<u8>> = isdbt_core::Layer::ALL
        .iter()
        .map(|&l| vec![0; m.bits_per_frame(l)])
        .collect();
    for frame in 0..4 {
        let sy = m
            .modulate_frame_carriers([&zero[0], &zero[1], &zero[2]], &tmcc, &ac)
            .unwrap();
        for t in m.layout().tmcc.clone() {
            let k = t.k as usize;
            // B'0 = W (reference).
            assert_eq!(sy[0][k], pilot_value(m.layout().w[k]));
            let mut bits = [0u8; 204];
            for n in 1..204 {
                bits[n] = u8::from((sy[n][k] * sy[n - 1][k].conj()).re < 0.0);
            }
            let d = decode_frame(&bits);
            assert_eq!(d.sync_odd, Some(frame % 2 == 1));
            assert_eq!(
                d.segment_type,
                if t.kind == SegmentKind::Differential {
                    [1, 1, 1]
                } else {
                    [0, 0, 0]
                }
            );
            assert_eq!(d.parity, dsc::ParityStatus::Ok);
            assert_eq!(d.info, tmcc);
            assert_eq!(d.info.params(p.mode(), p.guard()).unwrap(), p);
        }
        // AC stuffing: every information bit 1 → the carrier flips every symbol.
        for a in &m.layout().ac.clone() {
            let k = a.k as usize;
            assert_eq!(sy[0][k], pilot_value(m.layout().w[k]));
            for n in 1..204 {
                assert!((sy[n][k] + sy[n - 1][k]).norm() < 1e-6);
            }
        }
    }
}

/// Table 3-22..3-31: field placement of the TMCC information.
#[test]
fn spec_table_3_22_tmcc_fields() {
    let p = params(
        Mode::M3,
        true,
        [
            Some((1, Modulation::Qpsk)),
            Some((12, Modulation::Qam64)),
            None,
        ],
    );
    let info = TmccInfo::from_params(&p);
    let b = info.frame_bits(false, false);
    let field = |a: usize, z: usize| {
        b[a..=z]
            .iter()
            .fold(String::new(), |s, &x| s + if x == 1 { "1" } else { "0" })
    };
    assert_eq!(field(20, 21), "00"); // television
    assert_eq!(field(22, 25), "1111"); // no switching
    assert_eq!(field(26, 26), "0");
    assert_eq!(field(27, 27), "1"); // partial reception
    assert_eq!(field(28, 30), "001"); // A: QPSK
    assert_eq!(field(31, 33), "000"); // A: 1/2 (helper uses R1_2)
    assert_eq!(field(34, 36), "000");
    assert_eq!(field(37, 40), "0001"); // 1 segment
    assert_eq!(field(41, 43), "011"); // B: 64QAM
    assert_eq!(field(50, 53), "1100"); // 12 segments
    assert_eq!(field(54, 66), "1111111111111"); // C unused
    assert_eq!(field(107, 121), "111111111111111");
    assert_eq!(field(1, 16), "0011010111101110");
    assert_eq!(field(17, 19), "000");
}

/// The reserved payloads can only be decoded, never spelled: a table row
/// whose expectation is `None` must decode to `Reserved` carrying the code.
macro_rules! assert_decodes {
    ($enum:ident, $code:expr, $want:expr) => {{
        let got = $enum::from_code($code);
        match $want {
            Some(w) => assert_eq!(got, w, "{:b}", $code),
            None => assert!(
                matches!(got, $enum::Reserved(r) if r.get() == $code),
                "{:b}: {got:?}",
                $code
            ),
        }
    }};
}

/// Table 3-28: carrier modulation codes '000'–'011', '100'–'110' reserved,
/// '111' unused; every 3-bit code round-trips.
#[test]
fn spec_table_3_28_modulation_codes() {
    use ModulationCode::{Assigned, Unused};
    for (code, want) in [
        ("000", Some(Assigned(Modulation::Dqpsk))),
        ("001", Some(Assigned(Modulation::Qpsk))),
        ("010", Some(Assigned(Modulation::Qam16))),
        ("011", Some(Assigned(Modulation::Qam64))),
        ("100", None),
        ("101", None),
        ("110", None),
        ("111", Some(Unused)),
    ] {
        assert_decodes!(ModulationCode, label(code), want);
    }
    for c in 0..=7u8 {
        assert_eq!(ModulationCode::from_code(c).code(), c, "{c:03b}");
        assert_eq!(
            ModulationCode::from_code(c | 0b1000),
            ModulationCode::from_code(c)
        );
    }
    assert_eq!(TmccLayer::UNUSED.modulation, Unused);
}

/// Table 3-29: coding-rate codes '000'–'100', '101'–'110' reserved, '111'
/// unused; every 3-bit code round-trips.
#[test]
fn spec_table_3_29_code_rate_codes() {
    use RateCode::{Assigned, Unused};
    for (code, want) in [
        ("000", Some(Assigned(CodeRate::R1_2))),
        ("001", Some(Assigned(CodeRate::R2_3))),
        ("010", Some(Assigned(CodeRate::R3_4))),
        ("011", Some(Assigned(CodeRate::R5_6))),
        ("100", Some(Assigned(CodeRate::R7_8))),
        ("101", None),
        ("110", None),
        ("111", Some(Unused)),
    ] {
        assert_decodes!(RateCode, label(code), want);
    }
    for c in 0..=7u8 {
        assert_eq!(RateCode::from_code(c).code(), c, "{c:03b}");
        assert_eq!(RateCode::from_code(c | 0b1000), RateCode::from_code(c));
    }
    assert_eq!(TmccLayer::UNUSED.code_rate, Unused);
}

/// Table 3-30: time-interleaving codes '000'–'011' are the Table 3-12
/// indices, '100' "not used" (terrestrial sound), '101'–'110' reserved,
/// '111' unused; every 3-bit code round-trips.
#[test]
fn spec_table_3_30_interleave_codes() {
    use InterleaveCode::{Assigned, Unused};
    for (code, want) in [
        ("000", Some(Assigned(TimeInterleave::K0))),
        ("001", Some(Assigned(TimeInterleave::K1))),
        ("010", Some(Assigned(TimeInterleave::K2))),
        ("011", Some(Assigned(TimeInterleave::K3))),
        ("100", None),
        ("101", None),
        ("110", None),
        ("111", Some(Unused)),
    ] {
        assert_decodes!(InterleaveCode, label(code), want);
    }
    // The "Meaning (value I)" column is Table 3-12 / Table 3-2.
    for (t, i) in TimeInterleave::ALL
        .iter()
        .zip([[0, 0, 0], [4, 2, 1], [8, 4, 2], [16, 8, 4]])
    {
        for (mode, want) in MODES.iter().zip(i) {
            assert_eq!(t.length(*mode), want, "{t:?} {mode:?}");
        }
    }
    for c in 0..=7u8 {
        assert_eq!(InterleaveCode::from_code(c).code(), c, "{c:03b}");
        assert_eq!(
            InterleaveCode::from_code(c | 0b1000),
            InterleaveCode::from_code(c)
        );
    }
    assert_eq!(TmccLayer::UNUSED.interleave, Unused);
}

/// Table 3-31: segment-count codes '0001'–'1101' = 1..=13 segments,
/// '0000' and '1110' reserved, '1111' unused; every 4-bit code round-trips.
#[test]
fn spec_table_3_31_segment_codes() {
    use SegmentsCode::Unused;
    assert_decodes!(SegmentsCode, label("0000"), None);
    for n in Segments::ALL {
        assert_eq!(
            SegmentsCode::from_code(n.get()),
            SegmentsCode::Segments(n),
            "{:04b}",
            n.get()
        );
    }
    assert_eq!(
        SegmentsCode::from_code(label("1101")),
        SegmentsCode::Segments(Segments::MAX)
    );
    assert_decodes!(SegmentsCode, label("1110"), None);
    assert_eq!(SegmentsCode::from_code(label("1111")), Unused);
    for c in 0..=15u8 {
        assert_eq!(SegmentsCode::from_code(c).code(), c, "{c:04b}");
        assert_eq!(
            SegmentsCode::from_code(c | 0b1_0000),
            SegmentsCode::from_code(c)
        );
    }
    assert_eq!(TmccLayer::UNUSED.segments, Unused);
    assert!(TmccLayer::UNUSED.is_unused());
}

/// Tables 3-28..3-31: `from_code` is total and masks to the field width,
/// so every `u8` decodes to the canonical value of its low bits (the
/// `Reserved` payloads can only come from `from_code`, so `==` is wire
/// equality).
#[test]
fn prop_layer_codes_from_code_mask_to_width() {
    for c in 0..=255u8 {
        assert_eq!(
            ModulationCode::from_code(c),
            ModulationCode::from_code(c & 0b111)
        );
        assert_eq!(ModulationCode::from_code(c).code(), c & 0b111);
        assert_eq!(RateCode::from_code(c), RateCode::from_code(c & 0b111));
        assert_eq!(RateCode::from_code(c).code(), c & 0b111);
        assert_eq!(
            InterleaveCode::from_code(c),
            InterleaveCode::from_code(c & 0b111)
        );
        assert_eq!(InterleaveCode::from_code(c).code(), c & 0b111);
        assert_eq!(
            SegmentsCode::from_code(c),
            SegmentsCode::from_code(c & 0b1111)
        );
        assert_eq!(SegmentsCode::from_code(c).code(), c & 0b1111);
    }
}

/// Tables 3-28..3-31 in `TmccLayer::config`: the unused layer is `None`,
/// a reserved code in any field is an `Error::Tmcc` naming the field, and
/// a configured layer round-trips through its codes.
#[test]
fn spec_tables_3_28_to_3_31_layer_config() {
    let a = LayerConfig {
        segments: Segments::new(12).unwrap(),
        modulation: Modulation::Qam64,
        code_rate: CodeRate::R3_4,
        time_interleave: TimeInterleave::K2,
    };
    let coded = TmccLayer::from_config(&a);
    assert_eq!(coded.config().unwrap(), Some(a));
    assert_eq!(TmccLayer::UNUSED.config().unwrap(), None);
    let reserved = [
        (
            TmccLayer {
                modulation: ModulationCode::from_code(0b101),
                ..coded
            },
            "modulation",
        ),
        (
            TmccLayer {
                code_rate: RateCode::from_code(0b110),
                ..coded
            },
            "code-rate",
        ),
        (
            TmccLayer {
                interleave: InterleaveCode::from_code(0b100),
                ..coded
            },
            "interleave",
        ),
        (
            TmccLayer {
                segments: SegmentsCode::from_code(0b1110),
                ..coded
            },
            "segment count",
        ),
        (
            TmccLayer {
                segments: SegmentsCode::Unused,
                ..coded
            },
            "segment count",
        ),
    ];
    for (layer, field) in reserved {
        let err = layer.config().unwrap_err().to_string();
        assert!(err.contains(field), "{field}: {err}");
    }
}

/// §3.15.6.9: parity of the all-zero word is zero; information "…0001"
/// (B121 = 1) gives x^82 mod g(x) = g(x) − x^82.
#[test]
fn spec_3_15_6_9_parity_vectors() {
    assert_eq!(dsc::parity(&[0u8; 102]), [0u8; 82]);
    let mut info = [0u8; 102];
    info[101] = 1;
    let par = dsc::parity(&info);
    for (i, &b) in par.iter().enumerate() {
        let deg = 81 - i;
        assert_eq!(b == 1, dsc::G_EXPONENTS[1..].contains(&deg), "degree {deg}");
    }
    // Majority logic corrects 8 errors.
    let mut rng = common::Rng(9);
    let info = rng.bits(102);
    let mut word: Vec<u8> = info.iter().copied().chain(dsc::parity(&info)).collect();
    let orig = word.clone();
    for e in 0..8 {
        let pos = (rng.next_u64() % 184) as usize;
        if word[pos] == orig[pos] {
            word[pos] ^= 1;
        }
        let _ = e;
    }
    let st = dsc::decode(&mut word);
    assert!(matches!(st, dsc::ParityStatus::Corrected(_)), "{st:?}");
    assert_eq!(word, orig);
}

/// §3.16.6: AC seismic word layout, CRC (Fig. 3-35) and parity (§3.16.6.7).
#[test]
fn spec_3_16_6_seismic_word() {
    assert_eq!(crc10(&[0u8; 91]), [0u8; 10]);
    // Single 1 at the last position: remainder x^10 mod g = x^9+x^5+x^4+x+1.
    let mut v = [0u8; 91];
    v[90] = 1;
    assert_eq!(crc10(&v), [1, 0, 0, 0, 1, 1, 0, 0, 1, 1]);
    let w = SeismicWarning {
        start_end: 0,
        update: 1,
        signal_id: 0,
        detail: SeismicDetail::Warning {
            current_time: 0x1234_5678,
            page: SeismicPage::Areas(0x00ff_ffff_ffff_fffe),
        }
        .to_bits(),
    };
    for odd in [false, true] {
        let bits = w.to_bits(odd);
        // B1–B3 configuration identification = top of TMCC sync (001/110).
        assert_eq!(&bits[1..17], &sync_word(odd));
        assert_eq!(&bits[1..4], if odd { &[1, 1, 0] } else { &[0, 0, 1] });
        let d = SeismicWarning::decode(&bits);
        assert!(d.crc_ok);
        assert_eq!(d.parity, dsc::ParityStatus::Ok);
        assert_eq!(d.warning, w);
    }
    let na = SeismicWarning::not_available(0x5a5);
    assert_eq!(
        na.typed_detail(),
        Some(SeismicDetail::NotAvailable {
            broadcaster_id: 0x5a5
        })
    );
    assert!(na.detail[..32].iter().all(|&b| b == 1));
}

/// §3.14.2 / Table 3-1: samples per symbol and frame.
#[test]
fn spec_frame_sample_counts() {
    for mode in MODES {
        for gi in [
            GuardInterval::G1_4,
            GuardInterval::G1_8,
            GuardInterval::G1_16,
            GuardInterval::G1_32,
        ] {
            let p = Params::new(
                mode,
                gi,
                false,
                [
                    Some(LayerConfig {
                        segments: Segments::new(13).unwrap(),
                        modulation: Modulation::Qpsk,
                        code_rate: CodeRate::R1_2,
                        time_interleave: TimeInterleave::K0,
                    }),
                    None,
                    None,
                ],
            )
            .unwrap();
            let mut m = Modulator::new(p.clone());
            let bits = vec![0u8; m.bits_per_frame(isdbt_core::Layer::A)];
            let tmcc = TmccInfo::from_params(&p);
            let ac = AcFrame::stuffing(m.layout());
            let s = m.modulate_frame([&bits, &[], &[]], &tmcc, &ac).unwrap();
            assert_eq!(s.len(), 204 * (mode.fft_size() + gi.samples(mode)));
            // Guard interval = copy of the useful part's tail (§3.14.2).
            let n = mode.fft_size();
            let ng = gi.samples(mode);
            assert_eq!(&s[..ng], &s[n..n + ng]);
            let us = s.len() as f64 * 63.0 / 512.0;
            let want = [64.26e3, 57.834e3, 54.621e3, 53.0145e3][[4, 8, 16, 32]
                .iter()
                .position(|&d| d == gi.denominator())
                .unwrap()]
                * mode.scale() as f64;
            assert!((us - want).abs() < 1e-6, "{us} {want}");
        }
    }
}

//! Multiplex-frame pattern (ARIB STD-B31 §3.2) consistency over the
//! parameter space: slot count = Table 3-5 n, per-layer counts = Table 3-3,
//! model-receiver self-consistency (asserted inside `MultiplexPattern::new`),
//! Mode 2/3 sub-frame periodicity implied by the S3/S4 switching interval.

use isdbt_core::{
    CodeRate, GuardInterval, Layer, LayerConfig, Mode, Modulation, Params, TimeInterleave,
};
use isdbt_fec::MultiplexPattern;

const MODES: [Mode; 3] = [Mode::M1, Mode::M2, Mode::M3];
const GIS: [GuardInterval; 4] = [
    GuardInterval::G1_4,
    GuardInterval::G1_8,
    GuardInterval::G1_16,
    GuardInterval::G1_32,
];
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

fn check(p: &Params) -> MultiplexPattern {
    let pat = MultiplexPattern::new(p);
    assert_eq!(pat.len(), p.multiplex_frame_tsps(), "Table 3-5 n");
    for (id, l) in p.active_layers() {
        assert_eq!(
            pat.count(id),
            l.tsp_per_frame(p.mode()),
            "Table 3-3 count, layer {id}"
        );
    }
    let used: usize = p
        .active_layers()
        .map(|(_, l)| l.tsp_per_frame(p.mode()))
        .sum();
    assert_eq!(pat.null_count(), pat.len() - used);
    // §3.2.2.2: S3/S4 switch every 204/scale symbols, and each interval
    // carries an integral number of every layer's packets, so the pattern
    // repeats with period n/scale (reference Part 3.2.4 Figs. 3-4/3-5,
    // "Same pattern").
    let k = p.mode().scale();
    let sub = pat.len() / k;
    for q in 1..k {
        assert_eq!(
            &pat.slots()[..sub],
            &pat.slots()[q * sub..(q + 1) * sub],
            "{p:?} sub-frame {q}"
        );
    }
    pat
}

/// Every single-layer 13-segment configuration, all modes and GIs.
#[test]
fn spec_table_3_5_3_3_single_layer() {
    for mode in MODES {
        for gi in GIS {
            for m in MODS {
                for r in RATES {
                    let p = Params::new(mode, gi, false, [Some(lc(13, m, r)), None, None]).unwrap();
                    check(&p);
                }
            }
        }
    }
}

/// Every two-layer 1 + 12 configuration (partial reception), all modes and
/// GIs; and a set of three-layer splits.
#[test]
fn spec_table_3_5_3_3_multi_layer() {
    for mode in MODES {
        for gi in GIS {
            for ma in [Modulation::Qpsk, Modulation::Qam16] {
                for ra in RATES {
                    for mb in MODS {
                        for rb in RATES {
                            let p = Params::new(
                                mode,
                                gi,
                                true,
                                [Some(lc(1, ma, ra)), Some(lc(12, mb, rb)), None],
                            );
                            if let Ok(p) = p {
                                check(&p);
                            }
                        }
                    }
                }
            }
            for (sa, sb, sc) in [(1, 3, 9), (2, 5, 6), (4, 4, 5), (5, 7, 1)] {
                for (ma, mb, mc) in [
                    (Modulation::Dqpsk, Modulation::Qam16, Modulation::Qam64),
                    (Modulation::Qpsk, Modulation::Qam64, Modulation::Qam16),
                ] {
                    for (ra, rb, rc) in [
                        (CodeRate::R1_2, CodeRate::R2_3, CodeRate::R7_8),
                        (CodeRate::R7_8, CodeRate::R3_4, CodeRate::R5_6),
                    ] {
                        let p = Params::new(
                            mode,
                            gi,
                            false,
                            [
                                Some(lc(sa, ma, ra)),
                                Some(lc(sb, mb, rb)),
                                Some(lc(sc, mc, rc)),
                            ],
                        )
                        .unwrap();
                        check(&p);
                    }
                }
            }
        }
    }
}

/// The read-instant tie rule (datum complete at the end of its clock vs.
/// during it) does not change any pattern in this sweep.
#[test]
fn pattern_independent_of_tie_rule() {
    for mode in MODES {
        for gi in GIS {
            for mb in MODS {
                for rb in RATES {
                    let p = Params::new(
                        mode,
                        gi,
                        true,
                        [
                            Some(lc(1, Modulation::Qpsk, CodeRate::R2_3)),
                            Some(lc(12, mb, rb)),
                            None,
                        ],
                    )
                    .unwrap();
                    let a = MultiplexPattern::new(&p);
                    let b = MultiplexPattern::new_inclusive_tie(&p);
                    assert_eq!(a, b, "{p:?}");
                }
            }
        }
    }
}

fn render(p: &MultiplexPattern, n: usize) -> String {
    p.slots()[..n]
        .iter()
        .map(|s| match s {
            Some(Layer::A) => 'A',
            Some(Layer::B) => 'B',
            Some(Layer::C) => 'C',
            None => '.',
        })
        .collect()
}

/// Prints the patterns quoted in the crate report (`--nocapture`).
#[test]
fn print_reference_patterns() {
    let p = Params::new(
        Mode::M1,
        GuardInterval::G1_8,
        false,
        [
            Some(lc(5, Modulation::Dqpsk, CodeRate::R1_2)),
            Some(lc(8, Modulation::Qam64, CodeRate::R7_8)),
            None,
        ],
    )
    .unwrap();
    let pat = check(&p);
    let all = render(&pat, pat.len());
    println!(
        "Fig. 3-5 config (Mode 1 GI 1/8, DQPSK 1/2 x5 + 64QAM 7/8 x8): first 40 {} last 12 {}",
        &all[..40],
        &all[all.len() - 12..]
    );
    for gi in [GuardInterval::G1_4, GuardInterval::G1_8] {
        for ra in [CodeRate::R1_2, CodeRate::R2_3] {
            let p = Params::new(
                Mode::M3,
                gi,
                true,
                [
                    Some(lc(1, Modulation::Qpsk, ra)),
                    Some(lc(12, Modulation::Qam64, CodeRate::R3_4)),
                    None,
                ],
            )
            .unwrap();
            let pat = check(&p);
            println!(
                "Mode 3 GI 1/{} A=1seg QPSK {:?} B=12seg 64QAM 3/4: n={} A={} B={} null={}\n  first 60: {}",
                gi.denominator(),
                ra,
                pat.len(),
                pat.count(Layer::A),
                pat.count(Layer::B),
                pat.null_count(),
                render(&pat, 60)
            );
            let a_pos: Vec<usize> = pat
                .slots()
                .iter()
                .enumerate()
                .filter(|(_, s)| **s == Some(Layer::A))
                .map(|(i, _)| i)
                .take(12)
                .collect();
            println!("  first A slots: {a_pos:?}");
        }
    }
}

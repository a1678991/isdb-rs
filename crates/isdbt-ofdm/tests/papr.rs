//! Peak-to-average power of the first frames of a fresh modulator: the
//! start-up fill of the delay lines must not add up coherently across
//! carriers (a clipping hazard for 8-bit DACs).

mod common;

use common::Rng;
use isdbt_core::{
    CodeRate, GuardInterval, Layer, LayerConfig, Mode, Modulation, Params, TimeInterleave,
};
use isdbt_ofdm::Modulator;
use isdbt_ofdm::ac::AcFrame;
use isdbt_ofdm::tmcc::TmccInfo;

fn layer(segments: usize, modulation: Modulation, ti: u8) -> Option<LayerConfig> {
    Some(LayerConfig {
        segments,
        modulation,
        code_rate: CodeRate::R3_4,
        time_interleave: TimeInterleave(ti),
    })
}

fn papr_db(s: &[isdbt_ofdm::C32]) -> f64 {
    let p: Vec<f64> = s.iter().map(|v| v.norm_sqr() as f64).collect();
    let mean = p.iter().sum::<f64>() / p.len() as f64;
    let peak = p.iter().cloned().fold(0.0, f64::max);
    10.0 * (peak / mean).log10()
}

#[test]
fn papr_first_frames() {
    let cases = [
        Params::new(
            Mode::M3,
            GuardInterval::G1_8,
            true,
            [
                layer(1, Modulation::Qpsk, 2),
                layer(12, Modulation::Qam64, 2),
                None,
            ],
        ),
        Params::new(
            Mode::M3,
            GuardInterval::G1_8,
            true,
            [
                layer(1, Modulation::Qpsk, 0),
                layer(12, Modulation::Qam64, 0),
                None,
            ],
        ),
        Params::new(
            Mode::M1,
            GuardInterval::G1_4,
            false,
            [layer(13, Modulation::Dqpsk, 0), None, None],
        ),
        Params::new(
            Mode::M2,
            GuardInterval::G1_16,
            true,
            [
                layer(1, Modulation::Dqpsk, 0),
                layer(4, Modulation::Dqpsk, 1),
                layer(8, Modulation::Qam16, 3),
            ],
        ),
        Params::new(
            Mode::M3,
            GuardInterval::G1_32,
            false,
            [layer(13, Modulation::Dqpsk, 1), None, None],
        ),
        Params::new(
            Mode::M1,
            GuardInterval::G1_8,
            false,
            [layer(13, Modulation::Qam64, 0), None, None],
        ),
    ];
    for p in cases {
        let p = p.unwrap();
        for zero_input in [false, true] {
            let mut rng = Rng(7);
            let mut m = Modulator::new(p.clone());
            let tmcc = TmccInfo::from_params(&p);
            let ac = AcFrame::stuffing(m.layout());
            for f in 0..3 {
                let bits = Layer::ALL.map(|l| {
                    if zero_input {
                        vec![0; m.bits_per_frame(l)]
                    } else {
                        rng.bits(m.bits_per_frame(l))
                    }
                });
                let s = m
                    .modulate_frame([&bits[0], &bits[1], &bits[2]], &tmcc, &ac)
                    .unwrap();
                let papr = papr_db(&s);
                eprintln!(
                    "{:?} {:?} zero={zero_input} frame {f}: PAPR {papr:.1} dB",
                    p.mode, p.guard
                );
                if !zero_input {
                    assert!(papr < 15.0, "{p:?} frame {f}: PAPR {papr:.1} dB");
                }
            }
        }
    }
}

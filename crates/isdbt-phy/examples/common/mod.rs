//! Shared argument parsing for the file examples.
#![allow(dead_code)]
use isdbt_core::*;

pub fn parse_layer(s: &str) -> LayerConfig {
    // segs,mod,cr,ti   e.g. 12,64qam,3/4,2  (ti = TMCC index 0..3)
    let f: Vec<&str> = s.split(',').collect();
    let modulation = match f[1] {
        "dqpsk" => Modulation::Dqpsk,
        "qpsk" => Modulation::Qpsk,
        "16qam" => Modulation::Qam16,
        "64qam" => Modulation::Qam64,
        m => panic!("modulation {m}"),
    };
    let code_rate = match f[2] {
        "1/2" => CodeRate::R1_2,
        "2/3" => CodeRate::R2_3,
        "3/4" => CodeRate::R3_4,
        "5/6" => CodeRate::R5_6,
        "7/8" => CodeRate::R7_8,
        r => panic!("rate {r}"),
    };
    LayerConfig {
        segments: f[0].parse().unwrap(),
        modulation,
        code_rate,
        time_interleave: TimeInterleave::from_code(f[3].parse().unwrap())
            .expect("time-interleave index 0..=3"),
    }
}

pub fn params(mode: u8, gi: usize, partial: bool, a: &str, b: Option<&str>) -> Params {
    params3(mode, gi, partial, a, b, None)
}

pub fn params3(
    mode: u8,
    gi: usize,
    partial: bool,
    a: &str,
    b: Option<&str>,
    c: Option<&str>,
) -> Params {
    Params::new(
        Mode::from_number(mode).unwrap(),
        GuardInterval::from_denominator(gi).unwrap(),
        partial,
        [Some(parse_layer(a)), b.map(parse_layer), c.map(parse_layer)],
    )
    .unwrap()
}

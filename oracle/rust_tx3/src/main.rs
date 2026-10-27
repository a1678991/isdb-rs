//! TS files -> cf32 IQ through isdbt-phy's Transmitter, up to three layers:
//!   rust_tx3 OUT.cf32 FRAMES MODE GI PARTIAL A-SPEC A.ts [B-SPEC B.ts [C-SPEC C.ts]]
//! Spec "segs,mod,rate,ti_index" exactly as tx_file (ti_index = TMCC index 0..3).
//! Layer TS files are read cyclically, packet aligned (188 B).
use isdbt_core::*;
use std::io::Write;

fn parse_layer(s: &str) -> LayerConfig {
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
        segments: Segments::new(f[0].parse().unwrap()).expect("segments 1..=13"),
        modulation,
        code_rate,
        time_interleave: TimeInterleave::from_code(f[3].parse().unwrap())
            .expect("time-interleave index 0..=3"),
    }
}

fn load(p: &str) -> Vec<[u8; 188]> {
    let d = std::fs::read(p).unwrap();
    d.as_chunks::<188>().0.to_vec()
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let frames: usize = a[2].parse().unwrap();
    let mut layers = [None, None, None];
    let mut ts: [Vec<[u8; 188]>; 3] = Default::default();
    for l in 0..3 {
        if let (Some(spec), Some(path)) = (a.get(6 + 2 * l), a.get(7 + 2 * l)) {
            layers[l] = Some(parse_layer(spec));
            ts[l] = load(path);
        }
    }
    let params = Params::new(
        Mode::from_number(a[3].parse().unwrap()).unwrap(),
        GuardInterval::from_denominator(a[4].parse().unwrap()).unwrap(),
        a[5] == "1",
        layers,
    )
    .unwrap();
    let mut tx = isdbt_phy::Transmitter::new(params);
    let n: Vec<usize> = Layer::ALL.iter().map(|&l| tx.tsps_per_frame(l)).collect();
    let mut out = std::io::BufWriter::new(std::fs::File::create(&a[1]).unwrap());
    let mut idx = [0usize; 3];
    for _ in 0..frames {
        let f: Vec<Vec<[u8; 188]>> = (0..3)
            .map(|l| {
                (0..n[l])
                    .map(|k| ts[l][(idx[l] + k) % ts[l].len().max(1)])
                    .collect()
            })
            .collect();
        for l in 0..3 {
            idx[l] += n[l];
        }
        let s = tx.transmit_frame([&f[0], &f[1], &f[2]]).unwrap();
        out.write_all(&isdbt_phy::iq::cf32_to_bytes(&s)).unwrap();
    }
    eprintln!(
        "rust_tx3: {frames} frames, TSP/frame A {} B {} C {} -> {}",
        n[0], n[1], n[2], a[1]
    );
}

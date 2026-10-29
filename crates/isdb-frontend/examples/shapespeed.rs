//! Speed of the transmit pulse shaper ([`isdb_frontend::shape::Shaper`])
//! on one core: random 8PSK symbols, ISDB-S superframes (319 488
//! symbols) at the sample rates the radios use, best of 3 (development
//! tool).
//!
//! cargo run --release -p isdb-frontend --example shapespeed -- [SUPERFRAMES] [SPAN]

use isdb_frontend::shape::Shaper;
use num_complex::Complex32;
use std::f64::consts::TAU;
use std::time::Instant;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let superframes: usize = args.get(1).map_or(10, |s| s.parse().unwrap());
    let span: usize = args.get(2).map_or(16, |s| s.parse().unwrap());
    let mut s = 0x9e37_79b9_7f4a_7c15u64;
    let symbols: Vec<Complex32> = (0..319_488 * superframes)
        .map(|_| {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            let a = TAU * (s >> 61) as f64 / 8.0;
            // Unit-modulus points rounded to f32 on purpose.
            #[allow(clippy::cast_possible_truncation)]
            let v = Complex32::new(a.cos() as f32, a.sin() as f32);
            v
        })
        .collect();
    let rs = 28.86e6;
    for fs in [40e6, 57.72e6, 61.44e6] {
        let mut best = f64::INFINITY;
        let mut n = 0;
        for _ in 0..3 {
            let mut sh = Shaper::new(fs, rs, 0.35).unwrap().with_span(span).unwrap();
            let mut out = Vec::new();
            let t = Instant::now();
            for sf in symbols.chunks(319_488) {
                out.clear();
                sh.push(sf, &mut out);
                n += out.len();
            }
            best = best.min(t.elapsed().as_secs_f64());
        }
        let samples = n / 3;
        println!(
            "fs {:.2} MHz, span {span}: {samples} samples in {best:.3} s = {:.1} Msps ({:.2}× real time)",
            fs / 1e6,
            samples as f64 / best / 1e6,
            samples as f64 / best / fs
        );
    }
}

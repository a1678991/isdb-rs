//! Front-end throughput per instruction set (`simd::Isa`) on a recording,
//! single thread, through `process` and `process_each` (the receivers'
//! call), and whether every path's symbols equal the reference's
//! (development tool; plan §0.14).
//!
//! cargo run --release -p isdb-frontend --example feisa -- REC [s|s3] [TAPS]

use isdb_frontend::fast::FastFrontend;
use isdb_frontend::fixed::{Frontend, Symbol};
use isdb_frontend::params::{Design, Params};
use isdb_frontend::simd::Isa;
use isdb_iq::Recording;
use std::time::Instant;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let path = std::path::PathBuf::from(&args[1]);
    let s3 = args.get(2).is_none_or(|s| s == "s3");
    let taps: usize = args.get(3).map_or(32, |s| s.parse().unwrap());
    let mut rec = Recording::open(&path, None, None).unwrap();
    let fs = rec.sample_rate;
    let seg = rec.segments()[0].clone();
    let x = rec.read(seg.start, (seg.end - seg.start) as usize).unwrap();
    let xi: Vec<[i16; 2]> = x
        .iter()
        .map(|v| {
            [
                (v.re * 2048.0).round().clamp(-2048.0, 2047.0) as i16,
                (v.im * 2048.0).round().clamp(-2048.0, 2047.0) as i16,
            ]
        })
        .collect();
    let secs = xi.len() as f64 / fs;
    let (rs, a) = if s3 {
        (33.7561e6, 0.03)
    } else {
        (28.86e6, 0.35)
    };
    let mut d = Design::new(fs, rs, a);
    d.taps = taps;
    d.trim = -0.16;
    d.carrier_offset = if s3 { -130e3 } else { -112e3 };
    let p = Params::design(&d).unwrap();
    let mut want = Vec::new();
    Frontend::new(p.clone()).unwrap().process(&xi, &mut want);
    println!(
        "{}: {secs:.3} s at {:.2} Msps, {taps} taps, {} symbols; load {}",
        path.display(),
        fs / 1e6,
        want.len(),
        std::fs::read_to_string("/proc/loadavg")
            .unwrap_or_default()
            .split(' ')
            .next()
            .unwrap_or("?")
    );
    for isa in Isa::ALL.into_iter().filter(|i| i.available()) {
        let mut best = (f64::MAX, f64::MAX);
        let mut same = true;
        for _ in 0..3 {
            let t = Instant::now();
            let mut f = FastFrontend::with_isa(p.clone(), isa).unwrap();
            let mut out: Vec<Symbol> = Vec::with_capacity(want.len());
            for c in xi.chunks(1 << 16) {
                f.process(c, &mut out);
            }
            best.0 = best.0.min(t.elapsed().as_secs_f64());
            same &= out == want;
            let t = Instant::now();
            let mut f = FastFrontend::with_isa(p.clone(), isa).unwrap();
            let mut z = Vec::with_capacity(want.len());
            f.process_each(&xi, |s| z.push(s.zk));
            best.1 = best.1.min(t.elapsed().as_secs_f64());
            same &= z.len() == want.len() && z.iter().zip(&want).all(|(a, b)| *a == b.zk);
        }
        for chunk in [256usize, 4096, 1 << 16] {
            let t = Instant::now();
            let mut f = FastFrontend::with_isa(p.clone(), isa).unwrap();
            let mut out: Vec<Symbol> = Vec::with_capacity(want.len());
            for c in xi.chunks(chunk) {
                f.process(c, &mut out);
            }
            println!(
                "    {} process, chunk {chunk}: {:.3} s",
                isa.name(),
                t.elapsed().as_secs_f64()
            );
        }
        println!(
            "  {:8} process {:.3} s ({:.2}× real time), process_each {:.3} s ({:.2}×); identical: {same}",
            isa.name(),
            best.0,
            secs / best.0,
            best.1,
            secs / best.1
        );
    }
}

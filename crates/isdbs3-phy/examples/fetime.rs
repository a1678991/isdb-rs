//! Where the time of `frontend::symbols` goes: survey, conversion, carrier
//! choice, trim search and the front end itself (development tool; plan
//! §0.10).
//!
//! cargo run --release -p isdbs3-phy --example fetime -- REC

use isdb_iq::Recording;
use isdb_iq::survey::{Options, survey};
use isdbs3_phy::frontend::{carrier_candidates, design, samples_in, to_input};
use std::time::Instant;

fn main() {
    let path = std::path::PathBuf::from(std::env::args().nth(1).expect("REC"));
    let mut rec = Recording::open(&path, None, None).expect("open");
    let fs = rec.sample_rate;
    let seg = rec.segments()[0].clone();
    let x = rec
        .read(
            seg.start,
            usize::try_from(seg.end - seg.start).expect("segment length"),
        )
        .expect("read");
    let mut lap = Instant::now();
    let mut step = |name: &str| {
        println!("  {name:24} {:7.3} s", lap.elapsed().as_secs_f64());
        lap = Instant::now();
    };
    let sv = survey(&x[..x.len().min(1 << 22)], fs, &Options::default());
    step("survey");
    let xi = to_input(&x);
    step("to i16");
    let cands = carrier_candidates(&sv);
    let cfo = isdbs3_phy::frontend::pick_carrier(&xi, fs, &cands).unwrap();
    step("carrier choice");
    let mut d = design(fs, cfo);
    let n = samples_in(0.15, fs).min(xi.len());
    let tc: Vec<f64> = (-10..=2).map(|i| f64::from(i) * 0.025).collect();
    d.trim = isdb_frontend::s3sync::best_trim(&xi[..n], &d, &tc)
        .unwrap()
        .0;
    step("trim search");
    // one candidate's parts
    let mut fe1 =
        isdb_frontend::fast::FastFrontend::new(isdb_frontend::Params::design(&d).unwrap()).unwrap();
    let mut o1 = Vec::new();
    fe1.process(&xi[..n], &mut o1);
    step("  one candidate: front end");
    let z1: Vec<num_complex::Complex64> = o1
        .iter()
        .skip(3 * isdb_frontend::s3sync::SLOT)
        .map(|s| num_complex::Complex64::new(f64::from(s.zk[0]), f64::from(s.zk[1])))
        .collect();
    let _ = isdb_frontend::s3sync::measure(&z1);
    step("  one candidate: measure");
    let _ = isdb_frontend::Params::design(&d).unwrap();
    step("  Params::design");
    let mut fe =
        isdb_frontend::fast::FastFrontend::new(isdb_frontend::Params::design(&d).unwrap()).unwrap();
    let mut out = Vec::new();
    let mut z = Vec::new();
    for c in xi.chunks(1 << 16) {
        out.clear();
        fe.process(c, &mut out);
        z.extend(out.iter().map(|s| (s.zk[0], s.zk[1])));
    }
    step("front end");
    println!("  {} symbols", z.len());
}

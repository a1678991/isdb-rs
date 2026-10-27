//! Whether ISDB-S gains from the front end's output trim (format 2), as
//! ISDB-S3 does: run the bit-exact front end with output trims around 0
//! on a recording and report the pilot (burst) MER and main-signal MER
//! that `rx::receive` measures.
//!
//! cargo run --release -p isdbs-phy --example trimscan -- REC [SAMPLES]

// Narrowing casts are proven or fallible here (docs/plans/type-safety.md section 2).
#![warn(clippy::cast_possible_truncation)]

use isdb_frontend::fixed::Frontend;
use isdb_frontend::params::{Design, Params};
use isdb_iq::Recording;
use isdb_iq::survey::{Options, survey};
use isdbs_phy::Cplx;
use isdbs_phy::frontend::to_input;
use isdbs_phy::rx::{RxOptions, main_mer, receive};
use std::path::PathBuf;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let path = PathBuf::from(&args[1]);
    let n: usize = args.get(2).map_or(16 << 20, |s| s.parse().unwrap());
    let mut rec = Recording::open(&path, None, None).unwrap();
    let fs = rec.sample_rate;
    let seg = rec.segments()[0].clone();
    let x = rec
        .read(
            seg.start,
            n.min(usize::try_from(seg.end - seg.start).unwrap()),
        )
        .unwrap();
    let sv = survey(&x[..x.len().min(1 << 22)], fs, &Options::default());
    let cfo = sv.best_carrier().map_or(0.0, |c| c.offset_hz);
    let xi = to_input(&x);
    println!(
        "{}: {} samples at {fs} Hz, carrier {cfo:+.0} Hz",
        path.display(),
        xi.len()
    );
    for trim in [-0.15, -0.10, -0.05, 0.0, 0.05, 0.10] {
        let mut d = Design::for_system(isdbs_phy::frontend::system(), fs);
        d.carrier_offset = cfo;
        d.trim = trim;
        let mut fe = Frontend::new(Params::design(&d).unwrap()).unwrap();
        let mut out = Vec::new();
        fe.process(&xi, &mut out);
        let z: Vec<Cplx> = out
            .iter()
            .map(|s| Cplx::new(f64::from(s.zk[0]), f64::from(s.zk[1])))
            .collect();
        let rx = receive(&z, &RxOptions::default());
        let pilots: Vec<f64> = rx.frames.iter().map(|f| f.pilot_mer_db).collect();
        let pm = pilots.iter().sum::<f64>() / pilots.len().max(1) as f64;
        let mains: Vec<f64> = rx
            .superframes
            .iter()
            .filter_map(|sf| sf.info.as_ref().map(|i| (sf.first_frame, i)))
            .flat_map(|(first, info)| {
                (first..(first + 8).min(rx.frames.len()))
                    .filter_map(|i| main_mer(&rx.frames[i], info))
                    .collect::<Vec<_>>()
            })
            .collect();
        let mm = mains.iter().sum::<f64>() / mains.len().max(1) as f64;
        println!(
            "  trim {trim:+.2} T: {} frames, pilot MER {pm:.2} dB, main MER {mm:.2} dB",
            rx.frames.len()
        );
    }
}

//! Where the time of `isdb rx --system isdbs` goes, per stage, on one
//! recording: survey, trim search, front end, receive (frame sync,
//! carrier, TMCC) and the main signal to TS (development tool; plan
//! §0.10).
//!
//! cargo run --release -p isdbs-phy --example chaintime -- REC

// Narrowing casts are proven or fallible here (docs/plans/type-safety.md section 2).
#![warn(clippy::cast_possible_truncation)]

use isdb_iq::Recording;
use isdb_iq::survey::{Options, survey};
use isdbs_phy::demod::DemodOptions;
use isdbs_phy::frontend::{Trim, design, measure_trim, samples_in, symbols_with, to_input};
use isdbs_phy::ts::decode_all;
use isdbs_phy::{RxOptions, receive};
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
    let secs = x.len() as f64 / fs;
    println!(
        "{}: {secs:.3} s of signal at {:.2} Msps",
        path.display(),
        fs / 1e6
    );
    let mut lap = Instant::now();
    let mut step = |name: &str| {
        println!("  {name:26} {:7.3} s", lap.elapsed().as_secs_f64());
        lap = Instant::now();
    };
    let sv = survey(&x[..x.len().min(1 << 22)], fs, &Options::default());
    step("survey");
    let cfo = sv.best_carrier().map_or(0.0, |c| c.offset_hz);
    let xi = to_input(&x);
    let n = samples_in(0.15, fs).min(xi.len());
    let (trim, _) = measure_trim(&xi[..n], &design(fs, cfo)).expect("trim");
    step("trim search");
    let s = symbols_with(&x, fs, Some(cfo), Trim::Fixed(trim)).expect("front end");
    step("survey + front end");
    let (marks, _) = isdbs_phy::sync::find_frames(&s.z, &isdbs_phy::sync::SyncOptions::default());
    step("  find_frames");
    let _ = isdbs_phy::carrier::track(&s.z, &marks, RxOptions::default().carrier_bandwidth);
    step("  carrier track");
    let r = receive(&s.z, &RxOptions::default());
    step("receive (sync, TMCC)");
    let d = decode_all(&r, &DemodOptions::default());
    step("main signal to TS");
    println!("  {} superframes decoded", d.len());
}

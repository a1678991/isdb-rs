//! How the ISDB-S3 carrier is chosen on a recording: the survey's
//! strongest line (what `isdb symbols` used to take), the candidate list
//! and the front-end trial of [`isdbs3_phy::frontend::pick_carrier`], with
//! each candidate's sync-symbol Es/N0 and phase step on the excerpt
//! (development tool; plan §0.16).
//!
//! cargo run --release -p isdbs3-phy --example carrierpick -- REC [REC…]

use isdb_frontend::fast::FastFrontend as Frontend;
use isdb_frontend::params::Params;
use isdb_iq::Recording;
use isdb_iq::survey::{Options, survey};
use isdbs3_phy::frontend::{carrier_candidates, design, pick_carrier, samples_in, to_input};

fn main() {
    for path in std::env::args().skip(1) {
        let mut rec = Recording::open(std::path::Path::new(&path), None, None).expect("open");
        let fs = rec.sample_rate;
        let seg = rec.segments()[0].clone();
        let n = usize::try_from(seg.end - seg.start)
            .expect("segment length")
            .min(samples_in(0.2, fs));
        let x = rec.read(seg.start, n).expect("read");
        let sv = survey(&x[..x.len().min(1 << 22)], fs, &Options::default());
        let xi = to_input(&x);
        let cands = carrier_candidates(&sv);
        println!("{path}");
        println!(
            "  strongest line: {:+.0} Hz",
            sv.best_carrier().map_or(f64::NAN, |c| c.offset_hz)
        );
        let m = samples_in(0.05, fs).min(xi.len());
        for &c in &cands {
            let mut fe =
                Frontend::new(Params::design(&design(fs, c)).expect("design")).expect("fe");
            let mut out = Vec::new();
            fe.process(&xi[..m], &mut out);
            let z: Vec<num_complex::Complex64> = out
                .iter()
                .skip(3 * isdb_frontend::s3sync::SLOT)
                .map(|s| num_complex::Complex64::new(f64::from(s.zk[0]), f64::from(s.zk[1])))
                .collect();
            match isdb_frontend::s3sync::measure(&z) {
                Some(r) => println!(
                    "  candidate {c:+10.0} Hz: Es/N0 {:6.2} dB, phase step {:+.3} rad",
                    r.snr_db, r.phase_step_mean
                ),
                None => println!("  candidate {c:+10.0} Hz: no slot sync"),
            }
        }
        println!(
            "  pick_carrier: {:+.0} Hz",
            pick_carrier(&xi, fs, &cands).expect("pick")
        );
    }
}

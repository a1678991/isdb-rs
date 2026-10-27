//! Acquisition of every segment of a recording on its own (no prior):
//! survey candidates, the carrier picked, the trim and its Es/N0, and the
//! receiver's result (development tool; plan §0.10).
//!
//! cargo run --release -p isdbs3-phy --example blockacq -- REC

// Narrowing casts are proven or fallible here (docs/plans/type-safety.md section 2).
#![warn(clippy::cast_possible_truncation)]

use isdb_iq::Recording;
use isdb_iq::survey::{Options, survey};
use isdbs3_phy::frontend::{Trim, carrier_candidates, samples_in, symbols_with_prior, to_input};
use isdbs3_phy::rx::{RxOptions, receive};

fn main() {
    let path = std::path::PathBuf::from(std::env::args().nth(1).expect("REC"));
    let mut rec = Recording::open(&path, None, None).expect("open");
    let fs = rec.sample_rate;
    let segs = rec.segments().clone();
    let mut prior = None;
    for (i, seg) in segs.iter().enumerate() {
        let x = rec
            .read(
                seg.start,
                usize::try_from(seg.end - seg.start).expect("segment length"),
            )
            .expect("read");
        let sv = survey(&x[..x.len().min(1 << 22)], fs, &Options::default());
        let c: Vec<String> = carrier_candidates(&sv)
            .iter()
            .map(|f| format!("{:.1}", f / 1e3))
            .collect();
        // every candidate's sync statistics on the first 0.05 s
        let xi = to_input(&x);
        for &cfo in &carrier_candidates(&sv) {
            let d = isdbs3_phy::frontend::design(fs, cfo);
            let mut fe =
                isdb_frontend::fast::FastFrontend::new(isdb_frontend::Params::design(&d).unwrap())
                    .unwrap();
            let mut z = Vec::new();
            fe.process_each(&xi[..samples_in(0.05, fs)], |s| {
                z.push(num_complex::Complex64::new(
                    f64::from(s.zk[0]),
                    f64::from(s.zk[1]),
                ));
            });
            if let Some(r) = isdb_frontend::s3sync::measure(&z[3 * isdb_frontend::s3sync::SLOT..]) {
                println!(
                    "  candidate {:9.1} kHz: Es/N0 {:6.2} dB, phase step {:+.3} ± {:.3} rad",
                    cfo / 1e3,
                    r.snr_db,
                    r.phase_step_mean,
                    r.phase_step_rms
                );
            }
        }
        for (label, p) in [("fresh", None), ("prior", prior)] {
            if label == "prior" && p.is_none() {
                continue;
            }
            let (s, acq) =
                symbols_with_prior(&x, fs, None, Trim::Auto, p.as_ref()).expect("front end");
            let r = receive(&s.z, &RxOptions::default());
            let ok = r.frames.iter().filter(|f| f.tmcc.is_ok()).count();
            let failed = r
                .slots
                .iter()
                .filter(|s| s.ldpc_iterations.is_none())
                .count();
            println!(
                "segment {i} {label}: candidates [{}] kHz; carrier {:.1} kHz, trim {:.3}, excerpt Es/N0 {:.2} dB; TMCC {ok}/{}, slots {}, LDPC failed {failed}; sync {:.2} dB",
                c.join(", "),
                acq.carrier_offset_hz / 1e3,
                acq.trim,
                acq.sync_snr_db,
                r.frames.len(),
                r.slots.len(),
                r.sync_snr_db
            );
            if label == "fresh" {
                prior = Some(acq);
            }
        }
    }
}

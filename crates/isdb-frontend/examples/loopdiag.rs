//! Timing-loop diagnostics of the bit-exact model on a recording: period,
//! gain and timing error over time, and the sync-symbol Es/N0 of the
//! output (development tool; plan §0.15).
//!
//! cargo run --release -p isdb-frontend --example loopdiag -- REC [s|s3] [TRIM]

// Narrowing casts are proven or fallible here (docs/plans/type-safety.md section 2).
#![warn(clippy::cast_possible_truncation)]
use isdb_frontend::fixed::Frontend;
use isdb_frontend::params::{Design, Params};
use isdb_frontend::s3sync;
use isdb_iq::Recording;
use isdb_iq::survey::{KNOWN_SYSTEMS, Options, survey};
use num_complex::Complex64 as C;
use std::path::PathBuf;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let path = PathBuf::from(&args[1]);
    let s3 = args.get(2).is_none_or(|s| s == "s3");
    let trim: f64 = args.get(3).map_or(0.0, |s| s.parse().unwrap());
    let mut rec = Recording::open(&path, None, None).unwrap();
    let fs = rec.sample_rate;
    let seg = rec.segments()[0].clone();
    let n = usize::try_from(seg.end - seg.start).unwrap().min(12 << 20);
    let x = rec.read(seg.start, n).unwrap();
    let sv = survey(&x[..x.len().min(1 << 22)], fs, &Options::default());
    let cfo = sv.best_carrier().map_or(0.0, |c| c.offset_hz);
    let xi: Vec<[i16; 2]> = x.iter().map(|&v| isdb_frontend::input_sample(v)).collect();
    let mut d = Design::for_system(&KNOWN_SYSTEMS[usize::from(s3)], fs);
    d.carrier_offset = cfo;
    d.trim = trim;
    d.bits = 8;
    let p = Params::design(&d).unwrap();
    println!(
        "kp {} >> {}, ki {} >> {}, agc_shift {}, nominal {}",
        p.kp, p.kp_shift, p.ki, p.ki_shift, p.agc_shift, p.period_nominal
    );
    let mut fe = Frontend::new(p.clone()).unwrap();
    let mut out = Vec::new();
    fe.process(&xi, &mut out);
    let step = out.len() / 12;
    for k in (0..out.len()).step_by(step) {
        let s = &out[k];
        let es: Vec<i32> = out[k..(k + step).min(out.len())]
            .iter()
            .filter(|s| s.z != [0, 0] || s.e != 0)
            .map(|s| s.e)
            .collect();
        let me = es.iter().map(|&e| f64::from(e)).sum::<f64>() / es.len() as f64;
        println!(
            "sym {k:8}: period {:.6} gain {:.3} mean e {me:+.1} over {}",
            f64::from(s.period) / f64::from(1u32 << 24),
            f64::from(s.gain) / 1024.0,
            es.len()
        );
    }
    if s3 {
        let zk: Vec<C> = out
            .iter()
            .skip(out.len() / 4)
            .map(|s| C::new(f64::from(s.zk[0]), f64::from(s.zk[1])))
            .collect();
        if let Some(r) = s3sync::measure(&zk) {
            println!("Es/N0 {:.2} dB over {} slots", r.snr_db, r.slots);
        }
        // the same per window of 40 slots: slips show as bad windows
        let w = 40 * s3sync::SLOT;
        let per: Vec<String> = zk
            .chunks(w)
            .filter(|c| c.len() == w)
            .map(|c| s3sync::measure(c).map_or("-".into(), |r| format!("{:.1}", r.snr_db)))
            .collect();
        println!("per 40 slots: {}", per.join(" "));
    }
}

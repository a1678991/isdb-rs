//! How few taps the timing detector's banks (`h`, `g`) need: zero their
//! outer taps (keeping the centre `n` of the 32), keep the output bank `k`
//! whole, measure the trimmed ISDB-S3 sync-symbol Es/N0 with the bit-exact
//! model (development tool; plan §0.15: the FPGA's multiplier budget).
//!
//! cargo run --release -p isdb-frontend --example tedspan -- REC [N...]

use isdb_frontend::fast::FastFrontend;
use isdb_frontend::params::{Design, Params};
use isdb_frontend::s3sync::{self, SLOT};
use isdb_iq::Recording;
use isdb_iq::survey::{KNOWN_SYSTEMS, Options, survey};
use num_complex::Complex64 as C;
use std::path::PathBuf;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let path = PathBuf::from(&args[1]);
    let spans: Vec<usize> = if args.len() > 2 {
        args[2..].iter().map(|s| s.parse().unwrap()).collect()
    } else {
        vec![32, 24, 20, 16, 12, 8]
    };
    let mut rec = Recording::open(&path, None, None).unwrap();
    let fs = rec.sample_rate;
    let seg = rec.segments()[0].clone();
    let x = rec
        .read(seg.start, usize::try_from(seg.end - seg.start).unwrap())
        .unwrap();
    let sv = survey(&x[..x.len().min(1 << 22)], fs, &Options::default());
    let cfo = sv.best_carrier().map_or(0.0, |c| c.offset_hz);
    let xi: Vec<[i16; 2]> = x.iter().map(|&v| isdb_frontend::input_sample(v)).collect();
    let mut d = Design::for_system(&KNOWN_SYSTEMS[1], fs);
    d.carrier_offset = cfo;
    d.bits = 8;
    let cands: Vec<f64> = (-6..=1).map(|i| f64::from(i) * 0.05).collect();
    let (trim, _) = s3sync::best_trim(&xi[..xi.len().min(6 << 20)], &d, &cands).unwrap();
    d.trim = trim;
    let base = Params::design(&d).unwrap();
    let taps = base.taps();
    for n in spans {
        let mut p = base.clone();
        let lo = (taps - n) / 2;
        for bank in [&mut p.h, &mut p.g] {
            for ph in bank.iter_mut() {
                for (t, c) in ph.iter_mut().enumerate() {
                    if t < lo || t >= lo + n {
                        *c = 0;
                    }
                }
            }
        }
        let mut fe = FastFrontend::new(p).unwrap();
        let mut out = Vec::new();
        fe.process(&xi, &mut out);
        let zk: Vec<C> = out
            .iter()
            .skip(3 * SLOT)
            .map(|s| C::new(f64::from(s.zk[0]), f64::from(s.zk[1])))
            .collect();
        let r = s3sync::measure(&zk).unwrap();
        println!(
            "{}: trim {trim:+.3}, detector taps {n:2} of {taps}: Es/N0 {:.2} dB, phase step rms {:.3} rad",
            path.file_name().unwrap().to_string_lossy(),
            r.snr_db,
            r.phase_step_rms
        );
    }
}

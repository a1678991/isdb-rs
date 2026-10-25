//! Where the time of `isdb rx --system isdbs3` goes, per stage, on one
//! recording: survey, carrier choice and trim, the bit-exact front end,
//! and the receiver (sync, equaliser, TMCC, demap, LDPC, BCH), with the
//! fixed-point SIMD and the float LDPC (development tool; plan §0.9).
//!
//! cargo run --release -p isdbs3-phy --example chaintime -- REC

use isdb_iq::Recording;
use isdb_iq::survey::{Options, survey};
use isdbs3_phy::frontend::{Trim, symbols};
use isdbs3_phy::main_signal::Ldpc;
use isdbs3_phy::rx::{RxOptions, receive};
use std::time::Instant;

fn main() {
    let path = std::path::PathBuf::from(std::env::args().nth(1).expect("REC"));
    let mut rec = Recording::open(&path, None, None).expect("open");
    let fs = rec.sample_rate;
    let seg = rec.segments()[0].clone();
    let x = rec
        .read(seg.start, (seg.end - seg.start) as usize)
        .expect("read");
    let secs = x.len() as f64 / fs;
    let t = Instant::now();
    let sv = survey(&x[..x.len().min(1 << 22)], fs, &Options::default());
    let t_survey = t.elapsed().as_secs_f64();
    let cfo = sv.best_carrier().map_or(0.0, |c| c.offset_hz);
    let t = Instant::now();
    let s_fixed = symbols(&x, fs, Some(cfo), Trim::Fixed(-0.17)).expect("front end");
    let t_fe = t.elapsed().as_secs_f64();
    let t = Instant::now();
    let s = symbols(&x, fs, None, Trim::Auto).expect("front end");
    let t_full = t.elapsed().as_secs_f64();
    let _ = s_fixed;
    println!(
        "{}: {secs:.3} s of signal at {:.2} Msps",
        path.display(),
        fs / 1e6
    );
    println!("  survey                       {t_survey:7.2} s");
    println!(
        "  front end alone              {t_fe:7.2} s ({:.3}× real time)",
        secs / t_fe
    );
    println!(
        "  symbols(): carrier + trim + front end {t_full:7.2} s (choice and trim {:.2} s)",
        t_full - t_fe
    );
    {
        use isdbs3_phy::sync::synchronise;
        let t = Instant::now();
        let sy = synchronise(&s.z).expect("sync");
        let t_sync = t.elapsed().as_secs_f64();
        let es = 10f64.powf(sy.sync_snr_db / 10.0);
        let t = Instant::now();
        let mut ok = 0;
        for &f in &sy.frames {
            if let Some(l) = isdbs3_phy::tmcc::llrs(&s.z, &sy.slots, f, 1.0 / (2.0 * es)) {
                ok += usize::from(isdbs3_phy::tmcc::decode(&l, 50).is_ok());
            }
        }
        let t_tmcc = t.elapsed().as_secs_f64();
        println!(
            "  sync {t_sync:.2} s; TMCC of {} frames {t_tmcc:.2} s ({ok} decoded)",
            sy.frames.len()
        );
    }
    for (name, ldpc) in [("fixed SIMD", Ldpc::FIXED), ("float", Ldpc::FLOAT)] {
        let t = Instant::now();
        let r = receive(
            &s.z,
            &RxOptions {
                ldpc,
                ..RxOptions::default()
            },
        );
        let el = t.elapsed().as_secs_f64();
        let mut no_ldpc = RxOptions {
            ldpc: Ldpc::Fixed {
                max_iter: 0,
                input_scale: 1.0,
                offset: 1,
                simd: true,
            },
            ..RxOptions::default()
        };
        no_ldpc.max_iter = 50;
        let t = Instant::now();
        let _ = receive(&s.z, &no_ldpc);
        let el0 = t.elapsed().as_secs_f64();
        println!(
            "  receive, {name:10}           {el:7.2} s ({} slots; without LDPC iterations {el0:.2} s)",
            r.slots.len()
        );
    }
}

//! Front-end throughput: the reference (`fixed::Frontend`) and the fast
//! implementation (`fast::FastFrontend`) on a recording, single thread,
//! and whether their symbols agree (development tool; plan §0.10).
//!
//! cargo run --release -p isdb-frontend --example fespeed -- REC [s|s3] [TAPS]

// Narrowing casts are proven or fallible here (docs/plans/type-safety.md section 2).
#![warn(clippy::cast_possible_truncation)]
use isdb_frontend::fast::FastFrontend;
use isdb_frontend::fixed::Frontend;
use isdb_frontend::params::{Design, Params};
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
    let x = rec
        .read(seg.start, usize::try_from(seg.end - seg.start).unwrap())
        .unwrap();
    let xi: Vec<[i16; 2]> = x.iter().map(|&v| isdb_frontend::input_sample(v)).collect();
    let secs = xi.len() as f64 / fs;
    let (rs, a) = if s3 {
        (33.7561e6, 0.03)
    } else {
        (28.86e6, 0.35)
    };
    let mut d = Design::new(fs, rs, a);
    d.taps = taps;
    d.trim = -0.16;
    d.carrier_offset = -130e3;
    let p = Params::design(&d).unwrap();
    let t = Instant::now();
    let mut r = Frontend::new(p.clone()).unwrap();
    let mut a_out = Vec::new();
    r.process(&xi, &mut a_out);
    let t_ref = t.elapsed().as_secs_f64();
    let t = Instant::now();
    let mut f = FastFrontend::new(p).unwrap();
    let mut b_out = Vec::with_capacity(a_out.len());
    for c in xi.chunks(1 << 16) {
        f.process(c, &mut b_out);
    }
    let t_fast = t.elapsed().as_secs_f64();
    // symbols consumed one at a time (no Vec<Symbol>): what a receiver
    // that keeps only zk pays
    let t = Instant::now();
    let mut f2 = FastFrontend::new(f.params().clone()).unwrap();
    let mut zsum = 0i64;
    for &s in &xi {
        if let Some(y) = f2.push(s) {
            zsum += i64::from(y.zk[0]);
        }
    }
    let t_push = t.elapsed().as_secs_f64();
    // the same loop over symbols twice (warm caches, branch history)
    let t = Instant::now();
    let mut f3 = FastFrontend::new(f.params().clone()).unwrap();
    let mut n3 = 0u64;
    for &s in &xi {
        n3 += u64::from(f3.push(s).is_some());
    }
    println!("  fast again: {:.3} s ({n3})", t.elapsed().as_secs_f64());
    println!("  fast, push one by one, keep zk: {t_push:.3} s ({zsum})");
    // the mixer alone (the per-sample part), for the split of the time
    let t = Instant::now();
    let cos = isdb_frontend::tables::cos_table();
    let (mut nco, mut acc) = (0u32, 0i64);
    for s in &xi {
        let idx = (nco >> 22) as usize;
        let (c, si) = (i32::from(cos[idx]), i32::from(cos[(idx + 768) & 1023]));
        let re = (i32::from(s[0]) * c - i32::from(s[1]) * si + (1 << 14)) >> 15;
        acc += i64::from(re);
        nco = nco.wrapping_add(12345);
    }
    let t_mix = t.elapsed().as_secs_f64();
    println!("  mixer alone {t_mix:.3} s ({acc})");
    println!(
        "{}: {secs:.3} s at {:.2} Msps, {taps} taps, {} symbols",
        path.display(),
        fs / 1e6,
        a_out.len()
    );
    println!(
        "  reference {t_ref:.3} s ({:.2}× real time); fast {t_fast:.3} s ({:.2}× real time, {:.1}× the reference); identical: {}",
        secs / t_ref,
        secs / t_fast,
        t_ref / t_fast,
        a_out == b_out
    );
}

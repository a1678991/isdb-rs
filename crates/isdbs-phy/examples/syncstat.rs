//! Statistics of the W1-tail metric at the frame starts and elsewhere, to
//! choose sync thresholds (development tool).
//!
//! cargo run --release -p isdbs-phy --example syncstat -- REC

// Narrowing casts are proven or fallible here (docs/plans/type-safety.md section 2).
#![warn(clippy::cast_possible_truncation)]

use isdb_iq::Recording;
use isdbs_core::frame::{FRAME_SYMBOLS, W1_CODED_TAIL, W1_TAIL_AT};
use isdbs_phy::sync::{frame_metric, tail_coherent, tail_metric};

fn main() {
    let path = std::path::PathBuf::from(std::env::args().nth(1).unwrap());
    let mut rec = Recording::open(&path, None, None).unwrap();
    let seg = rec.segments()[0].clone();
    let x = rec
        .read(seg.start, usize::try_from(seg.end - seg.start).unwrap())
        .unwrap();
    let s = isdbs_phy::frontend::symbols(&x, rec.sample_rate, None).unwrap();
    let z = &s.z;
    // Strongest frame-signature peak in one frame gives the grid.
    let (mut best, mut at) = (0.0, 0);
    for i in 40_000..40_000 + FRAME_SYMBOLS {
        let m = frame_metric(z, i);
        if m > best {
            best = m;
            at = i;
        }
    }
    let mut fm_on = Vec::new();
    let f0 = at % FRAME_SYMBOLS;
    let mut on = Vec::new();
    let mut coh = Vec::new();
    let mut s0 = f0;
    while s0 + FRAME_SYMBOLS < z.len() {
        on.push(tail_metric(z, s0 + W1_TAIL_AT, W1_CODED_TAIL));
        fm_on.push(frame_metric(z, s0));
        let (c, p) = tail_coherent(z, s0 + W1_TAIL_AT, W1_CODED_TAIL);
        coh.push(c.norm() / p);
        s0 += FRAME_SYMBOLS;
    }
    let off: Vec<f64> = (0..20_000)
        .map(|k| tail_metric(z, 50_000 + k * 37 + W1_TAIL_AT, W1_CODED_TAIL))
        .collect();
    let fm_off: Vec<f64> = (0..20_000)
        .map(|k| frame_metric(z, f0 + 1 + 50_000 + k * 37))
        .collect();
    // the `permille`-th per-mille point of `v` (0 = min, 1000 = max)
    let q = |v: &mut Vec<f64>, permille: usize| {
        v.sort_by(f64::total_cmp);
        v[(v.len() - 1) * permille / 1000]
    };
    let (mut a, mut b, mut c) = (on.clone(), off.clone(), coh.clone());
    println!("{}: grid {f0}, {} frames", path.display(), on.len());
    println!(
        "  W1 differential at frames: min {:.3} p10 {:.3} median {:.3}",
        q(&mut a, 0),
        q(&mut a, 100),
        q(&mut a, 500)
    );
    println!(
        "  W1 coherent at frames:     min {:.3} p10 {:.3} median {:.3}",
        q(&mut c, 0),
        q(&mut c, 100),
        q(&mut c, 500)
    );
    println!(
        "  differential elsewhere:    median {:.3} p99.9 {:.3} max {:.3}",
        q(&mut b, 500),
        q(&mut b, 999),
        q(&mut b, 1000)
    );
    let (mut d, mut e) = (fm_on.clone(), fm_off.clone());
    println!(
        "  frame metric at frames:    min {:.3} p10 {:.3} median {:.3}",
        q(&mut d, 0),
        q(&mut d, 100),
        q(&mut d, 500)
    );
    println!(
        "  frame metric elsewhere:    median {:.3} p99.9 {:.3} max {:.3}",
        q(&mut e, 500),
        q(&mut e, 999),
        q(&mut e, 1000)
    );
}

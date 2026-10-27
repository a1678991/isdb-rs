//! Frame metric and pilot-only MER at the found frame starts and at ±1, ±2
//! symbols (development tool: checks that the search picks the right
//! symbol on weak signals).
//!
//! cargo run --release -p isdbs-phy --example offsets -- REC

// Narrowing casts are proven or fallible here (docs/plans/type-safety.md section 2).
#![warn(clippy::cast_possible_truncation)]

use isdb_iq::Recording;
use isdbs_phy::sync::{SyncOptions, find_frames, frame_metric};

fn main() {
    let path = std::path::PathBuf::from(std::env::args().nth(1).unwrap());
    let mut rec = Recording::open(&path, None, None).unwrap();
    let seg = rec.segments()[0].clone();
    let x = rec
        .read(seg.start, usize::try_from(seg.end - seg.start).unwrap())
        .unwrap();
    let s = isdbs_phy::frontend::symbols(&x, rec.sample_rate, None).unwrap();
    let (f, _) = find_frames(&s.z, &SyncOptions::default());
    for d in -2isize..=2 {
        let m: f64 = f
            .iter()
            .take(50)
            .map(|m| frame_metric(&s.z, m.start.wrapping_add_signed(d)))
            .sum::<f64>()
            / 50.0;
        println!("offset {d:+}: mean frame metric {m:.3}");
    }
    // Derotated burst symbols: mean of sign-corrected projection on I and Q.
    let pil = isdbs_phy::carrier::track(&s.z, &f, 0.05);
    let bits = isdbs_core::burst::frame_bits();
    let (mut si, mut sq, mut e2, mut n) = (0.0, 0.0, 0.0, 0.0);
    for k in 1..20 {
        let d = isdbs_phy::carrier::derotate(&s.z, &f[k], &pil[k], pil.get(k + 1).map(|p| &p[0]));
        for b in 0..192 {
            for j in 0..4 {
                let v = d[isdbs_core::frame::burst_symbol_position(b, j)];
                let sg = if bits[b * 4 + j] == 0 { 1.0 } else { -1.0 };
                si += sg * v.re;
                sq += sg * v.im;
                e2 += v.norm_sqr();
                n += 1.0;
            }
        }
    }
    let (mi, mq) = (si / n, sq / n);
    println!(
        "burst projection: I {mi:.0} Q {mq:.0}; rms |z| {:.0}; SNR-ish {:.2} dB",
        (e2 / n).sqrt(),
        10.0 * (mi * mi / (e2 / n - mi * mi)).log10()
    );
    // Magnitude statistics of the burst symbols vs all symbols.
    let all: f64 =
        s.z.iter()
            .take(4_000_000)
            .map(|v| v.norm_sqr())
            .sum::<f64>()
            / 4e6;
    println!("mean |z|^2 {all:.0}");
}

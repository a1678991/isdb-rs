//! Frame starts found by the acquisition search alone (no tracking), and
//! their spacing modulo 39 936: shows symbol slips of the timing loop
//! (development tool).
//!
//! cargo run --release -p isdbs-phy --example spacing -- REC

use isdb_iq::Recording;
use isdbs_core::frame::{FRAME_SYMBOLS, W1_CODED_TAIL, W1_TAIL_AT};
use isdbs_phy::sync::tail_metric;

fn main() {
    let path = std::path::PathBuf::from(std::env::args().nth(1).unwrap());
    let mut rec = Recording::open(&path, None, None).unwrap();
    let seg = rec.segments()[0].clone();
    let x = rec
        .read(seg.start, usize::try_from(seg.end - seg.start).unwrap())
        .unwrap();
    let s = isdbs_phy::frontend::symbols(&x, rec.sample_rate, None).unwrap();
    let z = &s.z;
    let mut prev: Option<usize> = None;
    let mut w = 0;
    while w + 2 * FRAME_SYMBOLS < z.len() {
        let (mut best, mut at) = (0.0, w);
        for i in w..w + FRAME_SYMBOLS {
            let m = tail_metric(z, i + W1_TAIL_AT, W1_CODED_TAIL);
            if m > best {
                best = m;
                at = i;
            }
        }
        let d = prev.map(|p| at as i64 - p as i64 - FRAME_SYMBOLS as i64);
        println!(
            "window @{w:>9}: best {best:.3} at {at:>9}  spacing-frame {:?}",
            d
        );
        prev = Some(at);
        w += FRAME_SYMBOLS;
    }
}

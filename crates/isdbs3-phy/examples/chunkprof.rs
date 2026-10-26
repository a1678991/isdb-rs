//! Where `rx::receive` spends its time on one chunk of a streamed ISDB-S3
//! reception (`isdb rx --symbols` / the FPGA path cut the stream into
//! chunks of `FRAMES` frames): synchronisation, equaliser training and
//! re-synchronisation, and the whole call (development tool; plan §0.16).
//!
//! cargo run --release -p isdbs3-phy --example chunkprof -- WORDS_FILE [FRAMES]

use isdb_frontend::format::{Event, Reader};
use isdbs3_core::params::FRAME_SYMBOLS;
use isdbs3_phy::Cplx;
use isdbs3_phy::rx::{RxOptions, receive};
use isdbs3_phy::sync::{synchronise, synchronise_at};
use std::time::Instant;

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let frames: usize = a.get(2).map_or(25, |s| s.parse().unwrap());
    let bytes = std::fs::read(&a[1]).expect("read words");
    let mut r = Reader::new();
    let mut ev = Vec::new();
    r.feed_bytes(&bytes, &mut ev);
    let mut z: Vec<Cplx> = Vec::new();
    for e in ev {
        if let Event::Block { symbols, .. } = e {
            z.extend(
                symbols
                    .iter()
                    .map(|s| Cplx::new(f64::from(s[0]), f64::from(s[1]))),
            );
        }
        if z.len() >= (frames + 1) * FRAME_SYMBOLS {
            break;
        }
    }
    let z = &z[..z.len().min((frames + 1) * FRAME_SYMBOLS)];
    let secs = z.len() as f64 / 33.7561e6;
    println!(
        "{} symbols = {secs:.3} s of signal ({frames} frames + 1)",
        z.len()
    );
    for _ in 0..2 {
        let t = Instant::now();
        let s = synchronise(z).expect("sync");
        let t_sync = t.elapsed().as_secs_f64();
        let t = Instant::now();
        let _ = synchronise_at(z, s.offset);
        let t_at = t.elapsed().as_secs_f64();
        let t = Instant::now();
        let rx = receive(z, &RxOptions::default());
        let t_rx = t.elapsed().as_secs_f64();
        println!(
            "  synchronise {t_sync:.3} s, synchronise_at {t_at:.3} s, receive {t_rx:.3} s ({:.2}× real time); {} frames, {} slots",
            secs / t_rx,
            rx.frames.len(),
            rx.slots.len()
        );
    }
}

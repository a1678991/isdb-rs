//! ISDB-S3 slot sync along a long `Symbols` file, window by window: slot
//! offset, sync Es/N0, residual carrier and the FSync / !FSync split per
//! window, and the header gain and period of the FPGA blocks — to see how
//! the front end's state evolves over a stream (development tool; plan
//! §0.18: a live run lost lock ≈ 30 s after the FPGA was configured, with
//! no loss nearby).
//!
//! cargo run --release -p isdb-frontend --example s3drift -- SYMBOLS_FILE [WINDOW_S]

use isdb_frontend::format::{Event, Reader};
use isdb_frontend::s3sync;
use num_complex::Complex64 as C;

const RS: f64 = 33.7561e6;

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let path = a.get(1).expect("usage: s3drift SYMBOLS_FILE [WINDOW_S]");
    let window_s: f64 = a.get(2).map_or(1.0, |s| s.parse().expect("WINDOW_S"));
    let bytes = std::fs::read(path).expect("read symbols file");
    let mut reader = Reader::new();
    let mut events = Vec::new();
    reader.feed_bytes(&bytes, &mut events);
    reader.finish(&mut events);
    // symbols, with the header gain / period of each block at its start
    let mut x = Vec::<C>::new();
    let mut heads: Vec<(usize, u16, u32)> = Vec::new();
    for e in events {
        match e {
            Event::Block { header, symbols } => {
                heads.push((x.len(), header.gain, header.period));
                x.extend(
                    symbols
                        .iter()
                        .map(|s| C::new(f64::from(s[0]), f64::from(s[1]))),
                );
            }
            Event::Loss { blocks, .. } => {
                println!("loss at symbol {}: {blocks} blocks", x.len());
            }
            Event::Truncated { .. } => {}
        }
    }
    let win = (window_s * RS) as usize;
    println!(
        "{path}: {} symbols ({:.2} s), windows of {window_s} s",
        x.len(),
        x.len() as f64 / RS
    );
    println!("    t s  offset  Es/N0 dB  resid Hz  step rms  FSync/!FSync   gain  period(samples)");
    let mut at = 0;
    while at + win <= x.len() {
        let w = &x[at..at + win];
        let h = heads
            .iter()
            .rev()
            .find(|h| h.0 <= at)
            .copied()
            .unwrap_or((0, 0, 0));
        match s3sync::measure(w) {
            Some(r) => println!(
                "{:7.2}  {:6}  {:8.2}  {:8.1}  {:8.3}  {:5}/{:<6}  {:5.3}  {:.6}",
                at as f64 / RS,
                (r.offset + at) % s3sync::SLOT,
                r.snr_db,
                r.residual_hz(RS),
                r.phase_step_rms,
                r.fsync,
                r.nfsync,
                f64::from(h.1) / 1024.0,
                f64::from(h.2) / f64::from(1u32 << 24),
            ),
            None => println!("{:7.2}  (too short)", at as f64 / RS),
        }
        at += win;
    }
}

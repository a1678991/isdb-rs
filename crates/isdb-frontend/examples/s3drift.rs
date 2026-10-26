//! ISDB-S3 slot sync along a long `Symbols` file, window by window: slot
//! offset, sync Es/N0, residual carrier and the FSync / !FSync split per
//! window, and the header gain and period of the FPGA blocks — to see how
//! the front end's state evolves over a stream (development tool; plan
//! §0.18: a live run lost lock ≈ 30 s after the FPGA was configured, with
//! no loss nearby).
//!
//! The file is streamed: read in chunks, parsed by an incremental
//! [`Reader`], and only the current window's symbols are held, so memory
//! does not grow with the file. (Reading a 45 s capture whole and
//! expanding every symbol took ≈ 27 GB and was killed by the kernel's OOM
//! killer.) Memory does grow with `WINDOW_S`: the window is 16 bytes per
//! symbol and `s3sync::measure` adds about three times that while it
//! searches every position (normalised copy, per-position magnitudes,
//! split I/Q), so ≈ 1.9 GB at the default 1 s and ≈ 9.5 GB at 5 s. A 45 s
//! file ran in 13.7 s under a 4 GB cap at 1 s windows.
//!
//! cargo run --release -p isdb-frontend --example s3drift -- SYMBOLS_FILE [WINDOW_S]

use isdb_frontend::format::{Event, Reader};
use isdb_frontend::s3sync;
use num_complex::Complex64 as C;
use std::io::Read;

const RS: f64 = 33.7561e6;
/// Bytes read from the file at a time.
const READ_CHUNK: usize = 1 << 20;

/// The window being filled: its symbols, the stream position of its first
/// symbol, and the header (gain, period) of the block covering its start.
struct Window {
    x: Vec<C>,
    start: usize,
    head: (u16, u32),
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let path = a.get(1).expect("usage: s3drift SYMBOLS_FILE [WINDOW_S]");
    let window_s: f64 = a.get(2).map_or(1.0, |s| s.parse().expect("WINDOW_S"));
    let win = (window_s * RS) as usize;
    let mut file = std::fs::File::open(path).expect("open symbols file");
    println!("{path}: windows of {window_s} s");
    println!("    t s  offset  Es/N0 dB  resid Hz  step rms  FSync/!FSync   gain  period(samples)");

    let mut reader = Reader::new();
    let mut events = Vec::new();
    let mut buf = vec![0u8; READ_CHUNK];
    // symbols delivered so far (the stream position)
    let mut pos = 0usize;
    // the header of the most recent block (gain, period)
    let mut last_head = (0u16, 0u32);
    let mut w = Window {
        x: Vec::with_capacity(win),
        start: 0,
        head: last_head,
    };
    let mut done = false;
    while !done {
        let n = file.read(&mut buf).expect("read symbols file");
        if n == 0 {
            reader.finish(&mut events);
            done = true;
        } else {
            reader.feed_bytes(&buf[..n], &mut events);
        }
        for e in events.drain(..) {
            match e {
                Event::Block { header, symbols } => {
                    last_head = (header.gain, header.period);
                    for s in symbols {
                        if w.x.is_empty() {
                            w.start = pos;
                            w.head = last_head;
                        }
                        w.x.push(C::new(f64::from(s[0]), f64::from(s[1])));
                        pos += 1;
                        if w.x.len() == win {
                            report(&w);
                            w.x.clear();
                        }
                    }
                }
                // a stream joined mid-block skips words without losing a block
                Event::Loss { blocks: 0, words } if pos == 0 => {
                    println!("joined mid-block: {words} words before the first header");
                }
                Event::Loss { blocks, words } => {
                    println!("loss at symbol {pos}: {blocks} blocks, {words} words skipped");
                }
                Event::Truncated { .. } => {}
            }
        }
    }
    // a final partial window is not measured, as before
    println!("{pos} symbols ({:.2} s) in all", pos as f64 / RS);
}

/// Measure one full window and print its line.
fn report(w: &Window) {
    match s3sync::measure(&w.x) {
        Some(r) => println!(
            "{:7.2}  {:6}  {:8.2}  {:8.1}  {:8.3}  {:5}/{:<6}  {:5.3}  {:.6}",
            w.start as f64 / RS,
            (r.offset + w.start) % s3sync::SLOT,
            r.snr_db,
            r.residual_hz(RS),
            r.phase_step_rms,
            r.fsync,
            r.nfsync,
            f64::from(w.head.0) / 1024.0,
            f64::from(w.head.1) / f64::from(1u32 << 24),
        ),
        None => println!("{:7.2}  (too short)", w.start as f64 / RS),
    }
}

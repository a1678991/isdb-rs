//! ISDB-S3 slot synchronisation and SNR from the known π/2-shift BPSK
//! synchronisation symbols ([`isdb_frontend::s3sync`]) on a `Symbols` file
//! the front end wrote (`isdb iq symbols --system isdb-s3 --out FILE`).
//! `isdb iq s3sync` does the same from the command line.
//!
//! cargo run --release -p isdb-frontend --example s3sync -- SYMBOLS_FILE

// Narrowing casts are proven or fallible here (docs/plans/type-safety.md section 2).
#![warn(clippy::cast_possible_truncation)]
use isdb_frontend::format::{Event, Reader};
use isdb_frontend::s3sync;
use num_complex::Complex64 as C;

fn main() {
    let path = std::env::args().nth(1).expect("usage: s3sync SYMBOLS_FILE");
    let bytes = std::fs::read(&path).expect("read symbols file");
    let mut reader = Reader::new();
    let mut events = Vec::new();
    reader.feed_bytes(&bytes, &mut events);
    reader.finish(&mut events);
    let (mut x, mut losses, mut bits) = (Vec::<C>::new(), 0u64, 0u8);
    for e in events {
        match e {
            Event::Block { header, symbols } => {
                bits = header.bits;
                x.extend(
                    symbols
                        .iter()
                        .map(|s| C::new(f64::from(s[0]), f64::from(s[1]))),
                );
            }
            Event::Loss { blocks, .. } => losses += blocks,
            Event::Truncated { .. } => {}
        }
    }
    println!(
        "{path}: {} symbols ({bits}-bit), {losses} lost blocks",
        x.len()
    );
    let Some(r) = s3sync::measure(&x) else {
        println!("fewer than three slots");
        return;
    };
    println!(
        "slot sync at symbol {} of {}: mean |corr| {:.3} on the sync, {:.3} elsewhere ({} slots)",
        r.offset,
        s3sync::SLOT,
        r.peak,
        r.off_peak,
        r.slots
    );
    println!(
        "SSync on slot parity {}; odd slots: FSync {} · !FSync {}",
        r.ssync_parity, r.fsync, r.nfsync
    );
    println!("SNR on the sync symbols: {:.2} dB (Es/N0)", r.snr_db);
    println!(
        "carrier phase step between SSync slots: mean {:+.4} rad, rms {:.4} rad ({:+.1} Hz at 33.7561 Mbaud)",
        r.phase_step_mean,
        r.phase_step_rms,
        r.residual_hz(33.7561e6)
    );
}

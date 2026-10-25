//! Host throughput of the `Symbols` trailer checks on one core: the
//! version-1/2 CRC-16 (bitwise over words) and the version-3 CRC-32C
//! (slicing by 8), against the link's rate for 6-bit ISDB-S3 symbols
//! (33.756 Mbaud × 12 bits = 50.6 MB/s of payload). Also the whole
//! `Reader` on a clean version-2 and version-3 stream (development tool;
//! plan §0.12).
//!
//! cargo run --release -p isdb-frontend --example crcspeed

use isdb_frontend::format::{Event, Packer, Reader, crc16, crc32c};
use isdb_frontend::params::{Design, Params};
use std::hint::black_box;
use std::time::Instant;

fn mbps(bytes: usize, secs: f64) -> f64 {
    bytes as f64 / secs / 1e6
}

fn time<F: FnMut() -> u64>(reps: usize, mut f: F) -> f64 {
    let t = Instant::now();
    let mut acc = 0u64;
    for _ in 0..reps {
        acc = acc.wrapping_add(f());
    }
    black_box(acc);
    t.elapsed().as_secs_f64()
}

fn main() {
    let link = 33.7561e6 * 12.0 / 8.0 / 1e6;
    println!("link payload rate, 6-bit ISDB-S3: {link:.1} MB/s");
    // one 6-bit, 4 096-symbol payload
    let payload: Vec<u16> = (0..3072u32)
        .map(|i| (i.wrapping_mul(2_654_435_761) >> 9) as u16)
        .collect();
    let bytes = payload.len() * 2;
    let reps = 20_000;
    let t16 = time(reps, || u64::from(crc16(7, black_box(&payload))));
    let t32 = time(reps, || u64::from(crc32c(7, black_box(&payload))));
    println!(
        "CRC-16 (v1/v2): {:8.0} MB/s = {:.0}× the link",
        mbps(bytes * reps, t16),
        mbps(bytes * reps, t16) / link
    );
    println!(
        "CRC-32C (v3):   {:8.0} MB/s = {:.0}× the link",
        mbps(bytes * reps, t32),
        mbps(bytes * reps, t32) / link
    );

    let mut d = Design::new(61.44e6, 33.7561e6, 0.03);
    d.bits = 6;
    let p = Params::design(&d).unwrap();
    for version in [2u16, 3] {
        let mut pk = Packer::with_version(&p, version);
        let mut words = Vec::new();
        for i in 0..(4096 * 400u32) {
            let q = [(i % 61) as i8 - 30, (i % 53) as i8 - 26];
            pk.push(q, 1024, 1 << 24, &mut words);
        }
        let t = Instant::now();
        let mut r = Reader::new();
        let mut ev = Vec::new();
        for c in words.chunks(4096) {
            r.feed(c, &mut ev);
        }
        r.finish(&mut ev);
        let secs = t.elapsed().as_secs_f64();
        let blocks = ev
            .iter()
            .filter(|e| matches!(e, Event::Block { .. }))
            .count();
        println!(
            "Reader, version {version}: {blocks} blocks, {:.0} MB/s of words (incl. unpacking)",
            mbps(words.len() * 2, secs)
        );
    }
}

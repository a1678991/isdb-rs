//! Time each stage of the ISDB-S receive chain on one recording
//! (development tool; VERIFICATION.md "ISDB-S main signal to TS"): front
//! end (IQ → symbols) and frame sync with TMCC on one thread, main-signal
//! decoding (demap, Viterbi, de-interleave, RS) on every core.
//!
//! cargo run --release -p isdbs-phy --example throughput -- REC

use isdb_iq::Recording;
use isdbs_phy::demod::DemodOptions;
use isdbs_phy::ts::decode_all;
use isdbs_phy::{RxOptions, receive};
use std::time::Instant;

fn main() {
    let path = std::path::PathBuf::from(std::env::args().nth(1).expect("REC"));
    let mut rec = Recording::open(&path, None, None).unwrap();
    let seg = rec.segments()[0].clone();
    let x = rec.read(seg.start, (seg.end - seg.start) as usize).unwrap();
    let secs = x.len() as f64 / rec.sample_rate;
    let t = Instant::now();
    let s = isdbs_phy::frontend::symbols(&x, rec.sample_rate, None).unwrap();
    let t_fe = t.elapsed().as_secs_f64();
    let t = Instant::now();
    let r = receive(&s.z, &RxOptions::default());
    let t_rx = t.elapsed().as_secs_f64();
    let t = Instant::now();
    let d = decode_all(&r, &DemodOptions::default());
    let t_dec = t.elapsed().as_secs_f64();
    let packets: usize = d.iter().map(|sf| sf.packets.len()).sum();
    let syms = s.z.len() as f64;
    println!(
        "{}: {secs:.3} s of signal, {} symbols, {} superframes, {packets} packets",
        path.display(),
        s.z.len(),
        d.len()
    );
    for (name, t) in [
        ("front end", t_fe),
        ("frame sync + TMCC", t_rx),
        ("main signal → TS", t_dec),
    ] {
        println!(
            "  {name:18}: {t:6.2} s = {:5.2}× real time, {:5.1} Msym/s",
            secs / t,
            syms / t / 1e6
        );
    }
    let all = t_fe + t_rx + t_dec;
    println!(
        "  {:18}: {all:6.2} s = {:5.2}× real time",
        "total",
        secs / all
    );
}

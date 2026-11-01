//! Where the fixed-point chain's CPU time goes (development tool): each
//! stage alone on a synthetic stream of `FRAMES` frames (default 4,
//! `chain::synth`), one thread, then the whole chain with the LDPC timed
//! apart.
//!
//! cargo run --release -p isdbs3-phy --example chainprof -- [FRAMES]

use isdbs3_phy::chain::ldpc::{FixedLdpc, LdpcDecoder};
use isdbs3_phy::chain::sync::{Nco, SlotSync};
use isdbs3_phy::chain::synth::{SynthOptions, generate};
use isdbs3_phy::chain::{Chain, ChainOptions};
use std::time::Instant;

fn main() {
    let frames: usize = std::env::args().nth(1).map_or(4, |s| s.parse().unwrap());
    let s = generate(
        &SynthOptions {
            snr_db: 16.0,
            carrier_hz: -300.0,
            ..SynthOptions::default()
        },
        frames,
    );
    let secs = s.symbols.len() as f64 / isdbs3_core::params::SYMBOL_RATE;
    println!("{} symbols = {secs:.3} s of signal", s.symbols.len());
    let per = |name: &str, t: f64| println!("  {name:28} {t:7.3} s = {:.3} s per s", t / secs);
    let t = Instant::now();
    let mut nco = Nco::new(0);
    let r: Vec<[i16; 2]> = s.symbols.iter().map(|&x| nco.rotate(x)).collect();
    per("NCO", t.elapsed().as_secs_f64());
    let t = Instant::now();
    let mut sy = SlotSync::new();
    let mut ev = Vec::new();
    for &v in &r {
        sy.push(v, &mut ev);
    }
    per("slot sync", t.elapsed().as_secs_f64());
    let ldpc_time = std::sync::Arc::new(std::sync::Mutex::new(0.0f64));
    let lt = std::sync::Arc::clone(&ldpc_time);
    let mut fixed = FixedLdpc::new();
    let timed = move |code: &isdb_fec::ldpc::Code, words: &[&[i8]]| {
        let t = Instant::now();
        let r = fixed.decode(code, words);
        *lt.lock().unwrap() += t.elapsed().as_secs_f64();
        r
    };
    let mut c = Chain::with_ldpc(ChainOptions::default(), Box::new(timed));
    let mut out = Vec::new();
    let t = Instant::now();
    for &x in &s.symbols {
        c.push(x, &mut out);
        out.clear();
    }
    let all = t.elapsed().as_secs_f64();
    let l = *ldpc_time.lock().unwrap();
    per("whole chain", all);
    per("  of which LDPC (all cores)", l);
    per("  the rest (one thread)", all - l);
}

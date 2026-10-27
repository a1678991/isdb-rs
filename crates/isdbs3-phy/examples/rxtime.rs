//! Where the time of `rx::receive` goes, step by step, on the front end's
//! symbols of one recording (development tool; plan §0.10).
//!
//! cargo run --release -p isdbs3-phy --example rxtime -- REC

use isdb_iq::Recording;
use isdbs3_core::slots::layout;
use isdbs3_phy::frontend::{Trim, symbols};
use isdbs3_phy::main_signal::{
    Ldpc, decode_frame, learned_points, main_symbols, nominal_points, pilots,
};
use isdbs3_phy::rx::noise_of;
use isdbs3_phy::sync::{synchronise, synchronise_at};
use isdbs3_phy::{eq, tmcc};
use std::time::Instant;

fn main() {
    let path = std::path::PathBuf::from(std::env::args().nth(1).expect("REC"));
    let mut rec = Recording::open(&path, None, None).expect("open");
    let fs = rec.sample_rate;
    let seg = rec.segments()[0].clone();
    let x = rec
        .read(
            seg.start,
            usize::try_from(seg.end - seg.start).expect("segment length"),
        )
        .expect("read");
    let s = symbols(&x, fs, None, Trim::Auto).expect("front end");
    let z = &s.z;
    let mut lap = Instant::now();
    let mut step = |name: &str| {
        println!("  {name:28} {:7.3} s", lap.elapsed().as_secs_f64());
        lap = Instant::now();
    };
    let sy0 = synchronise(z).expect("sync");
    step("synchronise (search)");
    let es = 10f64.powf(sy0.sync_snr_db / 10.0);
    let f0 = sy0.frames[0];
    let l = tmcc::llrs(z, &sy0.slots, f0, 1.0 / (2.0 * es)).expect("tmcc llrs");
    let t = tmcc::decode(&l, 50).expect("tmcc");
    let lay = layout(&t.info).expect("layout");
    step("first TMCC");
    let w = eq::train(z, &sy0.slots, f0, &lay).expect("train");
    step("equaliser train");
    let zeq = eq::apply(z, &w);
    step("equaliser apply");
    let sy = synchronise_at(&zeq, sy0.offset).expect("sync");
    step("synchronise_at (no search)");
    let es = 10f64.powf(sy.sync_snr_db / 10.0);
    let (mut t_tmcc, mut t_ms, mut t_pil, mut t_n0, mut t_dec) = (0.0, 0.0, 0.0, 0.0, 0.0);
    let mut slots = 0;
    for &f in &sy.frames {
        let t0 = Instant::now();
        let Some(l) = tmcc::llrs(&zeq, &sy.slots, f, 1.0 / (2.0 * es)) else {
            break;
        };
        let Ok(t) = tmcc::decode(&l, 50) else {
            continue;
        };
        let lay = layout(&t.info).expect("layout");
        t_tmcc += t0.elapsed().as_secs_f64();
        let t0 = Instant::now();
        let Some(ms) = main_symbols(&zeq, &sy.slots, f) else {
            break;
        };
        t_ms += t0.elapsed().as_secs_f64();
        let t0 = Instant::now();
        let pil = pilots(&zeq, &sy.slots, f).expect("pilots");
        let pts_for = |m, r| learned_points(&pil, &lay, m).unwrap_or_else(|| nominal_points(m, r));
        t_pil += t0.elapsed().as_secs_f64();
        let t0 = Instant::now();
        let n0 = noise_of(&ms, &lay, &pts_for);
        t_n0 += t0.elapsed().as_secs_f64();
        let t0 = Instant::now();
        slots += decode_frame(&ms, &lay, n0, &Ldpc::FIXED, &pts_for).len();
        t_dec += t0.elapsed().as_secs_f64();
    }
    println!("  {:28} {t_tmcc:7.3} s", "TMCC of every frame");
    println!("  {:28} {t_ms:7.3} s", "main_symbols (derotate)");
    println!("  {:28} {t_pil:7.3} s", "pilots + learned points");
    println!("  {:28} {t_n0:7.3} s", "noise_of");
    println!(
        "  {:28} {t_dec:7.3} s ({slots} slots)",
        "decode_frame (demap+LDPC+BCH)"
    );
    let t0 = Instant::now();
    let r = isdbs3_phy::rx::receive(z, &isdbs3_phy::rx::RxOptions::default());
    println!(
        "  {:28} {:7.3} s ({} slots)",
        "receive (whole)",
        t0.elapsed().as_secs_f64(),
        r.slots.len()
    );
}

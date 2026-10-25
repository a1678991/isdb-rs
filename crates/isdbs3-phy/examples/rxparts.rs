//! The stages of `rx::receive` as it runs them (parallel where it is), on
//! the front end's symbols of one recording (development tool; plan
//! §0.10).
//!
//! cargo run --release -p isdbs3-phy --example rxparts -- REC

use isdb_iq::Recording;
use isdbs3_phy::frontend::{Trim, symbols};
use isdbs3_phy::rx::{RxOptions, receive};
use std::time::Instant;

fn main() {
    let path = std::path::PathBuf::from(std::env::args().nth(1).expect("REC"));
    let mut rec = Recording::open(&path, None, None).expect("open");
    let fs = rec.sample_rate;
    let seg = rec.segments()[0].clone();
    let x = rec
        .read(seg.start, (seg.end - seg.start) as usize)
        .expect("read");
    let s = symbols(&x, fs, None, Trim::Auto).expect("front end");
    for (name, ldpc) in [
        ("receive", isdbs3_phy::main_signal::Ldpc::FIXED),
        (
            "receive, LDPC 0 iterations",
            isdbs3_phy::main_signal::Ldpc::Fixed {
                max_iter: 0,
                input_scale: 1.0,
                offset: 1,
                simd: true,
            },
        ),
    ] {
        let t = Instant::now();
        let r = receive(
            &s.z,
            &RxOptions {
                ldpc,
                ..RxOptions::default()
            },
        );
        println!(
            "  {name:28} {:7.3} s ({} slots)",
            t.elapsed().as_secs_f64(),
            r.slots.len()
        );
    }
    let t = Instant::now();
    let sy = isdbs3_phy::sync::synchronise(&s.z).expect("sync");
    println!("  {:28} {:7.3} s", "synchronise", t.elapsed().as_secs_f64());
    // per-frame preparation (as receive does it, in parallel) and the batch
    use isdbs3_phy::main_signal::{
        FrameInput, decode_frames, learned_points, main_symbols, nominal_points, pilots,
    };
    let es = 10f64.powf(sy.sync_snr_db / 10.0);
    let t = Instant::now();
    let frames: Vec<usize> = sy.frames.clone();
    let prep: Vec<_> = std::thread::scope(|sc| {
        let hs: Vec<_> = frames
            .iter()
            .map(|&f| {
                let (z, sl) = (&s.z, &sy.slots);
                sc.spawn(move || {
                    let l = isdbs3_phy::tmcc::llrs(z, sl, f, 1.0 / (2.0 * es))?;
                    let t = isdbs3_phy::tmcc::decode(&l, 50).ok()?;
                    let lay = isdbs3_core::slots::layout(&t.info)?;
                    Some((lay, main_symbols(z, sl, f)?, pilots(z, sl, f)?))
                })
            })
            .collect();
        hs.into_iter().filter_map(|h| h.join().unwrap()).collect()
    });
    println!(
        "  {:28} {:7.3} s ({} frames)",
        "prepare frames (parallel)",
        t.elapsed().as_secs_f64(),
        prep.len()
    );
    let pts: Vec<_> = prep
        .iter()
        .map(|(lay, _, pil)| {
            move |m, r| learned_points(pil, lay, m).unwrap_or_else(|| nominal_points(m, r))
        })
        .collect();
    let t = Instant::now();
    let n0s: Vec<f64> = prep
        .iter()
        .zip(&pts)
        .map(|((lay, ms, _), p)| isdbs3_phy::rx::noise_of(ms, lay, p))
        .collect();
    println!(
        "  {:28} {:7.3} s",
        "noise_of (sequential)",
        t.elapsed().as_secs_f64()
    );
    let inputs: Vec<FrameInput<'_>> = prep
        .iter()
        .zip(&pts)
        .zip(&n0s)
        .map(|(((lay, ms, _), p), &n0)| FrameInput {
            ms,
            layout: lay,
            n0,
            pts_for: p,
        })
        .collect();
    for (name, it) in [("decode_frames", 50), ("decode_frames, 0 iterations", 0)] {
        let t = Instant::now();
        let _ = decode_frames(
            &inputs,
            &isdbs3_phy::main_signal::Ldpc::Fixed {
                max_iter: it,
                input_scale: 1.0,
                offset: 1,
                simd: true,
            },
        );
        println!("  {name:28} {:7.3} s", t.elapsed().as_secs_f64());
    }
}

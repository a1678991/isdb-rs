//! The fixed-point LDPC decoder (`isdb_fec::ldpc::fixed`) against the
//! float one on the codewords of real ISDB-S3 recordings: failures and
//! mean iterations per decoder setting, and throughput per lane width
//! (development tool; plan §0.9).
//!
//! cargo run --release -p isdbs3-phy --example ldpcfixed -- [--frames N] [--scale S] [--offset O] REC [REC…]
//!
//! For each recording, every whole frame's codewords are demapped as in
//! `isdb rx` (equalised, learned constellation, measured N0) and decoded
//! with the float decoder (50 iterations, ×0.8) and with the fixed-point
//! one at the given input scale and offset; `SWEEP=1` sweeps scale and
//! offset instead.

use isdb_fec::ldpc::fixed::{Decoder, Params, quantise};
use isdb_iq::Recording;
use isdbs3_core::slots::layout;
use isdbs3_phy::frontend::{Trim, symbols};
use isdbs3_phy::main_signal::{codeword_llrs, learned_points, main_symbols, pilots};
use isdbs3_phy::sync::synchronise;
use isdbs3_phy::tmcc::{decode, llrs};
use std::time::Instant;

struct Word {
    rate: isdbs3_core::CodeRate,
    llr: Vec<f32>,
}

fn words_of(path: &str, max_frames: usize) -> Vec<Word> {
    let mut rec = Recording::open(std::path::Path::new(path), None, None).expect("open");
    let fs = rec.sample_rate;
    let mut out = Vec::new();
    for seg in rec.segments().clone() {
        let x = rec
            .read(seg.start, (seg.end - seg.start) as usize)
            .expect("read");
        let s = symbols(&x, fs, None, Trim::Auto).expect("front end");
        let Some(sy0) = synchronise(&s.z) else {
            continue;
        };
        // equalise as `rx::receive` does: taps from the first frame whose
        // TMCC decodes
        let es0 = 10f64.powf(sy0.sync_snr_db / 10.0);
        let w = sy0.frames.iter().find_map(|&f| {
            let l = llrs(&s.z, &sy0.slots, f, 1.0 / (2.0 * es0))?;
            let lay = layout(&decode(&l, 50).ok()?.info)?;
            isdbs3_phy::eq::train(&s.z, &sy0.slots, f, &lay)
        });
        let z = w.map_or(s.z.clone(), |w| isdbs3_phy::eq::apply(&s.z, &w));
        let Some(sy) = synchronise(&z) else { continue };
        let es = 10f64.powf(sy.sync_snr_db / 10.0);
        for &f in sy.frames.iter().take(max_frames) {
            let Some(l) = llrs(&z, &sy.slots, f, 1.0 / (2.0 * es)) else {
                continue;
            };
            let Ok(t) = decode(&l, 50) else { continue };
            let Some(lay) = layout(&t.info) else { continue };
            let (Some(ms), Some(pil)) = (main_symbols(&z, &sy.slots, f), pilots(&z, &sy.slots, f))
            else {
                continue;
            };
            let Some(pts) = learned_points(&pil, &lay, lay[0].modulation) else {
                continue;
            };
            let n0 = isdbs3_phy::rx::noise_of(&ms, &lay, &|_, _| pts.clone());
            for sl in lay.iter().filter(|s| !s.dummy) {
                out.push(Word {
                    rate: sl.rate,
                    llr: codeword_llrs(&ms, sl, &pts, n0),
                });
            }
        }
    }
    out
}

fn float_stats(words: &[Word]) -> (usize, f64) {
    let (mut fail, mut it) = (0, 0usize);
    let mut bits = Vec::new();
    for w in words {
        let mut l = w.llr.clone();
        match isdbs3_core::ldpc::code(w.rate).decode(&mut l, &mut bits, 50, 0.8) {
            Ok(d) => it += d.iterations,
            Err(e) => {
                fail += 1;
                it += e.iterations;
            }
        }
    }
    (fail, it as f64 / words.len() as f64)
}

fn fixed_stats<const W: usize>(words: &[Word], scale: f32, p: Params) -> (usize, f64, f64) {
    let q: Vec<Vec<i8>> = words
        .iter()
        .map(|w| {
            let mut v = Vec::new();
            quantise(&w.llr, scale, &mut v);
            v
        })
        .collect();
    let mut dec = Decoder::<W>::default();
    let (mut fail, mut it) = (0, 0usize);
    let t0 = Instant::now();
    // every codeword of these recordings has the same rate
    let code = isdbs3_core::ldpc::code(words[0].rate);
    assert!(words.iter().all(|w| w.rate == words[0].rate));
    let refs: Vec<&[i8]> = q.iter().map(Vec::as_slice).collect();
    {
        for (r, _) in dec.decode_all(code, &refs, p) {
            match r {
                Ok(d) => it += d.iterations,
                Err(e) => {
                    fail += 1;
                    it += e.iterations;
                }
            }
        }
    }
    let cw_s = words.len() as f64 / t0.elapsed().as_secs_f64();
    (fail, it as f64 / words.len() as f64, cw_s)
}

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mut opt = |name: &str, default: f64| -> f64 {
        match args.iter().position(|a| a == name) {
            Some(i) => {
                let v = args[i + 1].parse().expect("number");
                args.drain(i..=i + 1);
                v
            }
            None => default,
        }
    };
    let frames = opt("--frames", 8.0) as usize;
    let scale = opt("--scale", 2.0) as f32;
    let offset = opt("--offset", 1.0) as i8;
    let mut words = Vec::new();
    for a in &args {
        let w = words_of(a, frames);
        println!("{a}: {} codewords", w.len());
        words.extend(w);
    }
    if std::env::var_os("HIST").is_some() {
        let mut a: Vec<f32> = words
            .iter()
            .flat_map(|w| w.llr.iter().map(|x| x.abs()))
            .collect();
        a.sort_by(f32::total_cmp);
        let q = |p: f64| a[((a.len() - 1) as f64 * p) as usize];
        println!(
            "|LLR| percentiles: 1% {:.1} 10% {:.1} 50% {:.1} 90% {:.1} 99% {:.1} 99.9% {:.1} max {:.1}",
            q(0.01),
            q(0.1),
            q(0.5),
            q(0.9),
            q(0.99),
            q(0.999),
            q(1.0)
        );
        return;
    }
    let t0 = Instant::now();
    let (ff, fi) = float_stats(&words);
    let fcw = words.len() as f64 / t0.elapsed().as_secs_f64();
    println!(
        "float ×0.8: {ff} failed of {}, mean {fi:.2} iterations, {fcw:.0} cw/s (1 thread)",
        words.len()
    );
    if std::env::var_os("ITERS").is_some() {
        for max_iter in [5usize, 10, 20, 50, 100, 200] {
            let (f, i, _) = fixed_stats::<16>(&words, scale, Params { max_iter, offset });
            println!(
                "fixed i8 scale {scale} offset {offset}, max {max_iter:3} iterations: {f} failed, mean {i:.2}"
            );
        }
        for max_iter in [50usize, 200] {
            let (mut fail, mut bits) = (0, Vec::new());
            for w in &words {
                let mut l = w.llr.clone();
                fail += usize::from(
                    isdbs3_core::ldpc::code(w.rate)
                        .decode(&mut l, &mut bits, max_iter, 0.8)
                        .is_err(),
                );
            }
            println!("float ×0.8, max {max_iter:3} iterations: {fail} failed");
        }
        return;
    }
    if std::env::var_os("SWEEP").is_some() {
        let scales: Vec<f32> = std::env::var("SCALES").map_or(vec![0.25, 0.5, 1.0, 2.0], |v| {
            v.split(',').map(|x| x.parse().expect("scale")).collect()
        });
        for &scale in &scales {
            for offset in [0i8, 1, 2] {
                let (f, i, _) = fixed_stats::<32>(
                    &words,
                    scale,
                    Params {
                        max_iter: 50,
                        offset,
                    },
                );
                println!(
                    "fixed i8 scale {scale} offset {offset}: {f} failed, mean {i:.2} iterations"
                );
            }
        }
        return;
    }
    let p = Params {
        max_iter: 50,
        offset,
    };
    for (w, (f, i, cw)) in [
        (1, fixed_stats::<1>(&words, scale, p)),
        (8, fixed_stats::<8>(&words, scale, p)),
        (16, fixed_stats::<16>(&words, scale, p)),
        (32, fixed_stats::<32>(&words, scale, p)),
        (64, fixed_stats::<64>(&words, scale, p)),
    ] {
        println!(
            "fixed i8 scale {scale} offset {offset}, {w:2} lanes: {f} failed, mean {i:.2} iterations, {cw:.0} cw/s (1 thread)"
        );
    }
    println!("back end: {}", isdb_fec::simd::BACKEND);
}

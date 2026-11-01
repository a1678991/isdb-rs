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

mod common;

use isdb_fec::ldpc::fixed::{Decoder, Params, quantise};
use std::time::Instant;

struct Word {
    rate: isdbs3_core::CodeRate,
    llr: Vec<f32>,
}

fn words_of(path: &str, max_frames: usize) -> Vec<Word> {
    let mut out = Vec::new();
    common::for_each_word(path, max_frames, 0.0, |w| {
        if let common::Word::Main(rate, llr, _) = w {
            out.push(Word { rate, llr });
        }
    });
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

/// The value of option `name` (parsed as `T`, removed from `args`), or
/// `default`.
fn opt<T: std::str::FromStr>(args: &mut Vec<String>, name: &str, default: T) -> T
where
    T::Err: std::fmt::Debug,
{
    match args.iter().position(|a| a == name) {
        Some(i) => {
            let v = args[i + 1].parse().expect("number");
            args.drain(i..=i + 1);
            v
        }
        None => default,
    }
}

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let frames: usize = opt(&mut args, "--frames", 8);
    let scale: f32 = opt(&mut args, "--scale", 2.0);
    let offset: i8 = opt(&mut args, "--offset", 1);
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
        // the `permille`-th per-mille point (0 = min, 1000 = max)
        let q = |permille: usize| a[(a.len() - 1) * permille / 1000];
        println!(
            "|LLR| percentiles: 1% {:.1} 10% {:.1} 50% {:.1} 90% {:.1} 99% {:.1} 99.9% {:.1} max {:.1}",
            q(10),
            q(100),
            q(500),
            q(900),
            q(990),
            q(999),
            q(1000)
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
            let (f, i, _) = fixed_stats::<16>(&words, scale, Params::new(max_iter, offset));
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
                let (f, i, _) = fixed_stats::<32>(&words, scale, Params::new(50, offset));
                println!(
                    "fixed i8 scale {scale} offset {offset}: {f} failed, mean {i:.2} iterations"
                );
            }
        }
        return;
    }
    let p = Params::new(50, offset);
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

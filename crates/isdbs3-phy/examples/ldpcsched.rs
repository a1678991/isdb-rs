//! The fixed-point LDPC decoder's schedules (`isdb_fec::ldpc::fixed::Schedule`)
//! on the codewords of real ISDB-S3 recordings: per schedule and per
//! iteration limit, the failed codewords, the mean iterations and the
//! iteration histogram, to choose the FPGA decoder's step and limit
//! (docs/plans/isdbs3-fpga-rx.md, "The LDPC schedule contract";
//! development tool).
//!
//! cargo run --release -p isdbs3-phy --example ldpcsched -- [--frames N] [--steps 1,34,68,187,374] [--limits 5,8,10,12,20,50] [--noise V] REC [REC…]
//!
//! Every whole frame's codewords (up to N per segment, default all) are
//! demapped as in `isdb rx` and quantised as `Ldpc::FIXED` does (input
//! scale 1, offset 1). Each schedule decodes every codeword once at the
//! largest limit; a decode's trajectory does not depend on the limit
//! (the limit only stops it), so the result at a lower limit L is read
//! off that run: converged iff it converged within L iterations, the
//! iterations min(its count, L). The example checks that reading against
//! a direct decode at the second-largest limit and stops if they differ.
//! `--noise V` adds complex Gaussian noise of variance V (per complex
//! symbol, unit sync amplitude) to the main symbols before demapping
//! (the pilots as received, N0 measured on the noisy symbols): the
//! recordings made worse, to see the schedules near threshold, where
//! the recordings themselves no longer fail. Recordings are read in
//! place; nothing is written.

mod common;

use isdb_fec::ldpc::fixed::{Decoder, LaneResult, Params, Schedule, quantise};
use isdbs3_core::CodeRate;
use isdbs3_phy::main_signal::{LANES, Ldpc};
use std::time::Instant;

/// The value of option `name` (removed from `args`), or `default`.
fn opt(args: &mut Vec<String>, name: &str, default: &str) -> String {
    match args.iter().position(|a| a == name) {
        Some(i) => {
            let v = args[i + 1].clone();
            args.drain(i..=i + 1);
            v
        }
        None => default.to_string(),
    }
}

fn list(s: &str) -> Vec<usize> {
    s.split(',').map(|x| x.parse().expect("number")).collect()
}

/// Every codeword of `words` (one rate) decoded with `p`, in order, on
/// all threads (each a 16-lane decoder streaming its share).
fn decode(code: &isdb_fec::ldpc::Code, words: &[Vec<i8>], p: Params) -> Vec<LaneResult> {
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get());
    let per = words.len().div_ceil(threads).max(LANES);
    std::thread::scope(|sc| {
        let hs: Vec<_> = words
            .chunks(per)
            .map(|ch| {
                sc.spawn(move || {
                    let refs: Vec<&[i8]> = ch.iter().map(Vec::as_slice).collect();
                    Decoder::<LANES>::default()
                        .decode_all(code, &refs, p)
                        .into_iter()
                        .map(|(r, _)| r)
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        hs.into_iter()
            .flat_map(|h| h.join().expect("decoder thread"))
            .collect()
    })
}

/// The result at limit `lim` of a decode run at a higher limit.
fn at_limit(r: LaneResult, lim: usize) -> (bool, usize) {
    match r {
        Ok(d) if d.iterations <= lim => (true, d.iterations),
        Ok(_) => (false, lim),
        Err(e) => (false, e.iterations.min(lim)),
    }
}

fn name(s: Schedule) -> String {
    match s {
        Schedule::Sequential => "sequential".into(),
        Schedule::Layered { step } => format!("layered {step}"),
    }
}

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let frames: usize = opt(&mut args, "--frames", "1000000")
        .parse()
        .expect("frames");
    let steps = list(&opt(&mut args, "--steps", "1,34,68,187,374"));
    let mut limits = list(&opt(&mut args, "--limits", "5,8,10,12,20,50"));
    limits.sort_unstable();
    let noise: f64 = opt(&mut args, "--noise", "0").parse().expect("noise");
    let Ldpc::Fixed {
        input_scale,
        offset,
        ..
    } = Ldpc::FIXED
    else {
        unreachable!()
    };
    // codewords per rate, quantised
    let mut words: Vec<(CodeRate, Vec<Vec<i8>>)> = Vec::new();
    for a in &args {
        let t0 = Instant::now();
        let (mut n, mut n0_sum) = (0, 0.0);
        common::for_each_word(a, frames, noise, |w| {
            let common::Word::Main(rate, llr, n0) = w else {
                return;
            };
            let mut q = Vec::new();
            quantise(&llr, input_scale, &mut q);
            match words.iter_mut().find(|(r, _)| *r == rate) {
                Some((_, v)) => v.push(q),
                None => words.push((rate, vec![q])),
            }
            n += 1;
            n0_sum += n0;
        });
        // N0 per complex symbol against the unit sync amplitude, and its
        // inverse in dB (the sync-symbol SNR of the demapper)
        println!(
            "{a}: {n} codewords, mean N0 {:.4} (1/N0 {:.2} dB) ({:.0} s)",
            n0_sum / f64::from(n.max(1)),
            -10.0 * (n0_sum / f64::from(n.max(1))).log10(),
            t0.elapsed().as_secs_f64()
        );
    }
    let total: usize = words.iter().map(|(_, v)| v.len()).sum();
    let rates: Vec<String> = words
        .iter()
        .map(|(r, v)| format!("{}/120 × {}", r.numerator(), v.len()))
        .collect();
    println!(
        "{total} codewords ({}), input scale {input_scale}, offset {offset}, {LANES} lanes",
        rates.join(", ")
    );
    let top = *limits.last().expect("a limit");
    let check = limits.len().checked_sub(2).map(|i| limits[i]);
    let schedules: Vec<Schedule> = std::iter::once(Schedule::Sequential)
        .chain(steps.iter().map(|&step| Schedule::Layered { step }))
        .collect();
    println!();
    println!(
        "| schedule | limit | failed | mean iterations | histogram (iterations: codewords; F = failed) |"
    );
    println!("|---|---|---|---|---|");
    for &schedule in &schedules {
        let t0 = Instant::now();
        let mut results: Vec<LaneResult> = Vec::with_capacity(total);
        for (rate, ws) in &words {
            let code = isdbs3_core::ldpc::code(*rate);
            let p = Params {
                max_iter: top,
                offset,
                schedule,
            };
            let r = decode(code, ws, p);
            if let Some(lim) = check {
                // the limit-free reading, checked once per schedule
                let direct = decode(code, ws, Params { max_iter: lim, ..p });
                for (i, (&a, &b)) in r.iter().zip(&direct).enumerate() {
                    let (ok, it) = at_limit(a, lim);
                    let (ok2, it2) = at_limit(b, lim);
                    assert_eq!((ok, it), (ok2, it2), "{} codeword {i}", name(schedule));
                }
            }
            results.extend(r);
        }
        let secs = t0.elapsed().as_secs_f64();
        for &lim in &limits {
            let mut hist = vec![0usize; lim + 1];
            let (mut fail, mut sum) = (0, 0);
            for &r in &results {
                let (ok, it) = at_limit(r, lim);
                sum += it;
                if ok {
                    hist[it] += 1;
                } else {
                    fail += 1;
                }
            }
            let h: Vec<String> = hist
                .iter()
                .enumerate()
                .filter(|&(_, &c)| c > 0)
                .map(|(i, c)| format!("{i}: {c}"))
                .chain((fail > 0).then(|| format!("F: {fail}")))
                .collect();
            println!(
                "| {} | {lim} | {fail} | {:.3} | {} |",
                name(schedule),
                sum as f64 / total as f64,
                h.join(", ")
            );
        }
        eprintln!("{}: {secs:.1} s", name(schedule));
    }
}

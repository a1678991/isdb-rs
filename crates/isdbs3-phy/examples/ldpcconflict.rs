//! Why wide layered steps converged worse under the first contract: rules
//! for a variable two checks of one layered step share (a conflicting
//! pair, `isdb_fec::ldpc::fixed::Schedule::Layered`), on the codewords of
//! real recordings made noisier (development tool;
//! docs/plans/isdbs3-fpga-rx.md piece 1 "Results").
//!
//! cargo run --release -p isdbs3-phy --example ldpcconflict -- [--noise V] [--words N] [--steps 68,187,374] [--limit 50] REC [REC…]
//!
//! Rules, each a scalar decoder with the fixed-point per-check arithmetic
//! of `ldpc::fixed`, every check of a step reading the step's starting L:
//!
//! - `LaterWins`: each edge writes `L_v = sat(t_e + r_e)` in check order
//!   (the first contract);
//! - `Delta`: the contract now, `L_v = sat(t_e₁ + r_e₁,new + Σᵢ₌₂ (r_eᵢ,new
//!   − r_eᵢ,old))` over the variable's edges in the step, in a wide
//!   integer saturated once; checked equal to the library's
//!   `Layered { step }` decoder on every codeword;
//! - `DeltaL`: the variant first measured, `sat(L_v + Σ (r_new −
//!   r_old))` over all the edges (differs from `Delta` only where
//!   `t_e₁ = sat(L_v − r_e₁,old)` saturates);
//! - `Split`: `LaterWins`, but a check that shares a variable with an
//!   earlier check of its step starts a new step (no conflicting pair is
//!   ever inside a step): the steps become irregular.

mod common;

use isdb_fec::ldpc::Code;
use isdb_fec::ldpc::fixed::{Decoder, Params, Schedule, quantise};
use isdbs3_core::CodeRate;

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

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Rule {
    LaterWins,
    Delta,
    DeltaL,
    Split,
}

/// The steps of `Layered { step }`, or with `split` cut further so that
/// no two checks of a step share a variable.
fn steps(code: &Code, step: usize, split: bool) -> Vec<Vec<usize>> {
    let (p, q) = (code.checks(), code.q());
    let mut out = Vec::new();
    for l in 0..q {
        let layer: Vec<usize> = (l..p).step_by(q).collect();
        for ch in layer.chunks(step) {
            if !split {
                out.push(ch.to_vec());
                continue;
            }
            let mut cur: Vec<usize> = Vec::new();
            let mut seen = std::collections::HashSet::new();
            for &c in ch {
                if code.check(c).iter().any(|v| seen.contains(v)) {
                    out.push(std::mem::take(&mut cur));
                    seen.clear();
                }
                seen.extend(code.check(c).iter().copied());
                cur.push(c);
            }
            out.push(cur);
        }
    }
    out
}

/// One codeword: (converged, iterations).
fn run(code: &Code, llr: &[i8], steps: &[Vec<usize>], rule: Rule, limit: usize) -> (bool, usize) {
    let mut l = llr.to_vec();
    let mut r: Vec<Vec<i8>> = (0..code.checks())
        .map(|c| vec![0; code.check(c).len()])
        .collect();
    let hard = |l: &[i8]| l.iter().map(|&x| u8::from(x < 0)).collect::<Vec<u8>>();
    if code.syndrome_weight(&hard(&l)) == 0 {
        return (true, 0);
    }
    let mut acc: Vec<i32> = vec![0; l.len()];
    let mut touched: Vec<usize> = Vec::new();
    for it in 1..=limit {
        for st in steps {
            let mut outs = Vec::with_capacity(st.len());
            for &c in st {
                let vars = code.check(c);
                let t: Vec<i8> = vars
                    .iter()
                    .zip(&r[c])
                    .map(|(&v, &re)| l[v as usize].saturating_sub(re))
                    .collect();
                let (mut m1, mut m2, mut im, mut par) = (i8::MAX, i8::MAX, 0, false);
                for (e, &x) in t.iter().enumerate() {
                    let a = x.saturating_abs();
                    par ^= x < 0;
                    if a < m1 {
                        m2 = m1;
                        m1 = a;
                        im = e;
                    } else if a < m2 {
                        m2 = a;
                    }
                }
                let new: Vec<i8> = t
                    .iter()
                    .enumerate()
                    .map(|(e, &x)| {
                        let m = if e == im { m2 } else { m1 };
                        let mag = m.saturating_sub(1).max(0);
                        if par ^ (x < 0) { -mag } else { mag }
                    })
                    .collect();
                outs.push((c, t, new));
            }
            match rule {
                Rule::LaterWins | Rule::Split => {
                    for (c, t, new) in outs {
                        for ((&v, &te), &re) in code.check(c).iter().zip(&t).zip(&new) {
                            l[v as usize] = te.saturating_add(re);
                        }
                        r[c] = new;
                    }
                }
                Rule::Delta => {
                    // the contract: sat(t₁ + r₁,new + Σᵢ₌₂ (rᵢ,new − rᵢ,old)),
                    // edges per variable in check order, i32 then i8
                    let mut first: std::collections::HashMap<usize, i32> =
                        std::collections::HashMap::new();
                    for (c, t, new) in outs {
                        for (e, &v) in code.check(c).iter().enumerate() {
                            let v = v as usize;
                            let (te, ro, rn) =
                                (i32::from(t[e]), i32::from(r[c][e]), i32::from(new[e]));
                            *first.entry(v).or_insert_with(|| {
                                touched.push(v);
                                te + ro // so that adding rn − ro gives te + rn
                            }) += rn - ro;
                        }
                        r[c] = new;
                    }
                    for v in touched.drain(..) {
                        let x = first[&v].clamp(-128, 127);
                        l[v] = i8::try_from(x).expect("clamped");
                    }
                }
                Rule::DeltaL => {
                    for (c, _, new) in outs {
                        for ((&v, &old), &re) in code.check(c).iter().zip(&r[c]).zip(&new) {
                            let v = v as usize;
                            if acc[v] == 0 {
                                touched.push(v);
                            }
                            // a zero sum leaves `touched` with a duplicate:
                            // harmless, the value is written twice
                            acc[v] += i32::from(re) - i32::from(old);
                        }
                        r[c] = new;
                    }
                    for v in touched.drain(..) {
                        let x = (i32::from(l[v]) + acc[v]).clamp(-128, 127);
                        l[v] = i8::try_from(x).expect("clamped");
                        acc[v] = 0;
                    }
                }
            }
        }
        if code.syndrome_weight(&hard(&l)) == 0 {
            return (true, it);
        }
    }
    (false, limit)
}

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let noise: f64 = opt(&mut args, "--noise", "0.035").parse().expect("noise");
    let max_words: usize = opt(&mut args, "--words", "2000").parse().expect("words");
    let step_list: Vec<usize> = opt(&mut args, "--steps", "68,187,374")
        .split(',')
        .map(|s| s.parse().expect("step"))
        .collect();
    let limit: usize = opt(&mut args, "--limit", "50").parse().expect("limit");
    let mut words: Vec<Vec<i8>> = Vec::new();
    let per = max_words.div_ceil(args.len().max(1));
    for a in &args {
        let mut n = 0;
        common::for_each_word(a, usize::MAX, noise, |w| {
            if let common::Word::Main(rate, llr, _) = w
                && rate == CodeRate::new(93).expect("rate")
                && n < per
            {
                let mut q = Vec::new();
                quantise(&llr, 1.0, &mut q);
                words.push(q);
                n += 1;
            }
        });
    }
    let code = isdbs3_core::ldpc::code(CodeRate::new(93).expect("rate"));
    println!(
        "{} codewords of 93/120, extra noise {noise}, limit {limit}",
        words.len()
    );
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get());
    for &step in &step_list {
        let plain = steps(code, step, false);
        let split = steps(code, step, true);
        println!(
            "step {step}: {} steps per iteration ({} with conflicting pairs split)",
            plain.len(),
            split.len()
        );
        // the library's decoder, for the contract rule's check
        let p = Params {
            max_iter: limit,
            offset: 1,
            schedule: Schedule::Layered { step },
        };
        let refs: Vec<&[i8]> = words.iter().map(Vec::as_slice).collect();
        let lib = Decoder::<16>::default().decode_all(code, &refs, p);
        for rule in [Rule::LaterWins, Rule::Delta, Rule::DeltaL, Rule::Split] {
            let st = if rule == Rule::Split { &split } else { &plain };
            let res: Vec<(bool, usize)> = std::thread::scope(|sc| {
                let hs: Vec<_> = words
                    .chunks(words.len().div_ceil(threads).max(1))
                    .map(|ch| {
                        sc.spawn(move || {
                            ch.iter()
                                .map(|w| run(code, w, st, rule, limit))
                                .collect::<Vec<_>>()
                        })
                    })
                    .collect();
                hs.into_iter()
                    .flat_map(|h| h.join().expect("thread"))
                    .collect()
            });
            if rule == Rule::Delta {
                for (i, (a, (b, _))) in res.iter().zip(&lib).enumerate() {
                    let bi = match b {
                        Ok(d) => (true, d.iterations),
                        Err(e) => (false, e.iterations),
                    };
                    assert_eq!(
                        *a, bi,
                        "codeword {i}: the scalar contract differs from the library"
                    );
                }
            }
            let fail = res.iter().filter(|r| !r.0).count();
            let mean = res.iter().map(|r| r.1).sum::<usize>() as f64 / res.len() as f64;
            for lim in [10usize, 12, 20] {
                let f = res.iter().filter(|r| !r.0 || r.1 > lim).count();
                print!("  limit {lim}: {f:5} failed;");
            }
            println!("  {rule:?}: limit {limit}: {fail} failed, mean {mean:.2} iterations");
        }
    }
}

//! The slow loop's checks on the training sums (`isdb_radio::chain`
//! "Checks", issues #27 / #29) against real sums: the chain's fixed-point
//! model (`isdbs3_phy::chain`) over a `Symbols` words file gives each
//! frame's sums, as the FPGA latches them; for each, R's structure
//! (`sums_shape`: imaginary diagonal, diagonal spread, Toeplitz
//! deviation, all over the mean diagonal) and the `fit` of its solve.
//! Then, for every `EVERY`-th frame, each of the 308 register words read
//! as 0 (the value a foreign `direct_reg_access` client gives, #29): how
//! many of these corrupted sets the default `SumsCheck` catches, alone and
//! followed by the default `Gate` against the clean solve of 5 frames
//! before, and how far the taps of the ones they do not catch are from the
//! clean solve (`‖Δw‖ / ‖w‖`), for words of R and of p apart.
//!
//! Prints one line per frame (frame, count, the shape's three shares, the
//! fit and its MER), the first frame's diagonal profile, and a summary:
//! the extremes over the frames and the corruption table.
//!
//! cargo run --release -p isdb-cli --example sumscheck -- WORDS_FILE [MAX_SECONDS [EVERY]]

use isdb_frontend::control::chain::{R_ENTRIES, TrainingSums, sums_from_words, sums_to_words};
use isdb_radio::chain::{Gate, Judged, SumsCheck, check_sums, fit, residual, sums_shape};
use isdbs3_core::params::SYMBOL_RATE;
use isdbs3_phy::chain::eq::{r_index, solve_training, to_training};
use isdbs3_phy::chain::input::{Input, for_each};
use isdbs3_phy::chain::{Chain, ChainOptions, Output};

type Taps = [[i16; 2]; 11];

fn norm(t: &Taps) -> f64 {
    t.iter()
        .map(|w| f64::from(w[0]).powi(2) + f64::from(w[1]).powi(2))
        .sum::<f64>()
        .sqrt()
}

fn distance(a: &Taps, b: &Taps) -> f64 {
    a.iter()
        .zip(b)
        .map(|(x, y)| {
            (f64::from(x[0]) - f64::from(y[0])).powi(2)
                + (f64::from(x[1]) - f64::from(y[1])).powi(2)
        })
        .sum::<f64>()
        .sqrt()
}

/// The checks the slow loop applies: R's structure, then the solve's fit
/// (caught by the checks), then the gate against `good` (caught by the
/// gate); the solved taps.
fn caught(s: &TrainingSums, check: &SumsCheck, good: &Judged) -> (bool, bool, Option<Taps>) {
    if check_sums(s, check).is_some() {
        return (true, true, None);
    }
    let Some(t) = solve_training(s) else {
        return (true, true, None);
    };
    let f = fit(s, &t);
    if f > check.fit {
        return (true, true, Some(t));
    }
    let g = Gate::default();
    let gated = residual(s, &t) > g.residual
        || g.against(
            good,
            &Judged {
                taps: t,
                fit: Some(f),
            },
        )
        .is_some();
    (false, gated, Some(t))
}

/// Corrupted sets of words of R or of p, missed by the checks alone or by
/// the checks and the gate.
#[derive(Default, Clone, Copy)]
struct Corruptions {
    sets: u64,
    caught: u64,
    missed: u64,
    /// Missed sets whose taps moved by more than 2 %, 5 %, 20 %.
    missed_over: [u64; 3],
    missed_max: f64,
}

impl Corruptions {
    fn count(&mut self, caught: bool, moved: f64) {
        self.sets += 1;
        if caught {
            self.caught += 1;
            return;
        }
        self.missed += 1;
        self.missed_max = self.missed_max.max(moved);
        for (n, b) in self.missed_over.iter_mut().zip([0.02, 0.05, 0.2]) {
            *n += u64::from(moved > b);
        }
    }
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let max_s: f64 = a.get(2).map_or(f64::INFINITY, |s| s.parse().unwrap());
    let every: u32 = a.get(3).map_or(10, |s| s.parse().unwrap());
    let f = std::fs::File::open(&a[1]).expect("open words");
    let mut chain = Chain::new(ChainOptions::default());
    let check = SumsCheck::default();
    let mut outs = Vec::new();
    let mut ext = [(f64::INFINITY, f64::NEG_INFINITY); 5];
    let (mut frames, mut failing, mut zeros) = (0u32, 0u32, 0u64);
    let mut cor = [[Corruptions::default(); 2]; 2];
    let mut prev = std::collections::VecDeque::<Judged>::new();
    println!("frame count imaginary spread toeplitz fit fit_mer_db");
    for_each(f, |item| {
        match item {
            Input::Loss(n) => chain.loss(n, &mut outs),
            Input::Symbols(s, gain) => {
                chain.set_agc(gain);
                for x in s {
                    chain.push(x, &mut outs);
                }
            }
        }
        for o in outs.drain(..) {
            let Output::Frame(fr) = o else { continue };
            let Some(sums) = fr.sums.as_ref() else {
                continue;
            };
            let s = to_training(sums);
            let (Some(sh), Some(t)) = (sums_shape(&s), solve_training(&s)) else {
                continue;
            };
            let ft = fit(&s, &t);
            let mer = -10.0 * (1.0 - ft).log10();
            println!(
                "{} {} {:.3e} {:.3e} {:.3e} {:.5} {:.2}",
                s.frame, s.count, sh.imaginary.1, sh.spread.1, sh.toeplitz.1, ft, mer
            );
            if frames == 0 {
                let d: Vec<String> = (0..11)
                    .map(|k| {
                        format!(
                            "{:+.2}",
                            100.0 * (s.r[r_index(k, k)][0] as f64 / sh.diagonal - 1.0)
                        )
                    })
                    .collect();
                println!(
                    "# frame {}: diagonal R[k][k] / mean − 1 (%): {}",
                    s.frame,
                    d.join(" ")
                );
            }
            for (e, v) in ext
                .iter_mut()
                .zip([sh.imaginary.1, sh.spread.1, sh.toeplitz.1, ft, mer])
            {
                *e = (e.0.min(v), e.1.max(v));
            }
            if check_sums(&s, &check).is_some() || ft > check.fit {
                failing += 1;
                println!(
                    "# frame {}: the clean sums fail the default checks",
                    s.frame
                );
            }
            frames += 1;
            // the gate's reference: the clean solve 5 frames before (as a
            // slow loop's last good set would be, 2 s before: ~60 frames)
            let good = prev.front().copied().unwrap_or(Judged {
                taps: t,
                fit: Some(ft),
            });
            prev.push_back(Judged {
                taps: t,
                fit: Some(ft),
            });
            if prev.len() > 5 {
                prev.pop_front();
            }
            if frames % every == 0 {
                let words = sums_to_words(&s);
                let w0 = norm(&t);
                for i in 0..words.len() {
                    if words[i] == 0 {
                        zeros += 1;
                        continue;
                    }
                    let mut w = words.clone();
                    w[i] = 0;
                    let bad = sums_from_words(s.frame, s.count, &w);
                    let (by_checks, by_gate, tb) = caught(&bad, &check, &good);
                    let moved = tb.map_or(0.0, |tb| distance(&tb, &t) / w0);
                    let k = usize::from(i >= 4 * R_ENTRIES);
                    cor[k][0].count(by_checks, moved);
                    cor[k][1].count(by_gate, moved);
                }
            }
        }
        (chain.symbols() as f64 / SYMBOL_RATE) < max_s
    })
    .expect("read words");
    let names = ["imaginary", "spread", "toeplitz", "fit", "fit MER dB"];
    println!("# {frames} frames with sums, {failing} failing the default checks");
    for (n, e) in names.iter().zip(ext) {
        println!("# {n}: {:.4e} … {:.4e}", e.0, e.1);
    }
    println!("# one word read as 0, every {every}th frame ({zeros} words already 0, not counted):");
    for (what, by) in ["R", "p"].iter().zip(cor) {
        for (who, c) in ["the checks", "the checks and the gate"].iter().zip(by) {
            report(what, who, c);
        }
    }
}

fn report(what: &str, who: &str, c: Corruptions) {
    {
        println!(
            "#   words of {what}, {who}: {} sets; caught {} ({:.2} %), missed {}: taps off by > 2 % in {}, > 5 % in {}, > 20 % in {}; at most {:.2} %",
            c.sets,
            c.caught,
            100.0 * c.caught as f64 / c.sets.max(1) as f64,
            c.missed,
            c.missed_over[0],
            c.missed_over[1],
            c.missed_over[2],
            100.0 * c.missed_max
        );
    }
}

//! Stages 1–3 of the fixed-point chain (`isdbs3_phy::chain`) against the
//! floating-point receiver on a `Symbols` words file (development tool,
//! plan isdbs3-fpga-rx piece 3).
//!
//! The chain runs over the whole file (losses included). Every `SECONDS`
//! of signal (default 2) a stretch of `FRAMES` frames (default 4) of the
//! raw symbols is also given to `sync::synchronise` after removing the
//! chain's carrier register (as the host's streamed path removes its
//! tracked offset), and compared head by head: slot head positions, frame
//! heads, SSync parity, and the slot phase (chain θ against the float
//! receiver's phase after the same derotation) and amplitude ratio.
//! Prints the carrier register over time (its range is the drift it
//! followed), the lock events and the comparison totals.
//!
//! cargo run --release -p isdbs3-phy --example chainsync -- WORDS_FILE [SECONDS [FRAMES [MAX_SECONDS]]]

use isdbs3_core::params::{FRAME_SYMBOLS, SYMBOL_RATE};
use isdbs3_phy::Cplx;
use isdbs3_phy::chain::input::{Input, for_each};
use isdbs3_phy::chain::sync::Word;
use isdbs3_phy::chain::{Chain, ChainOptions, Output};
use std::collections::BTreeMap;
use std::time::Instant;

// a development tool: f64 seconds to symbol counts by `as`
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn main() {
    let a: Vec<String> = std::env::args().collect();
    let every: f64 = a.get(2).map_or(2.0, |s| s.parse().unwrap());
    let frames: usize = a.get(3).map_or(4, |s| s.parse().unwrap());
    let max_s: f64 = a.get(4).map_or(f64::INFINITY, |s| s.parse().unwrap());
    let f = std::fs::File::open(&a[1]).expect("open words");
    let mut chain = Chain::new(ChainOptions {
        trace: true,
        eq_every: 0,
        ..ChainOptions::default()
    });
    let mut out = Vec::new();
    // raw symbols of the current comparison window, with the chain's
    // heads over it and the carrier register at its start
    let win_len = (frames + 1) * FRAME_SYMBOLS;
    let mut next_win = 0u64;
    let mut win: Option<(u64, i32, Vec<Cplx>)> = None;
    let mut heads: BTreeMap<u64, (Word, u32, i64, Option<u8>, u32)> = BTreeMap::new();
    let (mut cmp_heads, mut cmp_equal, mut cmp_frames, mut cmp_frames_equal) = (0, 0, 0, 0);
    let (mut max_dphi, mut sum_dphi2, mut n_dphi) = (0f64, 0f64, 0usize);
    let (mut amp_ratio_min, mut amp_ratio_max) = (f64::MAX, f64::MIN);
    let (mut fmin, mut fmax) = (i32::MAX, i32::MIN);
    let mut restarts = 0;
    let mut cpu = 0.0;
    let t0 = Instant::now();
    for_each(f, |item| {
        let t = Instant::now();
        match item {
            Input::Loss(n) => {
                println!(
                    "{:8.3} s: loss of {n} symbols",
                    chain.symbols() as f64 / SYMBOL_RATE
                );
                chain.loss(n, &mut out);
                win = None;
            }
            Input::Symbols(s, _) => {
                for x in s {
                    let n = chain.symbols();
                    if win.is_none() && n >= next_win {
                        win = Some((n, chain.carrier(), Vec::with_capacity(win_len)));
                        heads.clear();
                    }
                    if let Some((_, _, v)) = win.as_mut() {
                        v.push(Cplx::new(f64::from(x[0]), f64::from(x[1])));
                    }
                    chain.push(x, &mut out);
                }
            }
        }
        cpu += t.elapsed().as_secs_f64();
        for o in out.drain(..) {
            match o {
                Output::Head(h) => {
                    heads.insert(h.at, (h.word, h.theta, h.amp, h.number, h.nco_phase));
                }
                Output::Carrier(fr) => {
                    fmin = fmin.min(fr);
                    fmax = fmax.max(fr);
                }
                Output::Restart { sync_lost } => {
                    restarts += 1;
                    println!(
                        "{:8.3} s: restart ({})",
                        chain.symbols() as f64 / SYMBOL_RATE,
                        if sync_lost { "sync lost" } else { "input loss" }
                    );
                    win = None;
                }
                Output::Symbol(_) | Output::Frame(_) | Output::Dropped { .. } => {}
            }
        }
        if let Some((start, carrier, v)) = win.as_ref()
            && v.len() >= win_len
        {
            let (start, carrier) = (*start, *carrier);
            // the float receiver on the same symbols, the chain's carrier
            // removed (constant over the window: the chain updates it
            // once per frame, so a little of the drift is left in)
            let hz = f64::from(carrier) * SYMBOL_RATE / 2f64.powi(32);
            let z = isdbs3_phy::rx::derotate(v, hz);
            if let Some(sy) = isdbs3_phy::sync::synchronise(&z) {
                let mut fheads = 0;
                let mut fequal = 0;
                for (k, s) in sy.slots.iter().enumerate() {
                    let at = start + s.at as u64;
                    let Some(&(word, _, _, number, _)) = heads.get(&at) else {
                        continue;
                    };
                    cmp_heads += 1;
                    let fnum = s.number;
                    let is_ss = k % 2 == sy.ssync_parity;
                    if (word == Word::Ssync) == is_ss {
                        cmp_equal += 1;
                    }
                    if fnum == Some(0) || number == Some(0) {
                        fheads += 1;
                        if fnum == Some(0) && number == Some(0) {
                            fequal += 1;
                        }
                    }
                }
                // the carrier phase of the input at each head: chain θ +
                // its NCO phase there; float: the derotated phase plus
                // the derotation at that symbol
                for s in &sy.slots {
                    let at = start + s.at as u64;
                    let Some(&(_, theta, amp, _, nco)) = heads.get(&at) else {
                        continue;
                    };
                    let tau = std::f64::consts::TAU;
                    let th = f64::from(theta.wrapping_add(nco)) / 2f64.powi(32) * tau;
                    let fl = s.phase.arg() + tau * hz * s.at as f64 / SYMBOL_RATE;
                    let dd = wrap(th - fl).abs();
                    max_dphi = max_dphi.max(dd);
                    sum_dphi2 += dd * dd;
                    n_dphi += 1;
                    // chain amplitude: 24·√2 · A_r, r = 32 767 / 2 048 · x
                    let am = amp as f64 / (24.0 * 2f64.sqrt() * 32_767.0 / 2_048.0);
                    let ratio = am / s.amp;
                    amp_ratio_min = amp_ratio_min.min(ratio);
                    amp_ratio_max = amp_ratio_max.max(ratio);
                }
                cmp_frames += fheads;
                cmp_frames_equal += fequal;
                println!(
                    "{:8.3} s: carrier {:+8.1} Hz; float sync offset {} frames {}; heads compared so far {cmp_heads} (parity equal {cmp_equal}), frame heads {cmp_frames} (equal {cmp_frames_equal})",
                    start as f64 / SYMBOL_RATE,
                    hz,
                    sy.offset,
                    sy.frames.len()
                );
            }
            win = None;
            next_win = start + (every * SYMBOL_RATE) as u64;
        }
        (chain.symbols() as f64) < max_s * SYMBOL_RATE
    })
    .expect("read");
    let secs = chain.symbols() as f64 / SYMBOL_RATE;
    let hz = |f: i32| f64::from(f) * SYMBOL_RATE / 2f64.powi(32);
    println!(
        "{secs:.3} s of signal; chain CPU {cpu:.2} s ({:.3} s per s), wall {:.2} s",
        cpu / secs,
        t0.elapsed().as_secs_f64()
    );
    println!(
        "carrier register {:+.1} … {:+.1} Hz, at the end {:+.1} Hz; restarts {restarts}",
        hz(fmin),
        hz(fmax),
        hz(chain.carrier())
    );
    println!(
        "heads compared {cmp_heads}: SSync parity equal {cmp_equal}; frame heads {cmp_frames}, equal {cmp_frames_equal}"
    );
    println!(
        "head phase of the input, chain − float: max {:.2e} rad, rms {:.2e} rad ({n_dphi} heads)",
        max_dphi,
        (sum_dphi2 / n_dphi.max(1) as f64).sqrt()
    );
    println!("head amplitude chain/float: {amp_ratio_min:.5} … {amp_ratio_max:.5}");
}

fn wrap(a: f64) -> f64 {
    use std::f64::consts::{PI, TAU};
    (a + PI).rem_euclid(TAU) - PI
}

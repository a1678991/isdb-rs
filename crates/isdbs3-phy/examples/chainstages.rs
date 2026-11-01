//! Stages 4–6 of the fixed-point chain against the floating-point
//! receiver, on the frames of a `Symbols` words file (development tool,
//! plan isdbs3-fpga-rx piece 3, "agreement per stage").
//!
//! The chain runs with tracing over the first `SECONDS` (default 4) with
//! the equaliser solved every frame. For each decoded frame after the
//! first `SKIP` (default 3):
//!
//! - **Stage 4**: the taps the chain solved from its integer sums of that
//!   frame (`chain::eq::solve_float`, before rounding to Q2.14) against
//!   `eq::train` on the same frame of the raw symbols (the float
//!   receiver's own sync, the chain's carrier register removed). The
//!   chain equalises after the slot phase and amplitude correction (Q.10,
//!   1.0 = 1 024), the float receiver before it (in the input's scale,
//!   the references rotated by the slot phase): the taps agree up to one
//!   complex factor (the amplitude and phase of the frame's slots), so
//!   both are normalised by their centre tap and compared as Q2.14
//!   values: the largest |Δ| over the 11 taps, in Q2.14 LSBs. And the
//!   chain's arithmetic alone: the taps rounded to Q2.14 (what the HDL
//!   gets) against the unrounded solve of the same integer sums, and
//!   against the solution of the float sums of the same frame recomputed
//!   from the chain's stage-3 output (its trace) in f64.
//! - **Stage 6**: on the chain's own equaliser output for the frame (so
//!   only the demapper's arithmetic differs), every main symbol of every
//!   valid slot demapped by `main_signal::demap` against the chain's
//!   points in force converted to float, with the chain's n0, then
//!   `fixed::quantise` at scale 1, against the chain's LLRs: the fraction
//!   equal, the largest |Δ|, and the histogram of Δ.
//! - **Stage 5**: the TMCC soft bits' signs against the float receiver's
//!   TMCC LLR signs (`tmcc::llrs` of the same frame), and the decoded
//!   TMCC information of both (bits equal).
//!
//! cargo run --release -p isdbs3-phy --example chainstages -- WORDS_FILE [SECONDS [SKIP]]

use isdb_frontend::format::{Event, Reader};
use isdbs3_core::params::{FRAME_SYMBOLS, MAIN_PER_MOD_SLOT, SLOTS, SYMBOL_RATE};
use isdbs3_core::slots::layout;
use isdbs3_phy::Cplx;
use isdbs3_phy::chain::{Chain, ChainOptions, Output};
use std::io::Read;

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let secs: f64 = a.get(2).map_or(4.0, |s| s.parse().unwrap());
    let skip: usize = a.get(3).map_or(3, |s| s.parse().unwrap());
    // the raw symbols up to the first loss (or SECONDS)
    // a symbol count from seconds (positive, far inside u64)
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let want = (secs * SYMBOL_RATE) as usize;
    let mut f = std::fs::File::open(&a[1]).expect("open words");
    let mut r = Reader::new();
    let mut ev = Vec::new();
    let mut x: Vec<[i8; 2]> = Vec::new();
    let mut buf = vec![0u8; 1 << 20];
    'read: while x.len() < want {
        let n = f.read(&mut buf).expect("read");
        if n == 0 {
            break;
        }
        r.feed_bytes(&buf[..n], &mut ev);
        for e in ev.drain(..) {
            match e {
                Event::Block { symbols, .. } => x.extend(symbols),
                Event::Loss { .. } if !x.is_empty() => break 'read,
                _ => {}
            }
        }
    }
    println!(
        "{} symbols ({:.2} s) without a loss",
        x.len(),
        x.len() as f64 / SYMBOL_RATE
    );
    let mut chain = Chain::new(ChainOptions {
        trace: true,
        eq_every: 1,
        ..ChainOptions::default()
    });
    let mut out = Vec::new();
    let mut frames = Vec::new();
    // stage 3's output by chain index (for the float sums)
    let mut ys: std::collections::BTreeMap<u64, [i16; 2]> = std::collections::BTreeMap::new();
    // the carrier register at each symbol where it changed
    let mut carrier: Vec<(usize, i32)> = vec![(0, 0)];
    for (n, &s) in x.iter().enumerate() {
        chain.push(s, &mut out);
        for o in out.drain(..) {
            match o {
                Output::Frame(fr) => frames.push(fr),
                Output::Symbol(c) => {
                    ys.insert(c.at, c.y);
                }
                Output::Carrier(c) => carrier.push((n + 1, c)),
                _ => {}
            }
        }
    }
    let z: Vec<Cplx> = x
        .iter()
        .map(|s| Cplx::new(f64::from(s[0]), f64::from(s[1])))
        .collect();
    let (mut tap_max, mut tap_rms, mut tap_n) = (0f64, 0f64, 0usize);
    let (mut q_max, mut s_max) = (0f64, 0f64);
    let (mut llr_eq, mut llr_n, mut llr_max) = (0usize, 0usize, 0i32);
    let mut hist = [0usize; 7];
    let (mut tsign_eq, mut tsign_n, mut tmcc_eq, mut tmcc_n) = (0usize, 0usize, 0usize, 0usize);
    for fr in frames.iter().filter(|f| !f.slots.is_empty()).skip(skip) {
        let tr = fr.trace.as_ref().expect("traced");
        let at = usize::try_from(fr.at).unwrap();
        if at + FRAME_SYMBOLS + 20_000 > z.len() || at < 20_000 {
            continue;
        }
        // the float receiver on this frame (plus a margin), with the
        // chain's register at the frame start removed
        let reg = carrier.iter().rev().find(|c| c.0 <= at).map_or(0, |c| c.1);
        let hz = f64::from(reg) * SYMBOL_RATE / 2f64.powi(32);
        let lo = at - 10_000;
        let zz = isdbs3_phy::rx::derotate(&z[lo..at + FRAME_SYMBOLS + 20_000], hz);
        let Some(sy) = isdbs3_phy::sync::synchronise(&zz) else {
            continue;
        };
        let Some(&fi) = sy.frames.iter().find(|&&f| sy.slots[f].at + lo == at) else {
            println!("frame {}: float sync did not find the frame head", fr.frame);
            continue;
        };
        let es = 10f64.powf(sy.sync_snr_db / 10.0);
        let Some(l) = isdbs3_phy::tmcc::llrs(&zz, &sy.slots, fi, 1.0 / (2.0 * es)) else {
            continue;
        };
        // stage 5
        for (a, b) in l.iter().zip(&tr.tmcc_soft) {
            tsign_n += 1;
            tsign_eq += usize::from((*a < 0.0) == (*b < 0));
        }
        let ft = isdbs3_phy::tmcc::decode(&l, 50).ok();
        if let Some(t) = &ft {
            tmcc_n += 1;
            tmcc_eq += usize::from(t.info.bits() == fr.tmcc.bits);
        }
        let Some(lay) = ft.as_ref().and_then(|t| layout(&t.info)) else {
            continue;
        };
        // stage 4
        if let (Some(w), Some(s)) = (
            isdbs3_phy::eq::train(&zz, &sy.slots, fi, &lay),
            fr.sums.as_ref(),
        ) && let Some(wc) = isdbs3_phy::chain::eq::solve_float(s)
        {
            let c = isdbs3_phy::chain::eq::CENTRE;
            let (nf, nc) = (w[c], wc[c]);
            for k in 0..w.len() {
                let d = (w[k] / nf - wc[k] / nc).norm() * 16_384.0;
                tap_max = tap_max.max(d);
                tap_rms += d * d;
                tap_n += 1;
            }
            // the rounding to Q2.14
            if let Some(q) = isdbs3_phy::chain::eq::solve_sums(s) {
                for k in 0..w.len() {
                    let qv = Cplx::new(f64::from(q[k][0]), f64::from(q[k][1]));
                    q_max = q_max.max((qv - wc[k] * 16_384.0).norm());
                }
            }
            // the same least squares in f64 on the chain's stage-3 output:
            // the integer sums are exact, so this is f64 rounding only
            let fs = float_sums(&ys, s, fr.at, &lay, &tr.points);
            if let Some(wf) = fs {
                for k in 0..w.len() {
                    s_max = s_max.max((wf[k] - wc[k]).norm() * 16_384.0);
                }
            }
        }
        // stage 6, on the chain's own equalised symbols
        let mut ms = Vec::with_capacity(SLOTS * MAIN_PER_MOD_SLOT);
        for k in 0..SLOTS {
            for j in isdbs3_phy::main_signal::main_offsets() {
                let v = tr.z[k * isdbs3_core::params::MOD_SLOT + j];
                ms.push(Cplx::new(f64::from(v[0]), f64::from(v[1])) / 1024.0);
            }
        }
        let n0 = fr.status.n0 as f64 / f64::from(1u32 << 20);
        let slots_lay: Vec<_> = fr.slots.iter().filter(|s| !s.dummy).collect();
        for ((si, llr), so) in tr.llrs.iter().zip(&slots_lay) {
            debug_assert_eq!(usize::from(so.slot), *si);
            let s = lay[*si];
            let pts: Vec<Cplx> = tr.points[*si]
                .iter()
                .map(|p| Cplx::new(f64::from(p[0]), f64::from(p[1])) / 1024.0)
                .collect();
            let f = isdbs3_phy::main_signal::codeword_llrs(&ms, &s, &pts, n0);
            let mut q = Vec::new();
            isdb_fec::ldpc::fixed::quantise(&f, 1.0, &mut q);
            for (a, b) in q.iter().zip(llr) {
                let d = (i32::from(*a) - i32::from(*b)).abs();
                llr_n += 1;
                llr_eq += usize::from(d == 0);
                llr_max = llr_max.max(d);
                hist[usize::try_from(d.min(6)).unwrap()] += 1;
            }
        }
        println!(
            "frame {}: TMCC soft-bit signs {:.5} equal; taps max |Δ| so far {tap_max:.1} LSB; LLRs equal so far {:.5}",
            fr.frame,
            tsign_eq as f64 / tsign_n.max(1) as f64,
            llr_eq as f64 / llr_n.max(1) as f64
        );
    }
    println!("--");
    println!(
        "stage 4: taps (normalised by the centre tap) chain-sums vs eq::train: max |Δ| {tap_max:.2} Q2.14 LSB, rms {:.2} LSB over {tap_n} taps",
        (tap_rms / tap_n.max(1) as f64).sqrt()
    );
    println!(
        "stage 4: chain taps rounded to Q2.14 vs its unrounded solve: max |Δ| {q_max:.2} LSB; integer sums vs the f64 sums of the same symbols: max |Δ| {s_max:.4} LSB"
    );
    println!(
        "stage 5: TMCC soft-bit signs equal {tsign_eq} of {tsign_n} ({:.6}); decoded TMCC bits equal {tmcc_eq} of {tmcc_n} frames",
        tsign_eq as f64 / tsign_n.max(1) as f64
    );
    println!(
        "stage 6: LLRs equal {llr_eq} of {llr_n} ({:.6}), max |Δ| {llr_max}; |Δ| histogram 0..≥6: {hist:?}",
        llr_eq as f64 / llr_n.max(1) as f64
    );
}

/// The f64 normal equations of the frame at `at` from the chain's stage-3
/// outputs `ys` (as the chain's training: sync and pilot references in
/// Q.10), solved: the float counterpart of the chain's integer sums.
fn float_sums(
    ys: &std::collections::BTreeMap<u64, [i16; 2]>,
    s: &isdbs3_phy::chain::eq::Sums,
    at: u64,
    lay: &[isdbs3_core::slots::Slot],
    _pts: &[Vec<[i16; 2]>],
) -> Option<Vec<Cplx>> {
    use isdbs3_core::params::{FSYNC, MOD_SLOT, PILOT_SYMBOLS, SSYNC, SYNC_SYMBOLS};
    use isdbs3_phy::chain::sync::word_symbols;
    let taps = isdbs3_phy::eq::TAPS;
    let signs = isdbs3_core::dispersal::pilot_signs();
    let (fs, ss) = (word_symbols(FSYNC), word_symbols(SSYNC));
    let y = |n: u64| {
        ys.get(&n)
            .map(|v| Cplx::new(f64::from(v[0]), f64::from(v[1])))
    };
    let mut a = vec![vec![Cplx::new(0.0, 0.0); taps]; taps];
    let mut b = vec![Cplx::new(0.0, 0.0); taps];
    let mut count = 0u32;
    for k in 0..SLOTS {
        let base = at + (k * MOD_SLOT) as u64;
        // the word's sign: as the chain read it (FSync parity: compare
        // with the reference word)
        let word = if k % 2 == 1 { &ss } else { &fs };
        let c: Cplx = (0..SYNC_SYMBOLS)
            .filter_map(|t| {
                y(base + t as u64)
                    .map(|v| v * Cplx::new(f64::from(word[t][0]), -f64::from(word[t][1])))
            })
            .sum();
        let sg = if c.re < 0.0 { -1.0 } else { 1.0 };
        let mut refs = Vec::new();
        for (t, w) in word.iter().enumerate() {
            refs.push((
                base + t as u64,
                Cplx::new(f64::from(w[0]), f64::from(w[1])) * 724.0 * sg,
            ));
        }
        let sl = &lay[k];
        let nom = isdbs3_phy::chain::demap::nominal(sl.modulation, sl.rate);
        for t in 0..PILOT_SYMBOLS {
            let p = nom[t % nom.len()];
            let g = f64::from(signs[k * PILOT_SYMBOLS + t]);
            refs.push((
                base + (SYNC_SYMBOLS + t) as u64,
                Cplx::new(f64::from(p[0]), f64::from(p[1])) * g,
            ));
        }
        for (n, d) in refs {
            let x: Option<Vec<Cplx>> = (0..taps)
                .map(|kk| y(n + (taps / 2) as u64 - kk as u64))
                .collect();
            let Some(x) = x else { continue };
            for r in 0..taps {
                for cc in 0..taps {
                    a[r][cc] += x[r].conj() * x[cc];
                }
                b[r] += x[r].conj() * d;
            }
            count += 1;
        }
    }
    if count != s.count {
        return None;
    }
    isdbs3_phy::eq::solve(a, b)
}

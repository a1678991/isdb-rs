//! Test vectors of the fixed-point chain (`isdbs3_phy::chain`) for the HDL
//! of stages 1–6 and 8 (plan isdbs3-fpga-rx piece 4): one JSON file per
//! stage with the stage's inputs, its outputs at the boundary and the
//! internal values the HDL needs to locate a mismatch, and a chain set
//! with the input symbols and the expected `Slots` words.
//!
//! cargo run --release -p isdbs3-phy --example chainvectors -- OUT_DIR [WORDS_FILE [SECONDS]]
//!
//! Without a words file: the synthetic sets, from `chain::synth` (whole
//! B44 frames: sync words, pilots with their PRBS signs, a real TMCC and
//! LDPC/BCH-coded slots on the §3.8 points; a carrier offset with a drift,
//! noise, 6-bit quantising), written as `OUT_DIR/chain-<n>-<stage>.json`.
//! With a words file (the FPGA recording): the same sets from its first
//! `SECONDS` (default 0.2) as `chain-rec-<n>-<stage>.json`.
//!
//! Every file is one JSON object with `"format": 1`, `"chain"` (the
//! module's constants: widths, shifts, thresholds), `"source"`, and the
//! stage's fields below. Complex sequences are flat arrays `[re0, im0,
//! re1, im1, …]`; indices `at` count chain input symbols from 0.
//!
//! - **1-sync** (stages 2a, 1, 2b): `input` (the symbols, 6-bit), `r` (the
//!   NCO output of every symbol), `nco_phase` (φ before each symbol, every
//!   4 096th symbol: `[at, φ]` pairs), `per_symbol` for the first
//!   `"detail"` positions (`[at, cf_re, cf_im, cs_re, cs_im, A]`: the
//!   window's correlations and the accumulator after the update),
//!   `periods` (`[at_end, peak, peak_pos, sum_lo32, sum_hi, locked,
//!   offset, parity]` at each period's end), `heads` (`[at, word, corr_re,
//!   corr_im, theta, amp, number, frame]`, word 0 SSync / 1 FSync / 2
//!   !FSync, number and frame −1 when unknown) and `carrier` (`[at, f]`
//!   after each register update: `at` the first symbol rotated with it).
//! - **2-phase** (stage 3): the heads of three slots (as above) and the
//!   NCO outputs `r` of the two slots between them; `out`: per symbol
//!   `[at, j, phi, gain, y_re, y_im]`.
//! - **3-eq** (stage 4): `taps` (Q2.14, 11 complex: in use over the
//!   frame, written at open by the host as `ChainOptions::initial_taps`
//!   from a first pass over the same symbols, so the FIR is not the
//!   identity) and `taps_from`; `y` (stage 3's output, from the frame's first
//!   symbol minus 5) and `z` (the FIR output, one per frame symbol);
//!   `training`: `[at, d_re, d_im]` per training symbol; `sums`: `{frame,
//!   count, r: [66 complex as 4 numbers re_hi, re_lo, im_hi, im_lo of
//!   2³²], p: [11 the same]}` and `solved` (the host's taps for them).
//! - **4-tmcc** (stage 5): `z_tmcc` (the frame's 31 680 TMCC symbols in
//!   TMCC order), `j` (their slot positions), `soft` (the soft bits),
//!   `position` (their positions in the shortened word), `known` (+127),
//!   and the result: `ldpc_iterations`, `bch_corrected`, `bits` (9 422),
//!   `layout` (`[slot, mode, modulation code, rate code, dummy,
//!   symbol_start, symbols]` per slot), `applies_to` (frame + lag).
//! - **5-demap** (stage 6): for each valid slot of one frame, `points`
//!   (Q.10), `k` (the LLR scale), `n0`, the codeword's symbols `z`, their
//!   LLRs in label order `llr_label`, the permutation (`perm`, codeword
//!   position of label bit i), the de-interleaved word `llr`; the frame's
//!   learning: `pilot_sums` (per modulation used, per label `[re, im,
//!   count]`), `learned` (the points for the next frame), `noise_sum`,
//!   `noise_count`, `n0_next`, `k_next`.
//! - **6-chain**: the whole chain: `input`, `losses` (`[before_index,
//!   symbols]`, none synthetic) and `slots_words` (every `Slots` word the
//!   chain's frames give, `chain::out::blocks` through the `Writer`), with
//!   `frames` (per frame: counter, decoded, LDPC iterations per slot).

use isdbs3_core::params::{FRAME_SYMBOLS, MOD_SLOT, SLOTS, SYMBOL_RATE};
use isdbs3_phy::chain::frame::{FrameOut, Part, part};
use isdbs3_phy::chain::fx::CORDIC_ATAN;
use isdbs3_phy::chain::input::{Input, for_each};
use isdbs3_phy::chain::phase::Corrected;
use isdbs3_phy::chain::sync::{Head, Nco, SlotSync, SyncEvent, Word};
use isdbs3_phy::chain::synth::{SynthOptions, generate};
use isdbs3_phy::chain::{Chain, ChainOptions, LAYOUT_LAG, Output};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

/// Per-symbol stage-1 internals for this many positions.
const DETAIL: usize = 30_000;

fn flat2<T: Copy + Into<i64>>(v: &[[T; 2]]) -> Vec<i64> {
    v.iter().flat_map(|x| [x[0].into(), x[1].into()]).collect()
}

fn word_code(w: Word) -> i64 {
    match w {
        Word::Ssync => 0,
        Word::Fsync => 1,
        Word::NotFsync => 2,
    }
}

fn head_json(h: &Head) -> Vec<i64> {
    vec![
        i64::try_from(h.at).unwrap(),
        word_code(h.word),
        i64::from(h.corr[0]),
        i64::from(h.corr[1]),
        i64::from(h.theta),
        h.amp,
        h.number.map_or(-1, i64::from),
        h.frame.map_or(-1, i64::from),
    ]
}

/// A 48-bit-or-wider signed sum as `[hi, lo]` of 2³² (lo unsigned), so
/// any JSON reader keeps it exact.
fn wide(v: i64) -> [i64; 2] {
    [v >> 32, v & 0xFFFF_FFFF]
}

fn constants() -> Value {
    use isdbs3_phy::chain::{demap, eq, fx, phase, sync, tmcc};
    json!({
        "slot": MOD_SLOT,
        "slots": SLOTS,
        "leak_shift": sync::LEAK_SHIFT,
        "lock_ratio": sync::LOCK_RATIO,
        "lock_periods": sync::LOCK_PERIODS,
        "loss_periods": sync::LOSS_PERIODS,
        "majority_max": sync::MAJORITY_MAX,
        "majority_min": sync::MAJORITY_MIN,
        "frame_miss_limit": sync::FRAME_MISS_LIMIT,
        "rot_shift": sync::ROT_SHIFT,
        "carrier_max": sync::CARRIER_MAX,
        "carrier_steps": sync::CARRIER_STEPS,
        "carrier_gain": sync::CARRIER_GAIN,
        "carrier_shift": sync::CARRIER_SHIFT,
        "cordic_atan": CORDIC_ATAN,
        "cordic_inv_gain": fx::CORDIC_INV_GAIN,
        "cordic_prescale": fx::CORDIC_PRESCALE,
        "slot_recip": fx::SLOT_RECIP,
        "interp_frac": phase::INTERP_FRAC,
        "gain_num": phase::GAIN_NUM,
        "gain_frac": phase::GAIN_FRAC,
        "min_amp": phase::MIN_AMP,
        "tap_frac": eq::TAP_FRAC,
        "sync_ref": isdbs3_phy::chain::frame::SYNC_REF,
        "eq_apply_frames": isdbs3_phy::chain::EQ_APPLY_FRAMES,
        "tmcc_shift": tmcc::TMCC_SHIFT,
        "known_zero": tmcc::KNOWN_ZERO,
        "llr_shift": demap::LLR_SHIFT,
        "n0_min": demap::N0_MIN,
        "n0_default": demap::N0_DEFAULT,
        "noise_step": demap::NOISE_STEP,
        "layout_lag": LAYOUT_LAG,
        "fsync": isdbs3_phy::chain::sync::word_symbols(isdbs3_core::params::FSYNC).iter().flat_map(|s| [i64::from(s[0]), i64::from(s[1])]).collect::<Vec<_>>(),
        "ssync": isdbs3_phy::chain::sync::word_symbols(isdbs3_core::params::SSYNC).iter().flat_map(|s| [i64::from(s[0]), i64::from(s[1])]).collect::<Vec<_>>(),
        "pilot_signs": isdbs3_core::dispersal::pilot_signs(),
        "cos_table": isdb_frontend::tables::cos_table(),
    })
}

fn write(dir: &Path, name: &str, mut v: Value) {
    v["format"] = json!(1);
    v["chain"] = constants();
    let p = dir.join(name);
    let s = serde_json::to_string(&v).expect("json");
    std::fs::write(&p, &s).expect("write vectors");
    println!("{}: {:.1} MB", p.display(), s.len() as f64 / 1e6);
}

/// Everything one traced chain run gives.
struct Run {
    input: Vec<[i8; 2]>,
    losses: Vec<(usize, u64)>,
    heads: Vec<Head>,
    carrier: Vec<(u64, i32)>,
    corrected: Vec<Corrected>,
    frames: Vec<FrameOut>,
    slots_words: Vec<u16>,
}

fn run(input: Vec<[i8; 2]>, losses: Vec<(usize, u64)>, taps: Option<[[i16; 2]; 11]>) -> Run {
    let mut c = Chain::new(ChainOptions {
        trace: true,
        eq_every: 1,
        initial_taps: taps,
        ..ChainOptions::default()
    });
    let mut out = Vec::new();
    let mut r = Run {
        input: Vec::new(),
        losses: losses.clone(),
        heads: Vec::new(),
        carrier: Vec::new(),
        corrected: Vec::new(),
        frames: Vec::new(),
        slots_words: Vec::new(),
    };
    let mut w = isdb_frontend::slots::Writer::new();
    let mut li = losses.iter().peekable();
    for (n, &x) in input.iter().enumerate() {
        while let Some(&&(at, k)) = li.peek() {
            if at != n {
                break;
            }
            c.loss(k, &mut out);
            li.next();
        }
        c.push(x, &mut out);
        for o in out.drain(..) {
            match o {
                Output::Head(h) => r.heads.push(h),
                Output::Carrier(f) => r.carrier.push((c.symbols(), f)),
                Output::Symbol(s) => r.corrected.push(s),
                Output::Frame(f) => {
                    for b in isdbs3_phy::chain::out::blocks(&f, LAYOUT_LAG) {
                        w.write(&b, &mut r.slots_words);
                    }
                    r.frames.push(*f);
                }
                Output::Dropped { .. } | Output::Restart { .. } => {}
            }
        }
    }
    r.input = input;
    r
}

/// Stage 1 run alone (the chain's own NCO and detector code) for the
/// per-period and per-position internals.
fn stage1(r: &Run) -> Value {
    let mut nco = Nco::new(0);
    let mut sy = SlotSync::new();
    let mut ev = Vec::new();
    let mut rs = Vec::with_capacity(r.input.len());
    let mut per_symbol = Vec::new();
    let mut periods = Vec::new();
    let mut heads = Vec::new();
    let mut phases = Vec::new();
    let mut carrier = Vec::new();
    // stop after the first frame head + one frame (enough for lock, the
    // parity, a frame head and a carrier update)
    let first = r
        .frames
        .first()
        .map_or(r.input.len(), |f| usize::try_from(f.at).unwrap());
    let end = (first + FRAME_SYMBOLS + MOD_SLOT).min(r.input.len());
    for (n, &x) in r.input[..end].iter().enumerate() {
        if n % 4096 == 0 {
            phases.push([n as i64, i64::from(nco.phase)]);
        }
        let v = nco.rotate(x);
        rs.push(v);
        sy.push(v, &mut ev);
        if n >= 23 && n - 23 < DETAIL {
            let (cf, cs, a) = sy.last_corr;
            per_symbol.push([
                (n - 23) as i64,
                i64::from(cf[0]),
                i64::from(cf[1]),
                i64::from(cs[0]),
                i64::from(cs[1]),
                i64::from(a),
            ]);
        }
        for e in ev.drain(..) {
            match e {
                SyncEvent::Head(h) => {
                    heads.push(head_json(&h));
                    if nco.head(&h) {
                        carrier.push([(n + 1) as i64, i64::from(nco.freq)]);
                    }
                }
                SyncEvent::Period(p) => periods.push([
                    (n - 23) as i64,
                    i64::from(p.peak),
                    i64::from(p.peak_pos),
                    i64::try_from(p.sum & 0xFFFF_FFFF).unwrap(),
                    i64::try_from(p.sum >> 32).unwrap(),
                    i64::from(p.locked),
                    i64::from(p.offset),
                    i64::from(p.ssync_parity),
                ]),
                SyncEvent::Lost => panic!("sync lost in the vectors' stream"),
            }
        }
    }
    // the same heads as the whole chain's
    let chain_heads: Vec<Vec<i64>> = r
        .heads
        .iter()
        .filter(|h| usize::try_from(h.at).unwrap() + 24 <= end)
        .map(head_json)
        .collect();
    assert_eq!(heads, chain_heads, "stage 1 alone vs the chain");
    json!({
        "input": flat2(&r.input[..end]),
        "r": flat2(&rs),
        "nco_phase": phases,
        "detail": DETAIL,
        "per_symbol": per_symbol,
        "periods": periods,
        "heads": heads,
        "carrier": carrier,
    })
}

fn stage2(r: &Run) -> Value {
    // three consecutive heads after lock: the slots between them
    let k = r.heads.len() / 2;
    let hs = &r.heads[k..k + 3];
    let (a, b) = (
        usize::try_from(hs[0].at).unwrap(),
        usize::try_from(hs[2].at).unwrap(),
    );
    let out: Vec<[i64; 6]> = r
        .corrected
        .iter()
        .filter(|c| (a..b).contains(&usize::try_from(c.at).unwrap()))
        .map(|c| {
            [
                i64::try_from(c.at).unwrap(),
                i64::from(c.j),
                i64::from(c.phi),
                c.gain,
                i64::from(c.y[0]),
                i64::from(c.y[1]),
            ]
        })
        .collect();
    assert_eq!(out.len(), 2 * MOD_SLOT);
    let rr: Vec<[i16; 2]> = r
        .corrected
        .iter()
        .filter(|c| (a..b).contains(&usize::try_from(c.at).unwrap()))
        .map(|c| c.r)
        .collect();
    json!({
        "heads": hs.iter().map(head_json).collect::<Vec<_>>(),
        "r": flat2(&rr),
        "out": out,
    })
}

/// The first decoded frame whose taps were loaded (or the first decoded
/// one) for stages 4–6.
fn pick(r: &Run) -> &FrameOut {
    r.frames
        .iter()
        .find(|f| !f.slots.is_empty())
        .expect("a decoded frame")
}

fn stage3(r: &Run) -> Value {
    let f = pick(r);
    let at = f.at;
    let ys: Vec<[i16; 2]> = r
        .corrected
        .iter()
        .filter(|c| c.at + 5 >= at && c.at < at + FRAME_SYMBOLS as u64 + 5)
        .map(|c| c.y)
        .collect();
    let tr = f.trace.as_ref().unwrap();
    // training references, as the chain forms them
    let layout = isdbs3_core::slots::layout(&isdbs3_core::tmcc::TmccInfo::parse(
        &r.frames
            .iter()
            .find(|p| Some(p.frame) == f.layout_from)
            .unwrap()
            .tmcc
            .bits,
    ))
    .unwrap();
    let signs = isdbs3_core::dispersal::pilot_signs();
    let mut training = Vec::new();
    let heads: Vec<&Head> = r
        .heads
        .iter()
        .filter(|h| h.at >= at && h.at < at + FRAME_SYMBOLS as u64)
        .collect();
    for (k, h) in heads.iter().enumerate() {
        let w = isdbs3_phy::chain::sync::word_symbols(if h.word == Word::Ssync {
            isdbs3_core::params::SSYNC
        } else {
            isdbs3_core::params::FSYNC
        });
        let s: i64 = if h.word == Word::NotFsync { -1 } else { 1 };
        for (t, ws) in w.iter().enumerate() {
            training.push([
                i64::try_from(h.at).unwrap() + t as i64,
                i64::from(ws[0]) * 724 * s,
                i64::from(ws[1]) * 724 * s,
            ]);
        }
        let sl = &layout[k];
        let nom = isdbs3_phy::chain::demap::nominal(sl.modulation, sl.rate);
        for t in 0..32 {
            let p = nom[t % nom.len()];
            let g = i64::from(signs[k * 32 + t]);
            training.push([
                i64::try_from(h.at).unwrap() + 24 + t as i64,
                i64::from(p[0]) * g,
                i64::from(p[1]) * g,
            ]);
        }
    }
    let sums = f.sums.as_ref().unwrap();
    json!({
        "frame": f.frame,
        "at": f.at,
        "taps": flat2(&f.status.taps),
        "taps_from": f.status.taps_from,
        "y_from": f.at - 5,
        "y": flat2(&ys),
        "z": flat2(&tr.z),
        "training": training,
        "sums": {
            "frame": sums.frame,
            "count": sums.count,
            "r": sums.r.iter().map(|v| [wide(v[0]), wide(v[1])]).collect::<Vec<_>>(),
            "p": sums.p.iter().map(|v| [wide(v[0]), wide(v[1])]).collect::<Vec<_>>(),
        },
        "solved": isdbs3_phy::chain::eq::solve_sums(sums).map(|t| flat2(&t)),
    })
}

fn stage4(r: &Run) -> Value {
    let f = pick(r);
    let tr = f.trace.as_ref().unwrap();
    let mut z = Vec::new();
    let mut js = Vec::new();
    for k in 0..SLOTS {
        for j in isdbs3_phy::tmcc::tmcc_offsets() {
            z.push(tr.z[k * MOD_SLOT + j]);
            js.push(j);
        }
    }
    let position: Vec<usize> = (0..tr.tmcc_soft.len())
        .map(isdbs3_phy::chain::tmcc::word_position)
        .collect();
    let t = &f.tmcc;
    let layout: Vec<[i64; 7]> = t
        .layout
        .iter()
        .flatten()
        .map(|s| {
            [
                s.index as i64,
                s.mode as i64,
                i64::from(s.modulation.code()),
                i64::from(s.rate.code()),
                i64::from(s.dummy),
                s.symbol_start as i64,
                s.symbols as i64,
            ]
        })
        .collect();
    json!({
        "frame": f.frame,
        "z_tmcc": flat2(&z),
        "j": js,
        "soft": tr.tmcc_soft,
        "position": position,
        "known": isdbs3_phy::chain::tmcc::KNOWN_ZERO,
        "ldpc_iterations": t.ldpc_iterations,
        "bch_corrected": t.bch_corrected,
        "bits": t.bits,
        "layout": layout,
        "applies_to": f.frame + LAYOUT_LAG,
    })
}

fn stage5(r: &Run) -> Value {
    use isdbs3_phy::chain::demap::{Learn, demap};
    let f = pick(r);
    let tr = f.trace.as_ref().unwrap();
    let lay = isdbs3_core::slots::layout(&isdbs3_core::tmcc::TmccInfo::parse(
        &r.frames
            .iter()
            .find(|p| Some(p.frame) == f.layout_from)
            .unwrap()
            .tmcc
            .bits,
    ))
    .unwrap();
    // the frame's main symbols in main order
    let mut ms = Vec::new();
    for k in 0..SLOTS {
        for j in 0..MOD_SLOT {
            if let Part::Main(_) = part(j) {
                ms.push(tr.z[k * MOD_SLOT + j]);
            }
        }
    }
    let k = f.status.llr_scale;
    let mut learn = Learn::new();
    let signs = isdbs3_core::dispersal::pilot_signs();
    for (si, s) in lay.iter().enumerate() {
        for t in 0..32 {
            let v = tr.z[si * MOD_SLOT + 24 + t];
            let g = i16::from(signs[si * 32 + t]);
            learn.pilot(
                s.modulation,
                t,
                [v[0].saturating_mul(g), v[1].saturating_mul(g)],
            );
        }
    }
    let mut slots = Vec::new();
    for (si, llr) in &tr.llrs {
        let s = lay[*si];
        let bits = s.modulation.bits();
        let pts = &tr.points[*si];
        let z = &ms[s.symbol_start..s.symbol_start + s.symbols];
        let perm = isdbs3_core::interleave::permutation(s.modulation, s.rate);
        let mut lab = Vec::with_capacity(z.len() * bits);
        let mut w = vec![0i8; isdbs3_core::params::N_LDPC];
        for (sym, &y) in z.iter().enumerate() {
            let mut o = [0i8; 5];
            let best = demap(y, pts, bits, k, &mut o);
            for (b, &l) in o[..bits].iter().enumerate() {
                lab.push(l);
                w[perm[sym * bits + b] as usize] = l;
            }
            if sym % 16 == 0 {
                learn.noise(y, best);
            }
        }
        assert_eq!(&w, llr, "slot {si}: the stage alone vs the chain");
        // only the first two slots' symbols in full (size)
        if slots.len() < 2 {
            slots.push(json!({
                "slot": si,
                "modulation": s.modulation.code(),
                "rate": s.rate.code(),
                "points": flat2(pts),
                "z": flat2(z),
                "llr_label": lab,
                "perm": perm,
                "llr": llr,
            }));
        }
    }
    let next = isdbs3_phy::chain::demap::InForce::default().next(&learn, f.frame);
    // the chain's next frame used these
    if let Some(nf) = r.frames.iter().find(|p| p.at > f.at && !p.slots.is_empty()) {
        assert_eq!(nf.status.n0, next.n0, "n0 for the next frame");
    }
    json!({
        "frame": f.frame,
        "k": k,
        "n0": f.status.n0,
        "slots": slots,
        "learned": next.learned.iter().map(|p| p.as_ref().map(|v| flat2(v))).collect::<Vec<_>>(),
        "n0_next": next.n0,
        "k_next": next.k,
    })
}

fn stage6(r: &Run) -> Value {
    let frames: Vec<Value> = r
        .frames
        .iter()
        .map(|f| {
            json!({
                "frame": f.frame,
                "at": f.at,
                "decoded": !f.slots.is_empty(),
                "tmcc_bch": f.tmcc.bch_corrected,
                "ldpc_iterations": f.slots.iter().filter(|s| !s.dummy).map(|s| s.ldpc_iterations).collect::<Vec<_>>(),
            })
        })
        .collect();
    json!({
        "input": flat2(&r.input),
        "losses": r.losses,
        "frames": frames,
        "slots_words": r.slots_words,
    })
}

fn write_all(dir: &Path, prefix: &str, source: &Value, r: &Run) {
    let with = |mut v: Value| {
        v["source"] = source.clone();
        v
    };
    write(dir, &format!("{prefix}1-sync.json"), with(stage1(r)));
    write(dir, &format!("{prefix}2-phase.json"), with(stage2(r)));
    write(dir, &format!("{prefix}3-eq.json"), with(stage3(r)));
    write(dir, &format!("{prefix}4-tmcc.json"), with(stage4(r)));
    write(dir, &format!("{prefix}5-demap.json"), with(stage5(r)));
    write(dir, &format!("{prefix}6-chain.json"), with(stage6(r)));
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let dir = PathBuf::from(a.get(1).expect("OUT_DIR"));
    std::fs::create_dir_all(&dir).expect("create OUT_DIR");
    if let Some(words) = a.get(2) {
        let secs: f64 = a.get(3).map_or(0.2, |s| s.parse().unwrap());
        let mut input = Vec::new();
        let mut losses = Vec::new();
        for_each(std::fs::File::open(words).expect("open words"), |i| {
            match i {
                Input::Symbols(s, _) => input.extend(s),
                Input::Loss(n) => losses.push((input.len(), n)),
            }
            (input.len() as f64) < secs * SYMBOL_RATE
        })
        .expect("read");
        // the taps the host would write at open: those a first pass
        // over the same symbols solves
        let first = run(input.clone(), losses.clone(), None);
        let taps = first
            .frames
            .iter()
            .filter_map(|f| f.sums.as_ref())
            .find_map(isdbs3_phy::chain::eq::solve_sums);
        let r = run(input, losses, taps);
        write_all(
            &dir,
            "chain-rec-",
            &json!({"recording": words, "seconds": secs}),
            &r,
        );
        return;
    }
    let o = SynthOptions {
        carrier_hz: -400.0,
        drift_hz_s: -22.0,
        snr_db: 14.0,
        amplitude: 9.0,
        seed: 7,
        ..SynthOptions::default()
    };
    let s = generate(&o, 5);
    // the taps the host would write at open: those a first pass solves
    let first = run(s.symbols.clone(), Vec::new(), None);
    let taps = first
        .frames
        .iter()
        .filter_map(|f| f.sums.as_ref())
        .find_map(isdbs3_phy::chain::eq::solve_sums);
    assert!(taps.is_some());
    let r = run(s.symbols, Vec::new(), taps);
    // the chain decodes what was sent
    let decoded: Vec<&FrameOut> = r.frames.iter().filter(|f| !f.slots.is_empty()).collect();
    assert!(!decoded.is_empty());
    for f in &decoded {
        let t = s.frames.iter().find(|t| t.at as u64 == f.at).unwrap();
        let got: Vec<&Vec<u8>> = f
            .slots
            .iter()
            .filter(|x| !x.dummy)
            .map(|x| &x.data)
            .collect();
        let want: Vec<&Vec<u8>> = t.slots.iter().map(|x| &x.2).collect();
        assert_eq!(got, want, "synthetic frame {} decodes", f.frame);
    }
    write_all(
        &dir,
        "chain-",
        &json!({
            "synthetic": {
                "frames": 5,
                "modes": "16APSK 93/120 x120",
                "lead": o.lead,
                "carrier_hz": o.carrier_hz,
                "drift_hz_s": o.drift_hz_s,
                "snr_db": o.snr_db,
                "amplitude": o.amplitude,
                "bits": o.bits,
                "seed": o.seed,
            }
        }),
        &r,
    );
}

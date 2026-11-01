//! The fixed-point chain (`isdbs3_phy::chain`) against the host's streamed
//! receiver on a `Symbols` words file, frame by frame and slot by slot
//! (plan isdbs3-fpga-rx piece 3, the acceptance check).
//!
//! Reference: `rx::receive` called as `isdb rx --symbols` calls it
//! (`isdb-cli` `rxs3::run_stream` over `symstream::run`): the symbols
//! buffered in chunks of 24 frames, each call decoding the buffer with
//! the carrier offset tracked so far removed and the slot offset hinted
//! at 0 when the buffer starts where the previous call cut, keeping what
//! starts at the last whole frame for the next call; a loss decodes the
//! buffer to its end and starts afresh (the tracked carrier kept). The
//! frames of both are matched by the chain-input index of their first
//! symbol.
//!
//! Prints per `REPORT` seconds the running totals and at the end: frames
//! of each, frames matched, slots compared, slots with equal bytes, and
//! the differences explained (one side not decoded: the chain's first
//! frames after a (re)start, or the reference's), LDPC / BCH failures of
//! both, LDPC iterations of both, and the chain's CPU time per second of
//! signal (the reference runs on every core and is timed by wall clock).
//! With `MAX_SECONDS` only the start of the file.
//!
//! The chain's slots are compared as they arrive through the `Slots`
//! format: each frame's blocks (`chain::out::blocks`) are written with
//! `isdb_frontend::slots::Writer`, read back with its `Reader`, and the
//! slot bytes compared are the read-back ones (the in-memory ones are
//! checked equal to them); with `SLOTS_OUT` the words are also written
//! to that file (little-endian, as a radio would send them).
//!
//! cargo run --release -p isdbs3-phy --example chaincheck -- WORDS_FILE [MAX_SECONDS [LAYOUT_LAG [REPORT [SLOTS_OUT]]]]

use isdbs3_core::params::{FRAME_SYMBOLS, SYMBOL_RATE};
use isdbs3_phy::Cplx;
use isdbs3_phy::chain::input::{Input, for_each};
use isdbs3_phy::chain::ldpc::{FixedLdpc, LdpcDecoder};
use isdbs3_phy::chain::{Chain, ChainOptions, Output};
use isdbs3_phy::rx::{RxOptions, receive};
use std::collections::BTreeMap;
use std::time::Instant;

/// One frame's slots: slot index → (data, LDPC iterations, BCH ok).
type Slots = BTreeMap<u8, (Option<Vec<u8>>, Option<usize>, bool)>;

/// The reference's streamed state.
struct Reference {
    buf: Vec<Cplx>,
    /// Chain index of `buf[0]`.
    start: u64,
    aligned: bool,
    tracked: f64,
    frames: BTreeMap<u64, Slots>,
    wall: f64,
    range: (f64, f64),
}

impl Reference {
    fn decode(&mut self, more: bool) {
        let t = Instant::now();
        let r = receive(
            &self.buf,
            &RxOptions {
                slot_offset: self.aligned.then_some(0),
                carrier_hz: self.tracked,
                ..RxOptions::default()
            },
        );
        if let Some(res) = r.residual_hz
            && r.sync_snr_db > 3.0
        {
            self.tracked += res;
            self.range = (
                self.range.0.min(self.tracked),
                self.range.1.max(self.tracked),
            );
        }
        let upto = if more {
            r.frame_starts.last().copied()
        } else {
            None
        };
        if more && upto.is_none() {
            self.wall += t.elapsed().as_secs_f64();
            return;
        }
        for (fi, &fs) in r.frame_starts.iter().enumerate() {
            if upto.is_some_and(|u| fs >= u) || fi >= r.frames.len() {
                continue;
            }
            let mut slots = Slots::new();
            for s in r.slots.iter().filter(|s| s.frame == fi) {
                slots.insert(
                    u8::try_from(s.slot).unwrap(),
                    (s.data.clone(), s.ldpc_iterations, s.bch_corrected.is_some()),
                );
            }
            self.frames.insert(self.start + fs as u64, slots);
        }
        let used = upto.unwrap_or(self.buf.len());
        self.buf.drain(..used);
        self.start += used as u64;
        self.aligned = more && used > 0;
        self.wall += t.elapsed().as_secs_f64();
    }
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let max_s: f64 = a.get(2).map_or(f64::INFINITY, |s| s.parse().unwrap());
    let lag: u32 = a
        .get(3)
        .map_or(isdbs3_phy::chain::LAYOUT_LAG, |s| s.parse().unwrap());
    let report: f64 = a.get(4).map_or(5.0, |s| s.parse().unwrap());
    let mut slots_out = a
        .get(5)
        .map(|p| std::io::BufWriter::new(std::fs::File::create(p).expect("create SLOTS_OUT")));
    let mut sw = isdb_frontend::slots::Writer::new();
    let mut sr = isdb_frontend::slots::Reader::new();
    let (mut slots_words, mut slots_blocks, mut slots_losses) = (0u64, 0u64, 0u64);
    let f = std::fs::File::open(&a[1]).expect("open words");
    // the LDPC through a wrapper that times it, so the model's own cost
    // (stages 1–6 and 8, one thread) is the push time minus the LDPC's
    let ldpc_time = std::sync::Arc::new(std::sync::Mutex::new(0.0f64));
    let lt = std::sync::Arc::clone(&ldpc_time);
    let mut fixed = FixedLdpc::new();
    let timed = move |code: &isdb_fec::ldpc::Code, words: &[&[i8]]| {
        let t = Instant::now();
        let r = fixed.decode(code, words);
        *lt.lock().unwrap() += t.elapsed().as_secs_f64();
        r
    };
    let mut chain = Chain::with_ldpc(
        ChainOptions {
            layout_lag: lag,
            ..ChainOptions::default()
        },
        Box::new(timed),
    );
    let chunk = 24 * FRAME_SYMBOLS;
    let mut rf = Reference {
        buf: Vec::new(),
        start: 0,
        aligned: false,
        tracked: 0.0,
        frames: BTreeMap::new(),
        wall: 0.0,
        range: (0.0, 0.0),
    };
    let mut next_at = chunk;
    let mut chain_frames: BTreeMap<u64, (u32, Option<Slots>)> = BTreeMap::new();
    let (mut restarts, mut dropped, mut tmcc_failed, mut undecoded) = (0, 0, 0, 0);
    let mut cpu = 0.0;
    let mut out = Vec::new();
    let mut next_report = report;
    let mut last_status = None;
    for_each(f, |item| {
        match item {
            Input::Loss(n) => {
                println!("{:8.3} s: loss of {n} symbols", chain.symbols() as f64 / SYMBOL_RATE);
                rf.decode(false);
                rf.start += n;
                rf.aligned = false;
                next_at = chunk;
                let t = Instant::now();
                chain.loss(n, &mut out);
                cpu += t.elapsed().as_secs_f64();
            }
            Input::Symbols(s, gain) => {
                rf.buf.extend(s.iter().map(|x| Cplx::new(f64::from(x[0]), f64::from(x[1]))));
                let t = Instant::now();
                chain.set_agc(gain);
                for x in s {
                    chain.push(x, &mut out);
                }
                cpu += t.elapsed().as_secs_f64();
                if rf.buf.len() >= next_at {
                    rf.decode(true);
                    next_at = rf.buf.len() + chunk;
                }
            }
        }
        for o in out.drain(..) {
            match o {
                Output::Frame(fr) => {
                    if fr.tmcc.bch_corrected.is_none() {
                        tmcc_failed += 1;
                    }
                    // through the Slots words and back
                    let mut words = Vec::new();
                    for b in isdbs3_phy::chain::out::blocks(&fr, lag) {
                        sw.write(&b, &mut words);
                    }
                    slots_words += words.len() as u64;
                    if let Some(w) = slots_out.as_mut() {
                        use std::io::Write;
                        let bytes: Vec<u8> = words.iter().flat_map(|x| x.to_le_bytes()).collect();
                        w.write_all(&bytes).expect("write SLOTS_OUT");
                    }
                    let mut ev = Vec::new();
                    sr.feed(&words, &mut ev);
                    let mut back: BTreeMap<u8, Option<Vec<u8>>> = BTreeMap::new();
                    for e in ev {
                        match e {
                            isdb_frontend::slots::Event::Block {
                                block: isdb_frontend::slots::Block::Slot(b),
                                ..
                            } => {
                                slots_blocks += 1;
                                if let Some((s, _, d)) = isdbs3_phy::chain::out::slot_data(&b) {
                                    back.insert(s, d);
                                }
                            }
                            isdb_frontend::slots::Event::Block { .. } => slots_blocks += 1,
                            _ => slots_losses += 1,
                        }
                    }
                    let slots = (!fr.slots.is_empty()).then(|| {
                        fr.slots
                            .iter()
                            .filter(|s| !s.dummy)
                            .map(|s| {
                                let d = back.remove(&s.slot).flatten();
                                assert_eq!(
                                    d.as_ref(),
                                    s.bch_corrected.map(|_| &s.data),
                                    "Slots round trip of frame {} slot {}",
                                    fr.frame,
                                    s.slot
                                );
                                (
                                    s.slot,
                                    (
                                        d,
                                        s.converged.then_some(s.ldpc_iterations),
                                        s.bch_corrected.is_some(),
                                    ),
                                )
                            })
                            .collect()
                    });
                    if slots.is_none() {
                        undecoded += 1;
                    }
                    last_status = Some(fr.status.clone());
                    chain_frames.insert(fr.at, (fr.frame, slots));
                }
                Output::Dropped { .. } => dropped += 1,
                Output::Restart { sync_lost } => {
                    restarts += 1;
                    println!(
                        "{:8.3} s: chain restart ({})",
                        chain.symbols() as f64 / SYMBOL_RATE,
                        if sync_lost { "sync lost" } else { "input loss" }
                    );
                }
                Output::Head(_) | Output::Symbol(_) | Output::Carrier(_) => {}
            }
        }
        let secs = chain.symbols() as f64 / SYMBOL_RATE;
        if secs >= next_report {
            next_report += report;
            println!(
                "{secs:8.3} s: chain {} frames ({} not decoded, {} dropped), carrier {:+.1} Hz; reference {} frames, tracked {:+.1} Hz; chain CPU {:.3} s per s",
                chain_frames.len(),
                undecoded,
                dropped,
                f64::from(chain.carrier()) * SYMBOL_RATE / 2f64.powi(32),
                rf.frames.len(),
                rf.tracked,
                cpu / secs
            );
        }
        secs < max_s
    })
    .expect("read");
    rf.decode(false);
    let secs = chain.symbols() as f64 / SYMBOL_RATE;
    // compare
    let (mut matched, mut slots, mut equal, mut differ) = (0, 0, 0, 0);
    let (mut only_chain, mut only_ref, mut chain_undec_matched) = (0, 0, 0);
    let (mut c_ldpc, mut c_bch, mut r_ldpc, mut r_bch) = (0, 0, 0, 0);
    let (mut c_it, mut r_it, mut c_n, mut r_n) = (0usize, 0usize, 0usize, 0usize);
    let mut first_diff: Vec<String> = Vec::new();
    for (at, (fc, cs)) in &chain_frames {
        let Some(rs) = rf.frames.get(at) else {
            only_chain += 1;
            continue;
        };
        matched += 1;
        let Some(cs) = cs else {
            chain_undec_matched += 1;
            continue;
        };
        for (slot, (cd, ci, cb)) in cs {
            let Some((rd, ri, rb)) = rs.get(slot) else {
                continue;
            };
            slots += 1;
            match ci {
                Some(i) => {
                    c_it += i;
                    c_n += 1;
                }
                None => c_ldpc += 1,
            }
            match ri {
                Some(i) => {
                    r_it += i;
                    r_n += 1;
                }
                None => r_ldpc += 1,
            }
            c_bch += usize::from(ci.is_some() && !cb);
            r_bch += usize::from(ri.is_some() && !rb);
            match (cd, rd) {
                (Some(x), Some(y)) if x == y => equal += 1,
                (Some(_), Some(_)) => {
                    differ += 1;
                    if first_diff.len() < 5 {
                        first_diff.push(format!(
                            "frame {fc} (symbol {at}) slot {slot}: bytes differ"
                        ));
                    }
                }
                _ => {}
            }
        }
    }
    for at in rf.frames.keys() {
        if !chain_frames.contains_key(at) {
            only_ref += 1;
        }
    }
    let both_ok = slots - c_ldpc.max(r_ldpc);
    println!("--");
    println!(
        "{secs:.3} s of signal; layout lag {lag}; chain restarts {restarts}, frames dropped by a restart {dropped}"
    );
    println!(
        "frames: chain {} (TMCC failed {tmcc_failed}, not decoded {undecoded}), reference {}; matched {matched} (of which the chain did not decode {chain_undec_matched}); only chain {only_chain}, only reference {only_ref}",
        chain_frames.len(),
        rf.frames.len()
    );
    println!(
        "slots compared {slots}: bytes equal {equal}, different {differ} (both decoded ≥ {both_ok})"
    );
    for d in &first_diff {
        println!("  {d}");
    }
    println!(
        "chain: LDPC failed {c_ldpc}, BCH failed {c_bch}, mean {:.3} iterations; reference: LDPC failed {r_ldpc}, BCH failed {r_bch}, mean {:.3} iterations",
        c_it as f64 / c_n.max(1) as f64,
        r_it as f64 / r_n.max(1) as f64
    );
    println!(
        "Slots: {slots_blocks} blocks, {slots_words} words ({:.2} MB/s of signal), losses {slots_losses}; slot bytes compared after the round trip",
        slots_words as f64 * 2.0 / secs / 1e6
    );
    println!(
        "carrier: chain register at the end {:+.1} Hz; reference tracked {:+.1} … {:+.1} Hz",
        f64::from(chain.carrier()) * SYMBOL_RATE / 2f64.powi(32),
        rf.range.0,
        rf.range.1
    );
    if let Some(s) = last_status {
        println!(
            "last status: frame {}, n0 {} (Q.20, MER {:.2} dB), taps from {:?}, centre tap {:?}, counters {:?}",
            s.frame,
            s.n0,
            -10.0 * (s.n0 as f64 / 1_048_576.0).log10(),
            s.taps_from,
            s.taps[5],
            s.counters
        );
    }
    let ldpc = *ldpc_time.lock().unwrap();
    println!(
        "time: chain {cpu:.2} s in push (wall), of which LDPC {ldpc:.2} s (all cores); the model's stages 1–6 and 8 on one thread {:.2} s = {:.3} s per s of signal; reference {:.2} s wall (all cores)",
        cpu - ldpc,
        (cpu - ldpc) / secs,
        rf.wall
    );
}

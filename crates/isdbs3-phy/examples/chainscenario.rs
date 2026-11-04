//! The fixed-point chain (`isdbs3_phy::chain`) over a whole-chain vector
//! set's input with host events injected: the expected `Slots` words of an
//! FPGA chaos run (fpga/isdb_fpga/chain/chaos.py), the model restarted at
//! the same points as the HDL.
//!
//! cargo run --release -p isdbs3-phy --example chainscenario -- SET.json EVENTS.json OUT.json
//!
//! `SET.json`: a whole-chain set of `examples/chainvectors` written with
//! the FPGA decoder's settings (chain-7-hdl, chain-8-loss, chain-9-fade,
//! chain-10-bch): its `input` (flat `[re, im, …]`) and, if present,
//! `bch_errors` (`[frame, slot, [positions]]`: bits flipped in the
//! decoder's output of those slots, as the set was written) are read; its
//! `losses` are not: list them as `loss` events.
//!
//! `EVENTS.json`: one object,
//!
//! - `eq_every` (default 0): [`ChainOptions::eq_every`]; 0 (the HDL
//!   never solves taps, the host does; the four sets are the same with 0
//!   and with 1, the setting chain-7 and -10 were written with);
//! - `taps` (22 integers `re, im` per tap, Q2.14, or `null`): the taps
//!   the host writes at open ([`ChainOptions::initial_taps`]), which are
//!   also the taps a START loads until a `taps` event writes others;
//! - `carrier` (default 0): the host's CARRIER, loaded at every START;
//! - `events`: in stream order, each `{"at": i, "op": …}`. The input is
//!   delivered from index 0; an event at `at` comes after the input up
//!   to symbol `at − 1` (from where the previous event left the input),
//!   and the input continues from index `resume` (default `at`):
//!   - `{"op": "loss", "lost": n}`: the chain is told `n` symbols were
//!     lost ([`Chain::loss`]); a set's own loss has `resume` = `at` (its
//!     input already lacks them), an injected block loss `at + n`;
//!   - `{"op": "start"}`: what CTRL START does to the chain
//!     ([`Chain::start`]: a loss of 0 symbols, CARRIER loaded, the taps
//!     last written in use, as with INIT bit 0), the `Slots` sequence
//!     numbers from 0 (a new writer). `resume` > `at`: the front end's
//!     symbols while the host had stopped the chain are not delivered;
//!     `resume` < `at`: the input replayed from there (a new run);
//!   - `{"op": "taps", "taps": [22 integers]}`: the host writes TAPS: the
//!     next START puts them in use.
//!
//! `OUT.json`: `slots_words` (every `Slots` word the chain's frames give,
//! `chain::out::blocks` through the writer); `frames` (per whole frame:
//! `frame`, `at` (its first symbol in chain symbols, losses included),
//! `done` (the symbols delivered when the model handed it out: the stream
//! position, losses not included), `done_input` (the input index of the
//! last symbol delivered then), `decoded`, `segment` (the events before
//! it), `ldpc_iterations` of its valid slots); `dropped` (`[frame, stream
//! position]` per frame cut short); `restarts` (`[stream position,
//! sync_lost]`); `delivered` (symbols delivered); `symbols` (chain
//! symbols, losses included).
//!
//! With `bch_errors` the flips go into the listed frame's 93/120 decoder
//! call (word w of the call is the w-th valid slot, as the set was
//! written), whenever that frame is decoded: the HDL bench flips the same
//! bits in its decoder's output by frame, slot and word.

use isdbs3_phy::chain::ldpc::{FixedLdpc, LdpcDecoder};
use isdbs3_phy::chain::{Chain, ChainOptions, LAYOUT_LAG, Output};
use serde_json::{Value, json};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

/// The FPGA decoder with bit errors flipped in the 93/120 call of frame
/// `frame` (`examples/chainvectors`' 10-bch: word w of the call is the
/// w-th valid slot of the frame). `open`: the frame the back end had open
/// before the push that decodes (a frame completes in the push of its
/// last symbol's look-ahead).
struct Injected {
    inner: FixedLdpc,
    frame: u32,
    open: Arc<AtomicU32>,
    errors: Vec<(usize, Vec<usize>)>,
}

impl LdpcDecoder for Injected {
    fn decode(
        &mut self,
        code: &isdb_fec::ldpc::Code,
        words: &[&[i8]],
    ) -> Vec<(isdb_fec::ldpc::fixed::LaneResult, Vec<u8>)> {
        let mut out = self.inner.decode(code, words);
        if code.k() == 374 * 93 && self.open.load(Ordering::Relaxed) == self.frame {
            for (w, pos) in &self.errors {
                for &p in pos {
                    out[*w].1[p] ^= 1;
                }
            }
        }
        out
    }
}

fn taps_of(v: &Value) -> Option<[[i16; 2]; 11]> {
    let a = v.as_array()?;
    assert_eq!(a.len(), 22, "22 tap values");
    let mut t = [[0i16; 2]; 11];
    for (k, x) in a.iter().enumerate() {
        t[k / 2][k % 2] = i16::try_from(x.as_i64().expect("tap value")).expect("Q2.14 tap");
    }
    Some(t)
}

fn read_json(path: &str) -> Value {
    let s = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{path}: {e}"));
    serde_json::from_str(&s).unwrap_or_else(|e| panic!("{path}: {e}"))
}

fn index(v: &Value) -> usize {
    usize::try_from(v.as_u64().expect("an index")).expect("index")
}

/// What one run hands out, with the stream positions.
#[derive(Default)]
struct Log {
    words: Vec<u16>,
    frames: Vec<Value>,
    dropped: Vec<Value>,
    restarts: Vec<Value>,
}

impl Log {
    fn take(
        &mut self,
        out: &mut Vec<Output>,
        w: &mut isdb_frontend::slots::Writer,
        (pos, input, segment): (usize, usize, usize),
    ) {
        for o in out.drain(..) {
            match o {
                Output::Frame(f) => {
                    let decoded = !f.slots.is_empty();
                    for b in isdbs3_phy::chain::out::blocks(&f, LAYOUT_LAG) {
                        w.write(&b, &mut self.words);
                    }
                    self.frames.push(json!({
                        "frame": f.frame,
                        "at": f.at,
                        "done": pos,
                        "done_input": input,
                        "decoded": decoded,
                        "segment": segment,
                        "ldpc_iterations": f.slots.iter().filter(|s| !s.dummy)
                            .map(|s| s.ldpc_iterations).collect::<Vec<_>>(),
                    }));
                }
                Output::Dropped { frame } => self.dropped.push(json!([frame, pos])),
                Output::Restart { sync_lost } => self.restarts.push(json!([pos, sync_lost])),
                Output::Carrier(_) | Output::Head(_) | Output::Symbol(_) => {}
            }
        }
    }
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    assert!(
        a.len() == 4,
        "usage: chainscenario SET.json EVENTS.json OUT.json"
    );
    let set = read_json(&a[1]);
    let ev = read_json(&a[2]);
    let input: Vec<[i8; 2]> = set["input"]
        .as_array()
        .expect("input")
        .chunks(2)
        .map(|c| {
            [
                i8::try_from(c[0].as_i64().expect("symbol")).expect("i8 symbol"),
                i8::try_from(c[1].as_i64().expect("symbol")).expect("i8 symbol"),
            ]
        })
        .collect();
    let open_taps = taps_of(&ev["taps"]);
    let carrier = i32::try_from(ev["carrier"].as_i64().unwrap_or(0)).expect("carrier");
    let eq_every = u32::try_from(ev["eq_every"].as_u64().unwrap_or(0)).expect("eq_every");
    let events = ev["events"].as_array().cloned().unwrap_or_default();
    // the listed slots in slot order are the frame's valid slots 10 … 17
    // (chainvectors' write_bch: decoder word = valid-slot index)
    let listed = set["bch_errors"].as_array().cloned().unwrap_or_default();
    let bch_frame = listed
        .first()
        .map(|e| u32::try_from(e[0].as_u64().expect("frame")).expect("frame"));
    let errors: Vec<(usize, Vec<usize>)> = listed
        .iter()
        .enumerate()
        .map(|(i, e)| {
            let pos = e[2]
                .as_array()
                .expect("positions")
                .iter()
                .map(index)
                .collect();
            (10 + i, pos)
        })
        .collect();
    let opt = ChainOptions {
        eq_every,
        initial_taps: open_taps,
        ..ChainOptions::default()
    };
    let open = Arc::new(AtomicU32::new(u32::MAX));
    let mut c = match bch_frame {
        None => Chain::with_ldpc(opt, Box::new(FixedLdpc::hdl())),
        Some(frame) => {
            let inj = Injected {
                inner: FixedLdpc::hdl(),
                frame,
                open: Arc::clone(&open),
                errors,
            };
            Chain::with_ldpc(opt, Box::new(inj))
        }
    };
    let mut written = open_taps;
    let mut w = isdb_frontend::slots::Writer::new();
    let mut log = Log::default();
    let mut out = Vec::new();
    let (mut ptr, mut pos, mut segment, mut last) = (0usize, 0usize, 0usize, 0usize);
    let mut push_to = |c: &mut Chain,
                       w: &mut isdb_frontend::slots::Writer,
                       log: &mut Log,
                       (ptr, pos, last): (&mut usize, &mut usize, &mut usize),
                       end: usize,
                       segment: usize| {
        assert!(end <= input.len(), "an event after the input's end");
        while *ptr < end {
            open.store(c.back().open_frame().unwrap_or(u32::MAX), Ordering::Relaxed);
            c.push(input[*ptr], &mut out);
            *last = *ptr;
            *ptr += 1;
            *pos += 1;
            log.take(&mut out, w, (*pos, *last, segment));
        }
    };
    let mut held = Vec::new();
    for e in &events {
        let at = index(&e["at"]);
        assert!(at >= ptr, "event at {at} before the input position {ptr}");
        push_to(
            &mut c,
            &mut w,
            &mut log,
            (&mut ptr, &mut pos, &mut last),
            at,
            segment,
        );
        match e["op"].as_str().expect("op") {
            "loss" => c.loss(e["lost"].as_u64().expect("lost"), &mut held),
            "start" => {
                c.start(carrier, written, &mut held);
                w = isdb_frontend::slots::Writer::new();
            }
            "taps" => written = taps_of(&e["taps"]),
            op => panic!("unknown event {op}"),
        }
        if e["op"] != "taps" {
            segment += 1;
            ptr = e.get("resume").map_or(at, index);
        }
        log.take(&mut held, &mut w, (pos, last, segment));
    }
    push_to(
        &mut c,
        &mut w,
        &mut log,
        (&mut ptr, &mut pos, &mut last),
        input.len(),
        segment,
    );
    let v = json!({
        "format": 1,
        "slots_words": log.words,
        "frames": log.frames,
        "dropped": log.dropped,
        "restarts": log.restarts,
        "delivered": pos,
        "symbols": c.symbols(),
    });
    std::fs::write(&a[3], serde_json::to_string(&v).expect("json")).expect("write OUT.json");
}

//! Write the FPGA LDPC decoder's test vectors (JSON) from the fixed-point
//! model `isdb_fec::ldpc::fixed` with the ISDB-S3 codes (ARIB STD-B44 v2.1
//! §3.4 (2), and §3.5 for the shortened TMCC code), for the HDL
//! simulation in `fpga/` (docs/plans/isdbs3-fpga-rx.md, "The LDPC
//! schedule contract", work split item 1). The layered sets follow the
//! amended contract (2026-11-01): a variable with several edges in one
//! step gets the delta write-back, sat(t₁ + r₁,new + Σᵢ₌₂ (rᵢ,new −
//! rᵢ,old)); vectors written before that date are of the first rule
//! and no longer match.
//!
//! cargo run --release -p isdbs3-phy --example ldpcvectors -- OUTDIR [--trace] [--recording REC]
//!
//! Writes into OUTDIR (`mise run fpga:vectors`: `fpga/build`):
//!
//! | File | Code | Schedule | Limit | Codewords |
//! |---|---|---|---|---|
//! | `ldpc-tables.json` | the eleven address tables | | | |
//! | `ldpc-1-93-seq.json` | 93/120 | sequential | 50 | 8, Es/N0 from easy to failing |
//! | `ldpc-2-93-layered34.json` | 93/120 | layered, step 34 | 12 | 8 |
//! | `ldpc-3-93-layered187.json` | 93/120 | layered, step 187 | 12 | 8 |
//! | `ldpc-4-41-layered34.json` | 41/120 (79 layers, degree ≤ 5 + 2) | layered, step 34 | 12 | 6 |
//! | `ldpc-5-109-layered34.json` | 109/120 (11 layers, degree ≤ 32) | layered, step 34 | 12 | 6 |
//! | `ldpc-6-tmcc-layered34.json` | 61/120 shortened (TMCC) | layered, step 34 | 12 | 6 |
//! | `ldpc-7-93-stress-layered34.json` | 93/120 | layered, step 34 | 12 | 8: clean (iteration 0), all-zero input, ±127 with strong flips, −128 inputs, failing |
//! | `ldpc-8-93-trace.json` | 93/120 | layered, step 34 | 12 | 2, with `trace_hex` |
//! | `ldpc-9-93-iter0-layered34.json` | 93/120 | layered, step 34 | 0 | 4 |
//! | `ldpc-10-93-rec-layered34.json` | 93/120, real codewords | layered, step 34 | 12 | 8 (opt-in) |
//! | `ldpc-11-tmcc-rec-layered34.json` | TMCC, real codewords | layered, step 34 | 12 | 4 (opt-in): 2 as received, 2 with 3 % of the values made weak and wrong |
//! | `ldpc-12-93-layered17.json` | 93/120 | layered, step 17 | 12 | 4, with `trace_hex` (a small step for the HDL simulation) |
//! | `ldpc-13-tmcc-layered17.json` | 61/120 shortened (TMCC) | layered, step 17 | 12 | 4, with `trace_hex` |
//!
//! The synthetic codewords are `Code::encode` of random information (a
//! fixed seed per set) sent as BPSK through Gaussian noise at a given
//! Es/N0, LLR = 2y/σ² · 2 rounded and saturated to ±127. The two
//! recording sets are written only with `--recording REC`, or with
//! `ISDB_RS_ISDBS3_TLV_IQ` set (its first `:`-separated SigMF
//! recording), from the recording's first frame, demapped as `isdb rx`
//! does and quantised as `Ldpc::FIXED` (input scale 1); the TMCC words
//! with the known zeros at +127. `--trace` adds `trace_hex` to every set
//! (`ldpc-8` always has it). Every set's results are also checked to be
//! the same with 16 lanes as with one.
//!
//! # Format (version 1)
//!
//! A vectors file:
//!
//! ```text
//! {
//!   "format": "isdb-rs ldpc vectors", "version": 1,
//!   "rate": 93,             // numerator of r/120 (61 for the TMCC code)
//!   "n": 44880, "k": 34782, "q": 27, "p": 10098,
//!   "shortened": null | {"info_start": 1870, "info_len": 9614},
//!   "schedule": {"kind": "sequential"} | {"kind": "layered", "step": 68},
//!   "max_iter": 12, "offset": 1,
//!   "codewords": [ {
//!     "llr": [ n values, −128 … 127 ],
//!     "bits_hex": "…",      // the n hard decisions after decoding
//!     "iterations": 3, "converged": true, "unsatisfied": 0,
//!     "trace_hex": ["…", …] // optional: after iterations 1 … iterations
//!   } ]
//! }
//! ```
//!
//! - `shortened`: the TMCC code (§3.5). The LLR positions of the first k
//!   outside [info_start, info_start + info_len) are known zeros; the
//!   file still carries all n LLRs, with the value the CPU uses there
//!   (+127), so a decoder that fills them itself must use +127.
//! - `bits_hex`, `trace_hex`: n bits packed 8 per byte, bit i in byte
//!   i / 8 at bit i % 8 (least significant first), lowercase hex (n = 44
//!   880: 5 610 bytes, 11 220 characters). `bits_hex` is the hard decision
//!   (L < 0 is a one) of the a-posteriori values when the decoder stopped.
//! - `iterations`: iterations run (0: the input's hard decision was a
//!   codeword, or `max_iter` is 0); `converged`: every check satisfied;
//!   `unsatisfied`: the checks not satisfied at the end (0 when
//!   converged).
//! - Input LLRs are i8; the CPU's `quantise` gives ±127 at most, but the
//!   arithmetic reaches −128 inside (t = sat(−127 − 1)), and the stress
//!   set has −128 inputs (|−128| reads 127).
//!
//! The tables file: `{"format": "isdb-rs ldpc tables", "version": 1,
//! "n": 44880, "group": 374, "tables": [{"rate": 41, "rows": [[…], …]},
//! …]}`, the eleven address tables of §3.4 (2) Appendix 1 in rate order,
//! one row per 374-bit information group (k = 374 · rows, P = n − k,
//! q = P / 374; bit j of group g is in the checks (x + j · q) mod P for
//! each x of row g, plus the staircase: check c also has the parity bits
//! c − 1 (for c > 0) and c, at codeword positions k + …; the decoder's
//! edge order within a check is the information bits in increasing
//! index, then parity c − 1, then parity c, `Code::check`).

mod common;

use isdb_fec::ldpc::Code;
use isdb_fec::ldpc::fixed::{Decoder, LaneResult, Params, Schedule, quantise};
use isdbs3_core::CodeRate;
use isdbs3_core::params::{N_LDPC, TMCC_INFO_BITS};
use isdbs3_core::tmcc::{DATA_BITS, NULL_HEAD, TmccInfo};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// Offset of every set (`Ldpc::FIXED`).
const OFFSET: i8 = 1;

/// xorshift64 with a Box–Muller normal.
struct Rng(u64);

impl Rng {
    fn bits(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn uniform(&mut self) -> f64 {
        (self.bits() >> 11) as f64 / (1u64 << 53) as f64
    }
    fn gauss(&mut self) -> f64 {
        let (u1, u2) = (self.uniform() + 1e-300, self.uniform());
        (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos()
    }
}

/// `bits` (0/1) sent as BPSK (0 → +1) at `esn0_db`: i8 LLRs 2y/σ² · 2.
fn bpsk(bits: &[u8], esn0_db: f64, rng: &mut Rng) -> Vec<i8> {
    let var = 1.0 / (2.0 * 10f64.powf(esn0_db / 10.0));
    let llr: Vec<f32> = bits
        .iter()
        .map(|&b| {
            let y = if b == 0 { 1.0 } else { -1.0 } + var.sqrt() * rng.gauss();
            // an LLR of a few hundred at most: inside f32
            #[allow(clippy::cast_possible_truncation)]
            let l = (2.0 * y / var) as f32;
            l
        })
        .collect();
    let mut q = Vec::new();
    quantise(&llr, 2.0, &mut q);
    q
}

/// A random codeword of `code`.
fn codeword(code: &Code, rng: &mut Rng) -> Vec<u8> {
    let info: Vec<u8> = (0..code.k()).map(|_| (rng.bits() & 1) as u8).collect();
    code.encode(&info)
}

/// n bits, 8 per byte least significant first, lowercase hex.
fn hex(bits: &[u8]) -> String {
    let mut s = String::with_capacity(bits.len() / 4 + 2);
    for ch in bits.chunks(8) {
        let b = ch
            .iter()
            .enumerate()
            .fold(0u8, |a, (i, &x)| a | ((x & 1) << i));
        write!(s, "{b:02x}").expect("write to String");
    }
    s
}

/// One vectors file.
struct Set {
    name: &'static str,
    rate: CodeRate,
    shortened: bool,
    schedule: Schedule,
    max_iter: usize,
    trace: bool,
    llrs: Vec<Vec<i8>>,
}

/// Decode a set (one lane, the scalar reference, checked against 16) and
/// write it; returns the file size and a summary line.
fn write_set(dir: &Path, set: &Set, trace_all: bool) -> String {
    let code = isdbs3_core::ldpc::code(set.rate);
    let p = Params {
        max_iter: set.max_iter,
        offset: OFFSET,
        schedule: set.schedule,
    };
    let refs: Vec<&[i8]> = set.llrs.iter().map(Vec::as_slice).collect();
    let mut traces: Vec<Vec<String>> = vec![Vec::new(); refs.len()];
    let trace = set.trace || trace_all;
    let out = Decoder::<1>::default().decode_all_traced(code, &refs, p, |w, b| {
        if trace {
            traces[w].push(hex(b));
        }
    });
    assert_eq!(
        out,
        Decoder::<16>::default().decode_all(code, &refs, p),
        "{}: 16 lanes differ from one",
        set.name
    );
    let mut j = String::new();
    j.push_str("{\"format\":\"isdb-rs ldpc vectors\",\"version\":1,");
    write!(
        j,
        "\"rate\":{},\"n\":{},\"k\":{},\"q\":{},\"p\":{},",
        set.rate.numerator(),
        code.n(),
        code.k(),
        code.q(),
        code.checks()
    )
    .expect("write to String");
    if set.shortened {
        write!(
            j,
            "\"shortened\":{{\"info_start\":{NULL_HEAD},\"info_len\":{DATA_BITS}}},"
        )
        .expect("write to String");
    } else {
        j.push_str("\"shortened\":null,");
    }
    match set.schedule {
        Schedule::Sequential => j.push_str("\"schedule\":{\"kind\":\"sequential\"},"),
        Schedule::Layered { step } => {
            write!(j, "\"schedule\":{{\"kind\":\"layered\",\"step\":{step}}},")
                .expect("write to String");
        }
    }
    write!(
        j,
        "\"max_iter\":{},\"offset\":{OFFSET},\n\"codewords\":[",
        set.max_iter
    )
    .expect("write to String");
    let mut summary = Vec::new();
    for (i, (llr, (r, bits))) in set.llrs.iter().zip(&out).enumerate() {
        let (it, conv, unsat) = describe(*r);
        summary.push(if conv {
            it.to_string()
        } else {
            format!("F{it}")
        });
        j.push_str(if i == 0 { "\n{" } else { ",\n{" });
        j.push_str("\"llr\":[");
        for (e, v) in llr.iter().enumerate() {
            if e > 0 {
                j.push(',');
            }
            write!(j, "{v}").expect("write to String");
        }
        write!(
            j,
            "],\"bits_hex\":\"{}\",\"iterations\":{it},\"converged\":{conv},\"unsatisfied\":{unsat}",
            hex(bits)
        )
        .expect("write to String");
        if trace {
            assert_eq!(traces[i].len(), it, "{}: trace length", set.name);
            j.push_str(",\"trace_hex\":[");
            j.push_str(
                &traces[i]
                    .iter()
                    .map(|t| format!("\"{t}\""))
                    .collect::<Vec<_>>()
                    .join(","),
            );
            j.push(']');
        }
        j.push('}');
    }
    j.push_str("\n]}\n");
    let path = dir.join(format!("{}.json", set.name));
    std::fs::write(&path, &j).expect("write vectors");
    format!(
        "{}: {} bytes, {} codewords, iterations {}",
        path.display(),
        j.len(),
        set.llrs.len(),
        summary.join(" ")
    )
}

fn describe(r: LaneResult) -> (usize, bool, usize) {
    match r {
        Ok(d) => (d.iterations, true, 0),
        Err(e) => (e.iterations, false, e.unsatisfied),
    }
}

/// The eleven address tables.
fn write_tables(dir: &Path) -> String {
    let mut j = format!(
        "{{\"format\":\"isdb-rs ldpc tables\",\"version\":1,\"n\":{N_LDPC},\"group\":{},\"tables\":[",
        isdb_fec::ldpc::GROUP
    );
    for (t, (rate, rows)) in isdbs3_core::ldpc_tables::TABLES.iter().enumerate() {
        if t > 0 {
            j.push(',');
        }
        write!(j, "\n{{\"rate\":{rate},\"rows\":[").expect("write to String");
        let rs: Vec<String> = rows
            .iter()
            .map(|r| {
                let v: Vec<String> = r.iter().map(u16::to_string).collect();
                format!("[{}]", v.join(","))
            })
            .collect();
        j.push_str(&rs.join(","));
        j.push_str("]}");
    }
    j.push_str("\n]}\n");
    let path = dir.join("ldpc-tables.json");
    std::fs::write(&path, &j).expect("write tables");
    format!("{}: {} bytes", path.display(), j.len())
}

fn rate(r: u8) -> CodeRate {
    CodeRate::new(r).expect("a B44 rate")
}

/// Synthetic codewords of `r` at each Es/N0 (dB), seed `seed`.
fn synthetic(r: CodeRate, esn0: &[f64], seed: u64) -> Vec<Vec<i8>> {
    let code = isdbs3_core::ldpc::code(r);
    let mut rng = Rng(seed);
    esn0.iter()
        .map(|&e| bpsk(&codeword(code, &mut rng), e, &mut rng))
        .collect()
}

/// Shortened TMCC words (§3.5) at each Es/N0: random information through
/// `isdbs3_core::tmcc::encode`, the 31 680 sent bits through the noise,
/// the known zeros at +127.
fn tmcc_words(esn0: &[f64], seed: u64) -> Vec<Vec<i8>> {
    let mut rng = Rng(seed);
    esn0.iter()
        .map(|&e| {
            let info: Vec<u8> = (0..TMCC_INFO_BITS)
                .map(|_| (rng.bits() & 1) as u8)
                .collect();
            let sent = isdbs3_core::tmcc::encode(&TmccInfo::parse(&info));
            isdbs3_phy::tmcc::ldpc_word(&bpsk(&sent, e, &mut rng), 127)
        })
        .collect()
}

/// The stress set: a clean codeword (a codeword at iteration 0), all-zero
/// input, ±127 with strong wrong bits, −128 for every one bit, a word at
/// an Es/N0 that fails, and three near threshold.
fn stress() -> Vec<Vec<i8>> {
    let r = rate(93);
    let code = isdbs3_core::ldpc::code(r);
    let mut rng = Rng(7_007);
    let sign = |c: &[u8], m: i8| -> Vec<i8> {
        c.iter()
            .map(|&b| if b == 0 { m } else { m.saturating_neg() })
            .collect()
    };
    let clean = sign(&codeword(code, &mut rng), 40);
    let zeros = vec![0i8; N_LDPC];
    let mut sat = sign(&codeword(code, &mut rng), 127);
    for _ in 0..300 {
        let i = usize::try_from(rng.bits() % N_LDPC as u64).expect("index");
        sat[i] = -sat[i];
    }
    // −128 on every one bit, +127 on the zeros, 200 of them flipped weak
    let mut min: Vec<i8> = codeword(code, &mut rng)
        .iter()
        .map(|&b| if b == 0 { 127 } else { -128 })
        .collect();
    for _ in 0..200 {
        let i = usize::try_from(rng.bits() % N_LDPC as u64).expect("index");
        min[i] = if min[i] > 0 { -3 } else { 3 };
    }
    let mut out = vec![clean, zeros, sat, min];
    out.extend(synthetic(r, &[0.5, 2.2, 2.4, 2.6], 7_008));
    out
}

/// The recording sets: the first frame's codewords of `rec`.
fn recording(rec: &str) -> (Vec<Vec<i8>>, Vec<Vec<i8>>) {
    let (mut main, mut tmcc) = (Vec::new(), Vec::new());
    common::for_each_word(rec, 4, 0.0, |w| {
        let mut q = Vec::new();
        match w {
            common::Word::Main(r, llr, _) if r == rate(93) && main.len() < 8 => {
                quantise(&llr, 1.0, &mut q);
                main.push(q);
            }
            common::Word::Tmcc(llr) if tmcc.len() < 2 => {
                quantise(&llr, 1.0, &mut q);
                tmcc.push(isdbs3_phy::tmcc::ldpc_word(&q, 127));
            }
            _ => {}
        }
    });
    // the real TMCC words are codewords as received (a strong BPSK
    // signal): two more from them with 3 % of the received values
    // replaced by a weak wrong value, so the decoder has work to do
    let mut rng = Rng(11_011);
    let extra: Vec<Vec<i8>> = tmcc
        .iter()
        .map(|w| {
            let mut w = w.clone();
            let p0 = NULL_HEAD + DATA_BITS + isdbs3_core::tmcc::NULL_TAIL;
            for i in (NULL_HEAD..NULL_HEAD + DATA_BITS).chain(p0..N_LDPC) {
                if rng.bits() % 100 < 3 {
                    w[i] = if w[i] < 0 { 6 } else { -6 };
                }
            }
            w
        })
        .collect();
    tmcc.extend(extra);
    (main, tmcc)
}

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let trace_all = args.iter().any(|a| a == "--trace");
    args.retain(|a| a != "--trace");
    let rec = args
        .iter()
        .position(|a| a == "--recording")
        .map(|i| {
            let v = args[i + 1].clone();
            args.drain(i..=i + 1);
            v
        })
        .or_else(|| {
            std::env::var("ISDB_RS_ISDBS3_TLV_IQ")
                .ok()
                .and_then(|l| l.split(':').next().map(str::to_string))
                .filter(|s| !s.is_empty())
        });
    let dir = PathBuf::from(args.first().expect("OUTDIR"));
    std::fs::create_dir_all(&dir).expect("create OUTDIR");
    let layered = |step| Schedule::Layered { step };
    let r93 = rate(93);
    let mut sets = vec![
        Set {
            name: "ldpc-1-93-seq",
            rate: r93,
            shortened: false,
            schedule: Schedule::Sequential,
            max_iter: 50,
            trace: false,
            llrs: synthetic(r93, &[4.0, 3.0, 2.4, 2.2, 2.0, 1.8, 1.6, 0.5], 1_001),
        },
        Set {
            name: "ldpc-2-93-layered34",
            rate: r93,
            shortened: false,
            schedule: layered(34),
            max_iter: 12,
            trace: false,
            llrs: synthetic(r93, &[4.0, 3.0, 2.4, 2.2, 2.0, 1.8, 1.6, 0.5], 2_002),
        },
        Set {
            name: "ldpc-3-93-layered187",
            rate: r93,
            shortened: false,
            schedule: layered(187),
            max_iter: 12,
            trace: false,
            llrs: synthetic(r93, &[4.0, 3.0, 2.4, 2.2, 2.0, 1.8, 1.6, 0.5], 3_003),
        },
        Set {
            name: "ldpc-4-41-layered34",
            rate: rate(41),
            shortened: false,
            schedule: layered(34),
            max_iter: 12,
            trace: false,
            llrs: synthetic(rate(41), &[0.0, -1.0, -1.5, -2.0, -2.3, -5.0], 4_004),
        },
        Set {
            name: "ldpc-5-109-layered34",
            rate: rate(109),
            shortened: false,
            schedule: layered(34),
            max_iter: 12,
            trace: false,
            llrs: synthetic(rate(109), &[6.0, 5.0, 4.5, 4.2, 4.0, 2.5], 5_005),
        },
        Set {
            name: "ldpc-6-tmcc-layered34",
            rate: CodeRate::TMCC,
            shortened: true,
            schedule: layered(34),
            max_iter: 12,
            trace: false,
            llrs: tmcc_words(&[2.0, 0.0, -1.0, -1.5, -2.0, -4.0], 6_006),
        },
        Set {
            name: "ldpc-7-93-stress-layered34",
            rate: r93,
            shortened: false,
            schedule: layered(34),
            max_iter: 12,
            trace: false,
            llrs: stress(),
        },
        Set {
            name: "ldpc-8-93-trace",
            rate: r93,
            shortened: false,
            schedule: layered(34),
            max_iter: 12,
            trace: true,
            llrs: synthetic(r93, &[2.4, 1.5], 8_008),
        },
        Set {
            name: "ldpc-9-93-iter0-layered34",
            rate: r93,
            shortened: false,
            schedule: layered(34),
            max_iter: 0,
            trace: false,
            llrs: {
                let mut v = stress();
                v.truncate(2);
                v.extend(synthetic(r93, &[3.0, 1.5], 9_009));
                v
            },
        },
        // Small steps for the HDL's Python simulation (its step count is
        // 374 / step per layer; 17 divides 374 = 2 · 11 · 17), with traces.
        Set {
            name: "ldpc-12-93-layered17",
            rate: r93,
            shortened: false,
            schedule: layered(17),
            max_iter: 12,
            trace: true,
            llrs: synthetic(r93, &[3.0, 2.2, 1.8, 0.5], 12_012),
        },
        Set {
            name: "ldpc-13-tmcc-layered17",
            rate: CodeRate::TMCC,
            shortened: true,
            schedule: layered(17),
            max_iter: 12,
            trace: true,
            llrs: tmcc_words(&[2.0, -1.0, -2.0, -4.0], 13_013),
        },
    ];
    if let Some(rec) = rec {
        let (main, tmcc) = recording(&rec);
        eprintln!(
            "{rec}: {} main and {} TMCC codewords",
            main.len(),
            tmcc.len()
        );
        sets.push(Set {
            name: "ldpc-10-93-rec-layered34",
            rate: r93,
            shortened: false,
            schedule: layered(34),
            max_iter: 12,
            trace: false,
            llrs: main,
        });
        sets.push(Set {
            name: "ldpc-11-tmcc-rec-layered34",
            rate: CodeRate::TMCC,
            shortened: true,
            schedule: layered(34),
            max_iter: 12,
            trace: false,
            llrs: tmcc,
        });
    }
    eprintln!("{}", write_tables(&dir));
    for s in sets.iter().filter(|s| !s.llrs.is_empty()) {
        eprintln!("{}", write_set(&dir, s, trace_all));
    }
}

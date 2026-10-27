//! LDPC decoding throughput on live codewords: one thread and all
//! threads, the floating-point reference and the fixed-point SIMD decoder
//! (`isdbs3_phy::main_signal::Ldpc::FIXED`), against the 2 905 codewords
//! per second of 16APSK 93/120 in 120 slots (development tool).
//!
//! cargo run --release -p isdbs3-phy --example ldpcspeed -- REC

use isdb_iq::Recording;
use isdbs3_core::interleave::permutation;
use isdbs3_core::params::{FRAME_SYMBOLS, N_LDPC, SYMBOL_RATE};
use isdbs3_core::slots::layout;
use isdbs3_phy::frontend::{Trim, symbols};
use isdbs3_phy::main_signal::{demap, learned_points, main_symbols, pilots};
use isdbs3_phy::sync::synchronise;
use isdbs3_phy::tmcc::{decode, llrs};

fn main() {
    let path = std::path::PathBuf::from(std::env::args().nth(1).expect("REC"));
    let mut rec = Recording::open(&path, None, None).unwrap();
    let seg = rec.segments()[0].clone();
    let n = (usize::try_from(seg.end - seg.start).expect("segment length")).min(8_000_000);
    let x = rec.read(seg.start, n).unwrap();
    let s = symbols(&x, rec.sample_rate, None, Trim::Auto).unwrap();
    let sy = synchronise(&s.z).unwrap();
    let f = sy.frames[0];
    let es = 10f64.powf(sy.sync_snr_db / 10.0);
    let t = decode(&llrs(&s.z, &sy.slots, f, 1.0 / (2.0 * es)).unwrap(), 50)
        .unwrap()
        .info;
    let lay = layout(&t).unwrap();
    let ms = main_symbols(&s.z, &sy.slots, f).unwrap();
    let pil = pilots(&s.z, &sy.slots, f).unwrap();
    let pts = learned_points(&pil, &lay, lay[0].modulation).unwrap();
    let n0 = 0.04;
    // the frame's 96 codewords as LLRs in codeword order
    let words: Vec<(isdbs3_core::CodeRate, Vec<f32>)> = lay
        .iter()
        .filter(|s| !s.dummy)
        .map(|sl| {
            let mut lab = Vec::new();
            demap(
                &ms[sl.symbol_start..sl.symbol_start + sl.symbols],
                &pts,
                sl.modulation.bits(),
                n0,
                &mut lab,
            );
            let perm = permutation(sl.modulation, sl.rate);
            let mut llr = vec![0f32; N_LDPC];
            for (i, &p) in perm.iter().enumerate() {
                llr[p as usize] = lab[i];
            }
            (sl.rate, llr)
        })
        .collect();
    let need = words.len() as f64 * SYMBOL_RATE / FRAME_SYMBOLS as f64;
    let run_float = |threads: usize| -> f64 {
        let reps = 4;
        let t0 = std::time::Instant::now();
        std::thread::scope(|sc| {
            for k in 0..threads {
                let words = &words;
                sc.spawn(move || {
                    let mut bits = Vec::new();
                    for r in 0..reps {
                        for (i, (rate, llr)) in words.iter().enumerate() {
                            if (i + r * words.len()) % threads != k {
                                continue;
                            }
                            let mut l = llr.clone();
                            isdbs3_core::ldpc::code(*rate)
                                .decode(&mut l, &mut bits, 50, 0.8)
                                .unwrap();
                        }
                    }
                });
            }
        });
        (reps * words.len()) as f64 / t0.elapsed().as_secs_f64()
    };
    // fixed point: every thread streams its share of `reps` copies of the
    // frame's codewords through one 16-lane decoder
    let isdbs3_phy::main_signal::Ldpc::Fixed {
        max_iter,
        input_scale,
        offset,
        ..
    } = isdbs3_phy::main_signal::Ldpc::FIXED
    else {
        unreachable!()
    };
    let q: Vec<Vec<i8>> = words
        .iter()
        .map(|(_, l)| {
            let mut v = Vec::new();
            isdb_fec::ldpc::fixed::quantise(l, input_scale, &mut v);
            v
        })
        .collect();
    let run_fixed = |threads: usize| -> f64 {
        let reps = 16;
        let all: Vec<&[i8]> = (0..reps)
            .flat_map(|_| q.iter().map(Vec::as_slice))
            .collect();
        let code = isdbs3_core::ldpc::code(words[0].0);
        let p = isdb_fec::ldpc::fixed::Params { max_iter, offset };
        let t0 = std::time::Instant::now();
        std::thread::scope(|sc| {
            for share in all.chunks(all.len().div_ceil(threads)) {
                sc.spawn(move || {
                    isdb_fec::ldpc::fixed::Decoder::<{ isdbs3_phy::main_signal::LANES }>::default()
                        .decode_all(code, share, p)
                });
            }
        });
        all.len() as f64 / t0.elapsed().as_secs_f64()
    };
    let all_n = std::thread::available_parallelism().map_or(1, |n| n.get());
    let bits = |cw: f64| cw * words[0].0.data_bits() as f64 / 1e6;
    println!(
        "needed: {need:.0} codewords/s ({:.1} Mbit/s of slot data); SIMD back end: {}",
        bits(need),
        isdb_fec::simd::BACKEND
    );
    for (name, run) in [
        ("float", &run_float as &dyn Fn(usize) -> f64),
        ("fixed", &run_fixed),
    ] {
        for t in [1, all_n] {
            let cw = run(t);
            println!(
                "{name} {t:2} thread(s): {cw:.0} codewords/s ({:.1} Mbit/s) = {:.2}× real time",
                bits(cw),
                cw / need
            );
        }
    }
}

//! The digital loopback of the fixed-point transmit interpolator
//! (`shape::fixed::Interpolator`) into the receive front end
//! (`fast::FastFrontend`, ISDB-S at 40 Msps as the receivers design it,
//! trim 0): random 8PSK codes, the model's 12-bit integers straight in,
//! the 8PSK MER of the 6-bit output and of `zk`.
//!
//! cargo run --release -p isdb-frontend --example txloop -- loop [SYMBOLS] [SEEDS]
//! cargo run --release -p isdb-frontend --example txloop -- study [SYMBOLS]
//!
//! `loop`: the MER over the second half of the output in the steady state
//! (the last 64 output symbols excluded), for the model (span 12, and 16)
//! and the f32 `Shaper` (span 16); and as `examples/txinterp.rs loop` and
//! `tests/shape.rs` measure it, over every output symbol, the stream's
//! end included. `metrics::psk_mer` counts every symbol after `settle`,
//! so the receiver's outputs over the zero tail after the last symbol
//! (2·span − 1 symbols, as many as the run's samples reach past the
//! receive filter's delay) count as errors of the full ring radius: each
//! costs about 0.02 dB at the 6-bit output and 0.1 dB before the
//! quantiser at 100 000 measured symbols. The model's run here ends where
//! `txinterp`'s does (one symbol later, its first code entering one
//! carry later); the shaper's `finish` tail is 31 symbols.
//!
//! `study`: the model against the arithmetic of `txinterp`'s "mux floor½
//! P 512 span 12 Q0.24" (interpolator.md §1), rebuilt here: the same
//! banks, step and shift, and the same samples bit for bit once the
//! study's stream gets one leading zero symbol (the study forced a carry
//! before sample 0; the model does not).

// A measurement tool: f64 sample counts and positions are converted to
// indices, and the study's arithmetic is mirrored as written.
#![allow(clippy::cast_possible_truncation, clippy::needless_range_loop)]

use isdb_frontend::fast::FastFrontend;
use isdb_frontend::metrics::psk_mer;
use isdb_frontend::params::{Design, Params};
use isdb_frontend::shape::Shaper;
use isdb_frontend::shape::fixed::{Interpolator, TxDesign, TxParams};
use isdb_frontend::tables::rrc;
use isdb_frontend::txformat;
use isdb_iq::survey::KNOWN_SYSTEMS;
use num_complex::Complex32;
use std::f64::consts::FRAC_1_SQRT_2;

const RS: f64 = 28.86e6;
const ALPHA: f64 = 0.35;

/// `n` random point codes from a xorshift64 (the top three bits), the
/// symbol sequence of `txinterp`.
fn codes(n: usize, seed: u64) -> Vec<u8> {
    let mut s = seed | 1;
    (0..n)
        .map(|_| {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            txformat::point_code(s.to_le_bytes()[7] >> 5)
        })
        .collect()
}

/// (6-bit MER, zk MER) over the second half of the front end's output on
/// `x`, the last `drop` symbols excluded.
fn rx(x: &[[i16; 2]], drop: usize) -> (f64, f64) {
    let d = Design::for_system(&KNOWN_SYSTEMS[0], 40e6);
    let mut fe = FastFrontend::new(Params::design(&d).unwrap()).unwrap();
    let mut out = Vec::new();
    fe.process(x, &mut out);
    out.truncate(out.len() - drop);
    let half = out.len() / 2;
    let q: Vec<[f64; 2]> = out.iter().map(|s| s.q.map(f64::from)).collect();
    let z: Vec<[f64; 2]> = out.iter().map(|s| s.zk.map(f64::from)).collect();
    (
        psk_mer(&q, 8, 1e-3, half).mer_db,
        psk_mer(&z, 8, 1e-3, half).mer_db,
    )
}

/// Samples until `symbols` codes have entered (⌈symbols·2²⁴ / step⌉).
fn samples_for(p: &TxParams, symbols: usize) -> usize {
    usize::try_from((((symbols as u64) << 24) - 1) / u64::from(p.step) + 1).unwrap()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let arg = |i: usize, d: usize| args.get(i).map_or(d, |s| s.parse().unwrap());
    match args.get(1).map_or("loop", String::as_str) {
        "loop" => lo(arg(2, 200_000), arg(3, 4)),
        "study" => study(arg(2, 20_000)),
        other => panic!("loop|study, not {other}"),
    }
}

fn lo(n: usize, seeds: usize) {
    let p = TxParams::design(&TxDesign::new(40e6, RS, ALPHA)).unwrap();
    let seed_list = [0x0123_4567_89ab_cdef_u64, 1, 2, 3, 4, 5, 6, 7];
    // 64 symbols: past the tail's 23 and the receive filter's delay.
    let end = 64;
    for &seed in seed_list.iter().take(seeds) {
        let c = codes(n, seed);
        let mut m = Interpolator::new(p.clone()).unwrap();
        m.push_codes(&c);
        let mut y = Vec::new();
        // the tail of `txinterp`: 2·span − 1 zero symbols (dry requests)
        m.process(samples_for(&p, n + 1 + 23), &mut y);
        let (sq, sz) = rx(&y, end);
        let (tq, tz) = rx(&y, 0);
        let p16 = TxParams::design(&TxDesign {
            span: 16,
            ..TxDesign::new(40e6, RS, ALPHA)
        })
        .unwrap();
        let mut m16 = Interpolator::new(p16.clone()).unwrap();
        m16.push_codes(&c);
        let mut y16 = Vec::new();
        m16.process(samples_for(&p16, n + 1 + 31), &mut y16);
        let (hq, hz) = rx(&y16, end);
        let mut sh = Shaper::new(40e6, RS, ALPHA).unwrap();
        let pts: Vec<Complex32> = c
            .iter()
            .map(|&k| {
                let v = txformat::point(k);
                Complex32::new(v.re as f32, v.im as f32)
            })
            .collect();
        let mut f = Vec::new();
        sh.push(&pts, &mut f);
        sh.finish(&mut f);
        let b = 10f32.powf(-0.5);
        let x: Vec<[i16; 2]> = f
            .iter()
            .map(|&v| isdb_frontend::input_sample(v * b))
            .collect();
        let (fq, fz) = rx(&x, end);
        let (gq, gz) = rx(&x, 0);
        println!(
            "seed {seed:#x}: model steady {sq:.2} / {sz:.2} dB, to the tail's end {tq:.2} / {tz:.2}; model span 16 steady {hq:.2} / {hz:.2}; f32 shaper (span 16) steady {fq:.2} / {fz:.2}, with its tail {gq:.2} / {gz:.2} (6-bit / zk)"
        );
    }
}

/// The study's interpolator ("mux", "floor½", P 512, span 12, Q0.24, 10 dB),
/// as `txinterp` builds and runs it.
fn study(n: usize) {
    let (p, span) = (512usize, 12usize);
    let width = 2 * span;
    let pulse = |x: f64| {
        if x.abs() < span as f64 {
            rrc(x, ALPHA)
        } else {
            0.0
        }
    };
    let energy = (0..p)
        .map(|r| {
            (0..width)
                .map(|t| pulse(r as f64 / p as f64 + t as f64 - span as f64).powi(2))
                .sum::<f64>()
        })
        .sum::<f64>()
        / p as f64;
    let g = energy.sqrt().recip();
    let cf: Vec<Vec<f64>> = (0..p)
        .map(|r| {
            (0..width)
                .map(|t| g * pulse((r as f64 + 0.5) / p as f64 + t as f64 - span as f64))
                .collect()
        })
        .collect();
    let d = 2048.0 * 10f64.powf(-0.5);
    let maxc = cf.iter().flatten().fold(0.0f64, |m, c| m.max(c.abs()));
    let s = ((f64::from((1 << 17) - 1) / (maxc * d)).log2().floor()) as u32;
    let k = d * f64::from(1u32 << s);
    let quant = |m: f64| -> Vec<Vec<i32>> {
        cf.iter()
            .map(|r| r.iter().map(|c| (c * k * m).round() as i32).collect())
            .collect()
    };
    let (a, b) = (quant(1.0), quant(FRAC_1_SQRT_2));
    let step = (f64::from(1u32 << 24) / (40e6 / RS)).round() as u32;
    let tp = TxParams::design(&TxDesign::new(40e6, RS, ALPHA)).unwrap();
    println!(
        "study: shift {s}, step {step:#x}; model: shift {}, step {:#x}; banks equal: a {}, b {}",
        tp.shift,
        tp.step,
        a == tp.a,
        b == tp.b
    );
    let c = codes(n, 0x9e37_79b9_7f4a_7c15);
    // the study's run: the symbol enters, then the samples of its
    // interval; index 8 is its zero symbol, k < 8 the point k
    let syms: Vec<u8> = std::iter::once(8).chain(c.iter().map(|&x| x - 8)).collect();
    let mut line = vec![8u8; width];
    let mut frac = 0u32;
    let mut out = Vec::new();
    for &kk in &syms {
        line.rotate_right(1);
        line[0] = kk;
        loop {
            let q = (frac >> 15) as usize;
            let (mut si, mut sq) = (0i32, 0i32);
            for (t, &k) in line.iter().enumerate() {
                let (aa, bb) = (a[q][t], b[q][t]);
                let (pi, pq) = match k {
                    0 => (aa, 0),
                    1 => (bb, bb),
                    2 => (0, aa),
                    3 => (-bb, bb),
                    4 => (-aa, 0),
                    5 => (-bb, -bb),
                    6 => (0, -aa),
                    7 => (bb, -bb),
                    _ => (0, 0),
                };
                si += pi;
                sq += pq;
            }
            let r = |v: i32| ((v + (1 << (s - 1))) >> s).clamp(-2048, 2047) as i16;
            out.push([r(si), r(sq)]);
            frac += step;
            if frac >= 1 << 24 {
                frac -= 1 << 24;
                break;
            }
        }
    }
    let mut m = Interpolator::new(tp).unwrap();
    m.push_codes(&c);
    let mut y = Vec::new();
    m.process(out.len(), &mut y);
    let differ = y.iter().zip(&out).filter(|(a, b)| a != b).count();
    println!(
        "{} samples of {n} symbols: {differ} differ from the study's with a leading zero symbol",
        out.len()
    );
}

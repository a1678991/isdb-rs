//! Throwaway investigation (FPGA transmit interpolator, not merged): a
//! polyphase RRC interpolator from 8PSK point indices to IQ at a
//! fractional rate, parameterised by phases P, span, phase selection
//! (nearest with P + 1 rows, floor with half-phase rows, linear between
//! rows), arithmetic (f64, 18-bit integer coefficients with the point
//! product as a mux or as a multiply, bank_scale coefficients plus a gain
//! multiplier) and the accumulator's fraction bits; the ISI MER through an
//! ideal f64 matched filter at the exact symbol instants (as
//! tests/shape.rs), and the digital loopback into `fast::FastFrontend`.
//!
//! cargo run --release -p isdb-frontend --example txinterp -- grid|arith|loop|rate|peak

// Throwaway study code: lints relaxed.
#![allow(clippy::all, clippy::pedantic, clippy::nursery)]

use isdb_frontend::fast::FastFrontend;
use isdb_frontend::metrics::psk_mer;
use isdb_frontend::params::{COEF_SUM_MAX, Design, Params};
use isdb_frontend::tables::rrc;
use isdb_iq::survey::KNOWN_SYSTEMS;
use num_complex::{Complex32, Complex64};
use std::f64::consts::{FRAC_1_SQRT_2, TAU};

const RS: f64 = 28.86e6;
const ALPHA: f64 = 0.35;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Sel {
    /// q = round(f·P), rows 0..=P at q/P (the f32 shaper).
    Round,
    /// q = floor(f·P), rows 0..P at (q + ½)/P (no row P needed).
    FloorHalf,
    /// q = floor(f·P), μ = next `bits` bits (0: exact), rows q and q + 1.
    Linear(u32),
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Arith {
    Float,
    /// 18-bit A = round(K·c), B = round(K·c/√2); the point product is a
    /// mux of {0, ±A, ±B}; K = 2^S·D, S the largest keeping |A| < 2^17.
    Mux,
    /// 18-bit A, points as integers ±2^14, ±round(2^14/√2); a multiply.
    Mul,
    /// RX-style bank_scale coefficients (row Σ|c| ≤ 2^17 − taps/2), mux,
    /// then a 16-bit gain g with a 20-bit shift (2 DSP).
    BankGain,
}

#[derive(Clone, Copy, Debug)]
struct Cfg {
    p: usize,
    span: usize,
    sel: Sel,
    arith: Arith,
    /// Fraction bits of the symbol-phase accumulator.
    frac_bits: u32,
    /// Output backoff, dB below the 12-bit full scale (RMS of |y|).
    backoff_db: f64,
    sps: f64,
}

/// Index k (0..8 = angle k·45°), 8 = zero symbol.
fn point(k: u8) -> Complex64 {
    if k >= 8 {
        return Complex64::new(0.0, 0.0);
    }
    Complex64::from_polar(1.0, f64::from(k) * TAU / 8.0)
}

fn round_shift(v: i64, s: u32) -> i64 {
    if s == 0 { v } else { (v + (1 << (s - 1))) >> s }
}

struct Tx {
    cfg: Cfg,
    width: usize,
    /// rows of float coefficients G·p (P + 1 rows; FloorHalf uses P).
    cf: Vec<Vec<f64>>,
    a: Vec<Vec<i64>>,
    b: Vec<Vec<i64>>,
    shift: u32,
    gain: i64,
    /// DAC units per unit RMS.
    d: f64,
    step: u64,
    unit: u64,
    pub max_row_abs: f64,
}

impl Tx {
    fn new(cfg: Cfg) -> Self {
        let span = cfg.span;
        let width = 2 * span;
        let p = cfg.p;
        let pulse = |x: f64| {
            if x.abs() < span as f64 {
                rrc(x, ALPHA)
            } else {
                0.0
            }
        };
        let pos = |r: usize| match cfg.sel {
            Sel::FloorHalf => (r as f64 + 0.5) / p as f64,
            _ => r as f64 / p as f64,
        };
        let raw: Vec<Vec<f64>> = (0..=p)
            .map(|r| {
                (0..width)
                    .map(|t| pulse(pos(r) + t as f64 - span as f64))
                    .collect()
            })
            .collect();
        // gain as Shaper: mean row energy over rows at r/P (r < P) is 1
        let energy = (0..p)
            .map(|r| {
                (0..width)
                    .map(|t| pulse(r as f64 / p as f64 + t as f64 - span as f64).powi(2))
                    .sum::<f64>()
            })
            .sum::<f64>()
            / p as f64;
        let g = energy.sqrt().recip();
        let cf: Vec<Vec<f64>> = raw
            .iter()
            .map(|r| r.iter().map(|c| c * g).collect())
            .collect();
        let max_row_abs = cf
            .iter()
            .map(|r| r.iter().map(|c| c.abs()).sum::<f64>())
            .fold(0.0, f64::max);
        let d = 2048.0 * 10f64.powf(-cfg.backoff_db / 20.0);
        let maxc = cf.iter().flatten().fold(0.0f64, |m, c| m.max(c.abs()));
        let lim = f64::from((1 << 17) - 1);
        let (mut a, mut b, mut shift, mut gain) = (vec![], vec![], 0u32, 0i64);
        match cfg.arith {
            Arith::Float => {}
            Arith::Mux | Arith::Mul => {
                let s = ((lim / (maxc * d)).log2().floor()) as u32;
                shift = s;
                let k = d * f64::from(1u32 << s);
                a = cf
                    .iter()
                    .map(|r| r.iter().map(|c| (c * k).round() as i64).collect())
                    .collect();
                b = cf
                    .iter()
                    .map(|r| {
                        r.iter()
                            .map(|c| (c * k * FRAC_1_SQRT_2).round() as i64)
                            .collect()
                    })
                    .collect();
            }
            Arith::BankGain => {
                let bs = (COEF_SUM_MAX as f64 - width as f64 / 2.0) / max_row_abs;
                a = cf
                    .iter()
                    .map(|r| r.iter().map(|c| (c * bs).round() as i64).collect())
                    .collect();
                b = cf
                    .iter()
                    .map(|r| {
                        r.iter()
                            .map(|c| (c * bs * FRAC_1_SQRT_2).round() as i64)
                            .collect()
                    })
                    .collect();
                shift = 20;
                gain = (d / bs * f64::from(1u32 << 20)).round() as i64;
                assert!(gain < 1 << 15, "gain {gain}");
            }
        }
        let unit = if cfg.frac_bits == 64 {
            u64::MAX
        } else {
            1u64 << cfg.frac_bits
        };
        let step = (unit as f64 / cfg.sps).round() as u64;
        Tx {
            cfg,
            width,
            cf,
            a,
            b,
            shift,
            gain,
            d,
            step,
            unit,
            max_row_abs,
        }
    }

    /// The realised samples per symbol.
    fn sps_real(&self) -> f64 {
        self.unit as f64 / self.step as f64
    }

    /// One row's dot product, in the arithmetic's raw units ([I, Q]).
    fn dot_int(&self, row: usize, line: &[u8]) -> [i64; 2] {
        let mut s = [0i64; 2];
        for (t, &k) in line.iter().enumerate() {
            if k >= 8 {
                continue;
            }
            let (a, b) = (self.a[row][t], self.b[row][t]);
            match self.cfg.arith {
                Arith::Mul => {
                    let one = 16384i64;
                    let c = (16384.0 * FRAC_1_SQRT_2).round() as i64;
                    let pts = [
                        (one, 0),
                        (c, c),
                        (0, one),
                        (-c, c),
                        (-one, 0),
                        (-c, -c),
                        (0, -one),
                        (c, -c),
                    ];
                    let (pi, pq) = pts[k as usize];
                    s[0] += a * pi;
                    s[1] += a * pq;
                }
                _ => {
                    // mux: even k → (±A, 0) / (0, ±A); odd k → (±B, ±B)
                    let (pi, pq) = match k {
                        0 => (a, 0),
                        1 => (b, b),
                        2 => (0, a),
                        3 => (-b, b),
                        4 => (-a, 0),
                        5 => (-b, -b),
                        6 => (0, -a),
                        _ => (b, -b),
                    };
                    s[0] += pi;
                    s[1] += pq;
                }
            }
        }
        s
    }

    fn finish_int(&self, s: [i64; 2], clips: &mut u64) -> [f64; 2] {
        let sh = if self.cfg.arith == Arith::Mul {
            self.shift + 14
        } else {
            self.shift
        };
        let mut out = [0.0; 2];
        for i in 0..2 {
            let v = match self.cfg.arith {
                Arith::BankGain => round_shift(s[i] * self.gain, sh),
                _ => round_shift(s[i], sh),
            };
            if !(-2048..=2047).contains(&v) {
                *clips += 1;
            }
            out[i] = v.clamp(-2048, 2047) as f64;
        }
        out
    }

    /// Shape indices `k` (then 2·span − 1 zero symbols); returns samples
    /// in DAC units (float mode: unit-RMS × d, unrounded) and clips.
    fn run(&self, syms: &[u8]) -> (Vec<[f64; 2]>, u64) {
        let w = self.width;
        let p = self.cfg.p as u64;
        let pbits = p.trailing_zeros();
        let fb = self.cfg.frac_bits;
        let mut line = vec![8u8; w];
        let mut frac: u64 = 0;
        let mut out = Vec::new();
        let mut clips = 0u64;
        let tail = vec![8u8; 2 * self.cfg.span - 1];
        for &k in syms.iter().chain(&tail) {
            line.rotate_right(1);
            line[0] = k;
            loop {
                let y = match self.cfg.sel {
                    Sel::Round => {
                        let q =
                            ((frac as u128 + (1u128 << (fb - pbits - 1))) >> (fb - pbits)) as usize;
                        self.row(q, &line, &mut clips)
                    }
                    Sel::FloorHalf => {
                        let q = (frac >> (fb - pbits)) as usize;
                        self.row(q, &line, &mut clips)
                    }
                    Sel::Linear(mb) => {
                        let q = (frac >> (fb - pbits)) as usize;
                        let rem = frac & ((1u64 << (fb - pbits)) - 1);
                        let mu_exact = rem as f64 / (1u64 << (fb - pbits)) as f64;
                        if self.cfg.arith == Arith::Float {
                            let y0 = self.dot_f(q, &line);
                            let y1 = self.dot_f(q + 1, &line);
                            let mu = if mb == 0 {
                                mu_exact
                            } else {
                                (rem >> (fb - pbits - mb)) as f64 / f64::from(1u32 << mb)
                            };
                            [y0[0] + mu * (y1[0] - y0[0]), y0[1] + mu * (y1[1] - y0[1])]
                        } else {
                            let s0 = self.dot_int(q, &line);
                            let s1 = self.dot_int(q + 1, &line);
                            let mb = if mb == 0 { 16 } else { mb };
                            let mu = (rem >> (fb - pbits - mb)) as i64;
                            let s = [
                                s0[0] + round_shift((s1[0] - s0[0]) * mu, mb),
                                s0[1] + round_shift((s1[1] - s0[1]) * mu, mb),
                            ];
                            self.finish_int(s, &mut clips)
                        }
                    }
                };
                out.push(y);
                frac += self.step;
                if frac >= self.unit {
                    frac -= self.unit;
                    break;
                }
            }
        }
        (out, clips)
    }

    fn dot_f(&self, row: usize, line: &[u8]) -> [f64; 2] {
        let mut s = Complex64::new(0.0, 0.0);
        for (t, &k) in line.iter().enumerate() {
            s += point(k) * self.cf[row][t];
        }
        [s.re * self.d, s.im * self.d]
    }

    fn row(&self, q: usize, line: &[u8], clips: &mut u64) -> [f64; 2] {
        if self.cfg.arith == Arith::Float {
            self.dot_f(q, line)
        } else {
            let s = self.dot_int(q, line);
            self.finish_int(s, clips)
        }
    }
}

fn xorshift(n: usize, seed: u64) -> Vec<u8> {
    let mut s = seed | 1;
    (0..n)
        .map(|_| {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            (s >> 61) as u8
        })
        .collect()
}

/// ISI MER through the ideal f64 matched filter at the exact instants.
fn isi_mer(tx: &Tx, syms: &[u8], y: &[[f64; 2]]) -> f64 {
    let sps = tx.sps_real();
    let span = tx.cfg.span;
    let pulse = |x: f64| {
        if x.abs() < span as f64 {
            rrc(x, ALPHA)
        } else {
            0.0
        }
    };
    let n = syms.len();
    let (mut num, mut den) = (Complex64::new(0.0, 0.0), 0.0);
    let mut z = Vec::new();
    for k in 2 * span..n - 2 * span {
        let u = (k + span) as f64;
        let i0 = ((u - span as f64) * sps).ceil() as usize;
        let i1 = (((u + span as f64) * sps).floor() as usize).min(y.len() - 1);
        let mut acc = Complex64::new(0.0, 0.0);
        for i in i0..=i1 {
            acc += Complex64::new(y[i][0], y[i][1]) * pulse(i as f64 / sps - u);
        }
        let a = point(syms[k]);
        z.push((acc / sps, a));
        num += acc / sps * a.conj();
        den += a.norm_sqr();
    }
    let c = num / den;
    let err: f64 = z.iter().map(|(z, a)| (z - c * a).norm_sqr()).sum();
    10.0 * (c.norm_sqr() * den / err).log10()
}

fn base(p: usize, span: usize, sel: Sel, arith: Arith) -> Cfg {
    Cfg {
        p,
        span,
        sel,
        arith,
        frac_bits: 63,
        backoff_db: 10.0,
        sps: 40e6 / RS,
    }
}

fn mer_of(cfg: Cfg, syms: &[u8]) -> (f64, u64) {
    let tx = Tx::new(cfg);
    let (y, clips) = tx.run(syms);
    (isi_mer(&tx, syms, &y), clips)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let what = args.get(1).map_or("grid", String::as_str);
    let n: usize = args.get(2).map_or(20_000, |s| s.parse().unwrap());
    let syms = xorshift(n, 0x9e37_79b9_7f4a_7c15);
    match what {
        "grid" => {
            println!(
                "ISI MER (dB), 1.386 sps, {n} random 8PSK, f64 arithmetic, exact 63-bit phase"
            );
            println!(
                "{:>5} {:>5} | {:>8} {:>8} {:>8} {:>8} {:>8}",
                "P", "span", "round", "floor½", "lin", "lin μ8", "lin μ4"
            );
            for span in [8, 12, 16] {
                for p in [16, 32, 64, 128, 256, 1024] {
                    let r: Vec<f64> = [
                        Sel::Round,
                        Sel::FloorHalf,
                        Sel::Linear(0),
                        Sel::Linear(8),
                        Sel::Linear(4),
                    ]
                    .iter()
                    .map(|&s| mer_of(base(p, span, s, Arith::Float), &syms).0)
                    .collect();
                    println!(
                        "{p:>5} {span:>5} | {:>8.2} {:>8.2} {:>8.2} {:>8.2} {:>8.2}",
                        r[0], r[1], r[2], r[3], r[4]
                    );
                }
            }
        }
        "arith" => {
            println!(
                "ISI MER (dB) with 18-bit coefficients and 12-bit output at 10 dB backoff, Q0.24 phase; {n} symbols"
            );
            println!(
                "{:>5} {:>5} {:>10} | {:>8} {:>8} {:>8} {:>8} | clips",
                "P", "span", "sel", "f64", "mux", "mul", "bankgain"
            );
            for span in [8, 12, 16] {
                for p in [64, 128, 256, 512, 1024] {
                    for sel in [Sel::FloorHalf, Sel::Linear(8)] {
                        let mut row = vec![];
                        let mut clips = 0;
                        for ar in [Arith::Float, Arith::Mux, Arith::Mul, Arith::BankGain] {
                            let mut c = base(p, span, sel, ar);
                            c.frac_bits = 24;
                            let (m, cl) = mer_of(c, &syms);
                            row.push(m);
                            clips += cl;
                        }
                        println!(
                            "{p:>5} {span:>5} {:>10} | {:>8.2} {:>8.2} {:>8.2} {:>8.2} | {clips}",
                            format!("{sel:?}"),
                            row[0],
                            row[1],
                            row[2],
                            row[3]
                        );
                    }
                }
            }
        }
        "frac" => {
            // the accumulator's fraction bits: realised rate and MER
            for fb in [16, 20, 24, 32, 63] {
                let mut c = base(1024, 16, Sel::Round, Arith::Float);
                c.frac_bits = fb;
                let tx = Tx::new(c);
                let ppm = (tx.sps_real() / c.sps - 1.0) * 1e6;
                let (y, _) = tx.run(&syms);
                println!(
                    "frac bits {fb}: step {} sps {:.9} rate error {ppm:+.4} ppm MER {:.2}",
                    tx.step,
                    tx.sps_real(),
                    isi_mer(&tx, &syms, &y)
                );
            }
        }
        "peak" => {
            for span in [8, 12, 16] {
                let mut c = base(64, span, Sel::FloorHalf, Arith::Mux);
                c.frac_bits = 24;
                let tx = Tx::new(c);
                let long = xorshift(200_000, 7);
                let (y, clips) = tx.run(&long);
                let pk = y
                    .iter()
                    .flat_map(|v| v.iter())
                    .fold(0.0f64, |m, v| m.max(v.abs()));
                let rms = (y.iter().map(|v| v[0] * v[0] + v[1] * v[1]).sum::<f64>()
                    / y.len() as f64)
                    .sqrt();
                println!(
                    "span {span}: shift S {}, max |A| {}, worst row Σ|G·p| {:.4} (×D = {:.1} DAC units of 2048), peak |component| {pk} over 200k symbols, RMS |y| {rms:.1} (target {:.1}), clips {clips}",
                    tx.shift,
                    tx.a.iter().flatten().map(|v| v.abs()).max().unwrap(),
                    tx.max_row_abs,
                    tx.max_row_abs * tx.d,
                    tx.d
                );
            }
        }
        "loop" => {
            // digital loopback: TX integers straight into the RX front end
            let fs = 40e6;
            let d = Design::for_system(&KNOWN_SYSTEMS[0], fs);
            let p = Params::design(&d).unwrap();
            let seed: u64 = args
                .get(3)
                .map_or(0x0123_4567_89ab_cdef, |s| s.parse().unwrap());
            let long = xorshift(200_000, seed);
            let run_fe = |x: &[[i16; 2]]| {
                let mut fe = FastFrontend::new(p.clone()).unwrap();
                let mut out = Vec::new();
                fe.process(x, &mut out);
                let half = out.len() / 2;
                let q: Vec<[f64; 2]> = out
                    .iter()
                    .map(|s| [f64::from(s.q[0]), f64::from(s.q[1])])
                    .collect();
                let z: Vec<[f64; 2]> = out
                    .iter()
                    .map(|s| [f64::from(s.zk[0]), f64::from(s.zk[1])])
                    .collect();
                (
                    psk_mer(&q, 8, 1e-3, half).mer_db,
                    psk_mer(&z, 8, 1e-3, half).mer_db,
                )
            };
            // f32 shaper reference at 10 dB backoff
            let mut sh = isdb_frontend::shape::Shaper::new(fs, RS, ALPHA).unwrap();
            let pts: Vec<Complex32> = long
                .iter()
                .map(|&k| {
                    let v = point(k);
                    Complex32::new(v.re as f32, v.im as f32)
                })
                .collect();
            let mut y = Vec::new();
            sh.push(&pts, &mut y);
            sh.finish(&mut y);
            let b = 10f32.powf(-0.5);
            let x: Vec<[i16; 2]> = y
                .iter()
                .map(|&v| isdb_frontend::input_sample(v * b))
                .collect();
            let (mq, mz) = run_fe(&x);
            println!(
                "f32 Shaper (P 1024, span 16), 12-bit at 10 dB backoff: 8PSK MER 6-bit q {mq:.2} dB, zk {mz:.2} dB"
            );
            for (pp, span, sel) in [
                (64, 16, Sel::FloorHalf),
                (128, 16, Sel::FloorHalf),
                (256, 16, Sel::FloorHalf),
                (1024, 16, Sel::FloorHalf),
                (64, 16, Sel::Linear(8)),
                (64, 12, Sel::Linear(8)),
                (64, 8, Sel::Linear(8)),
                (256, 12, Sel::FloorHalf),
                (256, 8, Sel::FloorHalf),
                (512, 12, Sel::FloorHalf),
                (512, 16, Sel::FloorHalf),
            ] {
                let mut c = base(pp, span, sel, Arith::Mux);
                c.frac_bits = 24;
                let tx = Tx::new(c);
                let (y, clips) = tx.run(&long);
                let x: Vec<[i16; 2]> = y.iter().map(|v| [v[0] as i16, v[1] as i16]).collect();
                let (mq, mz) = run_fe(&x);
                println!(
                    "TX mux P {pp} span {span} {sel:?}, Q0.24: 6-bit q {mq:.2} dB, zk {mz:.2} dB (clips {clips})"
                );
            }
        }
        "gap" => {
            // underrun: a gap of G symbols at 100 000 filled with zeros, the
            // last symbol held, or the 000 point; RX output MER in windows
            let fs = 40e6;
            let d = Design::for_system(&KNOWN_SYSTEMS[0], fs);
            let p = Params::design(&d).unwrap();
            let glen: usize = args.get(3).map_or(2000, |s| s.parse().unwrap());
            let base_syms = xorshift(200_000, 99);
            let mut c = base(256, 12, Sel::FloorHalf, Arith::Mux);
            c.frac_bits = 24;
            let tx = Tx::new(c);
            for mode in ["none", "zero", "hold", "point0"] {
                let mut s = base_syms.clone();
                let at = 100_000;
                for i in at..at + glen {
                    s[i] = match mode {
                        "none" => s[i],
                        "zero" => 8,
                        "hold" => base_syms[at - 1],
                        _ => 0,
                    };
                }
                let (y, _) = tx.run(&s);
                let x: Vec<[i16; 2]> = y.iter().map(|v| [v[0] as i16, v[1] as i16]).collect();
                let mut fe = FastFrontend::new(p.clone()).unwrap();
                let mut out = Vec::new();
                fe.process(&x, &mut out);
                let q: Vec<[f64; 2]> = out
                    .iter()
                    .map(|s| [f64::from(s.q[0]), f64::from(s.q[1])])
                    .collect();
                let win = |from: usize| {
                    let seg = &q[from..(from + 3000).min(q.len())];
                    psk_mer(seg, 8, 1e-3, 1000).mer_db
                };
                let e = at + glen;
                let sat = out[e..e + 3000]
                    .iter()
                    .filter(|s| s.q.iter().any(|&v| v == 31 || v == -32))
                    .count();
                println!(
                    "{mode:>6} gap {glen}: MER before {:.2} | after +0 {:.2}, +3k {:.2}, +10k {:.2}, +30k {:.2} dB; gain before {} at gap end {} +10k {}; period before {} at end {}; q at ±full scale in first 3k after: {sat}",
                    win(at - 6000),
                    win(e),
                    win(e + 3000),
                    win(e + 10_000),
                    win(e + 30_000),
                    out[at - 10].gain,
                    out[e].gain,
                    out[e + 10_000].gain,
                    out[at - 10].period,
                    out[e].period
                );
            }
        }
        "rate" => {
            for fs in [40e6, 57.72e6, 61.44e6] {
                for fb in [24u32, 32] {
                    let sps = fs / RS;
                    let unit = (1u64 << fb) as f64;
                    let step = (unit / sps).round();
                    let err = (step / (unit / sps) - 1.0) * 1e6;
                    println!(
                        "fs {:.2} MHz, Q0.{fb}: step {step} ({:#x}), symbol rate error {err:+.5} ppm",
                        fs / 1e6,
                        step as u64
                    );
                }
                let per = (1u64 << 24) as f64 * fs / RS;
                let rx = per.round();
                println!(
                    "   RX period Q8.24 nominal {rx} : {:+.5} ppm",
                    (rx / per - 1.0) * 1e6
                );
            }
        }
        _ => panic!("grid|arith|frac|peak|loop|rate"),
    }
}

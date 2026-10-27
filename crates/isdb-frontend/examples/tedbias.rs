//! Where the timing loop locks, on noiseless synthetic signals: the
//! fixed-point model's mean output instant against the true symbol
//! instants. A nonzero lock offset is a bias of the timing detector; for a
//! nearly rectangular spectrum (ISDB-S3's α = 0.03) a tenth of a symbol
//! costs several dB of SNR (examples/s3budget.rs).
//!
//! Signals: random QPSK, and an ISDB-S3-structured stream (STD-B44 Fig.
//! 3.3-1: per 9 296-symbol slot, 24 π/2-shift BPSK sync, 32 pilot, then
//! 66 × (136 main + 4 π/2-shift BPSK TMCC)) with 8PSK or 16APSK (γ 2.87)
//! main symbols.
//!
//! cargo run --release -p isdb-frontend --example tedbias -- [FS] [taps…]

// Narrowing casts are proven or fallible here (docs/plans/type-safety.md section 2).
#![warn(clippy::cast_possible_truncation)]
use isdb_frontend::fixed::Frontend;
use isdb_frontend::params::{Design, Params};
use isdb_frontend::tables::rrc;
use num_complex::Complex64 as C;
use std::f64::consts::{FRAC_PI_4, TAU};

#[derive(Clone, Copy)]
enum Kind {
    Qpsk,
    S3(&'static str),
}

struct Lcg(u32);
impl Lcg {
    fn next(&mut self) -> u32 {
        self.0 = self.0.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        self.0 >> 8
    }
}

fn symbols(kind: Kind, n: usize) -> Vec<C> {
    let mut r = Lcg(0x1357_9bdf);
    let pi2 = |k: usize, bit: u32| {
        let base = if bit == 0 {
            FRAC_PI_4
        } else {
            -3.0 * FRAC_PI_4
        };
        C::from_polar(1.0, base + if k % 2 == 1 { TAU / 4.0 } else { 0.0 })
    };
    (0..n)
        .map(|k| match kind {
            Kind::Qpsk => C::from_polar(1.0, FRAC_PI_4 + TAU / 4.0 * f64::from(r.next() & 3)),
            Kind::S3(main) => {
                let j = k % 9296;
                let in_data = j >= 56 && (j - 56) % 140 < 136;
                if !in_data {
                    // sync, pilot (taken as π/2-BPSK here) and TMCC
                    pi2(k, r.next() & 1)
                } else if main == "8psk" {
                    C::from_polar(1.0, TAU / 8.0 * f64::from(r.next() & 7))
                } else {
                    // 16APSK: 4 inner + 12 outer, γ = 2.87, unit power
                    let g = 2.87f64;
                    let r1 = (16.0 / (4.0 + 12.0 * g * g)).sqrt();
                    let v = r.next() & 15;
                    if v < 4 {
                        C::from_polar(r1, FRAC_PI_4 + TAU / 4.0 * f64::from(v))
                    } else {
                        C::from_polar(r1 * g, TAU / 24.0 + TAU / 12.0 * f64::from(v - 4))
                    }
                }
            }
        })
        .collect()
}

/// The pulse train at `sps` samples per symbol, symbol k at t0 + k·sps.
// f64 sample and symbol positions are truncated to indices on purpose
// (the symbol index is clamped to 0..syms.len()), and the components are
// rounded and clamped to the 12-bit range.
#[allow(clippy::cast_possible_truncation)]
fn train(syms: &[C], sps: f64, alpha: f64, t0: f64) -> Vec<[i16; 2]> {
    let n = (syms.len() as f64 * sps) as usize;
    let span = 240i64;
    (0..n)
        .map(|i| {
            let k = ((i as f64 - t0) / sps).round() as i64;
            let mut v = C::new(0.0, 0.0);
            for j in (k - span).max(0)..(k + span).min(syms.len() as i64) {
                v += syms[j as usize] * rrc((i as f64 - t0 - j as f64 * sps) / sps, alpha);
            }
            [
                (v.re * 500.0).round().clamp(-2048.0, 2047.0) as i16,
                (v.im * 500.0).round().clamp(-2048.0, 2047.0) as i16,
            ]
        })
        .collect()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let fs: f64 = args.get(1).map_or(61.44e6, |s| s.parse().unwrap());
    let taps: Vec<usize> = if args.len() > 2 {
        args[2..].iter().map(|s| s.parse().unwrap()).collect()
    } else {
        vec![32]
    };
    let cases: [(&str, f64, f64, f64, Kind); 4] = [
        ("ISDB-S QPSK", 28.86e6, 0.35, 2e-4, Kind::Qpsk),
        ("ISDB-S3 QPSK", 33.7561e6, 0.03, 5e-6, Kind::Qpsk),
        (
            "ISDB-S3 slots 8PSK",
            33.7561e6,
            0.03,
            5e-6,
            Kind::S3("8psk"),
        ),
        (
            "ISDB-S3 slots 16APSK",
            33.7561e6,
            0.03,
            5e-6,
            Kind::S3("16apsk"),
        ),
    ];
    for (name, rs, alpha, bw, kind) in cases {
        let sps = fs / rs;
        for &t in &taps {
            let mut d = Design::new(fs, rs, alpha);
            d.taps = t;
            d.loop_bandwidth = bw;
            let p = Params::design(&d).unwrap();
            let t0 = 3.3;
            let syms = symbols(kind, 80_000);
            let x = train(&syms, sps, alpha, t0);
            let mut fe = Frontend::new(p).unwrap();
            let mut out = Vec::new();
            fe.process(&x, &mut out);
            // Output instant: sample − taps/2 + phase/64.
            let offs: Vec<f64> = out[out.len() / 2..]
                .iter()
                .map(|s| {
                    let inst = s.sample as f64 - t as f64 / 2.0 + f64::from(s.phase) / 64.0;
                    let k = ((inst - t0) / sps).round();
                    (inst - t0 - k * sps) / sps
                })
                .collect();
            let mean = offs.iter().sum::<f64>() / offs.len() as f64;
            let rms =
                (offs.iter().map(|o| (o - mean).powi(2)).sum::<f64>() / offs.len() as f64).sqrt();
            println!(
                "{name:22} fs {:.2} MHz ({sps:.3} sps), {t:2} taps: lock offset {mean:+.4} T (rms {rms:.4} T)",
                fs / 1e6
            );
        }
    }
}

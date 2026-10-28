//! Write FPGA test vectors (JSON, `isdb_frontend::vectors`) for the HDL
//! simulation in `fpga/`.
//!
//! cargo run -p isdb-frontend --example vectors -- OUT.json [REC [s|s3] [TAPS] [SAMPLES] [BITS] [BLOCK_LOG2] [TRIM]]
//!
//! With a SigMF recording the input is its first samples (12-bit domain)
//! and the parameters are designed for the system at the recording's rate
//! and carrier offset; without one, a synthetic QPSK pulse train at 40 Msps.
//! TRIM (symbol periods, default 0) sets the output bank (format 2): a
//! nonzero trim makes `k` differ from `h`, so the vectors test that path.

use isdb_frontend::params::{Design, Params};
use isdb_frontend::tables::rrc;
use isdb_frontend::vectors::document;
use isdb_iq::Recording;
use isdb_iq::survey::{Options, survey};
use std::path::PathBuf;

/// `n` samples of a synthetic QPSK pulse train at `sps` samples per
/// symbol (a fixed LCG symbol sequence), 500 per unit.
// f64 symbol positions are truncated to indices on purpose, and the
// components (|a| ≤ a few units × 500) fit i16: f64 -> i16 saturates.
#[allow(clippy::cast_possible_truncation)]
fn pulse_train(n: usize, sps: f64, alpha: f64) -> Vec<[i16; 2]> {
    let mut state = 1u32;
    let syms: Vec<(f64, f64)> = (0..(n as f64 / sps) as usize + 64)
        .map(|_| {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (
                if state & 0x8000_0000 != 0 { 1.0 } else { -1.0 },
                if state & 0x4000_0000 != 0 { 1.0 } else { -1.0 },
            )
        })
        .collect();
    (0..n)
        .map(|i| {
            let k0 = ((i as f64 / sps) as usize).saturating_sub(16);
            let (a, b) = (k0..k0 + 32).fold((0.0, 0.0), |(a, b), k| {
                let w = rrc((i as f64 - k as f64 * sps) / sps, alpha);
                (a + syms[k].0 * w, b + syms[k].1 * w)
            });
            [(a * 500.0).round() as i16, (b * 500.0).round() as i16]
        })
        .collect()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let out = PathBuf::from(args.get(1).expect("output path"));
    let arg = |i: usize, d: &str| args.get(i).cloned().unwrap_or_else(|| d.to_string());
    let system = arg(3, "s");
    let taps: usize = arg(4, "32").parse().unwrap();
    let n: usize = arg(5, "6000").parse().unwrap();
    let bits: u8 = arg(6, "6").parse().unwrap();
    let block_log2: u8 = arg(7, "8").parse().unwrap();
    let trim: f64 = arg(8, "0").parse().unwrap();
    let (rs, alpha) = if system == "s3" {
        (33.7561e6, 0.03)
    } else {
        (28.86e6, 0.35)
    };
    let (fs, cfo, input) = match args.get(2) {
        Some(path) if path != "-" => {
            let mut rec = Recording::open(&PathBuf::from(path), None, None).unwrap();
            let fs = rec.sample_rate;
            let x = rec.read(0, (1 << 20).max(n)).unwrap();
            let sv = survey(&x, fs, &Options::default());
            let cfo = sv.best_carrier().map_or(0.0, |c| c.offset_hz);
            let input: Vec<[i16; 2]> = x[..n]
                .iter()
                .map(|&v| isdb_frontend::input_sample(v))
                .collect();
            (fs, cfo, input)
        }
        _ => {
            let fs = 40e6;
            (fs, 30e3, pulse_train(n, fs / rs, alpha))
        }
    };
    let mut d = Design::new(fs, rs, alpha);
    d.carrier_offset = cfo;
    d.taps = taps;
    d.bits = bits;
    d.block_log2 = block_log2;
    d.trim = trim;
    // Wider loops than for reception, so the test exercises them (ISDB-S3's
    // small timing-detector slope limits its integral gain register).
    d.loop_bandwidth = if alpha < 0.1 { 2e-4 } else { 2e-3 };
    let p = Params::design(&d).unwrap();
    let doc = document(&p, &input).unwrap();
    std::fs::write(&out, serde_json::to_vec(&doc).unwrap()).unwrap();
    eprintln!(
        "{}: {} samples, {} symbols, {} words",
        out.display(),
        input.len(),
        doc["symbols"].as_array().unwrap().len(),
        doc["words"].as_array().unwrap().len()
    );
}

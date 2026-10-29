//! Write the transmit interpolator's FPGA test vectors (JSON,
//! `isdb_frontend::vectors::tx_document`) for the HDL simulation in
//! `fpga/`.
//!
//! cargo run --release -p isdb-frontend --example txvectors -- OUT_DIR
//!
//! Writes, in OUT_DIR (ISDB-S, 28.86 Mbaud, α 0.35, P 512, span 12):
//!
//! | File | Content |
//! |---|---|
//! | `tx-1-continuous.json` | 40 Msps, 10 dB (the bitstream's default bank): 2 000 random point codes, then 3 zero words (the line drains), no stall |
//! | `tx-2-stalls.json` | 40 Msps, 10 dB: 2 000 codes of every kind (points, zero, reserved 1..7), a stall of 1–4 requests before about one word in ten |
//! | `tx-3-long-stall.json` | 40 Msps, 10 dB: 2 000 point codes, one stall of 64 requests (512 symbols) before word 100 |
//! | `tx-4-bank.json` | 61.44 Msps, 3 dB: another step, shift (6) and bank, which the simulation must load through the port (the `gpio` words); the output clips |
//!
//! The vectors with real ISDB-S frames come from `isdbs-phy`'s
//! `examples/txvectors.rs` (`tx-5-isdbs.json`).

use isdb_frontend::shape::fixed::TxDesign;
use isdb_frontend::txformat;
use isdb_frontend::vectors::tx_document;
use std::path::PathBuf;

/// One vectors file: name, design, words, stalls `(word_index, n)`.
type Set<'a> = (&'a str, &'a TxDesign, &'a [u32], Vec<(u64, u32)>);

/// xorshift64.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    /// 0..n (n ≤ 256).
    fn below(&mut self, n: u64) -> u8 {
        self.next().to_le_bytes()[0] % u8::try_from(n).unwrap()
    }
}

/// `n` random point codes.
fn points(n: usize, r: &mut Rng) -> Vec<u8> {
    (0..n).map(|_| txformat::point_code(r.below(8))).collect()
}

/// `n` codes of every kind: 1 in 16 the zero code, 1 in 16 a reserved
/// code, the rest points.
fn mixed(n: usize, r: &mut Rng) -> Vec<u8> {
    (0..n)
        .map(|_| match r.below(16) {
            0 => txformat::ZERO,
            1 => 1 + r.below(7),
            _ => txformat::point_code(r.below(8)),
        })
        .collect()
}

/// The words of `codes` and the zero words that drain the line (3 for 24
/// taps).
fn words(codes: &[u8]) -> Vec<u32> {
    let mut w = txformat::pack(codes);
    w.extend([0; 3]);
    w
}

fn main() {
    let dir = PathBuf::from(std::env::args().nth(1).expect("output directory"));
    std::fs::create_dir_all(&dir).unwrap();
    let isdbs = TxDesign::new(40e6, 28.86e6, 0.35);
    let mut r = Rng(0x5eed_1234_abcd_0001);
    let continuous = words(&points(2000, &mut r));
    let stalled = words(&mixed(2000, &mut r));
    let mut stalls: Vec<(u64, u32)> = Vec::new();
    for i in 0..stalled.len() as u64 {
        if r.below(10) == 0 {
            stalls.push((i, 1 + u32::from(r.below(4))));
        }
    }
    let long = words(&points(2000, &mut r));
    let bank = TxDesign {
        backoff_db: 3.0,
        ..TxDesign::new(61.44e6, 28.86e6, 0.35)
    };
    let bank_words = words(&points(2000, &mut r));
    let sets: [Set; 4] = [
        ("tx-1-continuous.json", &isdbs, &continuous, vec![]),
        ("tx-2-stalls.json", &isdbs, &stalled, stalls),
        ("tx-3-long-stall.json", &isdbs, &long, vec![(100, 64)]),
        ("tx-4-bank.json", &bank, &bank_words, vec![]),
    ];
    for (name, design, w, st) in sets {
        let doc = tx_document(design, w, &st).unwrap();
        let path = dir.join(name);
        let bytes = serde_json::to_vec(&doc).unwrap();
        std::fs::write(&path, &bytes).unwrap();
        let c = &doc["counters"];
        let clipped = doc["samples"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|s| {
                ["out_i", "out_q"]
                    .iter()
                    .any(|k| matches!(s[k].as_i64(), Some(-2048 | 2047)))
            })
            .count();
        eprintln!(
            "{}: {} bytes; step {:#x} shift {}, {} words, {} stalls, {} samples ({clipped} clipped), consumed {} missed {} first_miss {}, {} gpio words",
            path.display(),
            bytes.len(),
            doc["params"]["step"].as_u64().unwrap(),
            doc["params"]["shift"],
            w.len(),
            st.len(),
            c["samples"],
            c["consumed"],
            c["missed"],
            c["first_miss"],
            doc["gpio"].as_array().unwrap().len()
        );
    }
}

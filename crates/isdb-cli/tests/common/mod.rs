//! Helpers shared by the `isdb` binary's integration tests (`cli.rs`,
//! `live_feed.rs`).
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

pub const ISDB: &str = env!("CARGO_BIN_EXE_isdb");
/// Mode 3, GI 1/8: 8192·(1 + 1/8)·204 samples per OFDM frame.
pub const SAMPLES_PER_FRAME: u64 = 1_880_064;
pub const FRAME_S: f64 = SAMPLES_PER_FRAME as f64 * 63.0 / 512e6;

pub fn tmp(name: &str) -> PathBuf {
    let d = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("isdb-cli-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Distinct TS packet `n` of `pid` with a running continuity counter.
pub fn packet(pid: u16, n: u32) -> [u8; 188] {
    let mut p = [0u8; 188];
    p[0] = 0x47;
    p[1..3].copy_from_slice(&pid.to_be_bytes());
    p[3] = 0x10 | (n & 15) as u8;
    p[4..8].copy_from_slice(&n.to_be_bytes());
    let mut x = (u64::from(n) + 1).wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ u64::from(pid);
    for b in &mut p[8..] {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        *b = x.to_le_bytes()[4]; // bits 39..32
    }
    p
}

pub fn packets(pid: u16, range: std::ops::Range<u32>) -> Vec<u8> {
    range.flat_map(|n| packet(pid, n)).collect()
}

pub fn run(args: &[&str]) -> Output {
    let o = Command::new(ISDB).args(args).output().expect("run isdb");
    eprintln!(
        "$ isdb {}\n{}",
        args.join(" "),
        String::from_utf8_lossy(&o.stderr)
    );
    o
}

pub fn s(p: &Path) -> &str {
    p.to_str().unwrap()
}

pub fn assert_same(got: &Path, want: &[u8], what: &str) {
    let g = std::fs::read(got).unwrap();
    assert_eq!(
        g.len(),
        want.len(),
        "{what}: {} vs {} bytes",
        g.len(),
        want.len()
    );
    let first = g
        .chunks(188)
        .zip(want.chunks(188))
        .position(|(a, b)| a != b);
    assert!(first.is_none(), "{what}: first differing packet {first:?}");
}

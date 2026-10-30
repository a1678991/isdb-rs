#![allow(missing_docs)]
//! The terrestrial SI bytes are pinned: the NIT, SDT, BIT and EIT p/f
//! sections of the shipped `bench-minimal.toml` and `bench.toml` (media
//! streams turned into `declared` ones, fixed clock) must stay
//! byte-identical to `tests/fixtures/terrestrial_si.txt`. The fixture was
//! recorded from this crate's own output before the `[network] kind`
//! branch was added (docs/plans/isdbs-next/bs-si.md §6.4 test 3, §6.5), so
//! any change to the terrestrial NIT/SDT/BIT/EIT shows up here.

mod common;

use std::fmt::Write as _;

use common::*;
use isdb_ts::Pid;
use isdbt_mux::analysis::analyze;
use isdbt_mux::mux::clocks_to_ticks;
use isdbt_mux::{Config, Multiplexer};

/// `text` with its media streams turned into `declared` ones (PSI/SI/PCR
/// only, as isdb-cli's tests and `tools/isdbs-bench-tx.sh` do) and the
/// clock fixed.
fn declared_fixed(text: &str) -> String {
    let text = text.replace(
        "source = \"system\"",
        "start = \"2026-10-08T12:00:00\"\nsource = \"fixed\"",
    );
    let out: Vec<String> = text
        .lines()
        .filter(|l| {
            let t = l.trim_start();
            !t.starts_with("source = \"media/") && !t.starts_with("frames_per_pes")
        })
        .map(|l| {
            l.replace(
                "kind = \"mpeg2-video\"",
                "kind = \"declared\"\n  stream_type = 0x02",
            )
            .replace(
                "kind = \"aac-adts\"",
                "kind = \"declared\"\n  stream_type = 0x0F",
            )
        })
        .collect();
    out.join("\n")
}

/// The distinct sections of the NIT, SDT, EIT and BIT PIDs (B10 Table 5-1)
/// in the order they were first sent, as `<label> <table> <hex>` lines.
fn si_lines(label: &str, text: &str, frames: u64) -> String {
    let cfg = Config::from_toml(&declared_fixed(text)).unwrap();
    let dir = tmpdir(&format!("si_fixture_{label}"));
    let mut mux = Multiplexer::new(&cfg, &bench_params(), &dir).unwrap();
    let n = mux.pattern().len();
    let (mut full, mut times) = (Vec::new(), Vec::new());
    for k in 0..frames {
        let (f, _) = mux.next_frame_full();
        times.extend((0..n).map(|j| mux.slot_time(k, j)));
        full.extend(f);
    }
    let r = analyze(&full, &times, clocks_to_ticks(408));
    let mut s = String::new();
    for (pid, name) in [
        (0x0010, "NIT"),
        (0x0011, "SDT"),
        (0x0012, "EIT"),
        (0x0024, "BIT"),
    ] {
        let mut seen: Vec<&[u8]> = Vec::new();
        for (_, sec) in r.sections.get(&Pid::new(pid).unwrap()).unwrap() {
            if !seen.contains(&sec.as_slice()) {
                seen.push(sec);
            }
        }
        assert!(!seen.is_empty(), "{label}: no {name}");
        for sec in seen {
            let hex: String = sec.iter().fold(String::new(), |mut h, b| {
                let _ = write!(h, "{b:02x}");
                h
            });
            let _ = writeln!(s, "{label} {name} {hex}");
        }
    }
    s
}

/// 90 frames ≈ 20.8 s: the BIT (20 s), NIT (10 s) and SDT/EIT (2 s) are
/// all sent; the events of the examples do not change in that time.
const FRAMES: u64 = 90;

#[test]
fn terrestrial_si_bytes_are_unchanged() {
    let mut got = si_lines(
        "bench-minimal",
        include_str!("../examples/bench-minimal.toml"),
        FRAMES,
    );
    got.push_str(&si_lines(
        "bench",
        include_str!("../examples/bench.toml"),
        FRAMES,
    ));
    let want = include_str!("fixtures/terrestrial_si.txt");
    if got != want {
        let p = tmpdir("si_fixture").join("terrestrial_si.txt");
        std::fs::write(&p, &got).unwrap();
        panic!(
            "terrestrial SI differs from tests/fixtures/terrestrial_si.txt; \
             the sections sent now are in {}",
            p.display()
        );
    }
}

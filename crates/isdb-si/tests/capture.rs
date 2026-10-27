//! Real broadcast data (opt-in): every distinct SI section of a terrestrial
//! capture named by the environment variable
//! `ISDB_RS_CAPTURE_TS`.  Without the variable, or when the file is
//! missing, the test prints a `SKIP` line and passes. SI sections are
//! never scrambled, so only the SI PIDs are read and the payload of the
//! elementary streams does not matter.
//!
//! ```text
//! ISDB_RS_CAPTURE_TS=/path/to/capture.ts cargo test -p isdb-si --test capture -- --nocapture
//! ```
//!
//! Sections are reassembled by `isdb-ts` (H.222.0 §2.4.4) on the SI PIDs of
//! B10 Part 2 Table 5-1 whose tables this crate types: NIT 0x0010, SDT/BAT
//! 0x0011, EIT 0x0012/0x0026/0x0027, TDT/TOT 0x0014, BIT 0x0024.
//!
//! Asserts: CRC_32 valid, typed decode (no `Unknown` table; no `Unknown`
//! descriptor except the tags B10 delegates to other standards, 0xC9 /
//! 0xCB / 0xCC), byte-identical re-encode.  Prints per-table and per-tag
//! counts and the conformance deviations (not asserted; live multiplexes
//! carry some, see SPEC-NOTES).

// Narrowing casts are proven or fallible here (docs/plans/type-safety.md section 2).
#![warn(clippy::cast_possible_truncation)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use isdb_si::AnyTable;
use isdb_si::conformance;
use isdb_si::descriptor::Descriptor;
use isdb_ts::Pid;
use isdb_ts::crc::crc32;
use isdb_ts::demux::{Assembled, SectionDemux};
use isdb_ts::reader::SliceReader;

/// Environment variable naming the capture file.
const CAPTURE_ENV: &str = "ISDB_RS_CAPTURE_TS";

/// SI PIDs whose tables are typed here (B10 Part 2 Table 5-1).
const SI_PIDS: [u16; 7] = [0x0010, 0x0011, 0x0012, 0x0014, 0x0024, 0x0026, 0x0027];

/// Descriptor tags B10 delegates to other standards (no typed decoder).
const EXTERNAL_TAGS: [u8; 3] = [0xC9, 0xCB, 0xCC];

fn capture() -> Option<Vec<u8>> {
    let Some(p) = std::env::var_os(CAPTURE_ENV).map(PathBuf::from) else {
        eprintln!("SKIP: {CAPTURE_ENV} not set (path of an ISDB-T capture)");
        return None;
    };
    match std::fs::read(&p) {
        Ok(d) => Some(d),
        Err(e) => {
            eprintln!("SKIP: {CAPTURE_ENV}={}: {e}", p.display());
            None
        }
    }
}

#[test]
fn capture_si_sections_crc_decode_reencode() {
    let Some(data) = capture() else { return };
    let mut demux = SectionDemux::new();
    for pid in SI_PIDS {
        demux.add_pid(Pid::new(pid).unwrap());
    }
    let mut seen: BTreeSet<(Pid, Vec<u8>)> = BTreeSet::new();
    for (i, p) in SliceReader::new(&data).enumerate() {
        for o in demux.push(i as u64, p.packet) {
            if let Assembled::Section(s) = o {
                seen.insert((s.pid, s.data));
            }
        }
    }
    assert!(!seen.is_empty(), "no SI section in the capture");
    let mut per_table: BTreeMap<String, usize> = BTreeMap::new();
    let mut per_tag: BTreeMap<u8, usize> = BTreeMap::new();
    let mut deviations: BTreeMap<String, usize> = BTreeMap::new();
    for (pid, sec) in &seen {
        let tid = sec[0];
        if sec[1] & 0x80 != 0 || tid == 0x73 {
            assert_eq!(crc32(sec), 0, "CRC_32: pid {pid:#06x} tid {tid:#04x}");
        }
        let t = AnyTable::decode(*pid, sec)
            .unwrap_or_else(|e| panic!("pid {pid:#06x} tid {tid:#04x}: {e}"));
        assert!(
            !matches!(t, AnyTable::Unknown(_)),
            "pid {pid:#06x} tid {tid:#04x} not typed"
        );
        for (_, d) in t.descriptors() {
            *per_tag.entry(d.tag()).or_default() += 1;
            assert!(
                !matches!(d, Descriptor::Unknown { .. }) || EXTERNAL_TAGS.contains(&d.tag()),
                "untyped descriptor tag {:#04x} in {} pid {pid:#06x}",
                d.tag(),
                t.name()
            );
        }
        assert_eq!(
            &t.encode().unwrap(),
            sec,
            "re-encode differs: pid {pid:#06x} tid {tid:#04x}"
        );
        *per_table
            .entry(format!("{} {tid:#04x} @ {pid:#06x}", t.name()))
            .or_default() += 1;
        for d in conformance::check(*pid, &t) {
            *deviations
                .entry(format!("[{:?}] {}: {}", d.kind, d.clause, d.message))
                .or_default() += 1;
        }
    }
    println!("== {} distinct SI sections ==", seen.len());
    for (k, n) in &per_table {
        println!("  {k}: {n}");
    }
    println!("== descriptor tags ==");
    for (k, n) in &per_tag {
        println!("  {k:#04x}: {n}");
    }
    println!("== conformance deviations (reported, not asserted) ==");
    for (k, n) in &deviations {
        println!("  {n:4}x {k}");
    }
}

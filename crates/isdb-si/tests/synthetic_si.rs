//! Synthetic multiplex: every SI section of `tests/synthetic` (a fictional
//! terrestrial multiplex built with this crate's typed encoders; NIT, SDT,
//! BIT, EIT p/f + schedule basic/extended, L-EIT, TOT, TDT).
//!
//! Asserts: CRC_32 valid (H.222.0 Annex A / B10 Part 2 Annex B), typed
//! decode (no `Unknown` table, no `Unknown` descriptor), byte-identical
//! re-encode, every table_id and descriptor tag of the set present, and no
//! conformance deviation other than the short event descriptor missing
//! from event-group-only and extended-only events.  Prints a human-readable summary (run with
//! `--nocapture`).
//!
//! Real broadcast sections are exercised by the capture tests of
//! `isdb-ts` and `isdb-analyze` when `ISDB_RS_CAPTURE_TS` is set.

mod synthetic;

use isdb_si::AnyTable;
use isdb_si::conformance::{self, Kind};
use isdb_si::descriptor::Descriptor;
use isdb_ts::crc::crc32;
use std::collections::BTreeMap;

fn load() -> Vec<(u16, u8, Vec<u8>)> {
    synthetic::sections()
}

/// ARIB string decoded by `isdb-text` (B24 8-unit code, B10 Annex A).
fn show(b: &[u8]) -> String {
    isdb_text::AribString(b.to_vec()).to_string_lossy()
}

#[test]
fn synthetic_sections_crc_decode_reencode() {
    let secs = load();
    assert!(
        secs.len() >= 30,
        "synthetic set has {} sections",
        secs.len()
    );
    let mut per_table: BTreeMap<String, usize> = BTreeMap::new();
    let mut per_tag: BTreeMap<u8, usize> = BTreeMap::new();
    for (pid, tid, sec) in &secs {
        assert_eq!(sec[0], *tid);
        // CRC_32: every long section, and the TOT (B10 Part 2 Table 5-9).
        if sec[1] & 0x80 != 0 || *tid == 0x73 {
            assert_eq!(
                crc32(sec),
                0,
                "CRC_32 residue non-zero: pid {pid:#06x} tid {tid:#04x}"
            );
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
                !matches!(d, Descriptor::Unknown { .. }),
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
    }
    println!("== {} distinct sections ==", secs.len());
    for (k, n) in &per_table {
        println!("  {k}: {n}");
    }
    println!("== descriptor tags ==");
    for (k, n) in &per_tag {
        println!("  {k:#04x}: {n}");
    }
    // The syntax the set is meant to cover (table_id and descriptor tag).
    let tids: std::collections::BTreeSet<u8> = secs.iter().map(|s| s.1).collect();
    for tid in [0x40, 0x42, 0x4E, 0x50, 0x51, 0x58, 0x59, 0x70, 0x73, 0xC4] {
        assert!(tids.contains(&tid), "no table_id {tid:#04x}");
    }
    for tag in [
        0x40, 0x41, 0x48, 0x4D, 0x4E, 0x50, 0x54, 0x58, 0xC1, 0xC4, 0xC7, 0xCD, 0xCE, 0xCF, 0xD6,
        0xD7, 0xD8, 0xFA, 0xFB, 0xFC, 0xFE,
    ] {
        assert!(per_tag.contains_key(&tag), "no descriptor tag {tag:#04x}");
    }
}

#[test]
fn synthetic_summary_and_conformance() {
    let secs = load();
    for (pid, _, sec) in &secs {
        match AnyTable::decode(*pid, sec).unwrap() {
            AnyTable::Nit(n) => {
                println!(
                    "NIT tid={:#04x} network_id={:#06x} v{}",
                    n.header.table_id, n.header.table_id_extension, n.header.version_number
                );
                for d in &n.body.0.descriptors {
                    match d {
                        Descriptor::NetworkName(x) => {
                            println!("  network_name \"{}\"", show(&x.name.0))
                        }
                        _ => println!("  network: {d:?}"),
                    }
                }
                for ts in &n.body.0.transport_streams {
                    println!(
                        "  TS {:#06x} onid {:#06x}",
                        ts.transport_stream_id, ts.original_network_id
                    );
                    for d in &ts.descriptors {
                        match d {
                            Descriptor::TsInformation(x) => println!(
                                "    TS_information: remote_key={} name=\"{}\" types={:?}",
                                x.remote_control_key_id,
                                show(&x.ts_name.0),
                                x.transmission_types
                            ),
                            Descriptor::TerrestrialDeliverySystem(x) => println!(
                                "    terrestrial_delivery: area={:#05x} GI={} mode={} freqs={:?} MHz",
                                x.area_code,
                                x.guard_interval,
                                x.transmission_mode,
                                x.frequencies
                                    .iter()
                                    .map(|&f| f as f64 / 7.0)
                                    .collect::<Vec<_>>()
                            ),
                            _ => println!("    {d:?}"),
                        }
                    }
                }
            }
            AnyTable::Sdt(s) => {
                println!(
                    "SDT tid={:#04x} tsid={:#06x} onid={:#06x}",
                    s.header.table_id, s.header.table_id_extension, s.body.original_network_id
                );
                for sv in &s.body.services {
                    for d in &sv.descriptors {
                        if let Descriptor::Service(x) = d {
                            println!(
                                "  service {:#06x} type {:#04x} provider \"{}\" name \"{}\" (EIT sched {} p/f {}, running {})",
                                sv.service_id,
                                x.service_type,
                                show(&x.provider_name.0),
                                show(&x.service_name.0),
                                sv.eit_schedule_flag,
                                sv.eit_present_following_flag,
                                sv.running_status
                            );
                        }
                    }
                }
            }
            AnyTable::Bit(b) => {
                println!(
                    "BIT onid={:#06x} view_propriety={}",
                    b.header.table_id_extension, b.body.broadcast_view_propriety
                );
                for d in &b.body.first_descriptors {
                    println!("  first: {d:?}");
                }
                for bc in &b.body.broadcasters {
                    println!("  broadcaster_id {:#04x}", bc.broadcaster_id);
                    for d in &bc.descriptors {
                        match d {
                            Descriptor::BroadcasterName(x) => {
                                println!("    broadcaster_name \"{}\"", show(&x.name.0))
                            }
                            _ => println!("    {d:?}"),
                        }
                    }
                }
            }
            AnyTable::Tot(t) => println!(
                "TOT JST {} (UTC {}) descriptors {}",
                t.body
                    .jst_time
                    .to_jst()
                    .map(|d| d.to_string())
                    .unwrap_or_default(),
                t.body
                    .jst_time
                    .to_utc()
                    .map(|d| d.to_string())
                    .unwrap_or_default(),
                t.body.descriptors.len()
            ),
            AnyTable::Tdt(t) => println!("TDT JST {:?}", t.body.jst_time.to_jst()),
            AnyTable::Eit(e) if e.header.table_id == 0x4E && *pid == 0x0012 => {
                for ev in &e.body.events {
                    for d in &ev.descriptors {
                        if let Descriptor::ShortEvent(x) = d {
                            println!(
                                "EIT p/f sid={:#06x} sec={} event {:#06x} {} +{:?}s \"{}\"",
                                e.header.table_id_extension,
                                e.header.section_number,
                                ev.event_id,
                                ev.start_time
                                    .to_jst()
                                    .map(|d| d.to_string())
                                    .unwrap_or_default(),
                                ev.duration.seconds(),
                                show(&x.event_name.0)
                            );
                        }
                    }
                }
            }
            _ => {}
        }
    }

    // Conformance: the synthetic set is built to the B10 values.
    let mut by: BTreeMap<(Kind, &'static str, String), (usize, String)> = BTreeMap::new();
    let mut total = 0;
    for (pid, _, sec) in &secs {
        let t = AnyTable::decode(*pid, sec).unwrap();
        for d in conformance::check(*pid, &t) {
            total += 1;
            let key = (d.kind, d.clause, d.message.clone());
            let e = by.entry(key).or_insert((0, d.location.clone()));
            e.0 += 1;
        }
    }
    println!(
        "== conformance: {total} deviations, {} distinct ==",
        by.len()
    );
    for ((kind, clause, msg), (n, first)) in &by {
        println!("  {n:4}x [{kind:?}] {clause}: {msg}   (first at {first})");
    }
    // The only deviations are the ones live multiplexes show as well:
    // events carrying only an event group descriptor (a common event on a
    // sub-channel) or only extended event descriptors (EIT schedule
    // extended, 0x58–0x5F) lack the short event descriptor Table 6-1 marks
    // mandatory in the EIT.
    assert!(total > 0);
    for (kind, _, msg) in by.keys() {
        assert!(
            *kind == Kind::MandatoryDescriptor && msg.contains("short_event_descriptor"),
            "unexpected deviation: [{kind:?}] {msg}"
        );
    }
}

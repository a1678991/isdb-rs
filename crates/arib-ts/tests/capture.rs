//! Real-data check (workspace README "How it is verified", item 4) over a
//! descrambled capture of a live ISDB-T broadcast (null packets may be
//! stripped by the tuner software).  No capture is committed: the test reads
//! the file named by the environment variable `ISDB_RS_CAPTURE_TS` and
//! prints a `SKIP` line and passes when the variable is unset or the file
//! does not exist.
//!
//! ```text
//! ISDB_RS_CAPTURE_TS=/path/to/capture.ts cargo test -p arib-ts --test capture -- --nocapture
//! ```
//!
//! Asserts: every packet decodes and re-encodes byte-identically; every long
//! section on PIDs 0x0000, 0x0001, 0x0010–0x0014, 0x0024 and the PMT PIDs has
//! a valid CRC_32; PAT/CAT/PMT decode and re-encode byte-identically; every
//! PES packet on the PMT's elementary PIDs decodes and re-encodes
//! byte-identically.  Spec-rule violations are printed, not asserted.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use arib_ts::demux::{Assembled, SectionDemux};
use arib_ts::packet::Packet;
use arib_ts::pes::{PesAssembler, PesOutput, PesPacket};
use arib_ts::psi::{Cat, Pat, Pmt, check_cat, check_pat, check_pmt};
use arib_ts::reader::SliceReader;
use arib_ts::section::{Long, split_long_unchecked};
use arib_ts::stats::Stats;

/// Environment variable naming the capture file.
const CAPTURE_ENV: &str = "ISDB_RS_CAPTURE_TS";

/// The capture named by `ISDB_RS_CAPTURE_TS`, or `None` (with a `SKIP`
/// line on stderr) when the variable is unset or the file is missing.
fn capture() -> Option<Vec<u8>> {
    let Some(p) = std::env::var_os(CAPTURE_ENV).map(PathBuf::from) else {
        eprintln!("SKIP: {CAPTURE_ENV} not set (path of a descrambled ISDB-T capture)");
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
fn capture_round_trips() {
    let Some(data) = capture() else { return };
    let mut reader = SliceReader::new(&data);
    let packets: Vec<[u8; 188]> = reader.by_ref().map(|p| *p.packet).collect();
    let rs = reader.sync.stats;
    println!("packets: {} format {:?} reader {:?}", packets.len(), reader.sync.format(), rs);
    assert_eq!(rs.sync_losses, 0);
    assert_eq!(rs.skipped_bytes, 0);
    assert_eq!(packets.len() * 188 + rs.trailing_bytes as usize, data.len());

    // 1. Packet codec is lossless over the whole capture.
    let mut stats = Stats::new();
    let mut violations: BTreeMap<String, usize> = BTreeMap::new();
    let mut tei_undecodable = 0usize;
    for (i, raw) in packets.iter().enumerate() {
        stats.push(i as u64, raw);
        let tei = raw[1] & 0x80 != 0;
        let p = match Packet::decode(raw) {
            Ok(p) => p,
            // A packet with transport_error_indicator '1' holds "at least 1
            // uncorrectable bit error" (§2.4.3.3); its fields may be garbage.
            Err(_) if tei => {
                tei_undecodable += 1;
                continue;
            }
            Err(e) => panic!("packet {i}: {e}"),
        };
        assert_eq!(&p.encode().unwrap(), raw, "packet {i} not lossless");
        if tei {
            continue;
        }
        for v in arib_ts::packet::check(&p) {
            *violations.entry(format!("{} {}", v.clause, v.message)).or_default() += 1;
        }
    }
    let tei_total: u64 = stats.pids.values().map(|s| s.tei).sum();
    println!("TEI packets: {tei_total} ({tei_undecodable} not decodable; violations below exclude TEI packets)");
    println!("PIDs: {}", stats.pids.len());
    for (pid, s) in &stats.pids {
        println!(
            "  PID {pid:#06x}: {:>7} pkts, pusi {:>6}, tei {}, scrambled {}, af-only {}, pcr {}, disc {}, malformed {}, cc events {}",
            s.packets, s.pusi, s.tei, s.scrambled, s.adaptation_only, s.pcrs.len(), s.discontinuity, s.malformed, s.cc_events
        );
    }
    let mut cc_kinds: BTreeMap<String, usize> = BTreeMap::new();
    for e in &stats.cc_events {
        let k = format!("{:?}", e.kind);
        *cc_kinds.entry(k.split([' ', '{']).next().unwrap().to_string()).or_default() += 1;
    }
    println!("cc events: {cc_kinds:?}");
    println!("packet rule violations: {violations:#?}");

    // 2. Find the PAT, then assemble all sections of interest.
    let mut demux = SectionDemux::new();
    demux.add_pid(0x0000);
    let mut pat: Option<Long<Pat>> = None;
    'find: for (i, raw) in packets.iter().enumerate() {
        for o in demux.push(i as u64, raw) {
            if let Assembled::Section(s) = o {
                pat = Some(Long::<Pat>::decode(&s.data).unwrap());
                break 'find;
            }
        }
    }
    let pat = pat.expect("a PAT in the capture");
    let pmt_pids: BTreeSet<u16> = pat.body.programs().map(|(_, pid)| pid).collect();
    println!("PAT tsid {:#06x}: network_PID {:?}, programs {:?}", pat.header.table_id_extension, pat.body.network_pid(), pat.body.programs().collect::<Vec<_>>());

    let mut demux = SectionDemux::new();
    for pid in [0x0000, 0x0001, 0x0010, 0x0011, 0x0012, 0x0013, 0x0014, 0x0024].into_iter().chain(pmt_pids.iter().copied()) {
        demux.add_pid(pid);
    }
    let mut by_table: BTreeMap<(u16, u8), (usize, usize)> = BTreeMap::new(); // (pid, table_id) → (long, short)
    let mut events: BTreeMap<String, usize> = BTreeMap::new();
    let mut psi_violations: BTreeMap<String, usize> = BTreeMap::new();
    let (mut n_long, mut n_short) = (0usize, 0usize);
    let mut es_pids: BTreeSet<u16> = BTreeSet::new();
    let mut pmts_seen = 0;
    for (i, raw) in packets.iter().enumerate() {
        for o in demux.push(i as u64, raw) {
            match o {
                Assembled::Section(s) => {
                    let ssi = s.data[1] & 0x80 != 0;
                    let e = by_table.entry((s.pid, s.data[0])).or_default();
                    if ssi {
                        n_long += 1;
                        e.0 += 1;
                        let (_, _, residue) = split_long_unchecked(&s.data).unwrap_or_else(|err| panic!("section on {:#x} at packet {}: {err}", s.pid, s.start_packet));
                        assert_eq!(residue, 0, "CRC_32 of table_id {:#04x} on PID {:#06x} (packets {}..={})", s.data[0], s.pid, s.start_packet, s.end_packet);
                    } else {
                        n_short += 1;
                        e.1 += 1;
                    }
                    match (s.pid, s.data[0]) {
                        (0x0000, 0x00) => {
                            let t = Long::<Pat>::decode(&s.data).unwrap();
                            assert_eq!(t.encode().unwrap(), s.data);
                            for v in check_pat(&t) {
                                *psi_violations.entry(format!("PAT {v}")).or_default() += 1;
                            }
                        }
                        (0x0001, 0x01) => {
                            let t = Long::<Cat>::decode(&s.data).unwrap();
                            assert_eq!(t.encode().unwrap(), s.data);
                            for v in check_cat(&t) {
                                *psi_violations.entry(format!("CAT {v}")).or_default() += 1;
                            }
                        }
                        (pid, 0x02) if pmt_pids.contains(&pid) => {
                            let t = Long::<Pmt>::decode(&s.data).unwrap();
                            assert_eq!(t.encode().unwrap(), s.data);
                            pmts_seen += 1;
                            // stream_types carried in PES packets (Table 2-34).
                            es_pids.extend(
                                t.body.streams.iter().filter(|x| matches!(x.stream_type, 0x01..=0x04 | 0x06 | 0x0F..=0x11 | 0x1B)).map(|x| x.elementary_pid),
                            );
                            for v in check_pmt(&t) {
                                *psi_violations.entry(format!("PMT {pid:#06x} {v}")).or_default() += 1;
                            }
                        }
                        _ => {}
                    }
                }
                Assembled::Event(e) => *events.entry(format!("{:?}", e.kind)).or_default() += 1,
            }
        }
    }
    println!("sections: {n_long} long (all CRC valid), {n_short} short; PMT sections {pmts_seen}");
    for ((pid, tid), (l, s)) in &by_table {
        println!("  PID {pid:#06x} table_id {tid:#04x}: {l} long, {s} short");
    }
    println!("demux events: {events:#?}");
    println!("PSI rule violations: {psi_violations:#?}");
    assert!(n_long > 0 && pmts_seen > 0);
    assert!(by_table.contains_key(&(0x0000, 0x00)));

    // 3. PES on the elementary PIDs (PES stream_types) listed in the PMTs.
    let mut asm: BTreeMap<u16, PesAssembler> = es_pids.iter().map(|&p| (p, PesAssembler::new(p))).collect();
    let mut pes_count: BTreeMap<u16, usize> = BTreeMap::new();
    let mut pes_events: BTreeMap<String, usize> = BTreeMap::new();
    let mut pes_violations: BTreeMap<String, usize> = BTreeMap::new();
    let mut handle = |o: PesOutput, pes_count: &mut BTreeMap<u16, usize>| match o {
        PesOutput::Pes(x) => {
            let p = PesPacket::decode(&x.data).unwrap_or_else(|e| panic!("PES on {:#x} at packet {}: {e}", x.pid, x.start_packet));
            assert_eq!(p.encode().unwrap(), x.data, "PES on {:#x} at packet {} not lossless", x.pid, x.start_packet);
            *pes_count.entry(x.pid).or_default() += 1;
            for v in arib_ts::pes::check(&p) {
                *pes_violations.entry(format!("{:#06x} {v}", x.pid)).or_default() += 1;
            }
        }
        PesOutput::Event(e) => *pes_events.entry(format!("{e:?}").split(['{', ' ']).next().unwrap().to_string()).or_default() += 1,
    };
    for (i, raw) in packets.iter().enumerate() {
        let pid = (((raw[1] & 0x1F) as u16) << 8) | raw[2] as u16;
        if let Some(a) = asm.get_mut(&pid) {
            for o in a.push(i as u64, raw) {
                handle(o, &mut pes_count);
            }
        }
    }
    for a in asm.values_mut() {
        for o in a.flush() {
            handle(o, &mut pes_count);
        }
    }
    println!("PES packets per PID (all decoded and re-encoded byte-identically): {pes_count:?}");
    println!("PES events: {pes_events:?}");
    println!("PES rule violations: {pes_violations:#?}");
}

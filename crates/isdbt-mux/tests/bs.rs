#![allow(missing_docs)]
//! `[network] kind = "bs"` (docs/plans/isdbs-next/bs-si.md §6.4 tests 1, 2
//! and 5): the NIT and BIT of `examples/bs-bench.toml` against the B10
//! field layout, their typed decode, and the independent validator.

mod common;

use common::*;
use isdb_analyze::scan::{ScanOptions, scan_packets};
use isdb_analyze::validate::{Severity, ValidateOptions, validate};
use isdb_si::descriptor::Descriptor;
use isdb_si::values::{BroadcastingIdentifier, FecInner, Polarization, SatelliteModulation};
use isdb_si::{AnyTable, tables::Nit};
use isdb_ts::Pid;
use isdbt_mux::analysis::{Report, analyze};
use isdbt_mux::mux::clocks_to_ticks;
use isdbt_mux::{Config, Multiplexer, Packet};

const EXAMPLE: &str = include_str!("../examples/bs-bench.toml");

/// `text` with the clock fixed, muxed for `frames` frames.
fn run(text: &str, name: &str, frames: u64) -> (Vec<Packet>, Report, Vec<String>) {
    let text = text.replace(
        "source = \"system\"",
        "source = \"fixed\"\nstart = \"2026-10-31T12:00:00\"",
    );
    let cfg = Config::from_toml(&text).unwrap();
    let mut mux = Multiplexer::new(&cfg, &bench_params(), &tmpdir(name)).unwrap();
    let warnings = mux.warnings().to_vec();
    let n = mux.pattern().len();
    let (mut full, mut times) = (Vec::new(), Vec::new());
    for k in 0..frames {
        let (f, _) = mux.next_frame_full();
        times.extend((0..n).map(|j| mux.slot_time(k, j)));
        full.extend(f);
    }
    let report = analyze(&full, &times, clocks_to_ticks(408));
    (full, report, warnings)
}

fn first_section(r: &Report, pid: u16) -> &[u8] {
    &r.sections[&Pid::new(pid).unwrap()][0].1
}

/// The NIT actual section of `bs-bench.toml`, written field by field from
/// B10 Part 2 §5.2.4 Table 5-3 and the descriptor tables (not taken from
/// the multiplexer; `tools/bs-nit-section.py 1b6f597341` gives the same).
const GOLDEN_NIT: &str = concat!(
    "40",   // table_id: network_information_section, actual network
    "f030", // section_syntax_indicator 1, reserved_future_use 1, reserved 11, section_length 48
    "0004", // network_id: BS digital broadcasting (Annex N)
    "c1",   // reserved 11, version_number 0, current_next_indicator 1
    "00",
    "00",             // section_number, last_section_number
    "f00b",           // reserved_future_use 1111, network_descriptors_length 11
    "40051b6f597341", // network_name_descriptor: "ベンチ" (ARIB 8-unit code)
    "fe020201", // system_management_descriptor: flag '00', identifier '000010' (Table 6-36), 0x01
    "f018",     // reserved_future_use 1111, transport_stream_loop_length 24
    "4017",     // transport_stream_id
    "0004",     // original_network_id
    "f012",     // reserved_future_use 1111, transport_descriptors_length 18
    "410303e701", // service_list_descriptor: service 0x03E7, type 0x01
    "430b",     // satellite_delivery_system_descriptor (§6.2.6 Table 6-8), 11 bytes
    "01172748", // frequency 011.72748 GHz (BS1)
    "1100",     // orbital_position 110.0
    "e8", // west_east_flag 1 (east), polarization '11' (Table 6-9), modulation '01000' (Table 6-10)
    "02886008", // symbol_rate 028.8600, FEC_inner '1000' (Table 6-11)
    "3f9c8672", // CRC_32
);

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

#[test]
fn spec_table_6_8_bs_bench_nit_is_golden() {
    let (_, r, _) = run(EXAMPLE, "bs_golden", 3);
    assert_eq!(hex(first_section(&r, 0x0010)), GOLDEN_NIT);
    // one NIT section only, sent unchanged
    assert!(
        r.sections[&Pid::new(0x0010).unwrap()]
            .iter()
            .all(|(_, s)| hex(s) == GOLDEN_NIT)
    );
}

#[test]
fn bs_bench_nit_and_bit_decode_to_the_bs_values() {
    let (_, r, warnings) = run(EXAMPLE, "bs_decode", 90);
    assert!(warnings.is_empty(), "{warnings:?}");
    let AnyTable::Nit(nit) =
        AnyTable::decode(Pid::new(0x10).unwrap(), first_section(&r, 0x0010)).unwrap()
    else {
        panic!("not a NIT")
    };
    let nit: &Nit = &nit;
    assert_eq!(nit.header.table_id_extension, 0x0004);
    let sm = nit
        .body
        .0
        .descriptors
        .iter()
        .find_map(|d| match d {
            Descriptor::SystemManagement(s) => Some(s.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(
        sm.broadcasting_identifier_typed(),
        BroadcastingIdentifier::WideBandSatellite11_7
    );
    let ts = &nit.body.0.transport_streams;
    assert_eq!(ts.len(), 1);
    assert_eq!(
        (ts[0].transport_stream_id, ts[0].original_network_id),
        (0x4017, 0x0004)
    );
    let tags: Vec<u8> = ts[0].descriptors.iter().map(Descriptor::tag).collect();
    assert_eq!(tags, vec![0x41, 0x43], "no 0xFA, 0xFB or 0xCD");
    let Descriptor::SatelliteDeliverySystem(sat) = &ts[0].descriptors[1] else {
        panic!()
    };
    assert_eq!(sat.frequency_10khz(), Some(1_172_748));
    assert_eq!(sat.orbital_position_tenths(), Some(1100));
    assert!(sat.west_east_flag);
    assert_eq!(sat.polarization(), Polarization::CircularRight);
    assert_eq!(sat.modulation_type(), SatelliteModulation::IsdbS);
    assert_eq!(sat.fec(), FecInner::IsdbS);
    assert_eq!(sat.symbol_rate_100sps(), Some(288_600));

    // BIT: onid header, broadcaster 0xEE with 0xD8 then 0x41 03E7/01
    let bit = first_section(&r, 0x0024);
    let AnyTable::Bit(b) = AnyTable::decode(Pid::new(0x24).unwrap(), bit).unwrap() else {
        panic!("not a BIT")
    };
    assert_eq!(b.header.table_id_extension, 0x0004);
    assert!(b.body.broadcast_view_propriety);
    assert_eq!(b.body.broadcasters.len(), 1);
    let bc = &b.body.broadcasters[0];
    assert_eq!(bc.broadcaster_id, 0xEE);
    let mut loop_bytes = Vec::new();
    for d in &bc.descriptors {
        d.encode(&mut loop_bytes).unwrap();
    }
    assert_eq!(hex(&loop_bytes), "d8051b6f597341410303e701");
    assert!(
        !bc.descriptors.iter().any(|d| d.tag() == 0xCE),
        "no extended_broadcaster_descriptor on BS"
    );
}

#[test]
fn bs_bench_validates_clean() {
    // 12 s: NIT (10 s) and BIT (20 s criterion, sent at start) seen
    let (full, _, _) = run(EXAMPLE, "bs_validate", 52);
    let cap = scan_packets(&full, &ScanOptions::default());
    let r = validate(&cap, &ValidateOptions::default());
    assert!(!r.has_errors(), "{}", r.to_text(Severity::Error));
    let si: Vec<_> = r
        .findings
        .iter()
        .filter(|f| matches!(f.check, "cross.nit" | "cross.delivery" | "cross.onid"))
        // isdb-analyze before bs-si.md piece C names only 0xFA; drop this
        // exception once its cross.nit accepts a 0x43
        .filter(|f| !f.message.contains("no terrestrial_delivery_system_descriptor"))
        .collect();
    assert!(si.is_empty(), "{si:?}");
}

#[test]
fn bs_remote_key_adds_ts_information_after_the_delivery_descriptor() {
    let text = EXAMPLE.replace(
        "# remote_control_key_id = 9 ",
        "remote_control_key_id = 9\nts_name = \"ベンチ\" ",
    );
    assert_ne!(text, EXAMPLE);
    let (_, r, _) = run(&text, "bs_key", 3);
    let AnyTable::Nit(nit) =
        AnyTable::decode(Pid::new(0x10).unwrap(), first_section(&r, 0x0010)).unwrap()
    else {
        panic!()
    };
    let ds = &nit.body.0.transport_streams[0].descriptors;
    assert_eq!(
        ds.iter().map(Descriptor::tag).collect::<Vec<_>>(),
        vec![0x41, 0x43, 0xCD]
    );
    let Descriptor::TsInformation(t) = &ds[2] else {
        panic!()
    };
    assert_eq!(t.remote_control_key_id, 9);
}

#[test]
fn bs_channel_gives_frequency_and_polarization() {
    for (ch, freq) in [
        ("BS3", 1_176_584u32),
        ("bs9", 1_188_092),
        ("BS23", 1_214_944),
    ] {
        let text = EXAMPLE.replace("channel = \"BS1\"", &format!("channel = \"{ch}\""));
        let (_, r, _) = run(&text, &format!("bs_ch_{ch}"), 3);
        let AnyTable::Nit(nit) =
            AnyTable::decode(Pid::new(0x10).unwrap(), first_section(&r, 0x0010)).unwrap()
        else {
            panic!()
        };
        let Descriptor::SatelliteDeliverySystem(sat) =
            &nit.body.0.transport_streams[0].descriptors[1]
        else {
            panic!()
        };
        // the frequencies of the BS NIT on air ([live], bs-si.md §3.3)
        assert_eq!(sat.frequency_10khz(), Some(freq), "{ch}");
        assert_eq!(sat.polarization(), Polarization::CircularRight, "{ch}");
    }
}

#[test]
fn bs_network_id_other_than_0x0004_warns() {
    let text = EXAMPLE.replace("network_id = 0x0004", "network_id = 0x0005");
    let (_, _, warnings) = run(&text, "bs_nid", 1);
    assert!(
        warnings
            .iter()
            .any(|w| w.contains("0x0004") && w.contains("Annex N")),
        "{warnings:?}"
    );
}

/// Every shipped example loads and survives `to_toml` (§6.4 test 3).
#[test]
fn every_example_loads_and_round_trips() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("examples");
    let mut n = 0;
    for e in std::fs::read_dir(&dir).unwrap() {
        let p = e.unwrap().path();
        if p.extension().is_some_and(|x| x == "toml") {
            let text = std::fs::read_to_string(&p).unwrap();
            let cfg = Config::from_toml(&text).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
            let back = cfg.to_toml().unwrap();
            let again = Config::from_toml(&back).unwrap();
            assert_eq!(again.to_toml().unwrap(), back, "{}", p.display());
            n += 1;
        }
    }
    assert!(n >= 6, "{n} examples");
}

#![allow(missing_docs)]
//! `Schedule::satellite`: one ISDB-S superframe of a relative TS, its slot
//! and PCR-byte times from `isdbs_core::slots`, and a multiplexer that fills
//! it (`examples/bs-bench.toml` on the bench TMCC: one PCR per superframe,
//! stamped with its slot's PCR-byte time).

mod common;

use common::*;
use isdb_ts::Pid;
use isdbs_core::Combination;
use isdbs_core::TmccInfo;
use isdbs_core::frame::{SUPERFRAME_SYMBOLS, symbols_to_ticks};
use isdbs_core::slots::{packet_byte_symbol, packet_symbol, ts_slots};
use isdbt_core::Layer;
use isdbt_mux::analysis::analyze;
use isdbt_mux::{Config, Multiplexer, MuxError, Schedule, System};

const BS_BENCH: &str = include_str!("../examples/bs-bench.toml");

/// TC8PSK 2/3 × 48, one TS: the bench layout.
fn bench_info() -> TmccInfo {
    TmccInfo::assign(&[(Combination::Tc8psk2_3, 48)], &[(0x4017, 48)]).unwrap()
}

fn layouts() -> Vec<(TmccInfo, usize)> {
    vec![
        (bench_info(), 0),
        (
            TmccInfo::assign(
                &[(Combination::Tc8psk2_3, 48)],
                &[(0x4010, 16), (0x4011, 16), (0x4012, 16)],
            )
            .unwrap(),
            1,
        ),
        (
            TmccInfo::assign(
                &[
                    (Combination::Tc8psk2_3, 20),
                    (Combination::Qpsk7_8, 8),
                    (Combination::Qpsk3_4, 12),
                    (Combination::Bpsk1_2, 8),
                ],
                &[(0x4010, 10), (0x4011, 14), (0x4012, 14)],
            )
            .unwrap(),
            2,
        ),
    ]
}

#[test]
fn satellite_schedule_is_the_packet_slot_time() {
    for (info, r) in layouts() {
        let s = Schedule::satellite(&info, r).unwrap();
        s.validate().unwrap();
        let n = ts_slots(&info, r);
        assert_eq!(s.lanes(), 1);
        assert_eq!(s.lane_slots(0), 8 * n);
        assert_eq!(s.period, SUPERFRAME_SYMBOLS as u64);
        assert_eq!((s.num, s.den), (450, 481));
        // 11 059 200/37 ticks per superframe, rounded down
        assert_eq!(s.period_ticks(), 11_059_200 / 37);
        assert!((s.period_seconds() - 319_488.0 / 28.86e6).abs() < 1e-12);
        assert_eq!((s.default_pcr_per_period, s.control_lane), (1, 0));
        assert_eq!(
            s.system,
            System::Satellite {
                info: info.clone(),
                relative_ts: r
            }
        );
        assert_eq!(s.params(), None);
        assert_eq!(s.lane_name(0), format!("relative TS {r}"));
        // the carried slots, in order, are the TS's packets 0..8n
        let carried: Vec<usize> = (0..s.slots.len())
            .filter(|&j| s.slots[j].is_some())
            .collect();
        for (q, &j) in carried.iter().enumerate() {
            let at = packet_symbol(&info, r, q).unwrap() as u64;
            assert_eq!(s.start[j], at);
            assert_eq!(
                s.pcr_byte[j],
                packet_byte_symbol(&info, r, q, 9).unwrap() as u64
            );
            for k in [0u64, 1, 36, 37, 1 << 20] {
                let sf = k * SUPERFRAME_SYMBOLS as u64;
                assert_eq!(s.slot_time(k, j), symbols_to_ticks(sf + at));
            }
        }
        // every sent slot of the superframe is listed, the others' as None
        let sent = info.slot_combinations().iter().flatten().count();
        assert_eq!(s.slots.len(), 8 * sent);
    }
    // 48 TC8PSK slots: 8 · 48 · 1504 bits per 11.07 ms = 52.17 Mbit/s
    let s = Schedule::satellite(&bench_info(), 0).unwrap();
    assert!(
        (s.lane_bps[0] - 52_170_000.0).abs() < 1e-3,
        "{}",
        s.lane_bps[0]
    );
    // bench: packet 48 f + s at 39 936 f + 192 + 828 s
    assert_eq!(s.start[48 + 5], 39_936 + 192 + 828 * 5);
}

#[test]
fn satellite_schedule_errors() {
    let e = Schedule::satellite(&bench_info(), 1).unwrap_err();
    assert!(
        matches!(&e, MuxError::Schedule { what } if what.contains("no slot")),
        "{e}"
    );
    let mut bad = bench_info();
    bad.combinations[0].slots = 47;
    let e = Schedule::satellite(&bad, 0).unwrap_err();
    assert!(
        matches!(&e, MuxError::Schedule { what } if what.contains("TMCC")),
        "{e}"
    );
}

/// `bs-bench.toml` with a fixed clock and its service on the one lane.
fn bs_bench_config() -> Config {
    let text = BS_BENCH
        .replace(
            "source = \"system\"",
            "source = \"fixed\"\nstart = \"2026-10-31T12:00:00\"",
        )
        .replace("layer = \"B\"", "layer = \"A\"");
    Config::from_toml(&text).unwrap()
}

#[test]
fn bs_bench_fills_the_superframe() {
    let cfg = bs_bench_config();
    let info = bench_info();
    let schedule = Schedule::satellite(&info, 0).unwrap();
    let carried: Vec<usize> = (0..schedule.slots.len())
        .filter(|&j| schedule.slots[j].is_some())
        .collect();
    let mut mux =
        Multiplexer::with_schedule(&cfg, schedule.clone(), &tmpdir("sat_bs_bench")).unwrap();
    // 400 superframes ≈ 4.4 s: PAT (100 ms), SDT/EIT (2 s), the first NIT
    let periods = 400u64;
    let (mut full, mut times, mut pcr_at) = (Vec::new(), Vec::new(), Vec::new());
    for k in 0..periods {
        let p = mux.next_period();
        assert_eq!(p.len(), 8 * 48);
        for &j in &carried {
            times.push(mux.slot_time(k, j));
            pcr_at.push(schedule.pcr_time(k, j));
        }
        full.extend(p);
    }
    assert_eq!(mux.frames(), periods);
    // one TS packet: 28 800/37 ticks
    let r = analyze(&full, &times, 778);
    assert!(r.problems.is_empty(), "{:?}", r.problems);
    assert!(r.cc_events.is_empty(), "{:?}", r.cc_events);
    // one PCR per superframe, valued at its slot's PCR byte (start_pcr 0)
    let pcrs = &r.pcrs[&Pid::new(0x01FF).unwrap()];
    assert_eq!(pcrs.len() as u64, periods);
    for &(idx, arrival, pcr) in pcrs {
        let i = usize::try_from(idx).unwrap();
        assert_eq!(arrival, times[i]);
        assert_eq!(pcr, pcr_at[i], "PCR of packet {idx}");
        // byte 9 of a TC8PSK slot: 36 symbols after its start
        assert!((pcr - arrival).abs_diff(symbols_to_ticks(36)) <= 1);
    }
    for (pid, name) in [
        (0x0000, "PAT"),
        (0x0010, "NIT"),
        (0x0011, "SDT"),
        (0x01F0, "PMT"),
    ] {
        assert!(
            r.sections.contains_key(&Pid::new(pid).unwrap()),
            "no {name}"
        );
    }
    // the PAT names the TMCC's TS_ID
    let pat = &r.sections[&Pid::new(0).unwrap()][0].1;
    assert_eq!(u16::from_be_bytes([pat[3], pat[4]]), 0x4017);
}

#[test]
fn satellite_refuses_what_isdb_t_needs() {
    let dir = tmpdir("sat_refuse");
    let schedule = Schedule::satellite(&bench_info(), 0).unwrap();
    // a service on layer B: not a lane of the satellite schedule
    let text = BS_BENCH.replace(
        "source = \"system\"",
        "source = \"fixed\"\nstart = \"2026-10-31T12:00:00\"",
    );
    let cfg = Config::from_toml(&text).unwrap();
    let e = Multiplexer::with_schedule(&cfg, schedule.clone(), &dir)
        .err()
        .unwrap();
    assert!(
        matches!(
            e,
            MuxError::InactiveLayer {
                layer: Layer::B,
                ..
            }
        ),
        "{e}"
    );
    // a terrestrial network (kind = "terrestrial", the default)
    let cfg = test_config(&dir, "");
    let e = Multiplexer::with_schedule(&cfg, schedule, &dir)
        .err()
        .unwrap();
    assert_eq!(e.key(), Some("network.kind"), "{e}");
}

#[test]
#[should_panic(expected = "ISDB-S schedule")]
fn params_of_a_satellite_multiplexer_panics() {
    let mux = Multiplexer::with_schedule(
        &bs_bench_config(),
        Schedule::satellite(&bench_info(), 0).unwrap(),
        &tmpdir("sat_params"),
    )
    .unwrap();
    let _ = mux.params();
}

/// ⌊x·a/b + ½⌋ in u128: packet `x` of a TS with `n` slots per frame on
/// the uniform grid, 1 382 400/(37·n) ticks per packet.
fn grid(x: u64, n: u64) -> u64 {
    let (a, b) = (1_382_400u128, 37 * u128::from(n));
    u64::try_from((2 * u128::from(x) * a + b) / (2 * b)).unwrap()
}

#[test]
fn satellite_uniform_schedule_is_the_per_ts_grid() {
    for (info, r) in layouts() {
        let s = Schedule::satellite_uniform(&info, r).unwrap();
        s.validate().unwrap();
        let sat = Schedule::satellite(&info, r).unwrap();
        let n = ts_slots(&info, r);
        assert_eq!(s.slots, vec![Some(0); 8 * n], "only the TS's own slots");
        assert_eq!(s.lane_slots(0), 8 * n);
        assert_eq!(s.start, s.pcr_byte);
        assert_eq!(s.period_ticks(), 11_059_200 / 37);
        assert!((s.period_seconds() - sat.period_seconds()).abs() < 1e-15);
        assert_eq!(s.lane_bps, sat.lane_bps);
        assert_eq!(s.system, sat.system);
        assert_eq!(
            (s.default_pcr_per_period, s.control_lane),
            (sat.default_pcr_per_period, sat.control_lane)
        );
        let per_sf = 8 * n as u64;
        for k in [0u64, 1, 36, 37, 1 << 20] {
            for q in [0, 1, 2, n / 2, n, 4 * n, 8 * n - 1] {
                let g = k * per_sf + q as u64;
                assert_eq!(s.slot_time(k, q), grid(g, n as u64), "k {k} q {q}");
                assert_eq!(s.pcr_time(k, q), grid(g, n as u64));
            }
        }
        // against the true slot times of the same packets: the
        // frame-periodic sawtooth, at most a few slots
        let carried: Vec<usize> = (0..sat.slots.len())
            .filter(|&j| sat.slots[j].is_some())
            .collect();
        let dev: Vec<i64> = carried
            .iter()
            .enumerate()
            .map(|(q, &j)| sat.slot_time(0, j) as i64 - s.slot_time(0, q) as i64)
            .collect();
        let (lo, hi) = (dev.iter().min().unwrap(), dev.iter().max().unwrap());
        eprintln!("relative TS {r} ({n} slots): slot time − grid in [{lo}, {hi}] ticks");
        assert!(hi > lo);
    }
    let s = Schedule::satellite_uniform(&bench_info(), 0).unwrap();
    // 48 slots: 28 800/37 ticks per packet, units of 1/74 tick
    assert_eq!((s.num, s.den), (1, 74));
    assert_eq!(&s.start[..3], &[37, 57_637, 115_237]);
}

#[test]
fn bs_bench_on_the_uniform_grid_stamps_its_pcrs_on_it() {
    let schedule = Schedule::satellite_uniform(&bench_info(), 0).unwrap();
    let mut mux =
        Multiplexer::with_schedule(&bs_bench_config(), schedule, &tmpdir("sat_uniform")).unwrap();
    let periods = 200u64;
    let (mut full, mut times) = (Vec::new(), Vec::new());
    for k in 0..periods {
        let p = mux.next_period();
        assert_eq!(p.len(), 384);
        times.extend((0..384).map(|q| mux.slot_time(k, q)));
        full.extend(p);
    }
    let r = analyze(&full, &times, 778);
    assert!(r.problems.is_empty(), "{:?}", r.problems);
    assert!(r.cc_events.is_empty(), "{:?}", r.cc_events);
    let pcrs = &r.pcrs[&Pid::new(0x01FF).unwrap()];
    assert_eq!(pcrs.len() as u64, periods);
    for &(idx, arrival, pcr) in pcrs {
        assert_eq!(pcr, arrival);
        assert_eq!(pcr, grid(idx, 48), "PCR of packet {idx}");
    }
}

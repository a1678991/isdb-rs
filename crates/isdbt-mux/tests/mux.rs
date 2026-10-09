#![allow(missing_docs)]

mod common;

use std::collections::BTreeMap;

use common::*;
use isdb_si::AnyTable;
use isdb_ts::psi::{Cat, Pat, Pmt, check_cat, check_pat, check_pmt};
use isdb_ts::section::Long;
use isdbt_core::Layer;
use isdbt_mux::analysis::{Report, analyze};
use isdbt_mux::mux::{HZ, clocks_to_ticks};
use isdbt_mux::{Multiplexer, Packet};

struct Run {
    mux: Multiplexer,
    full: Vec<Packet>,
    times: Vec<u64>,
    report: Report,
}

fn run(extra: &str, name: &str, frames: u64) -> Run {
    let dir = tmpdir(name);
    let cfg = test_config(&dir, extra);
    let params = bench_params();
    let mut mux = Multiplexer::new(&cfg, &params, &dir).unwrap();
    let n = mux.pattern().len();
    let mut full = Vec::new();
    let mut times = Vec::new();
    for k in 0..frames {
        let (f, layers) = mux.next_frame_full();
        assert_eq!(f.len(), n);
        for l in Layer::ALL {
            let want = params.layers[l.index()].map_or(0, |c| c.tsp_per_frame(params.mode));
            assert_eq!(layers[l.index()].len(), want, "frame {k} layer {l}");
        }
        for j in 0..n {
            times.push(mux.slot_time(k, j));
        }
        full.extend(f);
    }
    let report = analyze(&full, &times, clocks_to_ticks(408));
    Run {
        mux,
        full,
        times,
        report,
    }
}

fn interval_limits() -> BTreeMap<u8, u64> {
    // B10 Table 5-2 criteria (ms) keyed by table_id
    [
        (0x00, 100),
        (0x02, 100),
        (0x01, 1000),
        (0x40, 10_000),
        (0x42, 2000),
        (0x4E, 2000),
        (0x73, 30_000),
        (0xC4, 20_000),
    ]
    .into_iter()
    .map(|(t, ms)| (t, ms * HZ / 1000))
    .collect()
}

#[test]
fn tables_conform_and_repeat() {
    // 175 frames × 231.3 ms ≈ 40.5 s: every table (TOT: 30 s) repeats at least once.
    let r = run("", "conform", 175);
    assert!(
        r.report.problems.is_empty(),
        "{:?}",
        &r.report.problems[..r.report.problems.len().min(5)]
    );
    let mut seen = BTreeMap::new();
    for (&pid, secs) in &r.report.sections {
        for (_, s) in secs {
            *seen.entry(s[0]).or_insert(0) += 1;
            match s[0] {
                0x00 => assert!(check_pat(&Long::<Pat>::decode(s).unwrap()).is_empty()),
                0x01 => assert!(check_cat(&Long::<Cat>::decode(s).unwrap()).is_empty()),
                0x02 => {
                    let v = check_pmt(&Long::<Pmt>::decode(s).unwrap());
                    assert!(v.is_empty(), "{v:?}");
                }
                _ => {
                    let t = AnyTable::decode(pid, s).unwrap();
                    assert!(
                        !matches!(t, AnyTable::Unknown(_)),
                        "pid {pid:#x} tid {:#x} not typed",
                        s[0]
                    );
                    let d = isdb_si::conformance::check(pid, &t);
                    assert!(
                        d.is_empty(),
                        "{}: {:?}",
                        t.name(),
                        d.iter().map(|x| x.to_string()).collect::<Vec<_>>()
                    );
                    // lossless re-encode
                    assert_eq!(&t.encode().unwrap(), s);
                }
            }
        }
    }
    for tid in [0x00, 0x01, 0x02, 0x40, 0x42, 0x4E, 0x73, 0xC4] {
        assert!(
            seen.get(&tid).copied().unwrap_or(0) >= 2,
            "table {tid:#x} sent {:?} times",
            seen.get(&tid)
        );
    }
    let lim = interval_limits();
    for ((pid, tid, ext, sn), (gap, count)) in r.report.section_intervals() {
        let max = lim[&tid];
        assert!(count >= 2, "{pid:#x}/{tid:#x}/{ext:#x}/{sn} sent once");
        assert!(
            gap <= max,
            "{pid:#x}/{tid:#x}/{ext:#x}/{sn}: gap {} ms > {} ms",
            gap * 1000 / HZ,
            max * 1000 / HZ
        );
    }
    // EIT p/f switched from event 1 to event 2 at 12:00:00 (10 s in): version bumped
    assert!(r.mux.stats().version_changes >= 1);
    // CC continuous on every PID
    assert!(
        r.report.cc_events.is_empty(),
        "{:?}",
        &r.report.cc_events[..r.report.cc_events.len().min(5)]
    );
}

#[test]
fn eit_present_following() {
    let r = run("", "eit_pf", 60);
    let mut versions = BTreeMap::new();
    for (_, s) in &r.report.sections[&0x0012] {
        let t = AnyTable::decode(0x12, s).unwrap();
        let AnyTable::Eit(e) = t else { panic!() };
        assert_eq!(e.header.last_section_number, 1);
        assert_eq!(e.body.segment_last_section_number, 1);
        assert_eq!(e.body.last_table_id, 0x4E);
        if e.header.table_id_extension == 0x0400 {
            let ev: Vec<u16> = e.body.events.iter().map(|x| x.event_id).collect();
            versions
                .entry(e.header.version_number)
                .or_insert_with(Vec::new)
                .push((e.header.section_number, ev));
        }
    }
    // version 0: present 1 / following 2; version 1: present 2 / following none
    assert!(versions[&0].contains(&(0, vec![1])) && versions[&0].contains(&(1, vec![2])));
    assert!(versions[&1].contains(&(0, vec![2])) && versions[&1].contains(&(1, vec![])));
}

#[test]
fn pcr_and_timestamps() {
    let frames = 60; // 13.9 s: media (1.3 s video, 3.2 s audio) loops several times
    let r = run("", "pcr", frames);
    let params = bench_params();
    let n = r.mux.pattern().len() as u64;
    let a_count = r.mux.pattern().count(Layer::A) as u64;
    for (&pid, pcrs) in &r.report.pcrs {
        assert!(
            pid == 0x01FF || pid == 0x01FE,
            "unexpected PCR PID {pid:#x}"
        );
        // Mode 3: four PCR packets per frame per service (B31 Part 2 Table 3-5)
        assert_eq!(pcrs.len() as u64, 4 * frames);
        for w in pcrs.windows(2) {
            let (ia, _, pa) = w[0];
            let (ib, _, pb) = w[1];
            // H.222.0 §2.7.2: ≤ 0.1 s
            assert!(pb - pa <= HZ / 10, "PCR gap {} ticks", pb - pa);
            // consistent with the byte distance at the multiplex rate:
            // 408 clocks of 512/63 MHz per packet slot
            let expect = (ib - ia) as f64 * 408.0 * 27.0 * 63.0 / 512.0;
            assert!(
                ((pb - pa) as f64 - expect).abs() <= 1.0,
                "PCR {pa}->{pb} vs {expect}"
            );
            // equal intervals in the 13-segment TS (B31 Part 2 §3.2.4)
            assert_eq!(ib - ia, n / 4);
        }
        // and in the layer-A-only TS a one-seg receiver reproduces
        if pid == 0x01FE {
            let slots = r.mux.pattern().slots();
            let a_index: Vec<u64> = pcrs
                .iter()
                .map(|&(i, _, _)| {
                    let f = i / n;
                    let j = (i % n) as usize;
                    f * a_count + slots[..j].iter().filter(|s| **s == Some(Layer::A)).count() as u64
                })
                .collect();
            for w in a_index.windows(2) {
                assert_eq!(w[1] - w[0], a_count / 4);
            }
        }
        // first PCR equals the arrival time of its byte (start_pcr = 0)
        let (i0, t0, p0) = pcrs[0];
        assert_eq!(p0, t0 + clocks_to_ticks(20));
        let _ = i0;
    }
    let _ = params;
    for (&pid, pes) in &r.report.pes {
        assert!(pes.len() > 10, "PID {pid:#x}: {} PES", pes.len());
        let mut prev_dts = None;
        for p in pes {
            let pts = p.pts.expect("PTS in every PES (B32 Part 1 §5.1.1)");
            let dts = p.dts.unwrap_or(pts);
            assert!(dts <= pts);
            if let Some(d) = prev_dts {
                assert!(dts > d, "PID {pid:#x}: DTS not increasing");
            }
            prev_dts = Some(dts);
            // T-STD: whole PES arrived before decoding; ≤ 1 s in the buffers
            assert!(
                p.last_arrival / 300 <= dts,
                "PID {pid:#x}: PES arrives after its DTS"
            );
            assert!(
                dts * 300 - p.first_arrival <= HZ,
                "PID {pid:#x}: > 1 s in T-STD"
            );
        }
    }
    assert!(r.report.pes.contains_key(&0x0111) && r.report.pes.contains_key(&0x0112));
    for (pid, v) in r.mux.tstd_violations() {
        assert_eq!(
            v.tb_overflow + v.buffer_overflow + v.underflow,
            0,
            "PID {pid:#x}: {v:?}"
        );
        assert!(
            v.tb_max_nonempty <= HZ && v.mb_max_nonempty <= HZ,
            "PID {pid:#x}: {v:?}"
        );
    }
    assert_eq!(r.mux.stats().late_pes, 0);
    assert!(r.report.cc_events.is_empty());
    let _ = (&r.full, &r.times);
}

#[test]
fn overrides() {
    // a transplanted SDT section (built here as raw bytes) must go out byte-identical
    let sdt = {
        use isdb_si::tables::{SdtBody, SdtService};
        use isdb_ts::section::LongHeader;
        let mut h = LongHeader::new(0x42, 0x1234, 7);
        h.private_indicator = true;
        Long {
            header: h,
            body: SdtBody {
                original_network_id: 0x7FE0,
                reserved_future_use: 0xFF,
                services: vec![SdtService {
                    service_id: 0x0400,
                    reserved_future_use: 7,
                    eit_user_defined_flags: 7,
                    eit_schedule_flag: false,
                    eit_present_following_flag: true,
                    running_status: 4,
                    free_ca_mode: false,
                    descriptors: vec![],
                }],
            },
        }
        .encode()
        .unwrap()
    };
    let hex: String = sdt.iter().map(|b| format!("{b:02x}")).collect();
    let extra = format!(
        r#"
[sdt]
replace_sections = ["{hex}"]

[nit]
add_descriptors = [ {{ loop = "ts", tag = 0x8F, hex = "60" }}, {{ loop = "network", tag = 0xCF, hex = "03 41", first = true }} ]

[pat]
header = {{ version_number = 9 }}

[tot]
byte_patches = [ {{ offset = 7, hex = "45" }} ]
interval_ms = 5000
"#
    );
    let r = run(&extra, "overrides", 30);
    let sdts = &r.report.sections[&0x0011];
    assert!(!sdts.is_empty());
    assert!(
        sdts.iter().all(|(_, s)| *s == sdt),
        "SDT on air differs from the transplanted bytes"
    );
    let nit = &r.report.sections[&0x0010][0].1;
    let AnyTable::Nit(nit) = AnyTable::decode(0x10, nit).unwrap() else {
        panic!()
    };
    assert_eq!(nit.body.0.descriptors[0].tag(), 0xCF);
    assert!(
        nit.body.0.transport_streams[0]
            .descriptors
            .iter()
            .any(|d| d.tag() == 0x8F)
    );
    let pat = Long::<Pat>::decode(&r.report.sections[&0x0000][0].1).unwrap();
    assert_eq!(pat.header.version_number, 9);
    for (_, s) in &r.report.sections[&0x0014] {
        assert_eq!(s[7], 0x45); // patched seconds byte; CRC still valid
        assert!(AnyTable::decode(0x14, s).is_ok());
    }
}

#[test]
fn feeds_transmitter() {
    let dir = tmpdir("phy");
    let cfg = test_config(&dir, "");
    let params = bench_params();
    let mut mux = Multiplexer::new(&cfg, &params, &dir).unwrap();
    let mut tx = isdbt_phy::Transmitter::new(params.clone());
    for f in mux.by_ref().take(3) {
        let iq = tx.transmit_frame(isdbt_mux::layer_slices(&f)).unwrap();
        assert_eq!(iq.len(), params.samples_per_frame());
    }
    assert_eq!(tx.frames(), 3);
}

#[test]
fn example_config_parses() {
    let cfg = isdbt_mux::Config::from_toml(isdbt_mux::config::EXAMPLE_TOML).unwrap();
    assert_eq!(cfg.services.len(), 2);
    let back = cfg.to_toml().unwrap();
    isdbt_mux::Config::from_toml(&back).unwrap();
}

#[test]
fn schedule_tdt_pcr_in_video_custom_intervals() {
    let extra = r#"
[eit_schedule]
enabled = true
interval_ms = 3000

[tdt]
enabled = true
interval_ms = 4000

[nit]
interval_ms = 1500

[timing]
pcr_per_frame = 8
"#;
    // PCR in the video PID: patch the first service's pcr_pid via a second config
    let dir = tmpdir("sched");
    let mut cfg = test_config(&dir, extra);
    cfg.services[0].pcr_pid = 0x0111;
    let params = bench_params();
    let mut mux = Multiplexer::new(&cfg, &params, &dir).unwrap();
    let n = mux.pattern().len();
    let mut full = Vec::new();
    let mut times = Vec::new();
    for k in 0..60 {
        let (f, _) = mux.next_frame_full();
        times.extend((0..n).map(|j| mux.slot_time(k, j)));
        full.extend(f);
    }
    let r = analyze(&full, &times, clocks_to_ticks(408));
    assert!(
        r.cc_events.is_empty(),
        "{:?}",
        &r.cc_events[..r.cc_events.len().min(3)]
    );
    // PCR carried in video packets, 8 per frame → ≤ 33 ms
    let p = &r.pcrs[&0x0111];
    assert_eq!(p.len(), 8 * 60);
    assert!(p.windows(2).all(|w| w[1].2 - w[0].2 <= HZ / 25));
    // EIT schedule sections decode, conform, and follow the segment rules
    let mut sched = 0;
    for (_, s) in &r.sections[&0x0012] {
        let AnyTable::Eit(e) = AnyTable::decode(0x12, s).unwrap() else {
            panic!()
        };
        let d = isdb_si::conformance::check(0x12, &AnyTable::Eit(e.clone()));
        assert!(
            d.is_empty(),
            "{:?}",
            d.iter().map(|x| x.to_string()).collect::<Vec<_>>()
        );
        if (0x50..=0x5F).contains(&e.header.table_id) {
            sched += 1;
            let s0 = e.header.section_number / 8 * 8;
            assert!(
                e.body.segment_last_section_number >= s0
                    && e.body.segment_last_section_number < s0 + 8
            );
            assert_eq!(e.body.last_table_id, 0x50);
        }
    }
    assert!(sched >= 2, "no EIT schedule sections");
    // SDT now announces the schedule
    let AnyTable::Sdt(sdt) = AnyTable::decode(0x11, &r.sections[&0x0011][0].1).unwrap() else {
        panic!()
    };
    assert!(
        sdt.body
            .services
            .iter()
            .find(|x| x.service_id == 0x0400)
            .unwrap()
            .eit_schedule_flag
    );
    // custom intervals respected; TDT present
    let iv = r.section_intervals();
    let lim: BTreeMap<u8, u64> = [(0x40u8, 1500u64), (0x70, 4000), (0x50, 3000)]
        .into_iter()
        .map(|(t, m)| (t, m * HZ / 1000))
        .collect();
    let mut tdt = 0;
    for ((pid, tid, _, _), (gap, count)) in iv {
        if let Some(&max) = lim.get(&tid) {
            assert!(gap <= max, "{pid:#x}/{tid:#x}: {} ms", gap * 1000 / HZ);
            if tid == 0x70 {
                tdt = count;
            }
        }
    }
    assert!(tdt >= 3);
    // B10 §5.1.4: no section PID exceeds 4096 bytes (21 packets) in any 32 ms
    let win = 32 * HZ / 1000;
    for pid in [
        0x0000u16, 0x0001, 0x0010, 0x0011, 0x0012, 0x0014, 0x0024, 0x01F0, 0x1FC8,
    ] {
        let t: Vec<u64> = full
            .iter()
            .zip(&times)
            .filter(|(p, _)| (((p[1] as u16 & 0x1F) << 8) | p[2] as u16) == pid)
            .map(|(_, &t)| t)
            .collect();
        let mut lo = 0;
        for hi in 0..t.len() {
            while t[hi] - t[lo] >= win {
                lo += 1;
            }
            assert!(
                hi - lo < 21,
                "PID {pid:#x}: {} packets in 32 ms",
                hi - lo + 1
            );
        }
    }
    for (pid, v) in mux.tstd_violations() {
        assert_eq!(
            v.tb_overflow + v.buffer_overflow + v.underflow,
            0,
            "PID {pid:#x}: {v:?}"
        );
    }
}

#[test]
fn derived_nit_values() {
    let r = run("", "nit", 3);
    let AnyTable::Nit(nit) = AnyTable::decode(0x10, &r.report.sections[&0x0010][0].1).unwrap()
    else {
        panic!()
    };
    let ts = &nit.body.0.transport_streams[0];
    let mut found = 0;
    for d in &ts.descriptors {
        match d {
            isdb_si::Descriptor::TerrestrialDeliverySystem(t) => {
                assert_eq!(t.guard_interval, 0b10); // 1/8 (B10 Table 6-62)
                assert_eq!(t.transmission_mode, 0b10); // Mode 3 (Table 6-63)
                assert_eq!(t.frequencies, vec![(557 * 7 + 1) as u16]); // ch 27 = 557 1/7 MHz
                assert_eq!(t.area_code, 0x5A5);
                found += 1;
            }
            isdb_si::Descriptor::PartialReception(p) => {
                assert_eq!(p.service_ids, vec![0x0588]);
                found += 1;
            }
            isdb_si::Descriptor::TsInformation(t) => {
                assert_eq!(t.remote_control_key_id, 12);
                assert_eq!(t.transmission_types.len(), 2);
                assert_eq!(t.transmission_types[0].service_ids, vec![0x0400]);
                assert_eq!(t.transmission_types[1].service_ids, vec![0x0588]);
                found += 1;
            }
            isdb_si::Descriptor::ServiceList(l) => {
                assert_eq!(l.services.len(), 2);
                found += 1;
            }
            _ => {}
        }
    }
    assert_eq!(found, 4);
    // PAT carries the network_PID entry and both PMTs
    let pat = Long::<Pat>::decode(&r.report.sections[&0x0000][0].1).unwrap();
    assert_eq!(pat.body.network_pid(), Some(0x0010));
    assert_eq!(
        pat.body.programs().collect::<Vec<_>>(),
        vec![(0x0400, 0x01F0), (0x0588, 0x1FC8)]
    );
    // PMT of the HD service: video decode control derived from the ES (1080/I)
    let pmt = Long::<Pmt>::decode(&r.report.sections[&0x01F0][0].1).unwrap();
    assert_eq!(pmt.body.pcr_pid, 0x01FF);
    let v = &pmt.body.streams[0];
    assert_eq!((v.stream_type, v.elementary_pid), (0x02, 0x0111));
    let c8 = v.descriptors.iter().find(|d| d.tag == 0xC8).unwrap();
    assert_eq!(c8.data[0] >> 2 & 0xF, 0b0001);
    assert_eq!(pmt.body.streams[1].stream_type, 0x0F);
    // PSI on the layers B31 Part 2 Tables 3-1/3-3 give: PAT/NIT on A, HD PMT on B
    let slots = r.mux.pattern().slots();
    let n = slots.len();
    let layer_of = |pid: u16| {
        r.full
            .iter()
            .enumerate()
            .filter(|(_, p)| (((p[1] as u16 & 0x1F) << 8) | p[2] as u16) == pid)
            .map(|(i, _)| slots[i % n].unwrap())
            .collect::<std::collections::BTreeSet<_>>()
    };
    assert_eq!(
        layer_of(0x0000).into_iter().collect::<Vec<_>>(),
        vec![Layer::A]
    );
    assert_eq!(
        layer_of(0x0010).into_iter().collect::<Vec<_>>(),
        vec![Layer::A]
    );
    assert_eq!(
        layer_of(0x01F0).into_iter().collect::<Vec<_>>(),
        vec![Layer::B]
    );
    assert_eq!(
        layer_of(0x1FC8).into_iter().collect::<Vec<_>>(),
        vec![Layer::A]
    );
}

#[test]
fn hierarchical_descriptor_derived() {
    let dir = tmpdir("hier");
    let mut cfg = test_config(&dir, "");
    // HQ video on layer B, a declared LQ video of the same service on layer A
    let mut lq = cfg.services[1].streams[0].clone();
    lq.pid = 0x0118;
    lq.layer = Some(isdbt_mux::config::LayerName::A);
    lq.hierarchical = Some(isdbt_mux::config::HierarchicalCfg {
        reference_pid: 0x0111,
        high_quality: None,
    });
    cfg.services[0].streams.push(lq);
    cfg.services[0].streams[0].hierarchical = Some(isdbt_mux::config::HierarchicalCfg {
        reference_pid: 0x0118,
        high_quality: None,
    });
    let params = bench_params();
    let mut mux = Multiplexer::new(&cfg, &params, &dir).unwrap();
    let n = mux.pattern().len();
    let mut full = Vec::new();
    let mut times = Vec::new();
    for k in 0..4 {
        let (f, _) = mux.next_frame_full();
        times.extend((0..n).map(|j| mux.slot_time(k, j)));
        full.extend(f);
    }
    let r = analyze(&full, &times, clocks_to_ticks(408));
    let pmt = Long::<Pmt>::decode(&r.sections[&0x01F0][0].1).unwrap();
    let q = |pid: u16| {
        let st = pmt
            .body
            .streams
            .iter()
            .find(|s| s.elementary_pid == pid)
            .unwrap();
        let d = st.descriptors.iter().find(|d| d.tag == 0xC0).unwrap();
        (
            d.data[0] & 1 == 1,
            u16::from_be_bytes([d.data[1], d.data[2]]) & 0x1FFF,
        )
    };
    assert_eq!(q(0x0111), (true, 0x0118)); // less robust layer B → high quality
    assert_eq!(q(0x0118), (false, 0x0111));
    // B31 Part 2 Table 3-3 condition 2: PMT on the robustest ES layer (A)
    let slots = mux.pattern().slots();
    assert!(
        full.iter()
            .enumerate()
            .filter(|(_, p)| (((p[1] as u16 & 0x1F) << 8) | p[2] as u16) == 0x01F0)
            .all(|(i, _)| slots[i % n] == Some(Layer::A))
    );
}

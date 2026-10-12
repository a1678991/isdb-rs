#![allow(missing_docs)]
//! BML data broadcasting (`kind = "bml-carousel"`) and Hybridcast
//! (`[service.hybridcast]`): the carousel on air reassembles to the files,
//! the DII / DDB / PMT / EIT / AIT fields carry the configured values, the
//! carousel keeps its rate and repetition, the independent validator finds
//! no error, and the configuration checks refuse what the specs forbid.

mod common;

use std::collections::BTreeMap;
use std::path::Path;

use common::*;
use isdb_analyze::scan::{ScanOptions, scan_packets};
use isdb_analyze::validate::{Severity, ValidateOptions, validate};
use isdb_si::ait::{Ait, AitDescriptor, HttpSelector};
use isdb_si::bxml::{AdditionalAribBxmlInfo, AitIdentifierInfo, AribBxmlInfo, BxmlEntry};
use isdb_si::descriptor::Descriptor;
use isdb_si::dsmcc::{Ddb, Dii, ModuleDescriptor};
use isdb_si::tables::EitBody;
use isdb_ts::demux::{Assembled, SectionAssembler};
use isdb_ts::psi::Pmt;
use isdb_ts::section::Long;
use isdbt_mux::analysis::{Report, analyze};
use isdbt_mux::carousel::parse_multipart;
use isdbt_mux::mux::{HZ, clocks_to_ticks};
use isdbt_mux::{Config, Multiplexer, Packet};

const STARTUP: &str = "<?xml version=\"1.0\" encoding=\"EUC-JP\"?>\n<bml><head><title>t</title></head><body/></bml>\n";

fn config(hybridcast: &str, stream_extra: &str, bml: &str) -> String {
    format!(
        r#"
[network]
network_id = 0x79EE
name = "ベンチ"
area_code = 0x5A5
remote_control_key_id = 12
channels = [27]

[transport]
transport_stream_id = 0x79EE

[clock]
source = "fixed"
start = "2026-10-08T12:00:00"

[[service]]
service_id = 0x0400
name = "ベンチHD"
provider = "ベンチ"
layer = "B"
pmt_pid = 0x01F0
pcr_pid = 0x01FF
copy_control = {{ recording_control = "copy-free" }}
{hybridcast}

  [[service.stream]]
  kind = "declared"
  pid = 0x0111
  stream_type = 0x02
  component_tag = 0x00

  [[service.stream]]
  kind = "bml-carousel"
  pid = 0x0140
  component_tag = 0x40
  {stream_extra}
    [service.stream.bml]
    {bml}

  [[service.event]]
  event_id = 1
  start_offset_s = -600
  duration = "01:00:00"
  name = "データ放送"
"#
    )
}

const HYBRIDCAST: &str = r#"
  [service.hybridcast]
  url = "https://example.com/hybrid/app1/index.html"
  organization_id = 16
  application_id = 0x0000_0002
"#;

struct Run {
    full: Vec<Packet>,
    times: Vec<u64>,
    report: Report,
    notes: Vec<String>,
}

fn run(cfg: &Config, dir: &Path, frames: u64) -> Run {
    let params = bench_params();
    let mut mux = Multiplexer::new(cfg, &params, dir).unwrap();
    assert!(mux.warnings().is_empty(), "{:?}", mux.warnings());
    let n = mux.pattern().len();
    let mut full = Vec::new();
    let mut times = Vec::new();
    for k in 0..frames {
        let (f, _) = mux.next_frame_full();
        times.extend((0..n).map(|j| mux.slot_time(k, j)));
        full.extend(f);
    }
    let report = analyze(&full, &times, clocks_to_ticks(408));
    Run {
        full,
        times,
        report,
        notes: mux.notes().to_vec(),
    }
}

/// Sections of `pid` with the arrival time of their first packet.
fn sections(r: &Run, pid: u16) -> Vec<(u64, Vec<u8>)> {
    let mut a = SectionAssembler::new(pid);
    let mut v = Vec::new();
    for (i, p) in r.full.iter().enumerate() {
        let ppid = ((p[1] as u16 & 0x1F) << 8) | p[2] as u16;
        if ppid != pid {
            continue;
        }
        for o in a.push(i as u64, p) {
            match o {
                Assembled::Section(s) => v.push((r.times[s.start_packet as usize], s.data)),
                Assembled::Event(e) => panic!("{e:?}"),
            }
        }
    }
    v
}

/// Reassemble the modules of a carousel from its DDBs: moduleId → bytes.
fn modules(secs: &[(u64, Vec<u8>)], dii: &Dii) -> BTreeMap<u16, Vec<u8>> {
    let mut out = BTreeMap::new();
    for m in &dii.body.modules {
        let mut blocks: BTreeMap<u16, Vec<u8>> = BTreeMap::new();
        for (_, s) in secs.iter().filter(|(_, s)| s[0] == 0x3C) {
            let d = Ddb::decode(s).unwrap();
            assert_eq!(d.body.download_id, dii.body.download_id);
            if d.body.module_id == m.module_id {
                assert_eq!(d.header.table_id_extension, m.module_id);
                assert_eq!(d.body.module_version, m.module_version);
                assert_eq!(d.header.version_number, m.module_version & 0x1F);
                assert_eq!(d.header.section_number, d.body.block_number as u8);
                if d.body.block_number as usize + 1
                    < m.module_size.div_ceil(dii.body.block_size as u32) as usize
                {
                    assert_eq!(d.body.data.len(), dii.body.block_size as usize);
                }
                blocks.insert(d.body.block_number, d.body.data);
            }
        }
        let n = blocks.len();
        assert_eq!(
            blocks.keys().last().copied(),
            Some(n as u16 - 1),
            "module {:#06x} blocks",
            m.module_id
        );
        let bytes: Vec<u8> = blocks.into_values().flatten().collect();
        assert_eq!(bytes.len(), m.module_size as usize);
        out.insert(m.module_id, bytes);
    }
    out
}

fn pmt(r: &Run) -> Long<Pmt> {
    Long::<Pmt>::decode(&r.report.sections[&0x01F0][0].1).unwrap()
}

fn no_validation_errors(pkts: &[Packet], what: &str) {
    let cap = scan_packets(pkts, &ScanOptions::default());
    let r = validate(&cap, &ValidateOptions::default());
    assert!(!r.has_errors(), "{what}:\n{}", r.to_text(Severity::Error));
}

#[test]
fn carousel_carries_the_files_and_signals_them() {
    let dir = tmpdir("bml-carousel");
    let bml = dir.join("bml");
    std::fs::create_dir_all(&bml).unwrap();
    std::fs::write(bml.join("startup.bml"), STARTUP).unwrap();
    let png: Vec<u8> = (0..9000u32).map(|i| (i * 7) as u8).collect();
    std::fs::write(bml.join("logo.png"), &png).unwrap();
    std::fs::write(bml.join("style.css"), "p { color-index: 7; }").unwrap();
    let cfg = Config::from_toml(&config(
        "",
        "source = \"bml\"",
        "return_to_entry = true\n    data_event_id = 5\n    pmt_data_event_id = 5\n    download_id = 0x123\n    independent = false\n    control_descriptor = true",
    ))
    .unwrap();
    let r = run(&cfg, &dir, 40); // ≈ 9.3 s
    assert!(
        r.notes
            .iter()
            .any(|n| n.contains("1 module(s)") && n.contains("startup.bml, logo.png, style.css")),
        "{:?}",
        r.notes
    );

    // ---- PMT: stream_type 0x0D, stream identifier 0x40, data component 0x000C
    let p = pmt(&r);
    let es = p
        .body
        .streams
        .iter()
        .find(|s| s.elementary_pid == 0x0140)
        .unwrap();
    assert_eq!(es.stream_type, 0x0D);
    assert_eq!(es.descriptors[0].tag, 0x52);
    assert_eq!(es.descriptors[0].data, vec![0x40]);
    let Descriptor::DataComponent(dc) =
        Descriptor::decode_body(es.descriptors[1].tag, &es.descriptors[1].data).unwrap()
    else {
        panic!("{:?}", es.descriptors[1]);
    };
    assert_eq!(dc.data_component_id, 0x000C);
    let info = AdditionalAribBxmlInfo::decode(&dc.additional_data_component_info).unwrap();
    let BxmlEntry::Entry(e) = &info.entry else {
        panic!("{info:?}")
    };
    assert!(e.auto_start);
    assert_eq!(e.document_resolution, 0b0011); // 960x540
    assert_eq!(e.versions.map(|v| (v.bml_major, v.bml_minor)), Some((3, 0))); // the default
    assert!(!e.independent && e.style_for_tv);
    assert_eq!(info.carousel.unwrap().data_event_id, 5);
    assert!(info.ondemand_retrieval);

    // ---- EIT: data content descriptor with the carousel
    let eit = r.report.sections[&0x0012]
        .iter()
        .map(|(_, s)| Long::<EitBody>::decode(s).unwrap())
        .find(|t| !t.body.events.is_empty())
        .unwrap();
    let Some(Descriptor::DataContent(dcd)) = eit.body.events[0]
        .descriptors
        .iter()
        .find(|d| matches!(d, Descriptor::DataContent(_)))
    else {
        panic!("{:?}", eit.body.events[0].descriptors);
    };
    assert_eq!(dcd.data_component_id, 0x000C);
    assert_eq!(dcd.entry_component, 0x40);
    assert_eq!(dcd.component_refs, vec![0x00]); // the video component
    let sel = AribBxmlInfo::decode(&dcd.selector).unwrap();
    assert_eq!(sel.carousels.len(), 1);
    assert_eq!(sel.carousels[0].component_tag, 0x40);
    assert_eq!(sel.language, *b"jpn");

    // ---- carousel: DII and the reassembled module
    let secs = sections(&r, 0x0140);
    let diis: Vec<&(u64, Vec<u8>)> = secs.iter().filter(|(_, s)| s[0] == 0x3B).collect();
    assert!(diis.len() >= 8, "{} DIIs in 9 s", diis.len());
    let dii = Dii::decode(&diis[0].1).unwrap();
    assert_eq!(dii.header.version_number, 0);
    assert_eq!(dii.body.transaction_id, 0x8000_0000);
    assert_eq!(dii.body.download_id, 0x5000_0123);
    assert_eq!(dii.body.block_size, 4066);
    assert_eq!(dii.body.compatibility, vec![0, 0]);
    assert_eq!(dii.body.t_c_download_scenario, u32::MAX);
    assert_eq!(dii.body.modules.len(), 1);
    let m0 = &dii.body.modules[0];
    assert_eq!(m0.module_id, 0);
    assert!(m0.info.contains(&ModuleDescriptor::Type {
        media_type: b"multipart/mixed; boundary=\"IETF_RFC1945_Multipart_Mixed\"".to_vec()
    }));
    assert!(
        m0.info
            .iter()
            .any(|d| matches!(d, ModuleDescriptor::Control { .. }))
    );
    assert_eq!(
        dii.body.private,
        vec![ModuleDescriptor::AribBxmlPrivateData {
            return_to_entry: true,
            reserved: 0x7F,
        }]
    );
    assert_eq!(sel.carousels[0].component_size, Some(m0.module_size));
    let mods = modules(&secs, &dii);
    let res = parse_multipart(&mods[&0]).unwrap();
    assert_eq!(res.len(), 3);
    assert_eq!(res[0].name, "startup.bml");
    assert_eq!(res[0].media_type, "text/X-arib-bml;charset=\"euc-jp\"");
    assert_eq!(res[0].data, STARTUP.as_bytes());
    assert_eq!(res[1].name, "logo.png");
    assert_eq!(res[1].media_type, "image/X-arib-png");
    assert_eq!(res[1].data, png);
    assert_eq!(res[2].name, "style.css");
    assert_eq!(res[2].media_type, "text/css");

    // ---- repetition and rate: DII at most 0.5 s apart, the cycle restarts
    // within 1 s, the PID never exceeds its rate over any 32 ms window
    for w in diis.windows(2) {
        assert!(
            w[1].0 - w[0].0 <= HZ / 2 + HZ / 20,
            "DII gap {} ms",
            (w[1].0 - w[0].0) * 1000 / HZ
        );
    }
    let firsts: Vec<u64> = secs
        .iter()
        .filter(|(_, s)| s[0] == 0x3C && s[6] == 0)
        .map(|(t, _)| *t)
        .collect();
    assert!(firsts.len() >= 8, "{} cycles", firsts.len());
    for w in firsts.windows(2) {
        assert!(
            w[1] - w[0] <= HZ + HZ / 10,
            "cycle {} ms",
            (w[1] - w[0]) * 1000 / HZ
        );
    }
    let pkt_times: Vec<u64> = r
        .full
        .iter()
        .zip(&r.times)
        .filter(|(p, _)| ((p[1] as u16 & 0x1F) << 8) | p[2] as u16 == 0x0140)
        .map(|(_, t)| *t)
        .collect();
    let window = 32 * HZ / 1000;
    let max_in_window = (1_000_000.0_f64 / 8.0 / 188.0 * 0.032).floor() as usize; // 21
    for (i, &t0) in pkt_times.iter().enumerate() {
        let n = pkt_times[i..]
            .iter()
            .take_while(|&&t| t < t0 + window)
            .count();
        assert!(n <= max_in_window, "{n} carousel packets in 32 ms");
    }
    assert!(r.report.cc_events.is_empty(), "{:?}", r.report.cc_events);
    no_validation_errors(&r.full, "bml carousel");
}

#[test]
fn hybridcast_ait_in_the_carousel_with_a_launcher() {
    let dir = tmpdir("hybridcast-carousel");
    let cfg = Config::from_toml(&config(HYBRIDCAST, "", "")).unwrap();
    let r = run(&cfg, &dir, 20);
    assert!(
        r.notes.iter().any(|n| n.contains("generated startup.bml")
            && n.contains("arib-dc://-1.-1.-1/40/9FFF/app1.ait")),
        "{:?}",
        r.notes
    );
    let secs = sections(&r, 0x0140);
    let dii = Dii::decode(&secs.iter().find(|(_, s)| s[0] == 0x3B).unwrap().1).unwrap();
    assert_eq!(dii.body.modules.len(), 2);
    let mods = modules(&secs, &dii);
    // module 0: the launcher
    let m0 = parse_multipart(&mods[&0]).unwrap();
    assert_eq!(m0.len(), 1);
    assert_eq!(m0[0].name, "startup.bml");
    let doc = std::str::from_utf8(&m0[0].data).unwrap();
    assert!(
        doc.starts_with("<?xml version=\"1.0\" encoding=\"EUC-JP\"?>"),
        "{doc}"
    );
    assert!(
        doc.contains("<!DOCTYPE bml PUBLIC \"+//ARIB STD-B24:1999//DTD BML Document//JA\" \"http://www.arib.or.jp/B24/DTD/bml_1_1.dtd\">"),
        "{doc}"
    );
    assert!(doc.contains("<?bml bml-version=\"3.0\"?>"), "{doc}");
    assert!(
        doc.contains("type=\"video/X-arib-mpeg2\" data=\"/-1\""),
        "{doc}"
    );
    assert!(
        doc.contains("browser.startAITControlledApp(16, 2, 'arib-dc://-1.-1.-1/40/9FFF/app1.ait')"),
        "{doc}"
    );
    assert!(doc.contains("resolution:960x540"), "{doc}");
    assert!(doc.contains("no Hybridcast engine"), "{doc}");
    assert!(doc.is_ascii());
    // the entry component's DII carries return_to_entry by default
    assert_eq!(
        dii.body.private,
        vec![ModuleDescriptor::AribBxmlPrivateData {
            return_to_entry: true,
            reserved: 0x7F
        }]
    );
    assert_eq!(dii.body.download_id, 0x0FFF_FFFF);
    // module 0x9FFF: the AIT, no resource list
    let ait_mod = parse_multipart(&mods[&0x9FFF]).unwrap();
    assert_eq!(ait_mod.len(), 1);
    assert_eq!(ait_mod[0].name, "app1.ait");
    assert_eq!(ait_mod[0].media_type, "application/X-arib-ait");
    assert!(
        !mods[&0x9FFF]
            .windows(b"X-arib-resourceList".len())
            .any(|w| w == b"X-arib-resourceList")
    );
    let ait = Ait::decode(&ait_mod[0].data).unwrap();
    assert_eq!(ait.header.table_id, 0x74);
    assert_eq!(ait.header.table_id_extension, 0x0010);
    assert_eq!(ait.body.applications.len(), 1);
    let app = &ait.body.applications[0];
    assert_eq!(
        (app.organization_id, app.application_id, app.control_code),
        (16, 2, 0x01)
    );
    let AitDescriptor::TransportProtocol(tp) = &ait.body.common_descriptors[0] else {
        panic!()
    };
    assert_eq!(tp.protocol_id, 0x0003);
    let sel = HttpSelector::decode(&tp.selector).unwrap();
    assert_eq!(sel.urls[0].base, b"https://example.com/hybrid/app1/");
    let mut seen = Vec::new();
    for d in &app.descriptors {
        seen.push(d.tag());
        match d {
            AitDescriptor::Application(a) => {
                assert_eq!(a.profiles.len(), 1);
                assert_eq!(
                    (
                        a.profiles[0].profile,
                        a.profiles[0].major,
                        a.profiles[0].minor,
                        a.profiles[0].micro
                    ),
                    (2, 1, 1, 1)
                );
                assert!(a.service_bound);
                assert_eq!(a.visibility, 0b11);
                assert_eq!(a.transport_protocol_labels, vec![0]);
            }
            AitDescriptor::SimpleApplicationLocation(l) => {
                assert_eq!(l.initial_path, b"index.html")
            }
            AitDescriptor::BoundaryAndPermission(b) => {
                assert_eq!(b.entries[0].permission_bitmaps, vec![0x1FFF]);
                assert_eq!(
                    b.entries[0].managed_urls,
                    vec![b"https://example.com".to_vec()]
                );
            }
            x => panic!("{x:?}"),
        }
    }
    assert_eq!(seen, vec![0x00, 0x15, 0x30]);
    // the PMT says nothing about the AIT (it travels inside the BML component)
    let p = pmt(&r);
    assert_eq!(p.body.streams.len(), 2);
    no_validation_errors(&r.full, "hybridcast in carousel");
}

#[test]
fn hybridcast_ait_as_its_own_component() {
    let dir = tmpdir("hybridcast-es");
    let bml = dir.join("bml");
    std::fs::create_dir_all(&bml).unwrap();
    std::fs::write(bml.join("startup.bml"), STARTUP).unwrap();
    let hc = r#"
  [service.hybridcast]
  url = "http://example.com/app/"
  organization_id = 1
  application_id = 1
  control_code = "present"
  carriage = "es"
  pid = 0x0141
  data_component_id = 0x0010
  ait_version = 3
  application_priority = 1
  autostart_priority = 2
  boundaries = ["http://example.com/app/", "https://cdn.example.com"]
"#;
    let cfg = Config::from_toml(&config(hc, "source = \"bml\"", "")).unwrap();
    let r = run(&cfg, &dir, 20);
    let p = pmt(&r);
    let es = p
        .body
        .streams
        .iter()
        .find(|s| s.elementary_pid == 0x0141)
        .unwrap();
    assert_eq!(es.stream_type, 0x05);
    let Descriptor::DataComponent(dc) =
        Descriptor::decode_body(es.descriptors[0].tag, &es.descriptors[0].data).unwrap()
    else {
        panic!()
    };
    assert_eq!(dc.data_component_id, 0x0010);
    let ii = AitIdentifierInfo::decode(&dc.additional_data_component_info).unwrap();
    assert_eq!(ii.entries.len(), 1);
    assert_eq!(ii.entries[0].application_type, 0x0010);
    assert!(ii.entries[0].section_transport);
    assert_eq!(ii.entries[0].application_priority, 1);
    assert_eq!(ii.entries[0].ait_version, 3);
    let aits = sections(&r, 0x0141);
    assert!(aits.len() >= 4, "{}", aits.len());
    for w in aits.windows(2) {
        assert!(w[1].0 - w[0].0 <= HZ);
    }
    let ait = Ait::decode(&aits[0].1).unwrap();
    assert_eq!(ait.header.version_number, 3);
    let app = &ait.body.applications[0];
    assert_eq!(app.control_code, 0x02);
    assert!(
        app.descriptors
            .contains(&AitDescriptor::AutostartPriority { priority: 2 })
    );
    let AitDescriptor::SimpleApplicationLocation(l) = &app.descriptors[1] else {
        panic!()
    };
    assert_eq!(l.initial_path, b"");
    let AitDescriptor::BoundaryAndPermission(b) = &app.descriptors[2] else {
        panic!()
    };
    assert_eq!(b.entries[0].managed_urls.len(), 2);
    // the carousel has no AIT module and keeps the configured startup.bml
    let secs = sections(&r, 0x0140);
    let dii = Dii::decode(&secs.iter().find(|(_, s)| s[0] == 0x3B).unwrap().1).unwrap();
    assert_eq!(dii.body.modules.len(), 1);
    no_validation_errors(&r.full, "hybridcast es");
}

#[test]
fn explicit_modules_single_file_mapping_and_names() {
    let dir = tmpdir("bml-modules");
    let bml = dir.join("bml");
    std::fs::create_dir_all(&bml).unwrap();
    std::fs::write(bml.join("startup.bml"), STARTUP).unwrap();
    std::fs::write(bml.join("big.png"), vec![0xAB; 20_000]).unwrap();
    std::fs::write(bml.join("x.dat"), b"data").unwrap();
    let layout = r#"types = { dat = "application/X-arib-btable" }
    modules = [
      { id = 0x0000, files = ["startup.bml"], multipart = true },
      { id = 0x0001, files = ["big.png"] },
      { id = 0x0002, files = ["x.dat"], name = "table" },
    ]"#;
    let cfg = Config::from_toml(&config("", "source = \"bml\"", layout)).unwrap();
    let r = run(&cfg, &dir, 12);
    let secs = sections(&r, 0x0140);
    let dii = Dii::decode(&secs.iter().find(|(_, s)| s[0] == 0x3B).unwrap().1).unwrap();
    assert_eq!(dii.body.modules.len(), 3);
    let m1 = &dii.body.modules[1];
    assert_eq!(m1.module_size, 20_000);
    assert_eq!(
        m1.info,
        vec![
            ModuleDescriptor::Type {
                media_type: b"image/X-arib-png".to_vec()
            },
            ModuleDescriptor::Name {
                name: b"big.png".to_vec()
            },
        ]
    );
    let m2 = &dii.body.modules[2];
    assert_eq!(
        m2.info,
        vec![
            ModuleDescriptor::Type {
                media_type: b"application/X-arib-btable".to_vec()
            },
            ModuleDescriptor::Name {
                name: b"table".to_vec()
            },
        ]
    );
    let mods = modules(&secs, &dii);
    assert_eq!(mods[&1], vec![0xAB; 20_000]); // direct mapping: the file itself
    assert_eq!(mods[&2], b"data");
    assert_eq!(parse_multipart(&mods[&0]).unwrap()[0].name, "startup.bml");
    // a 20 000-byte module is 5 DDBs: section_number 0..4, last_section_number 4
    let ddb1: Vec<Ddb> = secs
        .iter()
        .filter(|(_, s)| s[0] == 0x3C)
        .map(|(_, s)| Ddb::decode(s).unwrap())
        .filter(|d| d.body.module_id == 1)
        .collect();
    assert!(ddb1.iter().all(|d| d.header.last_section_number == 4));
    no_validation_errors(&r.full, "explicit modules");
}

#[test]
fn examples_mux_and_validate() {
    let examples = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples");
    for name in ["bml-data.toml", "hybridcast.toml"] {
        let text = std::fs::read_to_string(examples.join(name)).unwrap();
        let cfg = Config::from_toml(&text).unwrap();
        let r = run(&cfg, &examples, 12);
        let secs = sections(&r, 0x0140);
        let dii = Dii::decode(&secs.iter().find(|(_, s)| s[0] == 0x3B).unwrap().1).unwrap();
        let mods = modules(&secs, &dii);
        let m0 = parse_multipart(&mods[&0]).unwrap();
        assert_eq!(m0[0].name, "startup.bml", "{name}");
        if name == "hybridcast.toml" {
            assert!(mods.contains_key(&0x9FFF));
            assert!(
                std::str::from_utf8(&m0[0].data)
                    .unwrap()
                    .contains("startAITControlledApp(1, 1,")
            );
        } else {
            // the example page is UTF-8 with Japanese text; the carousel
            // carries it transcoded to EUC-JP with the declaration rewritten
            let src = std::fs::read(examples.join("bml/startup.bml")).unwrap();
            assert_eq!(
                m0[0].data,
                isdbt_mux::carousel::transcode_utf8_to_euc_jp(&src, "example").unwrap()
            );
            assert!(
                m0[0]
                    .data
                    .starts_with(b"<?xml version=\"1.0\" encoding=\"EUC-JP\"?>")
            );
            assert!(std::str::from_utf8(&m0[0].data).is_err()); // EUC-JP bytes, not UTF-8
        }
        no_validation_errors(&r.full, name);
        // round-trips through TOML
        let again = Config::from_toml(&cfg.to_toml().unwrap()).unwrap();
        assert_eq!(
            again.services[0].streams.len(),
            cfg.services[0].streams.len()
        );
    }
}

#[test]
fn configuration_errors() {
    let dir = tmpdir("bml-errors");
    let bml = dir.join("bml");
    std::fs::create_dir_all(&bml).unwrap();
    std::fs::write(bml.join("startup.bml"), STARTUP).unwrap();
    std::fs::write(bml.join("x.unknown"), b"?").unwrap();
    let load_err = |text: &str| -> String {
        match Config::from_toml(text) {
            Err(e) => e.to_string(),
            Ok(cfg) => Multiplexer::new(&cfg, &bench_params(), &dir)
                .err()
                .map(|e| e.to_string())
                .unwrap_or_default(),
        }
    };
    // an unknown extension
    let e = load_err(&config("", "source = \"bml\"", ""));
    assert!(e.contains("x.unknown") && e.contains("Table 9-6"), "{e}");
    std::fs::remove_file(bml.join("x.unknown")).unwrap();
    // no startup.bml and no launcher
    let empty = dir.join("empty");
    std::fs::create_dir_all(&empty).unwrap();
    let e = load_err(&config("", "source = \"empty\"", ""));
    assert!(e.contains("startup.bml"), "{e}");
    // no source at all
    let e = load_err(&config("", "", ""));
    assert!(e.contains("source"), "{e}");
    // bml keys on another kind
    let e = load_err(&config("", "source = \"bml\"", "").replace(
        "kind = \"bml-carousel\"",
        "kind = \"declared\"\n  stream_type = 0x0D",
    ));
    assert!(e.contains("bml-carousel"), "{e}");
    // hybridcast without a carousel
    let e = load_err(
        &config(HYBRIDCAST, "source = \"bml\"", "")
            .replace(
                "kind = \"bml-carousel\"",
                "kind = \"declared\"\n  stream_type = 0x0D",
            )
            .replace("    [service.stream.bml]\n", ""),
    );
    assert!(e.contains("carriage") && e.contains("bml-carousel"), "{e}");
    // organization_id 0
    let e = load_err(&config(
        &HYBRIDCAST.replace("organization_id = 16", "organization_id = 0"),
        "source = \"bml\"",
        "",
    ));
    assert!(e.contains("organization_id 0"), "{e}");
    // bad URL scheme, prefetch, bad bitmap, es without pid
    let e = load_err(&config(
        &HYBRIDCAST.replace("https://", "ftp://"),
        "source = \"bml\"",
        "",
    ));
    assert!(e.contains("http"), "{e}");
    let e = load_err(&config(
        &format!("{HYBRIDCAST}  control_code = \"prefetch\"\n"),
        "source = \"bml\"",
        "",
    ));
    assert!(e.contains("PREFETCH"), "{e}");
    let e = load_err(&config(
        &format!("{HYBRIDCAST}  control_code = 1\n"),
        "source = \"bml\"",
        "",
    ));
    assert!(e.contains("autostart") && e.contains("named value"), "{e}");
    let e = load_err(&config(
        &format!("{HYBRIDCAST}  permission_bitmap = 0x2000\n"),
        "source = \"bml\"",
        "",
    ));
    assert!(e.contains("permission_bitmap"), "{e}");
    let e = load_err(&config(
        &format!("{HYBRIDCAST}  carriage = \"es\"\n"),
        "source = \"bml\"",
        "",
    ));
    assert!(e.contains("needs pid"), "{e}");
    let e = load_err(&config(
        &format!("{HYBRIDCAST}  carriage = \"es\"\n  pid = 0x0141\n"),
        "source = \"bml\"",
        "",
    ));
    assert!(e.contains("data_component_id"), "{e}");
    // AIT module colliding with a bml module
    let e = load_err(&config(
        &format!("{HYBRIDCAST}  module_id = 0\n"),
        "source = \"bml\"",
        "",
    ));
    assert!(e.contains("module_id"), "{e}");
    // block size, resolution name, duplicate raw descriptor
    let e = load_err(&config("", "source = \"bml\"", "block_size = 5000"));
    assert!(e.contains("block_size"), "{e}");
    let e = load_err(&config(
        "",
        "source = \"bml\"",
        "document_resolution = \"4k\"",
    ));
    assert!(e.contains("960x540"), "{e}");
    let e = load_err(&config(
        "",
        "source = \"bml\"\n  descriptors = [ { tag = 0xFD, hex = \"000c\" } ]",
        "",
    ));
    assert!(
        e.contains("0xfd") && e.contains("data_component_descriptor"),
        "{e}"
    );
    let e = load_err(&config("", "source = \"bml\"\n  stream_type = 0x06", ""));
    assert!(e.contains("0x0b") || e.contains("0x0B"), "{e}");
    // a module too large for 256 blocks
    std::fs::write(bml.join("huge.png"), vec![0u8; 256 * 4066 + 1]).unwrap();
    let e = load_err(&config("", "source = \"bml\"", ""));
    assert!(e.contains("256 blocks"), "{e}");
}

/// `bml.watch`: a file changed on disk is sent in the next cycles with
/// moduleVersion + 1 on its module and a new DII transaction number; an
/// unchanged carousel keeps its versions; a half-written file (seen in only
/// one poll) is not sent.
#[test]
fn watched_files_update_the_carousel_on_air() {
    let dir = tmpdir("bml-watch");
    let bml = dir.join("bml");
    std::fs::create_dir_all(&bml).unwrap();
    std::fs::write(bml.join("startup.bml"), STARTUP).unwrap();
    std::fs::write(bml.join("a.css"), "p { color-index: 7; }").unwrap();
    let cfg = Config::from_toml(&config(HYBRIDCAST, "source = \"bml\"", "")).unwrap();
    let params = bench_params();
    let mut mux = Multiplexer::new(&cfg, &params, &dir).unwrap();
    let n = mux.pattern().len();
    let run = |mux: &mut Multiplexer, frames: u64| -> Run {
        let mut full = Vec::new();
        let mut times = Vec::new();
        let k0 = mux.frames();
        for k in k0..k0 + frames {
            let (f, _) = mux.next_frame_full();
            times.extend((0..n).map(|j| mux.slot_time(k, j)));
            full.extend(f);
        }
        let report = analyze(&full, &times, clocks_to_ticks(408));
        Run {
            full,
            times,
            report,
            notes: Vec::new(),
        }
    };
    let versions = |secs: &[(u64, Vec<u8>)]| -> (Vec<u16>, Vec<(u16, u8)>) {
        let mut tx: Vec<u16> = secs
            .iter()
            .filter(|(_, s)| s[0] == 0x3B)
            .map(|(_, s)| Dii::decode(s).unwrap().header.table_id_extension)
            .collect();
        tx.sort();
        tx.dedup();
        let mut mv: Vec<(u16, u8)> = secs
            .iter()
            .filter(|(_, s)| s[0] == 0x3C)
            .map(|(_, s)| {
                let d = Ddb::decode(s).unwrap();
                (d.body.module_id, d.body.module_version)
            })
            .collect();
        mv.sort();
        mv.dedup();
        (tx, mv)
    };
    // 1. steady state: transaction 0, every module at version 0
    let r1 = run(&mut mux, 10);
    let (tx, mv) = versions(&sections(&r1, 0x0140));
    assert_eq!(tx, vec![0]);
    assert_eq!(mv, vec![(0x0000, 0), (0x9FFF, 0)]);
    assert!(mux.take_alerts().is_empty());
    // 2. a file changes: within a few seconds the carousel carries it with
    //    module 0 at version 1 and transaction 1; the AIT module keeps 0
    std::fs::write(
        bml.join("a.css"),
        "p { color-index: 7; background-color-index: 1; }",
    )
    .unwrap();
    let r2 = run(&mut mux, 14); // ≈ 3.2 s: two polls one second apart, then the rebuild
    let secs = sections(&r2, 0x0140);
    let (tx, mv) = versions(&secs);
    assert_eq!(tx, vec![0, 1], "DII transactions seen");
    assert!(
        mv.contains(&(0x0000, 1)) && mv.contains(&(0x9FFF, 0)),
        "{mv:?}"
    );
    let alerts = mux.take_alerts();
    assert!(
        alerts
            .iter()
            .any(|a| a.contains("rebuilt") && a.contains("0x0000 (version 1)")),
        "{alerts:?}"
    );
    // the new DII describes the new module; its DDBs reassemble to the new file
    let dii = secs
        .iter()
        .filter(|(_, s)| s[0] == 0x3B)
        .map(|(_, s)| Dii::decode(s).unwrap())
        .find(|d| d.header.table_id_extension == 1)
        .unwrap();
    assert_eq!(dii.body.transaction_id, 0x8000_0001);
    let m0 = dii.body.modules.iter().find(|m| m.module_id == 0).unwrap();
    assert_eq!(m0.module_version, 1);
    let new_secs: Vec<(u64, Vec<u8>)> = secs
        .iter()
        .filter(|(_, s)| {
            s[0] != 0x3C
                || Ddb::decode(s).unwrap().body.module_version == m0.module_version
                || Ddb::decode(s).unwrap().body.module_id != 0
        })
        .cloned()
        .collect();
    let mods = modules(&new_secs, &dii);
    let res = parse_multipart(&mods[&0]).unwrap();
    assert_eq!(res[1].name, "a.css");
    assert_eq!(
        res[1].data,
        b"p { color-index: 7; background-color-index: 1; }"
    );
    // 3. no further change: versions stay
    let r3 = run(&mut mux, 10);
    let (tx, mv) = versions(&sections(&r3, 0x0140));
    assert_eq!(tx, vec![1]);
    assert_eq!(mv, vec![(0x0000, 1), (0x9FFF, 0)]);
    // the whole run (steady, update, steady) is one continuous, valid stream
    let mut all = r1.full.clone();
    all.extend(r2.full.iter().cloned());
    all.extend(r3.full.iter().cloned());
    let r4 = run(&mut mux, 12); // past the 10 s NIT interval for the validator
    all.extend(r4.full.iter().cloned());
    no_validation_errors(&all, "across a watched update");
}

#[test]
fn watch_can_be_switched_off() {
    let dir = tmpdir("bml-nowatch");
    let bml = dir.join("bml");
    std::fs::create_dir_all(&bml).unwrap();
    std::fs::write(bml.join("startup.bml"), STARTUP).unwrap();
    let cfg = Config::from_toml(&config("", "source = \"bml\"", "watch = false")).unwrap();
    let mut mux = Multiplexer::new(&cfg, &bench_params(), &dir).unwrap();
    for _ in 0..8 {
        mux.next_frame();
    }
    std::fs::write(
        bml.join("startup.bml"),
        format!("{STARTUP}<!-- changed -->"),
    )
    .unwrap();
    let n = mux.pattern().len();
    let mut full = Vec::new();
    let mut times = Vec::new();
    for k in 8..22 {
        let (f, _) = mux.next_frame_full();
        times.extend((0..n).map(|j| mux.slot_time(k, j)));
        full.extend(f);
    }
    let r = Run {
        full,
        times,
        report: analyze(&[], &[], 1),
        notes: Vec::new(),
    };
    for (_, s) in sections(&r, 0x0140) {
        if s[0] == 0x3B {
            assert_eq!(Dii::decode(&s).unwrap().header.table_id_extension, 0);
        }
    }
    assert!(mux.take_alerts().is_empty());
}

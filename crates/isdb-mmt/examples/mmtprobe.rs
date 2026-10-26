//! Run the MMT decoder over a TLV stream file and print the tables,
//! assets and MFU statistics it finds, with the first NAL unit types or
//! bytes of each asset (development tool). Streams the file.
//!
//! cargo run --release -p isdb-mmt --example mmtprobe -- FILE

use isdb_mmt::Decoder;
use isdb_mmt::es;
use isdb_tlv::tlv::FileReader;
use std::collections::BTreeMap;

fn main() {
    let path = std::env::args().nth(1).expect("usage: mmtprobe FILE");
    let mut r = FileReader::new(std::io::BufReader::new(std::fs::File::open(&path).unwrap()));
    let mut d = Decoder::new();
    let mut mfus = Vec::new();
    let mut first: BTreeMap<u16, Vec<String>> = BTreeMap::new();
    while let Some(p) = r.next_packet().unwrap() {
        d.push(&p, &mut mfus);
        for m in mfus.drain(..) {
            let v = first.entry(m.packet_id).or_default();
            if v.len() < 6 {
                let types = es::nal_units(&m.data).map(|n| {
                    n.iter()
                        .filter_map(|x| es::hevc_nal_type(x))
                        .collect::<Vec<_>>()
                });
                let head: String = m.data.iter().take(10).map(|b| format!("{b:02x}")).collect();
                v.push(format!(
                    "mpu {} sample {:?} len {} nal {:?} [{head}]",
                    m.mpu_sequence_number,
                    m.header.map(|h| h.sample_number),
                    m.data.len(),
                    types
                ));
            }
        }
    }
    println!("{path}: {:?}", d.counters);
    println!(
        "descriptor loops that did not split: {}",
        isdb_mmt::desc::split_errors()
    );
    for (id, m) in &d.mpts {
        println!(
            "MPT on {id:#06x}: v{} service {:?} assets {}",
            m.version,
            m.service_id(),
            m.assets.len()
        );
        for a in &m.assets {
            println!(
                "  {} packet {:?} tag {:?} descriptors {:?}",
                a.type_str(),
                a.packet_id().map(|p| format!("{p:#06x}")),
                a.component_tag(),
                a.descriptors
                    .iter()
                    .map(|x| format!("{x:?}").chars().take(90).collect::<String>())
                    .collect::<Vec<_>>()
            );
        }
    }
    println!("PLT: {:?}", d.plt);
    for (k, s) in &d.sdts {
        for sv in &s.services {
            println!(
                "SDT {k:?} service {:#06x} {:?}",
                sv.service_id,
                sv.service()
            );
        }
    }
    for (k, e) in d.eits.iter().take(8) {
        for ev in e.events.iter().take(2) {
            println!(
                "EIT {k:?} event {:#06x} {:?} {:?} {:?}",
                ev.event_id,
                ev.start.to_jst().map(|t| t.to_string()),
                ev.duration.seconds(),
                ev.short_event().map(|(n, _)| n)
            );
        }
    }
    println!("EIT sections {}", d.eits.len());
    println!(
        "TOT {:?}",
        d.tot
            .as_ref()
            .and_then(|t| t.jst.to_jst())
            .map(|t| t.to_string())
    );
    println!("NTP {:?}", d.ntp.map(isdb_mmt::desc::ntp_to_unix));
    println!(
        "NIT {:?} AMT {:?} BIT {}",
        d.nits.len(),
        d.amts.len(),
        d.bits.len()
    );
    for n in d.nits.values() {
        println!(
            "  NIT network {:#06x} name {:?} streams {:?}",
            n.header.extension,
            n.descriptors.iter().find_map(|x| x.network_name()),
            n.streams
                .iter()
                .map(|s| (
                    format!("{:#06x}", s.tlv_stream_id),
                    s.descriptors
                        .iter()
                        .find_map(|x| x.satellite_frequency_khz()),
                    s.descriptors
                        .iter()
                        .find_map(|x| x.service_list())
                        .map(|l| l.len())
                ))
                .collect::<Vec<_>>()
        );
    }
    for a in d.amts.values() {
        for s in a.services.iter().take(4) {
            println!(
                "  AMT service {:#06x} src {:?} dst {:?}",
                s.service_id, s.src, s.dst
            );
        }
        println!("  AMT services {}", a.services.len());
    }
    println!("other tables {:?}", d.other_tables);
    for (id, s) in d.mpu_stats() {
        println!("asset {id:#06x}: {s:?}");
        for l in first.get(&id).into_iter().flatten() {
            println!("    {l}");
        }
    }
}

//! Trace the caption assets of a clear TLV stream, to see which clock their
//! documents run on: each `stpp` asset as the MPTs list it (with its Additional
//! Arib Subtitle Info, B60 Table 9-3, whose TMD names the clock, Table 9-7) and
//! each caption MFU with its arrival, the document's times, and two measures in
//! milliseconds: `late`, the arrival after reference + begin (`-` without a
//! reference in force), and `lead`, the presentation time of the service's video
//! MPU then arriving after the arrival. Last, the IP flows each caption packet id
//! came on. Times are the stream's NTP clock as JST (development tool).
//!
//! cargo run --release -p isdb-remux --example captiontrace -- FILE [--text]

use isdb_mmt::Decoder;
use isdb_mmt::desc::{Descriptor, tag};
use isdb_mmt::es::{Subtitle, SubtitleInfo};
use isdb_remux::timing::Clock;
use isdb_tlv::ip::{Flow, compressed, ipv4, ipv6};
use isdb_tlv::mmtp;
use isdb_tlv::tlv::{FileReader, PacketType};
use std::collections::{BTreeMap, BTreeSet};

/// NTP seconds at the Unix epoch.
const NTP_UNIX: u64 = 2_208_988_800;

/// An NTP timestamp as JST of day, to the millisecond.
fn jst(ntp: Option<u64>) -> String {
    let Some(ntp) = ntp else {
        return "--:--:--.---".to_owned();
    };
    let s = (ntp >> 32).saturating_sub(NTP_UNIX) + 9 * 3600;
    let ms = ((ntp & 0xffff_ffff) * 1000) >> 32;
    format!(
        "{:02}:{:02}:{:02}.{ms:03}",
        s / 3600 % 24,
        s / 60 % 60,
        s % 60
    )
}

/// Milliseconds of a document time as `h:mm:ss.fff`.
fn clock(ms: Option<i64>) -> String {
    let Some(ms) = ms else {
        return "-".to_owned();
    };
    format!(
        "{}:{:02}:{:02}.{:03}",
        ms / 3_600_000,
        ms / 60_000 % 60,
        ms / 1000 % 60,
        ms % 1000
    )
}

/// The MH-data coding system descriptor's body as Table 9-3.
fn subtitle_info(b: &[u8]) -> String {
    let Some(i) = SubtitleInfo::parse(b) else {
        let hex: String = b.iter().map(|x| format!("{x:02x}")).collect();
        return format!("not captions {hex}");
    };
    format!(
        "tag {:#04x} info_version {} lang {} type {} format {} OPM {} TMD {:04b} DMF {:04b} resolution {} compression {} start_mpu {:?} reference {}",
        i.tag,
        i.version,
        String::from_utf8_lossy(&i.language),
        i.kind,
        i.format,
        i.operation_mode,
        i.time_control,
        i.display_mode,
        i.resolution,
        i.compression,
        i.start_mpu,
        i.reference_start_time
            .map_or("-".to_owned(), |t| jst(Some(t))),
    )
}

/// `a - b` in milliseconds on the NTP ring.
fn ms_between(a: u64, b: u64) -> i64 {
    #[allow(clippy::cast_possible_wrap, reason = "a difference on the NTP ring")]
    let d = a.wrapping_sub(b) as i64;
    i64::try_from((i128::from(d) * 1000) >> 32).unwrap_or(0)
}

/// One `stpp` asset as a line, without what moves with every MPT copy.
fn asset_line(service: Option<u16>, a: &isdb_mmt::table::Asset) -> String {
    let info: Vec<String> = a
        .descriptors
        .iter()
        .filter_map(|d| match d {
            Descriptor::Other(r) if r.tag == tag::DATA_CODING => Some(subtitle_info(&r.body)),
            _ => None,
        })
        .collect();
    format!(
        "service {:#06x} packet {:#06x} component {:?} locations {} | {}",
        service.unwrap_or(0),
        a.packet_id().unwrap_or(0),
        a.component_tag(),
        a.locations.len(),
        info.join(" ; ")
    )
}

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("usage: captiontrace FILE [--text]");
    let text = args.next().as_deref() == Some("--text");
    let f = std::fs::File::open(&path).expect("the input");
    let mut r = FileReader::new(std::io::BufReader::new(f));
    let mut d = Decoder::new();
    let mut mfus = Vec::new();
    let mut listed: BTreeMap<(u16, Option<u16>), String> = BTreeMap::new();
    let mut flows: BTreeMap<u16, BTreeSet<Option<Flow>>> = BTreeMap::new();
    let mut tables = 0;
    let mut clocks: BTreeMap<u16, (u8, Option<u64>)> = BTreeMap::new();
    let mut video_times: BTreeMap<u16, BTreeMap<u32, u64>> = BTreeMap::new();
    let mut lead: BTreeMap<Option<u16>, i64> = BTreeMap::new();
    println!("arrival service packet mpu tmd tag seq sub/last type late lead begin end lines");
    while let Some(p) = r.next_packet().expect("the read") {
        let dg = match p.kind {
            PacketType::Ipv4 => ipv4(&p.data).ok(),
            PacketType::Ipv6 => ipv6(&p.data).ok(),
            PacketType::Compressed => compressed(&p.data).ok(),
            _ => None,
        };
        let header = dg
            .as_ref()
            .and_then(|g| Some((g, mmtp::parse(&g.payload)?)));
        d.push(&p, &mut mfus);
        if let Some((g, (h, _))) = header
            && d.asset_of(h.packet_id)
                .is_some_and(|(_, a)| a.asset_type == *b"stpp")
        {
            flows.entry(h.packet_id).or_default().insert(g.flow.clone());
        }
        if d.counters.tables != tables {
            tables = d.counters.tables;
            for m in d.mpts.values() {
                let service = m.service_id();
                for a in m
                    .assets
                    .iter()
                    .filter(|a| matches!(&a.asset_type, b"hev1" | b"hvc1"))
                {
                    for x in &a.descriptors {
                        if let (Descriptor::MpuTimestamp(rows), Some(pid)) = (x, a.packet_id()) {
                            let t = video_times.entry(pid).or_default();
                            for row in rows {
                                t.insert(row.mpu_sequence_number, row.presentation_time);
                            }
                            while t.len() > 4096 {
                                t.pop_first();
                            }
                        }
                    }
                }
                for a in m.assets.iter().filter(|a| a.asset_type == *b"stpp") {
                    let pid = a.packet_id().unwrap_or(0);
                    let line = asset_line(service, a);
                    if listed.get(&(pid, service)) != Some(&line) {
                        println!("{} MPT v{} {line}", jst(d.ntp), m.version);
                        listed.insert((pid, service), line);
                    }
                    for x in &a.descriptors {
                        if let Descriptor::Other(r) = x
                            && r.tag == tag::DATA_CODING
                            && let Some(i) = SubtitleInfo::parse(&r.body)
                        {
                            clocks.insert(pid, (i.time_control, i.reference_start_time));
                        }
                    }
                }
            }
        }
        for m in mfus.drain(..) {
            let Some((service, asset)) = d.asset_of(m.packet_id) else {
                continue;
            };
            if matches!(&asset.asset_type, b"hev1" | b"hvc1") {
                if let (Some(now), Some(pt)) = (
                    d.ntp,
                    video_times
                        .get(&m.packet_id)
                        .and_then(|t| t.get(&m.mpu_sequence_number)),
                ) {
                    lead.insert(service, ms_between(*pt, now));
                }
                continue;
            }
            if asset.asset_type != *b"stpp" {
                continue;
            }
            let (tmd, reference) = clocks.get(&m.packet_id).copied().unwrap_or((0xff, None));
            let lead_now = lead
                .get(&service)
                .map_or("-".to_owned(), ToString::to_string);
            let head = format!(
                "{} {:#06x} {:#06x} {} {tmd:04b}",
                jst(d.ntp),
                service.unwrap_or(0),
                m.packet_id,
                m.mpu_sequence_number,
            );
            let Some(s) = Subtitle::parse(&m.data) else {
                println!("{head} unparsed {} bytes", m.data.len());
                continue;
            };
            let sub = format!(
                "{:#04x} {} {}/{} {}",
                s.tag, s.sequence_number, s.subsample_number, s.last_subsample_number, s.data_type
            );
            if s.data_type != 0 {
                println!("{head} {sub} - {lead_now} ({} bytes)", s.data.len());
                continue;
            }
            match isdb_caption::parse(s.data) {
                Ok(doc) if doc.blocks.is_empty() => {
                    println!("{head} {sub} - {lead_now} no blocks");
                }
                Ok(doc) => {
                    for b in &doc.blocks {
                        let lines = if text {
                            b.lines.join(" / ")
                        } else {
                            b.lines.len().to_string()
                        };
                        let late = match (d.ntp, reference, b.begin) {
                            (Some(now), Some(r), Some(begin)) => {
                                let here = Clock { ticks: 0, ntp: now };
                                (-here.ticks_after(r, begin) / 90).to_string()
                            }
                            _ => "-".to_owned(),
                        };
                        println!(
                            "{head} {sub} {late} {lead_now} {} {} {lines}",
                            clock(b.begin),
                            clock(b.end)
                        );
                    }
                }
                Err(e) => println!("{head} {sub} rejected: {e}"),
            }
        }
    }
    println!("{} end, {} NTP packets", jst(d.ntp), d.counters.ntp);
    for (pid, f) in &flows {
        println!("packet {pid:#06x}: {} flow(s) {f:?}", f.len());
    }
}

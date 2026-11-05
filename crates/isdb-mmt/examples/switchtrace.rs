//! Trace what changes in a service's stream structure, to find where a
//! programme really switches: MPT versions with their assets and component
//! descriptors, steps in the MPU timestamps and rows, each timed asset's
//! starts, stops and MPU jumps, audio configuration changes, and HEVC parameter
//! set changes, IRAP types other than CRA and end-of-sequence NAL units. Times
//! are the stream's NTP clock (arrival) and the MPU's presentation time, as JST
//! (development tool, clear TLV input).
//!
//! cargo run --release -p isdb-mmt --example switchtrace -- FILE SERVICE_ID

use isdb_mmt::Decoder;
use isdb_mmt::desc::Descriptor;
use isdb_mmt::es;
use std::collections::BTreeMap;

/// NTP seconds at the Unix epoch.
const NTP_UNIX: u64 = 2_208_988_800;

/// An NTP timestamp as JST of day, to the millisecond.
fn jst(ntp: Option<u64>) -> String {
    let Some(ntp) = ntp else {
        return "--:--:--.---".to_owned();
    };
    let s = (ntp >> 32) - NTP_UNIX + 9 * 3600;
    let ms = ((ntp & 0xffff_ffff) * 1000) >> 32;
    format!(
        "{:02}:{:02}:{:02}.{ms:03}",
        s / 3600 % 24,
        s / 60 % 60,
        s % 60
    )
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// One asset as a line: its type, packet id, tag and component descriptors.
fn asset_line(a: &isdb_mmt::table::Asset) -> String {
    let parts: Vec<String> = a
        .descriptors
        .iter()
        .filter_map(|d| match d {
            Descriptor::Other(r) if matches!(r.tag, 0x8010 | 0x8014) => {
                Some(format!("{:#06x}={}", r.tag, hex(&r.body)))
            }
            _ => None,
        })
        .collect();
    format!(
        "{} {:#06x} tag {:?} {}",
        a.type_str(),
        a.packet_id().unwrap_or(0),
        a.component_tag(),
        parts.join(" ")
    )
}

/// An MPU's row: its decoding time offset, AU count and AU offsets.
type Row = (u32, usize, Vec<(u16, Option<u32>)>);

/// What one timed asset has shown so far.
#[derive(Default)]
struct Timed {
    /// MPU → presentation time.
    pt: BTreeMap<u32, u64>,
    /// The last step between consecutive MPUs' presentation times.
    step: Option<i64>,
    /// The last row's (decoding time offset, AU count, offsets).
    row: Option<Row>,
    /// The MPUs whose rows were seen.
    rows_seen: std::collections::BTreeSet<u32>,
    /// The last MFU's MPU and arrival.
    last: Option<(u32, u64)>,
    /// The audio configuration in force.
    config: Option<Vec<u8>>,
    /// The parameter sets in force, by NAL type.
    sets: BTreeMap<u8, Vec<u8>>,
    /// The first MPT copy naming it was read.
    listed: bool,
}

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("usage: switchtrace FILE SERVICE_ID");
    let service = args
        .next()
        .and_then(|s| u16::from_str_radix(s.trim_start_matches("0x"), 16).ok())
        .expect("SERVICE_ID in hex");
    let f = std::fs::File::open(&path).unwrap();
    let mut r = isdb_tlv::tlv::FileReader::new(std::io::BufReader::new(f));
    let mut d = Decoder::new();
    let mut mfus = Vec::new();
    let mut assets: BTreeMap<u16, Timed> = BTreeMap::new();
    let mut listed: Vec<String> = Vec::new();
    let mut version = None;
    let mut tables = 0;
    while let Some(p) = r.next_packet().unwrap() {
        d.push(&p, &mut mfus);
        let now = d.ntp;
        if d.counters.tables != tables {
            tables = d.counters.tables;
            if let Some(mpt) = d.mpts.values().find(|m| m.service_id() == Some(service)) {
                let lines: Vec<String> = mpt.assets.iter().map(asset_line).collect();
                if Some(mpt.version) != version || lines != listed {
                    println!(
                        "{} MPT version {} ({} assets)",
                        jst(now),
                        mpt.version,
                        lines.len()
                    );
                    for l in lines.iter().filter(|l| !listed.contains(l)) {
                        println!("    + {l}");
                    }
                    for l in listed.iter().filter(|l| !lines.contains(l)) {
                        println!("    - {l}");
                    }
                    for a in &mpt.assets {
                        for x in &a.descriptors {
                            if let Descriptor::MpuTimestamp(rows) = x
                                && matches!(a.type_str().as_str(), "hev1" | "mp4a")
                            {
                                let cover: Vec<String> = rows
                                    .iter()
                                    .map(|r| {
                                        format!(
                                            "{} {}",
                                            r.mpu_sequence_number,
                                            jst(Some(r.presentation_time))
                                        )
                                    })
                                    .collect();
                                println!(
                                    "    {:#06x} covers {}",
                                    a.packet_id().unwrap_or(0),
                                    cover.join(", ")
                                );
                            }
                        }
                    }
                    println!("    MPT descriptors {:?}", mpt.descriptors);
                    version = Some(mpt.version);
                    listed = lines;
                }
                for a in &mpt.assets {
                    let Some(pid) = a.packet_id() else { continue };
                    let kind = a.type_str();
                    if !matches!(kind.as_str(), "hev1" | "hvc1" | "mp4a") {
                        continue;
                    }
                    let t = assets.entry(pid).or_default();
                    t.listed = true;
                    for x in &a.descriptors {
                        match x {
                            Descriptor::MpuTimestamp(rows) => {
                                for row in rows {
                                    let seq = row.mpu_sequence_number;
                                    if t.pt.insert(seq, row.presentation_time)
                                        == Some(row.presentation_time)
                                    {
                                        continue;
                                    }
                                    let Some(&prev) = t.pt.get(&seq.wrapping_sub(1)) else {
                                        continue;
                                    };
                                    let step =
                                        (row.presentation_time as i64).wrapping_sub(prev as i64);
                                    let close = t.step.is_some_and(|s| {
                                        (i128::from(s) - i128::from(step)).unsigned_abs()
                                            < (1u128 << 32) / 1000
                                    });
                                    if t.step.is_some() && !close {
                                        println!(
                                            "{} {pid:#06x} {kind} MPU {seq} PT {} steps {:.6} s (was {:.6} s)",
                                            jst(now),
                                            jst(Some(row.presentation_time)),
                                            step as f64 / 4_294_967_296.0,
                                            t.step.unwrap_or(0) as f64 / 4_294_967_296.0
                                        );
                                    }
                                    t.step = Some(step);
                                }
                            }
                            Descriptor::ExtendedTimestamp(e) => {
                                for m in &e.mpus {
                                    let row = (
                                        m.decoding_time_offset.into(),
                                        m.access_units.len(),
                                        m.access_units
                                            .iter()
                                            .map(|&(a, b)| (a, b.map(u32::from)))
                                            .collect::<Vec<_>>(),
                                    );
                                    let new = t.rows_seen.insert(m.mpu_sequence_number);
                                    if new && t.row.as_ref() != Some(&row) {
                                        println!(
                                            "{} {pid:#06x} {kind} MPU {} row: decoding offset {} AUs {} first {:?}",
                                            jst(now),
                                            m.mpu_sequence_number,
                                            row.0,
                                            row.1,
                                            &row.2[..row.2.len().min(8)]
                                        );
                                        t.row = Some(row);
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                    while t.pt.len() > 4096 {
                        t.pt.pop_first();
                    }
                }
            }
        }
        for m in mfus.drain(..) {
            let Some(t) = assets.get_mut(&m.packet_id) else {
                continue;
            };
            let pid = m.packet_id;
            let seq = m.mpu_sequence_number;
            let pt = t.pt.get(&seq).copied();
            let arrival = now.unwrap_or(0);
            match t.last {
                None => println!(
                    "{} {pid:#06x} starts at MPU {seq} (PT {})",
                    jst(now),
                    jst(pt)
                ),
                Some((l, at)) => {
                    if arrival.wrapping_sub(at) >> 32 >= 2 {
                        println!(
                            "{} {pid:#06x} back after {} s silent: MPU {l} → {seq} (PT {})",
                            jst(now),
                            arrival.wrapping_sub(at) >> 32,
                            jst(pt)
                        );
                    } else if seq != l && seq != l.wrapping_add(1) {
                        println!(
                            "{} {pid:#06x} MPU jump {l} → {seq} (PT {}; PT of {l} {})",
                            jst(now),
                            jst(pt),
                            jst(t.pt.get(&l).copied())
                        );
                    }
                }
            }
            t.last = Some((seq, arrival));
            if let Some(frame) = es::latm_frame(&m.data) {
                if let Some(c) = frame.config
                    && t.config.as_ref() != Some(&c)
                {
                    println!(
                        "{} {pid:#06x} audio config {} at MPU {seq} (PT {})",
                        jst(now),
                        hex(&c),
                        jst(pt)
                    );
                    t.config = Some(c);
                }
                continue;
            }
            for nal in es::nal_units(&m.data).unwrap_or_default() {
                let Some(ty) = es::hevc_nal_type(nal) else {
                    continue;
                };
                match ty {
                    32..=33 => {
                        if t.sets.get(&ty).is_some_and(|s| s != nal) {
                            println!(
                                "{} {pid:#06x} NAL {ty} changes at MPU {seq} (PT {})",
                                jst(now),
                                jst(pt)
                            );
                        }
                        t.sets.insert(ty, nal.to_vec());
                    }
                    16..=20 | 36 | 37 => println!(
                        "{} {pid:#06x} NAL {ty} at MPU {seq} (PT {})",
                        jst(now),
                        jst(pt)
                    ),
                    _ => {}
                }
            }
        }
    }
    println!("{} end", jst(d.ntp));
    for (pid, t) in &assets {
        println!(
            "{pid:#06x}: last MPU {:?} (PT {}), arrived {}",
            t.last.map(|l| l.0),
            jst(t.last.and_then(|l| t.pt.get(&l.0).copied())),
            jst(t.last.map(|l| l.1))
        );
    }
}

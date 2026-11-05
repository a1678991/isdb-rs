//! Trace how a service's MH-EIT changes while a stream runs, next to the
//! MPT versions and the MPU sequence jumps of its assets: when (on the
//! stream's NTP clock, as JST) each event's p/f or schedule entry appeared,
//! changed or left its table (development tool, TLV input; what the Matroska
//! chapters of `isdb-remux` are chosen from).
//!
//! cargo run --release -p isdb-mmt --example eittrace -- FILE SERVICE_ID

use isdb_mmt::Decoder;
use std::collections::{BTreeMap, BTreeSet};

/// NTP seconds at the Unix epoch.
const NTP_UNIX: u64 = 2_208_988_800;

/// The time of an NTP timestamp, as JST of day.
fn jst(ntp: Option<u64>) -> String {
    let Some(ntp) = ntp else {
        return "--:--:--.-".to_owned();
    };
    let s = (ntp >> 32) - NTP_UNIX + 9 * 3600;
    let tenths = ((ntp & 0xffff_ffff) * 10) >> 32;
    format!(
        "{:02}:{:02}:{:02}.{tenths}",
        s / 3600 % 24,
        s / 60 % 60,
        s % 60
    )
}

/// What an entry says, as printed.
fn entry(e: &isdb_mmt::table::Event) -> String {
    let start = e
        .start
        .to_jst()
        .map_or_else(|| "undefined".to_owned(), |t| t.to_string());
    let duration = e
        .duration
        .seconds()
        .map_or_else(|| "undefined".to_owned(), |s| format!("{s} s"));
    let name = e.short_event().map_or("(no name)", |(n, _)| n);
    format!("{start} {duration} running {} \"{name}\"", e.running_status)
}

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("usage: eittrace FILE SERVICE_ID");
    let service = args
        .next()
        .and_then(|s| u16::from_str_radix(s.trim_start_matches("0x"), 16).ok())
        .expect("SERVICE_ID in hex");
    let f = std::fs::File::open(&path).unwrap();
    let mut r = isdb_tlv::tlv::FileReader::new(std::io::BufReader::new(f));
    let mut d = Decoder::new();
    let mut mfus = Vec::new();
    // What each table and section held at the last look.
    let mut held: BTreeMap<(u8, u16, u8), BTreeMap<u16, String>> = BTreeMap::new();
    let mut versions: BTreeMap<(u8, u16, u8), u8> = BTreeMap::new();
    let mut mpt_version = None;
    let mut last_mpu: BTreeMap<u16, u32> = BTreeMap::new();
    let mut names: BTreeSet<u16> = BTreeSet::new();
    let (mut seen_tables, mut seen_changes) = (0, 0);
    while let Some(p) = r.next_packet().unwrap() {
        d.push(&p, &mut mfus);
        for m in mfus.drain(..) {
            let last = last_mpu.insert(m.packet_id, m.mpu_sequence_number);
            let kind = d.asset_of(m.packet_id).map(|(_, a)| a.type_str());
            // A timed asset's MPU numbers step by one.
            if matches!(kind.as_deref(), Some("hev1" | "mp4a"))
                && last.is_some_and(|l| {
                    m.mpu_sequence_number != l && m.mpu_sequence_number != l.wrapping_add(1)
                })
            {
                println!(
                    "{} MPU jump {:#06x}: {} → {}",
                    jst(d.ntp),
                    m.packet_id,
                    last.unwrap_or(0),
                    m.mpu_sequence_number
                );
            }
            names.insert(m.packet_id);
        }
        let v = d
            .mpts
            .values()
            .find(|m| m.service_id() == Some(service))
            .map(|m| m.version);
        if v != mpt_version {
            println!("{} MPT version {v:?}", jst(d.ntp));
            mpt_version = v;
        }
        if d.eits.len() == seen_tables && d.counters.version_changes == seen_changes {
            continue;
        }
        seen_tables = d.eits.len();
        seen_changes = d.counters.version_changes;
        for (&key, eit) in d.eits.iter().filter(|(k, _)| k.1 == service) {
            if versions.insert(key, eit.header.version) == Some(eit.header.version) {
                continue;
            }
            let table = if key.0 == 0x8B { "p/f" } else { "sch" };
            let now: BTreeMap<u16, String> =
                eit.events.iter().map(|e| (e.event_id, entry(e))).collect();
            let was = held.entry(key).or_default();
            for (id, text) in &now {
                if was.get(id) != Some(text) {
                    println!(
                        "{} {table} {:#04x}/{} v{} event {id:#06x}: {text}",
                        jst(d.ntp),
                        key.0,
                        key.2,
                        eit.header.version
                    );
                }
            }
            for id in was.keys().filter(|id| !now.contains_key(id)) {
                println!(
                    "{} {table} {:#04x}/{} v{} event {id:#06x} left the section",
                    jst(d.ntp),
                    key.0,
                    key.2,
                    eit.header.version
                );
            }
            *was = now;
        }
    }
    println!("{} end", jst(d.ntp));
    println!("assets with whole MFUs: {names:04x?}");
}

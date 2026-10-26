//! `isdb tlv`: the content of a TLV stream file (TLV packets back to back,
//! as `isdb rx --system isdbs3` writes them), read a chunk at a time:
//!
//! - `inspect`: transport counters (packets by type, IP, MMTP ids), the
//!   TLV-SI and MMT-SI tables decoded (network, TLV streams, services,
//!   events, packages and their assets, time), and per asset the MPU and
//!   packet continuity and the share of scrambled payloads;
//! - `extract`: one elementary stream per asset (HEVC Annex B, LOAS audio,
//!   caption files); scrambled payloads are skipped, or written as they
//!   are (still encrypted) with `--write-scrambled`. Nothing is ever
//!   descrambled;
//! - `validate`: structural checks (CRCs, table structure, descriptor
//!   loops, packet and MPU continuity, assets without packets, packets of
//!   no asset).

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};
use isdb_mmt::Decoder;
use isdb_mmt::desc::{Descriptor, ntp_to_unix, split_errors};
use isdb_mmt::es;
use isdb_mmt::mpu::Mfu;
use isdb_mmt::table::{Asset, Location};
use isdb_tlv::stats::StreamStats;
use isdb_tlv::tlv::FileReader;
use isdb_tlv::{mmtp, si};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

/// `isdb tlv` arguments.
#[derive(Debug, Args)]
pub struct TlvArgs {
    #[command(subcommand)]
    cmd: TlvCmd,
}

#[derive(Debug, Subcommand)]
enum TlvCmd {
    /// Summarise a TLV stream file: transport, tables, services, events,
    /// assets.
    Inspect(InspectArgs),
    /// Write each asset's elementary stream.
    Extract(ExtractArgs),
    /// Structural checks of a TLV stream file.
    Validate(InspectArgs),
}

#[derive(Debug, Args)]
struct InspectArgs {
    /// The TLV stream file.
    file: PathBuf,
    /// Print JSON.
    #[arg(long)]
    json: bool,
    /// Events listed per service (inspect).
    #[arg(long, default_value_t = 4)]
    events: usize,
}

#[derive(Debug, Args)]
struct ExtractArgs {
    /// The TLV stream file.
    file: PathBuf,
    /// Directory for the elementary streams (created; files are named
    /// SERVICE-PACKETID.EXT).
    #[arg(long)]
    out_dir: PathBuf,
    /// Only the assets of this service id (`0x0097` or decimal).
    #[arg(long, value_parser = parse_u16)]
    service: Option<u16>,
    /// Only the asset on this MMTP packet id (repeatable).
    #[arg(long, value_parser = parse_u16)]
    asset: Vec<u16>,
    /// Also write scrambled payloads, as they are (still encrypted, one
    /// `.scrambled` file per asset: the payloads after their clear 8-byte
    /// header, back to back). Nothing is descrambled.
    #[arg(long)]
    write_scrambled: bool,
    /// Replace existing files.
    #[arg(long)]
    overwrite: bool,
}

fn parse_u16(s: &str) -> Result<u16> {
    Ok(
        match s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
            Some(h) => u16::from_str_radix(h, 16)?,
            None => s.parse()?,
        },
    )
}

/// Read a TLV stream file through the transport counters and the MMT
/// decoder; `on_mfu` sees every whole MFU.
fn read(
    path: &Path,
    pass_scrambled: bool,
    mut on_mfu: impl FnMut(&Decoder, Mfu) -> Result<()>,
) -> Result<(StreamStats, Decoder, u64, u64)> {
    let f = std::fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut r = FileReader::new(std::io::BufReader::new(f));
    let mut st = StreamStats::default();
    let mut d = Decoder::new();
    d.set_pass_scrambled(pass_scrambled);
    let mut mfus = Vec::new();
    let mut n = 0u64;
    while let Some(p) = r
        .next_packet()
        .with_context(|| format!("read {}", path.display()))?
    {
        n += 1;
        st.add(&p);
        d.push(&p, &mut mfus);
        for m in mfus.drain(..) {
            on_mfu(&d, m)?;
        }
    }
    if n == 0 {
        bail!("{}: no TLV packet", path.display());
    }
    Ok((st, d, n, r.skipped))
}

/// Run `isdb tlv`.
pub fn run(a: &TlvArgs) -> Result<()> {
    match &a.cmd {
        TlvCmd::Inspect(a) => inspect(a),
        TlvCmd::Extract(a) => extract(a),
        TlvCmd::Validate(a) => validate(a),
    }
}

fn hex16(v: u16) -> String {
    format!("{v:#06x}")
}

fn location(l: &Location) -> String {
    match l {
        Location::SameFlow(p) => format!("packet {p:#06x}"),
        Location::Ipv4 {
            packet_id, port, ..
        }
        | Location::Ipv6 {
            packet_id, port, ..
        } => {
            format!("packet {packet_id:#06x} (other IP flow, port {port})")
        }
        Location::Mpeg2Ts { ts_id, pid, .. } => format!("TS {ts_id:#06x} PID {pid:#06x}"),
        Location::Mpeg2TsIpv6 { pid, .. } => format!("TS over IPv6, PID {pid:#06x}"),
        Location::Url(u) => format!("URL {u}"),
    }
}

/// What an asset is, from its type and descriptors.
fn asset_json(d: &Decoder, a: &Asset) -> Value {
    let pid = a.packet_id();
    let stats = pid.and_then(|p| d.mpu_stats().find(|(k, _)| *k == p).map(|(_, s)| s.clone()));
    let mut extra = serde_json::Map::new();
    for x in &a.descriptors {
        match x {
            Descriptor::Hevc {
                profile_idc,
                tier,
                level_idc,
            } => {
                extra.insert(
                    "hevc".into(),
                    json!({"profile_idc": profile_idc, "high_tier": tier, "level_idc": level_idc}),
                );
            }
            Descriptor::AccessControl { ca_system_id, .. } => {
                extra.insert("ca_system_id".into(), json!(ca_system_id));
            }
            Descriptor::Scrambler {
                layer_type,
                system_id,
            } => {
                extra.insert(
                    "scrambler".into(),
                    json!({"layer_type": layer_type, "system_id": system_id}),
                );
            }
            Descriptor::MpuTimestamp(t) => {
                extra.insert(
                    "mpu_timestamps".into(),
                    json!(t.iter().map(|x| json!({"mpu": x.mpu_sequence_number, "unix_time": ntp_to_unix(x.presentation_time)})).collect::<Vec<_>>()),
                );
            }
            Descriptor::ExtendedTimestamp(e) => {
                extra.insert(
                    "extended_timestamp".into(),
                    json!({"timescale": e.timescale, "default_pts_offset": e.default_pts_offset, "mpus": e.mpus.iter().map(|m| json!({"mpu": m.mpu_sequence_number, "decoding_time_offset": m.decoding_time_offset, "access_units": m.access_units.len()})).collect::<Vec<_>>()}),
                );
            }
            _ => {}
        }
    }
    json!({
        "asset_type": a.type_str(),
        "packet_id": pid.map(hex16),
        "locations": a.locations.iter().map(location).collect::<Vec<_>>(),
        "component_tag": a.component_tag(),
        "descriptors": a.descriptors.iter().map(|x| match x {
            Descriptor::Other(r) => format!("{:#06x}", r.tag),
            o => format!("{o:?}").split([' ', '(', '{']).next().unwrap_or_default().to_string(),
        }).collect::<Vec<_>>(),
        "detail": extra,
        "mpu": stats.map(|s| json!({
            "packets": s.packets,
            "packets_lost": s.packets_lost,
            "scrambled_packets": s.scrambled_packets,
            "mfus": s.mfus,
            "scrambled_mfus": s.scrambled_mfus,
            "mfus_dropped": s.mfus_dropped,
            "mpus": s.mpus,
            "mpu_sequence_jumps": s.mpu_sequence_jumps,
            "mpu_range": s.mpu_range,
            "malformed": s.malformed,
        })),
    })
}

fn report(
    path: &Path,
    st: &StreamStats,
    d: &Decoder,
    n: u64,
    skipped: u64,
    events: usize,
) -> Value {
    let services: BTreeMap<u16, Value> = d
        .sdts
        .values()
        .flat_map(|s| {
            s.services.iter().map(move |sv| {
                let (t, p, name) = sv.service().unwrap_or((0, "", ""));
                (
                    sv.service_id,
                    json!({
                        "service_id": hex16(sv.service_id),
                        "sdt": if s.header.table_id == 0x9F { "actual" } else { "other" },
                        "tlv_stream_id": hex16(s.header.extension),
                        "original_network_id": hex16(s.original_network_id),
                        "service_type": t,
                        "provider": p,
                        "name": name,
                        "running_status": sv.running_status,
                        "free_ca_mode": sv.free_ca,
                    }),
                )
            })
        })
        .collect();
    let mut by_service: BTreeMap<u16, Vec<Value>> = BTreeMap::new();
    for e in d.eits.values() {
        for ev in &e.events {
            by_service
                .entry(e.header.extension)
                .or_default()
                .push(json!({
                    "table": if e.header.table_id == 0x8B { "p/f" } else { "schedule" },
                    "event_id": hex16(ev.event_id),
                    "start_jst": ev.start.to_jst().map(|t| t.to_string()),
                    "duration_s": ev.duration.seconds(),
                    "running_status": ev.running_status,
                    "free_ca_mode": ev.free_ca,
                    "name": ev.short_event().map(|x| x.0),
                    "genre": ev.descriptors.iter().find_map(|x| match x {
                        Descriptor::Content(c) => c.first().map(|g| format!("{:x}/{:x}", g.0, g.1)),
                        _ => None,
                    }),
                }));
        }
    }
    for v in by_service.values_mut() {
        v.sort_by(|a, b| a["start_jst"].as_str().cmp(&b["start_jst"].as_str()));
        v.dedup_by(|a, b| a["event_id"] == b["event_id"]);
    }
    let packages: Vec<Value> = d
        .mpts
        .iter()
        .map(|(pid, m)| {
            json!({
                "pa_packet_id": hex16(*pid),
                "service_id": m.service_id().map(hex16),
                "mpt_version": m.version,
                "assets": m.assets.iter().map(|a| asset_json(d, a)).collect::<Vec<_>>(),
            })
        })
        .collect();
    let orphan: Vec<Value> = d
        .mpu_stats()
        .filter(|(p, _)| d.asset_of(*p).is_none())
        .map(|(p, s)| json!({"packet_id": hex16(p), "packets": s.packets, "scrambled_packets": s.scrambled_packets}))
        .collect();
    let (mpu_packets, scrambled) = d.mpu_stats().fold((0u64, 0u64), |a, (_, s)| {
        (a.0 + s.packets, a.1 + s.scrambled_packets)
    });
    json!({
        "file": path.display().to_string(),
        "tlv_packets": n,
        "bytes_skipped": skipped,
        "transport": {
            "by_type": st.by_type.iter().map(|(k, (n, b))| (k.clone(), json!({"packets": n, "bytes": b}))).collect::<serde_json::Map<_, _>>(),
            "ip_errors": st.ip_errors,
            "mmt_si_messages": st.messages.iter().map(|(k, n)| (format!("{k:#06x} {}", mmtp::message_name(*k)), json!(n))).collect::<serde_json::Map<_, _>>(),
            "mmt_si_tables": st.mmt_tables.iter().map(|(k, n)| (format!("{k:#04x} {}", mmtp::table_name(*k)), json!(n))).collect::<serde_json::Map<_, _>>(),
            "tlv_si_tables": st.tlv_si.iter().map(|(k, n)| (format!("{k:#04x} {}", si::table_name(*k)), json!(n))).collect::<serde_json::Map<_, _>>(),
        },
        "counters": {
            "ntp_packets": d.counters.ntp,
            "tables": d.counters.tables,
            "crc_errors": d.counters.crc_errors,
            "malformed_tables": d.counters.malformed,
            "version_changes": d.counters.version_changes,
            "descriptor_loops_not_splitting": split_errors(),
            "message_fragments_without_start": d.counters.message_errors.orphan_fragments,
            "messages_oversize": d.counters.message_errors.oversize,
            "messages_malformed": d.counters.message_errors.malformed,
        },
        "time": {
            "mh_tot_jst": d.tot.as_ref().and_then(|t| t.jst.to_jst()).map(|t| t.to_string()),
            "ntp_unix": d.ntp.map(ntp_to_unix),
        },
        "networks": d.nits.values().map(|n| json!({
            "table": if n.header.table_id == 0x40 { "actual" } else { "other" },
            "network_id": hex16(n.header.extension),
            "name": n.descriptors.iter().find_map(|x| x.network_name()),
            "tlv_streams": n.streams.iter().map(|s| json!({
                "tlv_stream_id": hex16(s.tlv_stream_id),
                "original_network_id": hex16(s.original_network_id),
                "frequency_khz": s.descriptors.iter().find_map(|x| x.satellite_frequency_khz()),
                "services": s.descriptors.iter().find_map(|x| x.service_list()).map(|l| l.iter().map(|(id, t)| json!({"service_id": hex16(*id), "type": t})).collect::<Vec<_>>()),
            })).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
        "address_map": d.amts.values().flat_map(|a| a.services.iter().map(|s| json!({
            "service_id": hex16(s.service_id),
            "src": ip_text(&s.src.0, s.src.1),
            "dst": ip_text(&s.dst.0, s.dst.1),
        }))).collect::<Vec<_>>(),
        "services": services.values().cloned().collect::<Vec<_>>(),
        "events": by_service.iter().map(|(sid, v)| json!({
            "service_id": hex16(*sid),
            "count": v.len(),
            "first": v.iter().take(events).cloned().collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
        "packages": packages,
        "plt": d.plt.as_ref().map(|p| json!({
            "version": p.version,
            "packages": p.packages.iter().map(|(id, l)| json!({"package_id": id.iter().map(|b| format!("{b:02x}")).collect::<String>(), "pa_message": location(l)})).collect::<Vec<_>>(),
        })),
        "packet_ids_without_asset": orphan,
        "other_tables": d.other_tables.iter().map(|(k, n)| (format!("{k:#04x} {}", mmtp::table_name(*k)), json!(n))).collect::<serde_json::Map<_, _>>(),
        "mpu_payloads": mpu_packets,
        "scrambled_mpu_payloads": scrambled,
    })
}

fn ip_text(a: &[u8], mask: u8) -> String {
    let s = if a.len() == 4 {
        std::net::Ipv4Addr::new(a[0], a[1], a[2], a[3]).to_string()
    } else if let Ok(b) = <[u8; 16]>::try_from(a) {
        std::net::Ipv6Addr::from(b).to_string()
    } else {
        format!("{a:02x?}")
    };
    format!("{s}/{mask}")
}

fn inspect(a: &InspectArgs) -> Result<()> {
    let (st, d, n, skipped) = read(&a.file, false, |_, _| Ok(()))?;
    let j = report(&a.file, &st, &d, n, skipped, a.events);
    if a.json {
        println!("{}", serde_json::to_string_pretty(&j)?);
        return Ok(());
    }
    print_report(&j);
    Ok(())
}

fn s(v: &Value) -> String {
    match v {
        Value::String(x) => x.clone(),
        Value::Null => "-".into(),
        o => o.to_string(),
    }
}

fn print_report(j: &Value) {
    println!(
        "{}: {} TLV packets ({} bytes skipped)",
        s(&j["file"]),
        j["tlv_packets"],
        j["bytes_skipped"]
    );
    for (k, v) in j["transport"]["by_type"].as_object().into_iter().flatten() {
        println!("  {k}: {} packets, {} bytes", v["packets"], v["bytes"]);
    }
    let c = &j["counters"];
    println!(
        "  tables {} (CRC errors {}, malformed {}, version changes {}, descriptor loops not splitting {}); message fragments without start {}",
        c["tables"],
        c["crc_errors"],
        c["malformed_tables"],
        c["version_changes"],
        c["descriptor_loops_not_splitting"],
        c["message_fragments_without_start"]
    );
    println!(
        "  time: MH-TOT {} JST, NTP {} (Unix s), {} NTP packets",
        s(&j["time"]["mh_tot_jst"]),
        s(&j["time"]["ntp_unix"]),
        c["ntp_packets"]
    );
    for n in j["networks"].as_array().into_iter().flatten() {
        println!(
            "  TLV-NIT ({}) network {} \"{}\":",
            s(&n["table"]),
            s(&n["network_id"]),
            s(&n["name"])
        );
        for t in n["tlv_streams"].as_array().into_iter().flatten() {
            let svc: Vec<String> = t["services"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|x| s(&x["service_id"]))
                .collect();
            println!(
                "    TLV stream {} at {} kHz, services {}",
                s(&t["tlv_stream_id"]),
                s(&t["frequency_khz"]),
                svc.join(" ")
            );
        }
    }
    for m in j["address_map"].as_array().into_iter().flatten() {
        println!(
            "  AMT service {}: {} -> {}",
            s(&m["service_id"]),
            s(&m["src"]),
            s(&m["dst"])
        );
    }
    for sv in j["services"].as_array().into_iter().flatten() {
        println!(
            "  MH-SDT ({}) service {} on TLV stream {}: type {}, \"{}\" / \"{}\", running {}, free_CA {}",
            s(&sv["sdt"]),
            s(&sv["service_id"]),
            s(&sv["tlv_stream_id"]),
            sv["service_type"],
            s(&sv["provider"]),
            s(&sv["name"]),
            sv["running_status"],
            sv["free_ca_mode"]
        );
    }
    for e in j["events"].as_array().into_iter().flatten() {
        println!(
            "  MH-EIT service {}: {} events",
            s(&e["service_id"]),
            e["count"]
        );
        for ev in e["first"].as_array().into_iter().flatten() {
            println!(
                "    {} {} {} s ({}) \"{}\" genre {}",
                s(&ev["event_id"]),
                s(&ev["start_jst"]),
                s(&ev["duration_s"]),
                s(&ev["table"]),
                s(&ev["name"]),
                s(&ev["genre"])
            );
        }
    }
    for p in j["packages"].as_array().into_iter().flatten() {
        println!(
            "  MPT (PA on {}) service {} version {}:",
            s(&p["pa_packet_id"]),
            s(&p["service_id"]),
            p["mpt_version"]
        );
        for a in p["assets"].as_array().into_iter().flatten() {
            let m = &a["mpu"];
            let scr = m["scrambled_packets"].as_u64().unwrap_or(0);
            let pk = m["packets"].as_u64().unwrap_or(0);
            println!(
                "    {} on {} tag {}: {} packets ({} scrambled), {} MFUs (+{} scrambled), {} MPUs {}, lost packets {}, MPU jumps {}",
                s(&a["asset_type"]),
                s(&a["packet_id"]),
                s(&a["component_tag"]),
                pk,
                scr,
                s(&m["mfus"]),
                s(&m["scrambled_mfus"]),
                s(&m["mpus"]),
                s(&m["mpu_range"]),
                s(&m["packets_lost"]),
                s(&m["mpu_sequence_jumps"])
            );
        }
    }
    let o = j["packet_ids_without_asset"].as_array().map_or(0, Vec::len);
    if o > 0 {
        println!(
            "  MPU packet ids without an asset in the MPTs held: {}",
            j["packet_ids_without_asset"]
        );
    }
    println!(
        "  MPU payloads {}, scrambled {} (the header extension of B60 Table 6-9 type 0001h; never descrambled)",
        j["mpu_payloads"], j["scrambled_mpu_payloads"]
    );
    if let Some(o) = j["other_tables"].as_object().filter(|o| !o.is_empty()) {
        println!("  other tables (recognised, not decoded): {o:?}");
    }
}

/// One asset's output file(s).
struct Sink {
    kind: String,
    file: Option<std::io::BufWriter<std::fs::File>>,
    stem: PathBuf,
    written: u64,
    units: u64,
    rejected: u64,
    nal_types: BTreeMap<u8, u64>,
}

fn create(path: &Path, overwrite: bool) -> Result<std::fs::File> {
    let mut o = std::fs::OpenOptions::new();
    o.write(true);
    if overwrite {
        o.create(true).truncate(true);
    } else {
        o.create_new(true);
    }
    o.open(path)
        .with_context(|| format!("create {} (exists? use --overwrite)", path.display()))
}

fn extract(a: &ExtractArgs) -> Result<()> {
    std::fs::create_dir_all(&a.out_dir)
        .with_context(|| format!("create {}", a.out_dir.display()))?;
    let mut sinks: BTreeMap<(u16, bool), Sink> = BTreeMap::new();
    let mut skipped_no_asset = 0u64;
    let (st, d, n, skipped) = read(&a.file, a.write_scrambled, |d, m| {
        let Some((svc, asset)) = d.asset_of(m.packet_id) else {
            skipped_no_asset += 1;
            return Ok(());
        };
        if a.service.is_some_and(|s| svc != Some(s))
            || (!a.asset.is_empty() && !a.asset.contains(&m.packet_id))
        {
            return Ok(());
        }
        let kind = asset.type_str();
        let key = (m.packet_id, m.scrambled);
        let sink = match sinks.entry(key) {
            std::collections::btree_map::Entry::Occupied(e) => e.into_mut(),
            std::collections::btree_map::Entry::Vacant(v) => {
                let stem = a.out_dir.join(format!(
                    "{}-{:04x}",
                    svc.map_or("unknown".into(), |s| format!("{s:04x}")),
                    m.packet_id
                ));
                let ext = if m.scrambled {
                    "scrambled"
                } else {
                    match kind.as_str() {
                        "hev1" | "hvc1" => "hevc",
                        "mp4a" => "latm",
                        "stpp" => "",
                        _ => "mfu",
                    }
                };
                let file = if ext.is_empty() {
                    None
                } else {
                    Some(std::io::BufWriter::new(create(
                        &stem.with_extension(ext),
                        a.overwrite,
                    )?))
                };
                v.insert(Sink {
                    kind: kind.clone(),
                    file,
                    stem,
                    written: 0,
                    units: 0,
                    rejected: 0,
                    nal_types: BTreeMap::new(),
                })
            }
        };
        let mut buf = Vec::new();
        let ok = if m.scrambled {
            buf.extend_from_slice(&m.data);
            true
        } else {
            match sink.kind.as_str() {
                "hev1" | "hvc1" => match es::hevc_annex_b(&m.data, &mut buf) {
                    Some(t) => {
                        for x in t {
                            *sink.nal_types.entry(x).or_default() += 1;
                        }
                        true
                    }
                    None => false,
                },
                "mp4a" => es::loas_frame(&m.data, &mut buf),
                "stpp" => match es::Subtitle::parse(&m.data) {
                    Some(t) => {
                        let path = sink.stem.with_extension(format!(
                            "{}-{}.{}",
                            m.mpu_sequence_number,
                            t.subsample_number,
                            t.extension()
                        ));
                        create(&path, a.overwrite)?.write_all(t.data)?;
                        sink.written += t.data.len() as u64;
                        sink.units += 1;
                        return Ok(());
                    }
                    None => false,
                },
                _ => {
                    buf.extend_from_slice(&m.data);
                    true
                }
            }
        };
        if ok {
            if let Some(f) = sink.file.as_mut() {
                f.write_all(&buf)?;
            }
            sink.written += buf.len() as u64;
            sink.units += 1;
        } else {
            sink.rejected += 1;
        }
        Ok(())
    })?;
    for s in sinks.values_mut() {
        if let Some(f) = s.file.as_mut() {
            f.flush()?;
        }
    }
    let j = report(&a.file, &st, &d, n, skipped, 0);
    println!("{}: {} TLV packets", a.file.display(), n);
    let mut any = false;
    for ((pid, scrambled), s) in &sinks {
        any = true;
        let types: Vec<String> = s
            .nal_types
            .iter()
            .map(|(t, n)| format!("{} {}×{n}", t, es::hevc_nal_name(*t)))
            .collect();
        println!(
            "  {} asset {pid:#06x}{}: {} units, {} bytes, {} rejected -> {}{}",
            s.kind,
            if *scrambled {
                " (scrambled, as is)"
            } else {
                ""
            },
            s.units,
            s.written,
            s.rejected,
            s.stem.display(),
            if types.is_empty() {
                String::new()
            } else {
                format!("; NAL units {}", types.join(", "))
            }
        );
    }
    if !any {
        println!("  nothing written: no unscrambled MFU of a selected asset in the file");
    }
    println!(
        "  MPU payloads {}, scrambled {} (never descrambled{}); MFUs of no asset {}",
        j["mpu_payloads"],
        j["scrambled_mpu_payloads"],
        if a.write_scrambled {
            "; written as is"
        } else {
            "; not written, see --write-scrambled"
        },
        skipped_no_asset
    );
    Ok(())
}

fn validate(a: &InspectArgs) -> Result<()> {
    let (st, d, n, skipped) = read(&a.file, false, |_, _| Ok(()))?;
    let j = report(&a.file, &st, &d, n, skipped, 0);
    let c = &j["counters"];
    let mut findings: Vec<(&str, String)> = Vec::new();
    let mut err = |what: &str, v: &Value| {
        if v.as_u64().unwrap_or(0) > 0 {
            findings.push(("error", format!("{what}: {v}")));
        }
    };
    err("table CRC errors", &c["crc_errors"]);
    err("malformed tables", &c["malformed_tables"]);
    err(
        "descriptor loops not splitting",
        &c["descriptor_loops_not_splitting"],
    );
    err("malformed messages", &c["messages_malformed"]);
    err("bytes outside TLV packets", &j["bytes_skipped"]);
    err(
        "IP packets that do not decode",
        &j["transport"]["ip_errors"],
    );
    for (pid, s) in d.mpu_stats() {
        if s.packets_lost > 0 {
            findings.push((
                "error",
                format!("asset {pid:#06x}: {} MMTP packets lost", s.packets_lost),
            ));
        }
        // timed media number their MPUs consecutively; an asynchronous
        // (non-timed) asset's numbers need only be "unique at least in
        // the asset" (B60 §10.2.1), so jumps are expected there
        let timed = d.asset_of(pid).is_some_and(|(_, a)| {
            matches!(
                &a.asset_type,
                b"hev1" | b"hvc1" | b"mp4a" | b"stpp" | b"asgd"
            )
        });
        if s.mpu_sequence_jumps > 0 && timed {
            findings.push((
                "error",
                format!(
                    "asset {pid:#06x}: {} MPU sequence number jumps",
                    s.mpu_sequence_jumps
                ),
            ));
        }
        if s.malformed > 0 {
            findings.push((
                "error",
                format!("asset {pid:#06x}: {} malformed MPU payloads", s.malformed),
            ));
        }
        if d.asset_of(pid).is_none() {
            findings.push((
                "warning",
                format!("MPU packet id {pid:#06x} is in no MPT held"),
            ));
        }
    }
    for (pa, m) in &d.mpts {
        for x in &m.assets {
            if let Some(p) = x.packet_id()
                && !d.mpu_stats().any(|(k, _)| k == p)
            {
                findings.push((
                    "info",
                    format!(
                        "MPT on {pa:#06x}: {} asset on {p:#06x} sent no packet in this file",
                        x.type_str()
                    ),
                ));
            }
        }
        if m.service_id().is_some_and(|sid| {
            !d.sdts
                .values()
                .any(|s| s.services.iter().any(|v| v.service_id == sid))
        }) {
            findings.push((
                "info",
                format!(
                    "MPT on {pa:#06x}: service {:?} not in any MH-SDT held",
                    m.service_id()
                ),
            ));
        }
    }
    if d.mpts.is_empty() {
        findings.push(("error", "no MPT".into()));
    }
    for (name, missing) in [
        ("MH-SDT", d.sdts.is_empty()),
        ("MH-EIT", d.eits.is_empty()),
        ("MH-TOT", d.tot.is_none()),
        ("TLV-NIT", d.nits.is_empty()),
        ("AMT", d.amts.is_empty()),
    ] {
        if missing {
            findings.push((
                "info",
                format!("no {name} (it repeats less often than this file is long?)"),
            ));
        }
    }
    if c["message_fragments_without_start"].as_u64().unwrap_or(0) > 0 {
        findings.push((
            "info",
            format!(
                "{} control-message fragments without their start (the file starts mid-message)",
                c["message_fragments_without_start"]
            ),
        ));
    }
    let errors = findings.iter().filter(|f| f.0 == "error").count();
    let warnings = findings.iter().filter(|f| f.0 == "warning").count();
    if a.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "file": a.file.display().to_string(),
                "errors": errors,
                "warnings": warnings,
                "findings": findings.iter().map(|(k, m)| json!({"level": k, "message": m})).collect::<Vec<_>>(),
            }))?
        );
    } else {
        for (k, m) in &findings {
            println!("{k}: {m}");
        }
        println!("{}: {errors} errors, {warnings} warnings", a.file.display());
    }
    if errors > 0 {
        std::process::exit(1);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ids() {
        assert_eq!(parse_u16("0x0097").unwrap(), 0x97);
        assert_eq!(parse_u16("151").unwrap(), 151);
        assert!(parse_u16("0xZZ").is_err());
    }

    #[test]
    fn ip_addresses() {
        assert_eq!(ip_text(&[239, 0, 0, 1], 32), "239.0.0.1/32");
        let mut v6 = [0u8; 16];
        v6[0] = 0xff;
        v6[1] = 0x3e;
        assert_eq!(ip_text(&v6, 128), "ff3e::/128");
    }
}

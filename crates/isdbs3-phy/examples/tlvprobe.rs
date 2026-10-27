//! Receive a recording to TLV packets and print per-stream statistics:
//! TLV packet types, IP/HCfB, MMTP packet ids, MMT-SI and TLV-SI found
//! (development tool).
//!
//! cargo run --release -p isdbs3-phy --example tlvprobe -- REC

use isdb_iq::Recording;
use isdb_tlv::stats::StreamStats;
use isdb_tlv::{Assembler, mmtp, si};
use isdbs3_phy::frontend::{Trim, symbols};
use isdbs3_phy::rx::{RxOptions, receive};
use std::collections::BTreeMap;

fn main() {
    let path = std::path::PathBuf::from(std::env::args().nth(1).expect("REC"));
    let mut rec = Recording::open(&path, None, None).unwrap();
    let seg = rec.segments()[0].clone();
    let x = rec
        .read(seg.start, usize::try_from(seg.end - seg.start).unwrap())
        .unwrap();
    let s = symbols(&x, rec.sample_rate, None, Trim::Auto).unwrap();
    let t0 = std::time::Instant::now();
    let r = receive(&s.z, &RxOptions::default());
    let dt = t0.elapsed().as_secs_f64();
    let ok = r.slots.iter().filter(|s| s.data.is_some()).count();
    println!(
        "{}: {} frames, {}/{} slots clean, {:.1} s decode",
        path.display(),
        r.frames.len(),
        ok,
        r.slots.len(),
        dt
    );
    let mut asm: BTreeMap<u16, Assembler> = BTreeMap::new();
    let mut st: BTreeMap<u16, StreamStats> = BTreeMap::new();
    let mut first_ctrl: BTreeMap<u16, Vec<u8>> = BTreeMap::new();
    for sl in &r.slots {
        let a = asm.entry(sl.stream_id).or_default();
        let mut out = Vec::new();
        a.push_slot(sl.data.as_deref(), sl.head, &mut out);
        let e = st.entry(sl.stream_id).or_default();
        for p in &out {
            if p.kind == isdb_tlv::PacketType::Control && !first_ctrl.contains_key(&sl.stream_id) {
                first_ctrl.insert(sl.stream_id, p.data.clone());
            }
            e.add(p);
        }
    }
    for (id, e) in &st {
        let a = &asm[id];
        println!("TLV stream {id:#06x}: {:?}", a.stats);
        for (k, (n, b)) in &e.by_type {
            println!("  {k}: {n} packets, {b} bytes");
        }
        println!(
            "  ip errors {}, non-MMTP UDP {}, HCfB contexts {:?}",
            e.ip_errors,
            e.non_mmtp,
            e.contexts.keys().collect::<Vec<_>>()
        );
        println!(
            "  MPU {} (with extension header {})",
            e.mpu, e.mpu_with_extension
        );
        let ids: Vec<String> = e
            .mmtp
            .iter()
            .map(|(k, (n, t))| format!("{k:#06x}×{n}(pt{t})"))
            .collect();
        println!("  MMTP packet ids: {}", ids.join(" "));
        for (m, n) in &e.messages {
            println!("  message {m:#06x} {}: {n}", mmtp::message_name(*m));
        }
        for (t, n) in &e.mmt_tables {
            println!("  MMT-SI table {t:#04x} {}: {n}", mmtp::table_name(*t));
        }
        for (t, n) in &e.tlv_si {
            println!("  TLV-SI {t:#04x} {}: {n}", si::table_name(*t));
        }
        println!(
            "  TLV-SI offsets in control packets: {:?}; NIT networks {:?}; NIT streams {:?}",
            e.si_offsets, e.nit_networks, e.nit_streams
        );
        if let Some(d) = first_ctrl.get(id) {
            let h: String = d.iter().take(24).map(|b| format!("{b:02x}")).collect();
            println!("  first control packet ({} bytes): {h}…", d.len());
        }
    }
}

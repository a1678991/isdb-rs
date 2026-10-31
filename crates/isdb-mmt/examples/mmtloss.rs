//! List the MMTP packet losses (sequence gaps) of a TLV stream file per
//! packet id, each with the MPU it hit and that MPU's presentation time
//! from the MPT's MPU timestamp descriptor, on a timeline from the first
//! timed MPU of the first video asset (development tool: placing a
//! player's decode errors on the capture's losses). Works on scrambled
//! input — the MMTP and MPU payload headers are clear.
//!
//! cargo run --release -p isdb-mmt --example mmtloss -- FILE [PACKET_ID_HEX,...]

use isdb_mmt::Decoder;
use isdb_mmt::desc::Descriptor;
use isdb_mmt::mpu::parse_header;
use isdb_tlv::ip::{compressed, ipv4, ipv6};
use isdb_tlv::mmtp::{self, PayloadType};
use isdb_tlv::tlv::{FileReader, PacketType};
use std::collections::BTreeMap;

/// One loss event.
struct Loss {
    pid: u16,
    lost: u32,
    /// The MPU of the packet before the gap and of the one after.
    before: Option<u32>,
    after: Option<u32>,
    /// The fragmentation indicator of the packet after (0 complete,
    /// 1 head, 2 middle, 3 last) and whether it is a RAP.
    frag_after: u8,
    rap_after: bool,
    /// TLV packet index.
    at: u64,
}

fn ntp_secs(t: u64) -> f64 {
    (t >> 32) as f64 + (t & 0xffff_ffff) as f64 / 4_294_967_296.0
}

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args
        .next()
        .expect("usage: mmtloss FILE [PACKET_ID_HEX,...]");
    let only: Vec<u16> = args
        .next()
        .map(|s| {
            s.split(',')
                .map(|x| u16::from_str_radix(x.trim_start_matches("0x"), 16).unwrap())
                .collect()
        })
        .unwrap_or_default();
    let mut r = FileReader::new(std::io::BufReader::new(std::fs::File::open(&path).unwrap()));
    let mut d = Decoder::new();
    let mut mfus = Vec::new();
    let mut last: BTreeMap<u16, (u32, Option<u32>)> = BTreeMap::new();
    let mut losses = Vec::new();
    // MPU presentation times per packet id.
    let mut pts: BTreeMap<u16, BTreeMap<u32, u64>> = BTreeMap::new();
    let mut packets: BTreeMap<u16, u64> = BTreeMap::new();
    let mut n = 0u64;
    while let Some(p) = r.next_packet().unwrap() {
        n += 1;
        d.push(&p, &mut mfus);
        mfus.clear();
        let dg = match p.kind {
            PacketType::Ipv4 => ipv4(&p.data).ok(),
            PacketType::Ipv6 => ipv6(&p.data).ok(),
            PacketType::Compressed => compressed(&p.data).ok(),
            _ => None,
        };
        let Some(dg) = dg else { continue };
        let Some((h, payload)) = mmtp::parse(&dg.payload) else {
            continue;
        };
        if h.payload_type != PayloadType::Mpu || (!only.is_empty() && !only.contains(&h.packet_id))
        {
            continue;
        }
        *packets.entry(h.packet_id).or_default() += 1;
        let ph = parse_header(payload);
        let mpu = ph.as_ref().map(|x| x.mpu_sequence_number);
        if let Some(&(seq, before)) = last.get(&h.packet_id)
            && h.sequence != seq.wrapping_add(1)
        {
            losses.push(Loss {
                pid: h.packet_id,
                lost: h.sequence.wrapping_sub(seq).wrapping_sub(1),
                before,
                after: mpu,
                frag_after: ph.as_ref().map_or(0xff, |x| match x.fragmentation {
                    isdb_mmt::mpu::FragmentationIndicator::Complete => 0,
                    isdb_mmt::mpu::FragmentationIndicator::Head => 1,
                    isdb_mmt::mpu::FragmentationIndicator::Middle => 2,
                    isdb_mmt::mpu::FragmentationIndicator::Last => 3,
                }),
                rap_after: h.rap,
                at: n,
            });
        }
        last.insert(h.packet_id, (h.sequence, mpu));
        if let Some((_, a)) = d.asset_of(h.packet_id) {
            for x in &a.descriptors {
                if let Descriptor::MpuTimestamp(rows) = x {
                    let m = pts.entry(h.packet_id).or_default();
                    for row in rows {
                        m.insert(row.mpu_sequence_number, row.presentation_time);
                    }
                }
            }
        }
    }
    // The origin: the earliest MPU presentation time of the first video
    // asset (by packet id order among hev1/hvc1).
    let video = d
        .mpts
        .values()
        .flat_map(|m| m.assets.iter())
        .find(|a| matches!(&a.asset_type, b"hev1" | b"hvc1"))
        .and_then(|a| a.packet_id());
    let origin = video
        .and_then(|v| pts.get(&v))
        .and_then(|m| m.values().min().copied())
        .map(ntp_secs);
    println!("{path}: {n} TLV packets; video {video:?} origin {origin:?}");
    for (pid, c) in &packets {
        let l: u64 = losses
            .iter()
            .filter(|x| x.pid == *pid)
            .map(|x| u64::from(x.lost))
            .sum();
        let ev = losses.iter().filter(|x| x.pid == *pid).count();
        println!("pid {pid:#06x}: packets {c} lost {l} in {ev} gaps");
    }
    for x in &losses {
        let t = |m: Option<u32>| {
            m.and_then(|m| pts.get(&x.pid)?.get(&m).copied())
                .map(ntp_secs)
                .zip(origin)
                .map_or("?".to_string(), |(t, o)| format!("{:.3}", t - o))
        };
        println!(
            "loss pid {:#06x} tlv#{} lost {} mpu {:?}->{:?} (t {} -> {}) next frag {} rap {}",
            x.pid,
            x.at,
            x.lost,
            x.before,
            x.after,
            t(x.before),
            t(x.after),
            x.frag_after,
            u8::from(x.rap_after)
        );
    }
}

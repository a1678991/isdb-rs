//! Dump the MPT timing descriptors (MPU timestamp, MPU extended timestamp)
//! of one asset per MPU next to its MFUs' sample numbers and NAL types,
//! to check how the descriptor clock maps onto the decode order
//! (development tool, clear TLV input).
//!
//! cargo run --release -p isdb-mmt --example mpuclock -- FILE PACKET_ID [MPUS]

use isdb_mmt::Decoder;
use isdb_mmt::desc::Descriptor;
use isdb_mmt::es;
use isdb_tlv::tlv::FileReader;
use std::collections::{BTreeMap, BTreeSet};

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("usage: mpuclock FILE PACKET_ID [MPUS]");
    let pid = args
        .next()
        .and_then(|s| u16::from_str_radix(s.trim_start_matches("0x"), 16).ok())
        .expect("PACKET_ID in hex");
    let limit: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(4);
    let mut r = FileReader::new(std::io::BufReader::new(std::fs::File::open(&path).unwrap()));
    let mut d = Decoder::new();
    let mut mfus = Vec::new();
    let mut seen_ts: BTreeSet<u32> = BTreeSet::new();
    let mut seen_ext: BTreeSet<u32> = BTreeSet::new();
    let mut samples: BTreeMap<u32, Vec<String>> = BTreeMap::new();
    let mut last_sample: Option<(u32, u32)> = None;
    while let Some(p) = r.next_packet().unwrap() {
        d.push(&p, &mut mfus);
        if let Some((_, a)) = d.asset_of(pid) {
            for x in &a.descriptors {
                match x {
                    Descriptor::MpuTimestamp(rows) => {
                        for row in rows {
                            if seen_ts.insert(row.mpu_sequence_number) {
                                let t = row.presentation_time;
                                let secs =
                                    (t >> 32) as f64 + (t & 0xffff_ffff) as f64 / 4_294_967_296.0;
                                println!("TS  mpu {} pt {secs:.6}", row.mpu_sequence_number);
                            }
                        }
                    }
                    Descriptor::ExtendedTimestamp(e) => {
                        for m in &e.mpus {
                            if seen_ext.insert(m.mpu_sequence_number) {
                                println!(
                                    "EXT mpu {} timescale {:?} default_pts {:?} dto {} aus {} {:?}",
                                    m.mpu_sequence_number,
                                    e.timescale,
                                    e.default_pts_offset,
                                    m.decoding_time_offset,
                                    m.access_units.len(),
                                    m.access_units
                                );
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        for m in mfus.drain(..) {
            if m.packet_id != pid {
                continue;
            }
            let Some(h) = m.header else { continue };
            let key = (m.mpu_sequence_number, h.sample_number);
            let types: Vec<u8> = es::nal_units(&m.data)
                .map(|n| n.iter().filter_map(|x| es::hevc_nal_type(x)).collect())
                .unwrap_or_default();
            let v = samples.entry(m.mpu_sequence_number).or_default();
            if last_sample != Some(key) {
                v.push(format!("s{}:{types:?}", h.sample_number));
            } else if let Some(l) = v.last_mut() {
                l.push_str(&format!("+{types:?}"));
            }
            last_sample = Some(key);
        }
        if samples.len() > limit {
            break;
        }
    }
    for (mpu, v) in &samples {
        println!("MFU mpu {mpu} samples {}: {}", v.len(), v.join(" "));
    }
}

//! Print, for one HEVC asset of a clear (descrambled) TLV stream file,
//! each whole MFU as `MPU:index len [NAL types]` (a `*` marks a slice
//! with `first_slice_segment_in_pic_flag`), and the MMTP packet sequence
//! gaps of the asset with the MFUs they took (development tool: how
//! pictures map onto MFUs, what a loss takes with it).
//!
//! The reassembly is its own (the payload's fragmentation indicator over
//! the packet sequence): a descrambled stream keeps its scramble
//! extension headers, which `Decoder` reads as "still scrambled".
//!
//! cargo run --release -p isdb-mmt --example mfunals -- FILE PACKET_ID [FROM_MPU] [MPUS]

use isdb_mmt::es;
use isdb_mmt::mpu::{self, FragmentType, FragmentationIndicator};
use isdb_tlv::ip::{compressed, ipv4, ipv6};
use isdb_tlv::mmtp::{self, PayloadType};
use isdb_tlv::tlv::{FileReader, PacketType};

fn nal_list(data: &[u8]) -> String {
    es::nal_units(data)
        .into_iter()
        .flatten()
        .filter_map(|n| {
            let ty = (n.first()? >> 1) & 0x3f;
            Some(if ty < 32 && n.get(2).is_some_and(|b| b & 0x80 != 0) {
                format!("{ty}*")
            } else {
                format!("{ty}")
            })
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args
        .next()
        .expect("usage: mfunals FILE PACKET_ID [FROM_MPU] [MPUS]");
    let pid = args
        .next()
        .and_then(|s| u16::from_str_radix(s.trim_start_matches("0x"), 16).ok())
        .expect("PACKET_ID in hex");
    let from: u32 = args.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let count: u32 = args.next().and_then(|s| s.parse().ok()).unwrap_or(4);
    let in_range = |mpu: u32| mpu >= from && mpu < from.saturating_add(count);
    let mut r = FileReader::new(std::io::BufReader::new(std::fs::File::open(&path).unwrap()));
    let mut last_seq: Option<u32> = None;
    let mut last_mpu = None;
    let mut index = 0;
    let mut partial: Option<(u32, Vec<u8>)> = None;
    while let Some(p) = r.next_packet().unwrap() {
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
        if h.packet_id != pid || h.payload_type != PayloadType::Mpu {
            continue;
        }
        let Some((ph, units)) = mpu::parse(payload) else {
            continue;
        };
        let mpu = ph.mpu_sequence_number;
        if mpu >= from.saturating_add(count) && last_mpu.is_some_and(|l| l >= from) {
            break;
        }
        if let Some(l) = last_seq
            && h.sequence != l.wrapping_add(1)
        {
            if in_range(mpu) {
                println!(
                    "   -- {} packet(s) lost; {} the MFU under assembly; next fragment {:?} rap {}",
                    h.sequence.wrapping_sub(l).wrapping_sub(1),
                    if partial.is_some() { "dropping" } else { "no" },
                    ph.fragmentation,
                    u8::from(h.rap)
                );
            }
            partial = None;
        }
        last_seq = Some(h.sequence);
        if ph.fragment_type != FragmentType::Mfu {
            continue;
        }
        if last_mpu != Some(mpu) {
            last_mpu = Some(mpu);
            index = 0;
        }
        for u in units {
            let whole = match ph.fragmentation {
                FragmentationIndicator::Complete => Some(u.data.to_vec()),
                FragmentationIndicator::Head => {
                    partial = Some((mpu, u.data.to_vec()));
                    None
                }
                FragmentationIndicator::Middle => {
                    if let Some(p) = partial.as_mut() {
                        p.1.extend_from_slice(u.data);
                    }
                    None
                }
                FragmentationIndicator::Last => partial.take().map(|mut p| {
                    p.1.extend_from_slice(u.data);
                    p.1
                }),
            };
            if let Some(data) = whole {
                index += 1;
                if in_range(mpu) {
                    println!("{mpu}:{index} len {} [{}]", data.len(), nal_list(&data));
                }
            }
        }
    }
}

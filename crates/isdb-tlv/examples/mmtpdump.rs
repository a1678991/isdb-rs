//! Dump the MMTP packets of a TLV stream file: per packet id, the MMTP
//! header and the first bytes of the payload (development tool for the
//! MMT-SI and MPU decoders). Streams the file.
//!
//! cargo run --release -p isdb-tlv --example mmtpdump -- FILE [PACKET_ID_HEX] [MAX]

use isdb_tlv::ip::{compressed, ipv4, ipv6};
use isdb_tlv::mmtp;
use isdb_tlv::tlv::{PacketType, SYNC};
use std::io::Read;

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let path = a
        .get(1)
        .expect("usage: mmtpdump FILE [PACKET_ID_HEX] [MAX]");
    let only = a
        .get(2)
        .filter(|s| !s.is_empty() && *s != "all")
        .map(|s| u16::from_str_radix(s.trim_start_matches("0x"), 16).unwrap());
    let max: usize = a.get(3).map_or(8, |s| s.parse().unwrap());
    let mut f = std::fs::File::open(path).unwrap();
    let mut buf = Vec::new();
    let mut chunk = vec![0u8; 1 << 20];
    let mut shown = std::collections::BTreeMap::<u16, usize>::new();
    loop {
        let n = f.read(&mut chunk).unwrap();
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
        let mut p = 0;
        while p + 4 <= buf.len() {
            if buf[p] != SYNC {
                p += 1;
                continue;
            }
            let len = usize::from(u16::from_be_bytes([buf[p + 2], buf[p + 3]]));
            if p + 4 + len > buf.len() {
                break;
            }
            let kind = PacketType::from_byte(buf[p + 1]);
            let data = &buf[p + 4..p + 4 + len];
            let dg = match kind {
                PacketType::Ipv4 => ipv4(data).ok(),
                PacketType::Ipv6 => ipv6(data).ok(),
                PacketType::Compressed => compressed(data).ok(),
                _ => None,
            };
            if let Some(d) = dg
                && let Some((h, pl)) = mmtp::parse(&d.payload)
                && only.is_none_or(|o| o == h.packet_id)
            {
                let c = shown.entry(h.packet_id).or_default();
                if *c < max {
                    *c += 1;
                    let hex: String = pl.iter().take(48).map(|b| format!("{b:02x}")).collect();
                    println!(
                        "id {:#06x} pt {} seq {} rap {} scr {} len {}: {hex}",
                        h.packet_id,
                        h.payload_type,
                        h.sequence,
                        u8::from(h.rap),
                        u8::from(h.scramble_info),
                        pl.len()
                    );
                }
            }
            p += 4 + len;
        }
        buf.drain(..p);
    }
}

//! Which bytes of a scrambled MPU payload are in the clear (development
//! tool; SPEC-NOTES "scrambled-mpu"): per packet id, how often the MPU
//! payload header fields are consistent (payload_length = payload − 2,
//! fragment_type 2, timed 1, MPU sequence number continuous), and how
//! often the MFU header after them would be (offset 0 for whole MFUs,
//! movie fragment sequence number constant within an MPU). Streams the
//! file.
//!
//! cargo run --release -p isdb-mmt --example scrambleprobe -- FILE

use isdb_tlv::ip::{compressed, ipv4, ipv6};
use isdb_tlv::mmtp;
use isdb_tlv::tlv::{FileReader, PacketType};
use std::collections::BTreeMap;

#[derive(Default, Debug)]
struct C {
    packets: u64,
    scrambled: u64,
    len_ok: u64,
    type2_timed: u64,
    mpu_cont: u64,
    whole: u64,
    whole_offset0: u64,
    aggregated: u64,
    last_mpu: Option<u32>,
}

fn main() {
    let path = std::env::args().nth(1).expect("usage: scrambleprobe FILE");
    let mut r = FileReader::new(std::io::BufReader::new(std::fs::File::open(&path).unwrap()));
    let mut c: BTreeMap<u16, C> = BTreeMap::new();
    while let Some(p) = r.next_packet().unwrap() {
        let d = match p.kind {
            PacketType::Ipv4 => ipv4(&p.data).ok(),
            PacketType::Ipv6 => ipv6(&p.data).ok(),
            PacketType::Compressed => compressed(&p.data).ok(),
            _ => None,
        };
        let Some(d) = d else { continue };
        let Some((h, pl)) = mmtp::parse(&d.payload) else {
            continue;
        };
        if h.payload_type != mmtp::MPU || pl.len() < 8 {
            continue;
        }
        let e = c.entry(h.packet_id).or_default();
        e.packets += 1;
        e.scrambled += u64::from(h.scramble_info);
        e.len_ok += u64::from(usize::from(u16::from_be_bytes([pl[0], pl[1]])) == pl.len() - 2);
        e.type2_timed += u64::from(pl[2] >> 4 == 2 && pl[2] & 0x08 != 0);
        let mpu = u32::from_be_bytes([pl[4], pl[5], pl[6], pl[7]]);
        e.mpu_cont += u64::from(
            e.last_mpu
                .is_none_or(|l| mpu == l || mpu == l.wrapping_add(1)),
        );
        e.last_mpu = Some(mpu);
        e.aggregated += u64::from(pl[2] & 1);
        if (pl[2] >> 1) & 3 == 0 && pl[2] & 1 == 0 && pl.len() >= 22 {
            e.whole += 1;
            e.whole_offset0 += u64::from(pl[16..20] == [0, 0, 0, 0]);
        }
    }
    for (id, e) in &c {
        println!("{id:#06x}: {e:?}");
    }
}

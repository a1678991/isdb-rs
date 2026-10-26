//! Dump the first PA message of each TLV stream of a recording (development
//! tool for the MMT-SI parsing).
//!
//! cargo run --release -p isdbs3-phy --example padump -- REC

use isdb_iq::Recording;
use isdb_tlv::ip::compressed;
use isdb_tlv::{Assembler, PacketType, mmtp};
use isdbs3_phy::frontend::{Trim, symbols};
use isdbs3_phy::rx::{RxOptions, receive};
use std::collections::BTreeMap;

fn main() {
    let path = std::path::PathBuf::from(std::env::args().nth(1).expect("REC"));
    let mut rec = Recording::open(&path, None, None).unwrap();
    let seg = rec.segments()[0].clone();
    let n = ((seg.end - seg.start) as usize).min(12_000_000);
    let x = rec.read(seg.start, n).unwrap();
    let s = symbols(&x, rec.sample_rate, None, Trim::Auto).unwrap();
    let r = receive(&s.z, &RxOptions::default());
    let mut asm: BTreeMap<u16, Assembler> = BTreeMap::new();
    let mut done: BTreeMap<u16, usize> = BTreeMap::new();
    for sl in &r.slots {
        let mut out = Vec::new();
        asm.entry(sl.stream_id)
            .or_default()
            .push_slot(sl.data.as_deref(), sl.head, &mut out);
        for p in out {
            if p.kind != PacketType::Compressed {
                continue;
            }
            let Ok(d) = compressed(&p.data) else { continue };
            let Some((h, pl)) = mmtp::parse(&d.payload) else {
                continue;
            };
            if h.packet_id != 0 || h.payload_type != mmtp::PayloadType::Messages {
                continue;
            }
            let c = done.entry(sl.stream_id).or_default();
            if *c >= 2 {
                continue;
            }
            *c += 1;
            let hex: String = pl.iter().take(48).map(|b| format!("{b:02x} ")).collect();
            println!(
                "stream {:#06x} PA payload ({} bytes): {hex}",
                sl.stream_id,
                pl.len()
            );
            println!("  parsed: {:?}", mmtp::messages(pl));
        }
    }
}

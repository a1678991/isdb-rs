//! Write the FPGA transmit block's test vectors with real ISDB-S codes
//! (`isdb_frontend::vectors::tx_document`): the prefix of a data
//! superframe of the BS layout (TC8PSK 2/3 × 48, three TSs), after a null
//! superframe so the inner coder's state is carried in, as
//! `tx::Transmitter::superframe_codes` makes it: the 192 BPSK header
//! symbols, bursts and TC8PSK main signal. ISDB-S at 40 Msps, 10 dB
//! backoff (the bitstream's default bank), no stall, three zero words
//! after the prefix (the line drains).
//!
//! cargo run --release -p isdbs-phy --example txvectors -- OUT.json [CODES]
//!
//! CODES (default 4 000) is rounded up to whole DMA words of 8. The other
//! vectors (continuous, stalls, another bank) come from `isdb-frontend`'s
//! `examples/txvectors.rs`.

use isdb_frontend::shape::fixed::TxDesign;
use isdb_frontend::txformat;
use isdb_frontend::vectors::tx_document;
use isdbs_core::combination::Combination;
use isdbs_core::tmcc::TmccInfo;
use isdbs_phy::frontend;
use isdbs_phy::tx::{Packet, Transmitter};
use std::path::PathBuf;

/// A packet of relative TS `r`, index `i`: PID 0x0100, a counted payload.
fn packet(r: u8, i: u32) -> Packet {
    let mut p = [0u8; 188];
    p[0] = 0x47;
    p[1] = 0x01;
    p[3] = 0x10 | (i.to_le_bytes()[0] & 0xF);
    p[4] = r;
    p[5..9].copy_from_slice(&i.to_be_bytes());
    for (k, b) in (0u32..).zip(p[9..].iter_mut()) {
        *b = (k * 37 + i * 11 + u32::from(r)).to_le_bytes()[0];
    }
    p
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let out = PathBuf::from(args.get(1).expect("output path"));
    let n: usize = args.get(2).map_or(4000, |s| s.parse().unwrap());
    let info = TmccInfo::assign(
        &[(Combination::Tc8psk2_3, 48)],
        &[(0x4010, 16), (0x4011, 16), (0x4012, 16)],
    )
    .unwrap();
    let mut tx = Transmitter::new(info).unwrap();
    tx.null_superframe_codes();
    let per_ts: Vec<Vec<Packet>> = (0..tx.ts_count())
        .map(|r| {
            let k = u32::try_from(tx.packets_per_superframe(r)).unwrap();
            (0..k)
                .map(|i| packet(u8::try_from(r).unwrap(), i))
                .collect()
        })
        .collect();
    let refs: Vec<&[Packet]> = per_ts.iter().map(Vec::as_slice).collect();
    let sf = tx.superframe_codes(&refs).unwrap();
    let codes = &sf.codes[..n.div_ceil(8) * 8];
    let mut words = txformat::pack(codes);
    words.extend([0; 3]);
    let design = TxDesign::for_system(frontend::system(), 40e6);
    let doc = tx_document(&design, &words, &[]).unwrap();
    // serde_json's compact form (Value's Display)
    let bytes = doc.to_string().into_bytes();
    std::fs::write(&out, &bytes).unwrap();
    let c = &doc["counters"];
    eprintln!(
        "{}: {} bytes; {} codes of superframe 2 ({} words), {} samples, consumed {} missed {}",
        out.display(),
        bytes.len(),
        codes.len(),
        words.len(),
        c["samples"],
        c["consumed"],
        c["missed"]
    );
}

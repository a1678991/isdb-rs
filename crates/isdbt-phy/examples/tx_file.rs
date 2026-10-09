//! TS files → cf32 IQ:
//!   tx_file OUT.cf32 FRAMES MODE GI PARTIAL A-SPEC A.ts [B-SPEC B.ts [C-SPEC C.ts]]
//! Layer TS files are read cyclically, packet aligned (188 B).  The
//! transmitter is pre-rolled and the stream ends with the flush frames.
mod common;
use std::io::Write;

fn load(p: &str) -> Vec<[u8; 188]> {
    let d = std::fs::read(p).unwrap();
    d.as_chunks::<188>().0.to_vec()
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let frames: usize = a[2].parse().unwrap();
    let arg = |i: usize| a.get(i).map(|s| s.as_str());
    let params = common::params3(
        a[3].parse().unwrap(),
        a[4].parse().unwrap(),
        a[5] == "1",
        &a[6],
        arg(8),
        arg(10),
    );
    let inputs: [Vec<[u8; 188]>; 3] = [
        load(&a[7]),
        arg(9).map(load).unwrap_or_default(),
        arg(11).map(load).unwrap_or_default(),
    ];
    let mut tx = isdbt_phy::Transmitter::new(params);
    tx.preroll().unwrap();
    let n: [usize; 3] = std::array::from_fn(|l| tx.tsps_per_frame(isdbt_core::Layer::ALL[l]));
    let mut out = std::io::BufWriter::new(std::fs::File::create(&a[1]).unwrap());
    let mut pos = [0usize; 3];
    for _ in 0..frames {
        let f: [Vec<[u8; 188]>; 3] = std::array::from_fn(|l| {
            let v = (0..n[l])
                .map(|k| inputs[l][(pos[l] + k) % inputs[l].len()])
                .collect();
            pos[l] += n[l];
            v
        });
        let s = tx.transmit_frame([&f[0], &f[1], &f[2]]).unwrap();
        out.write_all(&isdbt_phy::iq::cf32_to_bytes(&s)).unwrap();
    }
    out.write_all(&isdbt_phy::iq::cf32_to_bytes(&tx.finish().unwrap()))
        .unwrap();
    for _ in 1..tx.flush_frames() {
        out.write_all(&isdbt_phy::iq::cf32_to_bytes(
            &tx.transmit_null_frame().unwrap(),
        ))
        .unwrap();
    }
    eprintln!(
        "tx_file: {frames} frames, A/B/C {:?} TSP/frame -> {}",
        n, a[1]
    );
}

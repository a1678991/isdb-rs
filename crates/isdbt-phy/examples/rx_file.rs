//! cf32 IQ → per-layer TS:  rx_file IN.cf32 OUT_PREFIX
//! Writes OUT_PREFIX.{a,b,c}.ts (valid frames only) and OUT_PREFIX.mux.ts.
use std::io::{Read, Write};

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let mut f = std::fs::File::open(&a[1]).unwrap();
    let mut rx = isdbt_phy::Receiver::new(isdbt_ofdm::DemodConfig::default());
    let mut outs: Vec<std::io::BufWriter<std::fs::File>> = ["a", "b", "c", "mux"]
        .iter()
        .map(|s| {
            std::io::BufWriter::new(std::fs::File::create(format!("{}.{s}.ts", a[2])).unwrap())
        })
        .collect();
    let mut buf = vec![0u8; 8 << 20];
    let (mut frames, mut valid) = (0, 0);
    loop {
        let n = f.read(&mut buf).unwrap();
        if n == 0 {
            break;
        }
        for fr in rx
            .push(&isdbt_phy::iq::cf32_from_bytes(&buf[..n - n % 8]))
            .unwrap()
        {
            frames += 1;
            eprintln!(
                "frame tx={} valid={} parity={:?} mer={:?} cfo={:.3} rs={:?}",
                fr.tx_frame,
                fr.valid,
                fr.tmcc.parity,
                fr.metrics.mer_db,
                fr.metrics.cfo_carriers,
                fr.rs
                    .iter()
                    .map(|r| r.map(|r| (r.clean, r.corrected_bytes, r.uncorrectable)))
                    .collect::<Vec<_>>()
            );
            if !fr.valid {
                continue;
            }
            valid += 1;
            for (i, l) in fr.layers.iter().enumerate() {
                if let Some(l) = l {
                    for p in l {
                        outs[i].write_all(p).unwrap();
                    }
                }
            }
            for p in &fr.ts {
                outs[3].write_all(p).unwrap();
            }
        }
    }
    eprintln!(
        "rx_file: {frames} frames ({valid} valid); params {:?}",
        rx.params()
    );
}

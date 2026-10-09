//! Rotation-numbering sensitivity check: decode a cf32 file with isdbt-phy's
//! Receiver using the given frequency-deinterleaver carrier-rotation
//! numbering and print per-layer RS totals over valid frames.
//!   rx_rot IN.cf32 data|group
use isdbt_ofdm::DemodConfig;
use isdbt_ofdm::freq_interleave::RotationNumbering;
use std::io::Read;

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let rotation = match a[2].as_str() {
        "data" => RotationNumbering::DataSegmentNumber,
        "group" => RotationNumbering::GroupRelative,
        r => panic!("rotation {r}"),
    };
    let mut rx = isdbt_phy::Receiver::new(DemodConfig { rotation, ..DemodConfig::default() });
    let mut f = std::fs::File::open(&a[1]).unwrap();
    let mut buf = vec![0u8; 8 << 20];
    let mut tot = [[0usize; 3]; 3];
    let mut valid = 0;
    loop {
        let n = f.read(&mut buf).unwrap();
        if n == 0 {
            break;
        }
        for fr in rx.push(&isdbt_phy::iq::cf32_from_bytes(&buf[..n - n % 8])).unwrap() {
            if !fr.valid {
                continue;
            }
            valid += 1;
            for (i, r) in fr.rs.iter().enumerate() {
                if let Some(r) = r {
                    tot[i][0] += r.clean;
                    tot[i][1] += r.corrected_bytes;
                    tot[i][2] += r.uncorrectable;
                }
            }
        }
    }
    println!("rotation {:?}: {valid} valid frames; RS (clean, corrected bytes, uncorrectable) A {:?} B {:?} C {:?}", rotation, tot[0], tot[1], tot[2]);
}

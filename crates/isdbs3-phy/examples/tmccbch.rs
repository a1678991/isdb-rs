//! After the TMCC LDPC converges, try readings of the energy dispersal and
//! the BCH bit order until BCH accepts the data (development tool for the
//! TMCC readings in SPEC-NOTES).
//!
//! cargo run --release -p isdbs3-phy --example tmccbch -- REC

use isdb_fec::prbs::Prbs15;
use isdb_iq::Recording;
use isdbs3_core::params::CodeRate;
use isdbs3_core::tmcc::{DATA_BITS, NULL_HEAD, NULL_TAIL};
use isdbs3_phy::frontend::{Trim, symbols};
use isdbs3_phy::sync::synchronise;
use isdbs3_phy::tmcc::llrs;

/// Generic Fibonacci LFSR, stages 1..n, taps (a, n), state string stage 1
/// first, output = feedback (the B44 figures' form) or the last stage.
fn prbs(init: &str, a: usize, out_last: bool, len: usize) -> Vec<u8> {
    let mut s: Vec<u8> = init.bytes().map(|c| c - b'0').collect();
    let n = s.len();
    (0..len)
        .map(|_| {
            let fb = s[a - 1] ^ s[n - 1];
            let o = if out_last { s[n - 1] } else { fb };
            s.rotate_right(1);
            s[0] = fb;
            o
        })
        .collect()
}

fn main() {
    let path = std::path::PathBuf::from(std::env::args().nth(1).expect("REC"));
    let mut rec = Recording::open(&path, None, None).unwrap();
    let seg = rec.segments()[0].clone();
    let n = (usize::try_from(seg.end - seg.start).expect("segment length")).min(6_000_000);
    let x = rec.read(seg.start, n).unwrap();
    let s = symbols(&x, rec.sample_rate, None, Trim::Auto).unwrap();
    let sy = synchronise(&s.z).expect("slots");
    let f = sy.frames[0];
    let es_n0 = 10f64.powf(sy.sync_snr_db / 10.0);
    let l = llrs(&s.z, &sy.slots, f, 1.0 / (2.0 * es_n0)).unwrap();
    let code = isdbs3_core::ldpc::code(CodeRate::new(61).unwrap());
    let mut w = vec![1.0e4f32; code.n()];
    w[NULL_HEAD..NULL_HEAD + DATA_BITS].copy_from_slice(&l[..DATA_BITS]);
    let p0 = NULL_HEAD + DATA_BITS + NULL_TAIL;
    w[p0..].copy_from_slice(&l[DATA_BITS..]);
    let mut bits = Vec::new();
    let d = code.decode(&mut w, &mut bits, 50, 0.8).unwrap();
    println!("LDPC converged in {} iterations", d.iterations);
    let data = bits[NULL_HEAD..NULL_HEAD + DATA_BITS].to_vec();
    // raw: do the first bits look like anything?
    let hex: String = data[..64]
        .chunks(8)
        .map(|c| format!("{:02x}", c.iter().fold(0u8, |a, &b| a << 1 | b)))
        .collect();
    println!("first 8 bytes before dispersal removal: {hex}");
    let inits = [
        ("B44 TMCC 100000000001110", "100000000001110"),
        ("reversed 011100000000001", "011100000000001"),
        ("ISDB-S 100101010000000", "100101010000000"),
        ("pilot 100000000101100", "100000000101100"),
    ];
    let mut tried = 0;
    for (name, init) in inits {
        for out_last in [false, true] {
            for skip in [0usize, 1, 2, 8, 15] {
                let full = prbs(init, 14, out_last, DATA_BITS + skip);
                let pr = &full[skip..];
                for reverse in [false, true] {
                    let mut dd: Vec<u8> = data.iter().zip(pr).map(|(a, b)| a ^ b).collect();
                    if reverse {
                        dd.reverse();
                    }
                    tried += 1;
                    let mut t = dd.clone();
                    if let Ok(c) = isdb_fec::bch::decode(&mut t) {
                        let hex: String = t[..64]
                            .chunks(8)
                            .map(|c| format!("{:02x}", c.iter().fold(0u8, |a, &b| a << 1 | b)))
                            .collect();
                        println!(
                            "BCH OK ({c} corrected): {name}, output {}, skip {skip}, reversed {reverse}; first bytes {hex}",
                            if out_last { "last stage" } else { "feedback" }
                        );
                    }
                }
            }
        }
    }
    // and without any dispersal
    for reverse in [false, true] {
        let mut t = data.clone();
        if reverse {
            t.reverse();
        }
        if let Ok(c) = isdb_fec::bch::decode(&mut t) {
            println!("BCH OK ({c}) with no dispersal, reversed {reverse}");
        }
    }
    let _ = Prbs15::new();
    println!("{tried} dispersal readings tried");
}

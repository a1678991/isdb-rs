//! Bit-exactness probe for refactors of the PHY hot paths: FNV-1a hashes
//! of the transmitter's IQ samples, the demodulator's LLRs and the
//! receiver's TS for three configurations (DQPSK partial reception,
//! one-layer 16QAM, three layers), printed as `BITEXACT` lines.
//!
//! The hashes are not asserted (they change with any intended change of
//! the signal); run the probe on the tree before and after a change and
//! compare the lines, in the same profile:
//!
//! ```text
//! cargo test --release -p isdbt-phy --test bitexact_hashes -- --ignored --nocapture
//! ```

use isdbt_core::{
    CodeRate, GuardInterval, Layer, LayerConfig, Mode, Modulation, Params, Segments, TimeInterleave,
};
use isdbt_ofdm::{C32, DemodConfig, Demodulator};
use isdbt_phy::{Packet, Receiver, Transmitter};

fn lc(n: u8, m: Modulation, r: CodeRate, ti: TimeInterleave) -> Option<LayerConfig> {
    Some(LayerConfig {
        segments: Segments::new(n).unwrap(),
        modulation: m,
        code_rate: r,
        time_interleave: ti,
    })
}

struct Fnv(u64);
impl Fnv {
    fn eat(&mut self, b: &[u8]) {
        for &x in b {
            self.0 ^= u64::from(x);
            self.0 = self.0.wrapping_mul(0x100_0000_01b3);
        }
    }
}

fn packet(n: u32) -> Packet {
    let mut p = [0u8; 188];
    p[0] = 0x47;
    p[1] = 0x01;
    p[3] = 0x10 | (n & 15) as u8;
    let mut x = n.wrapping_mul(2654435761) | 1;
    for b in &mut p[4..] {
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        *b = x.to_le_bytes()[0];
    }
    p
}

#[test]
#[ignore = "a probe that prints hashes; compare its output across a change"]
fn bitexact_hashes() {
    use CodeRate::*;
    use Modulation::*;
    use TimeInterleave::*;
    let configs = [
        Params::new(
            Mode::M1,
            GuardInterval::G1_8,
            true,
            [lc(1, Dqpsk, R1_2, K1), lc(12, Qam64, R3_4, K1), None],
        )
        .unwrap(),
        Params::new(
            Mode::M2,
            GuardInterval::G1_4,
            false,
            [lc(13, Qam16, R2_3, K2), None, None],
        )
        .unwrap(),
        Params::new(
            Mode::M1,
            GuardInterval::G1_16,
            false,
            [
                lc(5, Dqpsk, R5_6, K0),
                lc(3, Qpsk, R1_2, K2),
                lc(5, Qam64, R7_8, K1),
            ],
        )
        .unwrap(),
    ];
    let mut n = 0u32;
    for p in configs {
        let mut tx = Transmitter::new(p.clone());
        let mut iq: Vec<C32> = Vec::new();
        let mut h_tx = Fnv(0xcbf2_9ce4_8422_2325);
        for _ in 0..7 {
            let layers: Vec<Vec<Packet>> = Layer::ALL
                .iter()
                .map(|&l| {
                    (0..tx.tsps_per_frame(l))
                        .map(|_| {
                            n += 1;
                            packet(n)
                        })
                        .collect()
                })
                .collect();
            let f = tx
                .transmit_frame([&layers[0], &layers[1], &layers[2]])
                .unwrap();
            for s in &f {
                h_tx.eat(&s.re.to_bits().to_le_bytes());
                h_tx.eat(&s.im.to_bits().to_le_bytes());
            }
            iq.extend(f);
        }
        let mut demod = Demodulator::new(DemodConfig::default());
        let mut h_llr = Fnv(0xcbf2_9ce4_8422_2325);
        let mut frames = 0;
        for chunk in iq.chunks(50_000) {
            for f in demod.push(chunk) {
                frames += 1;
                for l in f.llr.iter().flatten() {
                    for v in l {
                        h_llr.eat(&v.to_bits().to_le_bytes());
                    }
                }
                h_llr.eat(&f.metrics.n0.to_bits().to_le_bytes());
            }
        }
        let mut rx = Receiver::new(DemodConfig::default());
        let mut h_rx = Fnv(0xcbf2_9ce4_8422_2325);
        for chunk in iq.chunks(50_000) {
            for f in rx.push(chunk).unwrap() {
                for p in &f.ts {
                    h_rx.eat(p);
                }
            }
        }
        println!(
            "BITEXACT {:?} {:?} tx {:016x} llr {:016x} ({frames} frames) rx {:016x}",
            p.mode(),
            p.guard(),
            h_tx.0,
            h_llr.0,
            h_rx.0
        );
    }
}

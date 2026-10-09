//! End-to-end PHY loopback: TS packets → Transmitter → channel → Receiver
//! → TS packets, byte-exact, with the multiplex-frame reproduction.

use isdbt_core::{CodeRate, GuardInterval, LayerConfig, Mode, Modulation, Params, TimeInterleave};
use isdbt_ofdm::{C32, DemodConfig};
use isdbt_phy::{Packet, Receiver, Transmitter};

fn bench_params() -> Params {
    let a = LayerConfig {
        segments: 1,
        modulation: Modulation::Qpsk,
        code_rate: CodeRate::R2_3,
        time_interleave: TimeInterleave(2),
    };
    let b = LayerConfig {
        segments: 12,
        modulation: Modulation::Qam64,
        code_rate: CodeRate::R3_4,
        time_interleave: TimeInterleave(2),
    };
    Params::new(
        Mode::M3,
        GuardInterval::G1_8,
        true,
        [Some(a), Some(b), None],
    )
    .unwrap()
}

/// Deterministic packet whose payload encodes (layer, frame, index).
fn packet(layer: u8, frame: u32, idx: u32) -> Packet {
    let mut p = [0u8; 188];
    p[0] = 0x47;
    p[1] = 0x01;
    p[2] = layer;
    p[3] = 0x10;
    p[4..8].copy_from_slice(&frame.to_be_bytes());
    p[8..12].copy_from_slice(&idx.to_be_bytes());
    let mut x = frame.wrapping_mul(2654435761) ^ idx ^ ((layer as u32) << 24);
    for b in &mut p[12..] {
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        *b = x as u8;
    }
    p
}

fn awgn(s: &mut [C32], snr_db: f32, seed: &mut u64) {
    let p: f32 = s.iter().map(|c| c.norm_sqr()).sum::<f32>() / s.len() as f32;
    let sigma = (p / 10f32.powf(snr_db / 10.0) / 2.0).sqrt();
    let mut next = || {
        *seed ^= *seed << 13;
        *seed ^= *seed >> 7;
        *seed ^= *seed << 17;
        (*seed >> 11) as f32 / (1u64 << 53) as f32
    };
    for c in s.iter_mut() {
        let (u1, u2) = (next().max(1e-12), next());
        let r = (-2.0 * u1.ln()).sqrt() * sigma;
        let t = 2.0 * std::f32::consts::PI * u2;
        *c += C32::new(r * t.cos(), r * t.sin());
    }
}

#[test]
fn ts_loopback_hierarchical_mode3() {
    let params = bench_params();
    let mut tx = Transmitter::new(params.clone());
    let (na, nb) = (
        tx.tsps_per_frame(isdbt_core::Layer::A),
        tx.tsps_per_frame(isdbt_core::Layer::B),
    );
    assert_eq!((na, nb), (64, 2592));
    let frames = 7u32;
    let mut iq = vec![C32::new(0.0, 0.0); 23_456];
    let mut sent: Vec<[Vec<Packet>; 2]> = Vec::new();
    for f in 0..frames {
        let a: Vec<Packet> = (0..na as u32).map(|i| packet(0xA, f, i)).collect();
        let b: Vec<Packet> = (0..nb as u32).map(|i| packet(0xB, f, i)).collect();
        iq.extend(tx.transmit_frame([&a, &b, &[]]).unwrap());
        sent.push([a, b]);
    }
    // Channel: CFO of +1.7 carrier spacings, 28 dB SNR.
    let df = 1.7 / params.mode.fft_size() as f32;
    for (n, c) in iq.iter_mut().enumerate() {
        *c *= C32::from_polar(1.0, 2.0 * std::f32::consts::PI * df * n as f32);
    }
    let mut seed = 0x1234_5678_9abc_def0;
    awgn(&mut iq, 28.0, &mut seed);

    let mut rx = Receiver::new(DemodConfig::default());
    let mut got = Vec::new();
    for chunk in iq.chunks(100_003) {
        got.extend(rx.push(chunk).unwrap());
    }
    let valid: Vec<_> = got.iter().filter(|f| f.valid).collect();
    assert!(!valid.is_empty(), "no valid frames out of {}", got.len());
    // Frames are counted from lock; recover the TX index from packet contents.
    let mut checked = 0;
    for f in &valid {
        let a = f.layers[0].as_ref().unwrap();
        let b = f.layers[1].as_ref().unwrap();
        let txf = u32::from_be_bytes(b[0][4..8].try_into().unwrap()) as usize;
        assert_eq!(a, &sent[txf][0], "layer A frame {txf}");
        assert_eq!(b, &sent[txf][1], "layer B frame {txf}");
        assert_eq!(f.rs[1].unwrap().uncorrectable, 0);
        assert_eq!(f.ts.len(), f.params.multiplex_frame_tsps());
        let a_slots = f.ts.iter().filter(|p| p[2] == 0xA).count();
        assert_eq!(a_slots, na);
        checked += 1;
    }
    eprintln!(
        "checked {checked} frames byte-exact; mer {:?}",
        valid[0].metrics.mer_db
    );
}

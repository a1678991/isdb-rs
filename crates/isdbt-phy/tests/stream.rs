//! Stream boundaries of the PHY pipelines: pre-roll (no fill peaks), lead-in
//! / flush frame counts that deliver every input packet, and the pipelined
//! receiver matching the inline one.

use isdbt_core::{
    CodeRate, GuardInterval, Layer, LayerConfig, Mode, Modulation, Params, Segments, TimeInterleave,
};
use isdbt_ofdm::{C32, DemodConfig};
use isdbt_phy::{Packet, PipelinedReceiver, Receiver, RxFrame, Transmitter, null_packet};

fn params(mode: Mode, ti: TimeInterleave) -> Params {
    let a = LayerConfig {
        segments: Segments::new(1).unwrap(),
        modulation: Modulation::Qpsk,
        code_rate: CodeRate::R2_3,
        time_interleave: ti,
    };
    let b = LayerConfig {
        segments: Segments::new(12).unwrap(),
        modulation: Modulation::Qam16,
        code_rate: CodeRate::R1_2,
        time_interleave: ti,
    };
    Params::new(mode, GuardInterval::G1_8, true, [Some(a), Some(b), None]).unwrap()
}

fn packet(layer: u8, n: u32) -> Packet {
    let mut p = [0u8; 188];
    p[0] = 0x47;
    p[1] = 0x01;
    p[2] = layer;
    p[3] = 0x10 | (n & 15) as u8;
    p[4..8].copy_from_slice(&n.to_be_bytes());
    let mut x = n.wrapping_mul(2654435761) | 1;
    for b in &mut p[8..] {
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        *b = x.to_le_bytes()[0];
    }
    p
}

/// TX with lead-in and flush around `data` frames of distinct packets;
/// returns the samples and the input packets per layer.
fn transmit(p: &Params, preroll: bool, data: u32) -> (Vec<C32>, [Vec<Packet>; 2]) {
    let mut tx = Transmitter::new(p.clone());
    if preroll {
        tx.preroll().unwrap();
    }
    let (na, nb) = (tx.tsps_per_frame(Layer::A), tx.tsps_per_frame(Layer::B));
    let mut iq = Vec::new();
    for _ in 0..tx.lead_frames() {
        iq.extend(tx.transmit_null_frame().unwrap());
    }
    let mut sent: [Vec<Packet>; 2] = Default::default();
    for f in 0..data {
        let a: Vec<_> = (0..u32::try_from(na).unwrap())
            .map(|i| packet(0, f * 100_000 + i))
            .collect();
        let b: Vec<_> = (0..u32::try_from(nb).unwrap())
            .map(|i| packet(1, f * 100_000 + i))
            .collect();
        iq.extend(tx.transmit_frame([&a, &b, &[]]).unwrap());
        sent[0].extend(a);
        sent[1].extend(b);
    }
    iq.extend(tx.finish().unwrap());
    for _ in 1..tx.flush_frames() {
        iq.extend(tx.transmit_null_frame().unwrap());
    }
    (iq, sent)
}

fn non_null(frames: &[RxFrame], l: usize) -> Vec<Packet> {
    let n = null_packet();
    frames
        .iter()
        .filter(|f| f.valid)
        .flat_map(|f| f.layers[l].clone().unwrap())
        .filter(|p| *p != n)
        .collect()
}

#[test]
fn lead_and_flush_deliver_every_packet() {
    for (mode, ti) in [
        (Mode::M1, TimeInterleave::K3),
        (Mode::M3, TimeInterleave::K2),
        (Mode::M3, TimeInterleave::K0),
    ] {
        let p = params(mode, ti);
        let (iq, sent) = transmit(&p, true, 3);
        let mut rx = Receiver::new(DemodConfig::default());
        let mut frames = Vec::new();
        for c in iq.chunks(77_777) {
            frames.extend(rx.push(c).unwrap());
        }
        assert_eq!(non_null(&frames, 0), sent[0], "{mode:?} TI{ti} layer A");
        assert_eq!(non_null(&frames, 1), sent[1], "{mode:?} TI{ti} layer B");
    }
}

#[test]
fn preroll_removes_fill_peaks() {
    let p = params(Mode::M3, TimeInterleave::K2);
    let peak = |iq: &[C32]| iq.iter().map(|c| c.norm()).fold(0f32, f32::max);
    let rms = |iq: &[C32]| (iq.iter().map(|c| c.norm_sqr()).sum::<f32>() / iq.len() as f32).sqrt();
    let (raw, _) = transmit(&p, false, 1);
    let (pre, _) = transmit(&p, true, 1);
    // Without pre-roll the FEC encoder's zero start-up fill (byte
    // interleaver / delay adjustment, not energy-dispersed) maps to
    // identical carriers (> 30 dB PAPR); with it the PAPR is that of
    // ordinary OFDM (≈ 12 dB at 1e-7).  The raw figure is only reported,
    // so randomising the fill at its source does not break this test.
    let raw_db = 20.0 * (peak(&raw) / rms(&raw)).log10();
    eprintln!("start-up PAPR without pre-roll: {raw_db:.1} dB");
    assert!(
        20.0 * (peak(&pre) / rms(&pre)).log10() < 15.0,
        "PAPR {}",
        peak(&pre) / rms(&pre)
    );
}

#[test]
fn pipelined_receiver_matches_inline() {
    let p = params(Mode::M1, TimeInterleave::K2);
    let (iq, _) = transmit(&p, true, 4);
    let mut rx = Receiver::new(DemodConfig::default());
    let inline: Vec<RxFrame> = rx.push(&iq).unwrap();
    let mut prx = PipelinedReceiver::new(DemodConfig::default(), 2).unwrap();
    let mut piped = Vec::new();
    for c in iq.chunks(50_000) {
        prx.push(c.to_vec());
        while let Some(f) = prx.try_recv() {
            piped.push(f.unwrap());
        }
    }
    piped.extend(prx.finish().into_iter().map(|f| f.unwrap()));
    assert_eq!(inline.len(), piped.len());
    for (a, b) in inline.iter().zip(&piped) {
        assert_eq!((a.tx_frame, a.valid), (b.tx_frame, b.valid));
        assert_eq!(a.layers, b.layers);
        assert_eq!(a.ts, b.ts);
    }
}

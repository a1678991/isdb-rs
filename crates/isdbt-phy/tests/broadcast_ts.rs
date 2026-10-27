//! Broadcasting TS (B31 Part 2 §5.5) end to end: per-layer packets →
//! `BroadcastTsWriter` (204-byte TSPs with ISDB-T_information and one IIP
//! per frame) → `BroadcastTsReader` → a `Transmitter` configured from the
//! IIP alone → channel → `Receiver`; the received packets equal the input
//! and the receiver's reproduced TS has the layer arrangement the
//! dummy-byte `layer_indicator`s announced.

// Narrowing casts are proven or fallible here (docs/plans/type-safety.md section 2).
#![warn(clippy::cast_possible_truncation)]
use isdbt_core::{
    CodeRate, GuardInterval, Layer, LayerConfig, Mode, Modulation, Params, Segments, TimeInterleave,
};
use isdbt_ofdm::DemodConfig;
use isdbt_phy::broadcast_ts::{
    BroadcastFrame, BroadcastTsReader, BroadcastTsWriter, IsdbtInformation, LayerIndicator,
};
use isdbt_phy::{Packet, Receiver, Transmitter, null_packet};

fn params() -> Params {
    let a = LayerConfig {
        segments: Segments::new(1).unwrap(),
        modulation: Modulation::Qpsk,
        code_rate: CodeRate::R2_3,
        time_interleave: TimeInterleave::K1,
    };
    let b = LayerConfig {
        segments: Segments::new(12).unwrap(),
        modulation: Modulation::Qam16,
        code_rate: CodeRate::R1_2,
        time_interleave: TimeInterleave::K1,
    };
    Params::new(
        Mode::M1,
        GuardInterval::G1_8,
        true,
        [Some(a), Some(b), None],
    )
    .unwrap()
}

fn packet(layer: u8, n: u32) -> Packet {
    let mut p = [0u8; 188];
    p[0] = 0x47;
    p[1] = 0x01;
    p[2] = layer;
    p[3] = 0x10 | (n & 15) as u8;
    p[4..8].copy_from_slice(&n.to_be_bytes());
    let mut x = n.wrapping_mul(2_654_435_761) | 1;
    for b in &mut p[8..] {
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        *b = x.to_le_bytes()[0];
    }
    p
}

#[test]
fn transmitter_configured_from_the_iip_delivers_every_packet() {
    let p = params();
    let mut writer = BroadcastTsWriter::new(p.clone()).unwrap();
    let (na, nb) = (
        writer.pattern().count(Layer::A),
        writer.pattern().count(Layer::B),
    );

    // The broadcasting TS: three data frames, as a flat byte stream.
    let mut stream = Vec::new();
    let mut sent: [Vec<Packet>; 2] = Default::default();
    for f in 0..3u32 {
        let a: Vec<_> = (0..u32::try_from(na).unwrap())
            .map(|i| packet(0, f * 100_000 + i))
            .collect();
        let b: Vec<_> = (0..u32::try_from(nb).unwrap())
            .map(|i| packet(1, f * 100_000 + i))
            .collect();
        for tsp in writer.frame([&a, &b, &[]]).unwrap() {
            stream.extend_from_slice(&tsp);
        }
        sent[0].extend(a);
        sent[1].extend(b);
    }

    // Read it back frame by frame.
    let mut reader = BroadcastTsReader::new();
    let mut frames: Vec<BroadcastFrame> = Vec::new();
    for unit in stream.as_chunks::<204>().0 {
        frames.extend(reader.push(unit));
    }
    frames.extend(reader.finish());
    assert_eq!(frames.len(), 3);
    assert_eq!(reader.dropped(), 0);

    // The transmitter takes its parameters from the stream, not from `p`.
    let rx_params = frames[0].params().unwrap();
    assert_eq!(rx_params, p);
    let mut tx = Transmitter::new(rx_params);
    tx.preroll().unwrap();
    let mut iq = Vec::new();
    for _ in 0..tx.lead_frames() {
        iq.extend(tx.transmit_null_frame().unwrap());
    }
    for f in &frames {
        assert_eq!(f.parity_errors, 0);
        assert!(f.counter_ok());
        assert_eq!(f.slots(), tx.pattern().slots());
        let [a, b, c] = f.divide();
        iq.extend(tx.transmit_frame([&a, &b, &c]).unwrap());
    }
    iq.extend(tx.finish().unwrap());
    for _ in 1..tx.flush_frames() {
        iq.extend(tx.transmit_null_frame().unwrap());
    }

    let mut rx = Receiver::new(DemodConfig::default());
    let got = rx.push(&iq).unwrap();
    let non_null = |l: usize| -> Vec<Packet> {
        got.iter()
            .filter(|f| f.valid)
            .flat_map(|f| f.layers[l].clone().unwrap())
            .filter(|p| *p != null_packet())
            .collect()
    };
    assert_eq!(non_null(0), sent[0]);
    assert_eq!(non_null(1), sent[1]);

    // The receiver's §3.2.2 reproduced TS of a data frame has the same
    // arrangement as the broadcasting TS announced slot by slot (the IIP
    // slot is a null slot on air).
    let data = got
        .iter()
        .find(|f| f.valid && f.layers[0].as_ref().unwrap()[0] == sent[0][0])
        .expect("first data frame received");
    let announced: Vec<Option<Layer>> = frames[0]
        .info
        .iter()
        .map(|i| i.layer_indicator.slot())
        .collect();
    let on_air: Vec<Option<Layer>> = data
        .ts
        .iter()
        .zip(tx.pattern().slots())
        .map(|(pkt, slot)| if *pkt == null_packet() { None } else { *slot })
        .collect();
    assert_eq!(on_air, announced);
    assert_eq!(
        frames[0].info[frames[0].iip_slot.unwrap()].layer_indicator,
        LayerIndicator::Iip
    );
    assert!(
        frames[0]
            .info
            .iter()
            .all(|i| i.tmcc_identifier == IsdbtInformation::TERRESTRIAL_TV)
    );
}

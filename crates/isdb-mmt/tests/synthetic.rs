//! End to end over synthetic TLV packets: an MPT announcing an HEVC and an
//! MPEG-4 audio asset, then unscrambled MPU payloads (whole, divided and
//! aggregated MFUs) — the decoder must find the assets, reassemble the
//! MFUs, and the elementary-stream framing must give an Annex B stream and
//! LOAS frames. On air every media payload is scrambled (SPEC-NOTES
//! "scrambled-mpu"), so this is the only check of the unscrambled path.

use isdb_mmt::Decoder;
use isdb_mmt::es;
use isdb_tlv::tlv::{Packet, PacketType};

/// A TLV packet: IPv6 + UDP to `port`, carrying `payload`.
fn ipv6_udp(port: u16, payload: &[u8]) -> Packet {
    let mut b = vec![0x60, 0, 0, 0];
    b.extend(((payload.len() + 8) as u16).to_be_bytes());
    b.extend([17, 64]);
    b.extend([0x20; 16]);
    b.extend([0xFF; 16]);
    b.extend(1234u16.to_be_bytes());
    b.extend(port.to_be_bytes());
    b.extend(((payload.len() + 8) as u16).to_be_bytes());
    b.extend([0, 0]);
    b.extend(payload);
    Packet {
        kind: PacketType::Ipv6,
        data: b,
    }
}

/// An MMTP packet (version 0, no counter, no extension).
fn mmtp(payload_type: u8, packet_id: u16, seq: u32, payload: &[u8]) -> Vec<u8> {
    let mut b = vec![0x00, payload_type];
    b.extend(packet_id.to_be_bytes());
    b.extend(0u32.to_be_bytes());
    b.extend(seq.to_be_bytes());
    b.extend(payload);
    b
}

/// A PA message carrying one MPT with the given (asset type, packet id).
fn pa_with_mpt(assets: &[(&[u8; 4], u16)]) -> Vec<u8> {
    let mut t = vec![0x20, 1, 0, 0, 0xFC, 2, 0x00, 0x97, 0, 0, assets.len() as u8];
    for (ty, pid) in assets {
        t.extend([0x00, 0, 0, 0, 0, 0]);
        t.extend(*ty);
        t.extend([0xFE, 1, 0x00]);
        t.extend(pid.to_be_bytes());
        t.extend([0, 0]);
    }
    let len = (t.len() - 4) as u16;
    t[2..4].copy_from_slice(&len.to_be_bytes());
    let mut m = vec![0x00, 0x00, 0x00];
    m.extend(((t.len() + 1) as u32).to_be_bytes());
    m.push(0);
    m.extend(&t);
    let mut p = vec![0x3C, 0x00];
    p.extend(m);
    p
}

/// An MPU payload with one timed MFU (or fragment of one).
fn mpu(frag: u8, mpu_seq: u32, data: &[u8]) -> Vec<u8> {
    let mut p = vec![0, 0, 0x20 | 0x08 | (frag << 1), 0];
    p.extend(mpu_seq.to_be_bytes());
    p.extend([0u8; 14]);
    p.extend(data);
    let len = (p.len() - 2) as u16;
    p[..2].copy_from_slice(&len.to_be_bytes());
    p
}

/// An aggregated MPU payload of several whole timed MFUs.
fn mpu_aggregated(mpu_seq: u32, units: &[&[u8]]) -> Vec<u8> {
    let mut p = vec![0, 0, 0x20 | 0x08 | 1, 0];
    p.extend(mpu_seq.to_be_bytes());
    for u in units {
        p.extend(((u.len() + 14) as u16).to_be_bytes());
        p.extend([0u8; 14]);
        p.extend(*u);
    }
    let len = (p.len() - 2) as u16;
    p[..2].copy_from_slice(&len.to_be_bytes());
    p
}

fn nal(t: u8, len: usize) -> Vec<u8> {
    let mut n = vec![t << 1, 1];
    n.extend((0..len).map(|i| (i as u8) | 0x80));
    let mut m = (n.len() as u32).to_be_bytes().to_vec();
    m.extend(n);
    m
}

#[test]
fn unscrambled_media_reaches_annex_b_and_loas() {
    let (video, audio) = (0xF100u16, 0xF110u16);
    let mut d = Decoder::new();
    let mut mfus = Vec::new();
    let mut seq = [0u32; 3];
    let mut send = |d: &mut Decoder, mfus: &mut Vec<_>, k: usize, pt: u8, id: u16, p: Vec<u8>| {
        seq[k] += 1;
        d.push(&ipv6_udp(30000, &mmtp(pt, id, seq[k], &p)), mfus);
    };
    send(
        &mut d,
        &mut mfus,
        0,
        2,
        0x0000,
        pa_with_mpt(&[(b"hev1", video), (b"mp4a", audio)]),
    );
    // VPS, SPS, PPS aggregated; then an IRAP slice divided in three
    let (vps, sps, pps) = (nal(32, 10), nal(33, 20), nal(34, 5));
    send(
        &mut d,
        &mut mfus,
        1,
        0,
        video,
        mpu_aggregated(7, &[&vps, &sps, &pps]),
    );
    let slice = nal(19, 3000);
    let (a, rest) = slice.split_at(1000);
    let (b, c) = rest.split_at(1000);
    send(&mut d, &mut mfus, 1, 0, video, mpu(1, 7, a));
    send(&mut d, &mut mfus, 1, 0, video, mpu(2, 7, b));
    send(&mut d, &mut mfus, 1, 0, video, mpu(3, 7, c));
    // two AudioMuxElements
    send(&mut d, &mut mfus, 2, 0, audio, mpu(0, 40, &[0x20; 300]));
    send(&mut d, &mut mfus, 2, 0, audio, mpu(0, 41, &[0x21; 280]));

    let (svc, asset) = d.asset_of(video).unwrap();
    assert_eq!((svc, asset.type_str().as_str()), (Some(0x0097), "hev1"));
    let mut annex_b = Vec::new();
    let mut loas = Vec::new();
    let mut types = Vec::new();
    for m in &mfus {
        assert!(!m.scrambled);
        match m.packet_id {
            p if p == video => types.extend(es::hevc_annex_b(&m.data, &mut annex_b).unwrap()),
            p if p == audio => assert!(es::loas_frame(&m.data, &mut loas)),
            _ => panic!("unexpected packet id"),
        }
    }
    assert_eq!(types, vec![32, 33, 34, 19]);
    // the Annex B stream: four start codes, the slice whole
    let starts = annex_b.windows(4).filter(|w| *w == es::START_CODE).count();
    assert_eq!(starts, 4);
    assert!(annex_b.ends_with(&slice[4..]));
    // LOAS: two frames with syncword 0x2B7 and the right lengths
    assert_eq!(u16::from_be_bytes([loas[0], loas[1]]) >> 5, 0x2B7);
    assert_eq!(
        usize::from(u16::from_be_bytes([loas[1], loas[2]]) & 0x1FFF),
        300
    );
    assert_eq!(loas.len(), 3 + 300 + 3 + 280);
    let st: Vec<_> = d
        .mpu_stats()
        .map(|(k, s)| (k, s.mfus, s.mfus_dropped, s.packets_lost))
        .collect();
    assert_eq!(st, vec![(video, 4, 0, 0), (audio, 2, 0, 0)]);
}

#[test]
fn a_lost_fragment_drops_one_mfu_and_is_counted() {
    let video = 0xF100u16;
    let mut d = Decoder::new();
    let mut mfus = Vec::new();
    d.push(
        &ipv6_udp(30000, &mmtp(2, 0, 1, &pa_with_mpt(&[(b"hev1", video)]))),
        &mut mfus,
    );
    let slice = nal(1, 2000);
    let (a, rest) = slice.split_at(700);
    let (_lost, c) = rest.split_at(700);
    d.push(
        &ipv6_udp(30000, &mmtp(0, video, 10, &mpu(1, 3, a))),
        &mut mfus,
    );
    // packet 11 lost
    d.push(
        &ipv6_udp(30000, &mmtp(0, video, 12, &mpu(3, 3, c))),
        &mut mfus,
    );
    d.push(
        &ipv6_udp(30000, &mmtp(0, video, 13, &mpu(0, 4, &nal(1, 50)))),
        &mut mfus,
    );
    assert_eq!(mfus.len(), 1);
    let s = d.mpu_stats().next().unwrap().1;
    assert_eq!(
        (s.packets_lost, s.mfus_dropped, s.mfus, s.mpus),
        (1, 1, 1, 2)
    );
}

#[test]
fn ntp_is_not_taken_for_mmtp() {
    let mut d = Decoder::new();
    let mut ntp = vec![0x24, 1, 0, 0];
    ntp.resize(48, 0);
    ntp[40..48].copy_from_slice(&((2_208_988_800u64 + 100) << 32).to_be_bytes());
    d.push(&ipv6_udp(123, &ntp), &mut Vec::new());
    assert_eq!(d.counters.ntp, 1);
    assert_eq!(d.ntp.map(isdb_mmt::desc::ntp_to_unix), Some(100.0));
    assert_eq!(d.mpu_stats().count(), 0);
}

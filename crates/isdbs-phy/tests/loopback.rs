//! Symbol-domain loopback: TS packets per TS → `tx::Transmitter` → carrier
//! offset, phase and AWGN → `receive` → `ts::decode_all` → the same
//! packets per TS_ID, byte-exact, under the TMCC sent and in the slots the
//! transmitter reported. Sample domain: the same through the transmit
//! pulse shaper (`isdb_frontend::shape::Shaper`) at 40 Msps and the
//! receive front end (`frontend::symbols`, carrier and trim found on the
//! signal).

use isdb_frontend::shape::Shaper;
use isdbs_core::combination::Combination;
use isdbs_core::frame::FRAME_SYMBOLS;
use isdbs_core::tmcc::TmccInfo;
use isdbs_phy::demod::DemodOptions;
use isdbs_phy::tx::{NULL_PACKET, Packet, SUPERFRAME_SYMBOLS, Transmitter};
use isdbs_phy::{Cplx, RxOptions, frontend, receive, ts};
use num_complex::Complex32;

/// Deterministic packet whose payload encodes (relative TS, superframe,
/// index); PID 0x0100.
fn packet(r: u8, sf: u32, idx: u32) -> Packet {
    let mut p = [0u8; 188];
    p[0] = 0x47;
    p[1] = 0x01;
    p[2] = r;
    p[3] = 0x10;
    p[4..8].copy_from_slice(&sf.to_be_bytes());
    p[8..12].copy_from_slice(&idx.to_be_bytes());
    let mut x = (sf.wrapping_mul(2_654_435_761) ^ idx ^ (u32::from(r) << 24)) | 1;
    for b in &mut p[12..] {
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        *b = x.to_le_bytes()[0];
    }
    p
}

/// xorshift64, uniform in (0, 1].
struct Rng(u64);

impl Rng {
    fn uniform(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        ((self.0 >> 11) as f64 + 1.0) / (1u64 << 53) as f64
    }

    /// Complex Gaussian of variance `var` (`var / 2` per component).
    fn gauss(&mut self, var: f64) -> Cplx {
        let r = (-var * self.uniform().ln()).sqrt();
        Cplx::from_polar(r, std::f64::consts::TAU * self.uniform())
    }
}

/// Noise-only symbols before the first frame, so the receiver acquires
/// off symbol 0.
const LEAD: usize = 1_111;
/// Data superframes after the null one.
const DATA_SUPERFRAMES: u32 = 3;
/// Carrier offset, cycles per symbol (≈ 5.8 kHz at 28.86 Mbaud).
const CFO: f64 = 2e-4;

/// Totals of one loopback, for the report line.
struct Outcome {
    packets: usize,
    corrected_bytes: usize,
    worst_packet: usize,
    mer_db: f64,
}

/// One null superframe and three of distinct packets through the
/// transmitter, the channel (CFO, phase, AWGN at `snr_db` Es/N0) and the
/// receiver: everything sent comes back.
fn loopback(
    combinations: &[(Combination, u8)],
    ts_ids: &[(u16, u8)],
    snr_db: f64,
    seed: u64,
) -> Outcome {
    let info = TmccInfo::assign(combinations, ts_ids).unwrap();
    let mut tx = Transmitter::new(info.clone()).unwrap();
    assert_eq!(tx.ts_count(), ts_ids.len());
    let mut z = vec![Cplx::new(0.0, 0.0); LEAD];
    let null = tx.null_superframe();
    z.extend(&null.symbols);
    let mut placed = vec![null.placed];
    let mut sent: Vec<Vec<Vec<Packet>>> = Vec::new();
    for sf in 0..DATA_SUPERFRAMES {
        let per_ts: Vec<Vec<Packet>> = (0..tx.ts_count())
            .map(|r| {
                let n = u32::try_from(tx.packets_per_superframe(r)).unwrap();
                (0..n)
                    .map(|i| packet(u8::try_from(r).unwrap(), sf, i))
                    .collect()
            })
            .collect();
        let refs: Vec<&[Packet]> = per_ts.iter().map(Vec::as_slice).collect();
        let s = tx.superframe(&refs).unwrap();
        assert_eq!(s.symbols.len(), SUPERFRAME_SYMBOLS);
        z.extend(&s.symbols);
        placed.push(s.placed);
        sent.push(per_ts);
    }
    assert_eq!(tx.superframes(), u64::from(DATA_SUPERFRAMES) + 1);
    // channel
    let mut rng = Rng(seed | 1);
    let var = 10f64.powf(-snr_db / 10.0);
    for (n, v) in z.iter_mut().enumerate() {
        let rot = Cplx::from_polar(1.0, 0.9 + std::f64::consts::TAU * CFO * n as f64);
        *v = *v * rot + rng.gauss(var);
    }
    let r = receive(&z, &RxOptions::default());
    assert_eq!(r.slips, 0);
    assert_eq!(r.frames[0].mark.start, LEAD);
    assert_eq!(r.superframes.len(), 1 + sent.len());
    let d = ts::decode_all(&r, &DemodOptions::default());
    assert_eq!(d.len(), 1 + sent.len());
    let mut out = Outcome {
        packets: 0,
        corrected_bytes: 0,
        worst_packet: 0,
        mer_db: 0.0,
    };
    for (k, dsf) in d.iter().enumerate() {
        assert_eq!(dsf.info, info, "superframe {k}: TMCC");
        assert_eq!(r.superframes[k].info.as_ref(), Some(&info));
        let got: Vec<(usize, usize, u16)> = dsf
            .packets
            .iter()
            .map(|p| (p.frame, p.slot, p.ts_id))
            .collect();
        assert_eq!(got, placed[k], "superframe {k}: slots");
        for p in &dsf.packets {
            let c = p.corrected.unwrap_or_else(|| {
                panic!(
                    "superframe {k}: frame {} slot {} uncorrectable",
                    p.frame, p.slot
                )
            });
            out.corrected_bytes += c;
            out.worst_packet = out.worst_packet.max(c);
        }
        out.packets += dsf.packets.len();
        out.mer_db += dsf.mer_db / d.len() as f64;
        if k == 0 {
            assert!(dsf.packets.iter().all(|p| p.bytes == NULL_PACKET));
            continue;
        }
        for (rel, &(id, _)) in ts_ids.iter().enumerate() {
            let back: Vec<Packet> = dsf
                .packets
                .iter()
                .filter(|p| p.ts_id == id)
                .map(|p| p.bytes)
                .collect();
            assert!(
                back == sent[k - 1][rel],
                "superframe {k}, TS_ID {id:#06x}: packets differ"
            );
        }
    }
    // few RS corrections: well inside the code's 8 bytes per packet
    assert!(
        out.corrected_bytes * 20 < out.packets,
        "{} bytes corrected in {} packets",
        out.corrected_bytes,
        out.packets
    );
    assert!(
        (out.mer_db - snr_db).abs() < 1.5,
        "main MER {:.1} dB at {snr_db} dB",
        out.mer_db
    );
    eprintln!(
        "{}: {snr_db} dB, MER {:.1} dB, {} packets byte-exact, {} RS byte(s) corrected (at most {} in a packet)",
        isdbs_phy::rx::describe_tmcc(&info),
        out.mer_db,
        out.packets,
        out.corrected_bytes,
        out.worst_packet
    );
    out
}

/// The BS layout: TC8PSK 2/3 × 48, three TSs of 16 slots, 12 dB.
#[test]
fn bs_layout_three_ts() {
    loopback(
        &[(Combination::Tc8psk2_3, 48)],
        &[(0x4010, 16), (0x4011, 16), (0x4012, 16)],
        12.0,
        1,
    );
}

/// The CS layout: QPSK 3/4 × 48, one TS of 36 sent slots, 8 dB.
#[test]
fn cs_layout_one_ts() {
    loopback(&[(Combination::Qpsk3_4, 48)], &[(0x4010, 36)], 8.0, 2);
}

/// TC8PSK 2/3 × 44 + BPSK 1/2 × 4 (45 sent slots), two TSs, 12 dB.
#[test]
fn tc8psk_and_bpsk() {
    loopback(
        &[(Combination::Tc8psk2_3, 44), (Combination::Bpsk1_2, 4)],
        &[(0x4010, 30), (0x4011, 15)],
        12.0,
        3,
    );
}

/// QPSK 7/8 × 40 + QPSK 1/2 × 8 (39 sent slots), three TSs.
#[test]
fn qpsk_7_8_and_1_2() {
    loopback(
        &[(Combination::Qpsk1_2, 8), (Combination::Qpsk7_8, 40)],
        &[(0x4010, 20), (0x4011, 12), (0x4012, 7)],
        8.0,
        4,
    );
}

/// QPSK 5/6 × 30 + QPSK 2/3 × 18 (37 sent slots), two TSs.
#[test]
fn qpsk_5_6_and_2_3() {
    loopback(
        &[(Combination::Qpsk5_6, 30), (Combination::Qpsk2_3, 18)],
        &[(0x4010, 25), (0x4011, 12)],
        8.0,
        5,
    );
}

/// The sample domain at 40 Msps (1.386 samples per symbol), the BS layout:
/// one null superframe, three of distinct packets and one null frame (so
/// the filter tails do not cut the last superframe) → `Shaper` (α = 0.35)
/// at RMS 0.3 of the front end's full scale (10.5 dB below it) →
/// `frontend::symbols` (the survey's carrier, the trim measured on the
/// bursts) → `receive` → `ts::decode_all`: every packet byte-exact per
/// TS_ID, none uncorrectable, and the trim found within ±0.05 symbol of 0
/// (the timing detector is unbiased on a clean pulse, `isdb-frontend`
/// README "The timing trim").
#[test]
fn sample_domain_bs_layout_at_40_msps() {
    let ts_ids = [(0x4010, 16), (0x4011, 16), (0x4012, 16)];
    let info = TmccInfo::assign(&[(Combination::Tc8psk2_3, 48)], &ts_ids).unwrap();
    let mut tx = Transmitter::new(info.clone()).unwrap();
    let sys = frontend::system();
    let fs = 40e6;
    let mut sh = Shaper::new(fs, sys.symbol_rate_hz, sys.roll_off).unwrap();
    let mut x = Vec::new();
    let push = |sh: &mut Shaper, z: &[Cplx], x: &mut Vec<Complex32>| {
        // unit-modulus f64 symbols rounded to the shaper's f32 on purpose
        #[allow(clippy::cast_possible_truncation)]
        let v: Vec<Complex32> = z
            .iter()
            .map(|c| Complex32::new(c.re as f32, c.im as f32))
            .collect();
        sh.push(&v, x);
    };
    push(&mut sh, &tx.null_superframe().symbols, &mut x);
    let mut sent: Vec<Vec<Vec<Packet>>> = Vec::new();
    for sf in 0..DATA_SUPERFRAMES {
        let per_ts: Vec<Vec<Packet>> = (0..tx.ts_count())
            .map(|r| {
                let n = u32::try_from(tx.packets_per_superframe(r)).unwrap();
                (0..n)
                    .map(|i| packet(u8::try_from(r).unwrap(), sf, i))
                    .collect()
            })
            .collect();
        let refs: Vec<&[Packet]> = per_ts.iter().map(Vec::as_slice).collect();
        push(&mut sh, &tx.superframe(&refs).unwrap().symbols, &mut x);
        sent.push(per_ts);
    }
    push(
        &mut sh,
        &tx.null_superframe().symbols[..FRAME_SYMBOLS],
        &mut x,
    );
    sh.finish(&mut x);
    let symbols = 4 * SUPERFRAME_SYMBOLS + FRAME_SYMBOLS + 2 * sh.span() - 1;
    assert_eq!(x.len() as u64, sh.samples_for(symbols as u64));
    for v in &mut x {
        *v *= 0.3;
    }
    let t = std::time::Instant::now();
    let s = frontend::symbols(&x, fs, None).unwrap();
    let front_s = t.elapsed().as_secs_f64();
    assert!(
        s.trim.abs() <= 0.05,
        "trim {:+.3} symbol on a clean pulse",
        s.trim
    );
    let r = receive(&s.z, &RxOptions::default());
    assert_eq!(r.slips, 0);
    let d = ts::decode_all(&r, &DemodOptions::default());
    // The front end's loops settle during the first symbols of the lead
    // superframe (its first frame starts at the filters' delay, ≈ 28
    // symbols in): its first slots, spread over its 8 frames by the
    // interleaver, may be uncorrectable, and the whole superframe may be
    // missed. The data superframes are always whole and intact.
    assert!(
        (3..=4).contains(&d.len()),
        "{} superframes decoded",
        d.len()
    );
    let data = &d[d.len() - 3..];
    let lead_lost = if d.len() == 4 {
        assert!(
            d[0].packets
                .iter()
                .all(|p| p.bytes == NULL_PACKET || p.corrected.is_none()),
            "the lead superframe carries null packets"
        );
        d[0].packets
            .iter()
            .filter(|p| p.corrected.is_none())
            .count()
    } else {
        0
    };
    let mut corrected = 0;
    for (k, dsf) in data.iter().enumerate() {
        assert_eq!(dsf.info, info, "data superframe {k}: TMCC");
        for p in &dsf.packets {
            corrected += p.corrected.unwrap_or_else(|| {
                panic!(
                    "data superframe {k}: frame {} slot {} uncorrectable",
                    p.frame, p.slot
                )
            });
        }
        for (rel, &(id, _)) in ts_ids.iter().enumerate() {
            let back: Vec<Packet> = dsf
                .packets
                .iter()
                .filter(|p| p.ts_id == id)
                .map(|p| p.bytes)
                .collect();
            assert!(
                back == sent[k][rel],
                "data superframe {k}, TS_ID {id:#06x}: packets differ"
            );
        }
    }
    let mer = data.iter().map(|d| d.mer_db).sum::<f64>() / 3.0;
    eprintln!(
        "40 Msps, BS layout: trim {:+.4} symbol, carrier {:+.0} Hz, symbol rate {:.1} Bd; main MER {mer:.1} dB; {} superframes decoded (lead: {lead_lost} uncorrectable), data RS-corrected bytes {corrected}; front end and trim search {front_s:.2} s",
        s.trim,
        s.carrier_offset_hz,
        s.symbol_rate_hz,
        d.len(),
    );
}

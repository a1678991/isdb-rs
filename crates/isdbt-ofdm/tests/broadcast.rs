//! TMCC words received from broadcasters (VERIFICATION.md "Broadcast
//! reception"): B20–B203 of frames from two local stations in Japan,
//! demodulated with no parity correction needed (the received words were
//! codewords as received). They check the (184,102) code and its bit
//! order, the Table 3-22..3-31 field decoding and the transmitter's TMCC
//! encoding against transmitters this project did not build.

use isdbt_core::{CodeRate, GuardInterval, LayerConfig, Mode, Modulation, Params, TimeInterleave};
use isdbt_ofdm::dsc::{self, ParityStatus};
use isdbt_ofdm::tmcc::{TmccInfo, decode_frame, sync_word};

/// Station 1: Mode 3, GI 1/8, partial reception, A 1 seg QPSK 2/3 I-index 3,
/// B 12 seg 64QAM 3/4 I-index 2, and no next information (B67–B106 all 1).
const NO_NEXT: &str = "0011110100100101100010110100101100111111111111111111111111111111111111111111111111111111111111111111111001011110000110000011000010000100111011101100010011011000010010101100011011110101";

/// Station 2: the same parameters, with the next information repeating
/// the current information.
const NEXT_IS_CURRENT: &str = "0011110100100101100010110100101100111111111111110010010110001011010010110011111111111111111111111111110010101111101000000110011100111110101110011100101101101110101000111110001010010110";

fn word(s: &str) -> Vec<u8> {
    s.bytes().map(|b| b - b'0').collect()
}

/// The full 204-bit frame around a broadcast B20–B203 word (coherent
/// segment type, sync word of an even frame).
fn frame(w: &[u8]) -> [u8; 204] {
    let mut f = [0u8; 204];
    f[1..17].copy_from_slice(&sync_word(false));
    f[20..].copy_from_slice(w);
    f
}

/// The broadcast configuration.
fn broadcast_params() -> Params {
    let layer = |segments, modulation, code_rate, ti| {
        Some(LayerConfig {
            segments,
            modulation,
            code_rate,
            time_interleave: ti,
        })
    };
    Params::new(
        Mode::M3,
        GuardInterval::G1_8,
        true,
        [
            layer(1, Modulation::Qpsk, CodeRate::R2_3, TimeInterleave::K3),
            layer(12, Modulation::Qam64, CodeRate::R3_4, TimeInterleave::K2),
            None,
        ],
    )
    .unwrap()
}

#[test]
fn broadcast_words_are_codewords() {
    for w in [NO_NEXT, NEXT_IS_CURRENT] {
        let w = word(w);
        assert_eq!(w.len(), 184);
        assert!(dsc::syndrome_ok(&w));
        // The parity is exactly what our encoder computes from B20–B121.
        assert_eq!(dsc::parity(&w[..102]).as_slice(), &w[102..]);
    }
}

#[test]
fn broadcast_words_decode_to_the_broadcast_configuration() {
    let p = broadcast_params();
    for w in [NO_NEXT, NEXT_IS_CURRENT] {
        let d = decode_frame(&frame(&word(w)));
        assert_eq!(d.parity, ParityStatus::Ok);
        assert_eq!(d.sync_odd, Some(false));
        let i = &d.info;
        assert_eq!(i.system_id, 0b00); // television
        assert_eq!(i.switching_countdown, 0b1111); // no switching
        assert!(!i.emergency_alarm);
        assert_eq!(i.phase_shift_correction, 0b111);
        assert_eq!(i.reserved, 0xfff);
        assert_eq!(i.params(Mode::M3, GuardInterval::G1_8).unwrap(), p);
    }
    // §3.15.6.4 / Tables 3-28..3-31: "no next information" is all 1,
    // including the next partial-reception flag B67 (VERIFICATION.md D3).
    let d = decode_frame(&frame(&word(NO_NEXT)));
    let b = d.info.to_bits();
    assert!(b[47..87].iter().all(|&x| x == 1), "B67-B106");
    let d = decode_frame(&frame(&word(NEXT_IS_CURRENT)));
    assert_eq!(d.info.next, d.info.current);
    assert!(d.info.next.partial_reception, "B67 = B27 = 1");
}

#[test]
fn transmitter_tmcc_equals_a_broadcasters() {
    // The transmitter repeats the current information as the next
    // information, as station 2 does: its TMCC word is that station's, bit
    // for bit, parity included.
    let ours = TmccInfo::from_params(&broadcast_params()).frame_bits(false, false);
    assert_eq!(ours[20..].to_vec(), word(NEXT_IS_CURRENT));
}

#[test]
fn broadcast_words_correct_up_to_eight_errors() {
    // One-step majority logic over the 17 orthogonal checks corrects any
    // 8 errors; flip every 8-bit pattern of a deterministic sequence.
    for w in [NO_NEXT, NEXT_IS_CURRENT] {
        let orig = word(w);
        let mut x: u64 = 0x9e37_79b9_7f4a_7c15;
        for _ in 0..200 {
            let mut v = orig.clone();
            let mut pos = Vec::new();
            while pos.len() < 8 {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                let p = (x % 184) as usize;
                if !pos.contains(&p) {
                    pos.push(p);
                }
            }
            for &p in &pos {
                v[p] ^= 1;
            }
            assert_eq!(dsc::decode(&mut v), ParityStatus::Corrected(8));
            assert_eq!(v, orig);
        }
    }
}

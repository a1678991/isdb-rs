//! The fixed-point LDPC decoder gives each codeword the same result alone
//! (one lane, the scalar reference) and in any lane of a wider batch next
//! to other codewords: bits, iterations, convergence and the unsatisfied
//! count, whichever SIMD back end the build uses (plan §7.2).

use isdb_fec::ldpc::Code;
use isdb_fec::ldpc::fixed::{Decoder, LaneResult, Params};
use proptest::prelude::*;

/// A code of the B44 form (n = 374·8, k = 374·3, q = 5), small enough for
/// many cases.
fn code() -> Code {
    Code::new(
        374 * 8,
        &[&[3, 300, 1000, 1700], &[7, 900, 1500], &[11, 640, 1203]],
    )
}

/// Quantised LLRs of a random codeword through noise: `amp` is the clean
/// magnitude, `noise` the spread of the added uniform noise, `flips` the
/// fraction of strongly wrong bits (per mille), so cases run from easy to
/// non-converging.
fn word(code: &Code, seed: u64, amp: i8, noise: i8, flips: u16) -> Vec<i8> {
    let mut s = seed | 1;
    let mut next = move || {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        s
    };
    let info: Vec<u8> = (0..code.k()).map(|_| (next() & 1) as u8).collect();
    code.encode(&info)
        .iter()
        .map(|&b| {
            let clean = if b == 0 {
                i16::from(amp)
            } else {
                -i16::from(amp)
            };
            let span = 2 * i16::from(noise) + 1;
            let n = (next() % span as u64) as i16 - i16::from(noise);
            let flip = (next() % 1000) < u64::from(flips);
            let x = if flip { -clean } else { clean + n };
            x.clamp(-127, 127) as i8
        })
        .collect()
}

fn alone(code: &Code, w: &[i8], p: Params) -> (LaneResult, Vec<u8>) {
    let mut bits = vec![Vec::new()];
    let r = Decoder::<1>::default().decode(code, &[w], &mut bits, p)[0];
    (r, bits.pop().unwrap())
}

fn batch<const W: usize>(code: &Code, ws: &[Vec<i8>], p: Params) -> Vec<(LaneResult, Vec<u8>)> {
    let refs: Vec<&[i8]> = ws.iter().map(Vec::as_slice).collect();
    let mut bits = vec![Vec::new(); refs.len()];
    let r = Decoder::<W>::default().decode(code, &refs, &mut bits, p);
    r.into_iter().zip(bits).collect()
}

fn check<const W: usize>(code: &Code, ws: &[Vec<i8>], p: Params) {
    let wide = batch::<W>(code, ws, p);
    for (i, w) in ws.iter().enumerate() {
        assert_eq!(wide[i], alone(code, w, p), "lane {i} of {W}");
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(24))]

    /// Any mix of easy, hard and undecodable codewords, any number of them
    /// up to the lane count, any offset and iteration limit.
    #[test]
    fn prop_every_lane_decodes_as_the_scalar_reference(
        seeds in prop::collection::vec(any::<u64>(), 1..=16),
        amp in 4i8..40,
        noise in 0i8..60,
        flips in 0u16..120,
        offset in 0i8..4,
        max_iter in 0usize..12,
    ) {
        let code = code();
        let ws: Vec<Vec<i8>> = seeds.iter().enumerate()
            .map(|(i, &s)| word(&code, s, amp, noise, flips * (i as u16 % 3)))
            .collect();
        let p = Params { max_iter, offset };
        check::<16>(&code, &ws, p);
        check::<32>(&code, &ws[..ws.len().min(8)], p);
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(16))]

    /// More codewords than lanes stream through: a lane that stops takes
    /// the next codeword, and every codeword still decodes as alone.
    #[test]
    fn prop_streamed_codewords_decode_as_the_scalar_reference(
        seeds in prop::collection::vec(any::<u64>(), 1..=40),
        amp in 6i8..30,
        noise in 0i8..50,
        flips in 0u16..80,
        max_iter in 0usize..10,
    ) {
        let code = code();
        let ws: Vec<Vec<i8>> = seeds.iter().enumerate()
            .map(|(i, &s)| word(&code, s, amp, noise, flips * (i as u16 % 4)))
            .collect();
        let p = Params { max_iter, offset: 1 };
        let refs: Vec<&[i8]> = ws.iter().map(Vec::as_slice).collect();
        let streamed = Decoder::<8>::default().decode_all(&code, &refs, p);
        for (i, w) in ws.iter().enumerate() {
            prop_assert_eq!(&streamed[i], &alone(&code, w, p), "codeword {}", i);
        }
    }
}

#[test]
fn saturated_and_zero_inputs_match_too() {
    // ±127 everywhere (TMCC-style known bits), all zeros, and −128 cannot
    // occur after quantise; all go through the same lanes.
    let code = code();
    let mut ws = vec![
        word(&code, 1, 127, 0, 0),
        vec![0; code.n()],
        word(&code, 2, 1, 1, 0),
        word(&code, 3, 20, 30, 80),
    ];
    ws.push(ws[0].iter().map(|x| x.saturating_neg()).collect());
    check::<8>(
        &code,
        &ws,
        Params {
            max_iter: 20,
            offset: 1,
        },
    );
    check::<64>(
        &code,
        &ws,
        Params {
            max_iter: 20,
            offset: 0,
        },
    );
}

#[test]
fn converged_lanes_report_what_a_lone_decode_does() {
    // One clean word converges at iteration 0, one needs iterations, one
    // never converges: their results differ, and each equals its lone decode.
    let code = code();
    let ws = vec![
        word(&code, 10, 30, 0, 0),
        word(&code, 11, 12, 18, 20),
        word(&code, 12, 3, 60, 300),
    ];
    let p = Params {
        max_iter: 15,
        offset: 1,
    };
    let r = batch::<4>(&code, &ws, p);
    assert_eq!(r[0].0, Ok(isdb_fec::ldpc::Decoded { iterations: 0 }));
    assert!(matches!(r[1].0, Ok(d) if d.iterations > 0), "{:?}", r[1].0);
    assert!(r[2].0.is_err(), "{:?}", r[2].0);
    check::<4>(&code, &ws, p);
}

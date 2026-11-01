//! The fixed-point LDPC decoder's schedules (`ldpc::fixed::Schedule`,
//! docs/plans/isdbs3-fpga-rx.md "The LDPC schedule contract"): the
//! layered schedule gives each codeword the same result in any lane as
//! alone; `Layered { step }` equals an independent scalar reading of the
//! contract (which itself reproduces the sequential decoder when given
//! the checks in order); and a conflicting pair inside a step takes the
//! later check's value computed from the step's starting values.

use isdb_fec::ldpc::fixed::{Decoder, LaneResult, Params, Schedule};
use isdb_fec::ldpc::{Code, Decoded, GROUP, NotConverged};
use proptest::prelude::*;

/// A code of the B44 form (n = 374·8, k = 374·3, q = 5) whose rows have
/// entries congruent mod q (3 of them: 300, 1000, 1700 ≡ 0; 900, 1500 ≡
/// 0), so its layers have conflicting pairs.
fn code() -> Code {
    Code::new(
        374 * 8,
        &[&[3, 300, 1000, 1700], &[7, 900, 1500], &[11, 640, 1203]],
    )
}

/// Quantised LLRs of a random codeword through noise (as in
/// `ldpc_lanes.rs`): `amp` the clean magnitude, `noise` the spread of the
/// added uniform noise, `flips` the per-mille fraction of strongly wrong
/// bits.
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
            let n = i16::try_from(next() % span as u64).unwrap() - i16::from(noise);
            let flip = (next() % 1000) < u64::from(flips);
            let x = if flip { -clean } else { clean + n };
            i8::try_from(x.clamp(-127, 127)).unwrap()
        })
        .collect()
}

/// The steps of `Layered { step }` as the contract states them: layer l =
/// the checks c ≡ l mod q in increasing c, cut into runs of `step`.
fn layered_steps(code: &Code, step: usize) -> Vec<Vec<usize>> {
    let (p, q) = (code.checks(), code.q());
    let mut out = Vec::new();
    for l in 0..q {
        let layer: Vec<usize> = (0..p).filter(|c| c % q == l).collect();
        assert_eq!(layer.len(), GROUP);
        out.extend(layer.chunks(step).map(<[usize]>::to_vec));
    }
    out
}

/// The sequential schedule as steps of one check, 0 … P−1.
fn sequential_steps(code: &Code) -> Vec<Vec<usize>> {
    (0..code.checks()).map(|c| vec![c]).collect()
}

/// How the writes of a step are applied: the contract's rule and two
/// wrong ones, to show the conflict test tells them apart.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Writes {
    /// The contract: every check reads the step's starting values, the
    /// writes land in check order (the later check wins).
    Contract,
    /// Stale reads, writes in reverse order (the earlier check wins).
    EarlierWins,
    /// Each check reads the previous check's writes (no step barrier).
    Fresh,
}

/// A plain scalar decoder of one codeword with the given steps: the
/// per-check arithmetic of `ldpc::fixed` written out with branches.
fn reference(
    code: &Code,
    llr: &[i8],
    max_iter: usize,
    offset: i8,
    steps: &[Vec<usize>],
    writes: Writes,
) -> (LaneResult, Vec<u8>) {
    let mut l = llr.to_vec();
    let mut r: Vec<Vec<i8>> = (0..code.checks())
        .map(|c| vec![0; code.check(c).len()])
        .collect();
    let hard = |l: &[i8]| l.iter().map(|&x| u8::from(x < 0)).collect::<Vec<u8>>();
    let unsat = code.syndrome_weight(&hard(&l));
    if unsat == 0 {
        return (Ok(Decoded { iterations: 0 }), hard(&l));
    }
    if max_iter == 0 {
        return (
            Err(NotConverged {
                iterations: 0,
                unsatisfied: unsat,
            }),
            hard(&l),
        );
    }
    // one check from the values `l`: its t and new messages
    let check = |c: usize, l: &[i8], r: &[Vec<i8>]| -> (Vec<i8>, Vec<i8>) {
        let vars = code.check(c);
        let t: Vec<i8> = vars
            .iter()
            .zip(&r[c])
            .map(|(&v, &re)| l[v as usize].saturating_sub(re))
            .collect();
        let (mut min1, mut min2, mut imin, mut parity) = (i8::MAX, i8::MAX, 0, false);
        for (e, &x) in t.iter().enumerate() {
            let a = x.saturating_abs();
            parity ^= x < 0;
            if a < min1 {
                min2 = min1;
                min1 = a;
                imin = e;
            } else if a < min2 {
                min2 = a;
            }
        }
        let new: Vec<i8> = t
            .iter()
            .enumerate()
            .map(|(e, &x)| {
                let m = if e == imin { min2 } else { min1 };
                let mag = m.saturating_sub(offset).max(0);
                if parity ^ (x < 0) { -mag } else { mag }
            })
            .collect();
        (t, new)
    };
    let mut unsat = unsat;
    for it in 1..=max_iter {
        for step in steps {
            if writes == Writes::Fresh {
                for &c in step {
                    let (t, new) = check(c, &l, &r);
                    for ((&v, &te), &re) in code.check(c).iter().zip(&t).zip(&new) {
                        l[v as usize] = te.saturating_add(re);
                    }
                    r[c] = new;
                }
                continue;
            }
            let mut out: Vec<(usize, Vec<i8>, Vec<i8>)> = step
                .iter()
                .map(|&c| (c, check(c, &l, &r)))
                .map(|(c, (t, n))| (c, t, n))
                .collect();
            if writes == Writes::EarlierWins {
                out.reverse();
            }
            for (c, t, new) in out {
                for ((&v, &te), &re) in code.check(c).iter().zip(&t).zip(&new) {
                    l[v as usize] = te.saturating_add(re);
                }
                r[c] = new;
            }
        }
        unsat = code.syndrome_weight(&hard(&l));
        if unsat == 0 {
            return (Ok(Decoded { iterations: it }), hard(&l));
        }
    }
    (
        Err(NotConverged {
            iterations: max_iter,
            unsatisfied: unsat,
        }),
        hard(&l),
    )
}

fn alone(code: &Code, w: &[i8], p: Params) -> (LaneResult, Vec<u8>) {
    let mut bits = vec![Vec::new()];
    let r = Decoder::<1>::default().decode(code, &[w], &mut bits, p)[0];
    (r, bits.pop().unwrap())
}

fn check_lanes<const W: usize>(code: &Code, ws: &[Vec<i8>], p: Params) {
    let refs: Vec<&[i8]> = ws.iter().map(Vec::as_slice).collect();
    let wide = Decoder::<W>::default().decode_all(code, &refs, p);
    for (i, w) in ws.iter().enumerate() {
        assert_eq!(wide[i], alone(code, w, p), "lane {i} of {W}, {p:?}");
    }
}

/// Steps worth covering: 1 (sequential in layer order), small ones that
/// split a conflicting pair or not, ones that do not divide 374 (a short
/// last step), the HDL candidates 34, 68, 187, a whole layer, and more.
const STEPS: [usize; 9] = [1, 2, 3, 7, 34, 68, 187, 374, 1000];

proptest! {
    #![proptest_config(ProptestConfig::with_cases(24))]

    /// Any mix of easy, hard and undecodable codewords, 16 and 32 lanes,
    /// any layered step, offset and limit: every lane decodes as alone.
    #[test]
    fn prop_layered_lanes_decode_as_the_scalar_reference(
        seeds in prop::collection::vec(any::<u64>(), 1..=16),
        amp in 4i8..40,
        noise in 0i8..60,
        flips in 0u16..120,
        offset in 0i8..4,
        max_iter in 0usize..12,
        step in prop::sample::select(STEPS.to_vec()),
    ) {
        let code = code();
        let ws: Vec<Vec<i8>> = seeds.iter().zip(0u16..)
            .map(|(&s, i)| word(&code, s, amp, noise, flips * (i % 3)))
            .collect();
        let p = Params { max_iter, offset, schedule: Schedule::Layered { step } };
        check_lanes::<16>(&code, &ws, p);
        check_lanes::<32>(&code, &ws[..ws.len().min(8)], p);
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(16))]

    /// More codewords than lanes, layered: refilled lanes decode as alone.
    #[test]
    fn prop_layered_streamed_codewords_decode_as_the_scalar_reference(
        seeds in prop::collection::vec(any::<u64>(), 1..=40),
        amp in 6i8..30,
        noise in 0i8..50,
        flips in 0u16..80,
        max_iter in 0usize..10,
        step in prop::sample::select(STEPS.to_vec()),
    ) {
        let code = code();
        let ws: Vec<Vec<i8>> = seeds.iter().zip(0u16..)
            .map(|(&s, i)| word(&code, s, amp, noise, flips * (i % 4)))
            .collect();
        let p = Params { max_iter, offset: 1, schedule: Schedule::Layered { step } };
        let refs: Vec<&[i8]> = ws.iter().map(Vec::as_slice).collect();
        let streamed = Decoder::<8>::default().decode_all(&code, &refs, p);
        for (i, w) in ws.iter().enumerate() {
            prop_assert_eq!(&streamed[i], &alone(&code, w, p), "codeword {}", i);
        }
    }

    /// The decoder equals the scalar contract reading: `Sequential` with
    /// the checks one by one in order, `Layered { step }` with the
    /// contract's steps (step 1: the checks one by one in layer order).
    #[test]
    fn prop_schedules_equal_the_contract_reference(
        seed in any::<u64>(),
        amp in 4i8..30,
        noise in 0i8..50,
        flips in 0u16..100,
        offset in 0i8..3,
        max_iter in 0usize..10,
        step in prop::sample::select(STEPS.to_vec()),
    ) {
        let code = code();
        let w = word(&code, seed, amp, noise, flips);
        let p = Params::new(max_iter, offset);
        prop_assert_eq!(
            alone(&code, &w, p),
            reference(&code, &w, max_iter, offset, &sequential_steps(&code), Writes::Contract)
        );
        let p = Params { schedule: Schedule::Layered { step }, ..p };
        prop_assert_eq!(
            alone(&code, &w, p),
            reference(&code, &w, max_iter, offset, &layered_steps(&code, step), Writes::Contract)
        );
    }
}

#[test]
fn layered_step_1_is_sequential_in_layer_order() {
    // the checks one at a time, layer 0's in increasing c, then layer 1's
    // …, each after the previous one's writes
    let code = code();
    let (p, q) = (code.checks(), code.q());
    let mut order: Vec<usize> = (0..p).collect();
    order.sort_by_key(|&c| (c % q, c));
    let steps: Vec<Vec<usize>> = order.into_iter().map(|c| vec![c]).collect();
    let mut differs = false;
    for seed in 0..12 {
        let w = word(&code, 100 + seed, 14, 24, 30);
        let p = Params {
            max_iter: 20,
            offset: 1,
            schedule: Schedule::Layered { step: 1 },
        };
        let got = alone(&code, &w, p);
        assert_eq!(
            got,
            reference(&code, &w, 20, 1, &steps, Writes::Fresh),
            "seed {seed}"
        );
        assert_eq!(
            got,
            reference(&code, &w, 20, 1, &steps, Writes::Contract),
            "seed {seed}"
        );
        differs |= got != alone(&code, &w, Params::new(20, 1));
    }
    // and it is a different order from 0 … P−1
    assert!(differs, "layer order never changed a result");
}

/// A code with one conflicting pair per check of layer 3, built so the
/// rule decides a bit: n = 374·6, k = 374·2, P = 1 496, q = 4. Row 0 is
/// [3, 7], both ≡ 3 mod 4: bit j of group 0 is in checks 3 + 4j and 7 + 4j
/// of layer 3, i.e. layer positions j and j + 1. So bit 0 is in checks 3
/// (position 0) and 7 (position 1), and nowhere else. Row 1 ([0, 1, 2])
/// puts group 1 in layers 0–2 only.
///
/// Input: every bit +100 except bit 0 = +5 (weak 0), bit 373 = −40 (with
/// bit 0 the other information bit of check 3) and bit 1 = +10 (the other
/// of check 7). In iteration 1 (messages 0) check 3 sends bit 0
/// −(40 − offset) (its other edges: −40 and the strong parity bits) and
/// check 7 sends +(10 − offset):
///
/// - contract (both in one step, stale reads, later wins): L₀ = 5 + 10 −
///   offset > 0, a zero;
/// - checks one after the other (step 1, sequential, or no barrier): L₀ =
///   5 − 40 + 10 (± offsets) < 0, a one;
/// - stale reads with the earlier check winning: L₀ = 5 − 40 + offset < 0.
#[test]
fn conflicting_pair_in_one_step_takes_the_later_check_from_stale_values() {
    let code = Code::new(374 * 6, &[&[3, 7], &[0, 1, 2]]);
    assert_eq!((code.checks(), code.q()), (1496, 4));
    assert_eq!(code.check(3), &[0, 373, 374 * 2 + 2, 374 * 2 + 3]);
    assert_eq!(code.check(7), &[0, 1, 374 * 2 + 6, 374 * 2 + 7]);
    let mut w = vec![100i8; code.n()];
    w[0] = 5;
    w[373] = -40;
    w[1] = 10;
    for offset in [0, 1] {
        let bit0 = |schedule: Schedule| {
            let p = Params {
                max_iter: 1,
                offset,
                schedule,
            };
            let (_, bits) = alone(&code, &w, p);
            // every lane width agrees
            check_lanes::<16>(&code, &[w.clone(), w.clone()], p);
            bits[0]
        };
        for step in [2, 3, 34, 68, 187, 374] {
            assert_eq!(bit0(Schedule::Layered { step }), 0, "step {step}");
        }
        assert_eq!(bit0(Schedule::Layered { step: 1 }), 1);
        assert_eq!(bit0(Schedule::Sequential), 1);
        // the reference agrees, and the two wrong rules give a one
        let st = layered_steps(&code, 68);
        let rule = |wr| reference(&code, &w, 1, offset, &st, wr).1[0];
        assert_eq!(rule(Writes::Contract), 0);
        assert_eq!(rule(Writes::EarlierWins), 1);
        assert_eq!(rule(Writes::Fresh), 1);
    }
    // whole decodes of noisy codewords through the pairs (every check of
    // layer 3 has one), each step against the reference
    let ws: Vec<Vec<i8>> = (0..6).map(|s| word(&code, 900 + s, 16, 30, 20)).collect();
    for step in STEPS {
        let p = Params {
            max_iter: 20,
            offset: 1,
            schedule: Schedule::Layered { step },
        };
        let st = layered_steps(&code, step);
        for w in &ws {
            assert_eq!(
                alone(&code, w, p),
                reference(&code, w, 20, 1, &st, Writes::Contract),
                "step {step}"
            );
        }
        check_lanes::<16>(&code, &ws, p);
    }
}

#[test]
fn layered_iteration_trace_ends_at_the_result() {
    let code = code();
    let ws: Vec<Vec<i8>> = (0..5).map(|s| word(&code, 40 + s, 12, 20, 25)).collect();
    let refs: Vec<&[i8]> = ws.iter().map(Vec::as_slice).collect();
    let p = Params {
        max_iter: 12,
        offset: 1,
        schedule: Schedule::Layered { step: 68 },
    };
    let mut traces: Vec<Vec<Vec<u8>>> = vec![Vec::new(); ws.len()];
    let out = Decoder::<4>::default().decode_all_traced(&code, &refs, p, |w, b| {
        traces[w].push(b.to_vec());
    });
    assert_eq!(out, Decoder::<4>::default().decode_all(&code, &refs, p));
    for (i, (r, bits)) in out.iter().enumerate() {
        let it = match r {
            Ok(d) => d.iterations,
            Err(e) => e.iterations,
        };
        assert_eq!(traces[i].len(), it, "codeword {i}");
        if it > 0 {
            assert_eq!(traces[i].last().unwrap(), bits);
        }
    }
}

#[test]
#[should_panic(expected = "step 0")]
fn layered_step_0_is_refused() {
    let code = code();
    let w = word(&code, 1, 20, 0, 0);
    let p = Params {
        max_iter: 1,
        offset: 1,
        schedule: Schedule::Layered { step: 0 },
    };
    let _ = alone(&code, &w, p);
}

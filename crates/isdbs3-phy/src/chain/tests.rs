//! The chain end to end on synthetic streams ([`super::synth`]).

use super::synth::{SynthOptions, generate};
use super::{Chain, ChainOptions, Output};
use isdbs3_core::params::Modulation;

fn run(
    o: &SynthOptions,
    frames: usize,
    opt: ChainOptions,
    chunks: &[usize],
) -> (Vec<Output>, super::synth::Synth) {
    let s = generate(o, frames);
    let mut c = Chain::new(opt);
    let mut out = Vec::new();
    // push in chunks of the given sizes (cycled): the chain is a stream,
    // the split must not matter
    let mut i = 0;
    let mut k = 0;
    while i < s.symbols.len() {
        let n = chunks[k % chunks.len()].min(s.symbols.len() - i);
        for &x in &s.symbols[i..i + n] {
            c.push(x, &mut out);
        }
        i += n;
        k += 1;
    }
    (out, s)
}

fn frames(out: &[Output]) -> Vec<&super::frame::FrameOut> {
    out.iter()
        .filter_map(|o| match o {
            Output::Frame(f) => Some(f.as_ref()),
            _ => None,
        })
        .collect()
}

/// The slots decoded match the generated data, frame by frame; the
/// first `lag` frames after lock have no layout.
fn check(out: &[Output], s: &super::synth::Synth, lag: usize) -> usize {
    let fr = frames(out);
    assert!(!fr.is_empty(), "no frame");
    let mut decoded = 0;
    for f in &fr {
        let truth = s
            .frames
            .iter()
            .find(|t| t.at as u64 == f.at)
            .unwrap_or_else(|| panic!("frame at {} not generated", f.at));
        assert!(f.tmcc.bch_corrected.is_some(), "TMCC of frame {}", f.frame);
        if f.slots.is_empty() {
            continue;
        }
        decoded += 1;
        let valid: Vec<_> = f.slots.iter().filter(|x| !x.dummy).collect();
        assert_eq!(valid.len(), truth.slots.len());
        for (got, (slot, header, data)) in valid.iter().zip(&truth.slots) {
            assert_eq!(usize::from(got.slot), *slot);
            assert!(
                got.converged && got.bch_corrected.is_some(),
                "frame {} slot {slot}",
                f.frame
            );
            assert_eq!(&got.header, header);
            assert_eq!(&got.data, data, "frame {} slot {slot}", f.frame);
        }
    }
    assert_eq!(decoded + lag, fr.len(), "undecoded frames");
    decoded
}

#[test]
fn a_clean_16apsk_stream_decodes_after_the_layout_lag() {
    let o = SynthOptions {
        snr_db: 22.0,
        carrier_hz: 150.0,
        ..SynthOptions::default()
    };
    for lag in [1u32, 2] {
        let (out, s) = run(
            &o,
            8,
            ChainOptions {
                layout_lag: lag,
                eq_every: 2,
                ..ChainOptions::default()
            },
            &[4096],
        );
        let n = check(&out, &s, lag as usize);
        assert!(n >= 4, "lag {lag}: {n} frames decoded");
    }
}

#[test]
fn mixed_modes_and_dummies_decode() {
    let o = SynthOptions {
        modes: vec![
            (Modulation::Apsk32, 101, 20),
            (Modulation::Apsk16, 93, 40),
            (Modulation::Psk8, 81, 30),
            (Modulation::Qpsk, 61, 25),
            (Modulation::Bpsk, 41, 5),
        ],
        snr_db: 30.0,
        amplitude: 7.0,
        carrier_hz: -300.0,
        ..SynthOptions::default()
    };
    let (out, s) = run(&o, 7, ChainOptions::default(), &[4096]);
    assert!(check(&out, &s, 2) >= 3);
}

/// The carrier loop follows a drift of −22 Hz/s (the live stream's) from
/// −600 Hz, and a stream that drifts much faster (−2 kHz/s) still
/// decodes. The register moves by half the error every frame (33 ms), so
/// a drift D is followed with a lag of about 2·D·33 ms (1.5 Hz at
/// −22 Hz/s, 132 Hz at −2 kHz/s) once settled; the initial −600 Hz
/// decays as 2⁻ⁿ per frame.
#[test]
fn the_carrier_loop_follows_a_drift() {
    for drift in [-22.0, -2_000.0] {
        let o = SynthOptions {
            carrier_hz: -600.0,
            drift_hz_s: drift,
            snr_db: 22.0,
            ..SynthOptions::default()
        };
        let (out, s) = run(&o, 10, ChainOptions::default(), &[4096]);
        check(&out, &s, 2);
        let last = out
            .iter()
            .rev()
            .find_map(|o| match o {
                Output::Carrier(f) => Some(*f),
                _ => None,
            })
            .unwrap();
        let hz = f64::from(last) * isdbs3_core::params::SYMBOL_RATE / 2f64.powi(32);
        // the carrier during the last frame the register was updated on,
        // minus the settled lag
        let end = -600.0 + drift * (10.0 * 0.033);
        let lagged = end - drift * 2.0 * 0.033;
        assert!(
            (hz - lagged).abs() < 5.0 + drift.abs() * 0.04,
            "{drift}: {hz} vs {lagged}"
        );
    }
}

/// Any split of the stream into pushes gives the same frames, bit for
/// bit (the chain is one symbol per push; this guards against state kept
/// per call).
#[test]
fn prop_the_split_does_not_matter() {
    let o = SynthOptions {
        snr_db: 18.0,
        carrier_hz: 77.0,
        ..SynthOptions::default()
    };
    let opt = ChainOptions {
        eq_every: 1,
        ..ChainOptions::default()
    };
    type Key = (u32, Vec<Vec<u8>>, i64, [[i16; 2]; 11]);
    let key = |out: &[Output]| -> Vec<Key> {
        frames(out)
            .iter()
            .map(|f| {
                (
                    f.frame,
                    f.slots.iter().map(|s| s.data.clone()).collect(),
                    f.status.n0,
                    f.status.taps,
                )
            })
            .collect()
    };
    let (a, _) = run(&o, 4, opt.clone(), &[1]);
    let (b, _) = run(&o, 4, opt.clone(), &[4096]);
    let (c, _) = run(&o, 4, opt, &[1, 7_919, 3, 100_003, 9_296]);
    assert_eq!(key(&a), key(&b));
    assert_eq!(key(&a), key(&c));
    assert!(!key(&a).is_empty());
}

/// A loss restarts the chain: the frame cut short is dropped, the
/// stream is found again and decodes after the lag.
#[test]
fn a_loss_restarts_and_the_stream_decodes_again() {
    let o = SynthOptions {
        snr_db: 22.0,
        ..SynthOptions::default()
    };
    let s = generate(&o, 9);
    let mut c = Chain::new(ChainOptions::default());
    let mut out = Vec::new();
    let cut = (s.symbols.len() * 4 / 9, 300_000);
    for &x in &s.symbols[..cut.0] {
        c.push(x, &mut out);
    }
    c.loss(cut.1 as u64, &mut out);
    for &x in &s.symbols[cut.0 + cut.1..] {
        c.push(x, &mut out);
    }
    assert!(
        out.iter()
            .any(|o| matches!(o, Output::Restart { sync_lost: false }))
    );
    let fr = frames(&out);
    let after: Vec<_> = fr
        .iter()
        .filter(|f| f.at > (cut.0 + cut.1) as u64)
        .collect();
    assert!(
        after.iter().any(|f| !f.slots.is_empty()),
        "nothing decoded after the loss"
    );
    // the frame counter jumped over the gap
    let before = fr
        .iter()
        .filter(|f| f.at < cut.0 as u64)
        .map(|f| f.frame)
        .max()
        .unwrap();
    let first_after = after.iter().map(|f| f.frame).min().unwrap();
    assert!(first_after > before + 1, "{before} → {first_after}");
    for f in after.iter().filter(|f| !f.slots.is_empty()) {
        assert!(
            f.slots
                .iter()
                .filter(|x| !x.dummy)
                .all(|x| x.bch_corrected.is_some())
        );
    }
}

/// The `Slots` blocks of a frame (format 2): a decoded frame's end with
/// its status block and then its sums block, the sums the frame's
/// (`FrameOut::sums`, the status block's `sums_frame`), the block's
/// header frame the frame's; an undecoded frame (no layout: nothing
/// latched) ends with its status block. Written and read back, the
/// sums solve to the model's taps.
#[test]
fn a_decoded_frame_ends_with_its_sums_block() {
    use isdb_frontend::slots::{Block, Event, Reader, Writer};
    let (out, _) = run(
        &SynthOptions::default(),
        5,
        ChainOptions::default(),
        &[65_536],
    );
    let fr = frames(&out);
    let mut decoded = 0;
    let mut words = Vec::new();
    let mut w = Writer::new();
    for f in &fr {
        let b = super::out::blocks(f, super::LAYOUT_LAG);
        for x in &b {
            w.write(x, &mut words);
        }
        let n = b.len();
        match (&f.sums, &b[n - 1]) {
            (Some(s), Block::Sums(sb)) => {
                decoded += 1;
                assert!(!f.slots.is_empty(), "frame {}", f.frame);
                assert!(matches!(&b[n - 2], Block::Status(st) if st.sums_frame == Some(f.frame)));
                assert_eq!((sb.frame, sb.sums_frame), (f.frame, s.frame));
                assert_eq!(u32::from(sb.count), s.count);
                assert_eq!((&sb.r, &sb.p), (&s.r, &s.p));
                assert_eq!(super::eq::solve_block(sb), super::eq::solve_sums(s));
            }
            (None, Block::Status(st)) => {
                assert!(f.slots.is_empty(), "frame {}", f.frame);
                assert_eq!(st.sums_frame, None);
            }
            (s, last) => panic!(
                "frame {}: sums {:?}, last block {last:?}",
                f.frame,
                s.is_some()
            ),
        }
    }
    assert!(decoded >= 1, "{decoded} decoded frames");
    let mut ev = Vec::new();
    let mut r = Reader::new();
    r.feed(&words, &mut ev);
    r.finish(&mut ev);
    let sums = ev
        .iter()
        .filter(|e| {
            matches!(
                e,
                Event::Block {
                    block: Block::Sums(_),
                    ..
                }
            )
        })
        .count();
    assert_eq!(sums, decoded);
    assert!(ev.iter().all(|e| matches!(e, Event::Block { .. })));
}

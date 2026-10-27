//! End-to-end loopback: Modulator → channel → Demodulator.

// Narrowing casts are proven or fallible here (docs/plans/type-safety.md section 2).
#![warn(clippy::cast_possible_truncation)]

mod common;

use common::{Channel, Rng};
use isdbt_core::{
    CodeRate, GuardInterval, Layer, LayerConfig, Mode, Modulation, Params, Segments, TimeInterleave,
};
use isdbt_ofdm::ac::AcFrame;
use isdbt_ofdm::latency::end_to_end_frames;
use isdbt_ofdm::tmcc::TmccInfo;
use isdbt_ofdm::{C32, DemodConfig, Demodulator, Modulator};

fn layer(
    segments: u8,
    modulation: Modulation,
    code_rate: CodeRate,
    ti: TimeInterleave,
) -> Option<LayerConfig> {
    Some(LayerConfig {
        segments: Segments::new(segments).unwrap(),
        modulation,
        code_rate,
        time_interleave: ti,
    })
}

struct Report {
    bit_errors: [usize; 3],
    bits: [usize; 3],
    frames_checked: usize,
    cfo: f64,
    start_err: i64,
    mer: [Option<f32>; 3],
}

fn run(params: Params, frames: usize, ch: Channel, skip: usize, seed: u64) -> Report {
    run_sfo(params, frames, ch, skip, seed, 0.0)
}

fn run_sfo(params: Params, frames: usize, ch: Channel, skip: usize, seed: u64, ppm: f64) -> Report {
    let mut rng = Rng(seed);
    let mut m = Modulator::new(params.clone());
    let tmcc = TmccInfo::from_params(&params);
    let ac = AcFrame::stuffing(m.layout());
    let mut inputs: Vec<[Vec<u8>; 3]> = Vec::new();
    let mut tx = Vec::new();
    for _ in 0..frames {
        let bits = Layer::ALL.map(|l| rng.bits(m.bits_per_frame(l)));
        tx.extend(
            m.modulate_frame([&bits[0], &bits[1], &bits[2]], &tmcc, &ac)
                .unwrap(),
        );
        inputs.push(bits);
    }
    let tx = if ppm != 0.0 {
        common::resample_ppm(&tx, ppm)
    } else {
        tx
    };
    let rx = ch.apply(&tx[skip..], &mut rng);
    let mut d = Demodulator::new(DemodConfig::default());
    let mut out = Vec::new();
    for chunk in rx.chunks(10_007) {
        out.extend(d.push(chunk));
    }
    let frame_len = params.samples_per_frame() as i64;
    let lat = end_to_end_frames(&params);
    let mut rep = Report {
        bit_errors: [0; 3],
        bits: [0; 3],
        frames_checked: 0,
        cfo: 0.0,
        start_err: 0,
        mer: [None; 3],
    };
    assert!(!out.is_empty(), "no frames demodulated");
    for f in &out {
        if std::env::var("LOOPBACK_DEBUG").is_ok() {
            eprintln!(
                "frame {} start {} parity {:?} mer {:?} drift {:.4} corr {:.2} cfo {:.4} n0 {:.2e}",
                f.frame_number,
                f.start_sample,
                f.tmcc.parity,
                f.metrics.mer_db,
                f.metrics.timing_drift,
                f.metrics.timing_correction,
                f.metrics.cfo_carriers,
                f.metrics.n0
            );
        }
    }
    for f in &out {
        assert!(f.tmcc.parity.is_ok(), "TMCC parity {:?}", f.tmcc.parity);
        assert_eq!(
            f.tmcc.info.params(params.mode(), params.guard()).unwrap(),
            params
        );
        assert_eq!(f.params, params);
        let rel = f.start_sample as i64 - ch.offset as i64 + skip as i64;
        // With a sampling-clock offset the TX frame f starts at f·len/(1+ppm).
        let scale = 1.0 / (1.0 + ppm * 1e-6);
        // Frame indices and sample positions are far below 2^53 (test code).
        #[allow(clippy::cast_possible_truncation)]
        let tx_frame = (rel as f64 / (frame_len as f64 * scale)).round() as i64;
        #[allow(clippy::cast_possible_truncation)]
        let expect = (tx_frame as f64 * frame_len as f64 * scale).round() as i64;
        rep.start_err = rep.start_err.max((rel - expect).abs());
        rep.cfo = f.metrics.cfo_carriers;
        rep.mer = f.metrics.mer_db;
        let src = tx_frame - lat as i64;
        if !f.valid || src < 0 {
            continue;
        }
        rep.frames_checked += 1;
        for l in Layer::ALL {
            if let Some(llr) = &f.llr[l.index()] {
                let input = &inputs[usize::try_from(src).unwrap()][l.index()];
                assert_eq!(llr.len(), input.len());
                rep.bits[l.index()] += input.len();
                rep.bit_errors[l.index()] += llr
                    .iter()
                    .zip(input)
                    .filter(|(x, b)| u8::from(**x < 0.0) != **b)
                    .count();
            }
        }
    }
    let last = out.last().unwrap();
    eprintln!(
        "{:?} {:?}: frames_out={} checked={} errors={:?}/{:?} cfo={:.4} start_err={} mer={:?} drift={:.4} smp/sym corr={:.1}",
        params.mode(),
        params.guard(),
        out.len(),
        rep.frames_checked,
        rep.bit_errors,
        rep.bits,
        rep.cfo,
        rep.start_err,
        rep.mer,
        last.metrics.timing_drift,
        last.metrics.timing_correction
    );
    rep
}

fn channel(mode: Mode, offset: usize) -> Channel {
    Channel {
        offset,
        cfo_carriers: 2.3,
        fft_size: mode.fft_size(),
        taps: vec![
            (0, C32::new(1.0, 0.0)),
            (7 * mode.scale(), C32::new(0.12, -0.16)),
        ],
        snr_db: 30.0,
        k_total: mode.total_carriers(),
    }
}

#[test]
fn loopback_mode3_hierarchical() {
    let p = Params::new(
        Mode::M3,
        GuardInterval::G1_8,
        true,
        [
            layer(1, Modulation::Qpsk, CodeRate::R2_3, TimeInterleave::K2),
            layer(12, Modulation::Qam64, CodeRate::R3_4, TimeInterleave::K2),
            None,
        ],
    )
    .unwrap();
    let lat = end_to_end_frames(&p);
    let r = run(p, lat + 3, channel(Mode::M3, 12_345), 0, 1);
    assert!(r.frames_checked >= 2);
    assert_eq!(r.bit_errors, [0; 3]);
    assert!((r.cfo - 2.3).abs() < 0.01);
}

/// Mode 1, three layers: differential partial reception + differential
/// layer B + coherent layer C (CP at the differential/coherent boundary),
/// receiver starting mid-frame.
#[test]
fn loopback_mode1_three_layers_mid_frame_start() {
    let p = Params::new(
        Mode::M1,
        GuardInterval::G1_16,
        true,
        [
            layer(1, Modulation::Dqpsk, CodeRate::R1_2, TimeInterleave::K1),
            layer(4, Modulation::Dqpsk, CodeRate::R2_3, TimeInterleave::K2),
            layer(8, Modulation::Qam16, CodeRate::R3_4, TimeInterleave::K1),
        ],
    )
    .unwrap();
    let lat = end_to_end_frames(&p);
    let skip = p.samples_per_frame() * 3 / 5;
    let r = run(p, lat + 4, channel(Mode::M1, 777), skip, 2);
    assert!(r.frames_checked >= 2);
    assert_eq!(r.bit_errors, [0; 3]);
    assert!((r.cfo - 2.3).abs() < 0.01);
}

/// Mode 2, GI 1/4: 16QAM + 64QAM with long interleaving (I = 2 / 8).
#[test]
fn loopback_mode2_long_interleave() {
    let p = Params::new(
        Mode::M2,
        GuardInterval::G1_4,
        false,
        [
            layer(2, Modulation::Qam16, CodeRate::R1_2, TimeInterleave::K1),
            layer(11, Modulation::Qam64, CodeRate::R7_8, TimeInterleave::K3),
            None,
        ],
    )
    .unwrap();
    let lat = end_to_end_frames(&p);
    let r = run(p, lat + 3, channel(Mode::M2, 4_000), 0, 3);
    assert!(r.frames_checked >= 2);
    assert_eq!(r.bit_errors, [0; 3]);
    assert!((r.cfo - 2.3).abs() < 0.01);
}

/// Mode 3 GI 1/32, all-DQPSK single layer (no scattered pilots at all).
#[test]
fn loopback_mode3_all_differential() {
    let p = Params::new(
        Mode::M3,
        GuardInterval::G1_32,
        false,
        [
            layer(13, Modulation::Dqpsk, CodeRate::R1_2, TimeInterleave::K1),
            None,
            None,
        ],
    )
    .unwrap();
    let lat = end_to_end_frames(&p);
    let r = run(p, lat + 3, channel(Mode::M3, 100), 0, 4);
    assert!(r.frames_checked >= 2);
    assert_eq!(r.bit_errors, [0; 3]);
}

/// TMCC emergency flag + seismic-warning AC on segment 0 + serial AC data
/// elsewhere, over the noisy channel.
#[test]
fn loopback_ac_seismic_and_emergency_flag() {
    use isdbt_ofdm::ac::{SeismicDetail, SeismicPage, SeismicWarning};
    let p = Params::new(
        Mode::M1,
        GuardInterval::G1_8,
        true,
        [
            layer(1, Modulation::Qpsk, CodeRate::R2_3, TimeInterleave::K0),
            layer(12, Modulation::Qam64, CodeRate::R3_4, TimeInterleave::K0),
            None,
        ],
    )
    .unwrap();
    let mut rng = Rng(11);
    let mut m = Modulator::new(p.clone());
    let mut tmcc = TmccInfo::from_params(&p);
    tmcc.emergency_alarm = true;
    let warning = SeismicWarning {
        start_end: 0,
        update: 2,
        signal_id: 0,
        detail: SeismicDetail::Warning {
            current_time: 123_456_789,
            page: SeismicPage::Areas(!(1u64 << 40) & ((1 << 56) - 1)),
        }
        .to_bits(),
    };
    let mut tx = Vec::new();
    for f in 0..4u64 {
        let bits = Layer::ALL.map(|l| rng.bits(m.bits_per_frame(l)));
        let ac = AcFrame::with_seismic(m.layout(), &warning, f % 2 == 1);
        tx.extend(
            m.modulate_frame([&bits[0], &bits[1], &bits[2]], &tmcc, &ac)
                .unwrap(),
        );
    }
    let rx = channel(Mode::M1, 999).apply(&tx, &mut rng);
    let mut d = Demodulator::new(DemodConfig::default());
    let frames = d.push(&rx);
    assert!(frames.len() >= 2);
    let lay = m.layout().clone();
    for f in &frames {
        assert!(f.tmcc.info.emergency_alarm);
        assert_eq!(f.tmcc.info, tmcc);
        let s = f.seismic.expect("seismic word");
        assert!(s.crc_ok);
        assert_eq!(s.warning, warning);
        assert_eq!(
            s.config_id,
            if f.tmcc.sync_odd == Some(true) {
                [1, 1, 0]
            } else {
                [0, 0, 1]
            }
        );
        // Other AC carriers carry stuffing 1s.
        for (a, bits) in lay.ac.iter().zip(&f.ac_bits) {
            if a.seg != 0 {
                assert!(bits[1..].iter().all(|&b| b == 1));
            }
        }
    }
}

/// Long echo (−10 dB at half the guard interval) and several seeds:
/// reports BER/MER; slow, run with `--ignored`.
#[test]
#[ignore]
fn stress_long_echo_and_seeds() {
    let p = Params::new(
        Mode::M3,
        GuardInterval::G1_8,
        true,
        [
            layer(1, Modulation::Qpsk, CodeRate::R2_3, TimeInterleave::K2),
            layer(12, Modulation::Qam64, CodeRate::R3_4, TimeInterleave::K2),
            None,
        ],
    )
    .unwrap();
    let lat = end_to_end_frames(&p);
    let mut total = [0usize; 3];
    let mut bits = [0usize; 3];
    for seed in 10..16 {
        let r = run(
            p.clone(),
            lat + 3,
            channel(Mode::M3, 1000 + usize::try_from(seed).unwrap() * 37),
            0,
            seed,
        );
        for i in 0..3 {
            total[i] += r.bit_errors[i];
            bits[i] += r.bits[i];
        }
    }
    eprintln!("default channel, 6 seeds: errors {total:?} / {bits:?}");
    let ch = Channel {
        offset: 5000,
        cfo_carriers: -7.6,
        fft_size: 8192,
        taps: vec![(0, C32::new(1.0, 0.0)), (512, C32::new(0.2, 0.245))],
        snr_db: 30.0,
        k_total: 5617,
    };
    let r = run(p.clone(), lat + 3, ch, 0, 99);
    let ber = r.bit_errors[1] as f64 / r.bits[1] as f64;
    eprintln!(
        "long echo: errors {:?} / {:?} (layer B BER {ber:.2e}), cfo {:.4}",
        r.bit_errors, r.bits, r.cfo
    );
    assert!(ber < 1e-4);
    assert_eq!(r.bit_errors[0], 0);
    // Mode 3 with a 20 ppm sampling-clock offset (0.18 samples/symbol).
    let r = run_sfo(p.clone(), lat + 4, channel(Mode::M3, 777), 0, 5, 20.0);
    let ber = r.bit_errors[1] as f64 / r.bits[1] as f64;
    eprintln!(
        "mode 3, 20 ppm: errors {:?} / {:?} (layer B BER {ber:.2e}), mer {:?}",
        r.bit_errors, r.bits, r.mer
    );
    assert_eq!(r.bit_errors[0], 0);
    assert!(ber < 1e-4);
}

/// Sampling-clock offset of +40 ppm (Mode 1, 9 frames ≈ 0.5 s → ≈160
/// samples of drift, 2.5× the window back-off): the timing loop keeps the
/// FFT window inside the guard interval.
#[test]
fn loopback_sampling_clock_offset() {
    let p = Params::new(
        Mode::M1,
        GuardInterval::G1_8,
        true,
        [
            layer(1, Modulation::Qpsk, CodeRate::R1_2, TimeInterleave::K1),
            layer(12, Modulation::Qam16, CodeRate::R1_2, TimeInterleave::K1),
            None,
        ],
    )
    .unwrap();
    let lat = end_to_end_frames(&p);
    let r = run_sfo(p, lat + 6, channel(Mode::M1, 3000), 0, 21, 40.0);
    assert!(r.frames_checked >= 5);
    assert_eq!(r.bit_errors, [0; 3]);
    assert!(r.start_err <= 8, "start error {}", r.start_err);
}

/// Parameter switching: the transmitter changes the layer configuration
/// (and segment kinds) mid-stream; the receiver re-acquires and reports
/// the new parameters with valid TMCC.
#[test]
fn loopback_parameter_switch_reacquires() {
    let p1 = Params::new(
        Mode::M1,
        GuardInterval::G1_8,
        false,
        [
            layer(13, Modulation::Qpsk, CodeRate::R1_2, TimeInterleave::K0),
            None,
            None,
        ],
    )
    .unwrap();
    let p2 = Params::new(
        Mode::M1,
        GuardInterval::G1_8,
        true,
        [
            layer(1, Modulation::Dqpsk, CodeRate::R1_2, TimeInterleave::K0),
            layer(12, Modulation::Qam16, CodeRate::R2_3, TimeInterleave::K1),
            None,
        ],
    )
    .unwrap();
    let mut rng = Rng(31);
    let mut tx = Vec::new();
    for (p, frames) in [(&p1, 3), (&p2, 7)] {
        let mut m = Modulator::new(p.clone());
        let tmcc = TmccInfo::from_params(p);
        let ac = AcFrame::stuffing(m.layout());
        for _ in 0..frames {
            let bits = Layer::ALL.map(|l| rng.bits(m.bits_per_frame(l)));
            tx.extend(
                m.modulate_frame([&bits[0], &bits[1], &bits[2]], &tmcc, &ac)
                    .unwrap(),
            );
        }
    }
    let rx = channel(Mode::M1, 500).apply(&tx, &mut rng);
    let mut d = Demodulator::new(DemodConfig::default());
    let mut frames = Vec::new();
    for chunk in rx.chunks(50_000) {
        frames.extend(d.push(chunk));
    }
    assert!(frames.iter().any(|f| f.params == p1));
    let after: Vec<_> = frames.iter().filter(|f| f.params == p2).collect();
    assert!(after.len() >= 3, "frames with new params: {}", after.len());
    assert!(after.iter().all(|f| f.tmcc.parity.is_ok()));
}

/// Arbitrary small chunking of the input and a noise-only lead-in longer
/// than the detection window: identical output to one-shot pushing.
#[test]
fn loopback_chunking_and_noise_lead_in() {
    let p = Params::new(
        Mode::M1,
        GuardInterval::G1_4,
        false,
        [
            layer(5, Modulation::Qpsk, CodeRate::R1_2, TimeInterleave::K0),
            layer(8, Modulation::Qam64, CodeRate::R5_6, TimeInterleave::K0),
            None,
        ],
    )
    .unwrap();
    let mut rng = Rng(77);
    let mut m = Modulator::new(p.clone());
    let tmcc = TmccInfo::from_params(&p);
    let ac = AcFrame::stuffing(m.layout());
    let mut tx = Vec::new();
    let mut inputs = Vec::new();
    for _ in 0..4 {
        let bits = Layer::ALL.map(|l| rng.bits(m.bits_per_frame(l)));
        tx.extend(
            m.modulate_frame([&bits[0], &bits[1], &bits[2]], &tmcc, &ac)
                .unwrap(),
        );
        inputs.push(bits);
    }
    let ch = channel(Mode::M1, 150_000);
    let rx = ch.apply(&tx, &mut rng);
    let mut d = Demodulator::new(DemodConfig::default());
    let mut frames = Vec::new();
    let mut i = 0;
    while i < rx.len() {
        let n = 1 + (rng.next_u64() % 3000) as usize;
        frames.extend(d.push(&rx[i..(i + n).min(rx.len())]));
        i += n;
    }
    let lat = end_to_end_frames(&p);
    let mut checked = 0;
    for f in &frames {
        // The frame index is small and non-negative (test code).
        #[allow(clippy::cast_possible_truncation)]
        let idx = ((f.start_sample as f64 - ch.offset as f64) / p.samples_per_frame() as f64)
            .round() as usize;
        if f.valid && idx >= lat {
            for l in [Layer::A, Layer::B] {
                let hard: Vec<u8> = f.llr[l.index()]
                    .as_ref()
                    .unwrap()
                    .iter()
                    .map(|&x| u8::from(x < 0.0))
                    .collect();
                let errs = hard
                    .iter()
                    .zip(&inputs[idx - lat][l.index()])
                    .filter(|(a, b)| a != b)
                    .count();
                assert_eq!(
                    errs, 0,
                    "frame {} start {} layer {l}",
                    f.frame_number, f.start_sample
                );
            }
            checked += 1;
        }
    }
    assert!(checked >= 1);
}

/// Throughput of modulator and demodulator (Mode 3, GI 1/8, 4 frames);
/// prints the real-time factors.  Run with `--ignored --nocapture`.
#[test]
#[ignore]
fn perf_realtime_factor() {
    let p = Params::new(
        Mode::M3,
        GuardInterval::G1_8,
        true,
        [
            layer(1, Modulation::Qpsk, CodeRate::R2_3, TimeInterleave::K2),
            layer(12, Modulation::Qam64, CodeRate::R3_4, TimeInterleave::K2),
            None,
        ],
    )
    .unwrap();
    let mut rng = Rng(1);
    let mut m = Modulator::new(p.clone());
    let tmcc = TmccInfo::from_params(&p);
    let ac = AcFrame::stuffing(m.layout());
    let frames = 4;
    let bits: Vec<_> = (0..frames)
        .map(|_| Layer::ALL.map(|l| rng.bits(m.bits_per_frame(l))))
        .collect();
    let t0 = std::time::Instant::now();
    let mut tx = Vec::new();
    for b in &bits {
        tx.extend(m.modulate_frame([&b[0], &b[1], &b[2]], &tmcc, &ac).unwrap());
    }
    let t_tx = t0.elapsed().as_secs_f64();
    let rx = channel(Mode::M3, 1000).apply(&tx, &mut rng);
    let t1 = std::time::Instant::now();
    let mut d = Demodulator::new(DemodConfig::default());
    let n = d.push(&rx).len();
    let t_rx = t1.elapsed().as_secs_f64();
    let air = f64::from(frames) * p.frame_seconds();
    eprintln!(
        "TX {:.2}× real time, RX {:.2}× real time ({n} frames, {air:.3} s of signal)",
        air / t_tx,
        air / t_rx
    );
}

/// Acquisition helpers on carrier-domain symbols: SP phase, segment kinds,
/// integer CFO.
#[test]
fn acquisition_helpers() {
    use isdbt_ofdm::frame::{FrameLayout, SegmentKind};
    use isdbt_ofdm::rx::acquire::{detect_kinds, detect_sp_phase, integer_cfo};
    let p = Params::new(
        Mode::M2,
        GuardInterval::G1_8,
        true,
        [
            layer(1, Modulation::Qpsk, CodeRate::R1_2, TimeInterleave::K0),
            layer(4, Modulation::Dqpsk, CodeRate::R1_2, TimeInterleave::K0),
            layer(8, Modulation::Qam64, CodeRate::R1_2, TimeInterleave::K0),
        ],
    )
    .unwrap();
    let mut rng = Rng(3);
    let mut m = Modulator::new(p.clone());
    let tmcc = TmccInfo::from_params(&p);
    let ac = AcFrame::stuffing(m.layout());
    let bits = Layer::ALL.map(|l| rng.bits(m.bits_per_frame(l)));
    let sy = m
        .modulate_frame_carriers([&bits[0], &bits[1], &bits[2]], &tmcc, &ac)
        .unwrap();
    let noisy: Vec<Vec<C32>> = sy[5..45]
        .iter()
        .map(|s| s.iter().map(|&v| v + rng.cgauss(0.01)).collect())
        .collect();
    let layout = FrameLayout::from_params(&p);
    let (c, conf) = detect_sp_phase(&layout, &noisy).unwrap();
    // List index j holds frame symbol n = j + 5, so n mod 4 = (j + c) mod 4 with c = 1.
    assert_eq!(c, 1);
    assert!(conf > 0.9);
    let (kinds, _) = detect_kinds(p.mode(), &noisy);
    for (s, kind) in kinds.iter().enumerate() {
        let want = if p.segment_is_differential(s) {
            SegmentKind::Differential
        } else {
            SegmentKind::Coherent
        };
        assert_eq!(*kind, want, "segment {s}");
    }
    // Integer CFO: shift the band by +5 bins in a full N-bin spectrum.
    let n = p.mode().fft_size();
    let full: Vec<Vec<C32>> = noisy
        .iter()
        .map(|s| {
            let mut v = vec![C32::default(); n];
            for (k, &x) in s.iter().enumerate() {
                let b = isdbt_ofdm::ofdm::carrier_bin(p.mode(), k);
                v[(b + 5) % n] = x;
            }
            v
        })
        .collect();
    assert_eq!(integer_cfo(p.mode(), &full, 300).0, 5);
}

/// The experimental unaligned mode (no one-frame buffer) emits on-air frame
/// 0 first; the default aligned mode emits the priming frame −1 first.
#[test]
fn modulator_alignment_option() {
    use isdbt_ofdm::ModulatorOptions;
    let p = Params::new(
        Mode::M1,
        GuardInterval::G1_8,
        false,
        [
            layer(13, Modulation::Qpsk, CodeRate::R1_2, TimeInterleave::K0),
            None,
            None,
        ],
    )
    .unwrap();
    let aligned = Modulator::new(p.clone());
    assert_eq!(
        (aligned.onair_frame_index(), aligned.buffer_frames()),
        (-1, 1)
    );
    let legacy = Modulator::with_options(
        p,
        ModulatorOptions {
            bit_interleave_frame_alignment: false,
            ..ModulatorOptions::default()
        },
    );
    assert_eq!((legacy.onair_frame_index(), legacy.buffer_frames()), (0, 0));
}

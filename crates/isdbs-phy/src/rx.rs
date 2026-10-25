//! The receiver over a buffer of front-end symbols: frames, carrier, TMCC
//! per superframe, and the derotated main signal of every frame (the
//! boundary Phase 3's demapping and decoding start from).

use crate::Cplx;
use crate::carrier::{derotate, track};
use crate::sync::{EndWord, FrameMark, SyncOptions, find_frames};
use crate::tmcc::{block, check, header_bytes};
use isdbs_core::burst::frame_bits;
use isdbs_core::combination::Modulation;
use isdbs_core::frame::{
    BURST_SYMBOLS, BURSTS, FRAME_SYMBOLS, FRAMES, HEADER_SYMBOLS, MAIN_SYMBOLS,
    burst_symbol_position, main_symbol_position,
};
use isdbs_core::slots::runs;
use isdbs_core::tmcc::{TmccInfo, decode_blocks};

/// One received frame.
#[derive(Debug, Clone)]
pub struct Frame {
    /// Where it starts in the symbol stream.
    pub mark: FrameMark,
    /// Index of the frame in its superframe (0 = the W2 frame), when the
    /// superframe start has been seen.
    pub index: Option<usize>,
    /// The 38 976 main-signal symbols, derotated (BPSK "0" on +real), unit
    /// RMS amplitude on the pilots.
    pub main: Vec<Cplx>,
    /// MER of the frame's known symbols (header tails and bursts) against
    /// their BPSK points, dB.
    pub pilot_mer_db: f64,
    /// The header decoded W1 intact and the expected end word.
    pub header_ok: bool,
}

/// One superframe's TMCC.
#[derive(Debug, Clone)]
pub struct Superframe {
    /// Index into [`Reception::frames`] of its first frame.
    pub first_frame: usize,
    /// Decoded information (None when RS(64,48) failed).
    pub info: Option<TmccInfo>,
    /// Bytes RS corrected.
    pub corrected: Option<usize>,
}

/// Everything found in a buffer.
#[derive(Debug, Clone)]
pub struct Reception {
    /// Frames, in stream order.
    pub frames: Vec<Frame>,
    /// Whole superframes (8 consecutive frames from a W2 frame).
    pub superframes: Vec<Superframe>,
    /// Tracked frames that were missing where expected.
    pub slips: usize,
}

/// Receiver settings.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RxOptions {
    /// Frame search thresholds.
    pub sync: SyncOptions,
    /// Carrier loop bandwidth over the pilots, as a fraction of the pilot
    /// rate ([`crate::carrier::track`]).
    pub carrier_bandwidth: f64,
}

impl Default for RxOptions {
    fn default() -> Self {
        Self {
            sync: SyncOptions::default(),
            carrier_bandwidth: 0.05,
        }
    }
}

/// Mean |z|² over mean error² against BPSK `bits` (0 → +1), dB, after
/// scaling to the mean projection.
fn bpsk_mer(v: &[(Cplx, u8)]) -> f64 {
    let a = v
        .iter()
        .map(|&(z, b)| if b == 0 { z.re } else { -z.re })
        .sum::<f64>()
        / v.len().max(1) as f64;
    let err: f64 = v
        .iter()
        .map(|&(z, b)| (z - Cplx::new(if b == 0 { a } else { -a }, 0.0)).norm_sqr())
        .sum::<f64>()
        / v.len().max(1) as f64;
    10.0 * (a * a / err.max(1e-30)).log10()
}

/// Receive every frame in `z` (front-end symbols, any scale).
pub fn receive(z: &[Cplx], o: &RxOptions) -> Reception {
    let (marks, slips) = find_frames(z, &o.sync);
    let pilots = track(z, &marks, o.carrier_bandwidth);
    let burst = frame_bits();
    // every frame on its own (derotation, scaling, header): frames in
    // parallel, results in order
    let one = |k: usize| {
        let m = &marks[k];
        let next = pilots
            .get(k + 1)
            .filter(|_| marks[k + 1].start == m.start + FRAME_SYMBOLS)
            .map(|p| &p[0]);
        let d = derotate(z, m, &pilots[k], next);
        // Scale to unit amplitude on the bursts.
        let mut known: Vec<(Cplx, u8)> = Vec::with_capacity(BURSTS * BURST_SYMBOLS);
        for b in 0..BURSTS {
            for j in 0..BURST_SYMBOLS {
                known.push((d[burst_symbol_position(b, j)], burst[b * BURST_SYMBOLS + j]));
            }
        }
        let amp = known
            .iter()
            .map(|&(v, b)| if b == 0 { v.re } else { -v.re })
            .sum::<f64>()
            / known.len() as f64;
        let g = 1.0 / amp.max(1e-30);
        for v in &mut known {
            v.0 *= g;
        }
        let pilot_mer_db = bpsk_mer(&known);
        let header: Vec<Cplx> = d[..HEADER_SYMBOLS].iter().map(|v| v * g).collect();
        let h = header_bytes(&header, 4.0);
        let c = check(&h);
        let header_ok = c.w1
            && match m.end_word {
                EndWord::W2 => c.w2,
                EndWord::W3 => c.w3,
            };
        let main = (0..MAIN_SYMBOLS)
            .map(|i| d[main_symbol_position(i)] * g)
            .collect();
        (
            Frame {
                mark: *m,
                index: None,
                main,
                pilot_mer_db,
                header_ok,
            },
            block(&h),
        )
    };
    let idx: Vec<usize> = (0..marks.len()).collect();
    let mut frames = crate::par::map(&idx, |&k| one(k));
    // Superframes: a W2 frame followed by 7 consecutive frames.
    let mut superframes = Vec::new();
    let mut k = 0;
    while k + FRAMES <= frames.len() {
        let consecutive = (1..FRAMES)
            .all(|j| frames[k + j].0.mark.start == frames[k].0.mark.start + j * FRAME_SYMBOLS);
        if frames[k].0.mark.end_word == EndWord::W2 && consecutive {
            let blocks = std::array::from_fn(|j| frames[k + j].1);
            let d = decode_blocks(&blocks);
            for j in 0..FRAMES {
                frames[k + j].0.index = Some(j);
            }
            superframes.push(Superframe {
                first_frame: k,
                info: d.info,
                corrected: d.corrected,
            });
            k += FRAMES;
        } else {
            k += 1;
        }
    }
    Reception {
        frames: frames.into_iter().map(|f| f.0).collect(),
        superframes,
        slips,
    }
}

/// Decision-directed MER of a frame's main signal under `info`: each run's
/// symbols against its constellation (BPSK, QPSK at ±45°/±135°, 8PSK;
/// annexed table 46), dB. `None` when no run has symbols.
pub fn main_mer(frame: &Frame, info: &TmccInfo) -> Option<f64> {
    let (mut sig, mut err, mut n) = (0.0, 0.0, 0usize);
    for r in runs(info) {
        let (m, off) = match r.combination.modulation() {
            Modulation::Bpsk => (2, 0.0),
            Modulation::Qpsk => (4, std::f64::consts::FRAC_PI_4),
            Modulation::Tc8psk => (8, 0.0),
        };
        let step = std::f64::consts::TAU / m as f64;
        for &v in &frame.main[r.symbols.clone()] {
            let a = ((v.arg() - off) / step).round() * step + off;
            let d = Cplx::from_polar(1.0, a);
            sig += 1.0;
            err += (v - d).norm_sqr();
            n += 1;
        }
    }
    (n > 0).then(|| 10.0 * (sig / err.max(1e-30)).log10())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testsig::Synth;

    /// The synthetic signal's TMCC decodes on every superframe, with the
    /// same information as sent.
    #[test]
    fn decodes_synthetic_tmcc() {
        let s = Synth::new(3, 9).cfo(-3e-4).noise(0.2);
        let info = s.info.clone();
        let (z, _) = s.symbols(4321);
        let r = receive(&z, &RxOptions::default());
        assert_eq!(r.slips, 0);
        assert_eq!(r.superframes.len(), 3);
        for sf in &r.superframes {
            assert_eq!(sf.info.as_ref(), Some(&info));
        }
        assert!(r.frames.iter().all(|f| f.header_ok));
        let mer = main_mer(&r.frames[3], &info).unwrap();
        // noise 0.2 per component on unit points: MER ≈ 10·log10(1/0.08)
        assert!((mer - 11.0).abs() < 1.0, "{mer}");
    }
}

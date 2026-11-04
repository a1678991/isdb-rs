//! The equaliser's taps from a short IQ capture, before the chain runs:
//! the samples through the front end's model with the parameters the
//! FPGA's front end gets ([`isdb_frontend::fast`], bit-exact with it),
//! its output components through this chain's model ([`super::Chain`]),
//! and the first frame's training sums solved as the host's slow loop
//! solves the FPGA's ([`super::eq::solve_sums`]).
//!
//! The sums are taken at the equaliser's input (stage 4), so they do not
//! depend on the taps in use; the chain's own sums of a later frame of
//! the same signal differ only by the noise and whatever changed in the
//! meantime. A radio's chain can then start on solved taps instead of
//! the identity (`isdb rx --offload auto` with a chain: the acquisition's
//! probe of 0.2 s holds about six frames, and the first frame with a
//! layout, the third after the frame sync, ends within 0.14 s of it).

use super::eq::{Sums, solve_sums};
use super::ldpc::{FixedLdpc, LdpcDecoder};
use super::{Chain, ChainOptions, Output};
use crate::eq::TAPS;
use isdb_fec::ldpc::fixed::LaneResult;
use isdb_fec::ldpc::{Code, NotConverged};
use isdb_frontend::params::{ParamError, Params};
use isdbs3_core::params::CodeRate;

/// Samples per call of the front end: the probe stops at the first frame
/// whose sums solve, so it is fed in pieces.
const CHUNK: usize = 1 << 16;

/// The chain's LDPC decoder for the probe: the TMCC's word decoded (its
/// layout is what opens the training sums), the main signal's words not,
/// since their results do not reach the sums: each is "not converged after
/// 0 iterations" with its hard decisions (positive reads 0). A main
/// signal on the TMCC's rate (61/120) is decoded as well.
struct TmccOnly(FixedLdpc);

impl LdpcDecoder for TmccOnly {
    fn decode(&mut self, code: &Code, words: &[&[i8]]) -> Vec<(LaneResult, Vec<u8>)> {
        if std::ptr::eq(code, isdbs3_core::ldpc::code(CodeRate::TMCC)) {
            return self.0.decode(code, words);
        }
        words
            .iter()
            .map(|w| {
                (
                    Err(NotConverged {
                        iterations: 0,
                        unsatisfied: 0,
                    }),
                    w.iter().map(|&l| u8::from(l < 0)).collect(),
                )
            })
            .collect()
    }
}

/// What [`probe_taps`] solved.
#[derive(Debug, Clone, PartialEq)]
pub struct ProbeTaps {
    /// The taps (Q2.14), as [`solve_sums`] gives them.
    pub taps: [[i16; 2]; TAPS],
    /// The training sums they were solved from (`frame` counts from the
    /// probe's first frame head: 2 with the default layout lag).
    pub sums: Sums,
    /// Samples of the probe read until then.
    pub samples: usize,
    /// Front-end symbols the chain took until then.
    pub symbols: u64,
}

/// The taps of the first frame of `x` (IQ at the rate `params` were
/// designed for, unit scale as `isdbs3_phy::frontend::to_input` reads it)
/// whose training sums solve, through the front end with `params` (on a
/// thread of its own) and the chain's model with its defaults (layout lag
/// 2, no solve of its own), the TMCC decoded with the FPGA decoder's LDPC
/// settings ([`FixedLdpc::hdl`]) and the main signal not decoded: only
/// the TMCC's layout reaches the sums. `Ok(None)` when no frame of `x` gives
/// sums (no frame sync, no TMCC, or `x` too short: the first sums come at
/// the end of the third frame after the frame sync, 0.1 s of signal and
/// up to one frame more to find its head).
///
/// # Errors
///
/// `params` the front end does not take.
pub fn probe_taps(
    x: &[num_complex::Complex<f32>],
    params: &Params,
) -> Result<Option<ProbeTaps>, ParamError> {
    let mut fe = isdb_frontend::fast::FastFrontend::new(params.clone())?;
    let mut chain = Chain::with_ldpc(
        ChainOptions {
            eq_every: 0,
            ..ChainOptions::default()
        },
        Box::new(TmccOnly(FixedLdpc::hdl())),
    );
    // the front end on a thread of its own, a piece ahead of the chain
    let found = std::thread::scope(|sc| {
        let (tx, rx) = std::sync::mpsc::sync_channel::<(usize, Vec<(u16, [i8; 2])>)>(2);
        sc.spawn(move || {
            for piece in x.chunks(CHUNK) {
                let xi = crate::frontend::to_input(piece);
                let mut q = Vec::with_capacity(piece.len());
                fe.process_each(&xi, |s| q.push((s.gain, s.q)));
                // the chain has what it needs once the receiver is gone
                if tx.send((piece.len(), q)).is_err() {
                    break;
                }
            }
        });
        let mut out = Vec::new();
        let mut samples = 0;
        for (n, q) in rx {
            for (gain, v) in q {
                chain.set_agc(gain);
                chain.push(v, &mut out);
            }
            samples += n;
            for o in out.drain(..) {
                let Output::Frame(f) = o else { continue };
                let Some(sums) = f.sums else { continue };
                if let Some(taps) = solve_sums(&sums) {
                    return Some(ProbeTaps {
                        taps,
                        sums,
                        samples,
                        symbols: chain.symbols(),
                    });
                }
            }
        }
        None
    });
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Cplx;
    use crate::chain::synth::{IqOptions, Rng, SynthOptions, iq};
    use crate::frontend::{Trim, acquire, design};

    const FS: f64 = 40e6;

    /// `seconds` of 16APSK at 40 Msps through a symbol-spaced echo the
    /// equaliser has to undo, +120 kHz off.
    fn signal(seconds: f64, snr_db: f64) -> Vec<num_complex::Complex32> {
        let o = SynthOptions {
            carrier_hz: 120e3,
            snr_db,
            seed: 11,
            ..SynthOptions::default()
        };
        let io = IqOptions {
            sample_rate_hz: FS,
            channel: vec![
                Cplx::new(1.0, 0.0),
                Cplx::new(0.12, -0.08),
                Cplx::new(-0.04, 0.03),
            ],
            ..IqOptions::default()
        };
        // whole frames to cover `seconds`, then cut to it
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let (frames, n) = (
            (seconds / 0.033).ceil() as usize + 1,
            (seconds * FS) as usize,
        );
        let (mut x, _) = iq(&o, frames, &io);
        x.truncate(n);
        x
    }

    /// The front end's parameters as `isdb rx` designs them after the
    /// acquisition (32 taps, the image's).
    fn params_for(x: &[num_complex::Complex32]) -> Params {
        let p = acquire(x, FS, None, Trim::Auto).expect("acquire");
        let mut d = design(FS, p.carrier_offset_hz);
        d.trim = p.trim;
        d.taps = 32;
        Params::design(&d).expect("params")
    }

    /// `‖a − b‖ / ‖b‖`.
    fn distance(a: &[[i16; 2]; TAPS], b: &[[i16; 2]; TAPS]) -> f64 {
        let (mut d, mut n) = (0.0, 0.0);
        for (x, y) in a.iter().zip(b) {
            d += (f64::from(x[0]) - f64::from(y[0])).powi(2)
                + (f64::from(x[1]) - f64::from(y[1])).powi(2);
            n += f64::from(y[0]).powi(2) + f64::from(y[1]).powi(2);
        }
        (d / n).sqrt()
    }

    /// The acquisition's 0.2 s probe gives taps from its third frame, and
    /// they are the chain's own solves of the later frames of the same
    /// signal within the noise: what a radio's chain solves itself about
    /// a second after its open.
    #[test]
    fn the_probe_solves_what_the_chain_solves_later() {
        let x = signal(0.4, 15.0);
        let probe = &x[..8_000_000];
        let params = params_for(probe);
        let p = probe_taps(probe, &params)
            .expect("params")
            .expect("taps from the probe");
        assert_eq!(p.sums.frame, 2, "the first frame with a layout (lag 2)");
        assert_eq!(p.sums.count, 6_720, "a whole frame's training symbols");
        assert!(p.samples <= probe.len());
        let identity = super::super::eq::identity();
        assert!(
            distance(&p.taps, &identity) > 0.05,
            "the echo is undone, not ignored: {:?}",
            p.taps
        );
        // the chain over the whole signal, as the FPGA's run: its solves
        // of frames 3 on (none of them the probe's frame 2)
        let mut fe = isdb_frontend::fast::FastFrontend::new(params).expect("front end");
        let mut chain = Chain::new(ChainOptions {
            eq_every: 0,
            ..ChainOptions::default()
        });
        let mut out = Vec::new();
        fe.process_each(&crate::frontend::to_input(&x), |s| {
            chain.push(s.q, &mut out);
        });
        let later: Vec<_> = out
            .iter()
            .filter_map(|o| match o {
                Output::Frame(f) => f.sums.as_ref().filter(|s| s.frame > 2),
                _ => None,
            })
            .filter_map(solve_sums)
            .collect();
        assert!(later.len() >= 6, "{} later solves", later.len());
        for t in &later {
            let d = distance(&p.taps, t);
            assert!(d < 0.05, "‖Δw‖/‖w‖ {d:.3} against a later solve");
        }
    }

    /// Too little signal for a frame with a layout, or noise alone: no
    /// taps (the open falls back to the identity).
    #[test]
    fn no_taps_without_three_whole_frames() {
        let x = signal(0.2, 15.0);
        let params = params_for(&x);
        // 0.09 s: the sync and two frames at most, no layout in force yet
        assert_eq!(probe_taps(&x[..3_600_000], &params).expect("params"), None);
        let mut rng = Rng::new(5);
        let mut g = || {
            // DSP sample: f64 to f32 on purpose
            #[allow(clippy::cast_possible_truncation)]
            let v = (0.2 * rng.normal()) as f32;
            v
        };
        let noise: Vec<num_complex::Complex32> = (0..x.len())
            .map(|_| num_complex::Complex32::new(g(), g()))
            .collect();
        assert_eq!(probe_taps(&noise, &params).expect("params"), None);
    }
}

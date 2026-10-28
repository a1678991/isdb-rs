//! Carrier phase from the known symbols of each frame.
//!
//! Every frame carries 20 + 20 known BPSK symbols in its header (the coded
//! tails of W1 and W2/W3, the same on every frame) and 192 bursts of 4
//! known BPSK symbols, one every 207 symbols (annexed table 51). Their phase
//! is measured, unwrapped along the frame and across consecutive frames,
//! and interpolated linearly in between: a data-aided estimate that cannot
//! lock falsely (the false-lock concern of BO.1408-1 §7 is about loops
//! driven by the bursts alone) and whose ambiguity is fixed by W1 (BPSK
//! "0" at 0 rad, table 46). A residual offset up to about ±1/(2·207) cycle
//! per symbol (±70 kHz at 28.86 Mbaud) unwraps unambiguously; the front
//! end's mixer removes the measured offset first, leaving a few hundred Hz.
//!
//! The pilots' noise is averaged by a short moving average of the
//! unwrapped phases (`smooth` pilots on each side): the measured phase
//! noise of the lab feed (≈ 0.065 rad rms per millisecond, i.e. slow
//! against a 7 µs burst spacing) passes, white noise is reduced.

use crate::Cplx;
use crate::sync::{EndWord, FrameMark, tail_coherent};
use isdbs_core::burst::frame_bits;
use isdbs_core::frame::{
    BURST_SYMBOLS, BURSTS, CODED_TAIL_BITS, FRAME_SYMBOLS, W1_CODED_TAIL, W1_TAIL_AT,
    W2_CODED_TAIL, W3_CODED_TAIL, W23_TAIL_AT, burst_symbol_position,
};
use std::f64::consts::{PI, TAU};

/// One phase measurement on known symbols.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pilot {
    /// Stream index of the pilot's centre.
    pub at: f64,
    /// Phase, rad (the measured one from [`frame_pilots`]; the tracked,
    /// unwrapped estimate after [`track`]).
    pub phase: f64,
    /// Correlation magnitude over Σ|z| (0..1).
    pub quality: f64,
    /// The pilot's correlation Σ p_k z_k.
    pub c: Cplx,
}

/// The pilots of one frame: W1 tail, 192 bursts, W2/W3 tail of the *next*
/// header is not included (each frame ends with its last burst).
pub fn frame_pilots(z: &[Cplx], f: &FrameMark) -> Vec<Pilot> {
    let s = f.start;
    let mut out = Vec::with_capacity(BURSTS + 2);
    let tail = |at: usize, t: u32| {
        let (c, p) = tail_coherent(z, s + at, t);
        Pilot {
            at: (s + at) as f64 + (CODED_TAIL_BITS - 1) as f64 / 2.0,
            phase: c.arg(),
            quality: c.norm() / p,
            c,
        }
    };
    out.push(tail(W1_TAIL_AT, W1_CODED_TAIL));
    let end = match f.end_word {
        EndWord::W2 => W2_CODED_TAIL,
        EndWord::W3 => W3_CODED_TAIL,
    };
    out.push(tail(W23_TAIL_AT, end));
    let bits = frame_bits();
    for b in 0..BURSTS {
        let mut c = Cplx::new(0.0, 0.0);
        let mut p = 0.0;
        for j in 0..BURST_SYMBOLS {
            let v = z[s + burst_symbol_position(b, j)];
            c += if bits[b * BURST_SYMBOLS + j] == 0 {
                v
            } else {
                -v
            };
            p += v.norm();
        }
        out.push(Pilot {
            at: (s + burst_symbol_position(b, 0)) as f64 + 1.5,
            phase: c.arg(),
            quality: c.norm() / p.max(1e-30),
            c,
        });
    }
    out
}

/// Pilots of consecutive frames with their phase tracked: a second-order
/// loop runs over the pilot sequence of each run of consecutive frames,
/// forward and backward, and the two estimates are averaged (a zero-lag
/// smoother). The loop's error is the angle of each pilot's correlation
/// against the prediction, arg(c·e^{−jθ̂}), so a single bad pilot moves the
/// estimate by at most its gain times π and cannot start a cycle slip the
/// way unwrapping noisy measured phases does at low SNR. The loop
/// bandwidth `bandwidth` is a fraction of the pilot rate (one pilot per
/// 207 symbols): 0.05 averages about ten pilots.
pub fn track(z: &[Cplx], frames: &[FrameMark], bandwidth: f64) -> Vec<Vec<Pilot>> {
    let mut all: Vec<Vec<Pilot>> = frames.iter().map(|f| frame_pilots(z, f)).collect();
    let mut k = 0;
    while k < all.len() {
        let mut e = k + 1;
        while e < all.len() && frames[e].start == frames[e - 1].start + FRAME_SYMBOLS {
            e += 1;
        }
        let mut flat: Vec<Pilot> = all[k..e].iter().flatten().copied().collect();
        flat.sort_by(|a, b| a.at.total_cmp(&b.at));
        let fwd = run_loop(&flat, bandwidth);
        let rev: Vec<Pilot> = flat.iter().rev().copied().collect();
        let mut bwd = run_loop(&rev, bandwidth);
        bwd.reverse();
        // Align the backward branch to the forward one (both are unwrapped
        // from their own start), then average.
        let mut tracked: Vec<f64> = fwd
            .iter()
            .zip(&bwd)
            .map(|(&f, &b)| {
                let d = (b - f + PI).rem_euclid(TAU) - PI;
                f + d / 2.0
            })
            .collect();
        // Make the averaged track continuous.
        for i in 1..tracked.len() {
            let d = (tracked[i] - tracked[i - 1] + PI).rem_euclid(TAU) - PI;
            tracked[i] = tracked[i - 1] + d;
        }
        // one tracked phase per pilot of the group, in the same order
        let mut phases = tracked.into_iter();
        for ps in &mut all[k..e] {
            ps.sort_by(|a, b| a.at.total_cmp(&b.at));
            for (p, ph) in ps.iter_mut().zip(&mut phases) {
                p.phase = ph;
            }
        }
        k = e;
    }
    all
}

/// Second-order phase loop over `pilots` (in order); the unwrapped phase
/// estimate at each pilot, after its update.
fn run_loop(pilots: &[Pilot], bn: f64) -> Vec<f64> {
    let zeta = std::f64::consts::FRAC_1_SQRT_2;
    let theta = bn / (zeta + 1.0 / (4.0 * zeta));
    let d = 1.0 + 2.0 * zeta * theta + theta * theta;
    let (kp, ki) = (4.0 * zeta * theta / d, 4.0 * theta * theta / d);
    let Some(first) = pilots.first() else {
        return Vec::new();
    };
    // Start in lock: the frequency from the mean phase advance between
    // consecutive bursts (angle of Σ c_{i+1}·conj(c_i) over pilots 207
    // symbols apart, which is unbiased whatever the burst data signs, as
    // they are removed in c), the phase from the first pilot.
    let mut acc = Cplx::new(0.0, 0.0);
    for w in pilots.windows(2) {
        if ((w[1].at - w[0].at).abs() - 207.0).abs() < 1.0 {
            acc += w[1].c * w[0].c.conj();
        }
    }
    let mut fr = acc.arg();
    let mut ph = first.c.arg();
    let mut out = Vec::with_capacity(pilots.len());
    for (i, p) in pilots.iter().enumerate() {
        if i > 0 {
            // advance by the frequency term over the step (pilots are not
            // evenly spaced around the header; the backward run goes down
            // in time, and its frequency is the negated one)
            let step = (p.at - pilots[i - 1].at).abs() / 207.0;
            ph += fr * step;
        }
        let e = (p.c * Cplx::from_polar(1.0, -ph)).arg();
        fr += ki * e;
        ph += kp * e;
        out.push(ph);
    }
    out
}

/// Phase at stream index `n` by linear interpolation of sorted `pilots`
/// (held at the ends).
pub fn phase_at(pilots: &[Pilot], n: f64) -> f64 {
    match pilots.partition_point(|p| p.at <= n) {
        0 => pilots[0].phase,
        i if i == pilots.len() => pilots[i - 1].phase,
        i => {
            let (a, b) = (pilots[i - 1], pilots[i]);
            a.phase + (b.phase - a.phase) * (n - a.at) / (b.at - a.at)
        }
    }
}

/// The frame's symbols derotated by the interpolated pilot phase (so BPSK
/// "0" lies on the positive real axis), `FRAME_SYMBOLS` of them. The pilots
/// after the frame (the next frame's W1 tail) are used when given, so the
/// last stretch is interpolated rather than held.
pub fn derotate(z: &[Cplx], f: &FrameMark, pilots: &[Pilot], next: Option<&Pilot>) -> Vec<Cplx> {
    let mut ps = pilots.to_vec();
    ps.sort_by(|a, b| a.at.total_cmp(&b.at));
    if let Some(n) = next {
        ps.push(*n);
    }
    (0..FRAME_SYMBOLS)
        .map(|k| {
            let n = f.start + k;
            z[n] * Cplx::from_polar(1.0, -phase_at(&ps, n as f64))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::{SyncOptions, find_frames};
    use crate::testsig::Synth;

    /// With a carrier offset, the derotated header and bursts lie on the
    /// real axis with the right signs across a whole superframe.
    #[test]
    fn derotation_removes_cfo_and_phase() {
        let (z, _) = Synth::new(2, 5).cfo(4e-4).noise(0.05).symbols(900);
        let (f, _) = find_frames(&z, &SyncOptions::default());
        let pil = track(&z, &f, 0.05);
        for k in 0..f.len() - 1 {
            let d = derotate(&z, &f[k], &pil[k], pil.get(k + 1).map(|p| &p[0]));
            let bits = frame_bits();
            for b in 0..BURSTS {
                for j in 0..BURST_SYMBOLS {
                    let v = d[burst_symbol_position(b, j)];
                    let want = if bits[b * BURST_SYMBOLS + j] == 0 {
                        1.0
                    } else {
                        -1.0
                    };
                    assert!(
                        v.re * want > 0.7 && v.im.abs() < 0.3,
                        "frame {k} burst {b}: {v}"
                    );
                }
            }
        }
    }
}

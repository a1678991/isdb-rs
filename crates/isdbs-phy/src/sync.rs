//! Frame and superframe synchronisation on the symbol stream.
//!
//! The search correlates every candidate start with the frame's 808 known
//! BPSK symbols ([`frame_metric`]: the coded tails of W1 and W2/W3 and the
//! 192 bursts), coherently within each group and non-coherently between
//! groups, so the residual carrier offset of the front end's symbols does
//! not matter. The 20-symbol W1 tail alone ([`tail_metric`]) finds the
//! frames of a strong transponder but not those of the weak 110° CS ones:
//! random data reaches its on-frame level somewhere in every frame length
//! (VERIFICATION.md). Once locked, each frame is checked at exactly
//! 39 936 symbols after the previous one; a frame missing there is
//! reported as a slip and the search resumes, which also detects symbols
//! lost upstream (a capture's dropped samples).

use crate::Cplx;
use isdbs_core::frame::{
    CODED_TAIL_BITS, FRAME_SYMBOLS, W1_CODED_TAIL, W1_TAIL_AT, W2_CODED_TAIL, W3_CODED_TAIL,
    W23_TAIL_AT, tail_bits,
};

/// BPSK pattern of a coded tail (bit 0 → +1, table 46).
fn pattern(tail: u32) -> [f64; CODED_TAIL_BITS] {
    tail_bits(tail).map(|b| if b == 0 { 1.0 } else { -1.0 })
}

/// Normalised differential correlation of `z[at..at+20]` with `tail`
/// (0..1; 1 = perfect, sign-insensitive).
pub fn tail_metric(z: &[Cplx], at: usize, tail: u32) -> f64 {
    let p = pattern(tail);
    let (mut acc, mut pw) = (Cplx::new(0.0, 0.0), 0.0);
    for k in 0..CODED_TAIL_BITS - 1 {
        let (a, b) = (z[at + k], z[at + k + 1]);
        acc += a * b.conj() * (p[k] * p[k + 1]);
        pw += a.norm() * b.norm();
    }
    acc.norm() / pw.max(1e-30)
}

/// Coherent correlation of `z[at..at+20]` with `tail`: Σ p_k z_k. Its
/// angle is the carrier phase at the tail (BPSK 0 = 0 rad), its magnitude
/// over Σ|z_k| the match quality.
pub fn tail_coherent(z: &[Cplx], at: usize, tail: u32) -> (Cplx, f64) {
    let p = pattern(tail);
    let (mut acc, mut pw) = (Cplx::new(0.0, 0.0), 0.0);
    for (k, &pk) in p.iter().enumerate() {
        acc += z[at + k] * pk;
        pw += z[at + k].norm();
    }
    (acc, pw.max(1e-30))
}

/// Frame-signature metric of a candidate start `s`: the known BPSK symbols
/// of the frame (W1 tail, 192 bursts of 4, W2 or W3 tail), each group
/// correlated coherently and the group magnitudes summed (non-coherent
/// between groups, so a residual carrier offset does not matter), over the
/// summed |z| of those symbols. 808 known symbols against the 20 of a
/// single tail: about 16 dB more processing gain, which finds the frames of
/// the weak 110° CS transponders where the tail alone drowns in data. The
/// end word is unknown at acquisition, so the better of W2 and W3 counts.
pub fn frame_metric(z: &[Cplx], s: usize) -> f64 {
    use isdbs_core::burst::frame_bits;
    use isdbs_core::frame::{BURST_SYMBOLS, BURSTS, burst_symbol_position};
    use std::sync::OnceLock;
    static BITS: OnceLock<[f64; BURSTS * BURST_SYMBOLS]> = OnceLock::new();
    let bits = BITS.get_or_init(|| frame_bits().map(|b| if b == 0 { 1.0 } else { -1.0 }));
    let (c1, p1) = tail_coherent(z, s + W1_TAIL_AT, W1_CODED_TAIL);
    let (c2, p2) = tail_coherent(z, s + W23_TAIL_AT, W2_CODED_TAIL);
    let (c3, _) = tail_coherent(z, s + W23_TAIL_AT, W3_CODED_TAIL);
    let (mut acc, mut pw) = (c1.norm() + c2.norm().max(c3.norm()), p1 + p2);
    for b in 0..BURSTS {
        let at = s + burst_symbol_position(b, 0);
        let mut c = Cplx::new(0.0, 0.0);
        for j in 0..BURST_SYMBOLS {
            c += z[at + j] * bits[b * BURST_SYMBOLS + j];
            pw += z[at + j].norm();
        }
        acc += c.norm();
    }
    acc / pw.max(1e-30)
}

/// Which word ends a header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndWord {
    /// W2: the frame is frame 1 of its superframe.
    W2,
    /// W3: frames 2–8.
    W3,
}

/// A frame found in the stream.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FrameMark {
    /// Index of the frame's first symbol in the stream.
    pub start: usize,
    /// The word that ends its header.
    pub end_word: EndWord,
    /// [`frame_metric`] at the start (0..1).
    pub metric: f64,
}

/// Thresholds of the search, on [`frame_metric`]: random data scores
/// 0.44–0.48 (median) and at most 0.55 over 20 000 offsets; frames score
/// 0.99 on a strong transponder and above 0.81 on the weakest recorded
/// (MER 4.7 dB).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SyncOptions {
    /// Minimum metric to accept an acquisition.
    pub acquire: f64,
    /// Minimum metric of a tracked frame before it counts as missing.
    pub track: f64,
}

impl Default for SyncOptions {
    fn default() -> Self {
        Self {
            acquire: 0.65,
            track: 0.62,
        }
    }
}

/// The end word of a header at `start`, if its tail matches one coherently
/// in phase with W1.
fn end_word(z: &[Cplx], start: usize) -> Option<(EndWord, f64)> {
    let (c1, p1) = tail_coherent(z, start + W1_TAIL_AT, W1_CODED_TAIL);
    let ph = c1 / c1.norm().max(1e-30);
    let q = |tail| {
        let (c, p) = tail_coherent(z, start + W23_TAIL_AT, tail);
        (c * ph.conj()).re / p
    };
    let (q2, q3) = (q(W2_CODED_TAIL), q(W3_CODED_TAIL));
    let w1q = c1.norm() / p1;
    // W3 = ¬W2 but the coded tails are not complements, so both are
    // tested; the better coherent match in phase with W1 wins.
    Some(if q2 > q3 {
        (EndWord::W2, w1q)
    } else {
        (EndWord::W3, w1q)
    })
}

/// Find every frame in `z`: acquire, then track at 39 936-symbol steps.
/// Returns the frames in order and the number of slips (a tracked frame
/// missing, followed by a new acquisition).
pub fn find_frames(z: &[Cplx], o: &SyncOptions) -> (Vec<FrameMark>, usize) {
    let mut frames = Vec::new();
    let mut slips = 0;
    let mut at = 0usize;
    let last = z.len().saturating_sub(FRAME_SYMBOLS);
    'search: while at <= last {
        // Acquire: best metric over one frame length from `at`.
        let end = (at + FRAME_SYMBOLS).min(last + 1);
        // the first offset with the best metric, offsets shared out
        // between threads (crate::par::first_max: as a sequential scan)
        let best = crate::par::first_max(at..end, 0.0, |s| frame_metric(z, s));
        if best.0 < o.acquire {
            at = end;
            continue;
        }
        let mut s = best.1;
        let Some((w, _)) = end_word(z, s) else {
            at = s + 1;
            continue;
        };
        frames.push(FrameMark {
            start: s,
            end_word: w,
            metric: best.0,
        });
        // Track.
        loop {
            s += FRAME_SYMBOLS;
            if s > last {
                break 'search;
            }
            let m = frame_metric(z, s);
            match (m >= o.track).then(|| end_word(z, s)).flatten() {
                Some((w, _)) => frames.push(FrameMark {
                    start: s,
                    end_word: w,
                    metric: m,
                }),
                None => {
                    slips += 1;
                    at = s - FRAME_SYMBOLS + 1;
                    continue 'search;
                }
            }
        }
    }
    (frames, slips)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testsig::Synth;

    /// Frames of a synthetic signal are found at their exact starts, with
    /// the superframe marked by W2 every 8 frames, under a carrier offset.
    #[test]
    fn finds_synthetic_frames() {
        let s = Synth::new(3, 7).cfo(2e-5).noise(0.1);
        let (z, starts) = s.symbols(1234);
        let (f, slips) = find_frames(&z, &SyncOptions::default());
        assert_eq!(slips, 0);
        assert_eq!(f.iter().map(|m| m.start).collect::<Vec<_>>(), starts);
        let w2: Vec<usize> = f
            .iter()
            .enumerate()
            .filter(|(_, m)| m.end_word == EndWord::W2)
            .map(|(i, _)| i)
            .collect();
        assert!(w2.windows(2).all(|w| w[1] - w[0] == 8), "{w2:?}");
    }

    /// Symbols dropped between two frames are reported as one slip and the
    /// following frames are found at their shifted starts.
    #[test]
    fn reports_dropped_symbols() {
        let (mut z, starts) = Synth::new(3, 1).noise(0.05).symbols(500);
        let cut = starts[1] + 20_000;
        z.drain(cut..cut + 1024);
        let (f, slips) = find_frames(&z, &SyncOptions::default());
        assert_eq!(slips, 1);
        assert_eq!(f[0].start, starts[0]);
        assert_eq!(f[1].start, starts[1]);
        assert_eq!(f[2].start, starts[2] - 1024);
    }
}

//! A symbol-spaced linear equaliser trained by least squares on the known
//! symbols of a frame: the 24 synchronisation symbols and the 32 pilot
//! symbols of every modulation slot (ARIB STD-B44 v2.1 Fig. 3.3-1, §3.9),
//! 6 720 per frame. It removes the intersymbol interference the front end
//! leaves (the trim corrects the sampling instant, not the pulse shape).
//!
//! The pilots' reference points are the nominal constellation of the
//! slot's modulation: equalising towards the nominal points also undoes a
//! linear part of the transponder's distortion, while the learned
//! constellation (`main_signal::learned_points`) still absorbs the
//! non-linear part (ring compression).

use crate::Cplx;
use crate::sync::{SlotHead, phase_at};
use isdb_frontend::s3sync::sync_symbols;
use isdbs3_core::dispersal::pilot_signs;
use isdbs3_core::params::{FSYNC, MOD_SLOT, PILOT_SYMBOLS, SLOTS, SSYNC, SYNC_SYMBOLS};
use isdbs3_core::slots::Slot;

/// Equaliser taps (odd), centred.
pub const TAPS: usize = 11;

/// The known symbols of the frame starting at `slots[f]`: (index into `z`,
/// reference value at unit sync amplitude, already rotated by the slot's
/// carrier phase and scaled by its amplitude so the equaliser only has to
/// fix the pulse).
fn training(z: &[Cplx], slots: &[SlotHead], f: usize, layout: &[Slot]) -> Vec<(usize, Cplx)> {
    let signs = pilot_signs();
    let fs = sync_symbols(FSYNC);
    let ss = sync_symbols(SSYNC);
    let mut out = Vec::with_capacity(SLOTS * (SYNC_SYMBOLS + PILOT_SYMBOLS));
    // which parity carries SSync: the frame head (slot #1) carries FSync
    for (k, i) in (f..f + SLOTS).enumerate() {
        let at = slots[i].at;
        if at + MOD_SLOT > z.len() {
            break;
        }
        let word = if k % 2 == 1 { &ss } else { &fs };
        // FSync or !FSync: the sign of the slot's correlation with FSync
        // against its (sign-free) carrier phase
        let c: Cplx = word.iter().zip(&z[at..]).map(|(w, v)| v * w.conj()).sum();
        let sign = if (c * slots[i].phase.conj()).re < 0.0 {
            -1.0
        } else {
            1.0
        };
        for (j, w) in word.iter().enumerate() {
            let (ph, amp) = phase_at(slots, i, j);
            out.push((at + j, w * sign * ph * amp));
        }
        let m = layout[k].modulation;
        let pts = isdbs3_core::constellation::points(m, layout[k].rate);
        let n = 1usize << m.bits();
        for t in 0..PILOT_SYMBOLS {
            let j = SYNC_SYMBOLS + t;
            let (ph, amp) = phase_at(slots, i, j);
            let (pi, pq) = pts[t % n];
            let s = f64::from(signs[k * PILOT_SYMBOLS + t]);
            out.push((at + j, Cplx::new(pi, pq) * s * ph * amp));
        }
    }
    out
}

/// Solve the normal equations (Hermitian, `TAPS`²) by Gaussian
/// elimination with partial pivoting.
fn solve(mut a: Vec<Vec<Cplx>>, mut b: Vec<Cplx>) -> Option<Vec<Cplx>> {
    let n = b.len();
    for c in 0..n {
        let p = (c..n).max_by(|&x, &y| a[x][c].norm().total_cmp(&a[y][c].norm()))?;
        if a[p][c].norm() < 1e-12 {
            return None;
        }
        a.swap(c, p);
        b.swap(c, p);
        let (top, rest) = a.split_at_mut(c + 1);
        let pivot = &top[c];
        for (r, row) in rest.iter_mut().enumerate() {
            let f = row[c] / pivot[c];
            for (x, &v) in row[c..].iter_mut().zip(&pivot[c..]) {
                *x -= f * v;
            }
            let v = b[c];
            b[c + 1 + r] -= f * v;
        }
    }
    let mut x = vec![Cplx::new(0.0, 0.0); n];
    for r in (0..n).rev() {
        let mut s = b[r];
        for (ak, xk) in a[r][r + 1..].iter().zip(&x[r + 1..]) {
            s -= ak * xk;
        }
        x[r] = s / a[r][r];
    }
    Some(x)
}

/// The least-squares equaliser of the frame at `slots[f]`; `None` if the
/// frame is incomplete or the system is singular.
pub fn train(z: &[Cplx], slots: &[SlotHead], f: usize, layout: &[Slot]) -> Option<Vec<Cplx>> {
    if f + SLOTS > slots.len() {
        return None;
    }
    let h = TAPS / 2;
    let tr = training(z, slots, f, layout);
    let mut a = vec![vec![Cplx::new(0.0, 0.0); TAPS]; TAPS];
    let mut b = vec![Cplx::new(0.0, 0.0); TAPS];
    for &(n, d) in &tr {
        if n < h || n + h >= z.len() {
            continue;
        }
        let x: Vec<Cplx> = (0..TAPS).map(|k| z[n + h - k]).collect();
        for r in 0..TAPS {
            for c in 0..TAPS {
                a[r][c] += x[r].conj() * x[c];
            }
            b[r] += x[r].conj() * d;
        }
    }
    solve(a, b)
}

/// Apply taps `w` to `z` (output aligned with the input index).
pub fn apply(z: &[Cplx], w: &[Cplx]) -> Vec<Cplx> {
    let h = w.len() / 2;
    (0..z.len())
        .map(|n| {
            if n < h || n + h >= z.len() {
                return z[n];
            }
            w.iter().enumerate().map(|(k, &c)| c * z[n + h - k]).sum()
        })
        .collect()
}

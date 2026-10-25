//! Frame and slot synchronisation of ISDB-S3 symbols, and the carrier
//! phase of every modulation slot from its 24 synchronisation symbols
//! (ARIB STD-B44 v2.1 Fig. 3.3-1, §3.8).
//!
//! Slot heads come from [`isdb_frontend::s3sync::measure`]: the slot sync
//! correlation at the 9 296-symbol period. SSync (36715Ah) heads one slot
//! parity. The other parity carries FSync (52F866h) or !FSync (AD0799h);
//! against the phase of the neighbouring SSync slots one slot in 120 has
//! the opposite sign to the other 59 of its parity. That slot is the frame
//! head, modulation slot #1 (Fig. 3.3-1 puts FSync in slot #1;
//! SPEC-NOTES "Frame marker").
//!
//! The carrier phase of a slot is the angle of its sync correlation (the
//! word's sign removed); between two slot heads it is interpolated
//! linearly, unwrapped.

use crate::Cplx;
use isdb_frontend::s3sync::{self, SYNC_LEN, sync_symbols};
use isdbs3_core::params::{FRAME_SYMBOLS, FSYNC, MOD_SLOT, SLOTS, SSYNC};

/// One modulation slot found in the symbol stream.
#[derive(Debug, Clone, Copy)]
pub struct SlotHead {
    /// Index of its first symbol in the stream.
    pub at: usize,
    /// Slot number within its frame (0 = slot #1), if the frame head was
    /// found before it.
    pub number: Option<usize>,
    /// Carrier phase at the slot head (unit phasor).
    pub phase: Cplx,
    /// Amplitude of the sync symbols (stream scale).
    pub amp: f64,
}

/// The synchronised stream.
#[derive(Debug, Clone)]
pub struct Synced {
    /// Every whole slot head.
    pub slots: Vec<SlotHead>,
    /// Indices into `slots` of each frame head (slot #1).
    pub frames: Vec<usize>,
    /// Es/N0 on the sync symbols, dB (the `s3sync` figure).
    pub sync_snr_db: f64,
    /// Offset of the first slot head (0 … 9 295).
    pub offset: usize,
}

fn corr(x: &[Cplx], at: usize, r: &[Cplx; SYNC_LEN]) -> Cplx {
    r.iter()
        .zip(&x[at..at + SYNC_LEN])
        .map(|(a, b)| b * a.conj())
        .sum()
}

/// Find slots and frames in `z` (one value per symbol, any scale).
pub fn synchronise(z: &[Cplx]) -> Option<Synced> {
    synced(z, s3sync::measure(z)?)
}

/// [`synchronise`] with the slot offset known (`Synced::offset` of an
/// earlier call on symbols at the same positions, e.g. before an
/// equaliser whose output is aligned with its input): no search.
pub fn synchronise_at(z: &[Cplx], offset: usize) -> Option<Synced> {
    synced(z, s3sync::measure_at(z, offset)?)
}

fn synced(z: &[Cplx], rep: s3sync::SyncReport) -> Option<Synced> {
    let fs = sync_symbols(FSYNC);
    let ss = sync_symbols(SSYNC);
    let n = rep.slots;
    // per slot: correlation with its parity's word
    let raw: Vec<(usize, Cplx, bool)> = (0..n)
        .map(|k| {
            let at = rep.offset + k * MOD_SLOT;
            let is_ss = k % 2 == rep.ssync_parity;
            let c = corr(z, at, if is_ss { &ss } else { &fs });
            (at, c, is_ss)
        })
        .collect();
    // sign of each FSync-parity slot against its SSync neighbours
    let sign: Vec<Option<bool>> = (0..n)
        .map(|k| {
            if raw[k].2 {
                return None;
            }
            let nb: Cplx = [k.wrapping_sub(1), k + 1]
                .iter()
                .filter(|&&j| j < n)
                .map(|&j| raw[j].1 / raw[j].1.norm().max(1e-30))
                .sum();
            (nb.norm() > 0.0).then(|| (raw[k].1 * nb.conj()).re < 0.0)
        })
        .collect();
    let neg = sign.iter().filter(|s| **s == Some(true)).count();
    let pos = sign.iter().filter(|s| **s == Some(false)).count();
    // the minority sign marks the frame head
    let marker = neg < pos;
    let mut frames = Vec::new();
    let mut slots = Vec::with_capacity(n);
    let mut number: Option<usize> = None;
    for k in 0..n {
        if sign[k] == Some(marker) {
            // a frame head; accept it only at the right spacing once locked
            if frames.last().is_none_or(|&f| k - f == SLOTS) {
                frames.push(k);
                number = Some(0);
            }
        } else if let Some(m) = number.as_mut() {
            *m = (*m + 1) % SLOTS;
        }
        let (at, c, _) = raw[k];
        // remove the word's sign: a slot negative against its SSync
        // neighbours carries the inverted word
        let c = if sign[k] == Some(true) { -c } else { c };
        slots.push(SlotHead {
            at,
            number,
            phase: c / c.norm().max(1e-30),
            amp: c.norm() / SYNC_LEN as f64,
        });
    }
    let _ = FRAME_SYMBOLS;
    Some(Synced {
        slots,
        frames,
        sync_snr_db: rep.snr_db,
        offset: rep.offset,
    })
}

/// The carrier phase at symbol `j` (0 … 9 295) of slot `i`, interpolated
/// linearly between slot `i`'s head and the next one's (held after the
/// last slot), and the amplitude likewise.
pub fn phase_at(s: &[SlotHead], i: usize, j: usize) -> (Cplx, f64) {
    let a = s[i];
    let Some(b) = s.get(i + 1) else {
        return (a.phase, a.amp);
    };
    let d = (b.phase * a.phase.conj()).arg();
    let t = j as f64 / MOD_SLOT as f64;
    (
        a.phase * Cplx::from_polar(1.0, d * t),
        a.amp + (b.amp - a.amp) * t,
    )
}

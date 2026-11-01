//! Stage 3: each symbol divided by its slot's carrier phase and
//! amplitude, linearly interpolated between the slot's head and the
//! next head (module docs of [`crate::chain`], "Stage 3").

use super::fx::{SLOT_RECIP, cos_sin_round, round_shift, sat16, signed_angle};
use super::sync::{Head, Word};
use isdbs3_core::params::MOD_SLOT;

/// Unit of the corrected symbols: 1.0 (the sync symbols' amplitude, the
/// main signal's RMS, §3.8) is `2^UNIT_BITS`.
pub const UNIT_BITS: u32 = 10;
/// `round(2^(10 + 17) · 24·√2)`: the gain numerator. A head's magnitude
/// is 24·√2 · A for sync symbols of amplitude A in r units (each
/// integer word symbol has magnitude √2), so `G = GAIN_NUM / amp` maps a
/// symbol of amplitude A to 1.0 = 2¹⁰, with 17 fractional bits.
pub const GAIN_NUM: i64 = 4_555_500_750;
/// Fractional bits of the gain.
pub const GAIN_FRAC: u32 = 17;
/// Fractional bits of the per-symbol phase and gain steps.
pub const INTERP_FRAC: u32 = 16;
/// Smallest head magnitude a gain is computed for (below, the gain of
/// magnitude 64 is used: a slot that weak decodes nothing anyway).
pub const MIN_AMP: i64 = 64;

/// The gain `round(GAIN_NUM / max(amp, 64))` (the reciprocal of the
/// stage's docs; one divider per slot head, 17-bit quotient).
pub fn gain_of(amp: i64) -> i64 {
    super::fx::div_round(GAIN_NUM, amp.max(MIN_AMP))
}

/// One corrected symbol and the values the HDL compares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Corrected {
    /// Input index (chain symbols).
    pub at: u64,
    /// The word its slot's head carried.
    pub word: Word,
    /// Slot number (0 = slot #1) and frame counter, when known.
    pub number: Option<u8>,
    /// See `number`.
    pub frame: Option<u32>,
    /// Position in the slot (0 … 9 295).
    pub j: u16,
    /// The NCO output it was computed from.
    pub r: [i16; 2],
    /// The interpolated phase (binary angle) …
    pub phi: u32,
    /// … and gain (Q.17 from r units to Q.10).
    pub gain: i64,
    /// The corrected symbol, Q.10 (1.0 = 1 024), saturated to `i16`.
    pub y: [i16; 2],
}

/// Stage 3's state: the symbols of the slot being corrected and its two
/// heads.
#[derive(Debug, Clone)]
pub struct SlotPhase {
    cos: Vec<i16>,
    /// The current slot's head and its NCO outputs.
    cur: Option<(Head, Vec<[i16; 2]>)>,
}

impl Default for SlotPhase {
    fn default() -> Self {
        Self::new()
    }
}

impl SlotPhase {
    /// Nothing held.
    pub fn new() -> Self {
        Self {
            cos: isdb_frontend::tables::cos_table(),
            cur: None,
        }
    }

    /// Drop what is held (a restart).
    pub fn restart(&mut self) {
        self.cur = None;
    }

    /// Whether a slot is held (symbols are being collected).
    pub fn holding(&self) -> bool {
        self.cur.is_some()
    }

    /// One NCO output symbol of the current slot (ignored before the
    /// first head).
    pub fn push(&mut self, r: [i16; 2]) {
        if let Some((_, v)) = self.cur.as_mut()
            && v.len() < MOD_SLOT
        {
            v.push(r);
        }
    }

    /// The next head: the held slot (if whole) is corrected between its
    /// head and `h` into `out`, and `h`'s slot starts.
    pub fn head(&mut self, h: Head, out: &mut Vec<Corrected>) {
        if let Some((a, v)) = self.cur.take()
            && v.len() == MOD_SLOT
        {
            self.correct(&a, &h, &v, out);
        }
        self.cur = Some((h, Vec::with_capacity(MOD_SLOT)));
    }

    fn correct(&self, a: &Head, b: &Head, v: &[[i16; 2]], out: &mut Vec<Corrected>) {
        // per-symbol steps, Q.16, one multiply each per slot
        let d = signed_angle(b.theta.wrapping_sub(a.theta));
        let delta = round_shift(d * SLOT_RECIP, 37 - INTERP_FRAC);
        let (ga, gb) = (gain_of(a.amp), gain_of(b.amp));
        let gamma = round_shift((gb - ga) * SLOT_RECIP, 37 - INTERP_FRAC);
        for (j, &r) in v.iter().enumerate() {
            let jj = i64::try_from(j).expect("j < 9 296");
            let phi = a
                .theta
                .wrapping_add(step_angle(round_shift(delta * jj, INTERP_FRAC)));
            let gain = ga + round_shift(gamma * jj, INTERP_FRAC);
            let (c, s) = cos_sin_round(&self.cos, phi);
            let (ri, rq) = (i64::from(r[0]), i64::from(r[1]));
            // r · e^{−jφ}, Q.15 table, then the gain
            let u = round_shift(ri * c + rq * s, 15);
            let w = round_shift(rq * c - ri * s, 15);
            let y = [
                sat16(round_shift(u * gain, GAIN_FRAC)),
                sat16(round_shift(w * gain, GAIN_FRAC)),
            ];
            out.push(Corrected {
                at: a.at + u64::try_from(j).expect("j < 9 296"),
                word: a.word,
                number: a.number,
                frame: a.frame,
                j: u16::try_from(j).expect("j < 9 296"),
                r,
                phi,
                gain,
                y,
            });
        }
    }
}

/// A signed step (|step| < 2³¹) as the binary angle it adds.
fn step_angle(step: i64) -> u32 {
    // a signed angle within ±2³¹ by construction (|d| ≤ 2³¹, j / 9 296 < 1);
    // the cast keeps its two's-complement bits, which is the wrapping add
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let a = step as u32;
    a
}

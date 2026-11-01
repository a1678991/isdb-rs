//! Stage 1, slot and frame synchronisation, and stage 2, the carrier
//! loop with its NCO (module docs of [`crate::chain`], "Stage 1" and
//! "Stage 2", are the specification).

use super::fx::{cordic, cos_sin_round, mag_approx, round_shift};
use isdb_frontend::s3sync::sync_symbols;
use isdbs3_core::params::{FSYNC, MOD_SLOT, SLOTS, SSYNC, SYNC_SYMBOLS};

/// The slot period as `u64` (9 296 symbols, Fig. 3.3-1).
const SLOT_U: u64 = MOD_SLOT as u64;

/// Leak of the per-position accumulator: `A += (m − A) >> LEAK_SHIFT`
/// once per slot period (a time constant of 8 slots).
pub const LEAK_SHIFT: u32 = 3;
/// A period's peak counts when `peak · 9 296 ≥ LOCK_RATIO · Σ A` (the
/// peak at least this many times the period's mean).
pub const LOCK_RATIO: u64 = 3;
/// Consecutive periods with the same counting peak that lock the slot
/// sync.
pub const LOCK_PERIODS: u32 = 4;
/// Consecutive periods whose peak is elsewhere (or does not count) that
/// lose it.
pub const LOSS_PERIODS: u32 = 8;
/// The FSync-parity sign majority counter saturates at ±this.
pub const MAJORITY_MAX: i32 = 8;
/// A sign against a majority counter of at least this magnitude marks a
/// frame-head candidate.
pub const MAJORITY_MIN: i32 = 2;
/// Consecutive frame heads expected and not marked that lose the frame
/// sync (and restart the chain).
pub const FRAME_MISS_LIMIT: u32 = 2;

/// The word a slot head carries, as read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Word {
    /// SSync (36715Ah).
    Ssync,
    /// FSync (52F866h): positive against the previous SSync head.
    Fsync,
    /// !FSync (AD0799h): negative against it.
    NotFsync,
}

/// One slot head: what stage 1 hands to stages 2 and 3.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Head {
    /// Index of its first sync symbol in the chain's input (symbols since
    /// the chain's start, losses included).
    pub at: u64,
    /// The word read.
    pub word: Word,
    /// Correlation with the slot's word, sign removed (`!FSync` negated):
    /// 24·√2 · amplitude · e^{jθ} in r units ([`Nco`] output).
    pub corr: [i32; 2],
    /// Its angle (binary angle, [`cordic`]).
    pub theta: u32,
    /// Its magnitude ([`cordic`]).
    pub amp: i64,
    /// Slot number in the frame (0 = slot #1), once the frame head has
    /// been found.
    pub number: Option<u8>,
    /// Frame counter of the frame this slot belongs to (with `number`).
    pub frame: Option<u32>,
    /// The NCO phase at the head's first symbol (informational: θ +
    /// `nco_phase` is the carrier phase of the input itself).
    pub nco_phase: u32,
}

/// Sync detector statistics of the last whole slot period.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SyncStats {
    /// The period's largest accumulator value.
    pub peak: u32,
    /// Its position (0 … 9 295, symbols after a period start).
    pub peak_pos: u16,
    /// The sum of the period's accumulator values (mean · 9 296).
    pub sum: u64,
    /// Whether the slot sync is locked.
    pub locked: bool,
    /// The locked offset (position of the slot heads).
    pub offset: u16,
    /// SSync parity (0: the first head after lock carries SSync).
    pub ssync_parity: u8,
    /// Frame heads marked where none was expected.
    pub stray_markers: u32,
    /// Frame heads expected and not marked.
    pub missed_markers: u32,
}

/// What [`SlotSync::push`] reports besides heads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncEvent {
    /// A slot head (in order).
    Head(Head),
    /// The slot or frame sync was lost: the chain restarts.
    Lost,
}

/// The FSync and SSync words as integer symbols `(±1 ± j)` = √2 × the
/// unit symbol of §3.8 (angles π/4 + k·π/2), first symbol odd.
pub fn word_symbols(word: u32) -> [[i8; 2]; SYNC_SYMBOLS] {
    let s = sync_symbols(word);
    std::array::from_fn(|k| [sgn(s[k].re), sgn(s[k].im)])
}

fn sgn(v: f64) -> i8 {
    if v < 0.0 { -1 } else { 1 }
}

/// Correlation `Σ r_k · conj(w_k)` of 24 symbols with a word `w` of
/// `(±1 ± j)` symbols: re = Σ (r_i w_i + r_q w_q), im = Σ (r_q w_i − r_i w_q),
/// sums of ±r only.
pub fn correlate(r: &[[i16; 2]], w: &[[i8; 2]; SYNC_SYMBOLS]) -> [i32; 2] {
    let (mut re, mut im) = (0i32, 0i32);
    for (x, c) in r.iter().zip(w) {
        let (xi, xq) = (i32::from(x[0]), i32::from(x[1]));
        let (wi, wq) = (i32::from(c[0]), i32::from(c[1]));
        re += xi * wi + xq * wq;
        im += xq * wi - xi * wq;
    }
    [re, im]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Search { cand: u16, count: u32 },
    Locked { offset: u16, misses: u32 },
}

/// Stage 1: the slot sync correlator, the per-position accumulator, the
/// lock rule and the head records with their frame numbering.
#[derive(Debug, Clone)]
pub struct SlotSync {
    fs: [[i8; 2]; SYNC_SYMBOLS],
    ss: [[i8; 2]; SYNC_SYMBOLS],
    /// The last 24 inputs (ring, oldest at `head`).
    win: [[i16; 2]; SYNC_SYMBOLS],
    head: usize,
    /// Symbols pushed since the (re)start.
    t: u64,
    /// Symbol index of the (re)start in the chain's input.
    base: u64,
    acc: Vec<u32>,
    period: SyncStats,
    stats: SyncStats,
    state: State,
    /// Heads since lock (for the parity), and the first one held until
    /// the parity is known: (at, corr F, corr S).
    heads: u64,
    first: Option<(u64, [i32; 2], [i32; 2])>,
    parity: Option<u8>,
    last_ss: Option<[i32; 2]>,
    majority: i32,
    number: Option<u8>,
    frame: Option<u32>,
    last_frame_at: Option<(u64, u32)>,
    misses: u32,
    /// The correlations at the last position (for the vectors).
    pub last_corr: ([i32; 2], [i32; 2], u32),
}

impl Default for SlotSync {
    fn default() -> Self {
        Self::new()
    }
}

impl SlotSync {
    /// A detector at the start of a stream.
    pub fn new() -> Self {
        Self {
            fs: word_symbols(FSYNC),
            ss: word_symbols(SSYNC),
            win: [[0; 2]; SYNC_SYMBOLS],
            head: 0,
            t: 0,
            base: 0,
            acc: vec![0; MOD_SLOT],
            period: SyncStats::default(),
            stats: SyncStats::default(),
            state: State::Search { cand: 0, count: 0 },
            heads: 0,
            first: None,
            parity: None,
            last_ss: None,
            majority: 0,
            number: None,
            frame: None,
            last_frame_at: None,
            misses: 0,
            last_corr: ([0; 2], [0; 2], 0),
        }
    }

    /// Restart at chain input symbol `at` (the next symbol pushed): the
    /// accumulator cleared, unlocked, the frame numbering dropped; the
    /// frame counter continues from the last frame head
    /// ([`crate::chain`] "Frame counter").
    pub fn restart(&mut self, at: u64) {
        let keep = self.last_frame_at;
        let (fs, ss) = (self.fs, self.ss);
        *self = Self::new();
        self.fs = fs;
        self.ss = ss;
        self.base = at;
        self.last_frame_at = keep;
    }

    /// The statistics of the last whole period.
    pub fn stats(&self) -> SyncStats {
        self.stats
    }

    /// Push one NCO output symbol; appends the heads (and a loss) it
    /// completes to `out`.
    pub fn push(&mut self, r: [i16; 2], out: &mut Vec<SyncEvent>) {
        self.win[self.head] = r;
        self.head = (self.head + 1) % SYNC_SYMBOLS;
        self.t += 1;
        if self.t < SYNC_SYMBOLS as u64 {
            return;
        }
        // the window, oldest first, starts at position n
        let n = self.t - SYNC_SYMBOLS as u64;
        let mut w = [[0i16; 2]; SYNC_SYMBOLS];
        for (k, v) in w.iter_mut().enumerate() {
            *v = self.win[(self.head + k) % SYNC_SYMBOLS];
        }
        let cf = correlate(&w, &self.fs);
        let cs = correlate(&w, &self.ss);
        let m = mag_approx(i64::from(cf[0]), i64::from(cf[1]))
            .max(mag_approx(i64::from(cs[0]), i64::from(cs[1])));
        let pos = usize::try_from(n % SLOT_U).expect("position < 9 296");
        // A += (m − A) >> 3, arithmetic shift (floor)
        let a = i64::from(self.acc[pos]);
        let a = a + ((m - a) >> LEAK_SHIFT);
        let a = u32::try_from(a).expect("accumulator in 0 … 2^17");
        self.acc[pos] = a;
        self.last_corr = (cf, cs, a);
        if pos == 0 {
            self.period = SyncStats::default();
        }
        if a > self.period.peak {
            self.period.peak = a;
            self.period.peak_pos = u16::try_from(pos).expect("< 9 296");
        }
        self.period.sum += u64::from(a);
        if let State::Locked { offset, .. } = self.state
            && pos == usize::from(offset)
        {
            self.head_at(self.base + n, cf, cs, out);
        }
        if pos == MOD_SLOT - 1 {
            self.period_end(out);
        }
    }

    fn period_end(&mut self, out: &mut Vec<SyncEvent>) {
        let p = self.period;
        let counts = p.peak > 0 && u64::from(p.peak) * SLOT_U >= LOCK_RATIO * p.sum;
        self.state = match self.state {
            State::Search { cand, count } => {
                let count = match (counts, p.peak_pos == cand) {
                    (true, true) => count + 1,
                    (true, false) => 1,
                    (false, _) => 0,
                };
                if count >= LOCK_PERIODS {
                    State::Locked {
                        offset: p.peak_pos,
                        misses: 0,
                    }
                } else {
                    State::Search {
                        cand: p.peak_pos,
                        count,
                    }
                }
            }
            State::Locked { offset, misses } => {
                let misses = if counts && p.peak_pos == offset {
                    0
                } else {
                    misses + 1
                };
                if misses >= LOSS_PERIODS {
                    out.push(SyncEvent::Lost);
                    let at = self.base + self.t;
                    self.restart(at);
                    return;
                }
                State::Locked { offset, misses }
            }
        };
        self.stats = SyncStats {
            locked: matches!(self.state, State::Locked { .. }),
            offset: match self.state {
                State::Locked { offset, .. } => offset,
                State::Search { .. } => 0,
            },
            ssync_parity: self.parity.unwrap_or(0),
            stray_markers: self.stats.stray_markers,
            missed_markers: self.stats.missed_markers,
            ..p
        };
    }

    fn head_at(&mut self, at: u64, cf: [i32; 2], cs: [i32; 2], out: &mut Vec<SyncEvent>) {
        let k = self.heads;
        self.heads += 1;
        if k == 0 {
            self.first = Some((at, cf, cs));
            return;
        }
        let diff = |f: [i32; 2], s: [i32; 2]| {
            mag_approx(i64::from(s[0]), i64::from(s[1]))
                - mag_approx(i64::from(f[0]), i64::from(f[1]))
        };
        if k == 1 {
            let Some((at0, cf0, cs0)) = self.first.take() else {
                return;
            };
            // head 0 carries SSync when its S-minus-F margin is the larger
            let p = u8::from(diff(cf0, cs0) < diff(cf, cs));
            self.parity = Some(p);
            self.stats.ssync_parity = p;
            self.word_head(0, at0, cf0, cs0, out);
        }
        self.word_head(k, at, cf, cs, out);
    }

    fn word_head(&mut self, k: u64, at: u64, cf: [i32; 2], cs: [i32; 2], out: &mut Vec<SyncEvent>) {
        let is_ss = self.parity.is_some_and(|p| k % 2 == u64::from(p));
        let (word, corr) = if is_ss {
            self.last_ss = Some(cs);
            (Word::Ssync, cs)
        } else {
            // the sign against the previous SSync head (one-sided)
            let Some(prev) = self.last_ss else {
                // no SSync before it: the head is dropped
                return;
            };
            let s = i64::from(cf[0]) * i64::from(prev[0]) + i64::from(cf[1]) * i64::from(prev[1]);
            if s < 0 {
                (Word::NotFsync, [-cf[0], -cf[1]])
            } else {
                (Word::Fsync, cf)
            }
        };
        // frame numbering
        if let Some(n) = self.number.as_mut() {
            *n = (*n + 1) % u8::try_from(SLOTS).expect("120");
        }
        if word != Word::Ssync {
            let positive = word == Word::Fsync;
            let marked = self.majority.abs() >= MAJORITY_MIN && (self.majority > 0) != positive;
            self.majority =
                (self.majority + if positive { 1 } else { -1 }).clamp(-MAJORITY_MAX, MAJORITY_MAX);
            match (self.number, marked) {
                (None, true) => {
                    self.number = Some(0);
                    self.new_frame(at);
                }
                (Some(0), true) => {
                    self.misses = 0;
                    self.new_frame(at);
                }
                (Some(0), false) => {
                    self.stats.missed_markers += 1;
                    self.misses += 1;
                    if self.misses >= FRAME_MISS_LIMIT {
                        out.push(SyncEvent::Lost);
                        let at2 = self.base + self.t;
                        self.restart(at2);
                        return;
                    }
                    self.new_frame(at);
                }
                (Some(_), true) => self.stats.stray_markers += 1,
                (_, false) => {}
            }
        }
        // |corr| ≤ 24 · 2 · 2¹² < 2²²: the CORDIC's input range
        let (theta, amp) = cordic(i64::from(corr[0]), i64::from(corr[1]));
        out.push(SyncEvent::Head(Head {
            at,
            word,
            corr,
            theta,
            amp,
            number: self.number,
            frame: self.number.and(self.frame),
            nco_phase: 0,
        }));
    }

    /// A frame head at `at`: the frame counter advances by the whole
    /// frames since the last one (at least one).
    fn new_frame(&mut self, at: u64) {
        let fsym = SLOT_U * SLOTS as u64;
        let f = match self.last_frame_at {
            None => 0,
            Some((last, f)) => {
                let d = (at - last + fsym / 2) / fsym;
                f.wrapping_add(u32::try_from(d.max(1)).unwrap_or(u32::MAX))
            }
        };
        self.last_frame_at = Some((at, f));
        self.frame = Some(f);
    }
}

/// Units of the carrier register: 2⁻³² turn per symbol.
pub const NCO_BITS: u32 = 32;
/// The carrier register saturates at ±(2¹⁹ − 1) (±4.1 kHz).
pub const CARRIER_MAX: i32 = (1 << 19) - 1;
/// SSync phase steps per carrier update (one frame's 60).
pub const CARRIER_STEPS: u32 = 60;
/// `round(½ · 2³⁹ / (60 · 18 592))`: the carrier loop's gain ½ over the
/// mean of 60 steps of 18 592 symbols, Q39.
pub const CARRIER_GAIN: i64 = 246_412;
/// The shift of [`CARRIER_GAIN`].
pub const CARRIER_SHIFT: u32 = 39;
/// NCO rotation: `round_shift(x · c ± y · s, ROT_SHIFT)` (the 6-bit input
/// becomes 12-bit r, ×16).
pub const ROT_SHIFT: u32 = 11;

/// Stage 2: the NCO that removes the residual carrier, and the loop
/// that sets its frequency from the SSync phase steps.
#[derive(Debug, Clone)]
pub struct Nco {
    cos: Vec<i16>,
    /// Phase accumulator (binary angle).
    pub phase: u32,
    /// Carrier register: the frequency removed, 2⁻³² turn per symbol.
    pub freq: i32,
    last_ss: Option<u32>,
    sum: i64,
    steps: u32,
}

impl Default for Nco {
    fn default() -> Self {
        Self::new(0)
    }
}

impl Nco {
    /// An NCO at frequency `freq` (2⁻³² turn per symbol), phase 0.
    pub fn new(freq: i32) -> Self {
        Self {
            cos: isdb_frontend::tables::cos_table(),
            phase: 0,
            freq,
            last_ss: None,
            sum: 0,
            steps: 0,
        }
    }

    /// The register in Hz at symbol rate `rs`.
    pub fn hz(&self, rs: f64) -> f64 {
        f64::from(self.freq) * rs / 2f64.powi(32)
    }

    /// Rotate one input symbol by `e^{−jφ}` and advance φ by the
    /// register.
    pub fn rotate(&mut self, x: [i8; 2]) -> [i16; 2] {
        let (c, s) = cos_sin_round(&self.cos, self.phase);
        let (xi, xq) = (i64::from(x[0]), i64::from(x[1]));
        let re = round_shift(xi * c + xq * s, ROT_SHIFT);
        let im = round_shift(xq * c - xi * s, ROT_SHIFT);
        self.phase = self.phase.wrapping_add(self.freq.cast_unsigned());
        // |x| ≤ 128 → |re|, |im| ≤ 2 · 128 · 32 767 / 2¹¹ < 2¹³
        [narrow(re), narrow(im)]
    }

    /// A head from stage 1: SSync heads feed the loop; returns `true`
    /// when the register was updated.
    pub fn head(&mut self, h: &Head) -> bool {
        if h.word != Word::Ssync {
            return false;
        }
        let prev = self.last_ss.replace(h.theta);
        let Some(p) = prev else {
            return false;
        };
        self.sum += i64::from(h.theta.wrapping_sub(p).cast_signed());
        self.steps += 1;
        if self.steps < CARRIER_STEPS {
            return false;
        }
        let d = round_shift(self.sum * CARRIER_GAIN, CARRIER_SHIFT);
        let f = (i64::from(self.freq) + d).clamp(-i64::from(CARRIER_MAX), i64::from(CARRIER_MAX));
        self.freq = i32::try_from(f).expect("clamped to ±2^19");
        self.sum = 0;
        self.steps = 0;
        true
    }

    /// A chain restart: the step history is dropped, the register and
    /// the phase kept (the carrier is the signal's).
    pub fn restart(&mut self) {
        self.last_ss = None;
        self.sum = 0;
        self.steps = 0;
    }
}

fn narrow(v: i64) -> i16 {
    i16::try_from(v).expect("NCO output within 13 bits")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn word_symbols_are_the_float_words_times_root_two() {
        for w in [FSYNC, SSYNC] {
            let f = sync_symbols(w);
            let i = word_symbols(w);
            for (a, b) in f.iter().zip(&i) {
                assert!((a.re * 2f64.sqrt() - f64::from(b[0])).abs() < 1e-9);
                assert!((a.im * 2f64.sqrt() - f64::from(b[1])).abs() < 1e-9);
            }
        }
    }

    #[test]
    fn correlation_of_a_word_with_itself() {
        let w = word_symbols(SSYNC);
        let r: Vec<[i16; 2]> = w
            .iter()
            .map(|s| [i16::from(s[0]) * 100, i16::from(s[1]) * 100])
            .collect();
        assert_eq!(correlate(&r, &w), [4800, 0]);
    }
}

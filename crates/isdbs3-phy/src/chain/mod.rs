//! The streaming fixed-point model of the ISDB-S3 receive chain that
//! follows the symbol front end in the FPGA (docs/plans/isdbs3-fpga-rx.md,
//! stages 1–6 and 8). One symbol in per [`Chain::push`]; every value is an
//! integer of a stated width, every rounding "add half, arithmetic shift"
//! ([`fx::round_shift`]); the HDL reproduces these docs bit for bit. The
//! LDPC decoder (stage 7) is a [`LdpcDecoder`] given to the chain.
//!
//! Positions in a slot (Fig. 3.3-1, [`frame::part`]): j = 0 … 23 sync, 24 …
//! 55 pilots, then 66 runs of 136 main and 4 TMCC symbols. The chain's
//! latency from a symbol in to its corrected value is one slot plus 24 +
//! 5 symbols (stage 3 waits for the next head, stage 4's centre tap); a
//! frame's slots leave at the end of the frame (the LDPC needs its whole
//! codewords, the TMCC its 120 slots).
//!
//! # Input
//!
//! The `Symbols` stream's components (`isdb_frontend::format::unpack`:
//! two's-complement `i8`, 6 bits on the PlutoSky, any width ≤ 8 accepted),
//! one complex symbol per push, in stream order; a block loss is
//! [`Chain::loss`] with the number of symbols lost (the sequence-number
//! gap × the block length), which restarts every stage but the carrier
//! register.
//!
//! # Stage 2a, NCO (before stage 1)
//!
//! `r = round_shift(x · e^{−jφ}, 11)` per component (`x` 6-bit, the
//! cosine and sine from the front end's 1 024-entry table at the nearest
//! entry of φ, [`fx::cos_sin_round`]): `r_i = round_shift(x_i·c + x_q·s,
//! 11)`, `r_q = round_shift(x_q·c − x_i·s, 11)`, 13 bits signed (a 6-bit
//! input × 32 767 / 2¹¹ = ×16). Then `φ += f` (both `u32`, wrapping),
//! `f` the carrier register (2⁻³² turn per symbol, signed, saturated to
//! ±(2¹⁹ − 1) ≈ ±4.1 kHz at 33.756 Mbaud; 127.2 units per Hz).
//!
//! # Stage 1, slot and frame synchronisation (B44 §3.3 Fig. 3.3-1, §3.8)
//!
//! At every position n (the 24 NCO outputs r_n … r_{n+23}), the
//! correlations with FSync (52F866h) and SSync (36715Ah) as integer words
//! `w_k = (±1 ± j)` (√2 × the §3.8 π/2-BPSK points; [`sync::word_symbols`]):
//! `C = Σ r_k · conj(w_k)`, i.e. `re = Σ (r_i w_i + r_q w_q)`, `im = Σ
//! (r_q w_i − r_i w_q)`: additions and subtractions only, |C| < 2¹⁹ (19
//! bits signed per component). The statistic `m = max(M(C_F), M(C_S))`
//! with `M(c) = max(|re|, |im|) + (min(|re|, |im|) >> 1)`
//! ([`fx::mag_approx`], 20 bits unsigned) feeds a 9 296-entry
//! accumulator `A[n mod 9 296] += (m − A) >> 3` (arithmetic shift; a leak
//! with a time constant of 8 slots; 20 bits; BRAM 9 296 × 20).
//!
//! Positions are counted from the chain's (re)start; a *period* is 9 296
//! consecutive positions starting at a multiple of 9 296. At the end of
//! each period: `peak` = the largest A of the period and its position
//! (the first on ties), `sum` = ΣA (34 bits). The peak *counts* when
//! `peak · 9 296 ≥ 3 · sum` (at least three times the mean).
//!
//! **Lock rule** (hysteresis): searching, a counting peak at the same
//! position for [`sync::LOCK_PERIODS`] = 4 consecutive periods locks the
//! slot sync at that offset; locked, [`sync::LOSS_PERIODS`] = 8
//! consecutive periods whose peak does not count or lies elsewhere lose
//! it (the chain restarts, [`Chain::loss`] without a gap).
//!
//! **Heads.** Locked at offset o, each position n ≡ o (mod 9 296) is a
//! slot head; its record is the correlation with its parity's word. The
//! **SSync parity** is decided at the second head after lock: head 0
//! carries SSync if `M(C_S) − M(C_F)` of head 0 exceeds that of head 1
//! (else head 1 does); heads alternate from there. An FSync-parity head
//! is read as FSync when `Re(C_F · conj(C_S,prev)) ≥ 0` against the
//! previous SSync head's correlation (one slot earlier; 40-bit product),
//! as !FSync otherwise, and its correlation is then negated (the word's
//! sign removed, as `sync::synced`). An FSync-parity head without an
//! SSync head before it (only possible as head 0) is dropped.
//!
//! **Frame head** (`sync::synced`'s sign-minority rule, streaming): a
//! counter `μ` (saturating at ±8) adds +1 per FSync head and −1 per
//! !FSync head; a head whose sign is opposite to `μ` while |μ| ≥ 2
//! (μ read before the update) is *marked*. The first marked head is slot
//! #1 (number 0) of a frame; numbers then advance by one per head modulo
//! 120. At number 0 a mark confirms the frame; a missing mark is counted
//! (`missed_markers`) and the frame is assumed anyway (flywheel), and
//! two consecutive missing marks lose the sync (restart). A mark at any
//! other number is counted (`stray_markers`) and ignored.
//!
//! **Frame counter**: 0 at the first frame head of the stream; each
//! later frame head adds `max(1, round(Δ / 1 115 520))`, Δ the symbols
//! since the previous frame head (losses included), so a loss shows as a
//! gap in the counter (32 bits, wrapping).
//!
//! **Head record** ([`sync::Head`]): the correlation C (sign removed),
//! its angle θ and magnitude by the 16-iteration CORDIC ([`fx::cordic`],
//! 32-bit binary angle, magnitude ±1 LSB): the slot's carrier phase and
//! 24·√2 × its sync amplitude in r units.
//!
//! # Stage 2b, carrier loop
//!
//! At each SSync head after the first since the (re)start: the phase step
//! `δ = θ_k − θ_{k−2}` (two slots, the same word: no sign ambiguity; the
//! wrapping difference read as a signed 32-bit value, ±908 Hz
//! unambiguous, as `sync::residual_hz`) is added to a 38-bit sum. After
//! [`sync::CARRIER_STEPS`] = 60 steps (one frame) the register is updated
//! `f = clamp(f + round_shift(Σδ · 246 412, 39), ±(2¹⁹ − 1))`, i.e. by
//! half the mean step per symbol (gain ½ per frame; 246 412 =
//! round(½ · 2³⁹ / (60 · 18 592)), 18 bits), and the sum restarts. The
//! new `f` is used from the next symbol the NCO rotates. A restart drops
//! the steps and keeps `f` and φ.
//!
//! # Stage 3, slot phase and amplitude
//!
//! The NCO outputs of a slot are held until the next head (one slot of
//! delay: 9 296 × 2 × 13 bits), then each symbol j = 0 … 9 295 of the slot
//! between heads a and b is corrected (`sync::phase_at` in integers):
//!
//! - phase step `Δ = round_shift(d · 14 784 741, 21)` with `d = θ_b − θ_a`
//!   (signed 32-bit) and 14 784 741 = round(2³⁷ / 9 296): d / 9 296 in
//!   Q.16 of a binary-angle unit; `φ_j = θ_a + round_shift(Δ · j, 16)`
//!   (wrapping);
//! - gain `G = round(4 555 500 750 / max(amp, 64))` per head (one divider
//!   per slot: 4 555 500 750 = round(2²⁷ · 24·√2), so G maps a symbol of
//!   the head's amplitude to 1.0 = 2¹⁰ with 17 fractional bits);
//!   `Γ = round_shift((G_b − G_a) · 14 784 741, 21)`, `G_j = G_a +
//!   round_shift(Γ · j, 16)`;
//! - `u = round_shift(r_i·c + r_q·s, 15)`, `w = round_shift(r_q·c − r_i·s,
//!   15)` with (c, s) from the cosine table at the nearest entry of φ_j
//!   (the multiplication by the conjugate unit phasor);
//! - `y = sat16(round_shift(u · G_j, 17))`, likewise for w: **Q.10**, the
//!   sync symbols at amplitude 1 024 (the main signal's RMS, §3.8).
//!
//! # Stage 4, equaliser
//!
//! An 11-tap complex FIR on y: `z_n = sat16(round_shift(Σ_{k=0}^{10} w_k ·
//! y_{n+5−k}, 14))` (complex products, 40-bit accumulator), taps `w_k`
//! **Q2.14** (`i16`), output aligned with its input (delay 5), identity
//! taps (`w_5 = 16 384`) until the first solved taps are loaded.
//!
//! **Training sums** over the frame's 6 720 known symbols (the 24 sync and
//! 32 pilot symbols of each modulation slot, §3.3, §3.9), at the FIR
//! input: x_k = y_{n+5−k}, reference d_n:
//! `R[r][c] += conj(x_r) · x_c` for the 66 entries r ≤ c and
//! `p[r] += conj(x_r) · d` (11), complex integer sums of 48 bits (|x| ≤
//! 2¹⁵, 6 720 terms: |R| < 2⁴⁴). References in Q.10: sync symbol k of a
//! slot `±w_k · 724` (724 = round(1 024 / √2), − on !FSync slots); pilot t
//! the nominal §3.8 point of label t mod 2^bits of the slot's modulation
//! (the layout in force) in Q.10 (`round(1 024 · p)`), times the pilot
//! PRBS sign (§3.6 (3)). A frame without a layout in force, or the line
//! not yet holding 11 inputs since the restart, adds nothing.
//!
//! **The host's slow loop** ([`eq::solve_sums`]): at the end of frame N the
//! sums are latched; the host fills the lower triangle as conjugates,
//! solves in `f64` with `eq::solve` (the float receiver's elimination)
//! and writes the taps rounded to Q2.14 (saturated); they are in use from
//! the first output of frame N + [`EQ_APPLY_FRAMES`] (= 2: one frame to
//! read 66 + 11 complex sums and write 11 taps). The model solves the
//! first frame with a layout and then every [`ChainOptions::eq_every`]
//! frames. The host may also write taps at open
//! ([`ChainOptions::initial_taps`], e.g. the last ones of a previous run),
//! in use from the first symbol.
//!
//! # Stage 5, TMCC (§3.11, §3.5, §3.6 (2), §3.4 (1))
//!
//! **Soft bits**: TMCC symbol k = 0 … 263 of slot i (positions
//! [`crate::tmcc::tmcc_offsets`]) is TMCC signal bit 264·i + k; at slot
//! position j (π/2-BPSK, §3.8: the slot's even positions are the frame's
//! odd symbols, bit 0 at +45°; odd positions turned 90°, bit 0 at +135°)
//! its soft value is `sat127(round_shift(z_i + z_q, 5))` for even j and
//! `sat127(round_shift(z_q − z_i, 5))` for odd j (z the equaliser output,
//! Q.10; positive reads 0). A symbol on its point at unit amplitude gives
//! 1 448 / 32 = 45: **the TMCC soft-bit scale is 45 per unit
//! amplitude**, chosen so a symbol at three times the noise of the lab
//! signal's sync Es/N0 (13.5 dB: σ ≈ 0.15 per component) stays far from
//! saturation (±127 = 2.8 × unit) while the decoder sees ≈ 6 bits of
//! dynamic range; the float receiver's 2·x/σ² LLR (≈ 90 per unit there)
//! saturates the `i8` at input scale 1. On the lab transponders the TMCC
//! words are already valid at iteration 0, so any scale that keeps the
//! signs decodes them; this one also leaves the min-sum room to correct.
//!
//! **Codeword**: the 31 680 soft bits written to the shortened 61/120 word
//! (`crate::tmcc::ldpc_word`): position b + 1 870 for b < 9 614, b + 13 200
//! for the 22 066 parity bits, the 1 870 + 11 330 known zeros as **+127**
//! ([`tmcc::KNOWN_ZERO`]). At the frame's end the word goes through the
//! LDPC decoder (stage 7, the same decoder and settings as the main
//! signal), the 9 614 data bits through the TMCC dispersal (X¹⁵ + X¹⁴ + 1,
//! state `100000000001110`, last-stage output) and the BCH decoder;
//! `TmccInfo` and the layout (`slots::layout`) follow.
//!
//! **Layout lag** ([`ChainOptions::layout_lag`], default [`LAYOUT_LAG`] =
//! 2): the layout decoded from frame N's TMCC is the one used for frame
//! N + lag. B44 §3.11 says the TMCC carries the switching information two
//! frames before the switch takes effect, so the TMCC of frame N
//! describes frame N + 2 in that reading, and the FPGA has a whole frame
//! (33 ms) to decode it; with lag 1 it would have to finish the TMCC LDPC
//! and BCH within the 56 symbols between frame N's last TMCC symbol and
//! frame N + 1's first main symbol (or delay the stream). The first `lag`
//! frames after a (re)start have no layout and are not decoded (their
//! TMCC is; [`Output::Frame`] with no slots, counted in the status). The
//! float receiver uses each frame's own TMCC, identical on a constant
//! allocation (the lab's); crate SPEC-NOTES "tmcc-timing".
//!
//! # Stage 6, demapper (§3.8, §3.7, §3.9)
//!
//! **Metric**: per point p (Q.10) of the slot's constellation, `m(p) =
//! |p|² − 2(y_i·p_i + y_q·p_q)` (= |y − p|² − |y|², Q.20, 30 bits
//! signed; two multiplies per point). For each label bit b (the label's
//! MSB first): `Δ_b = min_{bit b = 1} m − min_{bit b = 0} m` and the LLR
//! `sat127(round_shift(Δ_b · K, 32))` (positive reads 0). This is
//! `main_signal::demap`'s (m₁ − m₀)/n0 and `fixed::quantise` at scale 1,
//! with n0 in Q.20 and `K = round(2³² / max(n0, 256))` (24 bits): the
//! float receiver's LLR scale exactly, rounded once.
//!
//! **De-interleaving** (§3.7, Table 3.7-2): main symbol s of a valid
//! slot's codeword (counted from the codeword's first symbol in the
//! frame's main-symbol stream, `slots::Slot::symbol_start`) gives LLR b
//! of its label to codeword position `perm[s·bits + b]`
//! (`interleave::permutation`: `col·rows + s` with col = b forward, or
//! bits − 1 − b backward; identity for π/2-BPSK and QPSK). Dummy slots
//! take no symbols (§3.2).
//!
//! **Constellation** in force for frame N: the points learned on frame N
//! − 1 (or the last frame learned on), per modulation; nominal (§3.8,
//! `round(1 024 · p)`) for a modulation not seen yet. Learning (§3.9):
//! pilot t of slot i (layout in force) has label t mod 2^bits; with its
//! PRBS sign s removed (`z·s`, saturating) it is added to the label's sum
//! (32 accumulators per modulation, 32 bits signed per component, and a
//! count); at the frame's end `round(Σ / count)` per label
//! ([`fx::div_round`], 32 divisions) for every modulation whose labels
//! all had pilots.
//!
//! **Noise** in force for frame N: n0 of frame N − 1, `round(Σ (|y|² +
//! min_p m(p)) / count)` over every 16th main symbol of each valid slot's
//! codeword (s ≡ 0 mod 16, `rx::noise_of`'s sampling: the mean of the
//! squared distance to the nearest point, Q.20, 48-bit sum), with its
//! one reciprocal K per frame. Before the first estimate n0 = 52 556 (Es/N0
//! 13 dB).
//!
//! # Stage 7, LDPC (§3.4 (2))
//!
//! A [`LdpcDecoder`] (default [`FixedLdpc`]: `isdb_fec::ldpc::fixed`, 50
//! iterations, offset 1, as `rx::receive`). Not modelled here; the HDL's
//! schedule is its own contract (plan "The LDPC schedule contract").
//!
//! # Stage 8, dispersal and BCH (§3.6 (1), §3.4 (1))
//!
//! As `main_signal::decode_frames`: the slot PRBS (X²⁵ + X²² + 1) over the
//! frame's valid slots in slot order (header, data, BCH parity and
//! stuffing; also over a slot whose LDPC failed), then BCH on header +
//! data + parity of the converged slots (`isdb_fec::bch::decode`).
//!
//! # Frame output and status
//!
//! [`Output::Frame`] at the end of each whole frame: its counter, TMCC
//! result, every slot (dummies flagged) with its LDPC iterations,
//! convergence, BCH result, header and data, and the status: the sync
//! statistics (`peak`, its position, `sum`, lock, offset, parity, the
//! marker counters), the carrier register, the input AGC (the `Symbols`
//! header's), the taps in use and the frame they were solved from, the
//! frame of the training sums latched, n0 and K in force, the learned
//! constellation in force, and the counters (frames, undecoded frames,
//! TMCC failures, slots, LDPC and BCH failures, iterations). These are the
//! `Slots` format's blocks (`isdb_frontend::slots`).

pub mod demap;
pub mod eq;
pub mod frame;
pub mod fx;
pub mod input;
pub mod ldpc;
pub mod out;
pub mod phase;
pub mod probe;
pub mod sync;
pub mod synth;
pub mod tmcc;

use frame::{Back, BackEvent, FrameOut};
use ldpc::{FixedLdpc, LdpcDecoder};
use phase::{Corrected, SlotPhase};
use sync::{Head, Nco, SlotSync, SyncEvent, SyncStats};

/// Frames between the frame whose sums are solved and the first frame
/// that uses the taps.
pub const EQ_APPLY_FRAMES: u32 = 2;

/// Frames between the frame whose TMCC gives a layout and the frame it is
/// used for, by default ([`ChainOptions::layout_lag`]).
pub const LAYOUT_LAG: u32 = 2;

/// The chain's options (fixed registers of the HDL).
#[derive(Debug, Clone)]
pub struct ChainOptions {
    /// The carrier register's initial value (2⁻³² turn per symbol).
    pub carrier: i32,
    /// Solve the equaliser every this many frames (0: never, identity
    /// taps).
    pub eq_every: u32,
    /// Taps written by the host at open (Q2.14; `None`: identity), in
    /// use from the first symbol, reported as solved from no frame.
    pub initial_taps: Option<[[i16; 2]; crate::eq::TAPS]>,
    /// The layout decoded from frame N's TMCC is used for frame N + lag
    /// (1 or 2; [`LAYOUT_LAG`]).
    pub layout_lag: u32,
    /// Also hand out every slot head and corrected symbol
    /// ([`Output::Head`], [`Output::Symbol`]) and each frame's stage
    /// boundaries ([`frame::FrameTrace`]): for the vectors and the stage
    /// checks.
    pub trace: bool,
}

impl Default for ChainOptions {
    fn default() -> Self {
        Self {
            carrier: 0,
            eq_every: 30,
            initial_taps: None,
            layout_lag: LAYOUT_LAG,
            trace: false,
        }
    }
}

/// What the chain hands out.
#[derive(Debug, Clone)]
pub enum Output {
    /// A slot head (stage 1; with [`ChainOptions::trace`]).
    Head(Head),
    /// The carrier register changed (stage 2).
    Carrier(i32),
    /// A corrected symbol (stage 3; with [`ChainOptions::trace`]).
    Symbol(Corrected),
    /// A whole frame: TMCC, slots, status.
    Frame(Box<FrameOut>),
    /// A frame cut short by a restart: not decoded.
    Dropped {
        /// Its counter.
        frame: u32,
    },
    /// The chain restarted (a loss or a lost sync).
    Restart {
        /// Whether the restart came from the sync (else an input loss).
        sync_lost: bool,
    },
}

/// The chain.
#[derive(Debug)]
pub struct Chain {
    opt: ChainOptions,
    nco: Nco,
    sync: SlotSync,
    phase: SlotPhase,
    back: Back,
    /// The NCO outputs of the last 24 symbols and the NCO phase they
    /// were rotated with: stage 3 lags stage 1 by the correlation window.
    delay: std::collections::VecDeque<([i16; 2], u32)>,
    /// Chain input symbols so far (losses included).
    t: u64,
    /// The input's AGC gain (the `Symbols` header's, for the status).
    agc: u16,
    events: Vec<SyncEvent>,
    corrected: Vec<Corrected>,
    back_events: Vec<BackEvent>,
}

impl Chain {
    /// A chain at the start of a stream, with the CPU's fixed-point LDPC
    /// decoder ([`FixedLdpc::new`]).
    pub fn new(opt: ChainOptions) -> Self {
        Self::with_ldpc(opt, Box::new(FixedLdpc::new()))
    }

    /// A chain with the LDPC decoder `ldpc`.
    pub fn with_ldpc(opt: ChainOptions, ldpc: Box<dyn LdpcDecoder + Send>) -> Self {
        Self {
            nco: Nco::new(opt.carrier),
            sync: SlotSync::new(),
            phase: SlotPhase::new(),
            back: {
                let mut b = Back::new(opt.layout_lag, opt.eq_every, ldpc);
                b.set_trace(opt.trace);
                if let Some(t) = opt.initial_taps {
                    b.set_taps(t);
                }
                b
            },
            delay: std::collections::VecDeque::with_capacity(isdbs3_core::params::SYNC_SYMBOLS + 1),
            t: 0,
            agc: 0,
            events: Vec::new(),
            corrected: Vec::new(),
            back_events: Vec::new(),
            opt,
        }
    }

    /// The options.
    pub fn options(&self) -> &ChainOptions {
        &self.opt
    }

    /// The carrier register.
    pub fn carrier(&self) -> i32 {
        self.nco.freq
    }

    /// The sync statistics of the last whole period.
    pub fn sync_stats(&self) -> SyncStats {
        self.sync.stats()
    }

    /// The stages after the slot phase (equaliser, constellation, noise,
    /// counters).
    pub fn back(&self) -> &Back {
        &self.back
    }

    /// Symbols pushed or lost so far.
    pub fn symbols(&self) -> u64 {
        self.t
    }

    /// The input's AGC gain (Q6.10) from now on, for the status.
    pub fn set_agc(&mut self, gain: u16) {
        self.agc = gain;
    }

    /// `lost` symbols are missing before the next push: every stage
    /// restarts (the carrier register stays).
    pub fn loss(&mut self, lost: u64, out: &mut Vec<Output>) {
        self.t += lost;
        self.sync.restart(self.t);
        self.restart_after_sync(false, out);
    }

    fn restart_after_sync(&mut self, sync_lost: bool, out: &mut Vec<Output>) {
        self.phase.restart();
        self.nco.restart();
        self.delay.clear();
        self.back.restart(&mut self.back_events);
        self.drain_back(out);
        out.push(Output::Restart { sync_lost });
    }

    fn drain_back(&mut self, out: &mut Vec<Output>) {
        for e in self.back_events.drain(..) {
            out.push(match e {
                BackEvent::Frame(f) => Output::Frame(f),
                BackEvent::Dropped { frame } => Output::Dropped { frame },
            });
        }
    }

    /// Push one input symbol; appends what it completes to `out`.
    pub fn push(&mut self, x: [i8; 2], out: &mut Vec<Output>) {
        let ph = self.nco.phase;
        let r = self.nco.rotate(x);
        self.t += 1;
        self.delay.push_back((r, ph));
        if self.delay.len() > isdbs3_core::params::SYNC_SYMBOLS
            && let Some((d, _)) = self.delay.pop_front()
        {
            self.phase.push(d);
        }
        self.sync.push(r, &mut self.events);
        if self.events.is_empty() {
            return;
        }
        for e in std::mem::take(&mut self.events) {
            match e {
                SyncEvent::Head(mut h) => {
                    // the delay line's oldest entry is the head's symbol
                    h.nco_phase = self.delay.front().map_or(0, |d| d.1);
                    if self.opt.trace {
                        out.push(Output::Head(h));
                    }
                    if self.nco.head(&h) {
                        out.push(Output::Carrier(self.nco.freq));
                    }
                    self.phase.head(h, &mut self.corrected);
                    let (stats, carrier, agc) = (self.sync.stats(), self.nco.freq, self.agc);
                    let status = move || (stats, carrier, agc);
                    for c in self.corrected.drain(..) {
                        if self.opt.trace {
                            out.push(Output::Symbol(c));
                        }
                        self.back.push(c, &status, &mut self.back_events);
                    }
                    self.drain_back(out);
                }
                SyncEvent::Lost => {
                    // the detector has restarted itself at the right index
                    self.restart_after_sync(true, out);
                }
                SyncEvent::Period(_) => {}
            }
        }
    }
}

#[cfg(test)]
mod tests;

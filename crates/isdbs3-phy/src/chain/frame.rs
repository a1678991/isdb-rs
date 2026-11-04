//! Stages 4–6 and 8 at the frame level: the equaliser with its training
//! sums and slow loop, the routing of each equalised symbol (sync,
//! pilot, main, TMCC) by its position in the slot, the demapper into the
//! codeword buffers, the TMCC codeword and the layout queue, and at each
//! frame's end the LDPC (stage 7, the [`LdpcDecoder`]), dispersal and
//! BCH (module docs of [`crate::chain`]).

use super::demap::{InForce, Learn, NOISE_STEP, demap, nominal};
use super::eq::{Equaliser, Sums, solve_sums};
use super::ldpc::LdpcDecoder;
use super::phase::Corrected;
use super::sync::{SyncStats, Word, word_symbols};
use super::tmcc::{TmccResult, soft};
use crate::eq::TAPS;
use isdbs3_core::dispersal::pilot_signs;
use isdbs3_core::interleave::permutation;
use isdbs3_core::params::{
    CodeRate, FSYNC, MAIN_PER_MOD_SLOT, MAIN_RUN, MOD_SLOT, Modulation, N_LDPC, PILOT_SYMBOLS,
    SLOTS, SSYNC, SYNC_SYMBOLS, TMCC_PER_MOD_SLOT, TMCC_RUN, TMCC_SIGNAL_BITS,
};
use isdbs3_core::slots::Slot;
use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;

/// `round(1 024 / √2)`: a sync symbol's components in Q.10.
pub const SYNC_REF: i16 = 724;

/// Where a symbol sits in its modulation slot (Fig. 3.3-1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Part {
    /// Sync symbol k (0 … 23).
    Sync(usize),
    /// Pilot symbol t (0 … 31).
    Pilot(usize),
    /// Main symbol m of the slot (0 … 8 975).
    Main(usize),
    /// TMCC symbol k of the slot (0 … 263).
    Tmcc(usize),
}

/// The part of slot position `j` (0 … 9 295).
pub fn part(j: usize) -> Part {
    if j < SYNC_SYMBOLS {
        return Part::Sync(j);
    }
    if j < SYNC_SYMBOLS + PILOT_SYMBOLS {
        return Part::Pilot(j - SYNC_SYMBOLS);
    }
    let x = j - SYNC_SYMBOLS - PILOT_SYMBOLS;
    let (r, p) = (x / (MAIN_RUN + TMCC_RUN), x % (MAIN_RUN + TMCC_RUN));
    if p < MAIN_RUN {
        Part::Main(r * MAIN_RUN + p)
    } else {
        Part::Tmcc(r * TMCC_RUN + p - MAIN_RUN)
    }
}

/// One slot of a decoded frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotOut {
    /// Slot index 0 … 119.
    pub slot: u8,
    /// Its transmission mode (0 … 7).
    pub mode: u8,
    /// Modulation and rate of the mode.
    pub modulation: Modulation,
    /// See `modulation`.
    pub rate: CodeRate,
    /// A dummy slot (no codeword; the fields below are empty / `None`).
    pub dummy: bool,
    /// LDPC iterations (the limit when not converged).
    pub ldpc_iterations: usize,
    /// Whether the LDPC converged.
    pub converged: bool,
    /// BCH bits corrected, `None` if uncorrectable or not converged.
    pub bch_corrected: Option<usize>,
    /// The 176 header bits (0/1), after dispersal and BCH.
    pub header: Vec<u8>,
    /// The data bits (Table 3.2-1 D of the rate) packed MSB first.
    pub data: Vec<u8>,
}

/// The per-frame status (the `Slots` status block's content).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    /// Frame counter.
    pub frame: u32,
    /// Sync statistics at the frame's end.
    pub sync: SyncStats,
    /// Carrier register (2⁻³² turn per symbol).
    pub carrier: i32,
    /// AGC gain of the input (Q6.10, the `Symbols` header's).
    pub agc: u16,
    /// The equaliser taps in use (Q2.14) and the frame they were solved
    /// from.
    pub taps: [[i16; 2]; TAPS],
    /// See `taps`.
    pub taps_from: Option<u32>,
    /// Frame counter of the training sums latched at this frame's end
    /// (`None`: none).
    pub sums_frame: Option<u32>,
    /// The noise n0 (Q.20) and LLR scale in force for this frame, and
    /// the frame they were learned on.
    pub n0: i64,
    /// See `n0`.
    pub llr_scale: i64,
    /// See `n0`.
    pub learned_from: Option<u32>,
    /// The constellation in force per modulation (Q.10, label order;
    /// empty: nominal).
    pub constellation: [Vec<[i16; 2]>; 5],
    /// Cumulative counters since the chain's start.
    pub counters: Counters,
}

/// Cumulative counters.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counters {
    /// Whole frames seen.
    pub frames: u32,
    /// Frames without a layout in force (not decoded).
    pub frames_undecoded: u32,
    /// TMCC codewords that failed (LDPC or BCH).
    pub tmcc_failed: u32,
    /// Valid slots decoded.
    pub slots: u32,
    /// Of which LDPC did not converge.
    pub ldpc_failed: u32,
    /// Of which LDPC converged and BCH failed.
    pub bch_failed: u32,
    /// LDPC iterations of the converged slots.
    pub iterations: u64,
}

/// A whole frame.
#[derive(Debug, Clone)]
pub struct FrameOut {
    /// Frame counter.
    pub frame: u32,
    /// Chain index of its first symbol (slot #1's sync).
    pub at: u64,
    /// Its TMCC.
    pub tmcc: TmccResult,
    /// The frame whose TMCC gave the layout used (`None`: not decoded).
    pub layout_from: Option<u32>,
    /// Every slot (valid and dummy) when decoded, in slot order.
    pub slots: Vec<SlotOut>,
    /// The frame's status.
    pub status: Status,
    /// The training sums latched at its end.
    pub sums: Option<Sums>,
    /// The stage boundaries' values, with [`super::ChainOptions::trace`].
    pub trace: Option<Box<FrameTrace>>,
}

/// One frame's values at the stage boundaries (for the vectors and the
/// stage checks).
#[derive(Debug, Clone, Default)]
pub struct FrameTrace {
    /// The equaliser's output for every symbol of the frame (Q.10), in
    /// order.
    pub z: Vec<[i16; 2]>,
    /// The 31 680 TMCC soft bits.
    pub tmcc_soft: Vec<i8>,
    /// Per valid slot: its index and its 44 880 codeword LLRs
    /// (de-interleaved), as the decoder got them.
    pub llrs: Vec<(usize, Vec<i8>)>,
    /// The points each slot was demapped against (Q.10; empty for
    /// dummies and frames without a layout).
    pub points: Vec<Vec<[i16; 2]>>,
}

/// A valid slot's codeword being filled: its slot index, LLRs and
/// interleaver permutation.
type Codeword = (usize, Vec<i8>, Arc<[u32]>);

/// The current frame's buffers.
struct Cur {
    frame: u32,
    at: u64,
    /// Symbols seen (a whole frame has 1 115 520).
    seen: usize,
    layout: Option<(u32, Vec<Slot>)>,
    tmcc: Vec<i8>,
    /// Per valid slot: its slot index, codeword LLRs and interleaver
    /// permutation.
    words: Vec<Codeword>,
    /// Main-symbol cursor: index into `words` of the codeword at the
    /// current main index.
    cursor: usize,
    learn: Learn,
    /// Per slot: the points demapped against and the nominal points (the
    /// pilots' references).
    pts: Vec<Vec<[i16; 2]>>,
    nominal: Vec<Vec<[i16; 2]>>,
    /// The equaliser outputs, with tracing.
    z: Option<Vec<[i16; 2]>>,
}

/// Stages 4–6, 8 and the frame bookkeeping.
pub struct Back {
    eq: Equaliser,
    metas: VecDeque<Corrected>,
    last_at: Option<u64>,
    pending_taps: Option<([[i16; 2]; TAPS], u32, u32)>,
    last_solve: Option<u32>,
    eq_every: u32,
    lag: u32,
    /// Layouts by the frame they are in force for: (from frame, layout).
    layouts: BTreeMap<u32, (u32, Vec<Slot>)>,
    in_force: InForce,
    cur: Option<Cur>,
    signs: Vec<i8>,
    fs: [[i8; 2]; SYNC_SYMBOLS],
    ss: [[i8; 2]; SYNC_SYMBOLS],
    perms: BTreeMap<(Modulation, CodeRate), Arc<[u32]>>,
    ldpc: Box<dyn LdpcDecoder + Send>,
    counters: Counters,
    trace: bool,
}

impl std::fmt::Debug for Back {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Back")
            .field("lag", &self.lag)
            .finish_non_exhaustive()
    }
}

/// What the back end reports per frame or restart.
#[derive(Debug, Clone)]
pub enum BackEvent {
    /// A whole frame.
    Frame(Box<FrameOut>),
    /// A frame cut short (a restart or a gap): dropped.
    Dropped {
        /// Its counter.
        frame: u32,
    },
}

impl Back {
    /// A back end with the layout lag `lag` (frames), the equaliser
    /// solved every `eq_every` frames (0: never) and the decoder `ldpc`.
    pub fn new(lag: u32, eq_every: u32, ldpc: Box<dyn LdpcDecoder + Send>) -> Self {
        Self {
            eq: Equaliser::new(),
            metas: VecDeque::with_capacity(TAPS + 1),
            last_at: None,
            pending_taps: None,
            last_solve: None,
            eq_every,
            lag,
            layouts: BTreeMap::new(),
            in_force: InForce::default(),
            cur: None,
            signs: pilot_signs(),
            fs: word_symbols(FSYNC),
            ss: word_symbols(SSYNC),
            perms: BTreeMap::new(),
            ldpc,
            counters: Counters::default(),
            trace: false,
        }
    }

    /// Put taps written by the host in use now (not solved from a frame:
    /// `taps_from` stays `None`).
    pub fn set_taps(&mut self, taps: [[i16; 2]; TAPS]) {
        self.eq.taps = taps;
    }

    /// The counter of the frame being received (`None` before the first
    /// frame head or after a restart).
    pub fn open_frame(&self) -> Option<u32> {
        self.cur.as_ref().map(|c| c.frame)
    }

    /// What the HDL's CTRL START with INIT bit 0 does to the taps
    /// (`SlotsRx`, fpga/isdb_fpga/chain/top.py; `eq.py` `tap_now`): `taps`
    /// in use from the next symbol, reported as solved from no frame
    /// (`taps_from` `None`), and taps waiting for their frame dropped.
    pub fn load_taps(&mut self, taps: [[i16; 2]; TAPS]) {
        self.eq.taps = taps;
        self.eq.taps_from = None;
        self.pending_taps = None;
    }

    /// Record the stage boundaries' values of every frame
    /// ([`FrameOut::trace`]).
    pub fn set_trace(&mut self, on: bool) {
        self.trace = on;
    }

    /// The equaliser (taps in use).
    pub fn equaliser(&self) -> &Equaliser {
        &self.eq
    }

    /// The constellation and noise in force.
    pub fn in_force(&self) -> &InForce {
        &self.in_force
    }

    /// The counters.
    pub fn counters(&self) -> Counters {
        self.counters
    }

    /// A restart: the line, the open frame and the queued layouts are
    /// dropped; the taps (and taps waiting to be loaded), the learned
    /// constellation and n0 stay.
    pub fn restart(&mut self, out: &mut Vec<BackEvent>) {
        self.eq.restart();
        self.metas.clear();
        self.last_at = None;
        self.layouts.clear();
        if let Some(c) = self.cur.take() {
            out.push(BackEvent::Dropped { frame: c.frame });
        }
    }

    /// One stage-3 output; `status` gives the chain's sync statistics,
    /// carrier and AGC for a frame that ends here.
    pub fn push(
        &mut self,
        c: Corrected,
        status: &dyn Fn() -> (SyncStats, i32, u16),
        out: &mut Vec<BackEvent>,
    ) {
        if self.last_at.is_some_and(|a| a + 1 != c.at) {
            // a gap between slots: the line restarts (a frame in
            // progress is dropped)
            self.restart(out);
        }
        self.last_at = Some(c.at);
        self.metas.push_back(c);
        if self.metas.len() <= TAPS / 2 {
            self.eq.push(c.y);
            return;
        }
        let Some(m) = self.metas.pop_front() else {
            return;
        };
        if let Some((taps, from, at)) = self.pending_taps
            && m.frame.is_some_and(|f| f >= at)
        {
            self.eq.load(taps, from);
            self.pending_taps = None;
        }
        let z = self.eq.push(c.y).unwrap_or(c.y);
        self.symbol(&m, z, status, out);
    }

    fn symbol(
        &mut self,
        m: &Corrected,
        z: [i16; 2],
        status: &dyn Fn() -> (SyncStats, i32, u16),
        out: &mut Vec<BackEvent>,
    ) {
        let (Some(number), Some(frame)) = (m.number, m.frame) else {
            return;
        };
        let (k, j) = (usize::from(number), usize::from(m.j));
        if k == 0 && j == 0 {
            if let Some(c) = self.cur.take() {
                out.push(BackEvent::Dropped { frame: c.frame });
            }
            self.start(frame, m.at);
        }
        let Some(cur) = self.cur.as_mut() else {
            return;
        };
        if cur.frame != frame {
            return;
        }
        cur.seen += 1;
        if let Some(v) = cur.z.as_mut() {
            v.push(z);
        }
        let lay = cur.layout.as_ref().map(|l| &l.1);
        match part(j) {
            Part::Sync(t) => {
                if lay.is_some() {
                    let w = if m.word == Word::Ssync {
                        &self.ss
                    } else {
                        &self.fs
                    };
                    let s: i16 = if m.word == Word::NotFsync { -1 } else { 1 };
                    let d = [
                        i16::from(w[t][0]) * SYNC_REF * s,
                        i16::from(w[t][1]) * SYNC_REF * s,
                    ];
                    self.eq.train(d);
                }
            }
            Part::Pilot(t) => {
                if let Some(lay) = lay {
                    let s = &lay[k];
                    let n = 1usize << s.modulation.bits();
                    let p = cur.nominal[k][t % n];
                    let sg = i16::from(self.signs[k * PILOT_SYMBOLS + t]);
                    // |p| < 2 048: the products fit; z saturates (−32 768)
                    self.eq.train([p[0] * sg, p[1] * sg]);
                    cur.learn.pilot(
                        s.modulation,
                        t,
                        [z[0].saturating_mul(sg), z[1].saturating_mul(sg)],
                    );
                }
            }
            Part::Tmcc(t) => {
                cur.tmcc[k * TMCC_PER_MOD_SLOT + t] = soft(z, m.j);
            }
            Part::Main(mi) => {
                if let Some(lay) = lay {
                    let mm = k * MAIN_PER_MOD_SLOT + mi;
                    while cur.cursor < cur.words.len() {
                        let s = &lay[cur.words[cur.cursor].0];
                        if mm < s.symbol_start + s.symbols {
                            break;
                        }
                        cur.cursor += 1;
                    }
                    if let Some((si, w, perm)) = cur.words.get_mut(cur.cursor) {
                        let s = &lay[*si];
                        let sym = mm - s.symbol_start;
                        let bits = s.modulation.bits();
                        let pts = &cur.pts[*si];
                        let mut llr = [0i8; 5];
                        let best = demap(z, pts, bits, self.in_force.k, &mut llr);
                        for (b, &l) in llr[..bits].iter().enumerate() {
                            w[perm[sym * bits + b] as usize] = l;
                        }
                        if sym.is_multiple_of(NOISE_STEP) {
                            cur.learn.noise(z, best);
                        }
                    }
                }
            }
        }
        if k == SLOTS - 1
            && j == MOD_SLOT - 1
            && let Some(c) = self.cur.take()
        {
            let ev = self.finish(c, status);
            out.push(ev);
        }
    }

    fn start(&mut self, frame: u32, at: u64) {
        // the layout in force: the one queued for this frame
        self.layouts.retain(|&f, _| f >= frame);
        let layout = self.layouts.remove(&frame);
        let mut words = Vec::new();
        let mut pts = vec![Vec::new(); SLOTS];
        let mut nom = vec![Vec::new(); SLOTS];
        if let Some((_, lay)) = layout.as_ref() {
            for s in lay {
                nom[s.index] = nominal(s.modulation, s.rate);
                if s.dummy {
                    continue;
                }
                let perm = self
                    .perms
                    .entry((s.modulation, s.rate))
                    .or_insert_with(|| permutation(s.modulation, s.rate).into());
                words.push((s.index, vec![0i8; N_LDPC], Arc::clone(perm)));
                pts[s.index] = self.in_force.points(s);
            }
            self.eq.open(frame);
        }
        self.cur = Some(Cur {
            frame,
            at,
            seen: 0,
            layout,
            tmcc: vec![0; TMCC_SIGNAL_BITS],
            words,
            cursor: 0,
            learn: Learn::new(),
            pts,
            nominal: nom,
            z: self.trace.then(|| Vec::with_capacity(SLOTS * MOD_SLOT)),
        });
    }

    fn finish(&mut self, c: Cur, status: &dyn Fn() -> (SyncStats, i32, u16)) -> BackEvent {
        if c.seen != SLOTS * MOD_SLOT {
            return BackEvent::Dropped { frame: c.frame };
        }
        self.counters.frames += 1;
        // stage 5: TMCC → the layout for frame + lag
        let tmcc = super::tmcc::decode(&c.tmcc, self.ldpc.as_mut());
        if tmcc.bch_corrected.is_none() {
            self.counters.tmcc_failed += 1;
        }
        if let Some(l) = &tmcc.layout {
            self.layouts
                .insert(c.frame.wrapping_add(self.lag), (c.frame, l.clone()));
        }
        let in_force = self.in_force.clone();
        let mut slots = Vec::new();
        let mut sums = None;
        let layout_from = c.layout.as_ref().map(|l| l.0);
        if let Some((_, lay)) = &c.layout {
            slots = self.decode_slots(lay, &c.words);
            // the next frame's constellation and noise
            self.in_force = self.in_force.next(&c.learn, c.frame);
            // the equaliser's slow loop
            sums = self.eq.close();
            if let Some(s) = &sums {
                let due = self.eq_every > 0
                    && self
                        .last_solve
                        .is_none_or(|l| c.frame.wrapping_sub(l) >= self.eq_every);
                if due && let Some(t) = solve_sums(s) {
                    self.pending_taps =
                        Some((t, c.frame, c.frame.wrapping_add(super::EQ_APPLY_FRAMES)));
                    self.last_solve = Some(c.frame);
                }
            }
        } else {
            self.counters.frames_undecoded += 1;
        }
        let (sync, carrier, agc) = status();
        let status = Status {
            frame: c.frame,
            sync,
            carrier,
            agc,
            taps: self.eq.taps,
            taps_from: self.eq.taps_from,
            sums_frame: sums.as_ref().map(|s| s.frame),
            n0: in_force.n0,
            llr_scale: in_force.k,
            learned_from: in_force.from,
            constellation: in_force.learned.clone().map(Option::unwrap_or_default),
            counters: self.counters,
        };
        let trace = c.z.map(|z| {
            Box::new(FrameTrace {
                z,
                tmcc_soft: c.tmcc.clone(),
                llrs: c.words.iter().map(|(s, w, _)| (*s, w.clone())).collect(),
                points: c.pts.clone(),
            })
        });
        BackEvent::Frame(Box::new(FrameOut {
            frame: c.frame,
            at: c.at,
            tmcc,
            layout_from,
            slots,
            status,
            sums,
            trace,
        }))
    }

    /// Stages 7 and 8 for a frame's codewords.
    fn decode_slots(&mut self, lay: &[Slot], words: &[Codeword]) -> Vec<SlotOut> {
        // the codewords of each rate together (the decoder takes one code
        // per call), results back in slot order
        let mut res: BTreeMap<usize, (Option<usize>, usize, Vec<u8>)> = BTreeMap::new();
        let mut rates: Vec<CodeRate> = words.iter().map(|(s, _, _)| lay[*s].rate).collect();
        rates.sort_by_key(|r| r.numerator());
        rates.dedup();
        for r in rates {
            let same: Vec<&Codeword> = words.iter().filter(|(s, _, _)| lay[*s].rate == r).collect();
            let refs: Vec<&[i8]> = same.iter().map(|(_, w, _)| w.as_slice()).collect();
            let out = self.ldpc.decode(isdbs3_core::ldpc::code(r), &refs);
            for ((s, _, _), (r, bits)) in same.iter().zip(out) {
                let (it, n) = match r {
                    Ok(d) => (Some(d.iterations), d.iterations),
                    Err(e) => (None, e.iterations),
                };
                res.insert(*s, (it, n, bits));
            }
        }
        let decoded: Vec<crate::main_signal::Decoded> = res
            .iter()
            .map(|(s, (it, _, b))| (*s, *it, b.clone()))
            .collect();
        let fin = crate::main_signal::finish_frame(decoded, lay);
        let mut fin = fin.into_iter();
        let mut out = Vec::with_capacity(SLOTS);
        for s in lay {
            let mode = u8::try_from(s.mode).expect("mode < 8");
            let slot = u8::try_from(s.index).expect("slot < 120");
            if s.dummy {
                out.push(SlotOut {
                    slot,
                    mode,
                    modulation: s.modulation,
                    rate: s.rate,
                    dummy: true,
                    ldpc_iterations: 0,
                    converged: false,
                    bch_corrected: None,
                    header: Vec::new(),
                    data: Vec::new(),
                });
                continue;
            }
            let d = fin.next().expect("one result per valid slot");
            let n = res[&s.index].1;
            self.counters.slots += 1;
            match (d.ldpc_iterations, d.bch_corrected) {
                (None, _) => self.counters.ldpc_failed += 1,
                (Some(i), None) => {
                    self.counters.bch_failed += 1;
                    self.counters.iterations += i as u64;
                }
                (Some(i), Some(_)) => self.counters.iterations += i as u64,
            }
            out.push(SlotOut {
                slot,
                mode,
                modulation: s.modulation,
                rate: s.rate,
                dummy: false,
                ldpc_iterations: n,
                converged: d.ldpc_iterations.is_some(),
                bch_corrected: d.bch_corrected,
                header: d.header,
                data: d.data,
            });
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parts_of_a_slot() {
        assert_eq!(part(0), Part::Sync(0));
        assert_eq!(part(23), Part::Sync(23));
        assert_eq!(part(24), Part::Pilot(0));
        assert_eq!(part(55), Part::Pilot(31));
        assert_eq!(part(56), Part::Main(0));
        assert_eq!(part(191), Part::Main(135));
        assert_eq!(part(192), Part::Tmcc(0));
        assert_eq!(part(196), Part::Main(136));
        assert_eq!(part(MOD_SLOT - 1), Part::Tmcc(263));
        let tm: Vec<usize> = (0..MOD_SLOT)
            .filter(|&j| matches!(part(j), Part::Tmcc(_)))
            .collect();
        let want: Vec<usize> = crate::tmcc::tmcc_offsets().collect();
        assert_eq!(tm, want);
        let mm: Vec<usize> = (0..MOD_SLOT)
            .filter(|&j| matches!(part(j), Part::Main(_)))
            .collect();
        let want: Vec<usize> = crate::main_signal::main_offsets().collect();
        assert_eq!(mm, want);
    }
}

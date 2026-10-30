//! PCR-paced placement of one transport stream on a slot grid, with PCR
//! restamping (docs/plans/isdbs-next/pcr-pacing.md §7.1–§7.2).
//!
//! A TS carried in a fixed share of a transmission frame (an ISDB-S
//! relative TS: N_r slots per frame) goes out at a constant packet rate:
//! slot `j` of the TS is at a fixed time [`SlotClock::time`]. Each input
//! packet is due at the time its own PCRs give it (H.222.0 §2.4.2.2
//! eq. 2-4, through [`PcrClock`]); [`SlotPacer`] puts it in the first free
//! slot at or after that time (the "late" strategy), never reordering, and
//! fills the slots nobody wants with null packets (H.222.0 §2.4.3.3). Every
//! PCR is then corrected by the time the packet was moved,
//! Δ = round(S(j) − due), as a re-multiplexer does (H.222.0 Annex D.0.4,
//! "correction of PCRs is necessary in a re-multiplex operation", with
//! del_const = 0). Δ is a duration, so it is right for every program's
//! time base: PCRs on any PID are corrected, not only the reference PID's.
//! OPCR (Table 2-6) is left alone, as are all other bytes.
//!
//! The rule, per packet p in input order:
//!
//! 1. input null packets are dropped (and counted);
//! 2. due(p): continuous 27 MHz time from the first PCR of the reference PID
//!    (the one given, else the first PID seen with a PCR), eq. 2-4
//!    interpolation; wrap and discontinuities (§2.4.3.5) are bridged by
//!    [`PcrClock`];
//! 3. j(p) = max(⌈due(p)·den/num⌉, next free slot, first slot not yet
//!    handed out): the first free slot at or after due(p);
//! 4. every PCR field gets Δ = [`SlotClock::time`]`(j) − due(p)` added
//!    modulo 2^33·300 (eq. 2-1..2-3). Steady state: Δ ∈ [0, one slot);
//!    when the input exceeds the capacity, Δ is the lag;
//! 5. a [`SlotPacer::fill`] of n slots returns the placed packets at their
//!    slots and nulls elsewhere; a packet due beyond the n slots waits for
//!    the next call.
//!
//! Slot 0 is due time 0, the first PCR (anchor offset A = 0, §7.4). Packets
//! before the first PCR are due at 0 and take slots 0, 1, …; their PCRs (if
//! any) and the first PCR are late by that count, which the restamp covers.
//!
//! This module knows no transmission system: the ISDB-S producer
//! (`txs.rs`) builds a [`SlotClock::isdbs`] per relative TS. The loop seam
//! (per-pass offsets, PTS/DTS, continuity counters; §7.6) and live inputs
//! (§7.7) come later: the pass tag of [`TaggedSource`] is carried but not
//! used yet, and [`WaitMode`] has only [`WaitMode::Block`].

use std::num::NonZeroU64;
use std::sync::atomic::AtomicBool;

use isdb_ts::Pid;
use isdb_ts::retime::add_to_pcr;
use isdbt_mux::pcrclock::{HZ, PcrClock};

use crate::source::{Packet, PacketStream, is_null, null_packet};

/// Bits of one 188-byte TS packet (H.222.0 §2.4.3.2).
const PACKET_BITS: u128 = 188 * 8;

/// An exact slot clock: slot `j` is at S(j) = j·num/den ticks of the
/// 27 MHz system clock (H.222.0 §2.4.2.1) after the anchor, a rational
/// number kept exact in integer arithmetic (u128); [`Self::time`] rounds it
/// to the nearest tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlotClock {
    /// Ticks per slot = num / den (reduced).
    num: u64,
    den: u64,
}

impl SlotClock {
    /// One slot every `num / den` ticks of 27 MHz; `None` if either is 0.
    /// The fraction is reduced.
    pub fn new(num: u64, den: u64) -> Option<Self> {
        let (n, d) = (NonZeroU64::new(num)?, NonZeroU64::new(den)?);
        let g = gcd(n.get(), d.get());
        Some(Self {
            num: n.get() / g,
            den: d.get() / g,
        })
    }

    /// The slot clock of an ISDB-S TS with `slots_per_frame` sent slots per
    /// frame (`None` for 0): 1 382 400 / (37 · N_r) ticks per packet.
    ///
    /// Symbol rate 28.860 Mbaud (MIC Ordinance Art. 52(3); ITU-R BO.1408-1
    /// Table 3), a superframe of 8 frames × 39 936 symbols = 319 488 symbols,
    /// so a superframe lasts 319 488 · 27e6 / 28.86e6 = 11 059 200 / 37 ticks
    /// (exactly: 481 = 13 · 37), a frame 1 382 400 / 37, and a TS taking N_r
    /// slots of every frame sends one packet every 1 382 400 / (37 · N_r)
    /// ticks: packet k of the TS is at k times that (the uniform per-TS grid
    /// a receiver outputs, pcr-pacing.md §3.3).
    pub fn isdbs(slots_per_frame: usize) -> Option<Self> {
        let n = u64::try_from(slots_per_frame).ok()?;
        Self::new(1_382_400, 37u64.checked_mul(n)?)
    }

    /// S(j) rounded to the nearest tick, ⌊j·num/den + ½⌋ (saturating at
    /// `u64::MAX`, some 21 000 years).
    pub fn time(&self, j: u64) -> u64 {
        let (num, den) = (u128::from(self.num), u128::from(self.den));
        sat((2 * u128::from(j) * num + den) / (2 * den))
    }

    /// The first slot at or after tick `t`: ⌈t·den/num⌉, the smallest j with
    /// S(j) ≥ t (and so [`Self::time`]`(j) ≥ t`).
    pub fn first_at_or_after(&self, t: u64) -> u64 {
        let (num, den) = (u128::from(self.num), u128::from(self.den));
        sat((u128::from(t) * den).div_ceil(num))
    }

    /// The TS rate the slots carry: 188 · 8 bits per slot,
    /// 188·8·27e6·den/num bit/s (52 170 000 for [`Self::isdbs`]`(48)`).
    pub fn bps(&self) -> f64 {
        (PACKET_BITS * u128::from(HZ) * u128::from(self.den)) as f64 / self.num as f64
    }

    /// True if `t1 − t0` ticks (exact) is within `tol` ticks of
    /// S(j1) − S(j0), i.e. |(t1 − t0)·den − (j1 − j0)·num| ≤ tol·den.
    #[cfg(test)]
    fn on_grid(&self, dt: i128, dj: i128, tol: i128) -> bool {
        let (num, den) = (i128::from(self.num), i128::from(self.den));
        (dt * den - dj * num).abs() <= tol * den
    }
}

fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

fn sat(v: u128) -> u64 {
    u64::try_from(v).unwrap_or(u64::MAX)
}

/// A packet source that tags each packet with the loop pass it was read
/// in (0 without looping; the loop seam of pcr-pacing.md §7.6 uses it).
pub trait TaggedSource {
    /// The next packet and its pass. `None`: end of input
    /// ([`Self::ended`]) or `stop` was raised while blocked.
    fn next(&mut self, stop: &AtomicBool) -> Option<(Packet, u32)>;
    /// True once the input has ended and is drained.
    fn ended(&self) -> bool;
}

/// A [`PacketStream`] as a [`TaggedSource`]: every packet is pass 0 (the
/// stream's own [`PacketStream::next`], unchanged; the reader thread does
/// not tag rewinds yet).
pub struct Untagged<'a>(pub &'a mut PacketStream);

impl TaggedSource for Untagged<'_> {
    fn next(&mut self, stop: &AtomicBool) -> Option<(Packet, u32)> {
        self.0.next(stop).map(|p| (p, 0))
    }

    fn ended(&self) -> bool {
        self.0.ended()
    }
}

/// How [`SlotPacer::fill`] waits for input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitMode {
    /// Block until the source has given every packet due in the requested
    /// slots, or has ended (file and loop feeds). A deadline-bounded `Live`
    /// mode for FIFOs and pipes is planned (pcr-pacing.md §7.7, piece P5).
    Block,
}

/// Counters of a [`SlotPacer`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PacerStats {
    /// Input packets placed in slots (input nulls excluded).
    pub placed: u64,
    /// Input null packets dropped (H.222.0 §2.4.3.3, PID 0x1FFF).
    pub dropped_nulls: u64,
    /// S(j) − due of the last placed packet (27 MHz ticks): under a slot in
    /// steady state, more when the input exceeds the capacity.
    pub lag: u64,
    /// The largest `lag` so far.
    pub max_lag: u64,
    /// Packets placed after their own slot because it was taken.
    pub saturated: u64,
    /// PCR fields corrected (any PID).
    pub pcr_restamped: u64,
    /// The largest correction Δ added to a PCR (27 MHz ticks; Δ ≥ 0 with
    /// the late strategy).
    pub max_delta: u64,
    /// PCR discontinuities of the reference PID bridged by the clock
    /// (indicator or out-of-range step, §2.4.3.5).
    pub discontinuities: u64,
}

/// Places a TS on a [`SlotClock`] by its PCR and restamps its PCRs; see
/// the module documentation for the rule.
#[derive(Debug)]
pub struct SlotPacer {
    clock: PcrClock<(Packet, u32)>,
    slots: SlotClock,
    mode: WaitMode,
    /// Global index of the first slot of the next [`Self::fill`].
    next_out: u64,
    /// Slot after the last placed packet.
    next_free: u64,
    /// A placed packet beyond the slots handed out so far: (slot, packet).
    carry: Option<(u64, Packet)>,
    /// The source has ended and the clock has timed every packet.
    ended: bool,
    stats: PacerStats,
    /// Due times of the first and last placed packets (input rate).
    first_t: Option<u64>,
    last_t: u64,
}

impl SlotPacer {
    /// A pacer on `slots`, timed by the PCRs of `pcr_pid` (or of the first
    /// PID carrying a PCR, H.222.0 Table 2-6), waiting for input as `mode`
    /// says.
    pub fn new(slots: SlotClock, pcr_pid: Option<Pid>, mode: WaitMode) -> Self {
        Self {
            clock: PcrClock::new(pcr_pid),
            slots,
            mode,
            next_out: 0,
            next_free: 0,
            carry: None,
            ended: false,
            stats: PacerStats::default(),
            first_t: None,
            last_t: 0,
        }
    }

    /// The next `n` slots: exactly `n` packets, the placed input packets at
    /// their slots and null packets (H.222.0 §2.4.3.3) elsewhere, and how
    /// many came from the input. Reads `src` until a packet falls beyond
    /// these slots (it waits for the next call), the input ends, or `stop`
    /// is raised (the slots not filled by then are nulls).
    pub fn fill(
        &mut self,
        n: usize,
        src: &mut dyn TaggedSource,
        stop: &AtomicBool,
    ) -> (Vec<Packet>, usize) {
        let start = self.next_out;
        let end = start.saturating_add(u64::try_from(n).unwrap_or(u64::MAX));
        self.next_out = end;
        // Slots before `start` are gone: a packet read late (after a stop)
        // takes the first slot still free.
        self.next_free = self.next_free.max(start);
        let mut out = vec![null_packet(); n];
        let mut real = 0;
        loop {
            let (j, p) = match self.carry.take() {
                Some(c) => c,
                None => match self.next_placed(src, stop) {
                    Some(c) => c,
                    None => break,
                },
            };
            let at = usize::try_from(j - start).ok();
            let Some(slot) = at.and_then(|k| out.get_mut(k)) else {
                self.carry = Some((j, p)); // due beyond these slots
                break;
            };
            *slot = p;
            real += 1;
        }
        (out, real)
    }

    /// The next input packet with its slot, restamped; `None` at the end of
    /// the input or when stopped.
    fn next_placed(
        &mut self,
        src: &mut dyn TaggedSource,
        stop: &AtomicBool,
    ) -> Option<(u64, Packet)> {
        loop {
            match self.mode {
                WaitMode::Block => {
                    while self.clock.ready() == 0 && !self.ended {
                        match src.next(stop) {
                            Some((p, pass)) => self.clock.push(&p, (p, pass)),
                            None if src.ended() => {
                                self.clock.finish();
                                self.ended = true;
                            }
                            None => return None, // stopped
                        }
                    }
                }
            }
            let x = self.clock.pop()?;
            // The pass tag is for the loop seam (§7.6), not used yet.
            let (mut p, _pass) = x.item;
            if is_null(&p) {
                self.stats.dropped_nulls += 1;
                continue;
            }
            let own = self.slots.first_at_or_after(x.t);
            let j = own.max(self.next_free);
            self.next_free = j.saturating_add(1);
            // time(j) ≥ t because j ≥ ⌈t·den/num⌉ and t is an integer.
            let delta = self.slots.time(j).saturating_sub(x.t);
            if add_to_pcr(&mut p, delta) {
                self.stats.pcr_restamped += 1;
                self.stats.max_delta = self.stats.max_delta.max(delta);
            }
            let st = &mut self.stats;
            st.saturated += u64::from(j > own);
            st.placed += 1;
            st.lag = delta;
            st.max_lag = st.max_lag.max(delta);
            self.first_t.get_or_insert(x.t);
            self.last_t = x.t;
            return Some((j, p));
        }
    }

    /// True once the input has ended, the clock is drained and nothing is
    /// carried: every input packet has been handed out.
    pub fn drained(&self) -> bool {
        self.ended && self.clock.ready() == 0 && self.carry.is_none()
    }

    /// Counters so far.
    pub fn stats(&self) -> PacerStats {
        PacerStats {
            discontinuities: self.clock.stats().discontinuities,
            ..self.stats
        }
    }

    /// Status-line fragment:
    /// ` | PCR lag 0.00 s (max 0.00) in 0.11 Mbit/s (cap 52.17) nulls-dropped N restamped N`,
    /// the input rate (non-null packets over their due-time span) once it
    /// spans half a second, `OVER CAPACITY` when it exceeds the slots'
    /// rate, and `slot-busy` / `pcr-discontinuities` counts when non-zero.
    pub fn status(&self) -> String {
        let st = self.stats();
        let secs = |t: u64| t as f64 / HZ as f64;
        let mut s = format!(
            " | PCR lag {:.2} s (max {:.2})",
            secs(st.lag),
            secs(st.max_lag)
        );
        let cap = self.slots.bps();
        let span = self.last_t.saturating_sub(self.first_t.unwrap_or(0));
        let rate = (span > HZ / 2)
            .then(|| (st.placed * 188 * 8) as f64 / secs(span))
            .filter(|r| r.is_finite());
        if let Some(r) = rate {
            s += &format!(" in {:.2} Mbit/s", r / 1e6);
        }
        s += &format!(" (cap {:.2})", cap / 1e6);
        if rate.is_some_and(|r| r > cap * 1.001) {
            s += " OVER CAPACITY";
        }
        s += &format!(
            " nulls-dropped {} restamped {}",
            st.dropped_nulls, st.pcr_restamped
        );
        if st.saturated > 0 {
            s += &format!(" slot-busy {}", st.saturated);
        }
        if st.discontinuities > 0 {
            s += &format!(" pcr-discontinuities {}", st.discontinuities);
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::Feed;
    use crate::source::tests::pkt;
    use isdb_ts::pcr::{PCR_MODULUS, pcr_wrap_add, pcr_wrap_diff};
    use isdb_ts::retime::pcr_field;
    use proptest::prelude::*;
    use std::sync::mpsc::sync_channel;

    /// AF-only PCR packet (H.222.0 Table 2-6), CC 0, with the
    /// discontinuity_indicator `disc` and an OPCR if given.
    fn pcr_pkt(pid: u16, ticks: u64, disc: bool, opcr: Option<u64>) -> Packet {
        let mut p = [0xFFu8; 188];
        let [hi, lo] = pid.to_be_bytes();
        let flags = 0x10 | if disc { 0x80 } else { 0 } | if opcr.is_some() { 0x08 } else { 0 };
        p[..6].copy_from_slice(&[0x47, hi, lo, 0x20, 183, flags]);
        put(&mut p[6..12], ticks);
        if let Some(o) = opcr {
            put(&mut p[12..18], o);
        }
        p
    }

    fn put(f: &mut [u8], ticks: u64) {
        let f6: &mut [u8; 6] = f.try_into().unwrap();
        *f6 = isdb_ts::pcr::Pcr::from_ticks(ticks).encode().unwrap();
    }

    fn pcr_of(p: &Packet) -> Option<u64> {
        isdbt_mux::pcrclock::packet_pcr(p).map(|(v, _)| v)
    }

    fn null() -> Packet {
        null_packet()
    }

    /// In-memory [`TaggedSource`], pass 0.
    struct VecSource(std::collections::VecDeque<Packet>, bool);

    impl VecSource {
        fn new(v: &[Packet]) -> Self {
            Self(v.iter().copied().collect(), false)
        }
    }

    impl TaggedSource for VecSource {
        fn next(&mut self, _stop: &AtomicBool) -> Option<(Packet, u32)> {
            let p = self.0.pop_front();
            self.1 |= p.is_none();
            p.map(|p| (p, 0))
        }
        fn ended(&self) -> bool {
            self.1
        }
    }

    /// (slot, packet) of every non-null packet of `out`, its first slot
    /// being slot `first`.
    fn placed_from(first: u64, out: &[Packet]) -> Vec<(u64, Packet)> {
        (first..)
            .zip(out)
            .filter(|(_, p)| !is_null(p))
            .map(|(j, p)| (j, *p))
            .collect()
    }

    fn placed(out: &[Packet]) -> Vec<(u64, Packet)> {
        placed_from(0, out)
    }

    /// Run `input` through a pacer `n` slots at a time until it is drained
    /// and a fill holds no input packet: the (global slot, packet) of every
    /// input packet handed out.
    fn run(input: &[Packet], slots: SlotClock, n: usize) -> (Vec<(u64, Packet)>, SlotPacer) {
        let mut src = VecSource::new(input);
        let mut pacer = SlotPacer::new(slots, None, WaitMode::Block);
        let stop = AtomicBool::new(false);
        let mut got = vec![];
        let mut first = 0;
        loop {
            let (v, real) = pacer.fill(n, &mut src, &stop);
            assert_eq!(v.len(), n);
            assert_eq!(real, v.iter().filter(|p| !is_null(p)).count());
            if pacer.drained() && real == 0 {
                return (got, pacer);
            }
            got.extend(placed_from(first, &v));
            first += n as u64;
        }
    }

    /// `n` packets of a CBR stream at `tpp` ticks per packet: a PCR
    /// (PID 0x01FF, first value `pcr0`) every `pcr_every` packets from
    /// packet 0, a null every 7th, else data (PID 0x0111, index in the
    /// payload). Due time of packet i: i·tpp.
    fn cbr(n: u32, tpp: u64, pcr_every: u32, pcr0: u64) -> Vec<Packet> {
        (0..n)
            .map(|i| match i {
                i if i % pcr_every == 0 => pcr_pkt(0x01FF, pcr0 + u64::from(i) * tpp, false, None),
                i if i % 7 == 3 => null(),
                i => pkt(0x0111, i),
            })
            .collect()
    }

    fn mbps(tpp: u64) -> f64 {
        188.0 * 8.0 * 27e6 / tpp as f64 / 1e6
    }

    #[test]
    fn spec_slot_clock_isdbs() {
        let c = SlotClock::isdbs(48).unwrap();
        // 384 slots = one superframe = 11 059 200 / 37 ticks, exactly: 37
        // superframes are 11 059 200 ticks.
        assert_eq!(c.time(384 * 37), 11_059_200);
        assert_eq!(c.time(384), 298_897); // 298 897.30
        assert_eq!(c.time(37), 28_800); // 28 800 / 37 ticks per slot
        assert_eq!(c.first_at_or_after(11_059_200), 384 * 37);
        assert_eq!(c.first_at_or_after(11_059_201), 384 * 37 + 1);
        assert_eq!(c.first_at_or_after(1), 1);
        assert_eq!(c.first_at_or_after(0), 0);
        assert_eq!(c.bps(), 52_170_000.0);
        assert_eq!(c, SlotClock::new(28_800, 37).unwrap(), "reduced");
        // the superframe of the ISDB-S transmitter (txs.rs), within 1 ppb
        let sf = c.time(384 * 37) as f64 / 37.0 / 27e6;
        assert!((sf / crate::txs::superframe_seconds() - 1.0).abs() < 1e-9);
        // 16 slots: a third of the rate
        assert_eq!(SlotClock::isdbs(16).unwrap().bps(), 17_390_000.0);
        assert_eq!(SlotClock::isdbs(0), None);
        assert_eq!(SlotClock::new(1, 0), None);
    }

    #[test]
    fn spec_table_2_6_pcr_correction_wraps_and_keeps_reserved_bits() {
        let mut p = pcr_pkt(0x100, PCR_MODULUS - 2, false, None);
        p[10] &= !0x02; // a reserved bit not '1': kept verbatim
        let before = p;
        assert_eq!(pcr_field(&p), Some(6));
        assert!(add_to_pcr(&mut p, 301));
        assert_eq!(pcr_of(&p), Some(299));
        assert_eq!(p[10] & 0x7E, before[10] & 0x7E);
        assert_eq!(p[12..], before[12..]);
        let mut d = pkt(0x100, 1);
        assert_eq!(pcr_field(&d), None);
        assert!(!add_to_pcr(&mut d, 5));
        assert_eq!(d, pkt(0x100, 1));
        // an adaptation field too short for the PCR it flags
        let mut s = pcr_pkt(0x100, 0, false, None);
        s[4] = 1;
        assert_eq!(pcr_field(&s), None);
    }

    #[test]
    fn cbr_input_is_placed_late_by_less_than_a_slot() {
        let c = SlotClock::isdbs(48).unwrap();
        // ticks per packet for 0.1, 10, 29.96 and 52.0 Mbit/s
        for tpp in [406_080, 4061, 1355, 781] {
            let pcr_every = u32::try_from((2_700_000 / tpp).clamp(1, 40)).unwrap();
            let input = cbr(3000, tpp, pcr_every, 1_000_000);
            let (got, pacer) = run(&input, c, 384);
            let want: Vec<(u64, Packet)> = (0u64..)
                .zip(&input)
                .filter(|(_, p)| !is_null(p))
                .map(|(i, p)| (i * tpp, *p))
                .collect();
            assert_eq!(
                got.len(),
                want.len(),
                "every packet once ({} Mbit/s)",
                mbps(tpp)
            );
            for ((j, p), (due, q)) in got.iter().zip(&want) {
                // 0 ≤ S(j) − due < one slot, exactly: j·num ≥ due·den and
                // j·num < due·den + num
                let jn = u128::from(*j) * u128::from(c.num);
                let dd = u128::from(*due) * u128::from(c.den);
                assert!(
                    jn >= dd && jn < dd + u128::from(c.num),
                    "slot {j} due {due}"
                );
                if pcr_of(q).is_none() {
                    assert_eq!(p, q, "order and content kept");
                }
            }
            let st = pacer.stats();
            let nulls = input.iter().filter(|p| is_null(p)).count() as u64;
            assert_eq!(st.dropped_nulls, nulls);
            assert_eq!(st.placed, want.len() as u64);
            assert_eq!(st.saturated, 0);
            assert!(st.max_lag < 779, "{st:?}");
            assert_eq!(st.pcr_restamped, u64::from(3000_u32.div_ceil(pcr_every)));
        }
    }

    /// Exact due times by H.222.0 eq. 2-4 between the PCRs at `pcrs`
    /// (index, continuous ticks), the integer division `PcrClock` uses.
    fn due_eq_2_4(pcrs: &[(u64, u64)], n: u64) -> Vec<u64> {
        let mut due = vec![0; usize::try_from(n).unwrap()];
        for w in pcrs.windows(2) {
            let ((i0, t0), (i1, t1)) = (w[0], w[1]);
            for i in i0..=i1 {
                due[usize::try_from(i).unwrap()] = t0 + (t1 - t0) * (i - i0) / (i1 - i0);
            }
        }
        due
    }

    /// A stream for the properties: `n_r` slots/frame, `frac` of the
    /// capacity, PCRs (PID 0x01FF, raw values from `base`) every
    /// `interval` ticks jittered by up to ±one slot, ending on a PCR; a
    /// second PCR PID 0x01FE on the same clock offset by `base2`, and a
    /// null every 5th packet. Returns the packets and their due times.
    fn jittered(
        n_r: usize,
        frac: f64,
        interval: u64,
        jitter: &[i64],
        base: u64,
        base2: u64,
    ) -> (SlotClock, Vec<Packet>, Vec<u64>) {
        let c = SlotClock::isdbs(n_r).unwrap();
        let slot = c.num.div_ceil(c.den);
        // f64 → u64 of a positive tick count well within range
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let tpp = ((c.num as f64 / c.den as f64) / frac).ceil() as u64;
        let every = (interval / tpp).max(1);
        // PCR jitter up to ±one slot, less when a PCR interval holds so few
        // packets that the jitter would push its local rate over the
        // capacity (the property is about the unsaturated case)
        let jb = slot.min((tpp - slot - 2) * every / 2);
        let n = every * jitter.len() as u64 + 1;
        let pcrs: Vec<(u64, u64)> = (0u64..)
            .zip(jitter.iter().chain([&0]))
            .map(|(k, &jit)| {
                let j = jit.clamp(-(jb as i64), jb as i64);
                let t = (k * every * tpp).saturating_add_signed(if k == 0 { 0 } else { j });
                (k * every, t)
            })
            .collect();
        let due = due_eq_2_4(&pcrs, n);
        let packets = (0..n)
            .map(|i| {
                let t = due[usize::try_from(i).unwrap()];
                if i % every == 0 {
                    pcr_pkt(0x01FF, base + t, false, None)
                } else if i % 13 == 6 {
                    pcr_pkt(0x01FE, base2 + t, false, None)
                } else if i % 5 == 2 {
                    null()
                } else {
                    pkt(0x0111, u32::try_from(i).unwrap())
                }
            })
            .collect();
        (c, packets, due)
    }

    fn stream_params() -> impl Strategy<Value = (usize, f64, u64, Vec<i64>, u64, u64)> {
        (
            1usize..=48,
            0.01f64..0.7,
            270_000u64..=2_700_000, // 10–100 ms
            prop::collection::vec(any::<i64>(), 2..12),
            0..PCR_MODULUS,
            0..PCR_MODULUS,
        )
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(48))]

        #[test]
        fn prop_placed_pcr_error_below_one_slot(
            (n_r, frac, interval, jitter, base, base2) in stream_params()
        ) {
            let (c, input, due) = jittered(n_r, frac, interval, &jitter, base, base2);
            let (got, pacer) = run(&input, c, 8 * n_r);
            prop_assert_eq!(pacer.stats().saturated, 0);
            prop_assert_eq!(pacer.stats().discontinuities, 0);
            let want: Vec<(u64, Packet)> = due
                .iter()
                .zip(&input)
                .filter(|(_, p)| !is_null(p))
                .map(|(&t, p)| (t, *p))
                .collect();
            prop_assert_eq!(got.len(), want.len());
            for ((j, p), (t, q)) in got.iter().zip(&want) {
                let jn = u128::from(*j) * u128::from(c.num);
                let dd = u128::from(*t) * u128::from(c.den);
                prop_assert!(jn >= dd && jn < dd + u128::from(c.num), "slot {} due {}", j, t);
                if pcr_of(q).is_none() {
                    prop_assert_eq!(p, q);
                }
            }
        }

        #[test]
        fn prop_restamped_pcr_is_on_the_slot_grid(
            (n_r, frac, interval, jitter, base, base2) in stream_params()
        ) {
            let (c, input, _) = jittered(n_r, frac, interval, &jitter, base, base2);
            let (got, _) = run(&input, c, 8 * n_r);
            for pid in [0x01FF, 0x01FE] {
                let pcrs: Vec<(u64, u64)> = got
                    .iter()
                    .filter(|(_, p)| crate::source::pid(p) == pid)
                    .map(|(j, p)| (*j, pcr_of(p).unwrap()))
                    .collect();
                // 0x01FE is absent when every packet is a reference PCR
                let Some(&(j0, v0)) = pcrs.first() else { continue };
                for &(j, v) in &pcrs {
                    let dt = i128::from(pcr_wrap_diff(v, v0));
                    let dj = i128::from(j - j0);
                    prop_assert!(c.on_grid(dt, dj, 2), "pid {:#x} slot {} pcr {}", pid, j, v);
                }
            }
        }
    }

    #[test]
    fn two_pcr_pids_are_both_restamped() {
        // Two programs with unrelated clocks, both at 1 234 ticks/packet;
        // 0x01FF is the reference (first seen).
        let c = SlotClock::isdbs(48).unwrap();
        let tpp = 1234;
        let input: Vec<Packet> = (0..400u64)
            .map(|i| match i % 10 {
                0 => pcr_pkt(0x01FF, 7_000_000 + i * tpp, false, None),
                5 => pcr_pkt(0x01FE, 900_000_000 + i * tpp, false, None),
                _ => pkt(0x0111, u32::try_from(i).unwrap()),
            })
            .collect();
        let (got, pacer) = run(&input, c, 384);
        assert_eq!(got.len(), 400);
        for ((j, p), (i, q)) in got.iter().zip((0u64..).zip(&input)) {
            if let Some(v_in) = pcr_of(q) {
                let delta = c.time(*j) - i * tpp;
                assert!(delta < 779);
                assert_eq!(
                    pcr_of(p),
                    Some(v_in + delta),
                    "pid {:#x}",
                    crate::source::pid(p)
                );
                assert_eq!(p[12..], q[12..]);
            }
        }
        assert_eq!(pacer.stats().pcr_restamped, 80);
        assert_eq!(pacer.stats().discontinuities, 0);
    }

    #[test]
    fn pcr_wrap_mid_stream() {
        let c = SlotClock::isdbs(48).unwrap();
        let tpp = 2000;
        // raw PCR wraps after 50 packets
        let input = cbr(200, tpp, 10, PCR_MODULUS - 50 * tpp);
        let (got, pacer) = run(&input, c, 384);
        assert_eq!(
            pacer.stats().discontinuities,
            0,
            "wrap is not a discontinuity"
        );
        let pcrs: Vec<(u64, u64)> = got
            .iter()
            .filter_map(|(j, p)| Some((*j, pcr_of(p)?)))
            .collect();
        assert_eq!(pcrs.len(), 20);
        assert!(pcrs.iter().any(|&(_, v)| v < 1_000_000), "wrapped");
        let (j0, v0) = pcrs[0];
        assert_eq!(v0, PCR_MODULUS - 50 * tpp, "the first PCR is at slot 0");
        for &(j, v) in &pcrs {
            assert_eq!(pcr_wrap_diff(v, v0), c.time(j) - c.time(j0));
        }
    }

    #[test]
    fn discontinuity_indicator_passes_through_with_delta() {
        let c = SlotClock::isdbs(48).unwrap();
        let tpp = 1500;
        // PCRs every 10 packets; from packet 100 on, a new time base
        // announced by the discontinuity_indicator.
        let input: Vec<Packet> = (0..200u64)
            .map(|i| match (i % 10, i) {
                (0, ..100) => pcr_pkt(0x01FF, 5_000_000 + i * tpp, false, None),
                (0, 100) => pcr_pkt(0x01FF, 42, true, None),
                (0, _) => pcr_pkt(0x01FF, 42 + (i - 100) * tpp, false, None),
                _ => pkt(0x0111, u32::try_from(i).unwrap()),
            })
            .collect();
        let (got, pacer) = run(&input, c, 384);
        assert_eq!(pacer.stats().discontinuities, 1);
        assert_eq!(got.len(), 200);
        for ((j, p), (i, q)) in got.iter().zip((0u64..).zip(&input)) {
            // the clock continues at the previous rate: due = i·tpp
            let delta = c.time(*j) - i * tpp;
            assert!(delta < 779);
            if let Some(v) = pcr_of(q) {
                assert_eq!(pcr_of(p), Some(pcr_wrap_add(v, delta)));
                assert_eq!(p[5], q[5], "flags, discontinuity_indicator included");
            } else {
                assert_eq!(p, q);
            }
        }
    }

    #[test]
    fn opcr_untouched() {
        let c = SlotClock::isdbs(48).unwrap();
        let tpp = 3000;
        let input: Vec<Packet> = (0..60u64)
            .map(|i| match i % 10 {
                0 => pcr_pkt(0x01FF, 100 + i * tpp, false, Some(77_777 + i)),
                _ => pkt(0x0111, u32::try_from(i).unwrap()),
            })
            .collect();
        let (got, pacer) = run(&input, c, 384);
        assert_eq!(pacer.stats().pcr_restamped, 6);
        let mut moved = 0;
        for ((j, p), (i, q)) in got.iter().zip((0u64..).zip(&input)) {
            if pcr_of(q).is_some() {
                assert_eq!(p[..6], q[..6]);
                assert_eq!(p[12..], q[12..], "OPCR and stuffing kept");
                let delta = c.time(*j) - i * tpp;
                assert_eq!(pcr_of(p), Some(pcr_of(q).unwrap() + delta));
                moved += u32::from(delta > 0);
            }
        }
        assert!(moved > 0, "some PCR was actually corrected");
    }

    #[test]
    fn over_capacity_lags_and_sends_everything() {
        let c = SlotClock::isdbs(48).unwrap();
        // 1.5 × 52.17 Mbit/s, no nulls: 30 000 packets due over 0.577 s
        // take 30 000 slots, 0.865 s.
        let tpp = 519;
        let input: Vec<Packet> = (0..30_000u64)
            .map(|i| match i % 40 {
                0 => pcr_pkt(0x01FF, 1_000 + i * tpp, false, None),
                _ => pkt(0x0111, u32::try_from(i).unwrap()),
            })
            .collect();
        let (got, pacer) = run(&input, c, 384);
        assert_eq!(got.len(), 30_000, "every packet sent");
        assert_eq!(got.last().unwrap().0, 29_999, "back to back from slot 0");
        for ((_, p), q) in got.iter().zip(&input) {
            if pcr_of(q).is_none() {
                assert_eq!(p, q, "order kept");
            }
        }
        let st = pacer.stats();
        // packet i is in slot i: saturated when its own slot is earlier
        // (from packet 4 on: 4 · 519 = 2 076 ticks is in slot 3)
        let own_earlier = (0..30_000u64)
            .filter(|&i| c.first_at_or_after(i * tpp) < i)
            .count() as u64;
        assert_eq!(own_earlier, 29_996);
        assert_eq!(st.saturated, own_earlier);
        // the last packet: S(29 999) − 29 999 · 519
        assert_eq!(st.max_lag, c.time(29_999) - 29_999 * tpp);
        assert_eq!(st.lag, st.max_lag);
        assert_eq!(st.max_delta, c.time(29_960) - 29_960 * tpp, "last PCR");
        // restamped PCRs still give the slot clock
        for (j, p) in &got {
            if let Some(v) = pcr_of(p) {
                assert_eq!(v - 1_000, c.time(*j));
            }
        }
        let s = pacer.status();
        assert!(
            s.contains("OVER CAPACITY") && s.contains("(cap 52.17)"),
            "{s}"
        );
        assert!(s.contains("in 78.25 Mbit/s"), "{s}");
    }

    /// The reference model `tools/isdbs-pacing-sim.py` (mode `restamp`, 48
    /// slots) on the same synthetic stream: 120 packets at 1 234 ticks
    /// each, PCR PID 0x01FF every 10 packets from 1 000 000, a second PCR
    /// PID 0x01FE (wrapping: from 2^33·300 − 50 000) at i ≡ 5 (mod 10), a
    /// null at the other i ≡ 3 (mod 4), else `pkt(0x0111, i)`. Reproduce:
    /// write that stream to `in.ts` (the bytes of [`pcr_pkt`] / `pkt`),
    /// `python3 tools/isdbs-pacing-sim.py in.ts 0.006 restamp out.ts`
    /// (208 slots), and list the non-null slots and the PCRs of `out.ts`.
    #[test]
    fn agrees_with_the_python_model() {
        let input: Vec<Packet> = (0..120u64)
            .map(|i| match i {
                i if i % 10 == 0 => pcr_pkt(0x01FF, 1_000_000 + i * 1234, false, None),
                i if i % 10 == 5 => pcr_pkt(0x01FE, PCR_MODULUS - 50_000 + i * 1234, false, None),
                i if i % 4 == 3 => null(),
                i => pkt(0x0111, u32::try_from(i).unwrap()),
            })
            .collect();
        let want_slots: [u64; 96] = [
            0, 2, 4, 7, 8, 10, 13, 15, 16, 20, 21, 23, 24, 26, 27, 29, 32, 34, 35, 39, 40, 42, 45,
            46, 48, 51, 53, 54, 56, 58, 59, 61, 64, 65, 67, 70, 72, 73, 77, 78, 80, 83, 85, 86, 88,
            89, 91, 92, 96, 97, 99, 102, 104, 105, 108, 110, 111, 115, 116, 118, 119, 121, 123,
            124, 127, 129, 130, 134, 135, 137, 140, 142, 143, 146, 148, 150, 151, 153, 154, 156,
            159, 161, 162, 165, 167, 169, 172, 173, 175, 178, 180, 181, 183, 184, 186, 188,
        ];
        let want_pcrs: [(u64, u16, u64); 24] = [
            (0, 0x1FF, 1_000_000),
            (8, 0x1FE, 2_576_980_333_827),
            (16, 0x1FF, 1_012_454),
            (24, 0x1FE, 2_576_980_346_281),
            (32, 0x1FF, 1_024_908),
            (40, 0x1FE, 2_576_980_358_735),
            (48, 0x1FF, 1_037_362),
            (56, 0x1FE, 2_576_980_371_189),
            (64, 0x1FF, 1_049_816),
            (72, 0x1FE, 6_043),
            (80, 0x1FF, 1_062_270),
            (88, 0x1FE, 18_497),
            (96, 0x1FF, 1_074_724),
            (104, 0x1FE, 30_951),
            (111, 0x1FF, 1_086_400),
            (119, 0x1FE, 42_627),
            (127, 0x1FF, 1_098_854),
            (135, 0x1FE, 55_081),
            (143, 0x1FF, 1_111_308),
            (151, 0x1FE, 67_535),
            (159, 0x1FF, 1_123_762),
            (167, 0x1FE, 79_989),
            (175, 0x1FF, 1_136_216),
            (183, 0x1FE, 92_443),
        ];
        let mut src = VecSource::new(&input);
        let mut pacer = SlotPacer::new(SlotClock::isdbs(48).unwrap(), None, WaitMode::Block);
        let stop = AtomicBool::new(false);
        let (out, real) = pacer.fill(208, &mut src, &stop);
        assert_eq!(real, 96);
        let got = placed(&out);
        assert_eq!(got.iter().map(|x| x.0).collect::<Vec<_>>(), want_slots);
        let pcrs: Vec<(u64, u16, u64)> = got
            .iter()
            .filter_map(|(j, p)| Some((*j, crate::source::pid(p), pcr_of(p)?)))
            .collect();
        assert_eq!(pcrs, want_pcrs);
        // the model's output TS: nulls in every other slot
        assert!(out.iter().filter(|p| is_null(p)).all(|p| *p == null()));
        assert!(pacer.drained());
    }

    #[test]
    fn untagged_stream_and_status_line() {
        let c = SlotClock::isdbs(48).unwrap();
        // 0.1 Mbit/s for 0.6 s: PCR every 4 packets
        let input = cbr(41, 406_080, 4, 0);
        let (tx, rx) = sync_channel(64);
        let mut s = PacketStream::from_receiver("f", rx, Feed::File);
        for p in &input {
            tx.send(*p).unwrap();
        }
        drop(tx);
        let mut pacer = SlotPacer::new(c, Pid::new(0x01FF), WaitMode::Block);
        let stop = AtomicBool::new(false);
        let mut n = 0;
        while !pacer.drained() {
            let (v, real) = pacer.fill(384, &mut Untagged(&mut s), &stop);
            assert_eq!(v.len(), 384);
            n += real;
        }
        assert!(s.ended());
        assert_eq!(n, input.iter().filter(|p| !is_null(p)).count());
        let st = pacer.status();
        assert!(
            st.starts_with(" | PCR lag 0.00 s (max 0.00) in 0.09 Mbit/s (cap 52.17) nulls-dropped 5 restamped 11"),
            "{st}"
        );
    }

    #[test]
    fn fill_honours_stop_and_late_packets_take_the_next_free_slot() {
        let c = SlotClock::isdbs(48).unwrap();
        let (tx, rx) = sync_channel(64);
        let mut s = PacketStream::from_receiver("f", rx, Feed::File);
        let stop = AtomicBool::new(true);
        let mut pacer = SlotPacer::new(c, None, WaitMode::Block);
        // nothing queued and stop raised: all nulls, not drained
        let (v, real) = pacer.fill(10, &mut Untagged(&mut s), &stop);
        assert_eq!((v.len(), real), (10, 0));
        assert!(!pacer.drained());
        // packets due at slot 0 arriving after slots 0..10 went out
        tx.send(pcr_pkt(0x01FF, 0, false, None)).unwrap();
        tx.send(pkt(0x0111, 1)).unwrap();
        drop(tx);
        stop.store(false, std::sync::atomic::Ordering::Relaxed);
        let (v, real) = pacer.fill(10, &mut Untagged(&mut s), &stop);
        assert_eq!(real, 2);
        assert_eq!(pcr_of(&v[0]), Some(c.time(10)), "restamped by the lag");
        assert!(pacer.drained());
        assert_eq!(pacer.stats().saturated, 2);
    }
}

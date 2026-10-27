//! continuity_counter semantics (H.222.0 §2.4.3.3) and the
//! discontinuity_indicator (§2.4.3.5).
//!
//! Rules implemented, quoting §2.4.3.3:
//! * "incrementing with each Transport Stream packet with the same PID …
//!   wraps around to 0 after its maximum value";
//! * "shall not be incremented when the adaptation_field_control of the
//!   packet equals '00' or '10'";
//! * "duplicate packets may be sent as two, and only two, consecutive
//!   Transport Stream packets of the same PID … same continuity_counter
//!   value … adaptation_field_control field shall be equal to '01' or '11'
//!   … each byte of the original packet shall be duplicated, with the
//!   exception that in the program clock reference fields … a valid value
//!   shall be encoded";
//! * "may be discontinuous when the discontinuity_indicator is set to '1'";
//! * "In the case of a null packet the value of the continuity_counter is
//!   undefined" (PID 0x1FFF is exempt).
//!
//! [`ContinuityCounter`] is the 4-bit value itself with its wrap-around
//! arithmetic; [`Tracker`] is the per-PID state machine shared by the
//! checker ([`CcChecker`]) and the section/PES reassemblers; [`Counter`]
//! generates counters on the transmit side.

use std::collections::HashMap;
use std::fmt;

use crate::packet::{Header, PACKET_SIZE, View};
use crate::pid::Pid;

/// The continuity_counter field (H.222.0 §2.4.3.3): "a 4-bit field
/// incrementing with each Transport Stream packet with the same PID. The
/// continuity_counter wraps around to 0 after its maximum value."
///
/// A value type that cannot hold more than 4 bits, so the `& 0x0F` that
/// every increment and comparison used to need is gone: [`next`](Self::next)
/// is the wrapping increment (15 → 0) and [`is_next_of`](Self::is_next_of)
/// the continuity test of §2.4.3.3 ("differs by a positive value of one").
/// *Which* packets increment the counter (payload-bearing ones only, never
/// a duplicate) is the business of [`Counter`] and [`Tracker`], not of the
/// value.
///
/// Formatting is the decimal value (`{}` gives `15`), as the analysers
/// print counters.
///
/// ```
/// use isdb_ts::ContinuityCounter;
///
/// let c = ContinuityCounter::new(15).unwrap();
/// assert_eq!(c.next(), ContinuityCounter::ZERO);
/// assert!(ContinuityCounter::ZERO.is_next_of(c));
/// assert!(ContinuityCounter::new(16).is_none());
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ContinuityCounter(u8);

impl ContinuityCounter {
    /// Counter value 0: the default, and what a fresh PID starts at.
    pub const ZERO: ContinuityCounter = ContinuityCounter(0);
    /// Largest value, 15: the one [`next`](Self::next) wraps to zero from.
    pub const MAX: ContinuityCounter = ContinuityCounter(0x0F);

    /// The counter with value `v`, or `None` if `v` does not fit in 4 bits.
    pub const fn new(v: u8) -> Option<Self> {
        if v <= Self::MAX.0 {
            Some(ContinuityCounter(v))
        } else {
            None
        }
    }

    /// The low 4 bits of `bits`, for a decoder that has just read the 4-bit
    /// field (`b[3] & 0x0F` of the packet header, Table 2-2).  Such a value
    /// is already within range, so the mask never changes it and the codec
    /// stays lossless; the mask exists so that the decoder is `const`,
    /// infallible and panic-free instead of carrying an `expect` for a
    /// case that cannot occur.
    pub const fn from_wire(bits: u8) -> Self {
        ContinuityCounter(bits & Self::MAX.0)
    }

    /// The value, 0..=15.
    pub const fn get(self) -> u8 {
        self.0
    }

    /// The counter of the next payload-bearing packet: this value plus one,
    /// wrapping "to 0 after its maximum value" (§2.4.3.3).
    #[must_use]
    pub const fn next(self) -> Self {
        ContinuityCounter((self.0 + 1) & Self::MAX.0)
    }

    /// True if `self` "differs by a positive value of one" from `prev`
    /// modulo 16 (§2.4.3.3): `self == prev.next()`.
    pub const fn is_next_of(self, prev: Self) -> bool {
        self.0 == prev.next().0
    }
}

impl Default for ContinuityCounter {
    /// [`ContinuityCounter::ZERO`].
    fn default() -> Self {
        Self::ZERO
    }
}

impl From<ContinuityCounter> for u8 {
    fn from(c: ContinuityCounter) -> u8 {
        c.0
    }
}

impl fmt::Display for ContinuityCounter {
    /// The decimal value, `0`..`15`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

/// Transmit-side continuity_counter for one PID (§2.4.3.3): increments
/// only for packets that carry payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Counter {
    next: ContinuityCounter,
}

impl Counter {
    /// Counter whose first payload-bearing packet gets `first`.
    pub const fn new(first: ContinuityCounter) -> Self {
        Self { next: first }
    }

    /// Value for the next packet: increments (mod 16) after use when
    /// `has_payload` (AFC '01'/'11'); for AFC '10' returns the value of the
    /// previous payload packet without incrementing.
    pub fn next(&mut self, has_payload: bool) -> ContinuityCounter {
        if has_payload {
            let c = self.next;
            self.next = c.next();
            c
        } else {
            // The previous payload packet's value, the one `next` follows.
            ContinuityCounter::from_wire(self.next.0.wrapping_sub(1))
        }
    }

    /// Value the next payload-bearing packet will get.
    pub const fn peek(&self) -> ContinuityCounter {
        self.next
    }
}

/// Classification of one packet's continuity_counter relative to the
/// previous packet of its PID (H.222.0 §2.4.3.3, §2.4.3.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Continuity {
    /// First packet seen on the PID: no expectation yet.
    First,
    /// Payload packet whose counter is previous + 1 (mod 16).
    Continuous,
    /// AFC '00'/'10' packet whose counter equals the previous one
    /// (or no previous): not incremented, as required.
    NoPayload,
    /// AFC '00'/'10' packet whose counter differs from the previous one
    /// without discontinuity_indicator ("shall not be incremented").
    /// Expectation is unchanged.
    NoPayloadChanged {
        /// Counter of the previous payload packet.
        previous: ContinuityCounter,
        /// Counter in this packet.
        got: ContinuityCounter,
    },
    /// Second copy of the previous payload packet (allowed once); the
    /// payload must not be used twice.
    Duplicate,
    /// Third or later consecutive packet with the same counter ("two, and
    /// only two"); the payload is also a repeat and must not be used.
    ExcessDuplicate,
    /// Counter jump in a packet with discontinuity_indicator '1' (allowed).
    Discontinuity {
        /// Expected counter.
        expected: ContinuityCounter,
        /// Counter in this packet.
        got: ContinuityCounter,
    },
    /// Counter jump without discontinuity_indicator: packets were lost or
    /// reordered.
    Break {
        /// Expected counter.
        expected: ContinuityCounter,
        /// Counter in this packet.
        got: ContinuityCounter,
    },
}

impl Continuity {
    /// True if the payload of this packet must be dropped (it repeats the
    /// previous packet's payload).
    pub fn is_repeat(&self) -> bool {
        matches!(self, Continuity::Duplicate | Continuity::ExcessDuplicate)
    }

    /// True if payload data may be missing before this packet.
    pub fn is_loss(&self) -> bool {
        matches!(self, Continuity::Break { .. })
    }
}

/// Per-PID continuity state machine (§2.4.3.3).
#[derive(Debug, Clone, Default)]
pub struct Tracker {
    last: Option<ContinuityCounter>,
    copies: u8,
}

impl Tracker {
    /// Fresh state (next packet is [`Continuity::First`]).
    pub fn new() -> Self {
        Self::default()
    }

    /// Forget the expectation (e.g. after a transport error).
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Classify the packet with header `h` and discontinuity_indicator
    /// `discontinuity`, and update the state.
    pub fn update(&mut self, h: &Header, discontinuity: bool) -> Continuity {
        let cc = h.continuity_counter;
        if !h.has_payload() {
            return match self.last {
                Some(prev) if prev != cc => {
                    if discontinuity {
                        let expected = prev;
                        self.last = Some(cc);
                        self.copies = 0;
                        Continuity::Discontinuity { expected, got: cc }
                    } else {
                        Continuity::NoPayloadChanged {
                            previous: prev,
                            got: cc,
                        }
                    }
                }
                _ => Continuity::NoPayload,
            };
        }
        let Some(prev) = self.last else {
            self.last = Some(cc);
            self.copies = 0;
            return Continuity::First;
        };
        let expected = prev.next();
        if cc.is_next_of(prev) {
            self.last = Some(cc);
            self.copies = 0;
            Continuity::Continuous
        } else if cc == prev {
            self.copies = self.copies.saturating_add(1);
            if self.copies == 1 {
                Continuity::Duplicate
            } else {
                Continuity::ExcessDuplicate
            }
        } else {
            self.last = Some(cc);
            self.copies = 0;
            if discontinuity {
                Continuity::Discontinuity { expected, got: cc }
            } else {
                Continuity::Break { expected, got: cc }
            }
        }
    }
}

/// Kind of a continuity event reported by [`CcChecker`] (H.222.0 §2.4.3.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CcEventKind {
    /// Counter jump without discontinuity_indicator (§2.4.3.3).
    Break {
        /// Expected counter.
        expected: ContinuityCounter,
        /// Counter in the packet.
        got: ContinuityCounter,
    },
    /// Counter jump signalled by discontinuity_indicator (§2.4.3.5).
    Discontinuity {
        /// Expected counter.
        expected: ContinuityCounter,
        /// Counter in the packet.
        got: ContinuityCounter,
    },
    /// Duplicate packet; `identical` is false if any byte other than the
    /// PCR field differs from the original (§2.4.3.3).
    Duplicate {
        /// Bytes equal except PCR.
        identical: bool,
    },
    /// More than one duplicate in a row (§2.4.3.3 "two, and only two").
    ExcessDuplicate,
    /// AFC '00'/'10' packet whose counter changed (§2.4.3.3 "shall not be
    /// incremented").
    NoPayloadChanged {
        /// Counter of the previous payload packet.
        previous: ContinuityCounter,
        /// Counter in the packet.
        got: ContinuityCounter,
    },
    /// transport_error_indicator '1': the packet was not used.
    TransportError,
}

/// One continuity event (H.222.0 §2.4.3.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CcEvent {
    /// Index of the packet in the stream.
    pub packet_index: u64,
    /// PID of the packet.
    pub pid: Pid,
    /// What happened.
    pub kind: CcEventKind,
}

#[derive(Debug, Clone)]
struct PidState {
    tracker: Tracker,
    prev: [u8; PACKET_SIZE],
}

/// Continuity checker for all PIDs of a stream (§2.4.3.3); PID 0x1FFF is
/// exempt.
#[derive(Debug, Clone, Default)]
pub struct CcChecker {
    pids: HashMap<Pid, PidState>,
}

impl CcChecker {
    /// Empty checker.
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed packet `index`; returns an event if the packet is not simply
    /// continuous.  Structurally invalid packets (adaptation field past the
    /// end) are classified from the header alone.
    pub fn push(&mut self, index: u64, pkt: &[u8; PACKET_SIZE]) -> Option<CcEvent> {
        let (h, disc) = match View::parse(pkt) {
            Ok(v) => (v.header, v.discontinuity_indicator()),
            Err(_) => (Header::decode(pkt).ok()?, false),
        };
        if h.pid == Pid::NULL {
            return None;
        }
        let ev = |kind| {
            Some(CcEvent {
                packet_index: index,
                pid: h.pid,
                kind,
            })
        };
        if h.transport_error_indicator {
            return ev(CcEventKind::TransportError);
        }
        let st = self.pids.entry(h.pid).or_insert_with(|| PidState {
            tracker: Tracker::new(),
            prev: [0; PACKET_SIZE],
        });
        let c = st.tracker.update(&h, disc);
        let out = match c {
            Continuity::First | Continuity::Continuous | Continuity::NoPayload => None,
            Continuity::NoPayloadChanged { previous, got } => {
                ev(CcEventKind::NoPayloadChanged { previous, got })
            }
            Continuity::Duplicate => ev(CcEventKind::Duplicate {
                identical: same_except_pcr(&st.prev, pkt),
            }),
            Continuity::ExcessDuplicate => ev(CcEventKind::ExcessDuplicate),
            Continuity::Discontinuity { expected, got } => {
                ev(CcEventKind::Discontinuity { expected, got })
            }
            Continuity::Break { expected, got } => ev(CcEventKind::Break { expected, got }),
        };
        if h.has_payload() && !c.is_repeat() {
            st.prev = *pkt;
        }
        out
    }
}

/// Byte equality except the 6-byte PCR field (§2.4.3.3 duplicate rule).
fn same_except_pcr(a: &[u8; PACKET_SIZE], b: &[u8; PACKET_SIZE]) -> bool {
    let has_pcr = |p: &[u8; PACKET_SIZE]| p[3] & 0x20 != 0 && p[4] >= 7 && p[5] & 0x10 != 0;
    if has_pcr(a) && has_pcr(b) {
        a[..6] == b[..6] && a[12..] == b[12..]
    } else {
        a == b
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packet::{AdaptationFields, Packet};
    use crate::pcr::Pcr;

    fn cc(v: u8) -> ContinuityCounter {
        ContinuityCounter::new(v).unwrap()
    }

    fn pkt(pid: Pid, c: u8, payload: bool) -> [u8; PACKET_SIZE] {
        let p = if payload {
            Packet::with_payload(pid, false, cc(c), &[c; 184]).unwrap()
        } else {
            Packet::with_adaptation(pid, false, cc(c), None, &[]).unwrap()
        };
        p.encode().unwrap()
    }

    /// §2.4.3.3: "a 4-bit field … wraps around to 0 after its maximum
    /// value": 16 is not a counter, `from_wire` keeps the low 4 bits,
    /// `next` wraps 15 → 0 and `is_next_of` is "differs by a positive value
    /// of one" modulo 16 and nothing else.
    #[test]
    fn spec_2_4_3_3_continuity_counter() {
        assert_eq!(ContinuityCounter::new(16), None);
        assert_eq!(ContinuityCounter::new(u8::MAX), None);
        assert_eq!(ContinuityCounter::new(15), Some(ContinuityCounter::MAX));
        assert_eq!(ContinuityCounter::new(0), Some(ContinuityCounter::ZERO));
        assert_eq!(ContinuityCounter::default(), ContinuityCounter::ZERO);
        assert_eq!(ContinuityCounter::from_wire(0x1F), cc(15));
        assert_eq!(ContinuityCounter::from_wire(0xF0), cc(0));
        assert_eq!(cc(15).next(), cc(0));
        assert_eq!(cc(0).next(), cc(1));
        for v in 0..=15u8 {
            let c = cc(v);
            assert_eq!(c.get(), v);
            assert_eq!(u8::from(c), v);
            assert_eq!(c.to_string(), v.to_string());
            assert_eq!(ContinuityCounter::from_wire(v), c);
            assert_eq!(c.next().get(), (v + 1) % 16);
            for w in 0..=15u8 {
                // Truth table: exactly one successor, modulo 16.
                assert_eq!(
                    cc(w).is_next_of(c),
                    w == (v + 1) % 16,
                    "{w}.is_next_of({v})"
                );
            }
        }
        assert!(cc(0).is_next_of(cc(15)));
        assert!(!cc(15).is_next_of(cc(0)));
        assert!(!cc(3).is_next_of(cc(3)));
        assert!(!cc(5).is_next_of(cc(3)));
        // `Ord` follows the value, so a sorted list of counters reads 0..15.
        assert!(cc(0) < cc(15));
    }

    /// §2.4.3.3: increments mod 16 on payload packets only; AFC '10' keeps.
    #[test]
    fn spec_2_4_3_3_counter_increment_rules() {
        let mut c = Counter::new(cc(14));
        assert_eq!(c.peek(), cc(14));
        assert_eq!(c.next(true), cc(14));
        assert_eq!(c.next(true), cc(15));
        assert_eq!(c.next(false), cc(15)); // AF-only: not incremented
        assert_eq!(c.next(true), cc(0)); // wraps to 0
        assert_eq!(Counter::default().next(false), cc(15)); // before any payload
        assert_eq!(Counter::default().next(true), cc(0));
        let mut ch = CcChecker::new();
        let mut events = vec![];
        let seq = [(14, true), (15, true), (15, false), (0, true), (1, true)];
        for (i, &(cc, pl)) in seq.iter().enumerate() {
            events.extend(ch.push(i as u64, &pkt(Pid::new(0x100).unwrap(), cc, pl)));
        }
        assert!(events.is_empty(), "{events:?}");
    }

    /// §2.4.3.3: one duplicate allowed, a second is excess; a gap is a break.
    #[test]
    fn spec_2_4_3_3_duplicates_and_breaks() {
        let mut ch = CcChecker::new();
        let a = pkt(Pid::new(0x100).unwrap(), 5, true);
        assert_eq!(ch.push(0, &a), None);
        assert_eq!(
            ch.push(1, &a).unwrap().kind,
            CcEventKind::Duplicate { identical: true }
        );
        assert_eq!(ch.push(2, &a).unwrap().kind, CcEventKind::ExcessDuplicate);
        let e = ch.push(3, &pkt(Pid::new(0x100).unwrap(), 8, true)).unwrap();
        assert_eq!(
            (e.packet_index, e.kind),
            (
                3,
                CcEventKind::Break {
                    expected: ContinuityCounter::new(6).unwrap(),
                    got: cc(8)
                }
            )
        );
        assert_eq!(ch.push(4, &pkt(Pid::new(0x100).unwrap(), 9, true)), None);
        // duplicate with different data
        let mut b = pkt(Pid::new(0x100).unwrap(), 9, true);
        b[100] ^= 1;
        assert_eq!(
            ch.push(5, &b).unwrap().kind,
            CcEventKind::Duplicate { identical: false }
        );
        // AF-only packet with changed counter
        assert_eq!(
            ch.push(6, &pkt(Pid::new(0x100).unwrap(), 3, false))
                .unwrap()
                .kind,
            CcEventKind::NoPayloadChanged {
                previous: ContinuityCounter::new(9).unwrap(),
                got: cc(3)
            }
        );
        assert_eq!(ch.push(7, &pkt(Pid::new(0x100).unwrap(), 10, true)), None);
    }

    /// §2.4.3.3: duplicates may differ only in the PCR value.
    #[test]
    fn spec_2_4_3_3_duplicate_pcr_exception() {
        let mk = |t| {
            Packet::with_adaptation(
                Pid::new(0x20).unwrap(),
                false,
                ContinuityCounter::new(2).unwrap(),
                Some(AdaptationFields {
                    pcr: Some(Pcr::from_ticks(t)),
                    ..Default::default()
                }),
                &[7; 100],
            )
            .unwrap()
            .encode()
            .unwrap()
        };
        let mut ch = CcChecker::new();
        ch.push(0, &mk(1000));
        assert_eq!(
            ch.push(1, &mk(2000)).unwrap().kind,
            CcEventKind::Duplicate { identical: true }
        );
    }

    /// §2.4.3.5: discontinuity_indicator resets the expectation; null PID exempt.
    #[test]
    fn spec_2_4_3_5_discontinuity_and_null_exempt() {
        let mut ch = CcChecker::new();
        ch.push(0, &pkt(Pid::new(0x100).unwrap(), 1, true));
        let d = Packet::with_adaptation(
            Pid::new(0x100).unwrap(),
            false,
            ContinuityCounter::new(9).unwrap(),
            Some(AdaptationFields {
                discontinuity_indicator: true,
                ..Default::default()
            }),
            &[0; 10],
        )
        .unwrap()
        .encode()
        .unwrap();
        assert_eq!(
            ch.push(1, &d).unwrap().kind,
            CcEventKind::Discontinuity {
                expected: ContinuityCounter::new(2).unwrap(),
                got: cc(9)
            }
        );
        assert_eq!(ch.push(2, &pkt(Pid::new(0x100).unwrap(), 10, true)), None);
        let n = Packet::null().encode().unwrap();
        for i in 0..3 {
            assert_eq!(ch.push(3 + i, &n), None);
        }
    }
}

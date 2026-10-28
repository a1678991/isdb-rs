//! Interactive playback of a `ts-program` relay: pause, seek and play rate
//! ([`crate::Multiplexer::pause`], [`crate::Multiplexer::seek`],
//! [`crate::Multiplexer::set_rate`]).
//!
//! The relay times every unit (PES, ADTS frame, carousel packet) in
//! *content time*: the source's PCR-derived time plus the pass offset
//! (27 MHz). A `Timeline` maps content time to mux time, piecewise
//! linearly: one segment per control change, each with its own rate.
//! Every PTS/DTS is mapped through it, and each packet keeps its source
//! arrival→decoding distance (arrival = mapped decoding time − source
//! lead), so the T-STD behaviour of the source is kept at any rate. A
//! segment starts at the largest content time already sent, so what is in
//! the receiver's buffers keeps its timing and the map stays monotonic.
//!
//! At a rate above 1 the content runs faster than the layer carries it, so
//! frames are skipped: video pictures by `VideoThinning` (whole PES,
//! respecting MPEG-2 reference dependencies, within the source's own bit
//! budget), audio frames by `AudioSlots` (one ADTS frame per output
//! frame period, from the content time that period shows). Below 1, video
//! pictures are shown longer (their PTS stretched) and audio frames are
//! repeated.

use crate::float;

/// Play rates offered by [`next_rate`]: slow motion, normal, fast.
pub const RATES: &[f64] = &[0.25, 0.5, 1.0, 1.5, 2.0, 4.0, 8.0, 16.0];

/// The next rate of [`RATES`] above (`up`) or below `rate`, if any.
pub fn next_rate(rate: f64, up: bool) -> Option<f64> {
    if up {
        RATES.iter().copied().find(|&r| r > rate + 1e-9)
    } else {
        RATES.iter().rev().copied().find(|&r| r < rate - 1e-9)
    }
}

/// One linear piece: content time `c0` is shown at mux time `o0`, and
/// content advances `rate` times faster than mux time from there.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Seg {
    pub c0: i64,
    pub o0: i64,
    pub rate: f64,
}

impl Seg {
    fn map(&self, c: i64) -> i64 {
        if self.rate == 1.0 {
            self.o0 + (c - self.c0)
        } else {
            self.o0 + float::to_i64(((c - self.c0) as f64 / self.rate).round())
        }
    }

    fn inverse(&self, o: i64) -> i64 {
        if self.rate == 1.0 {
            self.c0 + (o - self.o0)
        } else {
            self.c0 + float::to_i64(((o - self.o0) as f64 * self.rate).round())
        }
    }
}

/// Content time → mux time.
#[derive(Debug, Clone)]
pub(crate) struct Timeline {
    segs: Vec<Seg>,
}

impl Timeline {
    /// Rate 1 with content `c0` at mux time `o0`.
    pub fn new(c0: i64, o0: i64) -> Self {
        Self {
            segs: vec![Seg { c0, o0, rate: 1.0 }],
        }
    }

    fn seg(&self, c: i64) -> &Seg {
        let k = self.segs.partition_point(|s| s.c0 <= c);
        &self.segs[k.saturating_sub(1)]
    }

    /// Mux time of content time `c`.
    pub fn map(&self, c: i64) -> i64 {
        self.seg(c).map(c)
    }

    /// Rate in force at content time `c`.
    pub fn rate_at(&self, c: i64) -> f64 {
        self.seg(c).rate
    }

    /// The last segment.
    pub fn last(&self) -> Seg {
        // `new` makes one segment and no method removes the last one
        *self
            .segs
            .last()
            .expect("a Timeline keeps at least one segment")
    }

    /// Content time shown at mux time `o` (by the segment in force then).
    pub fn inverse(&self, o: i64) -> i64 {
        let k = self.segs.partition_point(|s| s.o0 <= o);
        self.segs[k.saturating_sub(1)].inverse(o)
    }

    /// From content time `c` on, show everything `delay` ticks later and,
    /// with `rate`, at that rate: a segment starts at `c` and the segments
    /// after it (seek anchors) move by `delay` and take the rate.
    pub fn change(&mut self, c: i64, delay: i64, rate: Option<f64>) {
        let at = self.map(c) + delay;
        let r = rate.unwrap_or(self.seg(c).rate);
        for s in self.segs.iter_mut().filter(|s| s.c0 > c) {
            s.o0 += delay;
            if let Some(r) = rate {
                s.rate = r;
            }
        }
        let k = self.segs.partition_point(|s| s.c0 <= c);
        if k > 0 && self.segs[k - 1].c0 == c {
            self.segs[k - 1] = Seg {
                c0: c,
                o0: at,
                rate: r,
            };
        } else {
            self.segs.insert(
                k,
                Seg {
                    c0: c,
                    o0: at,
                    rate: r,
                },
            );
        }
        if self.segs.len() > 64 {
            self.segs.drain(..self.segs.len() - 32);
        }
    }

    /// Continue with `seg` from its content time on (earlier segments that
    /// start at or after it are replaced). Segments that no unit can reach
    /// any more are dropped when there are many.
    pub fn push(&mut self, seg: Seg) {
        while self.segs.len() > 1 && self.segs.last().is_some_and(|s| s.c0 >= seg.c0) {
            self.segs.pop();
        }
        if self.segs.len() == 1 && self.segs[0].c0 >= seg.c0 {
            self.segs[0] = seg;
        } else {
            self.segs.push(seg);
        }
        if self.segs.len() > 64 {
            self.segs.drain(..self.segs.len() - 32);
        }
    }
}

/// Which video pictures to send at a rate above 1: the kept pictures'
/// bytes stay within the source's bytes divided by the rate (the source's
/// own rate on air), and a picture is kept only when the pictures it
/// predicts from were (MPEG-2: P from the previous I/P, B from the two
/// surrounding ones, in decoding order).
#[derive(Debug, Clone, Default)]
pub(crate) struct VideoThinning {
    /// Bytes that may still be sent (negative: debt).
    balance: f64,
    /// Largest picture seen: the balance never exceeds [`BALANCE_PICTURES`]
    /// of it.
    cap: f64,
    /// Kept flags of the last two reference pictures (older, newer).
    refs: [bool; 2],
}

impl VideoThinning {
    /// Forget the balance (a new rate or a seek).
    pub fn reset(&mut self) {
        self.balance = 0.0;
        self.refs = [false, false];
    }

    /// Whether to keep a picture of `bytes` bytes and `picture_coding_type`
    /// (1 I, 2 P, 3 B; `None` unknown) at `rate`; `allowed`: false when the
    /// picture would be shown within a frame period of a kept one (above
    /// rate 1 only). Updates the state as if the decision is carried out.
    pub fn decide(&mut self, kind: Option<u8>, bytes: usize, rate: f64, allowed: bool) -> bool {
        let b = bytes as f64;
        self.cap = self.cap.max(b);
        if rate <= 1.0 {
            self.balance = 0.0;
            if matches!(kind, Some(1 | 2)) {
                self.refs = [self.refs[1], true];
            }
            return true;
        }
        self.balance = (self.balance + BUDGET_SHARE * b / rate).min(BALANCE_PICTURES * self.cap);
        let keep = allowed
            && match kind {
                Some(1) => self.balance >= 0.0,
                Some(2) | None => self.refs[1] && self.balance >= b,
                Some(_) => self.refs[0] && self.refs[1] && self.balance >= 2.0 * b,
            };
        if keep {
            self.balance -= b;
        }
        if matches!(kind, Some(1 | 2) | None) {
            self.refs = [self.refs[1], keep];
        }
        keep
    }
}

/// Share of the source's bytes (divided by the rate) that fast play sends:
/// the rest is headroom for the bursts of the kept pictures, which keep
/// their source arrival→decoding lead but come closer together.
const BUDGET_SHARE: f64 = 0.85;

/// The saved-up budget of [`VideoThinning`] is capped at this many of
/// the largest picture: enough to keep the references of a GOP after
/// skipped pictures, little enough that a rate change cannot burst.
const BALANCE_PICTURES: f64 = 4.0;

/// What to do with an audio frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Slot {
    /// Not sent (the content it covers is skipped).
    Skip,
    /// Sent with presentation time `pts` (mux 27 MHz); `again`: it is due
    /// once more afterwards (slow motion).
    Emit { pts: i64, again: bool },
}

/// Output audio frame periods: each period shows the source frame whose
/// mapped span contains the period's start (within a quarter frame or half
/// the span), so
/// frames are skipped above rate 1 and repeated below, and the output
/// stays contiguous.
#[derive(Debug, Clone, Default)]
pub(crate) struct AudioSlots {
    next: Option<i64>,
}

impl AudioSlots {
    /// The slot decision for a frame of `dur` ticks whose content span maps
    /// to `[m0, m1)` (not committed).
    pub fn peek(&self, m0: i64, m1: i64, dur: i64) -> Slot {
        let tol = (dur / 4).min((m1 - m0) / 2);
        let mut next = self.next.unwrap_or(m0);
        if next < m0 - dur || next > m1 + 4 * dur {
            next = m0; // a gap (pause, seek, source discontinuity)
        }
        if next >= m1 - tol {
            return Slot::Skip;
        }
        Slot::Emit {
            pts: next,
            again: next + dur < m1 - tol,
        }
    }

    /// The frame was sent at `pts`.
    pub fn commit(&mut self, pts: i64, dur: i64) {
        self.next = Some(pts + dur);
    }
}

/// Playback state of a relay, for status lines.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlaybackStatus {
    /// Position shown on air, seconds from the start of the file.
    pub position_s: f64,
    /// Length of the file (seconds of PCR).
    pub duration_s: f64,
    /// Play rate.
    pub rate: f64,
    /// Paused.
    pub paused: bool,
}

impl PlaybackStatus {
    /// `▶ 12:34.5 / 30:02.0 ×2` style text.
    pub fn describe(&self) -> String {
        let mmss = |s: f64| {
            let s = s.max(0.0);
            format!("{:02}:{:04.1}", float::to_u64(s / 60.0), s % 60.0)
        };
        format!(
            "{} {} / {}{}",
            if self.paused { "paused" } else { "playing" },
            mmss(self.position_s),
            mmss(self.duration_s),
            if self.rate == 1.0 {
                String::new()
            } else {
                format!(" at {}×", self.rate)
            }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pcrclock::HZ;

    const S: i64 = HZ as i64;

    #[test]
    fn rates_step_through_the_ladder() {
        assert_eq!(next_rate(1.0, true), Some(1.5));
        assert_eq!(next_rate(1.0, false), Some(0.5));
        assert_eq!(next_rate(16.0, true), None);
        assert_eq!(next_rate(0.25, false), None);
    }

    #[test]
    fn timeline_is_identity_until_changed_and_monotonic() {
        let mut t = Timeline::new(0, 1000);
        assert_eq!(t.map(5 * S), 5 * S + 1000);
        // from content 10 s on, twice as fast
        t.push(Seg {
            c0: 10 * S,
            o0: t.map(10 * S),
            rate: 2.0,
        });
        assert_eq!(t.map(9 * S), 9 * S + 1000); // already-sent content keeps its time
        assert_eq!(t.map(14 * S), 10 * S + 1000 + 2 * S);
        assert_eq!(t.rate_at(14 * S), 2.0);
        assert_eq!(t.inverse(12 * S + 1000), 14 * S);
        // a pause of 3 s at content 20 s
        let o = t.map(20 * S);
        t.push(Seg {
            c0: 20 * S,
            o0: o + 3 * S,
            rate: 2.0,
        });
        assert_eq!(t.map(22 * S), o + 4 * S);
        let mut last = i64::MIN;
        for c in (0..30).map(|s| s * S) {
            assert!(t.map(c) > last);
            last = t.map(c);
        }
    }

    #[test]
    fn thinning_keeps_all_at_normal_rate_and_budget_above() {
        // GOP of 15 in decoding order: I P B B P B B …; I 100, P 50, B 20
        let gop: Vec<(u8, usize)> = std::iter::once((1, 100))
            .chain((0..4).flat_map(|_| [(2, 50), (3, 20), (3, 20)]))
            .chain([(3, 20), (3, 20)])
            .collect();
        let bytes: usize = gop.iter().map(|g| g.1).sum();
        for rate in [0.5, 1.0, 2.0, 4.0, 16.0] {
            let mut v = VideoThinning::default();
            let (mut kept, mut kept_i, mut total) = (0usize, 0, 0usize);
            for _ in 0..200 {
                for &(k, b) in &gop {
                    total += b;
                    if v.decide(Some(k), b, rate, true) {
                        kept += b;
                        kept_i += usize::from(k == 1);
                    }
                }
            }
            if rate <= 1.0 {
                assert_eq!(kept, total);
            } else {
                // within the budget (plus one picture)
                let budget = BUDGET_SHARE * total as f64 / rate;
                assert!(kept as f64 <= budget + 100.0, "{rate}");
                // and not far below it
                assert!(kept as f64 >= budget * 0.6, "{rate}: {kept}");
            }
            if rate <= 4.0 {
                assert_eq!(kept_i, 200, "every I kept at {rate}");
            }
            let _ = bytes;
        }
    }

    #[test]
    fn thinning_respects_references() {
        // a dropped P makes the following P and B undecodable
        let mut v = VideoThinning {
            refs: [true, true],
            ..Default::default()
        };
        assert!(!v.decide(Some(2), 1000, 2.0, true)); // no budget: dropped
        v.balance = 1e9;
        v.cap = 1e9;
        assert!(!v.decide(Some(3), 10, 2.0, true)); // B after a dropped ref
        assert!(!v.decide(Some(2), 10, 2.0, true)); // P after a dropped ref
        assert!(v.decide(Some(1), 10, 2.0, true)); // I starts over
        assert!(!v.decide(Some(2), 10, 2.0, false)); // too close to a kept one
        assert!(!v.decide(Some(3), 10, 2.0, true)); // and what depends on it
        assert!(v.decide(Some(1), 10, 2.0, true));
        assert!(v.decide(Some(2), 10, 2.0, true));
        assert!(v.decide(Some(3), 10, 2.0, true));
    }

    #[test]
    fn audio_slots_skip_and_repeat() {
        let dur = 576_000; // 1024 samples at 48 kHz
        for (rate, want) in [(1.0, 1.0), (2.0, 0.5), (4.0, 0.25), (0.5, 2.0)] {
            let mut a = AudioSlots::default();
            let tl = Timeline {
                segs: vec![Seg { c0: 0, o0: 0, rate }],
            };
            let mut out = Vec::new();
            for f in 0..400i64 {
                let c = f * dur;
                loop {
                    match a.peek(tl.map(c), tl.map(c + dur), dur) {
                        Slot::Skip => break,
                        Slot::Emit { pts, again } => {
                            a.commit(pts, dur);
                            out.push(pts);
                            if !again {
                                break;
                            }
                        }
                    }
                }
            }
            // contiguous output, one frame per period
            assert!(out.windows(2).all(|w| w[1] - w[0] == dur), "{rate}");
            let ratio = out.len() as f64 / 400.0;
            assert!((ratio - want).abs() < 0.01, "{rate}: {ratio}");
        }
    }

    #[test]
    fn audio_slots_follow_a_pause() {
        let dur = 576_000;
        let mut a = AudioSlots::default();
        a.commit(0, dur);
        // the next frame now maps 2 s later: the output jumps with it
        assert_eq!(
            a.peek(2 * S, 2 * S + dur, dur),
            Slot::Emit {
                pts: 2 * S,
                again: false
            }
        );
    }
}

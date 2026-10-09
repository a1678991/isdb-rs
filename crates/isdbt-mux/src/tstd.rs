//! Fluid model of the Transport Stream system target decoder buffers of one
//! elementary stream (H.222.0 §2.4.2.3–§2.4.2.6).
//!
//! * TBn: 512 bytes, every packet of the PID enters it, emptied at Rxn.
//! * Video: MBn (leak method, since B32 Part 1 §5.1.1 requires vbv_delay
//!   0xFFFF) drained into EBn at Rbxn while EBn is not full; EBn of size
//!   vbv_buffer_size. Audio: TBn → Bn.
//! * Access units are removed from EBn/Bn instantaneously at their decoding
//!   time; all their bytes must be there (no underflow).
//!
//! Simplification (conservative): PES header bytes are counted in MBn/EBn
//! and Bn together with the access unit they precede, and removed with it.
//! Used both by the scheduler (to decide whether a packet may be sent) and
//! by the output analyser.

use std::collections::VecDeque;

/// System clock (H.222.0 §2.4.2.1), ticks per second.
const HZ: f64 = 27_000_000.0;
/// TBSn: "The transport buffer size is fixed at 512 bytes" (§2.4.2.3).
pub const TBS: f64 = 512.0;
const EPS: f64 = 1e-6;
/// Longest Euler step of the fluid model (27 MHz ticks, 0.5 ms).
const MAX_STEP: u64 = 13_500;

/// Buffer sizes and rates of one elementary stream.
#[derive(Debug, Clone, Copy)]
pub struct TstdParams {
    /// Rxn, bit/s.
    pub rx: f64,
    /// (MBSn bytes, Rbxn bit/s) for video, `None` for audio.
    pub mb: Option<(f64, f64)>,
    /// EBSn (video) or BSn (audio), bytes.
    pub main: f64,
}

/// Counters of rule violations seen by the model.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct TstdViolations {
    /// TBn overflow (> 512 bytes).
    pub tb_overflow: u64,
    /// MBn/Bn overflow.
    pub buffer_overflow: u64,
    /// Access units not completely in EBn/Bn at their decoding time.
    pub underflow: u64,
    /// Longest interval without TBn being empty (ticks); §2.4.2.6 requires ≤ 1 s.
    pub tb_max_nonempty: u64,
    /// Longest interval without MBn being empty (ticks); ≤ 1 s with the leak method.
    pub mb_max_nonempty: u64,
}

/// Model state.
#[derive(Debug, Clone)]
pub struct Tstd {
    p: TstdParams,
    t: u64,
    tb: f64,
    tb_payload: f64,
    mb: f64,
    main: f64,
    delivered: f64,
    pending: VecDeque<(u64, f64, f64)>, // (decode time, bytes, cumulative end)
    cum_end: f64,
    tb_last_empty: u64,
    mb_last_empty: u64,
    /// Violations so far.
    pub violations: TstdViolations,
}

impl Tstd {
    /// Empty buffers at time `t0` (27 MHz ticks).
    pub fn new(p: TstdParams, t0: u64) -> Self {
        Self {
            p,
            t: t0,
            tb: 0.0,
            tb_payload: 0.0,
            mb: 0.0,
            main: 0.0,
            delivered: 0.0,
            pending: VecDeque::new(),
            cum_end: 0.0,
            tb_last_empty: t0,
            mb_last_empty: t0,
            violations: TstdViolations::default(),
        }
    }

    /// Parameters.
    pub fn params(&self) -> &TstdParams {
        &self.p
    }

    fn flow(&mut self, dt: u64) {
        let secs = dt as f64 / HZ;
        if self.tb > EPS {
            let d = (self.p.rx / 8.0 * secs).min(self.tb);
            let pay = if self.tb > 0.0 {
                d * self.tb_payload / self.tb
            } else {
                0.0
            };
            self.tb -= d;
            self.tb_payload = (self.tb_payload - pay).max(0.0);
            if self.tb <= EPS {
                self.tb = 0.0;
                self.tb_payload = 0.0;
            }
            match self.p.mb {
                Some(_) => self.mb += pay,
                None => {
                    self.main += pay;
                    self.delivered += pay;
                }
            }
        }
        if let Some((_, rbx)) = self.p.mb
            && self.mb > EPS
        {
            let room = (self.p.main - self.main).max(0.0);
            let d = (rbx / 8.0 * secs).min(self.mb).min(room);
            self.mb -= d;
            self.main += d;
            self.delivered += d;
            if self.mb <= EPS {
                self.mb = 0.0;
            }
        }
    }

    fn track_empty(&mut self) {
        if self.tb <= EPS {
            self.tb_last_empty = self.t;
        }
        if self.mb <= EPS {
            self.mb_last_empty = self.t;
        }
        let v = &mut self.violations;
        v.tb_max_nonempty = v.tb_max_nonempty.max(self.t - self.tb_last_empty);
        v.mb_max_nonempty = v.mb_max_nonempty.max(self.t - self.mb_last_empty);
    }

    /// Advance the model to time `t`, removing access units whose decoding
    /// time has passed.
    pub fn advance(&mut self, t: u64) {
        while self.t < t {
            let next_au = self.pending.front().map(|a| a.0).filter(|&d| d <= t);
            let target = next_au.unwrap_or(t).max(self.t);
            let step_end = target.min(self.t + MAX_STEP);
            self.flow(step_end - self.t);
            self.t = step_end;
            self.track_empty();
            if Some(self.t) == next_au {
                self.remove_due();
            }
        }
        if self.pending.front().is_some_and(|a| a.0 <= self.t) {
            self.remove_due();
        }
    }

    fn remove_due(&mut self) {
        while let Some(&(d, bytes, end)) = self.pending.front() {
            if d > self.t {
                break;
            }
            // Bytes are whole: half a byte absorbs the rounding the f64
            // running sums accumulate over hours (≈ 0.01 byte after 4 GB).
            if self.delivered + 0.5 < end {
                self.violations.underflow += 1;
            }
            self.main = (self.main - bytes).max(0.0);
            self.pending.pop_front();
        }
    }

    /// Register the next access unit (PES packet of `bytes`) decoded at
    /// `decode_time`; call before its first byte is sent.
    pub fn add_au(&mut self, decode_time: u64, bytes: usize) {
        self.cum_end += bytes as f64;
        self.pending
            .push_back((decode_time, bytes as f64, self.cum_end));
    }

    /// Whether a packet carrying `payload` PES bytes may enter at time `t`
    /// without overflowing TBn or the next buffer.
    pub fn can_accept(&mut self, t: u64, payload: usize) -> bool {
        self.advance(t);
        if self.tb + 188.0 > TBS + EPS {
            return false;
        }
        let after = self.tb_payload + payload as f64;
        // Leak method: "MBn … shall empty at least once every second"
        // (§2.4.2.6). If MBn has been occupied for half a second, pause this
        // PID until it has drained (at most MBSn/Rbxn, a few tens of ms).
        if self.p.mb.is_some()
            && (self.mb + self.tb_payload) > EPS
            && self.t - self.mb_last_empty > HZ as u64 / 2
        {
            return false;
        }
        match self.p.mb {
            // Video: besides MBn not overflowing, keep MBn + EBn within EBSn so
            // that MBn always drains completely (§2.4.2.6 leak method: "MBn …
            // shall empty at least once every second").
            Some((mbs, _)) => {
                self.mb + after <= mbs + EPS && self.main + self.mb + after <= self.p.main + EPS
            }
            None => self.main + after <= self.p.main + EPS,
        }
    }

    /// Whether a packet may enter TBn at time `t` without overflowing it
    /// (TBn only; the main buffer is not checked).
    pub fn tb_can_accept(&mut self, t: u64) -> bool {
        self.advance(t);
        self.tb + 188.0 <= TBS + EPS
    }

    /// A packet carrying `payload` PES bytes enters at time `t`.
    pub fn push(&mut self, t: u64, payload: usize) {
        self.advance(t);
        self.tb += 188.0;
        self.tb_payload += payload as f64;
        if self.tb > TBS + EPS {
            self.violations.tb_overflow += 1;
        }
        let next = match self.p.mb {
            Some((mbs, _)) => (self.mb + self.tb_payload, mbs),
            None => (self.main + self.tb_payload, self.p.main),
        };
        if next.0 > next.1 + EPS {
            self.violations.buffer_overflow += 1;
        }
    }

    /// Bytes in MBn + EBn (video) or Bn (audio).
    pub fn occupancy(&self) -> f64 {
        self.mb + self.main + self.tb_payload
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tb_limits_bursts() {
        let p = TstdParams {
            rx: 2_000_000.0,
            mb: None,
            main: 3584.0,
        };
        let mut m = Tstd::new(p, 0);
        assert!(m.can_accept(0, 184));
        m.push(0, 184);
        assert!(m.can_accept(0, 184));
        m.push(0, 184);
        assert!(!m.can_accept(0, 184)); // 3 × 188 > 512
        // 2 Mbit/s drains 188 bytes in 752 µs = 20 304 ticks
        assert!(m.can_accept(20_400, 184));
    }

    #[test]
    fn underflow_detected() {
        let p = TstdParams {
            rx: 2_000_000.0,
            mb: None,
            main: 3584.0,
        };
        let mut m = Tstd::new(p, 0);
        m.add_au(27_000, 368);
        m.push(0, 184);
        m.advance(30_000);
        assert_eq!(m.violations.underflow, 1);
        let mut m = Tstd::new(p, 0);
        m.add_au(270_000, 368);
        m.push(0, 184);
        m.push(100, 184);
        m.advance(300_000);
        assert_eq!(m.violations.underflow, 0);
        assert!(m.occupancy() < 1.0);
    }
}

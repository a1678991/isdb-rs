//! Delay adjustment for byte interleaving (ARIB STD-B31 §3.6, Table 3-7).
//!
//! > "An appropriate adjustment value must be selected and specified for
//! > each hierarchical layer from among those (equivalent to the number of
//! > transmission TSPs) shown below, such that all delays, including
//! > transmission and reception delays caused by byte interleaving (11
//! > transmission TSPs), are one frame in length." — §3.6
//!
//! The adjustment is a pure FIFO delay of `T − 11` transmission-TSP slots
//! of the layer's own byte stream (T = TSPs of the layer per frame, Table
//! 3-3), applied on the transmitting side only, after energy dispersal and
//! before byte interleaving (Fig. 3-2 block order). The value comes from
//! [`isdbt_core::LayerConfig::delay_adjustment_tsps`] (Table 3-7).

use std::collections::VecDeque;

use isdbt_core::TRANSMISSION_TSP;

/// Byte FIFO delaying a layer stream by a whole number of 204-byte slots
/// (§3.6). Initial contents are filler slots (zero bytes with 0x47 at the
/// sync offset 203 of the §3.4 shifted slot), so that sync positions stay
/// sync bytes from the very first output byte. `LayerEncoder` overwrites
/// this filler by priming with a frame of dispersed null TSPs.
#[derive(Debug, Clone)]
pub struct DelayLine {
    fifo: VecDeque<u8>,
}

impl DelayLine {
    /// A delay of `slots` transmission TSPs (Table 3-7 value).
    pub fn new(slots: usize) -> Self {
        let mut fifo = VecDeque::with_capacity((slots + 1) * TRANSMISSION_TSP);
        for _ in 0..slots {
            fifo.extend(std::iter::repeat_n(0u8, TRANSMISSION_TSP - 1));
            fifo.push_back(0x47);
        }
        Self { fifo }
    }

    /// Delay in bytes.
    pub fn delay_bytes(&self) -> usize {
        self.fifo.len()
    }

    /// Push `data` through the delay in place.
    pub fn process(&mut self, data: &mut [u8]) {
        if self.fifo.is_empty() {
            return;
        }
        for b in data.iter_mut() {
            self.fifo.push_back(*b);
            *b = self.fifo.pop_front().expect("non-empty");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delay_is_exact_slots() {
        let mut d = DelayLine::new(3);
        assert_eq!(d.delay_bytes(), 3 * 204);
        let mut v: Vec<u8> = (0..2000u32).map(|i| (i % 251) as u8).collect();
        let orig = v.clone();
        d.process(&mut v);
        assert_eq!(&v[612..], &orig[..2000 - 612]);
        assert_eq!(v[203], 0x47);
        assert_eq!(v[0], 0);
    }
}

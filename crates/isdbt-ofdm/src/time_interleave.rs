//! Time interleaving (ARIB STD-B31 §3.11.1, Figs. 3-22/3-23, Table 3-12).
//!
//! Operates on combined OFDM symbols in data-segment order (`13·nc`
//! values, see [`crate::combine`]).  Carrier `i` (0..nc) of every data
//! segment of a layer with interleave length `I` is delayed on the
//! transmit side by `A + I·m_i` OFDM symbols, where `m_i = (5·i) mod 96`
//! (Fig. 3-23; `i` spans the whole data segment, 0..nc−1, in every mode)
//! and `A` is the delay-adjustment value of Table 3-12 (applied "prior to
//! time interleaving"; merged into the same delay line).  The receiver
//! delays carrier `i` by `I·(95 − m_i)`, so every carrier sees
//! `A + 95·I` = (Table 3-12 frames) × 204 symbols in total.

use crate::delay::MultiDelay;
use isdbt_core::Params;
use isdbt_core::ofdm_params::{TIME_INTERLEAVE_M_MAX, time_interleave_m};

fn layer_length_and_adjust(params: &Params, seg: usize) -> (usize, usize) {
    let ti = params.segment_layer(seg).1.time_interleave;
    (
        ti.length(params.mode()),
        ti.adjustment_symbols(params.mode()),
    )
}

/// Per-carrier transmit delays (data-segment order).
pub fn tx_delays(params: &Params) -> Vec<usize> {
    let nc = params.mode().data_carriers_per_segment();
    let mut d = Vec::with_capacity(13 * nc);
    for seg in 0..13 {
        let (len, adj) = layer_length_and_adjust(params, seg);
        for i in 0..nc {
            d.push(adj + len * time_interleave_m(i));
        }
    }
    d
}

/// Per-carrier receive delays (data-segment order).
pub fn rx_delays(params: &Params) -> Vec<usize> {
    let nc = params.mode().data_carriers_per_segment();
    let mut d = Vec::with_capacity(13 * nc);
    for seg in 0..13 {
        let (len, _) = layer_length_and_adjust(params, seg);
        for i in 0..nc {
            d.push(len * (TIME_INTERLEAVE_M_MAX - time_interleave_m(i)));
        }
    }
    d
}

/// Transmit time interleaver (with Table 3-12 delay adjustment).
#[derive(Debug, Clone)]
pub struct TimeInterleaver<T> {
    lines: MultiDelay<T>,
    tmp: Vec<T>,
}

impl<T: Copy> TimeInterleaver<T> {
    /// Interleaver for `params`; lines start filled with `fill`.
    pub fn new(params: &Params, fill: T) -> Self {
        let d = tx_delays(params);
        let n = d.len();
        Self {
            lines: MultiDelay::new(d, fill),
            tmp: vec![fill; n],
        }
    }

    /// Interleaver whose lines for data segment `seg` start filled with
    /// `fill(seg)`.
    pub fn with_fill(params: &Params, fill: impl Fn(usize) -> T) -> Self {
        let d = tx_delays(params);
        let nc = params.mode().data_carriers_per_segment();
        let fills: Vec<T> = (0..d.len()).map(|q| fill(q / nc)).collect();
        Self {
            lines: MultiDelay::with_fills(d, &fills),
            tmp: fills,
        }
    }

    /// Interleaver with explicit initial line contents: `init(q, delay)`
    /// returns the `delay` values output by position `q` (data-segment
    /// order) before its first input emerges.
    pub fn with_initial(params: &Params, mut init: impl FnMut(usize, usize) -> Vec<T>) -> Self {
        let d = tx_delays(params);
        let contents: Vec<Vec<T>> = d.iter().enumerate().map(|(q, &dq)| init(q, dq)).collect();
        // Scratch is sized on the first `process` call.
        Self {
            lines: MultiDelay::with_contents(d, contents),
            tmp: Vec::new(),
        }
    }

    /// Process one combined OFDM symbol in place.
    pub fn process(&mut self, symbol: &mut [T]) {
        if self.tmp.len() != symbol.len() {
            self.tmp = symbol.to_vec();
        }
        self.tmp.copy_from_slice(symbol);
        self.lines.push(&self.tmp, symbol);
    }
}

/// Receive time deinterleaver.
#[derive(Debug, Clone)]
pub struct TimeDeinterleaver<T> {
    lines: MultiDelay<T>,
    tmp: Vec<T>,
}

impl<T: Copy> TimeDeinterleaver<T> {
    /// Deinterleaver for `params`; lines start filled with `fill`.
    pub fn new(params: &Params, fill: T) -> Self {
        let d = rx_delays(params);
        let n = d.len();
        Self {
            lines: MultiDelay::new(d, fill),
            tmp: vec![fill; n],
        }
    }

    /// Process one combined OFDM symbol in place.
    pub fn process(&mut self, symbol: &mut [T]) {
        self.tmp.copy_from_slice(symbol);
        self.lines.push(&self.tmp, symbol);
    }
}

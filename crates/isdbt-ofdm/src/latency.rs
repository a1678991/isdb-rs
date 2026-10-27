//! Frame-alignment contract between `isdbt-fec` and this crate.
//!
//! **Input** (per OFDM frame, per active layer): `nc·S·N·204` coded bits
//! (0/1), in the order they enter the carrier modulator of §3.9 (bit 0
//! first).  Bit `j` of the frame belongs to OFDM symbol `j / (nc·S·N)` of
//! the layer stream and is bit `j mod S` (b0 first) of carrier symbol
//! `(j / S) mod (nc·N)`.
//!
//! **Frame alignment (ARIB STD-B31 Part 2, Figs. 3-8/3-9).**  Fig. 3-8
//! (transmitting side), "Combining of hierarchical layers": "(Formation of
//! OFDM symbol by 1-frame buffer; S-1,B,n is a symbol made up of B'''-2,12
//! to B'''-1,11.) (Two-symbol delay is caused by bit interleaving.
//! Therefore, frame synchronization is delayed by two symbols.)" — the OFDM
//! frame formation starts at bit-interleaver output symbol S-1,A,3.
//! Fig. 3-9 (receiving side): after time de-interleaving and "Bit
//! de-interleaving (start bit delayed by 120 bits)" the first symbol of the
//! OFDM frame is S''n,A,1.  The [`Modulator`](crate::Modulator) therefore
//! advances each layer's bit-interleaved carrier-symbol stream by the 2 OFDM
//! symbols of total bit-interleaving delay (§3.9.2, Table 3-9) through a
//! one-frame buffer, so the TX + RX bit-interleaving delay nets to zero
//! relative to the OFDM frame:
//!
//! * on-air frames are numbered like data frames; the modulator emits
//!   on-air frame `k` in the call that receives input frame `k + 1`
//!   ([`MODULATOR_BUFFER_FRAMES`] = 1; the first emitted frame is on-air
//!   frame −1);
//! * after the receiver's time and bit deinterleaving, symbol `s` (0..203)
//!   of on-air frame `m` carries symbol `s` of input frame `m − F_l`, where
//!   `F_l` = Table 3-12 "number of delayed frames" of layer `l` (time
//!   interleaving incl. its delay adjustment) — e.g. bit 0 of input frame
//!   `k` is b0 of the first carrier symbol of on-air frame `k + F_l`,
//!   symbol 0;
//! * mapping/DQPSK, combining, frequency interleaving, framing and OFDM
//!   add no delay.
//!
//! `F_l` may differ between layers, so the
//! [`Demodulator`](crate::Demodulator) adds a receive FIFO of
//! `204·(L − F_l)` OFDM symbols per layer, **L = [`latency_frames`] =
//! max_l F_l**:
//!
//! > the LLRs yielded with on-air frame `m` are, bit for bit and in the
//! > same order, the LLRs of input frame `m − L` for every layer.
//!
//! Counted in modulator calls the end-to-end delay is
//! [`end_to_end_frames`] = `1 + L`: the LLRs yielded with the `e`-th
//! emitted frame (e = 0 first) belong to the input of call `e − 1 − L`.
//!
//! The modulator's delay lines start with a deterministic pseudo-random fill
//! (mapper bits; constellation points in the time-interleaver lines and the
//! one-frame buffer, a π/4-shift random walk for DQPSK), so the first
//! emitted frames carry fill data.
//! A receiver that locks at on-air frame `s` yields valid LLRs from on-air
//! frame `s + L + 1` on ([`DemodFrame::valid`](crate::DemodFrame)); the
//! extra frame covers the up to 120 carrier symbols that the bit
//! deinterleaver takes from the preceding frame.

use isdbt_core::ofdm_params::time_interleave_delayed_frames;
use isdbt_core::{Layer, Params, SYMBOLS_PER_FRAME};

/// Table 3-12 delayed frames `F_l` of `layer` (0 for unused layers).
pub fn ti_frames(params: &Params, layer: Layer) -> usize {
    time_interleave_delayed_frames(params.mode(), params.time_interleave_length(layer)).unwrap_or(0)
}

/// Frames held by the modulator for the bit-interleave frame alignment
/// (Fig. 3-8 "1-frame buffer").
pub const MODULATOR_BUFFER_FRAMES: usize = 1;

/// Latency `L` in on-air frames from modulator input frame to LLR output,
/// the same for all layers: `max_l F_l` (Table 3-12).
pub fn latency_frames(params: &Params) -> usize {
    params
        .active_layers()
        .map(|(id, _)| ti_frames(params, id))
        .max()
        .unwrap_or(0)
}

/// End-to-end delay in modulator calls: [`MODULATOR_BUFFER_FRAMES`] + L.
pub fn end_to_end_frames(params: &Params) -> usize {
    MODULATOR_BUFFER_FRAMES + latency_frames(params)
}

/// Receive-side alignment delay of `layer` in OFDM symbols: `204·(L − F_l)`.
pub fn rx_alignment_symbols(params: &Params, layer: Layer) -> usize {
    SYMBOLS_PER_FRAME * (latency_frames(params) - ti_frames(params, layer))
}

/// Coded bits per OFDM frame of `layer` (`nc·S·N·204`; 0 if unused).
pub fn bits_per_frame(params: &Params, layer: Layer) -> usize {
    params.layers()[layer.index()].map_or(0, |l| {
        params.mode().data_carriers_per_segment()
            * l.modulation.bits_per_symbol()
            * l.segments.count()
            * SYMBOLS_PER_FRAME
    })
}

/// Coded bits per OFDM symbol of `layer` (`nc·S·N`).
pub fn bits_per_symbol(params: &Params, layer: Layer) -> usize {
    bits_per_frame(params, layer) / SYMBOLS_PER_FRAME
}

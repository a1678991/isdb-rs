//! TMCC decoding from derotated headers.
//!
//! The 192 header symbols are BPSK of the r = 1/2 coded header (W1, 8 TMCC
//! bytes, W2/W3), mother-code bits in the order X (171₈) then Y (133₈):
//! the order that reproduces the coded tails of annexed table 45 note 1
//! (table 49 note 3 calls the BPSK pair "C0, C1"; SPEC-NOTES "bpsk-order").
//! The encoder runs on through the main signal (the W words' first 6 bits
//! flush its memory, BO.1408-1 §6.3), so the Viterbi decoder starts in an
//! unknown state and its last bits rely on the W2/W3 word it already knows.

use crate::Cplx;
use isdb_fec::ViterbiDecoder;
use isdbs_core::frame::{HEADER_BYTES, HEADER_SYMBOLS, TMCC_BYTES_PER_FRAME, W1, W2, W3};

/// The 12 header bytes decoded from the 192 derotated header symbols of a
/// frame (soft BPSK: LLR ∝ Re z, scaled by `llr_scale`).
pub fn header_bytes(h: &[Cplx], llr_scale: f64) -> [u8; HEADER_BYTES] {
    assert!(h.len() >= HEADER_SYMBOLS);
    let llr: Vec<f32> = h[..HEADER_SYMBOLS]
        .iter()
        .map(|v| crate::soft(v.re * llr_scale))
        .collect();
    let mut v = ViterbiDecoder::new_unknown_state(64);
    let mut bits = Vec::with_capacity(HEADER_SYMBOLS / 2);
    v.push(&llr, &mut bits);
    v.flush(&mut bits);
    let mut out = [0u8; HEADER_BYTES];
    for (i, b) in out.iter_mut().enumerate() {
        *b = bits[8 * i..8 * i + 8]
            .iter()
            .fold(0u8, |a, &x| (a << 1) | x);
    }
    out
}

/// Sync-word check of a decoded header: W1 at the start and which end
/// word (W2 → frame 1 of a superframe).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeaderCheck {
    /// W1 decoded intact.
    pub w1: bool,
    /// Ends in W2.
    pub w2: bool,
    /// Ends in W3.
    pub w3: bool,
}

/// Check the sync words of a decoded header.
pub fn check(h: &[u8; HEADER_BYTES]) -> HeaderCheck {
    let end = u16::from_be_bytes([h[10], h[11]]);
    HeaderCheck {
        w1: u16::from_be_bytes([h[0], h[1]]) == W1,
        w2: end == W2,
        w3: end == W3,
    }
}

/// The 8 TMCC bytes of a decoded header.
pub fn block(h: &[u8; HEADER_BYTES]) -> [u8; TMCC_BYTES_PER_FRAME] {
    h[2..10].try_into().expect("8 bytes")
}

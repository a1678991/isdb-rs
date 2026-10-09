//! Convolutional byte interleaving (ARIB STD-B31 §3.7, Fig. 3-9).
//!
//! > "Interleaving must be 12 bytes in depth. Note, however, that the byte
//! > next to the synchronization byte must pass through a reference path
//! > that causes no delay. … path 0 has no delay. The memory size for path 1
//! > must be 17 bytes, that for path 2 be 17 × 2 = 34 bytes, and so on.
//! > Input and output must be switched over to a different path every byte,
//! > in a sequential and cyclic manner, in ascending order of path number."
//! > — §3.7
//!
//! Both commutators start at path 0 on the first byte of each 204-byte
//! slot in the §3.4 shifted order (the byte after the sync byte). Because
//! 204 = 12 × 17, path *j* = byte offset mod 12 in every slot, and the
//! commutator never needs re-synchronising: a byte on path *j* leaves the
//! interleaver 17·*j* path-visits = 17·*j*·12 = 204·*j* bytes later (*j*
//! whole slots), and the deinterleaver delays path *j* by 17·(11 − *j*)
//! visits, so the pair delays every byte by 11 × 204 bytes (the "11
//! transmission TSPs" of §3.6).

use isdbt_core::TRANSMISSION_TSP;

/// Interleaving depth, number of paths (§3.7).
pub const DEPTH: usize = 12;
/// FIFO unit length M = 17 bytes (§3.7: "memory size for path 1 must be 17
/// bytes").
pub const UNIT: usize = 17;
/// End-to-end byte latency of interleaver + deinterleaver:
/// 17 × 11 × 12 = 2244 bytes = 11 transmission TSPs (§3.6, §3.7).
pub const PAIR_LATENCY_BYTES: usize = UNIT * (DEPTH - 1) * DEPTH;

const _: () = assert!(PAIR_LATENCY_BYTES == 11 * TRANSMISSION_TSP);

/// Path delay structure shared by interleaver and deinterleaver: path *j*
/// is a FIFO of `len(j)` bytes; a commutator visits paths 0..11 cyclically.
#[derive(Debug, Clone)]
struct Paths {
    buf: Vec<u8>,
    start: [usize; DEPTH],
    len: [usize; DEPTH],
    pos: [usize; DEPTH],
    path: usize,
}

impl Paths {
    fn new(len: [usize; DEPTH]) -> Self {
        let mut start = [0; DEPTH];
        let mut acc = 0;
        for j in 0..DEPTH {
            start[j] = acc;
            acc += len[j];
        }
        Self { buf: vec![0; acc], start, len, pos: [0; DEPTH], path: 0 }
    }

    fn process(&mut self, data: &mut [u8]) {
        for b in data.iter_mut() {
            let j = self.path;
            let l = self.len[j];
            if l != 0 {
                let idx = self.start[j] + self.pos[j];
                std::mem::swap(b, &mut self.buf[idx]);
                self.pos[j] = if self.pos[j] + 1 == l { 0 } else { self.pos[j] + 1 };
            }
            self.path = if j + 1 == DEPTH { 0 } else { j + 1 };
        }
    }
}

/// Byte interleaver of Fig. 3-9: path *j* holds 17·*j* bytes (§3.7).
/// Feed it whole layer frames (a multiple of 204 bytes, §3.4 shifted slot
/// order) so that path 0 meets the byte after each sync byte.
#[derive(Debug, Clone)]
pub struct ByteInterleaver(Paths);

impl Default for ByteInterleaver {
    fn default() -> Self {
        Self::new()
    }
}

impl ByteInterleaver {
    /// Empty (zero-filled) interleaver, commutator at path 0.
    pub fn new() -> Self {
        Self(Paths::new(std::array::from_fn(|j| UNIT * j)))
    }
    /// Interleave in place.
    pub fn process(&mut self, data: &mut [u8]) {
        self.0.process(data)
    }
}

/// Inverse of [`ByteInterleaver`]: path *j* holds 17·(11 − *j*) bytes, so
/// every byte sees 17 × 11 path visits in total (§3.7, receiver side).
#[derive(Debug, Clone)]
pub struct ByteDeinterleaver(Paths);

impl Default for ByteDeinterleaver {
    fn default() -> Self {
        Self::new()
    }
}

impl ByteDeinterleaver {
    /// Empty (zero-filled) deinterleaver, commutator at path 0.
    pub fn new() -> Self {
        Self(Paths::new(std::array::from_fn(|j| UNIT * (DEPTH - 1 - j))))
    }
    /// Deinterleave in place.
    pub fn process(&mut self, data: &mut [u8]) {
        self.0.process(data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// §3.7 / Fig. 3-9: a byte entering at stream position p (path p mod 12)
    /// leaves the interleaver 17·j·12 bytes later; path 0 is undelayed.
    #[test]
    fn spec_fig_3_9_path_delays() {
        for p in 0..DEPTH * 3 {
            let j = p % DEPTH;
            let n = p + UNIT * j * DEPTH + 1;
            let mut v = vec![0u8; n];
            v[p] = 0xAA;
            ByteInterleaver::new().process(&mut v);
            let out: Vec<usize> = v.iter().enumerate().filter(|(_, b)| **b == 0xAA).map(|(i, _)| i).collect();
            assert_eq!(out, vec![p + 204 * j], "path {j}");
        }
    }

    /// Interleaver memory sizes 0, 17, 34, …, 187 bytes (Fig. 3-9).
    #[test]
    fn spec_fig_3_9_memory_sizes() {
        let i = ByteInterleaver::new();
        assert_eq!(i.0.len, std::array::from_fn(|j| 17 * j));
        assert_eq!(i.0.buf.len(), 17 * 66);
    }

    /// Interleaver ∘ deinterleaver = delay of 11 × 204 bytes.
    #[test]
    fn pair_latency() {
        let mut v: Vec<u8> = (0..20 * 204u32).map(|i| (i * 7 + 3) as u8).collect();
        let orig = v.clone();
        let mut il = ByteInterleaver::new();
        let mut dl = ByteDeinterleaver::new();
        // feed in uneven chunks to check the commutator state carries over
        for c in v.chunks_mut(1000) {
            il.process(c);
        }
        for c in v.chunks_mut(333) {
            dl.process(c);
        }
        assert_eq!(&v[PAIR_LATENCY_BYTES..], &orig[..orig.len() - PAIR_LATENCY_BYTES]);
    }
}

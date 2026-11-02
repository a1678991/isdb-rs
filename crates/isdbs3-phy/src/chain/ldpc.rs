//! Stage 7, the LDPC decoder, as the chain calls it: a [`LdpcDecoder`]
//! given to [`super::Chain`], so the schedule (`isdb_fec::ldpc::fixed`'s
//! sequential one today, the layered one of the plan's contract later)
//! is the caller's choice. The chain itself implements no decoder.

use isdb_fec::ldpc::Code;
use isdb_fec::ldpc::fixed::{Decoder, LaneResult, Params};

/// Decodes `i8` LLR words (length n each, all of `code`); returns each
/// word's result and its n hard decisions, in input order. A word's
/// result must depend on its LLRs, the code and the decoder's settings
/// only (never on the other words of the call), as the HDL's queue
/// guarantees (plan, stage 7).
pub trait LdpcDecoder {
    /// Decode `words` of `code`.
    fn decode(&mut self, code: &Code, words: &[&[i8]]) -> Vec<(LaneResult, Vec<u8>)>;
}

impl<F: FnMut(&Code, &[&[i8]]) -> Vec<(LaneResult, Vec<u8>)>> LdpcDecoder for F {
    fn decode(&mut self, code: &Code, words: &[&[i8]]) -> Vec<(LaneResult, Vec<u8>)> {
        self(code, words)
    }
}

/// The CPU's fixed-point decoder (`isdb_fec::ldpc::fixed`, 16 lanes),
/// the words shared out between `threads` threads (the results do not
/// depend on the split: `decode_all`'s lane equivalence).
#[derive(Debug, Clone, Copy)]
pub struct FixedLdpc {
    /// The decoder's settings.
    pub params: Params,
    /// Threads (1: the calling thread only).
    pub threads: usize,
}

impl FixedLdpc {
    /// The settings `rx::receive` uses for the main signal
    /// (`main_signal::Ldpc::FIXED`: 50 iterations, offset 1), on all
    /// cores.
    pub fn new() -> Self {
        Self::with(50, 1)
    }

    /// `max_iter` iterations, offset `offset`, on all cores.
    pub fn with(max_iter: usize, offset: i8) -> Self {
        Self {
            // the one place the chain builds the decoder's settings
            params: Params::new(max_iter, offset),
            threads: std::thread::available_parallelism().map_or(4, std::num::NonZero::get),
        }
    }
}

impl Default for FixedLdpc {
    fn default() -> Self {
        Self::new()
    }
}

/// The FPGA decoder's default iteration limit (fpga `ldpc.DEFAULT_MAX_ITER`,
/// the chain's MAX_ITER register at reset): 14, so that its queue (S = 34,
/// three banks, 200 MHz) has no overrun on the measured distribution at
/// the chain's pace (at 16 it overruns 9 of 363 codewords).
pub const HDL_MAX_ITER: usize = 14;
/// The FPGA decoder's layered schedule step (S = 34 units).
pub const HDL_STEP: usize = 34;
/// The FPGA decoder's min-sum offset.
pub const HDL_OFFSET: i8 = 1;

impl FixedLdpc {
    /// The FPGA decoder's default settings: `Schedule::Layered { step:
    /// HDL_STEP }`, [`HDL_MAX_ITER`] iterations, offset [`HDL_OFFSET`]; an
    /// HDL chain with the real decoder matches the model with these bit
    /// for bit.
    pub fn hdl() -> Self {
        let mut l = Self::with(HDL_MAX_ITER, HDL_OFFSET);
        l.params.schedule = isdb_fec::ldpc::fixed::Schedule::Layered { step: HDL_STEP };
        l
    }
}

/// Lanes of the CPU decoder.
const LANES: usize = 16;

impl LdpcDecoder for FixedLdpc {
    fn decode(&mut self, code: &Code, words: &[&[i8]]) -> Vec<(LaneResult, Vec<u8>)> {
        let p = self.params;
        let per = words.len().div_ceil(self.threads.max(1)).max(LANES);
        if words.len() <= per {
            return Decoder::<LANES>::default().decode_all(code, words, p);
        }
        std::thread::scope(|sc| {
            let hs: Vec<_> = words
                .chunks(per)
                .map(|c| sc.spawn(move || Decoder::<LANES>::default().decode_all(code, c, p)))
                .collect();
            hs.into_iter()
                .flat_map(|h| h.join().unwrap_or_else(|e| std::panic::resume_unwind(e)))
                .collect()
        })
    }
}

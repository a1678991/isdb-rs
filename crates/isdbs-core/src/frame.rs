//! Frame and superframe geometry, and the frame sync words.
//!
//! BO.1408-1 Appendix 1 Table 3 gives the numbers; MIC Ordinance No. 87 of
//! 2011, annexed tables 45, 50 and 51 (figures `H23F11001000087-085`,
//! `-092`, `-095`), give the layout that BO.1408 leaves to "the system":
//!
//! ```text
//! frame (39 936 symbols)
//! ├─ header, 192 BPSK symbols = 96 bits through the r = 1/2 inner code
//! │    W1 (2 bytes) │ TMCC (8 bytes) │ W2 or W3 (2 bytes)
//! └─ 192 × ( 203 main-signal symbols │ 4 burst symbols )
//! ```
//!
//! The header bytes replace the first byte of slots 1–12 (table 45 note 4):
//! the main signal carries 203 bytes per slot, 48 × 203 bytes per frame.

/// Slots per frame, N (Table 3 item 1).
pub const SLOTS: usize = 48;
/// Frames per superframe, M (Table 3 item 2).
pub const FRAMES: usize = 8;
/// Bytes per slot on air, B (Table 3 item 3: the MPEG sync byte is not
/// sent).
pub const SLOT_BYTES: usize = 203;
/// Bytes per slot before the sync byte is removed (RS(204,188) codeword).
pub const PACKET_BYTES: usize = 204;
/// Main-signal symbols per frame, I_D (Table 3 item 4).
pub const MAIN_SYMBOLS: usize = 38_976;
/// Symbols per frame including TMCC and bursts (Table 3 item 5).
pub const FRAME_SYMBOLS: usize = 39_936;
/// Symbols of the header (W1, TMCC, W2/W3) at the start of each frame.
pub const HEADER_SYMBOLS: usize = 192;
/// Main-signal symbols between two bursts (BO.1408-1 §7: "every 203
/// symbols when the MPEG sync words are not transmitted").
pub const BURST_PERIOD: usize = 203;
/// Symbols per burst (§7).
pub const BURST_SYMBOLS: usize = 4;
/// Bursts per frame: MAIN_SYMBOLS / BURST_PERIOD.
pub const BURSTS: usize = MAIN_SYMBOLS / BURST_PERIOD;
/// Header bytes per frame: W1 (2) + TMCC (8) + W2/W3 (2).
pub const HEADER_BYTES: usize = 12;
/// TMCC bytes per frame.
pub const TMCC_BYTES_PER_FRAME: usize = 8;
/// Highest spectrum efficiency, bits per symbol (Table 3 item 7).
pub const E_MAX: usize = 2;

/// W1 = 1B95h, at the start of every header (BO.1408-1 §6.3).
pub const W1: u16 = 0x1B95;
/// W2 = A340h, ending the header of frame 1 of a superframe.
pub const W2: u16 = 0xA340;
/// W3 = 5CBFh (= ¬W2), ending the headers of frames 2–8.
pub const W3: u16 = 0x5CBF;

/// The last 20 bits of W1, W2, W3 after the inner code (annexed table 45
/// note 1: "ｗ１：ＥＣＤ２８ｈ，ｗ２：０Ｂ６７７ｈ，ｗ３：Ｆ４９８８ｈ"),
/// in transmission order, MSB first. The first 6 bits of each word only
/// flush the encoder's memory (§6.3), so the coded tail of 10 bits × 2 is
/// independent of what precedes the word.
pub const W1_CODED_TAIL: u32 = 0xECD28;
/// See [`W1_CODED_TAIL`].
pub const W2_CODED_TAIL: u32 = 0x0B677;
/// See [`W1_CODED_TAIL`].
pub const W3_CODED_TAIL: u32 = 0xF4988;
/// Length of a coded tail in bits (= BPSK symbols).
pub const CODED_TAIL_BITS: usize = 20;
/// Header symbol where the W1 tail starts (32 coded bits, the last 20).
pub const W1_TAIL_AT: usize = 12;
/// Header symbol where the W2/W3 tail starts.
pub const W23_TAIL_AT: usize = HEADER_SYMBOLS - CODED_TAIL_BITS;

/// Symbol index (within a frame) of main-signal symbol `k` (0..38 976).
pub const fn main_symbol_position(k: usize) -> usize {
    HEADER_SYMBOLS + (k / BURST_PERIOD) * (BURST_PERIOD + BURST_SYMBOLS) + k % BURST_PERIOD
}

/// Symbol index (within a frame) of burst `b` (0..192), symbol `j` (0..4).
pub const fn burst_symbol_position(b: usize, j: usize) -> usize {
    HEADER_SYMBOLS + b * (BURST_PERIOD + BURST_SYMBOLS) + BURST_PERIOD + j
}

/// The coded-tail bits of `tail` (MSB first) as 0/1 values.
pub fn tail_bits(tail: u32) -> [u8; CODED_TAIL_BITS] {
    let mut out = [0u8; CODED_TAIL_BITS];
    for (i, b) in out.iter_mut().enumerate() {
        *b = ((tail >> (CODED_TAIL_BITS - 1 - i)) & 1) as u8;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use isdb_fec::ConvEncoder;

    /// BO.1408-1 Appendix 1 Table 3 and the annexed-table layout agree.
    #[test]
    fn spec_table_3_geometry() {
        assert_eq!((SLOTS, FRAMES, SLOT_BYTES, E_MAX), (48, 8, 203, 2));
        // Equation (3): N = I_D · E_max / (8 B).
        assert_eq!(MAIN_SYMBOLS * E_MAX / (8 * SLOT_BYTES), SLOTS);
        assert_eq!(MAIN_SYMBOLS, 38_976);
        assert_eq!(BURSTS, 192);
        assert_eq!(MAIN_SYMBOLS % BURST_PERIOD, 0);
        assert_eq!(
            HEADER_SYMBOLS + BURSTS * (BURST_PERIOD + BURST_SYMBOLS),
            FRAME_SYMBOLS
        );
        assert_eq!(FRAME_SYMBOLS, 39_936);
        // header = 12 bytes at rate 1/2 BPSK
        assert_eq!(HEADER_BYTES * 8 * 2, HEADER_SYMBOLS);
        // TMCC: 8 frames × 8 bytes = the RS(64,48) codeword.
        assert_eq!(FRAMES * TMCC_BYTES_PER_FRAME, isdb_fec::RS_64_48.n());
        assert_eq!(main_symbol_position(0), 192);
        assert_eq!(main_symbol_position(203), 192 + 207);
        assert_eq!(burst_symbol_position(0, 0), 395);
        assert_eq!(burst_symbol_position(191, 3), FRAME_SYMBOLS - 1);
    }

    /// BO.1408-1 §6.3 (W3 = ¬W2) and annexed table 45 note 1: the inner
    /// code (171₈ first, then 133₈, from any state) ends each word in the
    /// printed 20-bit tail.
    #[test]
    fn spec_w_words_and_coded_tails() {
        assert_eq!(W3, !W2);
        for (w, tail) in [
            (W1, W1_CODED_TAIL),
            (W2, W2_CODED_TAIL),
            (W3, W3_CODED_TAIL),
        ] {
            for start in [0u8, 0x15, 0x3F] {
                let mut e = ConvEncoder::new();
                // reach an arbitrary state
                for k in 0..6 {
                    e.push((start >> k) & 1);
                }
                let mut coded = Vec::new();
                for i in (0..16).rev() {
                    let (x, y) = e.push(((w >> i) & 1) as u8);
                    coded.push(x);
                    coded.push(y);
                }
                assert_eq!(&coded[12..], &tail_bits(tail), "{w:04X} from {start:02X}");
            }
        }
    }
}

//! Per-layer quantities used by the bit-domain channel coding
//! (ARIB STD-B31 v2.2 §3.2.2, §3.6, §3.8, Tables 3-6, 3-7).
//!
//! Each helper is an exact integer function of the [`LayerConfig`] and
//! [`Mode`]; the tests below check them against the spec's own tables.

use crate::params::{CodeRate, LayerConfig, Mode, Modulation, SYMBOLS_PER_FRAME, TRANSMISSION_TSP};

/// Bytes after which the RS-protected stream is carried per transmission
/// TSP through the convolutional mother code: 204 × 2 = 408 bytes
/// (§3.2.2.2, footnote "Convolutional coding of a single TS packet (204
/// bytes) of data produces 408 bytes"). Also the TS packet period in model
/// receiver clocks (§3.2.2.2: "every TS packet period (408 clocks)").
pub const MOTHER_CODED_TSP_BYTES: usize = 2 * TRANSMISSION_TSP;

/// Transmission TSPs of delay caused by byte interleaving, transmit plus
/// receive (§3.6: "byte interleaving (11 transmission TSPs)").
pub const BYTE_INTERLEAVE_DELAY_TSPS: usize = 11;

/// Table 3-7 coefficient (Mode 1 column, "12 × N − 11" ⇒ 12) per
/// modulation group and code rate. Rows: DQPSK/QPSK, 16QAM, 64QAM; columns
/// 1/2, 2/3, 3/4, 5/6, 7/8. Mode 2 and Mode 3 columns are ×2 and ×4.
const TABLE_3_7_MODE1: [[usize; 5]; 3] = [
    [12, 16, 18, 20, 21],
    [24, 32, 36, 40, 42],
    [36, 48, 54, 60, 63],
];

fn code_rate_index(r: CodeRate) -> usize {
    match r {
        CodeRate::R1_2 => 0,
        CodeRate::R2_3 => 1,
        CodeRate::R3_4 => 2,
        CodeRate::R5_6 => 3,
        CodeRate::R7_8 => 4,
    }
}

impl LayerConfig {
    /// Delay-adjustment value in transmission TSPs (§3.6, Table 3-7:
    /// "coefficient × N − 11", N = segments of the layer). Together with the
    /// 11-TSP byte-interleaving delay the total equals one frame of this
    /// layer's TSPs.
    pub fn delay_adjustment_tsps(&self, mode: Mode) -> usize {
        let row = match self.modulation {
            Modulation::Dqpsk | Modulation::Qpsk => 0,
            Modulation::Qam16 => 1,
            Modulation::Qam64 => 2,
        };
        TABLE_3_7_MODE1[row][code_rate_index(self.code_rate)] * mode.scale() * self.segments
            - BYTE_INTERLEAVE_DELAY_TSPS
    }

    /// Convolutionally coded (punctured) bits carried by this layer in one
    /// OFDM frame: nc × S_X × N × 204 (data carriers per segment, Table 3-1;
    /// S_X, Table 3-6; 204 symbols per frame, Table 3-1). This is the length
    /// of the bit vector handed from the inner coder (§3.8) to the carrier
    /// modulator (§3.9).
    pub fn coded_bits_per_frame(&self, mode: Mode) -> usize {
        mode.data_carriers_per_segment() * self.modulation.bits_per_symbol() * self.segments * SYMBOLS_PER_FRAME
    }

    /// Information (pre-inner-code) bits per OFDM frame:
    /// `tsp_per_frame × 204 × 8` (§3.2.2.2 / Table 3-3).
    pub fn info_bits_per_frame(&self, mode: Mode) -> usize {
        self.tsp_per_frame(mode) * TRANSMISSION_TSP * 8
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::params::TimeInterleave;

    const MODS: [Modulation; 4] = [Modulation::Dqpsk, Modulation::Qpsk, Modulation::Qam16, Modulation::Qam64];
    const RATES: [CodeRate; 5] = [CodeRate::R1_2, CodeRate::R2_3, CodeRate::R3_4, CodeRate::R5_6, CodeRate::R7_8];

    /// Table 3-7, all three mode columns as printed (Mode 1 / Mode 2 / Mode 3
    /// coefficient of N) and the §3.6 rule "all delays ... are one frame".
    #[test]
    fn spec_table_3_7_delay_adjustment() {
        let printed: [(Modulation, [[usize; 3]; 5]); 3] = [
            (Modulation::Qpsk, [[12, 24, 48], [16, 32, 64], [18, 36, 72], [20, 40, 80], [21, 42, 84]]),
            (Modulation::Qam16, [[24, 48, 96], [32, 64, 128], [36, 72, 144], [40, 80, 160], [42, 84, 168]]),
            (Modulation::Qam64, [[36, 72, 144], [48, 96, 192], [54, 108, 216], [60, 120, 240], [63, 126, 252]]),
        ];
        for (m, rows) in printed {
            for (r, cols) in RATES.iter().zip(rows) {
                for (mode, coef) in [Mode::M1, Mode::M2, Mode::M3].into_iter().zip(cols) {
                    for n in 1..=13 {
                        let l = LayerConfig { segments: n, modulation: m, code_rate: *r, time_interleave: TimeInterleave(0) };
                        assert_eq!(l.delay_adjustment_tsps(mode), coef * n - 11);
                        assert_eq!(l.delay_adjustment_tsps(mode) + BYTE_INTERLEAVE_DELAY_TSPS, l.tsp_per_frame(mode));
                    }
                }
            }
        }
    }

    /// Coded bits per frame equal info bits / R (Table 3-8 rate) for every
    /// layer configuration.
    #[test]
    fn coded_bits_consistent_with_rate() {
        for mode in [Mode::M1, Mode::M2, Mode::M3] {
            for m in MODS {
                for r in RATES {
                    let l = LayerConfig { segments: 3, modulation: m, code_rate: r, time_interleave: TimeInterleave(0) };
                    let (num, den) = r.ratio();
                    assert_eq!(l.coded_bits_per_frame(mode) * num, l.info_bits_per_frame(mode) * den);
                }
            }
        }
    }
}

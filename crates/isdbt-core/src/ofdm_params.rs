//! Derived parameters for the symbol-domain chain (ARIB STD-B31 v2.2
//! §3.9–3.14): bit-interleave delays (Figs. 3-12/3-14/3-16/3-18, Table 3-9),
//! time-interleave delays (Fig. 3-23, Table 3-12), segment arrangement on
//! the spectrum (Fig. 3-32), carrier counts (Tables 3-1/3-2) and the
//! centre carrier (§3.14.1).

use crate::params::{
    GuardInterval, Mode, Modulation, Params, SEGMENTS, SYMBOLS_PER_FRAME, Segments,
};

/// Carrier number of the RF centre frequency, Kc (§3.14.1: 702/1404/2808;
/// Attachment Table 2-1). It is the middle of the `K` carriers.
pub fn center_carrier(mode: Mode) -> usize {
    mode.total_carriers() / 2
}

/// Per-bit delays of the bit interleaver, in carrier symbols, for bits
/// b0..b(S-1) of each carrier symbol (§3.9.3: Fig. 3-12 DQPSK and Fig. 3-14
/// QPSK "120-bit delay" on b1; Fig. 3-16 16QAM 40/80/120 on b1..b3;
/// Fig. 3-18 64QAM 24/48/72/96/120 on b1..b5).  Each branch carries one bit
/// per carrier symbol, so an n-bit delay element delays by n symbols.
pub fn bit_interleave_delays(modulation: Modulation) -> &'static [usize] {
    match modulation {
        Modulation::Dqpsk | Modulation::Qpsk => &[0, 120],
        Modulation::Qam16 => &[0, 40, 80, 120],
        Modulation::Qam64 => &[0, 24, 48, 72, 96, 120],
    }
}

/// The longest bit-interleave branch delay, 120 carrier symbols (§3.9.2:
/// "delays equivalent to 120 carrier symbols occur").
pub const BIT_INTERLEAVE_MAX_DELAY: usize = 120;

/// Total transmission + reception delay of bit interleaving after the
/// delay adjustment of Table 3-9: 2 OFDM symbols (§3.9.2).
pub const BIT_INTERLEAVE_TOTAL_SYMBOLS: usize = 2;

/// Delay-adjustment value in bits inserted before bit interleaving
/// (Table 3-9): Mode 1 DQPSK/QPSK `384·N − 240`, 16QAM `768·N − 480`,
/// 64QAM `1152·N − 720`; Modes 2/3 double/quadruple the `N` coefficient.
/// `n_segments` is the number of segments of the layer.
pub fn bit_delay_adjustment_bits(
    mode: Mode,
    modulation: Modulation,
    n_segments: Segments,
) -> usize {
    let (coef_m1, constant) = match modulation {
        Modulation::Dqpsk | Modulation::Qpsk => (384, 240),
        Modulation::Qam16 => (768, 480),
        Modulation::Qam64 => (1152, 720),
    };
    coef_m1 * mode.scale() * n_segments.count() - constant
}

/// Time-interleave branch factor m_i = (i·5) mod 96 for data-carrier index
/// `i` (0..nc) within a data segment (Fig. 3-23).  The branch delay is
/// `I · m_i` OFDM symbols.
pub fn time_interleave_m(i: usize) -> usize {
    (i * 5) % 96
}

/// Largest m_i (95): the total TX+RX time-interleave delay is `95·I`
/// symbols per carrier (Table 3-2: e.g. I=4 → 380 symbols).
pub const TIME_INTERLEAVE_M_MAX: usize = 95;

/// Table 3-12: (I, delay-adjustment symbols, delayed frames) per mode.
const TABLE_3_12: [[(usize, usize, usize); 4]; 3] = [
    [(0, 0, 0), (4, 28, 2), (8, 56, 4), (16, 112, 8)],
    [(0, 0, 0), (2, 14, 1), (4, 28, 2), (8, 56, 4)],
    [(0, 0, 0), (1, 109, 1), (2, 14, 1), (4, 28, 2)],
];

fn table_3_12(mode: Mode, length: usize) -> Option<(usize, usize, usize)> {
    TABLE_3_12[mode.number() as usize - 1]
        .iter()
        .copied()
        .find(|e| e.0 == length)
}

/// Number of delay-adjustment symbols applied before time interleaving for
/// interleave length `length` (= I), Table 3-12.  `None` if `I` is not a
/// value of the table for `mode`.
pub fn time_interleave_adjustment_symbols(mode: Mode, length: usize) -> Option<usize> {
    table_3_12(mode, length).map(|e| e.1)
}

/// Number of delayed frames in transmission and reception for interleave
/// length `length`, Table 3-12 (= (adjustment + 95·I) / 204).
pub fn time_interleave_delayed_frames(mode: Mode, length: usize) -> Option<usize> {
    table_3_12(mode, length).map(|e| e.2)
}

/// Position (0 = lowest frequency .. 12 = highest) of OFDM segment number
/// `seg` on the transmission spectrum (Fig. 3-32: 11, 9, 7, 5, 3, 1, 0, 2,
/// 4, 6, 8, 10, 12 in ascending frequency).
pub fn segment_position(seg: usize) -> usize {
    assert!(seg < SEGMENTS, "segment {seg}");
    if seg == 0 {
        6
    } else if seg % 2 == 1 {
        6 - seg.div_ceil(2)
    } else {
        6 + seg / 2
    }
}

/// Inverse of [`segment_position`].
pub fn segment_at_position(pos: usize) -> usize {
    const ORDER: [usize; SEGMENTS] = [11, 9, 7, 5, 3, 1, 0, 2, 4, 6, 8, 10, 12];
    ORDER[pos]
}

/// Carrier counts of one OFDM segment (Table 3-1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SegmentCarrierCounts {
    /// Data carriers (96/192/384).
    pub data: usize,
    /// Scattered pilots per symbol (coherent: 9/18/36; differential: 0).
    pub sp: usize,
    /// Continual pilots (coherent: 0; differential: 1).
    pub cp: usize,
    /// TMCC carriers (coherent 1/2/4; differential 5/10/20).
    pub tmcc: usize,
    /// AC1 carriers (2/4/8).
    pub ac1: usize,
    /// AC2 carriers (coherent 0; differential 4/9/19).
    pub ac2: usize,
}

/// Table 3-1 carrier counts for a coherent (`differential = false`) or
/// differential segment.
pub fn segment_carrier_counts(mode: Mode, differential: bool) -> SegmentCarrierCounts {
    let s = mode.scale();
    if differential {
        SegmentCarrierCounts {
            data: 96 * s,
            sp: 0,
            cp: 1,
            tmcc: 5 * s,
            ac1: 2 * s,
            ac2: [4, 9, 19][mode.number() as usize - 1],
        }
    } else {
        SegmentCarrierCounts {
            data: 96 * s,
            sp: 9 * s,
            cp: 0,
            tmcc: s,
            ac1: 2 * s,
            ac2: 0,
        }
    }
}

/// Samples of guard interval + useful part for `mode`/`guard`.
pub fn samples_per_symbol(mode: Mode, guard: GuardInterval) -> usize {
    mode.fft_size() + guard.samples(mode)
}

impl Params {
    /// True if data segment / OFDM segment `seg` (0..13, §3.11.2 numbering)
    /// is differentially modulated (its layer uses DQPSK, §3.12.1).
    pub fn segment_is_differential(&self, seg: usize) -> bool {
        let layer = self.layer_of_segment(seg);
        self.layers()[layer.index()].is_some_and(|l| l.modulation.is_differential())
    }

    /// Time-interleave length I of `layer` (0 if the layer is unused).
    pub fn time_interleave_length(&self, layer: crate::Layer) -> usize {
        self.layers()[layer.index()].map_or(0, |l| l.time_interleave.length(self.mode()))
    }

    /// Total symbol-domain latency of `layer` from bits entering the
    /// carrier modulator to LLRs leaving the bit deinterleaver, in OFDM
    /// symbols: 2 (bit interleave, §3.9.2) + 204 × Table 3-12 frames.
    pub fn symbol_domain_delay_symbols(&self, layer: crate::Layer) -> usize {
        let frames =
            time_interleave_delayed_frames(self.mode(), self.time_interleave_length(layer))
                .unwrap_or(0);
        BIT_INTERLEAVE_TOTAL_SYMBOLS + frames * SYMBOLS_PER_FRAME
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CodeRate, LayerConfig, Segments, TimeInterleave};

    /// §3.14.1: Kc = 702/1404/2808.
    #[test]
    fn spec_3_14_1_center_carrier() {
        assert_eq!(center_carrier(Mode::M1), 702);
        assert_eq!(center_carrier(Mode::M2), 1404);
        assert_eq!(center_carrier(Mode::M3), 2808);
    }

    /// Table 3-9: adjustment + 120-symbol interleave delay = 2 OFDM symbols.
    #[test]
    fn spec_table_3_9_total_two_symbols() {
        assert_eq!(
            bit_delay_adjustment_bits(Mode::M1, Modulation::Qpsk, Segments::new(1).unwrap()),
            144
        );
        assert_eq!(
            bit_delay_adjustment_bits(Mode::M3, Modulation::Qam64, Segments::new(12).unwrap()),
            4608 * 12 - 720
        );
        for mode in [Mode::M1, Mode::M2, Mode::M3] {
            for m in [
                Modulation::Dqpsk,
                Modulation::Qpsk,
                Modulation::Qam16,
                Modulation::Qam64,
            ] {
                let s = m.bits_per_symbol();
                assert_eq!(bit_interleave_delays(m).len(), s);
                assert_eq!(
                    *bit_interleave_delays(m).last().unwrap(),
                    BIT_INTERLEAVE_MAX_DELAY
                );
                for n in Segments::ALL {
                    let adj = bit_delay_adjustment_bits(mode, m, n);
                    assert_eq!(adj % s, 0);
                    let total = adj / s + BIT_INTERLEAVE_MAX_DELAY;
                    assert_eq!(
                        total,
                        BIT_INTERLEAVE_TOTAL_SYMBOLS * mode.data_carriers_per_segment() * n.count()
                    );
                }
            }
        }
    }

    /// Table 3-12 / Table 3-2: adjustment + 95·I = frames × 204.
    #[test]
    fn spec_table_3_12_frame_multiple() {
        for mode in [Mode::M1, Mode::M2, Mode::M3] {
            for k in TimeInterleave::ALL {
                let i = k.length(mode);
                let adj = time_interleave_adjustment_symbols(mode, i).unwrap();
                let frames = time_interleave_delayed_frames(mode, i).unwrap();
                assert_eq!(
                    adj + TIME_INTERLEAVE_M_MAX * i,
                    frames * SYMBOLS_PER_FRAME,
                    "{mode:?} I={i}"
                );
            }
        }
        // Fig. 3-23: m_i takes every value 0..95 once per 96 carriers.
        let mut seen = [false; 96];
        for i in 0..96 {
            seen[time_interleave_m(i)] = true;
        }
        assert!(seen.iter().all(|&s| s));
    }

    /// Fig. 3-32 arrangement.
    #[test]
    fn spec_fig_3_32_positions() {
        let order: Vec<usize> = (0..13).map(segment_at_position).collect();
        assert_eq!(order, [11, 9, 7, 5, 3, 1, 0, 2, 4, 6, 8, 10, 12]);
        for s in 0..13 {
            assert_eq!(segment_at_position(segment_position(s)), s);
        }
    }

    /// Table 3-1 / Table 3-2 carrier totals.
    #[test]
    fn spec_table_3_1_counts() {
        for mode in [Mode::M1, Mode::M2, Mode::M3] {
            for diff in [false, true] {
                let c = segment_carrier_counts(mode, diff);
                assert_eq!(
                    c.data + c.sp + c.cp + c.tmcc + c.ac1 + c.ac2,
                    mode.carriers_per_segment()
                );
            }
        }
    }

    #[test]
    fn differential_segments_and_delay() {
        let a = Some(LayerConfig {
            segments: Segments::new(1).unwrap(),
            modulation: Modulation::Dqpsk,
            code_rate: CodeRate::R1_2,
            time_interleave: TimeInterleave::K1,
        });
        let b = Some(LayerConfig {
            segments: Segments::new(12).unwrap(),
            modulation: Modulation::Qam64,
            code_rate: CodeRate::R3_4,
            time_interleave: TimeInterleave::K2,
        });
        let p = Params::new(Mode::M3, GuardInterval::G1_8, true, [a, b, None]).unwrap();
        assert!(p.segment_is_differential(0));
        assert!(!p.segment_is_differential(1));
        assert_eq!(p.symbol_domain_delay_symbols(crate::Layer::A), 2 + 204);
        assert_eq!(p.symbol_domain_delay_symbols(crate::Layer::B), 2 + 204);
    }
}

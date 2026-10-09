//! Transmission parameters and derived figures (ARIB STD-B31 §2.3, §3,
//! Tables 3-1 .. 3-7, 3-9, 3-12).

use std::fmt;

/// Symbols per OFDM frame (Table 3-1).
pub const SYMBOLS_PER_FRAME: usize = 204;
/// OFDM segments in the 6 MHz channel (Table 3-2, Ns).
pub const SEGMENTS: usize = 13;
/// IFFT sampling frequency numerator/denominator: 512/63 MHz = 2048/252 µs
/// (Table 3-1 "FFT sampling frequency").
pub const SAMPLE_RATE_NUM: u64 = 512_000_000;
/// See [`SAMPLE_RATE_NUM`].
pub const SAMPLE_RATE_DEN: u64 = 63;
/// Transmission TSP length: 188 bytes + 16 bytes (RS parity) (§3.2.1, §3.3).
pub const TRANSMISSION_TSP: usize = 204;
/// Transport stream packet length.
pub const TSP: usize = 188;

/// IFFT sampling frequency in Hz (≈ 8.126984 MHz).
pub fn sample_rate_hz() -> f64 {
    SAMPLE_RATE_NUM as f64 / SAMPLE_RATE_DEN as f64
}

/// System mode (§2.3, Table 3-1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Mode {
    /// Mode 1: 108 carriers/segment, 2k FFT.
    M1,
    /// Mode 2: 216 carriers/segment, 4k FFT.
    M2,
    /// Mode 3: 432 carriers/segment, 8k FFT.
    M3,
}

impl Mode {
    /// 1, 2 or 3.
    pub fn number(self) -> u8 {
        match self {
            Mode::M1 => 1,
            Mode::M2 => 2,
            Mode::M3 => 3,
        }
    }
    /// From 1, 2 or 3.
    pub fn from_number(n: u8) -> Option<Self> {
        match n {
            1 => Some(Mode::M1),
            2 => Some(Mode::M2),
            3 => Some(Mode::M3),
            _ => None,
        }
    }
    /// 2^(mode-1): the scale factor between modes.
    pub fn scale(self) -> usize {
        1 << (self.number() - 1)
    }
    /// Carriers per OFDM segment (Table 3-1 "Total count": 108/216/432).
    pub fn carriers_per_segment(self) -> usize {
        108 * self.scale()
    }
    /// Data carriers per segment, `nc` (Table 3-1: 96/192/384).
    pub fn data_carriers_per_segment(self) -> usize {
        96 * self.scale()
    }
    /// Total carriers in the band: 108·Ns+1 etc. (Table 3-2: 1405/2809/5617).
    pub fn total_carriers(self) -> usize {
        self.carriers_per_segment() * SEGMENTS + 1
    }
    /// IFFT size (2048/4096/8192): useful symbol length × sample rate.
    pub fn fft_size(self) -> usize {
        2048 * self.scale()
    }
    /// Useful (effective) symbol length in µs (Table 3-1: 252/504/1008).
    pub fn useful_symbol_us(self) -> f64 {
        252.0 * self.scale() as f64
    }
}

/// Guard-interval ratio (Table 3-1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum GuardInterval {
    /// 1/4
    G1_4,
    /// 1/8
    G1_8,
    /// 1/16
    G1_16,
    /// 1/32
    G1_32,
}

impl GuardInterval {
    /// Denominator of the ratio (4, 8, 16, 32).
    pub fn denominator(self) -> usize {
        match self {
            GuardInterval::G1_4 => 4,
            GuardInterval::G1_8 => 8,
            GuardInterval::G1_16 => 16,
            GuardInterval::G1_32 => 32,
        }
    }
    /// From denominator 4/8/16/32.
    pub fn from_denominator(d: usize) -> Option<Self> {
        match d {
            4 => Some(GuardInterval::G1_4),
            8 => Some(GuardInterval::G1_8),
            16 => Some(GuardInterval::G1_16),
            32 => Some(GuardInterval::G1_32),
            _ => None,
        }
    }
    /// Guard samples for a mode (FFT size / denominator).
    pub fn samples(self, mode: Mode) -> usize {
        mode.fft_size() / self.denominator()
    }
}

/// Carrier modulation (§3.9.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Modulation {
    /// π/4-shift DQPSK (differential).
    Dqpsk,
    /// QPSK (coherent).
    Qpsk,
    /// 16QAM (coherent).
    Qam16,
    /// 64QAM (coherent).
    Qam64,
}

impl Modulation {
    /// Bits per carrier symbol, S_X of Table 3-6.
    pub fn bits_per_symbol(self) -> usize {
        match self {
            Modulation::Dqpsk | Modulation::Qpsk => 2,
            Modulation::Qam16 => 4,
            Modulation::Qam64 => 6,
        }
    }
    /// True for DQPSK (differential-modulation segments, §3.12.1).
    pub fn is_differential(self) -> bool {
        matches!(self, Modulation::Dqpsk)
    }
}

/// Inner-code coding rate (Table 3-8).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CodeRate {
    /// 1/2
    R1_2,
    /// 2/3
    R2_3,
    /// 3/4
    R3_4,
    /// 5/6
    R5_6,
    /// 7/8
    R7_8,
}

impl CodeRate {
    /// (numerator, denominator).
    pub fn ratio(self) -> (usize, usize) {
        match self {
            CodeRate::R1_2 => (1, 2),
            CodeRate::R2_3 => (2, 3),
            CodeRate::R3_4 => (3, 4),
            CodeRate::R5_6 => (5, 6),
            CodeRate::R7_8 => (7, 8),
        }
    }
}

/// Time-interleaving length index (Table 3-2/3-12): the TMCC code 0..3 maps
/// to I = 0,4,8,16 (Mode 1), 0,2,4,8 (Mode 2), 0,1,2,4 (Mode 3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TimeInterleave(pub u8);

impl TimeInterleave {
    /// The length I for `mode`; `None` if the index is not 0..=3.
    pub fn length(self, mode: Mode) -> Option<usize> {
        if self.0 > 3 {
            return None;
        }
        let i = if self.0 == 0 {
            0
        } else {
            1usize << (self.0 - 1)
        };
        Some(i * 4 / mode.scale())
    }
}

/// One hierarchical layer (A, B or C).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LayerConfig {
    /// Number of segments (1..=13).
    pub segments: usize,
    /// Carrier modulation.
    pub modulation: Modulation,
    /// Inner-code rate.
    pub code_rate: CodeRate,
    /// Time-interleaving length index.
    pub time_interleave: TimeInterleave,
}

impl LayerConfig {
    /// Transmission TSPs carried per OFDM frame by this layer:
    /// nc·S·R/8 per segment (equals Table 3-3 "Number of TSPs transmitted").
    pub fn tsp_per_frame(&self, mode: Mode) -> usize {
        let (n, d) = self.code_rate.ratio();
        let bits = mode.data_carriers_per_segment() * self.modulation.bits_per_symbol() * n;
        debug_assert_eq!(bits % (d * 8), 0);
        bits / (d * 8) * self.segments
    }
}

/// Hierarchical layer identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Layer {
    /// Layer A (lowest data-segment numbers; partial reception if enabled).
    A,
    /// Layer B.
    B,
    /// Layer C.
    C,
}

impl Layer {
    /// All layers in order.
    pub const ALL: [Layer; 3] = [Layer::A, Layer::B, Layer::C];
    /// 0, 1, 2.
    pub fn index(self) -> usize {
        self as usize
    }
}

impl fmt::Display for Layer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Layer::A => "A",
            Layer::B => "B",
            Layer::C => "C",
        })
    }
}

/// Invalid parameter combination: which §3.11.2 / Table 3-2 constraint a
/// [`Params::new`] call violates.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ParamError {
    /// No layer is configured.
    #[error("invalid ISDB-T parameters: no layer configured")]
    NoLayer,
    /// A layer is configured after an unused one (layers are used
    /// contiguously from A, §3.11.2).
    #[error("invalid ISDB-T parameters: layers must be used contiguously from A")]
    NonContiguousLayers,
    /// A configured layer has no segment.
    #[error("invalid ISDB-T parameters: layer {0} has 0 segments")]
    EmptyLayer(Layer),
    /// The layers' segment counts do not sum to the 13 segments of the
    /// channel (Table 3-2, Ns = 13).
    #[error("invalid ISDB-T parameters: segments sum to {0}, need 13")]
    SegmentSum(usize),
    /// Partial reception (§3.15.6.4) with a layer A of more than one
    /// segment.
    #[error("invalid ISDB-T parameters: partial reception requires layer A = 1 segment (got {0})")]
    PartialReceptionSegments(usize),
    /// A differential (DQPSK) layer follows a coherent one: data segments
    /// are numbered partial reception → differential → coherent (§3.11.2).
    #[error("invalid ISDB-T parameters: differential layer {0} after a coherent layer")]
    DifferentialAfterCoherent(Layer),
    /// Time-interleaving length index outside 0..=3 (Table 3-12, TMCC
    /// codes '000'–'011').
    #[error("invalid ISDB-T parameters: layer {0} time interleave index {1} not in 0..=3")]
    TimeInterleaveIndex(Layer, u8),
}

/// Complete validated transmission configuration (what TMCC signals).
///
/// The fields are private: a `Params` only exists through [`Params::new`],
/// so every value satisfies the §3.11.2 constraints the modulator and
/// demodulator rely on (segment counts summing to 13, contiguous layers,
/// segment order). Read them with the accessors.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Params {
    mode: Mode,
    guard: GuardInterval,
    partial_reception: bool,
    layers: [Option<LayerConfig>; 3],
}

impl Params {
    /// Validate the §3.11.2 constraints: segment counts sum to 13; used
    /// layers are contiguous from A; partial reception means layer A has
    /// exactly one segment; data-segment numbers are assigned partial
    /// reception → differential → coherent, so after the (optional)
    /// partial-reception layer no DQPSK layer may follow a coherent one;
    /// time-interleave index 0..=3.
    ///
    /// # Errors
    ///
    /// The [`ParamError`] of the first violated constraint.
    pub fn new(
        mode: Mode,
        guard: GuardInterval,
        partial_reception: bool,
        layers: [Option<LayerConfig>; 3],
    ) -> Result<Self, ParamError> {
        let used: Vec<(Layer, &LayerConfig)> = Layer::ALL
            .into_iter()
            .zip(layers.iter())
            .map_while(|(id, l)| l.as_ref().map(|l| (id, l)))
            .collect();
        if layers.iter().flatten().count() != used.len() {
            return Err(ParamError::NonContiguousLayers);
        }
        if used.is_empty() {
            return Err(ParamError::NoLayer);
        }
        if let Some(&(id, _)) = used.iter().find(|(_, l)| l.segments == 0) {
            return Err(ParamError::EmptyLayer(id));
        }
        let total = used
            .iter()
            .fold(0usize, |acc, (_, l)| acc.saturating_add(l.segments));
        if total != SEGMENTS {
            return Err(ParamError::SegmentSum(total));
        }
        if partial_reception && used[0].1.segments != 1 {
            return Err(ParamError::PartialReceptionSegments(used[0].1.segments));
        }
        let skip = usize::from(partial_reception);
        let mut seen_coherent = false;
        for &(id, l) in &used[skip..] {
            if l.modulation.is_differential() && seen_coherent {
                return Err(ParamError::DifferentialAfterCoherent(id));
            }
            seen_coherent |= !l.modulation.is_differential();
        }
        if let Some(&(id, l)) = used.iter().find(|(_, l)| l.time_interleave.0 > 3) {
            return Err(ParamError::TimeInterleaveIndex(id, l.time_interleave.0));
        }
        Ok(Self {
            mode,
            guard,
            partial_reception,
            layers,
        })
    }

    /// Mode.
    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// Guard interval ratio.
    pub fn guard(&self) -> GuardInterval {
        self.guard
    }

    /// Partial-reception flag (§3.15.6.4): layer A is the single centre
    /// segment.
    pub fn partial_reception(&self) -> bool {
        self.partial_reception
    }

    /// Layers A, B, C; `None` for an unused layer.
    pub fn layers(&self) -> [Option<LayerConfig>; 3] {
        self.layers
    }

    /// The configuration of layer `id`, `None` when it is unused.
    pub fn layer(&self, id: Layer) -> Option<&LayerConfig> {
        self.layers[id.index()].as_ref()
    }

    /// Configured layers with their identifiers, in data-segment order.
    pub fn active_layers(&self) -> impl Iterator<Item = (Layer, &LayerConfig)> {
        Layer::ALL
            .into_iter()
            .zip(self.layers.iter())
            .filter_map(|(id, l)| l.as_ref().map(|l| (id, l)))
    }

    /// Layer owning data segment `seg` (0..13), in the §3.11.2 numbering.
    ///
    /// # Panics
    ///
    /// If `seg` ≥ 13.
    pub fn layer_of_segment(&self, seg: usize) -> Layer {
        let mut acc = 0;
        for (id, l) in self.active_layers() {
            acc += l.segments;
            if seg < acc {
                return id;
            }
        }
        panic!("segment {seg} out of range")
    }

    /// Number of differentially modulated segments, nd (Table 3-2).
    pub fn differential_segments(&self) -> usize {
        self.active_layers()
            .filter(|(_, l)| l.modulation.is_differential())
            .map(|(_, l)| l.segments)
            .sum()
    }

    /// Transmission TSPs per multiplex frame, n of Table 3-5:
    /// (FFT size × (1 + GI)) × 204 symbols / 408 clocks per TSP.
    pub fn multiplex_frame_tsps(&self) -> usize {
        let samples_per_symbol = self.mode.fft_size() + self.guard.samples(self.mode);
        samples_per_symbol * SYMBOLS_PER_FRAME / 408
    }

    /// Samples per OFDM symbol including the guard interval.
    pub fn samples_per_symbol(&self) -> usize {
        self.mode.fft_size() + self.guard.samples(self.mode)
    }

    /// Samples per OFDM frame.
    pub fn samples_per_frame(&self) -> usize {
        self.samples_per_symbol() * SYMBOLS_PER_FRAME
    }

    /// OFDM frame length in seconds (Table 3-1 "Frame length").
    pub fn frame_seconds(&self) -> f64 {
        self.samples_per_frame() as f64 / sample_rate_hz()
    }

    /// Frame length in 27 MHz system-clock ticks (H.222.0 PCR units):
    /// samples × 27e6 × 63 / 512e6; exact for every mode/GI.
    pub fn frame_ticks_27mhz(&self) -> u64 {
        let num = self.samples_per_frame() as u64 * 27_000_000 * SAMPLE_RATE_DEN;
        assert_eq!(num % SAMPLE_RATE_NUM, 0);
        num / SAMPLE_RATE_NUM
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layer(segments: usize, modulation: Modulation, code_rate: CodeRate) -> LayerConfig {
        LayerConfig {
            segments,
            modulation,
            code_rate,
            time_interleave: TimeInterleave(0),
        }
    }

    /// Table 3-5: transmission TSPs per multiplex frame.
    #[test]
    fn table_3_5() {
        let want = [
            (Mode::M1, [1280, 1152, 1088, 1056]),
            (Mode::M2, [2560, 2304, 2176, 2112]),
            (Mode::M3, [5120, 4608, 4352, 4224]),
        ];
        let gis = [
            GuardInterval::G1_4,
            GuardInterval::G1_8,
            GuardInterval::G1_16,
            GuardInterval::G1_32,
        ];
        for (mode, row) in want {
            for (gi, n) in gis.iter().zip(row) {
                let p = Params::new(
                    mode,
                    *gi,
                    false,
                    [
                        Some(layer(13, Modulation::Qpsk, CodeRate::R1_2)),
                        None,
                        None,
                    ],
                )
                .unwrap();
                assert_eq!(p.multiplex_frame_tsps(), n, "{mode:?} {gi:?}");
            }
        }
    }

    /// Table 3-3: TSPs per segment per frame (Mode 1/2/3).
    #[test]
    fn table_3_3() {
        use CodeRate::*;
        use Modulation::*;
        let rows = [
            (Dqpsk, R1_2, 12),
            (Dqpsk, R2_3, 16),
            (Dqpsk, R3_4, 18),
            (Dqpsk, R5_6, 20),
            (Dqpsk, R7_8, 21),
            (Qam16, R1_2, 24),
            (Qam16, R2_3, 32),
            (Qam16, R3_4, 36),
            (Qam16, R5_6, 40),
            (Qam16, R7_8, 42),
            (Qam64, R1_2, 36),
            (Qam64, R2_3, 48),
            (Qam64, R3_4, 54),
            (Qam64, R5_6, 60),
            (Qam64, R7_8, 63),
        ];
        for (m, r, mode1) in rows {
            for mode in [Mode::M1, Mode::M2, Mode::M3] {
                assert_eq!(layer(1, m, r).tsp_per_frame(mode), mode1 * mode.scale());
            }
        }
    }

    /// Table 3-1: frame length 257.04 ms (Mode 3, GI 1/4), 231.336 ms (1/8),
    /// 53.0145 ms (Mode 1, 1/32).
    #[test]
    fn frame_lengths() {
        let mk = |m, g| {
            Params::new(
                m,
                g,
                false,
                [
                    Some(layer(13, Modulation::Qpsk, CodeRate::R1_2)),
                    None,
                    None,
                ],
            )
            .unwrap()
        };
        assert!((mk(Mode::M3, GuardInterval::G1_4).frame_seconds() - 0.25704).abs() < 1e-12);
        assert!((mk(Mode::M3, GuardInterval::G1_8).frame_seconds() - 0.231336).abs() < 1e-12);
        assert!((mk(Mode::M1, GuardInterval::G1_32).frame_seconds() - 0.0530145).abs() < 1e-12);
        assert_eq!(
            mk(Mode::M3, GuardInterval::G1_4).frame_ticks_27mhz(),
            6_940_080
        );
    }

    /// Table 3-2: carriers and time-interleave lengths.
    #[test]
    fn table_3_2() {
        assert_eq!(Mode::M1.total_carriers(), 1405);
        assert_eq!(Mode::M2.total_carriers(), 2809);
        assert_eq!(Mode::M3.total_carriers(), 5617);
        let i = |m, k| TimeInterleave(k).length(m).unwrap();
        assert_eq!([0, 1, 2, 3].map(|k| i(Mode::M1, k)), [0, 4, 8, 16]);
        assert_eq!([0, 1, 2, 3].map(|k| i(Mode::M2, k)), [0, 2, 4, 8]);
        assert_eq!([0, 1, 2, 3].map(|k| i(Mode::M3, k)), [0, 1, 2, 4]);
    }

    #[test]
    fn segment_ordering_rules() {
        let a = Some(layer(1, Modulation::Qpsk, CodeRate::R2_3));
        let b = Some(layer(12, Modulation::Qam64, CodeRate::R3_4));
        let p = Params::new(Mode::M3, GuardInterval::G1_8, true, [a, b, None]).unwrap();
        assert_eq!(p.layer_of_segment(0), Layer::A);
        assert_eq!(p.layer_of_segment(12), Layer::B);
        // DQPSK after coherent (outside partial reception) is illegal.
        let c = Some(layer(6, Modulation::Qam64, CodeRate::R3_4));
        let d = Some(layer(6, Modulation::Dqpsk, CodeRate::R1_2));
        assert!(Params::new(Mode::M3, GuardInterval::G1_8, true, [a, c, d]).is_err());
        assert!(Params::new(Mode::M3, GuardInterval::G1_8, false, [b, None, None]).is_err());
    }

    /// Invalid combinations are errors, never a panic later in the
    /// modulator (the fields are private, so `Params::new` is the only way
    /// to build one).
    #[test]
    fn invalid_configurations_are_rejected() {
        let l = |n| Some(layer(n, Modulation::Qam64, CodeRate::R3_4));
        let new = |p, ls| Params::new(Mode::M3, GuardInterval::G1_8, p, ls);
        assert_eq!(new(false, [None, None, None]), Err(ParamError::NoLayer));
        assert_eq!(
            new(false, [l(1), l(13), None]),
            Err(ParamError::SegmentSum(14))
        );
        assert_eq!(
            new(false, [l(12), None, None]),
            Err(ParamError::SegmentSum(12))
        );
        assert_eq!(
            new(false, [l(usize::MAX), l(14), None]),
            Err(ParamError::SegmentSum(usize::MAX))
        );
        assert_eq!(
            new(false, [l(13), None, l(0)]),
            Err(ParamError::NonContiguousLayers)
        );
        assert_eq!(
            new(false, [l(0), l(13), None]),
            Err(ParamError::EmptyLayer(Layer::A))
        );
        assert_eq!(
            new(true, [l(2), l(11), None]),
            Err(ParamError::PartialReceptionSegments(2))
        );
        let mut ti = layer(13, Modulation::Qpsk, CodeRate::R1_2);
        ti.time_interleave = TimeInterleave(4);
        assert_eq!(
            new(false, [Some(ti), None, None]),
            Err(ParamError::TimeInterleaveIndex(Layer::A, 4))
        );
        let d = Some(layer(6, Modulation::Dqpsk, CodeRate::R1_2));
        assert_eq!(
            new(false, [l(7), d, None]),
            Err(ParamError::DifferentialAfterCoherent(Layer::B))
        );
        let p = new(true, [l(1), l(12), None]).unwrap();
        assert_eq!(p.mode(), Mode::M3);
        assert_eq!(p.guard(), GuardInterval::G1_8);
        assert!(p.partial_reception());
        assert_eq!(p.layer(Layer::B).map(|x| x.segments), Some(12));
        assert_eq!(p.layer(Layer::C), None);
        assert_eq!(p.layers()[0].map(|x| x.segments), Some(1));
    }
}

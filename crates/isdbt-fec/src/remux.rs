//! TS re-multiplexing: multiplex-frame pattern from the model receiver
//! (ARIB STD-B31 §3.2, Tables 3-5, 3-6, Figs. 3-3 .. 3-5) and the hierarchical
//! division / TS reproduction built on it (§3.4).
//!
//! > "The arrangement of transmission TSPs within a multiplex frame is
//! > determined in advance to ensure that it is identical to that of the TSs
//! > that will be reproduced by the model receiver shown in Fig. 3-4."
//! > — §3.2.1
//!
//! [`MultiplexPattern::new`] runs that model receiver as a clock-level
//! simulation (one clock = one FFT sample, §3.2.2):
//!
//! * **Input (§3.2.2.1, Fig. 3-5).** Each OFDM symbol lasts
//!   `fft_size + guard` clocks. In it, the data carriers (nc per segment,
//!   Table 3-1) of segments 0..12 arrive one per clock in ascending segment
//!   order — layer A's segments first, then B, then C — followed by a null
//!   signal for the remaining clocks (pilots, out-of-band FFT bins, guard).
//! * **Hierarchical buffer (§3.2.2.2).** On the *k*-th datum of layer X in
//!   the multiplex frame, B_X,k = 2([k·S_X·R_X] − [(k−1)·S_X·R_X]) depunctured
//!   bits are stored (S_X: Table 3-6). Whenever 408 bytes (= 3264 bits, one
//!   TSP after the rate-1/2 mother code) have accumulated, S1 moves that
//!   packet instantly into the TS buffer of the TS reproduction section S3
//!   currently selects. A datum input during clock *c* is complete at the
//!   end of that clock (time *c* + 1).
//! * **TS reproduction (§3.2.2.2).** Every 408 clocks the section whose
//!   output S4 selects reads one TSP from its TS buffer if at least one whole
//!   packet is present (FIFO; packets of different layers keep their
//!   completion order), otherwise emits a null TSP.
//! * **S3/S4 (§3.2.2.2).** S3 alternates between the two sections at the
//!   start of every OFDM frame (Mode 1), half frame = 102 symbols (Mode 2)
//!   or quarter frame = 51 symbols (Mode 3). S4 follows S3 3 × 408 clocks
//!   later, "that is, at the beginning of an OFDM frame": multiplex-frame
//!   slot *j* is therefore the read at time 1224 + 408·*j* clocks after the
//!   model receiver's frame input started.
//!
//! The simulation asserts its own consistency: every packet produced in the
//! frame is read out inside the frame, and a section is empty whenever S4
//! leaves it. See SPEC-NOTES.md for the interpretation of each step and
//! for the comparison with Fig. 3-3 and the reference Fig. 3-8 of Part 2.

use isdbt_core::fec_params::MOTHER_CODED_TSP_BYTES;
use isdbt_core::{Layer, Mode, Params, SYMBOLS_PER_FRAME, TSP};

use crate::FecError;

/// Model-receiver clocks per TS-packet period: 408 (§3.2.2.2).
pub const TSP_PERIOD_CLOCKS: usize = MOTHER_CODED_TSP_BYTES;
/// S4 lags S3 by three TS-packet periods (§3.2.2.2: "408 × 3 clocks").
pub const S4_DELAY_CLOCKS: usize = 3 * TSP_PERIOD_CLOCKS;
/// Depunctured bits per TS packet in the hierarchical buffer: 408 × 8.
const PACKET_BITS: u64 = (MOTHER_CODED_TSP_BYTES * 8) as u64;

/// Number of S3/S4 switching intervals per OFDM frame: 1 (Mode 1), 2
/// (Mode 2, every 102 symbols), 4 (Mode 3, every 51 symbols) (§3.2.2.2).
pub fn switch_intervals_per_frame(mode: Mode) -> usize {
    mode.scale()
}

/// One multiplex-frame slot (§3.2.1): a transmission TSP of layer A/B/C, or
/// a null TSP that is not transmitted.
pub type Slot = Option<Layer>;

/// The TSP arrangement of one multiplex frame (§3.2.1, Fig. 3-3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MultiplexPattern {
    slots: Vec<Slot>,
    counts: [usize; 3],
}

#[derive(Clone, Copy)]
struct Completion {
    time: u64,
    layer: Layer,
}

impl MultiplexPattern {
    /// Derive the pattern for `params` by simulating the §3.2.2 model
    /// receiver over one frame. The slot count equals Table 3-5's n and the
    /// per-layer counts equal Table 3-3 (asserted).
    pub fn new(params: &Params) -> Self {
        Self::simulate(params, true)
    }

    /// The same simulation with the opposite tie rule at the 408-clock read
    /// instants (a datum input during clock *c* counts as present at read
    /// time *c*); used to show the pattern does not depend on it.
    #[doc(hidden)]
    pub fn new_inclusive_tie(params: &Params) -> Self {
        Self::simulate(params, false)
    }

    fn simulate(params: &Params, strict: bool) -> Self {
        let mode = params.mode;
        let sym = params.samples_per_symbol() as u64;
        let n = params.multiplex_frame_tsps();
        let intervals = switch_intervals_per_frame(mode);
        let interval_clocks = sym * (SYMBOLS_PER_FRAME / intervals) as u64;
        let nc = mode.data_carriers_per_segment();

        // Packet completions per S3 section-interval, in time order.
        let mut sections: Vec<Vec<Completion>> = vec![Vec::new(); intervals];
        let mut offset = 0u64; // clock offset of the layer's first carrier in a symbol
        let mut events: Vec<Completion> = Vec::new();
        for (id, l) in params.active_layers() {
            let carriers = (nc * l.segments) as u64;
            let s = l.modulation.bits_per_symbol() as u64;
            let (num, den) = l.code_rate.ratio();
            let (num, den) = (num as u64, den as u64);
            let mut k: u64 = 0;
            let mut packets: u64 = 0;
            for symbol in 0..SYMBOLS_PER_FRAME as u64 {
                for i in 0..carriers {
                    k += 1;
                    // Σ B_X,k telescopes to 2·[k·S·R].
                    let bits = 2 * (k * s * num / den);
                    while bits >= PACKET_BITS * (packets + 1) {
                        packets += 1;
                        let input_clock = symbol * sym + offset + i;
                        events.push(Completion { time: input_clock + u64::from(strict), layer: id });
                    }
                }
            }
            assert_eq!(packets as usize, l.tsp_per_frame(mode), "Table 3-3 count for layer {id}");
            offset += carriers;
        }
        assert!(offset <= sym, "data carriers exceed the symbol period");
        events.sort_by_key(|e| e.time);
        for e in events {
            // S3 position when the packet is transferred (input_clock = time − strict).
            let input_clock = e.time - u64::from(strict);
            let q = (input_clock / interval_clocks) as usize;
            sections[q].push(e);
        }

        let mut slots = Vec::with_capacity(n);
        let mut counts = [0usize; 3];
        let mut heads = vec![0usize; intervals];
        for j in 0..n {
            let t = (S4_DELAY_CLOCKS + TSP_PERIOD_CLOCKS * j) as u64;
            let q = ((TSP_PERIOD_CLOCKS * j) as u64 / interval_clocks) as usize;
            if q > 0 && heads[q - 1] < sections[q - 1].len() && (TSP_PERIOD_CLOCKS * j) as u64 % interval_clocks == 0 {
                panic!("model receiver: section {} not drained when S4 switched", q - 1);
            }
            let sec = &sections[q];
            let h = &mut heads[q];
            if *h < sec.len() && sec[*h].time <= t {
                let l = sec[*h].layer;
                *h += 1;
                counts[l.index()] += 1;
                slots.push(Some(l));
            } else {
                slots.push(None);
            }
        }
        for (q, sec) in sections.iter().enumerate() {
            assert_eq!(heads[q], sec.len(), "model receiver: packets left in section {q} at frame end");
        }
        Self { slots, counts }
    }

    /// The n slots of the multiplex frame in transmission order (Table 3-5).
    pub fn slots(&self) -> &[Slot] {
        &self.slots
    }

    /// n, the number of transmission TSPs per multiplex frame (Table 3-5).
    pub fn len(&self) -> usize {
        self.slots.len()
    }

    /// Always false (n ≥ 1056).
    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// Transmission TSPs of `layer` per multiplex frame (Table 3-3 × N).
    pub fn count(&self, layer: Layer) -> usize {
        self.counts[layer.index()]
    }

    /// Null TSPs per multiplex frame.
    pub fn null_count(&self) -> usize {
        self.slots.len() - self.counts.iter().sum::<usize>()
    }

    /// TX re-multiplexer (§3.2.1): build one multiplex frame from per-layer
    /// packet sequences (each exactly `count(layer)` long, in layer order),
    /// filling null slots with `null()`.
    pub fn assemble<P: Clone>(&self, layers: [&[P]; 3], mut null: impl FnMut() -> P) -> Result<Vec<P>, FecError> {
        for l in Layer::ALL {
            if layers[l.index()].len() != self.count(l) {
                return Err(FecError::LayerCount { layer: l, expected: self.count(l), got: layers[l.index()].len() });
            }
        }
        let mut next = [0usize; 3];
        Ok(self
            .slots
            .iter()
            .map(|s| match s {
                Some(l) => {
                    let i = l.index();
                    next[i] += 1;
                    layers[i][next[i] - 1].clone()
                }
                None => null(),
            })
            .collect())
    }

    /// Hierarchical divider (§3.4): split one multiplex frame (n packets)
    /// into the per-layer sequences, dropping null slots.
    pub fn divide<P: Clone>(&self, frame: &[P]) -> Result<[Vec<P>; 3], FecError> {
        if frame.len() != self.len() {
            return Err(FecError::FrameLength { expected: self.len(), got: frame.len() });
        }
        let mut out: [Vec<P>; 3] = Default::default();
        for (s, p) in self.slots.iter().zip(frame) {
            if let Some(l) = s {
                out[l.index()].push(p.clone());
            }
        }
        Ok(out)
    }

    /// Receiver TS reproduction (§3.2.2.2, Fig. 3-4): interleave the
    /// per-layer decoded 188-byte TSPs of one frame into the single TS in
    /// the multiplex-frame order, inserting null packets at null slots.
    pub fn reproduce_ts(&self, layers: [&[[u8; TSP]]; 3]) -> Result<Vec<[u8; TSP]>, FecError> {
        self.assemble(layers, null_tsp)
    }
}

/// An MPEG-2 null packet (ITU-T H.222.0 §2.4.3.2–3: PID 0x1FFF, payload
/// only, payload bytes 0xFF), used for the null slots of the reproduced TS.
pub fn null_tsp() -> [u8; TSP] {
    let mut p = [0xFFu8; TSP];
    p[0] = 0x47;
    p[1] = 0x1F;
    p[2] = 0xFF;
    p[3] = 0x10;
    p
}

#[cfg(test)]
mod tests {
    use super::*;
    use isdbt_core::{CodeRate, GuardInterval, LayerConfig, Modulation, TimeInterleave};

    fn lc(segments: usize, modulation: Modulation, code_rate: CodeRate) -> LayerConfig {
        LayerConfig { segments, modulation, code_rate, time_interleave: TimeInterleave(0) }
    }

    fn fig_3_5_params() -> Params {
        Params::new(
            Mode::M1,
            GuardInterval::G1_8,
            false,
            [Some(lc(5, Modulation::Dqpsk, CodeRate::R1_2)), Some(lc(8, Modulation::Qam64, CodeRate::R7_8)), None],
        )
        .unwrap()
    }

    /// Fig. 3-5 example (Mode 1, GI 1/8, DQPSK 1/2 ×5 + 64QAM 7/8 ×8):
    /// n = 1152 (Table 3-5), A = 60, B = 504 (Table 3-3), and the frame
    /// starts "TSP_B, TSP_B, TSP_null" and ends with a null as in Fig. 3-3.
    #[test]
    fn spec_fig_3_3_and_3_5_example() {
        let p = MultiplexPattern::new(&fig_3_5_params());
        assert_eq!(p.len(), 1152);
        assert_eq!(p.count(Layer::A), 60);
        assert_eq!(p.count(Layer::B), 504);
        assert_eq!(p.count(Layer::C), 0);
        assert_eq!(&p.slots()[..3], &[Some(Layer::B), Some(Layer::B), None]);
        assert_eq!(p.slots()[1151], None);
    }

    /// Divide ∘ assemble = identity on the per-layer sequences.
    #[test]
    fn assemble_divide_roundtrip() {
        let par = fig_3_5_params();
        let p = MultiplexPattern::new(&par);
        let a: Vec<u32> = (0..60).collect();
        let b: Vec<u32> = (1000..1504).collect();
        let f = p.assemble([&a, &b, &[]], || u32::MAX).unwrap();
        assert_eq!(f.len(), 1152);
        let [da, db, dc] = p.divide(&f).unwrap();
        assert_eq!((&da, &db, dc.len()), (&a, &b, 0));
        assert!(p.assemble([&a[1..], &b, &[]], || 0).is_err());
    }
}

//! TMCC signal (ARIB STD-B31 §3.15, Tables 3-20..3-31).
//!
//! A TMCC frame is 204 bits B0..B203 carried one bit per OFDM symbol on
//! every TMCC carrier (DBPSK, §3.13.3):
//!
//! | bits | content |
//! |---|---|
//! | B0 | differential reference (B'0 = W of the carrier) |
//! | B1–B16 | sync word w0 = 0011010111101110 / w1 = !w0, alternating per frame (Table 3-21) |
//! | B17–B19 | segment type: 111 differential, 000 coherent |
//! | B20–B121 | TMCC information (Table 3-22), 102 bits |
//! | B122–B203 | parity, shortened (184,102) difference-set code (§3.15.6.9) |
//!
//! Multi-bit fields are written most significant bit first in ascending
//! B order (the tables print "B28–B30 = 001" with B28 leftmost).

use crate::dsc::{self, ParityStatus};
use crate::Error;
use isdbt_core::{CodeRate, GuardInterval, LayerConfig, Mode, Modulation, Params, TimeInterleave};

/// Sync word w0 (§3.15.4).
pub const W0: [u8; 16] = [0, 0, 1, 1, 0, 1, 0, 1, 1, 1, 1, 0, 1, 1, 1, 0];

/// Sync word for a frame: w0 for even `odd == false`, w1 = !w0 otherwise.
pub fn sync_word(odd: bool) -> [u8; 16] {
    W0.map(|b| b ^ u8::from(odd))
}

/// Segment type identification B17–B19 (§3.15.5).
pub fn segment_type_bits(differential: bool) -> [u8; 3] {
    if differential { [1, 1, 1] } else { [0, 0, 0] }
}

/// Transmission-parameter information of one layer (Table 3-23), raw codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TmccLayer {
    /// Carrier modulation, Table 3-28 (3 bits; 111 = unused).
    pub modulation: u8,
    /// Convolutional-coding rate, Table 3-29 (3 bits; 111 = unused).
    pub code_rate: u8,
    /// Time-interleaving length, Table 3-30 (3 bits; 111 = unused).
    pub interleave: u8,
    /// Number of segments, Table 3-31 (4 bits; 1111 = unused).
    pub segments: u8,
}

impl TmccLayer {
    /// Unused hierarchical layer / no next information: all ones.
    pub const UNUSED: Self = Self { modulation: 0b111, code_rate: 0b111, interleave: 0b111, segments: 0b1111 };

    /// Codes for a configured layer.
    pub fn from_config(l: &LayerConfig) -> Self {
        let modulation = match l.modulation {
            Modulation::Dqpsk => 0,
            Modulation::Qpsk => 1,
            Modulation::Qam16 => 2,
            Modulation::Qam64 => 3,
        };
        let code_rate = match l.code_rate {
            CodeRate::R1_2 => 0,
            CodeRate::R2_3 => 1,
            CodeRate::R3_4 => 2,
            CodeRate::R5_6 => 3,
            CodeRate::R7_8 => 4,
        };
        Self { modulation, code_rate, interleave: l.time_interleave.0, segments: l.segments as u8 }
    }

    /// True if every field has its "unused" code.
    pub fn is_unused(&self) -> bool {
        *self == Self::UNUSED
    }

    /// Typed configuration; `Ok(None)` for an unused layer, `Err` for
    /// reserved / inconsistent codes.
    pub fn to_config(&self) -> Result<Option<LayerConfig>, Error> {
        if self.modulation == 0b111 {
            return Ok(None);
        }
        let modulation = match self.modulation {
            0 => Modulation::Dqpsk,
            1 => Modulation::Qpsk,
            2 => Modulation::Qam16,
            3 => Modulation::Qam64,
            v => return Err(Error::Tmcc(format!("reserved modulation code {v:03b}"))),
        };
        let code_rate = match self.code_rate {
            0 => CodeRate::R1_2,
            1 => CodeRate::R2_3,
            2 => CodeRate::R3_4,
            3 => CodeRate::R5_6,
            4 => CodeRate::R7_8,
            v => return Err(Error::Tmcc(format!("reserved code-rate code {v:03b}"))),
        };
        if self.interleave > 3 {
            return Err(Error::Tmcc(format!("reserved/unused interleave code {:03b}", self.interleave)));
        }
        if !(1..=13).contains(&self.segments) {
            return Err(Error::Tmcc(format!("reserved segment count code {:04b}", self.segments)));
        }
        Ok(Some(LayerConfig {
            segments: self.segments as usize,
            modulation,
            code_rate,
            time_interleave: TimeInterleave(self.interleave),
        }))
    }
}

/// Current or next transmission parameters (partial-reception flag +
/// layers A, B, C), Table 3-22.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TmccParams {
    /// Partial-reception flag (B27 / B67), Table 3-27.
    pub partial_reception: bool,
    /// Layers A, B, C.
    pub layers: [TmccLayer; 3],
}

impl TmccParams {
    /// "No next information": flag 1 and all layers unused (§3.15.6.4–8).
    pub const NONE: Self = Self { partial_reception: true, layers: [TmccLayer::UNUSED; 3] };

    /// From validated parameters.
    pub fn from_params(p: &Params) -> Self {
        Self {
            partial_reception: p.partial_reception,
            layers: p.layers.map(|l| l.as_ref().map_or(TmccLayer::UNUSED, TmccLayer::from_config)),
        }
    }

    /// Validated parameters for a known mode / guard interval.
    pub fn to_params(&self, mode: Mode, guard: GuardInterval) -> Result<Params, Error> {
        let mut layers = [None; 3];
        for (dst, l) in layers.iter_mut().zip(&self.layers) {
            *dst = l.to_config()?;
        }
        Params::new(mode, guard, self.partial_reception, layers).map_err(|e| Error::Tmcc(e.to_string()))
    }
}

/// TMCC information B20–B121 (Table 3-22), lossless.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TmccInfo {
    /// System identification B20–B21 (Table 3-24; 00 = television).
    pub system_id: u8,
    /// Indicator of transmission-parameter switching B22–B25 (Table 3-25; 1111 normal).
    pub switching_countdown: u8,
    /// Startup control (emergency-alarm start flag) B26 (Table 3-26).
    pub emergency_alarm: bool,
    /// Current information B27–B66.
    pub current: TmccParams,
    /// Next information B67–B106.
    pub next: TmccParams,
    /// Phase-shift-correction value for connected transmission B107–B109 (111).
    pub phase_shift_correction: u8,
    /// Reserved B110–B121 (12 bits, all 1).
    pub reserved: u16,
}

impl TmccInfo {
    /// Information for steady transmission of `p`: system 00, countdown
    /// 1111, no emergency flag, next = current (no switching announced),
    /// phase-correction 111, reserved all ones.
    pub fn from_params(p: &Params) -> Self {
        let cur = TmccParams::from_params(p);
        Self {
            system_id: 0,
            switching_countdown: 0b1111,
            emergency_alarm: false,
            current: cur,
            next: cur,
            phase_shift_correction: 0b111,
            reserved: 0xfff,
        }
    }

    /// The current parameters as validated [`Params`].
    pub fn params(&self, mode: Mode, guard: GuardInterval) -> Result<Params, Error> {
        self.current.to_params(mode, guard)
    }

    /// Serialise B20..B121 (102 bits).
    pub fn to_bits(&self) -> [u8; 102] {
        let mut w = BitWriter::default();
        w.put(self.system_id as u32, 2);
        w.put(self.switching_countdown as u32, 4);
        w.put(self.emergency_alarm as u32, 1);
        for p in [&self.current, &self.next] {
            w.put(p.partial_reception as u32, 1);
            for l in &p.layers {
                w.put(l.modulation as u32, 3);
                w.put(l.code_rate as u32, 3);
                w.put(l.interleave as u32, 3);
                w.put(l.segments as u32, 4);
            }
        }
        w.put(self.phase_shift_correction as u32, 3);
        w.put(self.reserved as u32, 12);
        assert_eq!(w.bits.len(), 102);
        w.bits.try_into().expect("102 bits")
    }

    /// Parse B20..B121.
    pub fn from_bits(bits: &[u8; 102]) -> Self {
        let mut r = BitReader { bits, pos: 0 };
        let system_id = r.get(2) as u8;
        let switching_countdown = r.get(4) as u8;
        let emergency_alarm = r.get(1) == 1;
        let mut tp = || {
            let partial_reception = r.get(1) == 1;
            let layers = std::array::from_fn(|_| TmccLayer {
                modulation: r.get(3) as u8,
                code_rate: r.get(3) as u8,
                interleave: r.get(3) as u8,
                segments: r.get(4) as u8,
            });
            TmccParams { partial_reception, layers }
        };
        let current = tp();
        let next = tp();
        let phase_shift_correction = r.get(3) as u8;
        let reserved = r.get(12) as u16;
        Self { system_id, switching_countdown, emergency_alarm, current, next, phase_shift_correction, reserved }
    }

    /// Full 204-bit TMCC frame B0..B203 for a segment of the given type in
    /// a frame of the given sync parity.  B0 is returned as 0 (it is not
    /// information; the transmitted B'0 is W, §3.13.3).
    pub fn frame_bits(&self, odd_frame: bool, differential: bool) -> [u8; 204] {
        let mut out = [0u8; 204];
        out[1..17].copy_from_slice(&sync_word(odd_frame));
        out[17..20].copy_from_slice(&segment_type_bits(differential));
        let info = self.to_bits();
        out[20..122].copy_from_slice(&info);
        out[122..204].copy_from_slice(&dsc::parity(&info));
        out
    }
}

/// Result of decoding one TMCC frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TmccDecoded {
    /// Information (after majority-logic correction if needed).
    pub info: TmccInfo,
    /// Sync word: `Some(false)` = w0, `Some(true)` = w1, `None` = neither.
    pub sync_odd: Option<bool>,
    /// Segment type bits B17–B19 as received.
    pub segment_type: [u8; 3],
    /// Parity status of the (184,102) code.
    pub parity: ParityStatus,
}

/// Decode a 204-bit TMCC frame (B0 ignored).
pub fn decode_frame(bits: &[u8; 204]) -> TmccDecoded {
    let sync = &bits[1..17];
    let sync_odd = if sync == W0 {
        Some(false)
    } else if sync == sync_word(true) {
        Some(true)
    } else {
        None
    };
    let mut word = [0u8; 184];
    word.copy_from_slice(&bits[20..204]);
    let parity = dsc::decode(&mut word);
    let info_bits: [u8; 102] = word[..102].try_into().expect("102");
    TmccDecoded {
        info: TmccInfo::from_bits(&info_bits),
        sync_odd,
        segment_type: [bits[17], bits[18], bits[19]],
        parity,
    }
}

#[derive(Default)]
pub(crate) struct BitWriter {
    pub bits: Vec<u8>,
}

impl BitWriter {
    pub fn put(&mut self, v: u32, n: usize) {
        for i in (0..n).rev() {
            self.bits.push(((v >> i) & 1) as u8);
        }
    }
}

pub(crate) struct BitReader<'a> {
    pub bits: &'a [u8],
    pub pos: usize,
}

impl BitReader<'_> {
    pub fn get(&mut self, n: usize) -> u32 {
        let mut v = 0;
        for _ in 0..n {
            v = (v << 1) | self.bits[self.pos] as u32;
            self.pos += 1;
        }
        v
    }
}

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

use std::fmt;

use crate::Error;
use crate::dsc::{self, ParityStatus};
use isdbt_core::{
    CodeRate, GuardInterval, LayerConfig, Mode, Modulation, Params, Segments, TimeInterleave,
};

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

/// A code one of Tables 3-28 … 3-31 does not assign, as decoded.
///
/// Only the `from_code` of [`ModulationCode`], [`RateCode`],
/// [`InterleaveCode`] and [`SegmentsCode`] builds one, after it has
/// excluded every assigned code and the all-ones "unused" code: the field
/// is private, so a `Reserved(c)` is canonical by construction (one value
/// per wire code, never the spelling of `Unused`) and `==` on the enums is
/// wire equality.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ReservedCode(u8);

impl ReservedCode {
    /// `c` must be a reserved code of its table, already masked to the
    /// field width; only the `from_code`s uphold this.
    pub(crate) const fn new_unchecked(c: u8) -> Self {
        Self(c)
    }

    /// The code, in the width of its field (3 or 4 bits).
    pub const fn get(self) -> u8 {
        self.0
    }
}

impl fmt::Display for ReservedCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

impl fmt::Binary for ReservedCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Binary::fmt(&self.0, f)
    }
}

impl fmt::LowerHex for ReservedCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::LowerHex::fmt(&self.0, f)
    }
}

/// A reserved code cannot be spelled as a TMCC layer code outside this
/// crate; the only way in is `from_code`, so a decoded value is canonical:
///
/// ```compile_fail,E0624
/// use isdbt_ofdm::tmcc::{ModulationCode, ReservedCode};
/// let _ = ModulationCode::Reserved(ReservedCode::new_unchecked(0b101));
/// ```
///
/// ```
/// use isdbt_ofdm::tmcc::ModulationCode;
/// assert_eq!(ModulationCode::from_code(0b101), ModulationCode::from_code(0b1101));
/// assert_eq!(ModulationCode::from_code(0b101).code(), 0b101);
/// ```
#[doc(hidden)]
pub const _TMCC_LAYER_CODES_CANONICAL: () = ();

/// Carrier modulation mapping scheme code, Table 3-28 (3 bits, §3.15.6.5).
///
/// Lossless: every 3-bit code has a variant, so a received TMCC word
/// re-encodes bit for bit (`from_code(c).code() == c`). The `Reserved`
/// payload is a [`ReservedCode`] only `from_code` can build, so every value
/// is canonical and `==` is wire equality.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ModulationCode {
    /// '000' DQPSK, '001' QPSK, '010' 16QAM, '011' 64QAM.
    Assigned(Modulation),
    /// '100'–'110' "Reserved"; the code as decoded.
    Reserved(ReservedCode),
    /// '111' "Unused hierarchical layer" (also "no next information").
    Unused,
}

impl ModulationCode {
    /// The 3-bit code.
    pub const fn code(self) -> u8 {
        match self {
            ModulationCode::Assigned(Modulation::Dqpsk) => 0b000,
            ModulationCode::Assigned(Modulation::Qpsk) => 0b001,
            ModulationCode::Assigned(Modulation::Qam16) => 0b010,
            ModulationCode::Assigned(Modulation::Qam64) => 0b011,
            ModulationCode::Reserved(c) => c.get(),
            ModulationCode::Unused => 0b111,
        }
    }

    /// From the 3-bit code (higher bits of `code` are ignored).
    pub const fn from_code(code: u8) -> Self {
        match code & 0b111 {
            0b000 => ModulationCode::Assigned(Modulation::Dqpsk),
            0b001 => ModulationCode::Assigned(Modulation::Qpsk),
            0b010 => ModulationCode::Assigned(Modulation::Qam16),
            0b011 => ModulationCode::Assigned(Modulation::Qam64),
            0b111 => ModulationCode::Unused,
            c => ModulationCode::Reserved(ReservedCode::new_unchecked(c)),
        }
    }
}

/// Convolutional-coding rate code, Table 3-29 (3 bits, §3.15.6.6).
/// Lossless like [`ModulationCode`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RateCode {
    /// '000' 1/2, '001' 2/3, '010' 3/4, '011' 5/6, '100' 7/8.
    Assigned(CodeRate),
    /// '101'–'110' "Reserved"; the code as decoded.
    Reserved(ReservedCode),
    /// '111' "Unused hierarchical layer" (also "no next information").
    Unused,
}

impl RateCode {
    /// The 3-bit code.
    pub const fn code(self) -> u8 {
        match self {
            RateCode::Assigned(CodeRate::R1_2) => 0b000,
            RateCode::Assigned(CodeRate::R2_3) => 0b001,
            RateCode::Assigned(CodeRate::R3_4) => 0b010,
            RateCode::Assigned(CodeRate::R5_6) => 0b011,
            RateCode::Assigned(CodeRate::R7_8) => 0b100,
            RateCode::Reserved(c) => c.get(),
            RateCode::Unused => 0b111,
        }
    }

    /// From the 3-bit code (higher bits of `code` are ignored).
    pub const fn from_code(code: u8) -> Self {
        match code & 0b111 {
            0b000 => RateCode::Assigned(CodeRate::R1_2),
            0b001 => RateCode::Assigned(CodeRate::R2_3),
            0b010 => RateCode::Assigned(CodeRate::R3_4),
            0b011 => RateCode::Assigned(CodeRate::R5_6),
            0b100 => RateCode::Assigned(CodeRate::R7_8),
            0b111 => RateCode::Unused,
            c => RateCode::Reserved(ReservedCode::new_unchecked(c)),
        }
    }
}

/// Time-interleaving length code, Table 3-30 (3 bits, §3.15.6.7).
/// Lossless like [`ModulationCode`]; the assigned codes are the Table
/// 3-12 indices of [`TimeInterleave`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InterleaveCode {
    /// '000'–'011': I = 0 / 4 / 8 / 16 (Mode 1), 0 / 2 / 4 / 8 (Mode 2),
    /// 0 / 1 / 2 / 4 (Mode 3).
    Assigned(TimeInterleave),
    /// '100' "Not used" (assigned to digital terrestrial sound
    /// broadcasting, "not used in this system") and '101'–'110'
    /// "Reserved"; the code as decoded.
    Reserved(ReservedCode),
    /// '111' "Unused hierarchical layer" (also "no next information").
    Unused,
}

impl InterleaveCode {
    /// The 3-bit code.
    pub const fn code(self) -> u8 {
        match self {
            InterleaveCode::Assigned(t) => t.code(),
            InterleaveCode::Reserved(c) => c.get(),
            InterleaveCode::Unused => 0b111,
        }
    }

    /// From the 3-bit code (higher bits of `code` are ignored).
    pub const fn from_code(code: u8) -> Self {
        let code = code & 0b111;
        match TimeInterleave::from_code(code) {
            Some(t) => InterleaveCode::Assigned(t),
            None if code == 0b111 => InterleaveCode::Unused,
            None => InterleaveCode::Reserved(ReservedCode::new_unchecked(code)),
        }
    }
}

/// Number-of-segments code, Table 3-31 (4 bits, §3.15.6.8).
/// Lossless like [`ModulationCode`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SegmentsCode {
    /// '0001'–'1101': 1 to 13 segments (the count, equal to the code).
    Segments(Segments),
    /// '0000' and '1110' "Reserved"; the code as decoded.
    Reserved(ReservedCode),
    /// '1111' "Unused hierarchical layer" (also "no next information").
    Unused,
}

impl SegmentsCode {
    /// The 4-bit code.
    pub const fn code(self) -> u8 {
        match self {
            SegmentsCode::Segments(n) => n.get(),
            SegmentsCode::Reserved(n) => n.get(),
            SegmentsCode::Unused => 0b1111,
        }
    }

    /// From the 4-bit code (higher bits of `code` are ignored).
    pub const fn from_code(code: u8) -> Self {
        let code = code & 0b1111;
        match Segments::new(code) {
            Some(n) => SegmentsCode::Segments(n),
            None if code == 0b1111 => SegmentsCode::Unused,
            None => SegmentsCode::Reserved(ReservedCode::new_unchecked(code)),
        }
    }
}

/// Transmission-parameter information of one layer (Table 3-23): the four
/// codes as received, each a lossless coded enum of its table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TmccLayer {
    /// Carrier modulation, Table 3-28.
    pub modulation: ModulationCode,
    /// Convolutional-coding rate, Table 3-29.
    pub code_rate: RateCode,
    /// Time-interleaving length, Table 3-30.
    pub interleave: InterleaveCode,
    /// Number of segments, Table 3-31.
    pub segments: SegmentsCode,
}

impl TmccLayer {
    /// Unused hierarchical layer / no next information: all ones.
    pub const UNUSED: Self = Self {
        modulation: ModulationCode::Unused,
        code_rate: RateCode::Unused,
        interleave: InterleaveCode::Unused,
        segments: SegmentsCode::Unused,
    };

    /// Codes for a configured layer: every field has an assigned code (the
    /// `LayerConfig` types are closed / bounded, so none is reserved).
    pub fn from_config(l: &LayerConfig) -> Self {
        Self {
            modulation: ModulationCode::Assigned(l.modulation),
            code_rate: RateCode::Assigned(l.code_rate),
            interleave: InterleaveCode::Assigned(l.time_interleave),
            segments: SegmentsCode::Segments(l.segments),
        }
    }

    /// True if every field has its "unused" code.
    pub fn is_unused(&self) -> bool {
        *self == Self::UNUSED
    }

    /// Typed configuration; `Ok(None)` for an unused layer (modulation
    /// '111'), `Err` for reserved / inconsistent codes.
    pub fn config(self) -> Result<Option<LayerConfig>, Error> {
        let modulation = match self.modulation {
            ModulationCode::Unused => return Ok(None),
            ModulationCode::Assigned(m) => m,
            ModulationCode::Reserved(c) => {
                return Err(Error::Tmcc(format!("reserved modulation code {c:03b}")));
            }
        };
        let code_rate = match self.code_rate {
            RateCode::Assigned(r) => r,
            RateCode::Reserved(c) => {
                return Err(Error::Tmcc(format!("reserved code-rate code {c:03b}")));
            }
            RateCode::Unused => {
                return Err(Error::Tmcc(
                    "unused code-rate code 111 in a used layer".into(),
                ));
            }
        };
        let time_interleave = match self.interleave {
            InterleaveCode::Assigned(t) => t,
            c @ (InterleaveCode::Reserved(_) | InterleaveCode::Unused) => {
                return Err(Error::Tmcc(format!(
                    "reserved/unused interleave code {:03b}",
                    c.code()
                )));
            }
        };
        let segments = match self.segments {
            SegmentsCode::Segments(n) => n,
            c @ (SegmentsCode::Reserved(_) | SegmentsCode::Unused) => {
                return Err(Error::Tmcc(format!(
                    "reserved segment count code {:04b}",
                    c.code()
                )));
            }
        };
        Ok(Some(LayerConfig {
            segments,
            modulation,
            code_rate,
            time_interleave,
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
    pub const NONE: Self = Self {
        partial_reception: true,
        layers: [TmccLayer::UNUSED; 3],
    };

    /// From validated parameters.
    pub fn from_params(p: &Params) -> Self {
        Self {
            partial_reception: p.partial_reception(),
            layers: p
                .layers()
                .map(|l| l.as_ref().map_or(TmccLayer::UNUSED, TmccLayer::from_config)),
        }
    }

    /// Validated parameters for a known mode / guard interval.
    pub fn to_params(&self, mode: Mode, guard: GuardInterval) -> Result<Params, Error> {
        let mut layers = [None; 3];
        for (dst, l) in layers.iter_mut().zip(&self.layers) {
            *dst = l.config()?;
        }
        Params::new(mode, guard, self.partial_reception, layers)
            .map_err(|e| Error::Tmcc(e.to_string()))
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
        w.put(u32::from(self.system_id), 2);
        w.put(u32::from(self.switching_countdown), 4);
        w.put(u32::from(self.emergency_alarm), 1);
        for p in [&self.current, &self.next] {
            w.put(u32::from(p.partial_reception), 1);
            for l in &p.layers {
                w.put(u32::from(l.modulation.code()), 3);
                w.put(u32::from(l.code_rate.code()), 3);
                w.put(u32::from(l.interleave.code()), 3);
                w.put(u32::from(l.segments.code()), 4);
            }
        }
        w.put(u32::from(self.phase_shift_correction), 3);
        w.put(u32::from(self.reserved), 12);
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
                modulation: ModulationCode::from_code(r.get(3) as u8),
                code_rate: RateCode::from_code(r.get(3) as u8),
                interleave: InterleaveCode::from_code(r.get(3) as u8),
                segments: SegmentsCode::from_code(r.get(4) as u8),
            });
            TmccParams {
                partial_reception,
                layers,
            }
        };
        let current = tp();
        let next = tp();
        let phase_shift_correction = r.get(3) as u8;
        let reserved = r.get(12) as u16;
        Self {
            system_id,
            switching_countdown,
            emergency_alarm,
            current,
            next,
            phase_shift_correction,
            reserved,
        }
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
            v = (v << 1) | u32::from(self.bits[self.pos]);
            self.pos += 1;
        }
        v
    }
}

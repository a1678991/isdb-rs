//! Broadcasting-TS additional information (ARIB STD-B31 v2.2 Part 2
//! Ch. 5 §5.5, Tables 5-5 .. 5-13): the 204-byte re-multiplexed TS that
//! carries the transmission control information from the studio to the
//! transmitter (interface point I/F (2) of Fig. 5-1).  It is carried in two
//! places:
//!
//! * the **dummy-byte part** (§5.5.2): 8 bytes after the 188-byte
//!   information part of every TSP hold `ISDB-T_information` (Table 5-6) —
//!   the TMCC identifier, frame-head / frame-parity flags, the layer the
//!   TSP is transmitted on, the countdown index, a 13-bit TSP counter and
//!   4 bytes of AC data; the last 8 bytes of the 204 carry an optional
//!   shortened RS(204,196) parity over the first 196 (Fig. 5-10);
//! * the **IIP** (`ISDB-T_Information_Packet`, §5.5.3, Table 5-8): one
//!   188-byte TSP per multiplex frame in a slot of no layer
//!   (layer_indicator '1000') carrying
//!   `modulation_control_configuration_information` (Table 5-10: the
//!   OFDM-frame sync word, mode and guard interval, the 102-bit TMCC
//!   information of `isdbt_ofdm::tmcc::TmccInfo`) and
//!   `network_synchronization_information` (Table 5-12, SFN delay control).
//!
//! [`BroadcastTsWriter`] turns per-layer packets (or a §3.2.1 multiplex
//! frame) into broadcasting TSPs with the dummy-byte information of every
//! slot and one IIP per frame.  [`BroadcastTsReader`] does the reverse: it
//! groups TSPs into frames at `frame_head_packet_flag`, splits them by
//! `layer_indicator` and recovers the [`Params`] from the IIP, so a
//! [`Transmitter`](crate::Transmitter) can be configured from the stream
//! itself.  Every codec is lossless (reserved bits are kept).  The crate
//! SPEC-NOTES.md records the interpretations (which slot carries the IIP,
//! the layer_indicator table typo, the IIP PID, which frame the IIP
//! describes).
//!
//! # Example
//!
//! Wrap one frame, read it back, configure a transmitter from the IIP:
//!
//! ```
//! use isdbt_core::{CodeRate, GuardInterval, Layer, LayerConfig, Mode, Modulation, Params, TimeInterleave};
//! use isdbt_phy::broadcast_ts::{BroadcastTsReader, BroadcastTsWriter};
//! use isdbt_phy::{Transmitter, null_packet};
//!
//! let a = LayerConfig {
//!     segments: 13,
//!     modulation: Modulation::Qpsk,
//!     code_rate: CodeRate::R1_2,
//!     time_interleave: TimeInterleave::K0,
//! };
//! let params = Params::new(Mode::M1, GuardInterval::G1_8, false, [Some(a), None, None])?;
//! let mut writer = BroadcastTsWriter::new(params.clone())?;
//! let packets = vec![null_packet(); writer.pattern().count(Layer::A)];
//! let tsps = writer.frame([&packets, &[], &[]])?;
//! assert_eq!(tsps.len(), params.multiplex_frame_tsps());
//!
//! let mut reader = BroadcastTsReader::new();
//! let mut frames = Vec::new();
//! for t in &tsps {
//!     frames.extend(reader.push(t));
//! }
//! frames.extend(reader.finish());
//! let frame = &frames[0];
//! assert_eq!(frame.params()?, params);
//! let mut tx = Transmitter::new(frame.params()?);
//! let [la, lb, lc] = frame.divide();
//! let _iq = tx.transmit_frame([&la, &lb, &lc])?;
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

use std::sync::OnceLock;

use isdb_ts::crc::crc32;
use isdbt_core::{GuardInterval, Layer, Mode, Params, TRANSMISSION_TSP, TSP};
use isdbt_fec::remux::{MultiplexPattern, Slot, null_tsp};
use isdbt_fec::rs::{gf_alpha_pow, gf_mul};
use isdbt_ofdm::tmcc::TmccInfo;

use crate::{Error, Packet, Result};

/// Length of a broadcasting TSP: 188 information bytes, 8 bytes of
/// ISDB-T_information and 8 parity bytes (Fig. 5-10).
pub const BROADCAST_TSP: usize = TRANSMISSION_TSP;
/// Offset of ISDB-T_information in a broadcasting TSP.
pub const INFO_OFFSET: usize = TSP;
/// Length of ISDB-T_information (Table 5-6: 64 bits).
pub const INFO_LEN: usize = 8;
/// Offset of the optional parity.
pub const PARITY_OFFSET: usize = INFO_OFFSET + INFO_LEN;
/// Parity bytes of the shortened (204,196) Reed-Solomon code (§5.5.2 (1)).
pub const PARITY_LEN: usize = BROADCAST_TSP - PARITY_OFFSET;
/// A broadcasting TSP (204 bytes).
pub type BroadcastTsp = [u8; BROADCAST_TSP];

/// PID of the IIP in digital terrestrial television, 0x1FF0.  Table 5-9
/// only says "service provider defined; however, the same PID is used for
/// digital terrestrial television broadcasting" — the value itself is from
/// operational knowledge (TR-B14), not from the local specifications.
pub const IIP_PID: u16 = 0x1FF0;
/// Payload of the IIP: 184 bytes (Table 5-8, payload only).
pub const IIP_PAYLOAD: usize = TSP - 4;
/// Length of `modulation_control_configuration_information` (160 bits).
pub const MODULATION_CONTROL_LEN: usize = 20;
/// Largest `network_synchronization_information_length` (Table 5-9).
pub const MAX_NETWORK_SYNC_LEN: usize = IIP_PAYLOAD - 2 - MODULATION_CONTROL_LEN - 3;
/// Largest value of the 100 ns time fields of Table 5-12 (0x98967F, under
/// one second).
pub const MAX_TIME_100NS: u32 = 9_999_999;

const _: () = assert!(MAX_NETWORK_SYNC_LEN == 159);

fn err(msg: impl Into<String>) -> Error {
    Error::BroadcastTs(msg.into())
}

// ---------------------------------------------------------------------------
// Bit packing (MSB first, as every bslbf/uimsbf field of the tables).

#[derive(Default)]
struct BitWriter {
    bytes: Vec<u8>,
    n: usize,
}

impl BitWriter {
    fn put(&mut self, v: u32, bits: usize) {
        for i in (0..bits).rev() {
            if self.n.is_multiple_of(8) {
                self.bytes.push(0);
            }
            if (v >> i) & 1 == 1 {
                self.bytes[self.n / 8] |= 0x80 >> (self.n % 8);
            }
            self.n += 1;
        }
    }
}

struct BitReader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl BitReader<'_> {
    fn get(&mut self, bits: usize) -> u32 {
        let mut v = 0u32;
        for _ in 0..bits {
            let bit = (self.data[self.pos / 8] >> (7 - self.pos % 8)) & 1;
            v = (v << 1) | u32::from(bit);
            self.pos += 1;
        }
        v
    }
}

// ---------------------------------------------------------------------------
// §5.5.2 ISDB-T_information (Tables 5-6, 5-7)

/// `layer_indicator` (Table 5-7, byte 1 bits 7–4): the hierarchical layer
/// a TSP is transmitted on, or why it is not transmitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LayerIndicator {
    /// '0000': a null TSP transmitted by no layer.
    Null,
    /// '0001', '0010', '0011': transmitted by layer A, B or C.
    Layer(Layer),
    /// '0100': a TSP carrying AC data, transmitted by no layer.  (Table
    /// 5-7 prints this row's code as '0010' too; see SPEC-NOTES.)
    Ac,
    /// '1000': the IIP, transmitted by no layer.
    Iip,
    /// '0101'–'0111', '1001'–'1111': data a service provider multiplexes
    /// on its own, transmitted by no layer.
    Provider(u8),
}

impl LayerIndicator {
    /// The 4-bit code.
    pub fn code(self) -> u8 {
        match self {
            Self::Null => 0b0000,
            Self::Layer(Layer::A) => 0b0001,
            Self::Layer(Layer::B) => 0b0010,
            Self::Layer(Layer::C) => 0b0011,
            Self::Ac => 0b0100,
            Self::Iip => 0b1000,
            Self::Provider(c) => c & 0xF,
        }
    }

    /// From the 4-bit code (upper bits ignored).
    pub fn from_code(code: u8) -> Self {
        match code & 0xF {
            0b0000 => Self::Null,
            0b0001 => Self::Layer(Layer::A),
            0b0010 => Self::Layer(Layer::B),
            0b0011 => Self::Layer(Layer::C),
            0b0100 => Self::Ac,
            0b1000 => Self::Iip,
            c => Self::Provider(c),
        }
    }

    /// The multiplex-frame slot this indicator corresponds to: a layer, or
    /// a null slot for everything transmitted by no layer.
    pub fn slot(self) -> Slot {
        match self {
            Self::Layer(l) => Some(l),
            _ => None,
        }
    }

    /// From a multiplex-frame slot.
    pub fn from_slot(slot: Slot) -> Self {
        slot.map_or(Self::Null, Self::Layer)
    }
}

/// `ISDB-T_information` of one TSP (Table 5-6), the 8-byte dummy-byte
/// part.  Lossless: reserved bits and the AC bytes are kept as received.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct IsdbtInformation {
    /// `TMCC_identifier` (2 bits): '10' digital terrestrial television
    /// (Table 5-5 / Table 5-7: "=1", "=0"); '00' BS digital, '11' digital
    /// terrestrial audio.
    pub tmcc_identifier: u8,
    /// Reserved bit, "should be '1'".
    pub reserved: bool,
    /// `buffer_reset_control_flag`: '1' resets the synchronised device's
    /// buffer; normally '0'.
    pub buffer_reset: bool,
    /// `switch-on_control_flag_for_emergency_broadcasting`: '1' during
    /// emergency broadcasting.
    pub emergency_switch_on: bool,
    /// `initialization_timing_head_packet_flag`: '1' on the head packet
    /// of the frame at which the transmission parameters change (the
    /// countdown index returned to '1111').
    pub initialization_timing_head_packet: bool,
    /// `frame_head_packet_flag`: '1' on the first TSP of a multiplex frame.
    pub frame_head_packet: bool,
    /// `frame_indicator`: '0' during an even OFDM frame (sync word w0),
    /// '1' during an odd one (w1).
    pub frame_indicator: bool,
    /// Which layer transmits this TSP.
    pub layer_indicator: LayerIndicator,
    /// `count_down_index` (4 bits): the TMCC transmission-parameter
    /// switching index ('1111' normal).
    pub count_down_index: u8,
    /// `AC_data_invalid_flag`: '1' when no AC data is carried in the
    /// dummy bytes.
    pub ac_data_invalid: bool,
    /// `AC_data_effective_bytes` (2 bits): '00' 1 byte … '11' 4 bytes
    /// (also the value when no AC data is carried).
    pub ac_data_effective_bytes: u8,
    /// `TSP_counter` (13 bits): 0 on the head packet of the multiplex
    /// frame, +1 per TSP including null, IIP and AC TSPs.
    pub tsp_counter: u16,
    /// Bytes 4–7: `AC_data`, or 32 stuffing bits when
    /// [`ac_data_invalid`](Self::ac_data_invalid) is set.
    pub ac_data: [u8; 4],
}

impl IsdbtInformation {
    /// `TMCC_identifier` of digital terrestrial television ('10').
    pub const TERRESTRIAL_TV: u8 = 0b10;

    /// Information of a TSP that carries no AC data and no event flags:
    /// identifier '10', reserved '1', flags 0, `AC_data_invalid_flag` '1',
    /// `AC_data_effective_bytes` '11', stuffing 0xFF.
    pub fn plain(layer_indicator: LayerIndicator, tsp_counter: u16) -> Self {
        Self {
            tmcc_identifier: Self::TERRESTRIAL_TV,
            reserved: true,
            buffer_reset: false,
            emergency_switch_on: false,
            initialization_timing_head_packet: false,
            frame_head_packet: false,
            frame_indicator: false,
            layer_indicator,
            count_down_index: 0b1111,
            ac_data_invalid: true,
            ac_data_effective_bytes: 0b11,
            tsp_counter,
            ac_data: [0xFF; 4],
        }
    }

    /// Serialise (Table 5-7 byte/bit layout, bit 7 = MSB).  The TSP
    /// counter is masked to 13 bits, the other multi-bit fields to their
    /// widths.
    pub fn to_bytes(&self) -> [u8; INFO_LEN] {
        let mut b = [0u8; INFO_LEN];
        b[0] = (self.tmcc_identifier & 0b11) << 6
            | u8::from(self.reserved) << 5
            | u8::from(self.buffer_reset) << 4
            | u8::from(self.emergency_switch_on) << 3
            | u8::from(self.initialization_timing_head_packet) << 2
            | u8::from(self.frame_head_packet) << 1
            | u8::from(self.frame_indicator);
        b[1] = self.layer_indicator.code() << 4 | (self.count_down_index & 0xF);
        let counter = self.tsp_counter & 0x1FFF;
        b[2] = u8::from(self.ac_data_invalid) << 7
            | (self.ac_data_effective_bytes & 0b11) << 5
            | (counter >> 8) as u8;
        b[3] = counter as u8;
        b[4..].copy_from_slice(&self.ac_data);
        b
    }

    /// Parse the 8-byte dummy-byte part.
    pub fn from_bytes(b: &[u8; INFO_LEN]) -> Self {
        Self {
            tmcc_identifier: b[0] >> 6,
            reserved: b[0] & 0x20 != 0,
            buffer_reset: b[0] & 0x10 != 0,
            emergency_switch_on: b[0] & 0x08 != 0,
            initialization_timing_head_packet: b[0] & 0x04 != 0,
            frame_head_packet: b[0] & 0x02 != 0,
            frame_indicator: b[0] & 0x01 != 0,
            layer_indicator: LayerIndicator::from_code(b[1] >> 4),
            count_down_index: b[1] & 0xF,
            ac_data_invalid: b[2] & 0x80 != 0,
            ac_data_effective_bytes: (b[2] >> 5) & 0b11,
            tsp_counter: u16::from(b[2] & 0x1F) << 8 | u16::from(b[3]),
            ac_data: [b[4], b[5], b[6], b[7]],
        }
    }

    /// The dummy-byte part of a broadcasting TSP.
    pub fn from_tsp(tsp: &BroadcastTsp) -> Self {
        let mut b = [0u8; INFO_LEN];
        b.copy_from_slice(&tsp[INFO_OFFSET..PARITY_OFFSET]);
        Self::from_bytes(&b)
    }

    /// True if the AC bytes are AC data (`AC_data_invalid_flag` '0').
    pub fn has_ac_data(&self) -> bool {
        !self.ac_data_invalid
    }

    /// Number of AC bytes that go on air (`AC_data_effective_bytes` + 1).
    pub fn ac_data_effective_len(&self) -> usize {
        usize::from(self.ac_data_effective_bytes & 0b11) + 1
    }
}

// ---------------------------------------------------------------------------
// §5.5.2 (1) optional parity: shortened (204,196) Reed-Solomon code

/// g(x) = ∏_{i=0}^{7} (x − λ^i) without its leading 1, highest degree
/// first (the (255,247) code of §5.5.2 (1), same field as RS(204,188)).
fn gen_tail() -> &'static [u8; PARITY_LEN] {
    static G: OnceLock<[u8; PARITY_LEN]> = OnceLock::new();
    G.get_or_init(|| {
        // Ascending-degree coefficients, g[k] = coefficient of x^k.
        let mut g = [0u8; PARITY_LEN + 1];
        g[0] = 1;
        for i in 0..PARITY_LEN {
            let root = gf_alpha_pow(i as u32);
            let mut next = [0u8; PARITY_LEN + 1];
            for k in 0..=i {
                next[k + 1] ^= g[k];
                next[k] ^= gf_mul(g[k], root);
            }
            g = next;
        }
        let mut t = [0u8; PARITY_LEN];
        for (j, t) in t.iter_mut().enumerate() {
            *t = g[PARITY_LEN - 1 - j];
        }
        t
    })
}

/// Parity of the shortened (204,196) Reed-Solomon code over the 188
/// information bytes and the 8 ISDB-T_information bytes (§5.5.2 (1):
/// the (255,247) code over GF(2⁸)/p(x), λ = 02HEX, shortened by 51
/// leading zero bytes; systematic, r(x) = d(x)·x⁸ mod g(x)).
pub fn parity(data: &[u8; PARITY_OFFSET]) -> [u8; PARITY_LEN] {
    let g = gen_tail();
    let mut reg = [0u8; PARITY_LEN];
    for &b in data {
        let fb = b ^ reg[0];
        reg.copy_within(1.., 0);
        reg[PARITY_LEN - 1] = 0;
        if fb != 0 {
            for (r, &gk) in reg.iter_mut().zip(g.iter()) {
                *r ^= gf_mul(fb, gk);
            }
        }
    }
    reg
}

/// True if the 204 bytes form a codeword of the (204,196) code (all eight
/// syndromes c(λ^j), j = 0..7, are zero).  A sender that adds no parity
/// (the parity is optional) fails this check.
pub fn parity_valid(tsp: &BroadcastTsp) -> bool {
    (0..PARITY_LEN as u32).all(|j| {
        let a = gf_alpha_pow(j);
        tsp.iter().fold(0u8, |acc, &b| gf_mul(acc, a) ^ b) == 0
    })
}

/// Assemble a broadcasting TSP: the packet, its ISDB-T_information and the
/// parity (Fig. 5-10).
pub fn assemble_tsp(packet: &Packet, info: &IsdbtInformation) -> BroadcastTsp {
    let mut out = [0u8; BROADCAST_TSP];
    out[..TSP].copy_from_slice(packet);
    out[INFO_OFFSET..PARITY_OFFSET].copy_from_slice(&info.to_bytes());
    let mut data = [0u8; PARITY_OFFSET];
    data.copy_from_slice(&out[..PARITY_OFFSET]);
    out[PARITY_OFFSET..].copy_from_slice(&parity(&data));
    out
}

/// The 188-byte information part of a broadcasting TSP.
pub fn packet_of(tsp: &BroadcastTsp) -> Packet {
    let mut p = [0u8; TSP];
    p.copy_from_slice(&tsp[..TSP]);
    p
}

// ---------------------------------------------------------------------------
// §5.5.3 (3) modulation_control_configuration_information (Tables 5-10, 5-11)

/// `mode_GI_information` (Table 5-10), raw codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ModeGi {
    /// `initialization_timing_indicator` (4 bits): '1111' normally; counts
    /// down per OFDM frame from 15 frames before a mode / guard-interval
    /// switch, which takes effect at the frame where it returns to 15.
    pub initialization_timing: u8,
    /// `current_mode` (2 bits): '01' Mode 1, '10' Mode 2, '11' Mode 3,
    /// '00' reserved.
    pub current_mode: u8,
    /// `current_guard_interval` (2 bits): '00' 1/32, '01' 1/16, '10' 1/8,
    /// '11' 1/4.
    pub current_guard: u8,
    /// `next_mode`, same codes.
    pub next_mode: u8,
    /// `next_guard_interval`, same codes.
    pub next_guard: u8,
}

impl ModeGi {
    /// `initialization_timing_indicator` outside a switch.
    pub const NORMAL_TIMING: u8 = 0b1111;

    /// Steady transmission: next = current, indicator '1111'.
    pub fn steady(mode: Mode, guard: GuardInterval) -> Self {
        Self {
            initialization_timing: Self::NORMAL_TIMING,
            current_mode: Self::mode_code(mode),
            current_guard: Self::guard_code(guard),
            next_mode: Self::mode_code(mode),
            next_guard: Self::guard_code(guard),
        }
    }

    /// Mode code (Table 5-11): the mode number.
    pub fn mode_code(mode: Mode) -> u8 {
        mode.number()
    }

    /// Mode from its code; `None` for the reserved '00'.
    pub fn mode_from_code(code: u8) -> Option<Mode> {
        Mode::from_number(code & 0b11)
    }

    /// Guard-interval code (Table 5-11).
    pub fn guard_code(guard: GuardInterval) -> u8 {
        match guard {
            GuardInterval::G1_32 => 0b00,
            GuardInterval::G1_16 => 0b01,
            GuardInterval::G1_8 => 0b10,
            GuardInterval::G1_4 => 0b11,
        }
    }

    /// Guard interval from its code.
    pub fn guard_from_code(code: u8) -> GuardInterval {
        match code & 0b11 {
            0b00 => GuardInterval::G1_32,
            0b01 => GuardInterval::G1_16,
            0b10 => GuardInterval::G1_8,
            _ => GuardInterval::G1_4,
        }
    }

    /// The current mode and guard interval.
    pub fn current(&self) -> Result<(Mode, GuardInterval)> {
        let mode = Self::mode_from_code(self.current_mode)
            .ok_or_else(|| err("IIP: reserved current_mode '00'"))?;
        Ok((mode, Self::guard_from_code(self.current_guard)))
    }

    /// The next mode and guard interval.
    pub fn next(&self) -> Result<(Mode, GuardInterval)> {
        let mode = Self::mode_from_code(self.next_mode)
            .ok_or_else(|| err("IIP: reserved next_mode '00'"))?;
        Ok((mode, Self::guard_from_code(self.next_guard)))
    }
}

/// `modulation_control_configuration_information` (Table 5-10): 128 bits
/// of content followed by CRC_32 (H.222.0 Annex A polynomial) over them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ModulationControl {
    /// `TMCC_synchronization_word`: '0' for a frame with sync word w0,
    /// '1' for w1 (Table 5-11).
    pub tmcc_sync_odd: bool,
    /// `AC_data_effective_position`: '1' when the AC data on air comes
    /// from the dummy bytes (also when no AC data is multiplexed), '0'
    /// when it comes from an invalid-hierarchy TSP.
    pub ac_in_dummy_bytes: bool,
    /// Reserved (2 bits).
    pub reserved: u8,
    /// Mode and guard interval, current and next.
    pub mode_gi: ModeGi,
    /// `TMCC_information`: "Same as the TMCC information in ISDB-T"
    /// (B20–B121 of Table 3-22, 102 bits).
    pub tmcc: TmccInfo,
    /// `reserved_future_use` (10 bits), "all of them should be '1'".
    pub reserved_future_use: u16,
}

impl ModulationControl {
    /// Steady transmission of `params`: AC from the dummy bytes, mode /
    /// guard interval of `params` as current and next, TMCC as
    /// [`TmccInfo::from_params`], reserved bits all ones.
    pub fn steady(params: &Params, odd_frame: bool) -> Self {
        Self::with_tmcc(params, odd_frame, TmccInfo::from_params(params))
    }

    /// As [`Self::steady`] with a caller-supplied TMCC information.
    pub fn with_tmcc(params: &Params, odd_frame: bool, tmcc: TmccInfo) -> Self {
        Self {
            tmcc_sync_odd: odd_frame,
            ac_in_dummy_bytes: true,
            reserved: 0b11,
            mode_gi: ModeGi::steady(params.mode(), params.guard()),
            tmcc,
            reserved_future_use: 0x3FF,
        }
    }

    /// The parameters in force: current mode / guard interval with the
    /// TMCC current information.
    pub fn params(&self) -> Result<Params> {
        let (mode, guard) = self.mode_gi.current()?;
        self.tmcc.params(mode, guard).map_err(Error::Ofdm)
    }

    /// The announced next parameters (next mode / guard interval with the
    /// TMCC next information); `Ok(None)` when the TMCC carries no next
    /// information.
    pub fn next_params(&self) -> Result<Option<Params>> {
        let (mode, guard) = self.mode_gi.next()?;
        if self.tmcc.next.layers.iter().all(|l| l.is_unused()) {
            return Ok(None);
        }
        self.tmcc
            .next
            .to_params(mode, guard)
            .map(Some)
            .map_err(Error::Ofdm)
    }

    fn content(&self) -> [u8; MODULATION_CONTROL_LEN - 4] {
        let mut w = BitWriter::default();
        w.put(u32::from(self.tmcc_sync_odd), 1);
        w.put(u32::from(self.ac_in_dummy_bytes), 1);
        w.put(u32::from(self.reserved & 0b11), 2);
        w.put(u32::from(self.mode_gi.initialization_timing & 0xF), 4);
        w.put(u32::from(self.mode_gi.current_mode & 0b11), 2);
        w.put(u32::from(self.mode_gi.current_guard & 0b11), 2);
        w.put(u32::from(self.mode_gi.next_mode & 0b11), 2);
        w.put(u32::from(self.mode_gi.next_guard & 0b11), 2);
        for bit in self.tmcc.to_bits() {
            w.put(u32::from(bit), 1);
        }
        w.put(u32::from(self.reserved_future_use & 0x3FF), 10);
        debug_assert_eq!(w.n, 128);
        w.bytes.try_into().expect("128 bits")
    }

    /// Serialise with CRC_32.
    pub fn to_bytes(&self) -> [u8; MODULATION_CONTROL_LEN] {
        let mut out = [0u8; MODULATION_CONTROL_LEN];
        let content = self.content();
        out[..content.len()].copy_from_slice(&content);
        out[content.len()..].copy_from_slice(&crc32(&content).to_be_bytes());
        out
    }

    /// Parse; `Err` on a CRC_32 mismatch.
    pub fn from_bytes(b: &[u8; MODULATION_CONTROL_LEN]) -> Result<Self> {
        if crc32(b) != 0 {
            return Err(err(
                "modulation_control_configuration_information CRC_32 mismatch",
            ));
        }
        let mut r = BitReader { data: b, pos: 0 };
        let tmcc_sync_odd = r.get(1) == 1;
        let ac_in_dummy_bytes = r.get(1) == 1;
        let reserved = r.get(2) as u8;
        let mode_gi = ModeGi {
            initialization_timing: r.get(4) as u8,
            current_mode: r.get(2) as u8,
            current_guard: r.get(2) as u8,
            next_mode: r.get(2) as u8,
            next_guard: r.get(2) as u8,
        };
        let mut bits = [0u8; 102];
        for bit in &mut bits {
            *bit = r.get(1) as u8;
        }
        let reserved_future_use = r.get(10) as u16;
        Ok(Self {
            tmcc_sync_odd,
            ac_in_dummy_bytes,
            reserved,
            mode_gi,
            tmcc: TmccInfo::from_bits(&bits),
            reserved_future_use,
        })
    }
}

// ---------------------------------------------------------------------------
// §5.5.3 (4) network_synchronization_information (Tables 5-12, 5-13)

/// `equipment_control_information` (Table 5-12): per-station SFN delay
/// control, 5 bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EquipmentControl {
    /// `equipment_id` (12 bits): the broadcasting station addressed.
    pub equipment_id: u16,
    /// `renewal_flag`: toggles when the values below are renewed.
    pub renewal: bool,
    /// `static_delay_flag`: '1' when the station uses the fixed delay
    /// `time_offset` instead of a reference time.
    pub static_delay: bool,
    /// Reserved bit, "should be '1'".
    pub reserved: bool,
    /// `time_offset_polarity`: '1' for a negative offset ('0' when
    /// `static_delay` is set).
    pub time_offset_negative: bool,
    /// `time_offset` (24 bits, 100 ns units, ≤ [`MAX_TIME_100NS`]):
    /// offset against `maximum_delay`, or the fixed delay.
    pub time_offset: u32,
}

/// `SFN_synchronization_information` (Table 5-12, `synchronization_id`
/// 0x00).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct SfnSync {
    /// `synchronization_time_stamp` (24 bits, 100 ns units): delay of the
    /// head of the next w0 multiplex frame against the latest 1 pps.
    pub time_stamp: u32,
    /// `maximum_delay` (24 bits, 100 ns units, ≤ [`MAX_TIME_100NS`]):
    /// studio output to antenna emission across the SFN.
    pub maximum_delay: u32,
    /// The equipment loop (≤ [`SfnSync::MAX_EQUIPMENT`] entries).
    pub equipment: Vec<EquipmentControl>,
}

impl SfnSync {
    /// Bytes besides the equipment loop: id, header, loop length, CRC.
    const FIXED_LEN: usize = 1 + 6 + 1 + 4;
    /// Entries that fit the 159-byte limit: (159 − 12) / 5.
    pub const MAX_EQUIPMENT: usize = (MAX_NETWORK_SYNC_LEN - Self::FIXED_LEN) / 5;
}

/// `network_synchronization_information` (Table 5-12).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum NetworkSync {
    /// `network_synchronization_information_length` 0: no information.
    Absent,
    /// `synchronization_id` 0x00: SFN synchronisation information.
    Sfn(SfnSync),
    /// `synchronization_id` 0xFF followed by this many stuffing bytes
    /// (0xFF): "SFN_synchronization_information is not added".
    Stuffing(usize),
    /// `synchronization_id` 0x01–0xFE, "for future extension": kept raw.
    Reserved {
        /// The identifier.
        id: u8,
        /// The bytes after it.
        body: Vec<u8>,
    },
}

impl Default for NetworkSync {
    /// `Stuffing(0)`: the one-byte "not added" form.
    fn default() -> Self {
        Self::Stuffing(0)
    }
}

fn check_24(name: &str, v: u32) -> Result<()> {
    if v > 0xFF_FFFF {
        return Err(err(format!("{name} {v} does not fit 24 bits")));
    }
    Ok(())
}

impl NetworkSync {
    /// Serialise: the bytes that follow
    /// `network_synchronization_information_length`.
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        let out = match self {
            Self::Absent => Vec::new(),
            Self::Stuffing(n) => {
                let mut v = vec![0xFF; n + 1];
                v[0] = 0xFF;
                v
            }
            Self::Reserved { id, body } => {
                if *id == 0x00 || *id == 0xFF {
                    return Err(err(format!(
                        "synchronization_id 0x{id:02X} is not a reserved value"
                    )));
                }
                let mut v = Vec::with_capacity(body.len() + 1);
                v.push(*id);
                v.extend_from_slice(body);
                v
            }
            Self::Sfn(s) => {
                if s.equipment.len() > SfnSync::MAX_EQUIPMENT {
                    return Err(err(format!(
                        "equipment loop of {} entries exceeds {}",
                        s.equipment.len(),
                        SfnSync::MAX_EQUIPMENT
                    )));
                }
                check_24("synchronization_time_stamp", s.time_stamp)?;
                check_24("maximum_delay", s.maximum_delay)?;
                let mut v = Vec::with_capacity(SfnSync::FIXED_LEN + 5 * s.equipment.len());
                v.push(0x00);
                v.extend_from_slice(&s.time_stamp.to_be_bytes()[1..]);
                v.extend_from_slice(&s.maximum_delay.to_be_bytes()[1..]);
                v.push((5 * s.equipment.len()) as u8);
                for e in &s.equipment {
                    if e.equipment_id > 0xFFF {
                        return Err(err(format!(
                            "equipment_id {} does not fit 12 bits",
                            e.equipment_id
                        )));
                    }
                    check_24("time_offset", e.time_offset)?;
                    v.push((e.equipment_id >> 4) as u8);
                    v.push(
                        ((e.equipment_id & 0xF) as u8) << 4
                            | u8::from(e.renewal) << 3
                            | u8::from(e.static_delay) << 2
                            | u8::from(e.reserved) << 1
                            | u8::from(e.time_offset_negative),
                    );
                    v.extend_from_slice(&e.time_offset.to_be_bytes()[1..]);
                }
                // CRC_32 "from synchronization_time_stamp … to time_offset".
                let crc = crc32(&v[1..]);
                v.extend_from_slice(&crc.to_be_bytes());
                v
            }
        };
        if out.len() > MAX_NETWORK_SYNC_LEN {
            return Err(err(format!(
                "network_synchronization_information of {} bytes exceeds {}",
                out.len(),
                MAX_NETWORK_SYNC_LEN
            )));
        }
        Ok(out)
    }

    /// Parse the bytes that follow
    /// `network_synchronization_information_length`.
    pub fn from_bytes(b: &[u8]) -> Result<Self> {
        let Some((&id, rest)) = b.split_first() else {
            return Ok(Self::Absent);
        };
        match id {
            0xFF => Ok(Self::Stuffing(rest.len())),
            0x00 => {
                if rest.len() < SfnSync::FIXED_LEN - 1 {
                    return Err(err(format!(
                        "SFN_synchronization_information of {} bytes is too short",
                        b.len()
                    )));
                }
                let loop_len = usize::from(rest[6]);
                if !loop_len.is_multiple_of(5) || rest.len() != 7 + loop_len + 4 {
                    return Err(err(format!(
                        "equipment_loop_length {loop_len} does not match {} bytes",
                        b.len()
                    )));
                }
                if crc32(rest) != 0 {
                    return Err(err("SFN_synchronization_information CRC_32 mismatch"));
                }
                let u24 = |x: &[u8]| u32::from_be_bytes([0, x[0], x[1], x[2]]);
                let equipment = rest[7..7 + loop_len]
                    .as_chunks::<5>()
                    .0
                    .iter()
                    .map(|e| EquipmentControl {
                        equipment_id: u16::from(e[0]) << 4 | u16::from(e[1] >> 4),
                        renewal: e[1] & 0x08 != 0,
                        static_delay: e[1] & 0x04 != 0,
                        reserved: e[1] & 0x02 != 0,
                        time_offset_negative: e[1] & 0x01 != 0,
                        time_offset: u24(&e[2..5]),
                    })
                    .collect();
                Ok(Self::Sfn(SfnSync {
                    time_stamp: u24(&rest[0..3]),
                    maximum_delay: u24(&rest[3..6]),
                    equipment,
                }))
            }
            id => Ok(Self::Reserved {
                id,
                body: rest.to_vec(),
            }),
        }
    }
}

// ---------------------------------------------------------------------------
// §5.5.3 (2) IIP (Tables 5-8, 5-9)

/// `ISDB-T_information_packet` (Table 5-8): one TSP per multiplex frame.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Iip {
    /// PID ("service provider defined"; [`IIP_PID`] in terrestrial TV).
    pub pid: u16,
    /// `continuity_counter` (H.222.0 §2.4.3.3).
    pub continuity_counter: u8,
    /// `IIP_packet_pointer`: TSPs from this packet to the next multiplex
    /// frame head, minus one (0 on the last TSP of a frame).
    pub packet_pointer: u16,
    /// `modulation_control_configuration_information`.
    pub modulation_control: ModulationControl,
    /// `IIP_branch_number`: index of this sub-IIP packet (0x00 first).
    pub branch_number: u8,
    /// `last_IIP_branch_number`: 0x00 for a single packet, 0x01 when the
    /// network synchronisation information spans two frames.
    pub last_branch_number: u8,
    /// `network_synchronization_information`.
    pub network_sync: NetworkSync,
}

impl Iip {
    /// Serialise: header with `payload_unit_start_indicator` '1',
    /// `adaptation_field_control` '01', the payload, and 0xFF stuffing
    /// (Table 5-8 / 5-9).
    pub fn to_packet(&self) -> Result<Packet> {
        if self.pid > 0x1FFF {
            return Err(err(format!("PID 0x{:X} does not fit 13 bits", self.pid)));
        }
        let ns = self.network_sync.to_bytes()?;
        let mut p = [0xFFu8; TSP];
        p[0] = 0x47;
        p[1] = 0x40 | (self.pid >> 8) as u8;
        p[2] = self.pid as u8;
        p[3] = 0x10 | (self.continuity_counter & 0xF);
        p[4..6].copy_from_slice(&self.packet_pointer.to_be_bytes());
        p[6..26].copy_from_slice(&self.modulation_control.to_bytes());
        p[26] = self.branch_number;
        p[27] = self.last_branch_number;
        p[28] = ns.len() as u8;
        p[29..29 + ns.len()].copy_from_slice(&ns);
        Ok(p)
    }

    /// Parse an IIP TSP.  Checks the sync byte, `adaptation_field_control`
    /// '01' (the payload offset depends on it), the length field and both
    /// CRC_32s; `transport_error_indicator`, `payload_unit_start_indicator`
    /// and `transport_priority` are not checked.
    pub fn from_packet(p: &Packet) -> Result<Self> {
        if p[0] != 0x47 {
            return Err(err(format!("IIP sync byte 0x{:02X}", p[0])));
        }
        if (p[3] >> 4) & 0b11 != 0b01 {
            return Err(err(format!(
                "IIP adaptation_field_control '{:02b}' (expected '01')",
                (p[3] >> 4) & 0b11
            )));
        }
        let ns_len = usize::from(p[28]);
        if ns_len > MAX_NETWORK_SYNC_LEN {
            return Err(err(format!(
                "network_synchronization_information_length {ns_len} exceeds {MAX_NETWORK_SYNC_LEN}"
            )));
        }
        let mut mc = [0u8; MODULATION_CONTROL_LEN];
        mc.copy_from_slice(&p[6..26]);
        Ok(Self {
            pid: u16::from(p[1] & 0x1F) << 8 | u16::from(p[2]),
            continuity_counter: p[3] & 0xF,
            packet_pointer: u16::from_be_bytes([p[4], p[5]]),
            modulation_control: ModulationControl::from_bytes(&mc)?,
            branch_number: p[26],
            last_branch_number: p[27],
            network_sync: NetworkSync::from_bytes(&p[29..29 + ns_len])?,
        })
    }
}

// ---------------------------------------------------------------------------
// Writer

/// Builds broadcasting TSPs, frame by frame, for fixed [`Params`].
///
/// Per frame: every slot of the §3.2.1 multiplex frame gets its
/// ISDB-T_information (`layer_indicator` from the model-receiver pattern,
/// `TSP_counter` = slot index, `frame_head_packet_flag` on slot 0,
/// `frame_indicator` alternating per frame, `count_down_index` and the
/// emergency flag from the TMCC information) and the first null slot of the
/// pattern carries the IIP (`layer_indicator` '1000'), whose
/// `modulation_control_configuration_information` describes the following
/// frame (§5.5.3 (1)) — with the steady TMCC information this crate
/// transmits, the same as the current one.  No AC data is carried
/// (`AC_data_invalid_flag` '1', `AC_data_effective_position` '1').
#[derive(Debug, Clone)]
pub struct BroadcastTsWriter {
    params: Params,
    pattern: MultiplexPattern,
    iip_slot: usize,
    iip_pid: u16,
    tmcc: TmccInfo,
    network_sync: NetworkSync,
    continuity_counter: u8,
    frames: u64,
    prev_countdown: u8,
}

impl BroadcastTsWriter {
    /// Writer for `params`.  Fails if the multiplex frame has no null slot
    /// to carry the IIP.
    pub fn new(params: Params) -> Result<Self> {
        let pattern = MultiplexPattern::new(&params);
        let iip_slot = pattern
            .slots()
            .iter()
            .position(Option::is_none)
            .ok_or_else(|| err("multiplex frame has no null slot for the IIP"))?;
        Ok(Self {
            tmcc: TmccInfo::from_params(&params),
            params,
            pattern,
            iip_slot,
            iip_pid: IIP_PID,
            network_sync: NetworkSync::default(),
            continuity_counter: 0,
            frames: 0,
            prev_countdown: 0b1111,
        })
    }

    /// Use another IIP PID.
    pub fn with_iip_pid(mut self, pid: u16) -> Result<Self> {
        if pid > 0x1FFF {
            return Err(err(format!("PID 0x{pid:X} does not fit 13 bits")));
        }
        self.iip_pid = pid;
        Ok(self)
    }

    /// Override the TMCC information carried by the IIP and (countdown
    /// index, emergency flag) by every dummy-byte part.
    pub fn set_tmcc(&mut self, tmcc: TmccInfo) {
        self.tmcc = tmcc;
    }

    /// Network synchronisation information to carry in every IIP (default:
    /// the one-byte "not added" form).  Fails if it does not fit.
    pub fn set_network_sync(&mut self, network_sync: NetworkSync) -> Result<()> {
        network_sync.to_bytes()?;
        self.network_sync = network_sync;
        Ok(())
    }

    /// Parameters.
    pub fn params(&self) -> &Params {
        &self.params
    }

    /// §3.2 multiplex-frame arrangement.
    pub fn pattern(&self) -> &MultiplexPattern {
        &self.pattern
    }

    /// Slot index of the IIP in every frame (the pattern's first null slot).
    pub fn iip_slot(&self) -> usize {
        self.iip_slot
    }

    /// Frames written so far.
    pub fn frames(&self) -> u64 {
        self.frames
    }

    /// One frame from per-layer packets (exactly `pattern().count(layer)`
    /// each); null slots carry null packets.
    pub fn frame(&mut self, layers: [&[Packet]; 3]) -> Result<Vec<BroadcastTsp>> {
        let mux = self.pattern.assemble(layers, null_tsp)?;
        Ok(self.wrap(&mux))
    }

    /// One frame from a §3.2.1 multiplex frame of `pattern().len()`
    /// packets in slot order.  The packet in the IIP slot (a null slot,
    /// transmitted by no layer) is replaced by the IIP.
    pub fn multiplex_frame(&mut self, frame: &[Packet]) -> Result<Vec<BroadcastTsp>> {
        if frame.len() != self.pattern.len() {
            return Err(Error::Input(format!(
                "multiplex frame of {} packets, expected {}",
                frame.len(),
                self.pattern.len()
            )));
        }
        Ok(self.wrap(frame))
    }

    fn wrap(&mut self, frame: &[Packet]) -> Vec<BroadcastTsp> {
        let n = frame.len();
        let odd = self.frames % 2 == 1;
        let countdown = self.tmcc.switching_countdown & 0xF;
        // The head packet of the first frame after a countdown (Table 5-7).
        let switched = self.frames > 0 && self.prev_countdown != 0b1111 && countdown == 0b1111;
        let iip = Iip {
            pid: self.iip_pid,
            continuity_counter: self.continuity_counter,
            packet_pointer: (n - 1 - self.iip_slot) as u16,
            modulation_control: ModulationControl::with_tmcc(&self.params, !odd, self.tmcc),
            branch_number: 0,
            last_branch_number: 0,
            network_sync: self.network_sync.clone(),
        };
        let iip_packet = iip
            .to_packet()
            .expect("network_sync validated by set_network_sync");
        self.continuity_counter = (self.continuity_counter + 1) & 0xF;

        let out = self
            .pattern
            .slots()
            .iter()
            .zip(frame)
            .enumerate()
            .map(|(j, (slot, packet))| {
                let (packet, layer_indicator) = if j == self.iip_slot {
                    (&iip_packet, LayerIndicator::Iip)
                } else {
                    (packet, LayerIndicator::from_slot(*slot))
                };
                let mut info = IsdbtInformation::plain(layer_indicator, j as u16);
                info.emergency_switch_on = self.tmcc.emergency_alarm;
                info.initialization_timing_head_packet = switched && j == 0;
                info.frame_head_packet = j == 0;
                info.frame_indicator = odd;
                info.count_down_index = countdown;
                assemble_tsp(packet, &info)
            })
            .collect();
        self.frames += 1;
        self.prev_countdown = countdown;
        out
    }
}

// ---------------------------------------------------------------------------
// Reader

/// One multiplex frame read from a broadcasting TS.
#[derive(Debug, Clone)]
pub struct BroadcastFrame {
    /// The 188-byte information parts, in slot order (the IIP included).
    pub packets: Vec<Packet>,
    /// The ISDB-T_information of each TSP.
    pub info: Vec<IsdbtInformation>,
    /// The IIP that describes this frame: the one carried by the previous
    /// frame (§5.5.3 (1)).  `None` for the first frame read.
    pub iip: Option<Iip>,
    /// The IIP carried in this frame (it describes the next frame).
    pub carried_iip: Option<Iip>,
    /// Slot of the carried IIP.
    pub iip_slot: Option<usize>,
    /// Why the TSP with `layer_indicator` '1000' did not parse, if so.
    pub iip_error: Option<String>,
    /// TSPs whose (204,196) parity does not check.  Equals the frame
    /// length when the sender adds no parity.
    pub parity_errors: usize,
}

impl BroadcastFrame {
    /// Number of TSPs.
    pub fn len(&self) -> usize {
        self.packets.len()
    }

    /// True if the frame has no TSP.
    pub fn is_empty(&self) -> bool {
        self.packets.is_empty()
    }

    /// `frame_indicator` of the head packet: odd (w1) frame.
    pub fn odd(&self) -> bool {
        self.info.first().is_some_and(|i| i.frame_indicator)
    }

    /// The slot arrangement given by the layer indicators (to compare with
    /// [`MultiplexPattern::slots`]).
    pub fn slots(&self) -> Vec<Slot> {
        self.info.iter().map(|i| i.layer_indicator.slot()).collect()
    }

    /// True if `TSP_counter` runs 0, 1, … over the frame.
    pub fn counter_ok(&self) -> bool {
        self.info
            .iter()
            .enumerate()
            .all(|(j, i)| usize::from(i.tsp_counter) == j)
    }

    /// Per-layer packets by `layer_indicator` (the hierarchical divider of
    /// §3.4: null, IIP, AC and provider TSPs are dropped).
    pub fn divide(&self) -> [Vec<Packet>; 3] {
        let mut out: [Vec<Packet>; 3] = Default::default();
        for (p, i) in self.packets.iter().zip(&self.info) {
            if let LayerIndicator::Layer(l) = i.layer_indicator {
                out[l.index()].push(*p);
            }
        }
        out
    }

    /// The modulation control that applies to this frame: from
    /// [`iip`](Self::iip), else (first frame, steady state assumed) from
    /// [`carried_iip`](Self::carried_iip).
    pub fn modulation_control(&self) -> Result<&ModulationControl> {
        self.iip
            .as_ref()
            .or(self.carried_iip.as_ref())
            .map(|i| &i.modulation_control)
            .ok_or_else(|| match &self.iip_error {
                Some(e) => err(format!("no usable IIP: {e}")),
                None => err("no IIP in this or the previous frame"),
            })
    }

    /// Transmission parameters of this frame from the IIP.
    pub fn params(&self) -> Result<Params> {
        self.modulation_control()?.params()
    }
}

/// Groups broadcasting TSPs into multiplex frames at
/// `frame_head_packet_flag` and parses the IIP of each.
#[derive(Debug, Default)]
pub struct BroadcastTsReader {
    synced: bool,
    dropped: u64,
    packets: Vec<Packet>,
    info: Vec<IsdbtInformation>,
    carried_iip: Option<Iip>,
    iip_slot: Option<usize>,
    iip_error: Option<String>,
    parity_errors: usize,
    prev_iip: Option<Iip>,
}

impl BroadcastTsReader {
    /// New reader; TSPs before the first frame head are dropped.
    pub fn new() -> Self {
        Self::default()
    }

    /// TSPs dropped before the first frame head.
    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    /// Push one TSP; returns the previous frame when `tsp` starts a new
    /// one.
    pub fn push(&mut self, tsp: &BroadcastTsp) -> Option<BroadcastFrame> {
        let info = IsdbtInformation::from_tsp(tsp);
        let mut done = None;
        if info.frame_head_packet {
            if self.synced && !self.packets.is_empty() {
                done = self.take();
            }
            self.synced = true;
        }
        if !self.synced {
            self.dropped += 1;
            return None;
        }
        let packet = packet_of(tsp);
        if !parity_valid(tsp) {
            self.parity_errors += 1;
        }
        if info.layer_indicator == LayerIndicator::Iip {
            match Iip::from_packet(&packet) {
                Ok(iip) => {
                    self.iip_slot = Some(self.packets.len());
                    self.carried_iip = Some(iip);
                }
                Err(e) => self.iip_error = Some(e.to_string()),
            }
        }
        self.packets.push(packet);
        self.info.push(info);
        done
    }

    /// The frame in progress (after the last TSP of a stream), if any.
    pub fn finish(&mut self) -> Option<BroadcastFrame> {
        self.take()
    }

    fn take(&mut self) -> Option<BroadcastFrame> {
        if self.packets.is_empty() {
            return None;
        }
        let carried_iip = self.carried_iip.take();
        let frame = BroadcastFrame {
            packets: std::mem::take(&mut self.packets),
            info: std::mem::take(&mut self.info),
            iip: self.prev_iip.take(),
            carried_iip: carried_iip.clone(),
            iip_slot: self.iip_slot.take(),
            iip_error: self.iip_error.take(),
            parity_errors: std::mem::take(&mut self.parity_errors),
        };
        self.prev_iip = carried_iip;
        Some(frame)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use isdbt_core::{CodeRate, LayerConfig, Modulation, TimeInterleave};

    fn lc(segments: usize, modulation: Modulation, code_rate: CodeRate) -> LayerConfig {
        LayerConfig {
            segments,
            modulation,
            code_rate,
            time_interleave: TimeInterleave::K1,
        }
    }

    fn bench() -> Params {
        Params::new(
            Mode::M3,
            GuardInterval::G1_8,
            true,
            [
                Some(lc(1, Modulation::Qpsk, CodeRate::R2_3)),
                Some(lc(12, Modulation::Qam64, CodeRate::R3_4)),
                None,
            ],
        )
        .unwrap()
    }

    fn packet(tag: u8, n: u32) -> Packet {
        let mut p = [0u8; TSP];
        p[0] = 0x47;
        p[1] = 0x01;
        p[2] = tag;
        p[3] = 0x10 | (n & 15) as u8;
        p[4..8].copy_from_slice(&n.to_be_bytes());
        let mut x = n.wrapping_mul(2_654_435_761) | 1;
        for b in &mut p[8..] {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            *b = x as u8;
        }
        p
    }

    /// Table 5-7: byte 0 = identifier '10', reserved '1', flags; byte 1 =
    /// layer_indicator | count_down_index; bytes 2–3 = AC flags and the
    /// 13-bit TSP counter (bit 0 = LSB).
    #[test]
    fn spec_table_5_7_layout() {
        let mut i = IsdbtInformation::plain(LayerIndicator::Layer(Layer::A), 0);
        i.frame_head_packet = true;
        assert_eq!(
            i.to_bytes(),
            [0xA2, 0x1F, 0xE0, 0x00, 0xFF, 0xFF, 0xFF, 0xFF]
        );

        let mut i = IsdbtInformation::plain(LayerIndicator::Iip, 0x1234);
        i.frame_indicator = true;
        i.count_down_index = 0b0101;
        i.ac_data_invalid = false;
        i.ac_data_effective_bytes = 0b01;
        i.ac_data = [1, 2, 3, 4];
        assert_eq!(i.to_bytes(), [0xA1, 0x85, 0x32, 0x34, 1, 2, 3, 4]);
        assert_eq!(IsdbtInformation::from_bytes(&i.to_bytes()), i);
        assert_eq!(i.ac_data_effective_len(), 2);
        assert!(i.has_ac_data());
    }

    /// Every 8-byte value survives a parse/serialise round trip (lossless).
    #[test]
    fn isdbt_information_lossless() {
        let mut x = 0x1234_5678u32;
        for _ in 0..2000 {
            let mut b = [0u8; INFO_LEN];
            for v in &mut b {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                *v = x as u8;
            }
            assert_eq!(IsdbtInformation::from_bytes(&b).to_bytes(), b);
        }
    }

    /// Table 5-7 layer_indicator codes, including the '0100' reading of
    /// the duplicated '0010' row (SPEC-NOTES).
    #[test]
    fn spec_table_5_7_layer_indicator() {
        for c in 0..16u8 {
            let l = LayerIndicator::from_code(c);
            assert_eq!(l.code(), c);
            let expected = match c {
                0 => LayerIndicator::Null,
                1 => LayerIndicator::Layer(Layer::A),
                2 => LayerIndicator::Layer(Layer::B),
                3 => LayerIndicator::Layer(Layer::C),
                4 => LayerIndicator::Ac,
                8 => LayerIndicator::Iip,
                c => LayerIndicator::Provider(c),
            };
            assert_eq!(l, expected);
            assert_eq!(l.slot().is_some(), (1..=3).contains(&c));
        }
    }

    /// §5.5.2 (1): the parity makes the 204 bytes a codeword of the
    /// (204,196) code (c(λ^j) = 0 for j = 0..7, λ = 02HEX), it is
    /// systematic, and any single corrupted byte is detected.
    #[test]
    fn spec_5_5_2_rs_204_196_parity() {
        let p = packet(7, 42);
        let info = IsdbtInformation::plain(LayerIndicator::Layer(Layer::B), 17);
        let tsp = assemble_tsp(&p, &info);
        assert_eq!(&tsp[..TSP], &p);
        assert_eq!(&tsp[INFO_OFFSET..PARITY_OFFSET], &info.to_bytes());
        assert!(parity_valid(&tsp));
        // Independent syndrome evaluation: c(x) = Σ c_k x^(203−k).
        for j in 0..8u32 {
            let a = gf_alpha_pow(j);
            let mut v = 0u8;
            for (k, &c) in tsp.iter().enumerate() {
                let mut term = c;
                for _ in 0..(BROADCAST_TSP - 1 - k) {
                    term = gf_mul(term, a);
                }
                v ^= term;
            }
            assert_eq!(v, 0, "syndrome {j}");
        }
        // Root λ^8 is not a root of g(x): a (204,188) word is not one here.
        for k in [0, 100, 187, 190, 195, 196, 203] {
            let mut bad = tsp;
            bad[k] ^= 0x5A;
            assert!(!parity_valid(&bad), "byte {k}");
        }
        // "adding 00HEX of 51 bytes in front": a shortened code, so the
        // parity of all-zero data is zero.
        assert_eq!(parity(&[0u8; PARITY_OFFSET]), [0u8; PARITY_LEN]);
    }

    /// Table 5-10: 160 bits = 128 content bits + CRC_32; Table 5-11 mode /
    /// guard-interval codes; CRC failure is detected.
    #[test]
    fn spec_table_5_10_modulation_control() {
        let params = bench();
        let mc = ModulationControl::steady(&params, true);
        let b = mc.to_bytes();
        // sync '1', AC position '1', reserved '11', timing '1111'.
        assert_eq!(b[0], 0xFF);
        // Mode 3 '11', GI 1/8 '10', next the same.
        assert_eq!(b[1], 0b1110_1110);
        // TMCC bits follow: B20–B21 system '00', B22–B25 '1111', B26 '0',
        // B27 partial reception '1' → 0b0011_1101.
        assert_eq!(b[2], 0b0011_1101);
        // Last 10 reserved bits all ones: byte 15 ends with 1s.
        assert_eq!(b[15] & 0x3F, 0x3F);
        assert_eq!(crc32(&b), 0);
        let back = ModulationControl::from_bytes(&b).unwrap();
        assert_eq!(back, mc);
        assert_eq!(back.params().unwrap(), params);
        assert_eq!(back.next_params().unwrap(), Some(params));

        let mut bad = b;
        bad[7] ^= 1;
        assert!(ModulationControl::from_bytes(&bad).is_err());

        assert_eq!(ModeGi::guard_code(GuardInterval::G1_32), 0b00);
        assert_eq!(ModeGi::guard_code(GuardInterval::G1_4), 0b11);
        assert_eq!(ModeGi::mode_code(Mode::M1), 0b01);
        assert_eq!(ModeGi::mode_from_code(0), None);
        for g in [
            GuardInterval::G1_4,
            GuardInterval::G1_8,
            GuardInterval::G1_16,
            GuardInterval::G1_32,
        ] {
            assert_eq!(ModeGi::guard_from_code(ModeGi::guard_code(g)), g);
        }
    }

    /// No next information in the TMCC → `next_params` is `None`; a
    /// reserved mode code is an error.
    #[test]
    fn modulation_control_next_and_reserved() {
        let params = bench();
        let mut mc = ModulationControl::steady(&params, false);
        mc.tmcc.next = isdbt_ofdm::tmcc::TmccParams::NONE;
        assert_eq!(mc.next_params().unwrap(), None);
        mc.mode_gi.current_mode = 0;
        let b = mc.to_bytes();
        assert!(ModulationControl::from_bytes(&b).unwrap().params().is_err());
    }

    /// Table 5-12: SFN information layout, the CRC_32 range (time stamp …
    /// last time_offset), the equipment-entry bit layout, and the length
    /// limit (≤ 159 bytes ⇒ ≤ 29 entries).
    #[test]
    fn spec_table_5_12_network_sync() {
        let sfn = SfnSync {
            time_stamp: 0x123456,
            maximum_delay: MAX_TIME_100NS,
            equipment: vec![
                EquipmentControl {
                    equipment_id: 0xABC,
                    renewal: true,
                    static_delay: false,
                    reserved: true,
                    time_offset_negative: true,
                    time_offset: 0x000102,
                },
                EquipmentControl {
                    equipment_id: 1,
                    renewal: false,
                    static_delay: true,
                    reserved: true,
                    time_offset_negative: false,
                    time_offset: 5,
                },
            ],
        };
        let ns = NetworkSync::Sfn(sfn.clone());
        let b = ns.to_bytes().unwrap();
        assert_eq!(b.len(), 12 + 10);
        assert_eq!(&b[..8], &[0x00, 0x12, 0x34, 0x56, 0x98, 0x96, 0x7F, 10]);
        assert_eq!(&b[8..13], &[0xAB, 0xCB, 0x00, 0x01, 0x02]);
        assert_eq!(&b[13..18], &[0x00, 0x16, 0x00, 0x00, 0x05]);
        assert_eq!(crc32(&b[1..]), 0);
        assert_eq!(NetworkSync::from_bytes(&b).unwrap(), ns);

        let mut bad = b.clone();
        bad[2] ^= 0x80;
        assert!(NetworkSync::from_bytes(&bad).is_err());
        let mut short = b.clone();
        short.pop();
        assert!(NetworkSync::from_bytes(&short).is_err());

        assert_eq!(SfnSync::MAX_EQUIPMENT, 29);
        let full = NetworkSync::Sfn(SfnSync {
            equipment: vec![sfn.equipment[0]; 29],
            ..sfn.clone()
        });
        assert_eq!(full.to_bytes().unwrap().len(), 157);
        let over = NetworkSync::Sfn(SfnSync {
            equipment: vec![sfn.equipment[0]; 30],
            ..sfn
        });
        assert!(over.to_bytes().is_err());

        assert_eq!(NetworkSync::Absent.to_bytes().unwrap(), Vec::<u8>::new());
        assert_eq!(NetworkSync::from_bytes(&[]).unwrap(), NetworkSync::Absent);
        assert_eq!(NetworkSync::Stuffing(3).to_bytes().unwrap(), vec![0xFF; 4]);
        assert_eq!(
            NetworkSync::from_bytes(&[0xFF; 4]).unwrap(),
            NetworkSync::Stuffing(3)
        );
        let r = NetworkSync::Reserved {
            id: 0x7E,
            body: vec![1, 2, 3],
        };
        assert_eq!(NetworkSync::from_bytes(&r.to_bytes().unwrap()).unwrap(), r);
        assert!(NetworkSync::Stuffing(159).to_bytes().is_err());
    }

    /// Table 5-8 / 5-9: header (PUSI '1', priority '0', scrambling '00',
    /// AFC '01'), payload offsets, 0xFF stuffing, 184-byte payload.
    #[test]
    fn spec_table_5_8_iip_packet() {
        let params = bench();
        let iip = Iip {
            pid: IIP_PID,
            continuity_counter: 9,
            packet_pointer: 0x0102,
            modulation_control: ModulationControl::steady(&params, false),
            branch_number: 0,
            last_branch_number: 0,
            network_sync: NetworkSync::default(),
        };
        let p = iip.to_packet().unwrap();
        assert_eq!(&p[..4], &[0x47, 0x5F, 0xF0, 0x19]);
        assert_eq!(&p[4..6], &[0x01, 0x02]);
        assert_eq!(&p[6..26], &iip.modulation_control.to_bytes());
        assert_eq!(&p[26..29], &[0, 0, 1]);
        assert_eq!(p[29], 0xFF);
        assert!(p[30..].iter().all(|&b| b == 0xFF));
        assert_eq!(Iip::from_packet(&p).unwrap(), iip);

        let mut bad = p;
        bad[3] = 0x39; // AFC '11'
        assert!(Iip::from_packet(&bad).is_err());
        let mut bad = p;
        bad[28] = 160;
        assert!(Iip::from_packet(&bad).is_err());
        let mut bad = p;
        bad[10] ^= 1;
        assert!(Iip::from_packet(&bad).is_err());
        assert!(Iip { pid: 0x2000, ..iip }.to_packet().is_err());
    }

    /// Writer: one IIP per frame in the first null slot, pointer = TSPs to
    /// the next head − 1 (0 on a last TSP), TSP_counter 0..n−1, frame head
    /// on slot 0, frame_indicator alternating, IIP sync word = next frame,
    /// layer indicators = the pattern, continuity counter advancing.
    #[test]
    fn spec_5_5_writer_frame_structure() {
        let params = bench();
        let mut w = BroadcastTsWriter::new(params.clone()).unwrap();
        let n = params.multiplex_frame_tsps();
        let pat = w.pattern().clone();
        assert_eq!(pat.slots()[w.iip_slot()], None);
        assert!(pat.slots()[..w.iip_slot()].iter().all(Option::is_some));
        let a: Vec<Packet> = (0..pat.count(Layer::A) as u32)
            .map(|i| packet(1, i))
            .collect();
        let b: Vec<Packet> = (0..pat.count(Layer::B) as u32)
            .map(|i| packet(2, i))
            .collect();
        for k in 0..3u64 {
            let tsps = w.frame([&a, &b, &[]]).unwrap();
            assert_eq!(tsps.len(), n);
            let mut iips = 0;
            for (j, t) in tsps.iter().enumerate() {
                assert!(parity_valid(t));
                let i = IsdbtInformation::from_tsp(t);
                assert_eq!(i.tmcc_identifier, IsdbtInformation::TERRESTRIAL_TV);
                assert!(i.reserved);
                assert_eq!(i.frame_head_packet, j == 0);
                assert_eq!(i.frame_indicator, k % 2 == 1);
                assert_eq!(usize::from(i.tsp_counter), j);
                assert_eq!(i.count_down_index, 0b1111);
                assert!(!i.initialization_timing_head_packet);
                assert!(i.ac_data_invalid);
                if j == w.iip_slot() {
                    iips += 1;
                    assert_eq!(i.layer_indicator, LayerIndicator::Iip);
                    let iip = Iip::from_packet(&packet_of(t)).unwrap();
                    assert_eq!(iip.pid, IIP_PID);
                    assert_eq!(iip.continuity_counter as u64, k);
                    assert_eq!(usize::from(iip.packet_pointer), n - 1 - j);
                    assert_eq!(iip.modulation_control.tmcc_sync_odd, k % 2 == 0);
                    assert_eq!(iip.modulation_control.params().unwrap(), params);
                    assert_eq!(iip.network_sync, NetworkSync::Stuffing(0));
                } else {
                    assert_eq!(i.layer_indicator, LayerIndicator::from_slot(pat.slots()[j]));
                    if pat.slots()[j].is_none() {
                        assert_eq!(packet_of(t), null_tsp());
                    }
                }
            }
            assert_eq!(iips, 1);
        }
        assert_eq!(w.frames(), 3);
        assert!(w.frame([&a[1..], &b, &[]]).is_err());
        assert!(w.multiplex_frame(&a).is_err());
    }

    /// `initialization_timing_head_packet_flag` is set on the head packet
    /// of the frame where the countdown returns to '1111'; the emergency
    /// flag and the countdown follow the TMCC information.
    #[test]
    fn writer_countdown_flags() {
        let params = bench();
        let mut w = BroadcastTsWriter::new(params.clone()).unwrap();
        let pat = w.pattern().clone();
        let a = vec![null_tsp(); pat.count(Layer::A)];
        let b = vec![null_tsp(); pat.count(Layer::B)];
        let mut tmcc = TmccInfo::from_params(&params);
        tmcc.switching_countdown = 0b0010;
        tmcc.emergency_alarm = true;
        w.set_tmcc(tmcc);
        let f = w.frame([&a, &b, &[]]).unwrap();
        let i = IsdbtInformation::from_tsp(&f[0]);
        assert_eq!(i.count_down_index, 0b0010);
        assert!(i.emergency_switch_on);
        assert!(!i.initialization_timing_head_packet);
        tmcc.switching_countdown = 0b1111;
        tmcc.emergency_alarm = false;
        w.set_tmcc(tmcc);
        let f = w.frame([&a, &b, &[]]).unwrap();
        assert!(IsdbtInformation::from_tsp(&f[0]).initialization_timing_head_packet);
        assert!(!IsdbtInformation::from_tsp(&f[1]).initialization_timing_head_packet);
        let f = w.frame([&a, &b, &[]]).unwrap();
        assert!(!IsdbtInformation::from_tsp(&f[0]).initialization_timing_head_packet);
    }

    /// Reader ∘ writer: frames are delimited at the head flag, TSPs before
    /// the first head are dropped, the per-layer packets and the slot
    /// arrangement come back, the IIP of frame k applies to frame k+1, and
    /// parity errors are counted.
    #[test]
    fn reader_recovers_frames() {
        let params = bench();
        let mut w = BroadcastTsWriter::new(params.clone()).unwrap();
        let pat = w.pattern().clone();
        let mut r = BroadcastTsReader::new();
        let mut frames = Vec::new();
        let mut sent = Vec::new();
        for k in 0..3u32 {
            let a: Vec<Packet> = (0..pat.count(Layer::A) as u32)
                .map(|i| packet(1, k * 10_000 + i))
                .collect();
            let b: Vec<Packet> = (0..pat.count(Layer::B) as u32)
                .map(|i| packet(2, k * 10_000 + i))
                .collect();
            let tsps = w.frame([&a, &b, &[]]).unwrap();
            sent.push((a, b));
            // Start mid-frame on the first one: it cannot be delimited.
            let skip = if k == 0 { 5 } else { 0 };
            for t in &tsps[skip..] {
                frames.extend(r.push(t));
            }
        }
        frames.extend(r.finish());
        // Everything up to the next frame head is dropped.
        assert_eq!(r.dropped() as usize, pat.len() - 5);
        assert_eq!(frames.len(), 2);
        for (f, (a, b)) in frames.iter().zip(&sent[1..]) {
            assert_eq!(f.len(), pat.len());
            assert!(f.counter_ok());
            assert_eq!(f.slots(), pat.slots());
            assert_eq!(f.iip_slot, Some(w.iip_slot()));
            assert_eq!(f.parity_errors, 0);
            assert!(f.iip_error.is_none());
            let [da, db, dc] = f.divide();
            assert_eq!((&da, &db, dc.len()), (a, b, 0));
            assert_eq!(f.params().unwrap(), params);
        }
        // Writer frame 1 (odd, the first complete one) has no previous
        // IIP; writer frame 2 (even) is described by the IIP frame 1
        // carried, whose sync word is therefore w0.
        assert!(frames[0].iip.is_none());
        assert!(frames[0].carried_iip.is_some());
        assert!(frames[0].odd());
        assert_eq!(frames[1].iip, frames[0].carried_iip);
        assert!(!frames[1].odd());
        assert!(
            !frames[1]
                .iip
                .as_ref()
                .unwrap()
                .modulation_control
                .tmcc_sync_odd
        );

        // A corrupted IIP: reported, params fall back to nothing.
        let mut r = BroadcastTsReader::new();
        let tsps = w.frame([&sent[0].0, &sent[0].1, &[]]).unwrap();
        let mut got = Vec::new();
        for (j, t) in tsps.iter().enumerate() {
            let mut t = *t;
            if j == w.iip_slot() {
                t[10] ^= 0xFF;
            }
            got.extend(r.push(&t));
        }
        got.extend(r.finish());
        assert_eq!(got.len(), 1);
        assert!(got[0].iip_error.is_some());
        assert!(got[0].carried_iip.is_none());
        assert!(got[0].params().is_err());
        assert_eq!(got[0].parity_errors, 1);
    }

    /// Every configuration of the parameter sweep has a null slot for the
    /// IIP, and the pointer of the IIP never exceeds the frame.
    #[test]
    fn every_configuration_has_an_iip_slot() {
        for mode in [Mode::M1, Mode::M2, Mode::M3] {
            for gi in [
                GuardInterval::G1_4,
                GuardInterval::G1_8,
                GuardInterval::G1_16,
                GuardInterval::G1_32,
            ] {
                for (m, r) in [
                    (Modulation::Dqpsk, CodeRate::R1_2),
                    (Modulation::Qam64, CodeRate::R7_8),
                ] {
                    let p = Params::new(mode, gi, false, [Some(lc(13, m, r)), None, None]).unwrap();
                    let w = BroadcastTsWriter::new(p.clone()).unwrap();
                    assert!(w.iip_slot() < p.multiplex_frame_tsps());
                }
            }
        }
    }
}

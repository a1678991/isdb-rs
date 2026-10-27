//! PES packets (H.222.0 §2.4.3.6 Table 2-21, §2.4.3.7, stream_id
//! Table 2-22), their packetisation into Transport Stream packets, and
//! reassembly.
//!
//! [`PesPacket::decode`] is lossless: marker bits, reserved bits, the
//! forbidden PTS_DTS_flags '01', header stuffing bytes and the raw
//! DSM_trick_mode byte are all kept.  `PES_packet_length` = 0 ("neither
//! specified nor bounded", allowed only for video in TS, §2.4.3.7) is kept
//! as [`PesPacket::unbounded`].

use crate::bits::{BitReader, BitWriter};
use crate::cc::{Counter, Tracker};
use crate::error::{Error, Result};
use crate::packet::{AdaptationFields, PACKET_SIZE, PAYLOAD_CAPACITY, Packet, View};
use crate::pcr::Timestamp;
use crate::pid::Pid;
use crate::validate::{Violation, require, reserved_ones};

/// packet_start_code_prefix 0x000001 (§2.4.3.7).
pub const START_CODE_PREFIX: [u8; 3] = [0x00, 0x00, 0x01];

/// stream_id assignments of Table 2-22.
pub mod stream_id {
    /// program_stream_map.
    pub const PROGRAM_STREAM_MAP: u8 = 0xBC;
    /// private_stream_1.
    pub const PRIVATE_STREAM_1: u8 = 0xBD;
    /// padding_stream.
    pub const PADDING_STREAM: u8 = 0xBE;
    /// private_stream_2.
    pub const PRIVATE_STREAM_2: u8 = 0xBF;
    /// First audio stream_id (110x xxxx).
    pub const AUDIO_FIRST: u8 = 0xC0;
    /// First video stream_id (1110 xxxx).
    pub const VIDEO_FIRST: u8 = 0xE0;
    /// ECM_stream.
    pub const ECM_STREAM: u8 = 0xF0;
    /// EMM_stream.
    pub const EMM_STREAM: u8 = 0xF1;
    /// H.222.0 Annex A or ISO/IEC 13818-6 DSMCC_stream.
    pub const DSMCC_STREAM: u8 = 0xF2;
    /// ISO/IEC_13522_stream.
    pub const ISO_13522_STREAM: u8 = 0xF3;
    /// ITU-T Rec. H.222.1 type A (B..E are 0xF5..0xF8).
    pub const H222_1_TYPE_A: u8 = 0xF4;
    /// ITU-T Rec. H.222.1 type E.
    pub const H222_1_TYPE_E: u8 = 0xF8;
    /// ancillary_stream.
    pub const ANCILLARY_STREAM: u8 = 0xF9;
    /// ISO/IEC 14496-1_SL-packetized_stream.
    pub const SL_PACKETIZED_STREAM: u8 = 0xFA;
    /// ISO/IEC 14496-1_FlexMux_stream.
    pub const FLEXMUX_STREAM: u8 = 0xFB;
    /// metadata stream.
    pub const METADATA_STREAM: u8 = 0xFC;
    /// extended_stream_id.
    pub const EXTENDED_STREAM_ID: u8 = 0xFD;
    /// reserved data stream.
    pub const RESERVED_DATA_STREAM: u8 = 0xFE;
    /// program_stream_directory.
    pub const PROGRAM_STREAM_DIRECTORY: u8 = 0xFF;
}

/// Class of a stream_id per Table 2-22.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamIdClass {
    /// 110x xxxx: ISO/IEC 13818-3/11172-3/13818-7/14496-3 audio, stream number.
    Audio(u8),
    /// 1110 xxxx: H.262/11172-2/14496-2/H.264 video, stream number.
    Video(u8),
    /// Any other value; the Table 2-22 name.
    Named(&'static str),
    /// Below 0xBC: not a stream_id (packet start codes of other syntaxes).
    NotStreamId,
}

/// Classify `id` per Table 2-22.
pub fn classify_stream_id(id: u8) -> StreamIdClass {
    use stream_id::*;
    match id {
        0xC0..=0xDF => StreamIdClass::Audio(id & 0x1F),
        0xE0..=0xEF => StreamIdClass::Video(id & 0x0F),
        PROGRAM_STREAM_MAP => StreamIdClass::Named("program_stream_map"),
        PRIVATE_STREAM_1 => StreamIdClass::Named("private_stream_1"),
        PADDING_STREAM => StreamIdClass::Named("padding_stream"),
        PRIVATE_STREAM_2 => StreamIdClass::Named("private_stream_2"),
        ECM_STREAM => StreamIdClass::Named("ECM_stream"),
        EMM_STREAM => StreamIdClass::Named("EMM_stream"),
        DSMCC_STREAM => StreamIdClass::Named("DSMCC_stream"),
        ISO_13522_STREAM => StreamIdClass::Named("ISO/IEC_13522_stream"),
        0xF4 => StreamIdClass::Named("ITU-T Rec. H.222.1 type A"),
        0xF5 => StreamIdClass::Named("ITU-T Rec. H.222.1 type B"),
        0xF6 => StreamIdClass::Named("ITU-T Rec. H.222.1 type C"),
        0xF7 => StreamIdClass::Named("ITU-T Rec. H.222.1 type D"),
        H222_1_TYPE_E => StreamIdClass::Named("ITU-T Rec. H.222.1 type E"),
        ANCILLARY_STREAM => StreamIdClass::Named("ancillary_stream"),
        SL_PACKETIZED_STREAM => StreamIdClass::Named("ISO/IEC 14496-1_SL-packetized_stream"),
        FLEXMUX_STREAM => StreamIdClass::Named("ISO/IEC 14496-1_FlexMux_stream"),
        METADATA_STREAM => StreamIdClass::Named("metadata stream"),
        EXTENDED_STREAM_ID => StreamIdClass::Named("extended_stream_id"),
        RESERVED_DATA_STREAM => StreamIdClass::Named("reserved data stream"),
        PROGRAM_STREAM_DIRECTORY => StreamIdClass::Named("program_stream_directory"),
        _ => StreamIdClass::NotStreamId,
    }
}

/// Whether PES packets with this stream_id carry the optional PES header
/// (the first `if` of Table 2-21: every stream_id except
/// program_stream_map, padding_stream, private_stream_2, ECM, EMM,
/// program_stream_directory, DSMCC_stream and H.222.1 type E).
pub fn has_optional_header(id: u8) -> bool {
    use stream_id::*;
    !matches!(
        id,
        PROGRAM_STREAM_MAP
            | PADDING_STREAM
            | PRIVATE_STREAM_2
            | ECM_STREAM
            | EMM_STREAM
            | PROGRAM_STREAM_DIRECTORY
            | DSMCC_STREAM
            | H222_1_TYPE_E
    )
}

/// PTS/DTS presence (PTS_DTS_flags, Table 2-21, §2.4.3.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PtsDts {
    /// '00': neither present.
    None,
    /// '01': forbidden by §2.4.3.7; no fields follow (kept for losslessness).
    Forbidden,
    /// '10': PTS only (prefix '0010').
    Pts(Timestamp),
    /// '11': PTS (prefix '0011') and DTS (prefix '0001').
    PtsDts(Timestamp, Timestamp),
}

impl PtsDts {
    fn flags(&self) -> u8 {
        match self {
            PtsDts::None => 0b00,
            PtsDts::Forbidden => 0b01,
            PtsDts::Pts(_) => 0b10,
            PtsDts::PtsDts(..) => 0b11,
        }
    }

    /// PTS value if present.
    pub fn pts(&self) -> Option<u64> {
        match self {
            PtsDts::Pts(p) | PtsDts::PtsDts(p, _) => Some(p.value),
            _ => None,
        }
    }

    /// DTS value if present.
    pub fn dts(&self) -> Option<u64> {
        match self {
            PtsDts::PtsDts(_, d) => Some(d.value),
            _ => None,
        }
    }

    /// PTS only, prefix '0010' and markers '1'.
    pub fn pts_only(pts: u64) -> Self {
        PtsDts::Pts(Timestamp::new(Timestamp::PREFIX_PTS_ONLY, pts))
    }

    /// PTS and DTS with prefixes '0011'/'0001' and markers '1'.
    pub fn pts_and_dts(pts: u64, dts: u64) -> Self {
        PtsDts::PtsDts(
            Timestamp::new(Timestamp::PREFIX_PTS_WITH_DTS, pts),
            Timestamp::new(Timestamp::PREFIX_DTS, dts),
        )
    }
}

/// ESCR field (Table 2-21): reserved(2) base[32..30] m base[29..15] m
/// base[14..0] m extension(9) m — 48 bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Escr {
    /// The 2 reserved bits (spec '11').
    pub reserved: u8,
    /// ESCR_base (33 bits, equation 2-14).
    pub base: u64,
    /// ESCR_extension (9 bits, equation 2-15).
    pub extension: u16,
    /// The four marker bits, first in bit 3 (spec 0b1111).
    pub markers: u8,
}

/// ES_rate field (Table 2-21): marker ES_rate(22) marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EsRate {
    /// The two marker bits, first in bit 1 (spec 0b11).
    pub markers: u8,
    /// ES_rate in units of 50 bytes/s (22 bits; '0' forbidden).
    pub rate: u32,
}

/// additional_copy_info field (Table 2-21): marker additional_copy_info(7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdditionalCopyInfo {
    /// marker_bit (spec '1').
    pub marker: bool,
    /// additional_copy_info (7 bits).
    pub info: u8,
}

/// Decoded view of the 8-bit DSM trick mode field (Table 2-21, Table 2-24).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrickMode {
    /// '000' fast forward.
    FastForward {
        /// field_id (Table 2-25).
        field_id: u8,
        /// intra_slice_refresh.
        intra_slice_refresh: bool,
        /// frequency_truncation (Table 2-26).
        frequency_truncation: u8,
    },
    /// '001' slow motion.
    SlowMotion {
        /// rep_cntrl.
        rep_cntrl: u8,
    },
    /// '010' freeze frame.
    FreezeFrame {
        /// field_id.
        field_id: u8,
        /// The 3 reserved bits.
        reserved: u8,
    },
    /// '011' fast reverse.
    FastReverse {
        /// field_id.
        field_id: u8,
        /// intra_slice_refresh.
        intra_slice_refresh: bool,
        /// frequency_truncation.
        frequency_truncation: u8,
    },
    /// '100' slow reverse.
    SlowReverse {
        /// rep_cntrl.
        rep_cntrl: u8,
    },
    /// '101'–'111' reserved; the 5 reserved bits.
    Reserved {
        /// trick_mode_control.
        control: u8,
        /// The 5 trailing bits.
        bits: u8,
    },
}

impl TrickMode {
    /// Interpret the raw trick-mode byte.
    pub fn from_byte(b: u8) -> Self {
        let low = b & 0x1F;
        match b >> 5 {
            0 => TrickMode::FastForward {
                field_id: low >> 3,
                intra_slice_refresh: low & 4 != 0,
                frequency_truncation: low & 3,
            },
            1 => TrickMode::SlowMotion { rep_cntrl: low },
            2 => TrickMode::FreezeFrame {
                field_id: low >> 3,
                reserved: low & 7,
            },
            3 => TrickMode::FastReverse {
                field_id: low >> 3,
                intra_slice_refresh: low & 4 != 0,
                frequency_truncation: low & 3,
            },
            4 => TrickMode::SlowReverse { rep_cntrl: low },
            c => TrickMode::Reserved {
                control: c,
                bits: low,
            },
        }
    }
}

/// PES_extension fields (Table 2-21 under PES_extension_flag).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PesExtension {
    /// The 3 reserved bits before PES_extension_flag_2 (spec '111').
    pub reserved: u8,
    /// PES_private_data (128 bits).
    pub private_data: Option<[u8; 16]>,
    /// pack_header() bytes (pack_field_length implied), kept raw.
    pub pack_header: Option<Vec<u8>>,
    /// program_packet_sequence_counter group.
    pub program_packet_sequence_counter: Option<ProgramPacketSequenceCounter>,
    /// P-STD buffer fields.
    pub p_std_buffer: Option<PStdBuffer>,
    /// PES_extension_flag_2 data.
    pub extension2: Option<PesExtension2>,
}

/// marker program_packet_sequence_counter(7) marker MPEG1_MPEG2_identifier
/// original_stuff_length(6) (Table 2-21).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProgramPacketSequenceCounter {
    /// The two marker bits, first in bit 1 (spec 0b11).
    pub markers: u8,
    /// program_packet_sequence_counter (7 bits).
    pub counter: u8,
    /// MPEG1_MPEG2_identifier.
    pub mpeg1_mpeg2_identifier: bool,
    /// original_stuff_length (6 bits).
    pub original_stuff_length: u8,
}

/// '01' P-STD_buffer_scale P-STD_buffer_size(13) (Table 2-21).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PStdBuffer {
    /// The 2-bit prefix (spec '01').
    pub prefix: u8,
    /// P-STD_buffer_scale.
    pub scale: bool,
    /// P-STD_buffer_size (13 bits).
    pub size: u16,
}

/// H.222.0 Table 2-21 PES_extension_flag_2 group: marker
/// PES_extension_field_length(7) and the `PES_extension_field_length`
/// bytes that follow (stream_id_extension_flag/stream_id_extension byte
/// first, then reserved bytes), kept raw — see SPEC-NOTES.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PesExtension2 {
    /// marker_bit (spec '1').
    pub marker: bool,
    /// The bytes counted by PES_extension_field_length.
    pub data: Vec<u8>,
}

impl PesExtension2 {
    /// stream_id_extension if stream_id_extension_flag is '0' (Table 2-27).
    pub fn stream_id_extension(&self) -> Option<u8> {
        self.data
            .first()
            .filter(|b| *b & 0x80 == 0)
            .map(|b| b & 0x7F)
    }
}

/// The optional PES header (Table 2-21, from the '10' bits through the
/// stuffing bytes).  Flags are implied by the `Option`s; PES_header_data_length
/// is implied by the fields plus `stuffing`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PesHeader {
    /// The two leading bits (spec '10').
    pub marker_bits: u8,
    /// PES_scrambling_control (Table 2-23).
    pub scrambling_control: u8,
    /// PES_priority.
    pub priority: bool,
    /// data_alignment_indicator.
    pub data_alignment_indicator: bool,
    /// copyright.
    pub copyright: bool,
    /// original_or_copy.
    pub original_or_copy: bool,
    /// PTS/DTS.
    pub pts_dts: PtsDts,
    /// ESCR.
    pub escr: Option<Escr>,
    /// ES_rate.
    pub es_rate: Option<EsRate>,
    /// Raw DSM trick-mode byte (trick_mode_control + 5 bits; see [`TrickMode`]).
    pub trick_mode: Option<u8>,
    /// additional_copy_info.
    pub additional_copy_info: Option<AdditionalCopyInfo>,
    /// previous_PES_packet_CRC.
    pub previous_pes_packet_crc: Option<u16>,
    /// PES extension.
    pub extension: Option<PesExtension>,
    /// stuffing_bytes (spec 0xFF, at most 32, §2.4.3.7), kept verbatim.
    pub stuffing: Vec<u8>,
}

impl Default for PesHeader {
    fn default() -> Self {
        Self {
            marker_bits: 0b10,
            scrambling_control: 0,
            priority: false,
            data_alignment_indicator: false,
            copyright: false,
            original_or_copy: false,
            pts_dts: PtsDts::None,
            escr: None,
            es_rate: None,
            trick_mode: None,
            additional_copy_info: None,
            previous_pes_packet_crc: None,
            extension: None,
            stuffing: Vec::new(),
        }
    }
}

/// What follows PES_packet_length (H.222.0 Table 2-21: the three branches
/// selected by stream_id).
// The header variant dominates in practice; boxing it would only add an
// allocation per PES packet.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PesBody {
    /// Optional PES header then PES_packet_data_bytes.
    Header {
        /// Optional header.
        header: PesHeader,
        /// PES_packet_data_bytes.
        data: Vec<u8>,
    },
    /// PES_packet_data_bytes directly (program_stream_map, private_stream_2,
    /// ECM, EMM, program_stream_directory, DSMCC, H.222.1 type E).
    Data(Vec<u8>),
    /// padding_bytes (padding_stream).
    Padding(Vec<u8>),
}

/// A PES packet (Table 2-21).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PesPacket {
    /// stream_id.
    pub stream_id: u8,
    /// PES_packet_length = 0 ("neither specified nor bounded").
    pub unbounded: bool,
    /// The rest of the packet.
    pub body: PesBody,
}

fn marker(r: &mut BitReader<'_>) -> Result<u8> {
    Ok(r.read(1)? as u8)
}

impl PesHeader {
    /// Decode the optional header from `b` (starting at the '10' bits);
    /// returns the header and its size in bytes.
    pub fn decode(b: &[u8]) -> Result<(Self, usize)> {
        if b.len() < 3 {
            return Err(Error::Truncated {
                what: "PES header",
                need: 3,
                have: b.len(),
            });
        }
        let hdl = b[2] as usize;
        if b.len() < 3 + hdl {
            return Err(Error::invalid(
                "PES_header_data_length",
                format!("{hdl} runs past the PES packet"),
            ));
        }
        let mut r = BitReader::new(&b[..3], "PES header");
        let marker_bits = r.read(2)? as u8;
        let scrambling_control = r.read(2)? as u8;
        let priority = r.bool()?;
        let data_alignment_indicator = r.bool()?;
        let copyright = r.bool()?;
        let original_or_copy = r.bool()?;
        let pts_dts_flags = r.read(2)? as u8;
        let escr_flag = r.bool()?;
        let es_rate_flag = r.bool()?;
        let trick_flag = r.bool()?;
        let aci_flag = r.bool()?;
        let crc_flag = r.bool()?;
        let ext_flag = r.bool()?;
        let mut r = BitReader::new(&b[3..3 + hdl], "PES optional fields");
        let pts_dts = match pts_dts_flags {
            0b00 => PtsDts::None,
            0b01 => PtsDts::Forbidden,
            0b10 => PtsDts::Pts(Timestamp::read(&mut r)?),
            _ => {
                let p = Timestamp::read(&mut r)?;
                PtsDts::PtsDts(p, Timestamp::read(&mut r)?)
            }
        };
        let escr = if escr_flag {
            let reserved = r.read(2)? as u8;
            let hi = r.read(3)?;
            let m0 = marker(&mut r)?;
            let mid = r.read(15)?;
            let m1 = marker(&mut r)?;
            let lo = r.read(15)?;
            let m2 = marker(&mut r)?;
            let extension = r.read(9)? as u16;
            let m3 = marker(&mut r)?;
            Some(Escr {
                reserved,
                base: (hi << 30) | (mid << 15) | lo,
                extension,
                markers: (m0 << 3) | (m1 << 2) | (m2 << 1) | m3,
            })
        } else {
            None
        };
        let es_rate = if es_rate_flag {
            let m0 = marker(&mut r)?;
            let rate = r.read(22)? as u32;
            let m1 = marker(&mut r)?;
            Some(EsRate {
                markers: (m0 << 1) | m1,
                rate,
            })
        } else {
            None
        };
        let trick_mode = if trick_flag { Some(r.u8()?) } else { None };
        let additional_copy_info = if aci_flag {
            Some(AdditionalCopyInfo {
                marker: r.bool()?,
                info: r.read(7)? as u8,
            })
        } else {
            None
        };
        let previous_pes_packet_crc = if crc_flag { Some(r.u16()?) } else { None };
        let extension = if ext_flag {
            Some(PesExtension::read(&mut r)?)
        } else {
            None
        };
        let stuffing = r.rest()?.to_vec();
        Ok((
            Self {
                marker_bits,
                scrambling_control,
                priority,
                data_alignment_indicator,
                copyright,
                original_or_copy,
                pts_dts,
                escr,
                es_rate,
                trick_mode,
                additional_copy_info,
                previous_pes_packet_crc,
                extension,
                stuffing,
            },
            3 + hdl,
        ))
    }

    /// Value of PES_header_data_length.
    pub fn header_data_length(&self) -> usize {
        let pd = match self.pts_dts {
            PtsDts::Pts(_) => 5,
            PtsDts::PtsDts(..) => 10,
            _ => 0,
        };
        pd + self.escr.map_or(0, |_| 6)
            + self.es_rate.map_or(0, |_| 3)
            + self.trick_mode.map_or(0, |_| 1)
            + self.additional_copy_info.map_or(0, |_| 1)
            + self.previous_pes_packet_crc.map_or(0, |_| 2)
            + self.extension.as_ref().map_or(0, PesExtension::len)
            + self.stuffing.len()
    }

    /// Append the encoded optional header.
    pub fn encode(&self, out: &mut Vec<u8>) -> Result<()> {
        let hdl = self.header_data_length();
        if hdl > 255 {
            return Err(Error::Overflow {
                what: "PES_header_data_length",
                value: hdl as u64,
                bits: 8,
            });
        }
        let mut w = BitWriter::new();
        w.write("'10'", 2, u64::from(self.marker_bits))?;
        w.write(
            "PES_scrambling_control",
            2,
            u64::from(self.scrambling_control),
        )?;
        w.bool(self.priority);
        w.bool(self.data_alignment_indicator);
        w.bool(self.copyright);
        w.bool(self.original_or_copy);
        w.write("PTS_DTS_flags", 2, u64::from(self.pts_dts.flags()))?;
        w.bool(self.escr.is_some());
        w.bool(self.es_rate.is_some());
        w.bool(self.trick_mode.is_some());
        w.bool(self.additional_copy_info.is_some());
        w.bool(self.previous_pes_packet_crc.is_some());
        w.bool(self.extension.is_some());
        w.write("PES_header_data_length", 8, hdl as u64)?;
        match &self.pts_dts {
            PtsDts::Pts(p) => p.write(&mut w)?,
            PtsDts::PtsDts(p, d) => {
                p.write(&mut w)?;
                d.write(&mut w)?;
            }
            _ => {}
        }
        if let Some(e) = self.escr {
            if e.base >> 33 != 0 {
                return Err(Error::Overflow {
                    what: "ESCR_base",
                    value: e.base,
                    bits: 33,
                });
            }
            w.write("ESCR reserved", 2, u64::from(e.reserved))?;
            w.write("ESCR_base[32..30]", 3, e.base >> 30)?;
            w.write("marker_bit", 1, (u64::from(e.markers) >> 3) & 1)?;
            w.write("ESCR_base[29..15]", 15, (e.base >> 15) & 0x7FFF)?;
            w.write("marker_bit", 1, (u64::from(e.markers) >> 2) & 1)?;
            w.write("ESCR_base[14..0]", 15, e.base & 0x7FFF)?;
            w.write("marker_bit", 1, (u64::from(e.markers) >> 1) & 1)?;
            w.write("ESCR_extension", 9, u64::from(e.extension))?;
            w.write("marker_bit", 1, u64::from(e.markers) & 1)?;
        }
        if let Some(e) = self.es_rate {
            w.write("marker_bit", 1, (u64::from(e.markers) >> 1) & 1)?;
            w.write("ES_rate", 22, u64::from(e.rate))?;
            w.write("marker_bit", 1, u64::from(e.markers) & 1)?;
        }
        if let Some(t) = self.trick_mode {
            w.write("trick mode", 8, u64::from(t))?;
        }
        if let Some(a) = self.additional_copy_info {
            w.bool(a.marker);
            w.write("additional_copy_info", 7, u64::from(a.info))?;
        }
        if let Some(c) = self.previous_pes_packet_crc {
            w.write("previous_PES_packet_CRC", 16, u64::from(c))?;
        }
        if let Some(e) = &self.extension {
            e.write(&mut w)?;
        }
        w.bytes(&self.stuffing);
        out.extend_from_slice(&w.into_bytes());
        Ok(())
    }
}

impl PesExtension {
    fn len(&self) -> usize {
        1 + self.private_data.map_or(0, |_| 16)
            + self.pack_header.as_ref().map_or(0, |p| 1 + p.len())
            + self.program_packet_sequence_counter.map_or(0, |_| 2)
            + self.p_std_buffer.map_or(0, |_| 2)
            + self.extension2.as_ref().map_or(0, |e| 1 + e.data.len())
    }

    fn read(r: &mut BitReader<'_>) -> Result<Self> {
        let private_flag = r.bool()?;
        let pack_flag = r.bool()?;
        let ppsc_flag = r.bool()?;
        let pstd_flag = r.bool()?;
        let reserved = r.read(3)? as u8;
        let ext2_flag = r.bool()?;
        let private_data = if private_flag {
            let mut a = [0u8; 16];
            a.copy_from_slice(r.bytes(16)?);
            Some(a)
        } else {
            None
        };
        let pack_header = if pack_flag {
            let n = r.u8()? as usize;
            Some(
                r.bytes(n)
                    .map_err(|_| Error::invalid("pack_field_length", "runs past the PES header"))?
                    .to_vec(),
            )
        } else {
            None
        };
        let program_packet_sequence_counter = if ppsc_flag {
            let m0 = marker(r)?;
            let counter = r.read(7)? as u8;
            let m1 = marker(r)?;
            Some(ProgramPacketSequenceCounter {
                markers: (m0 << 1) | m1,
                counter,
                mpeg1_mpeg2_identifier: r.bool()?,
                original_stuff_length: r.read(6)? as u8,
            })
        } else {
            None
        };
        let p_std_buffer = if pstd_flag {
            Some(PStdBuffer {
                prefix: r.read(2)? as u8,
                scale: r.bool()?,
                size: r.read(13)? as u16,
            })
        } else {
            None
        };
        let extension2 = if ext2_flag {
            let marker = r.bool()?;
            let n = r.read(7)? as usize;
            let data = r
                .bytes(n)
                .map_err(|_| {
                    Error::invalid("PES_extension_field_length", "runs past the PES header")
                })?
                .to_vec();
            Some(PesExtension2 { marker, data })
        } else {
            None
        };
        Ok(Self {
            reserved,
            private_data,
            pack_header,
            program_packet_sequence_counter,
            p_std_buffer,
            extension2,
        })
    }

    fn write(&self, w: &mut BitWriter) -> Result<()> {
        w.bool(self.private_data.is_some());
        w.bool(self.pack_header.is_some());
        w.bool(self.program_packet_sequence_counter.is_some());
        w.bool(self.p_std_buffer.is_some());
        w.write("PES_extension reserved", 3, u64::from(self.reserved))?;
        w.bool(self.extension2.is_some());
        if let Some(p) = &self.private_data {
            w.bytes(p);
        }
        if let Some(p) = &self.pack_header {
            w.write("pack_field_length", 8, p.len() as u64)?;
            w.bytes(p);
        }
        if let Some(c) = self.program_packet_sequence_counter {
            w.write("marker_bit", 1, (u64::from(c.markers) >> 1) & 1)?;
            w.write("program_packet_sequence_counter", 7, u64::from(c.counter))?;
            w.write("marker_bit", 1, u64::from(c.markers) & 1)?;
            w.bool(c.mpeg1_mpeg2_identifier);
            w.write(
                "original_stuff_length",
                6,
                u64::from(c.original_stuff_length),
            )?;
        }
        if let Some(p) = self.p_std_buffer {
            w.write("'01'", 2, u64::from(p.prefix))?;
            w.bool(p.scale);
            w.write("P-STD_buffer_size", 13, u64::from(p.size))?;
        }
        if let Some(e) = &self.extension2 {
            w.bool(e.marker);
            w.write("PES_extension_field_length", 7, e.data.len() as u64)?;
            w.bytes(&e.data);
        }
        Ok(())
    }
}

impl PesPacket {
    /// PES packet with an optional header (for stream_ids that have one).
    pub fn new(stream_id: u8, header: PesHeader, data: Vec<u8>) -> Self {
        Self {
            stream_id,
            unbounded: false,
            body: PesBody::Header { header, data },
        }
    }

    /// Decode one complete PES packet.  For a bounded packet `b` must be
    /// exactly `6 + PES_packet_length` bytes; for an unbounded one
    /// (PES_packet_length 0) everything after the header is data.
    pub fn decode(b: &[u8]) -> Result<Self> {
        if b.len() < 6 {
            return Err(Error::Truncated {
                what: "PES packet",
                need: 6,
                have: b.len(),
            });
        }
        if b[..3] != START_CODE_PREFIX {
            return Err(Error::invalid(
                "packet_start_code_prefix",
                format!("{:02x?}, shall be 00 00 01", &b[..3]),
            ));
        }
        let stream_id = b[3];
        let len = u16::from_be_bytes([b[4], b[5]]) as usize;
        let unbounded = len == 0;
        if !unbounded && b.len() != 6 + len {
            return Err(Error::invalid(
                "PES_packet_length",
                format!("{len} but {} bytes follow", b.len() - 6),
            ));
        }
        let rest = &b[6..];
        let body = if stream_id == stream_id::PADDING_STREAM {
            PesBody::Padding(rest.to_vec())
        } else if !has_optional_header(stream_id) {
            PesBody::Data(rest.to_vec())
        } else {
            let (header, n) = PesHeader::decode(rest)?;
            PesBody::Header {
                header,
                data: rest[n..].to_vec(),
            }
        };
        Ok(Self {
            stream_id,
            unbounded,
            body,
        })
    }

    /// Encode; PES_packet_length is computed (or 0 when `unbounded`).
    /// A bounded packet with nothing after PES_packet_length is rejected,
    /// since length 0 means "neither specified nor bounded".
    pub fn encode(&self) -> Result<Vec<u8>> {
        let mut v = Vec::new();
        v.extend_from_slice(&START_CODE_PREFIX);
        v.push(self.stream_id);
        v.extend_from_slice(&[0, 0]);
        match &self.body {
            PesBody::Header { header, data } => {
                header.encode(&mut v)?;
                v.extend_from_slice(data);
            }
            PesBody::Data(d) | PesBody::Padding(d) => v.extend_from_slice(d),
        }
        if !self.unbounded {
            let len = v.len() - 6;
            if len > 0xFFFF {
                return Err(Error::Overflow {
                    what: "PES_packet_length",
                    value: len as u64,
                    bits: 16,
                });
            }
            if len == 0 {
                return Err(Error::invalid(
                    "PES_packet_length",
                    "an empty bounded PES packet would encode as 0, which means unbounded (§2.4.3.7)",
                ));
            }
            v[4..6].copy_from_slice(&(len as u16).to_be_bytes());
        }
        Ok(v)
    }

    /// The optional header, if this stream_id has one.
    pub fn header(&self) -> Option<&PesHeader> {
        match &self.body {
            PesBody::Header { header, .. } => Some(header),
            _ => None,
        }
    }

    /// PES_packet_data_bytes (or padding bytes).
    pub fn data(&self) -> &[u8] {
        match &self.body {
            PesBody::Header { data, .. } | PesBody::Data(data) | PesBody::Padding(data) => data,
        }
    }
}

/// Report violations of §2.4.3.7 on a decoded PES packet: '10' bits,
/// PTS_DTS_flags '01' forbidden, PTS/DTS prefixes ('0010'/'0011'/'0001')
/// and marker bits, ES_rate ≠ 0, rep_cntrl ≠ 0, at most 32 stuffing bytes of
/// value 0xFF, PES_packet_length 0 only for video (Table 2-22 1110 xxxx),
/// P-STD '01' prefix, reserved bits '1' (§2.1.64).
pub fn check(p: &PesPacket) -> Vec<Violation> {
    let c = "§2.4.3.7";
    let mut v = Vec::new();
    if p.unbounded {
        require(
            &mut v,
            matches!(classify_stream_id(p.stream_id), StreamIdClass::Video(_)),
            c,
            || {
                format!(
                    "PES_packet_length 0 with non-video stream_id {:#04x}",
                    p.stream_id
                )
            },
        );
    }
    match &p.body {
        PesBody::Padding(d) => require(&mut v, d.iter().all(|&b| b == 0xFF), c, || {
            "padding_byte not 0xFF".into()
        }),
        PesBody::Data(_) => {}
        PesBody::Header { header: h, .. } => {
            require(
                &mut v,
                h.marker_bits == 0b10,
                "§2.4.3.6 Table 2-21",
                || format!("leading bits '{:02b}', shall be '10'", h.marker_bits),
            );
            let ts = |v: &mut Vec<Violation>, t: &Timestamp, prefix: u8, name: &str| {
                require(v, t.prefix == prefix, "§2.4.3.6 Table 2-21", || {
                    format!("{name} prefix '{:04b}', shall be '{prefix:04b}'", t.prefix)
                });
                require(v, t.markers == 0b111, c, || {
                    format!("{name} marker bits {:03b}", t.markers)
                });
            };
            match &h.pts_dts {
                PtsDts::Forbidden => v.push(Violation::new(c, "PTS_DTS_flags '01' is forbidden")),
                PtsDts::Pts(p) => ts(&mut v, p, Timestamp::PREFIX_PTS_ONLY, "PTS"),
                PtsDts::PtsDts(p, d) => {
                    ts(&mut v, p, Timestamp::PREFIX_PTS_WITH_DTS, "PTS");
                    ts(&mut v, d, Timestamp::PREFIX_DTS, "DTS");
                }
                PtsDts::None => {}
            }
            if let Some(e) = h.escr {
                reserved_ones(&mut v, u64::from(e.reserved), 2, "ESCR reserved");
                require(&mut v, e.markers == 0b1111, c, || {
                    format!("ESCR marker bits {:04b}", e.markers)
                });
            }
            if let Some(e) = h.es_rate {
                require(&mut v, e.markers == 0b11, c, || {
                    format!("ES_rate marker bits {:02b}", e.markers)
                });
                require(&mut v, e.rate != 0, c, || {
                    "ES_rate value '0' is forbidden".into()
                });
            }
            if let Some(t) = h.trick_mode {
                match TrickMode::from_byte(t) {
                    TrickMode::SlowMotion { rep_cntrl: 0 }
                    | TrickMode::SlowReverse { rep_cntrl: 0 } => {
                        v.push(Violation::new(c, "rep_cntrl value '0' is forbidden"))
                    }
                    TrickMode::Reserved { control, .. } => v.push(Violation::new(
                        "§2.4.3.7 Table 2-24",
                        format!("trick_mode_control '{control:03b}' is reserved"),
                    )),
                    _ => {}
                }
            }
            if let Some(a) = h.additional_copy_info {
                require(&mut v, a.marker, c, || {
                    "additional_copy_info marker_bit '0'".into()
                });
            }
            if let Some(e) = &h.extension {
                reserved_ones(&mut v, u64::from(e.reserved), 3, "PES_extension reserved");
                if let Some(s) = e.program_packet_sequence_counter {
                    require(&mut v, s.markers == 0b11, c, || {
                        "program_packet_sequence_counter marker bits".into()
                    });
                }
                if let Some(b) = e.p_std_buffer {
                    require(&mut v, b.prefix == 0b01, "§2.4.3.6 Table 2-21", || {
                        format!("P-STD prefix '{:02b}', shall be '01'", b.prefix)
                    });
                }
                if let Some(x) = &e.extension2 {
                    require(&mut v, x.marker, c, || {
                        "PES_extension_field_length marker_bit '0'".into()
                    });
                }
            }
            require(&mut v, h.stuffing.len() <= 32, c, || {
                format!("{} stuffing bytes, at most 32", h.stuffing.len())
            });
            require(&mut v, h.stuffing.iter().all(|&b| b == 0xFF), c, || {
                "stuffing_byte not 0xFF".into()
            });
        }
    }
    v
}

/// Split an encoded PES packet into Transport Stream packets on `pid`
/// (§2.4.3.3, §2.4.3.5): the first packet has payload_unit_start_indicator
/// '1' and carries `first_af` (e.g. PCR, random_access_indicator) if given;
/// the last packet is padded with adaptation-field stuffing — "the only
/// method of stuffing allowed for Transport Stream packets carrying PES
/// packets" (§2.4.3.5).  Continuity counters come from `cc`.
pub fn packetize(
    pid: Pid,
    pes: &[u8],
    first_af: Option<AdaptationFields>,
    cc: &mut Counter,
) -> Result<Vec<[u8; PACKET_SIZE]>> {
    let mut out = Vec::new();
    let mut rest = pes;
    let mut af = first_af;
    let mut first = true;
    loop {
        let af_len = af.as_ref().map_or(0, |f| {
            let mut f = f.clone();
            f.stuffing.clear();
            1 + f.length()
        });
        if af_len > PAYLOAD_CAPACITY - 1 {
            return Err(Error::invalid(
                "adaptation_field",
                "first_af leaves no room for payload",
            ));
        }
        let cap = PAYLOAD_CAPACITY - af_len;
        let n = rest.len().min(cap);
        let (chunk, tail) = rest.split_at(n);
        let p = if af.is_none() && n == PAYLOAD_CAPACITY {
            Packet::with_payload(pid, first, cc.next(true), chunk)?
        } else {
            Packet::with_adaptation(pid, first, cc.next(true), af.take(), chunk)?
        };
        out.push(p.encode()?);
        rest = tail;
        first = false;
        if rest.is_empty() {
            break;
        }
    }
    Ok(out)
}

/// A reassembled PES packet (H.222.0 §2.4.3.6, Table 2-21).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssembledPes {
    /// PID.
    pub pid: Pid,
    /// Index of the packet with PUSI that started it.
    pub start_packet: u64,
    /// Index of the last packet contributing bytes.
    pub end_packet: u64,
    /// The PES packet bytes.
    pub data: Vec<u8>,
}

/// Events of the PES reassembler (H.222.0 §2.4.3.3, §2.4.3.7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PesEvent {
    /// A partial PES packet was dropped.
    Discarded {
        /// Packet index.
        packet_index: u64,
        /// Bytes dropped.
        bytes: usize,
        /// Why.
        reason: &'static str,
    },
    /// Bytes after the end of a bounded PES packet before the next PUSI.
    TrailingBytes {
        /// Packet index.
        packet_index: u64,
        /// Count.
        bytes: usize,
    },
}

/// Output of [`PesAssembler::push`] (H.222.0 §2.4.3.6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PesOutput {
    /// A complete PES packet.
    Pes(AssembledPes),
    /// A diagnostic event.
    Event(PesEvent),
}

/// Per-PID TS → PES reassembler: a PES packet starts in a packet with
/// payload_unit_start_indicator '1' (§2.4.3.3 "one and only one PES packet
/// starts in this Transport Stream packet") and ends after
/// `6 + PES_packet_length` bytes, or — for PES_packet_length 0 — at the
/// next PUSI (or [`PesAssembler::flush`]).
#[derive(Debug, Clone)]
pub struct PesAssembler {
    pid: Pid,
    cc: Tracker,
    buf: Vec<u8>,
    start: u64,
    last: u64,
    active: bool,
    done: bool,
}

impl PesAssembler {
    /// Reassembler for `pid`.
    pub fn new(pid: Pid) -> Self {
        Self {
            pid,
            cc: Tracker::new(),
            buf: Vec::new(),
            start: 0,
            last: 0,
            active: false,
            done: false,
        }
    }

    fn declared(&self) -> Option<usize> {
        (self.buf.len() >= 6)
            .then(|| u16::from_be_bytes([self.buf[4], self.buf[5]]) as usize)
            .filter(|&l| l != 0)
            .map(|l| 6 + l)
    }

    fn drop_partial(&mut self, index: u64, reason: &'static str, out: &mut Vec<PesOutput>) {
        if self.active && !self.done {
            out.push(PesOutput::Event(PesEvent::Discarded {
                packet_index: index,
                bytes: self.buf.len(),
                reason,
            }));
        }
        self.active = false;
        self.done = false;
        self.buf.clear();
    }

    fn emit(&mut self, out: &mut Vec<PesOutput>) {
        out.push(PesOutput::Pes(AssembledPes {
            pid: self.pid,
            start_packet: self.start,
            end_packet: self.last,
            data: std::mem::take(&mut self.buf),
        }));
        self.done = true;
    }

    /// Feed packet `index`; packets of other PIDs are ignored.
    pub fn push(&mut self, index: u64, pkt: &[u8; PACKET_SIZE]) -> Vec<PesOutput> {
        let mut out = Vec::new();
        let Ok(v) = View::parse(pkt) else { return out };
        if v.header.pid != self.pid {
            return out;
        }
        if v.header.transport_error_indicator {
            self.drop_partial(index, "transport_error_indicator", &mut out);
            self.cc.reset();
            return out;
        }
        let c = self.cc.update(&v.header, v.discontinuity_indicator());
        if c.is_repeat() {
            return out;
        }
        if c.is_loss() {
            self.drop_partial(index, "continuity_counter break", &mut out);
        }
        let Some(payload) = v.payload else { return out };
        if v.header.payload_unit_start_indicator {
            if self.active && !self.done {
                if self.declared().is_none() && self.buf.len() >= 6 {
                    self.emit(&mut out); // unbounded: ends at next PUSI
                } else {
                    self.drop_partial(
                        index,
                        "PES packet incomplete at next payload_unit_start_indicator",
                        &mut out,
                    );
                }
            }
            self.buf.clear();
            self.active = true;
            self.done = false;
            self.start = index;
        } else if !self.active {
            return out;
        } else if self.done {
            out.push(PesOutput::Event(PesEvent::TrailingBytes {
                packet_index: index,
                bytes: payload.len(),
            }));
            return out;
        }
        self.buf.extend_from_slice(payload);
        self.last = index;
        if let Some(t) = self.declared().filter(|&t| self.buf.len() >= t) {
            let extra = self.buf.len() - t;
            self.buf.truncate(t);
            self.emit(&mut out);
            if extra > 0 {
                out.push(PesOutput::Event(PesEvent::TrailingBytes {
                    packet_index: index,
                    bytes: extra,
                }));
            }
        }
        out
    }

    /// End of stream: emit an unbounded PES packet in progress.
    pub fn flush(&mut self) -> Vec<PesOutput> {
        let mut out = Vec::new();
        if self.active && !self.done && self.declared().is_none() && self.buf.len() >= 6 {
            self.emit(&mut out);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pcr::Pcr;

    /// Table 2-21: PTS_DTS_flags '10' ⇒ one '0010' field; '11' ⇒ '0011' then
    /// '0001'; PES_header_data_length counts optional fields and stuffing.
    #[test]
    fn spec_table2_21_pts_dts_prefixes() {
        let h = PesHeader {
            pts_dts: PtsDts::pts_and_dts(0x1_2345_6789, 0x0_0000_0001),
            ..Default::default()
        };
        let p = PesPacket::new(0xE0, h, vec![0xAA; 4]);
        let b = p.encode().unwrap();
        assert_eq!(&b[..6], &[0, 0, 1, 0xE0, 0, 3 + 10 + 4]);
        assert_eq!(b[6], 0x80); // '10' 00 0 0 0 0
        assert_eq!(b[7], 0xC0); // PTS_DTS_flags '11'
        assert_eq!(b[8], 10);
        assert_eq!(b[9] >> 4, 0b0011);
        assert_eq!(b[14] >> 4, 0b0001);
        // PTS bits 32..30 = 0b100 → 0011 100 1
        assert_eq!(b[9], 0b0011_1001);
        assert_eq!(&b[14..19], &[0x11, 0x00, 0x01, 0x00, 0x03]);
        let d = PesPacket::decode(&b).unwrap();
        assert_eq!(d, p);
        assert!(check(&d).is_empty());
        let h = PesHeader {
            pts_dts: PtsDts::pts_only(0),
            ..Default::default()
        };
        let b = PesPacket::new(0xC0, h, vec![]).encode().unwrap();
        assert_eq!(b[7], 0x80);
        assert_eq!(&b[9..14], &[0x21, 0x00, 0x01, 0x00, 0x01]);
        // forbidden '01'
        let mut f = b.clone();
        f[7] = 0x40;
        f[8] = 0;
        f.truncate(9);
        f[5] = 3;
        let d = PesPacket::decode(&f).unwrap();
        assert_eq!(d.header().unwrap().pts_dts, PtsDts::Forbidden);
        assert_eq!(d.encode().unwrap(), f);
        assert!(check(&d).iter().any(|v| v.message.contains("'01'")));
    }

    /// §2.4.3.7: PES_packet_length 0 allowed only for video; Table 2-21
    /// stream_ids without optional header.
    #[test]
    fn spec_2_4_3_7_length_zero_and_stream_id_classes() {
        let mut p = PesPacket::new(0xE0, PesHeader::default(), vec![1; 70_000]);
        assert!(p.encode().is_err());
        p.unbounded = true;
        let b = p.encode().unwrap();
        assert_eq!(&b[4..6], &[0, 0]);
        assert_eq!(PesPacket::decode(&b).unwrap(), p);
        assert!(check(&p).is_empty());
        p.stream_id = 0xC0;
        assert!(!check(&p).is_empty());
        for id in [0xBC, 0xBE, 0xBF, 0xF0, 0xF1, 0xF2, 0xF8, 0xFF] {
            assert!(!has_optional_header(id), "{id:#x}");
        }
        for id in [
            0xBD, 0xC0, 0xDF, 0xE0, 0xEF, 0xF3, 0xF4, 0xF7, 0xF9, 0xFA, 0xFB, 0xFC, 0xFD, 0xFE,
        ] {
            assert!(has_optional_header(id), "{id:#x}");
        }
        assert_eq!(classify_stream_id(0xC5), StreamIdClass::Audio(5));
        assert_eq!(classify_stream_id(0xE1), StreamIdClass::Video(1));
        assert_eq!(
            classify_stream_id(0xBD),
            StreamIdClass::Named("private_stream_1")
        );
        let pad = PesPacket {
            stream_id: 0xBE,
            unbounded: false,
            body: PesBody::Padding(vec![0xFF; 10]),
        };
        assert_eq!(PesPacket::decode(&pad.encode().unwrap()).unwrap(), pad);
    }

    /// Table 2-21 ESCR / ES_rate / trick mode / extension layouts.
    #[test]
    fn spec_table2_21_optional_fields() {
        let h = PesHeader {
            escr: Some(Escr {
                reserved: 3,
                base: (1 << 33) - 1,
                extension: 0x1FF,
                markers: 0xF,
            }),
            es_rate: Some(EsRate {
                markers: 3,
                rate: 0x3F_FFFF,
            }),
            trick_mode: Some(0b001_00101),
            additional_copy_info: Some(AdditionalCopyInfo {
                marker: true,
                info: 0x55,
            }),
            previous_pes_packet_crc: Some(0xBEEF),
            extension: Some(PesExtension {
                reserved: 7,
                private_data: Some([9; 16]),
                pack_header: Some(vec![0, 0, 1, 0xBA]),
                program_packet_sequence_counter: Some(ProgramPacketSequenceCounter {
                    markers: 3,
                    counter: 0x7F,
                    mpeg1_mpeg2_identifier: true,
                    original_stuff_length: 0x3F,
                }),
                p_std_buffer: Some(PStdBuffer {
                    prefix: 1,
                    scale: true,
                    size: 0x1FFF,
                }),
                extension2: Some(PesExtension2 {
                    marker: true,
                    data: vec![0x05, 0xFF],
                }),
            }),
            stuffing: vec![0xFF; 3],
            ..Default::default()
        };
        let p = PesPacket::new(0xFD, h, vec![1, 2, 3]);
        let b = p.encode().unwrap();
        // ESCR: 11 111 1 | 15 ones 1 | 15 ones 1 | 9 ones 1 → all ones
        assert_eq!(&b[9..15], &[0xFF; 6]);
        assert_eq!(&b[15..18], &[0xFF; 3]); // ES_rate all ones
        assert_eq!(
            TrickMode::from_byte(b[18]),
            TrickMode::SlowMotion { rep_cntrl: 5 }
        );
        let d = PesPacket::decode(&b).unwrap();
        assert_eq!(d, p);
        assert_eq!(
            d.header()
                .unwrap()
                .extension
                .as_ref()
                .unwrap()
                .extension2
                .as_ref()
                .unwrap()
                .stream_id_extension(),
            Some(5)
        );
        assert!(check(&d).is_empty(), "{:?}", check(&d));
    }

    #[test]
    fn packetize_and_reassemble() {
        for n in [
            0usize, 1, 160, 170, 171, 175, 176, 177, 178, 183, 184, 400, 1000,
        ] {
            let h = PesHeader {
                pts_dts: PtsDts::pts_only(n as u64),
                ..Default::default()
            };
            let pes = PesPacket::new(0xE0, h, (0..n).map(|i| i as u8).collect())
                .encode()
                .unwrap();
            let mut cc = Counter::default();
            let af = AdaptationFields {
                random_access_indicator: true,
                pcr: Some(Pcr::from_ticks(n as u64)),
                ..Default::default()
            };
            let pkts = packetize(Pid::new(0x100).unwrap(), &pes, Some(af), &mut cc).unwrap();
            let mut payload = Vec::new();
            for (i, raw) in pkts.iter().enumerate() {
                let p = Packet::decode(raw).unwrap();
                assert_eq!(p.header.payload_unit_start_indicator, i == 0);
                assert!(crate::packet::check(&p).is_empty());
                if i == 0 {
                    assert_eq!(
                        p.adaptation_field.as_ref().unwrap().pcr(),
                        Some(Pcr::from_ticks(n as u64))
                    );
                }
                payload.extend_from_slice(&p.payload);
            }
            assert_eq!(payload, pes, "n={n}");
            let mut a = PesAssembler::new(Pid::new(0x100).unwrap());
            let mut got = vec![];
            for (i, raw) in pkts.iter().enumerate() {
                got.extend(a.push(i as u64, raw));
            }
            assert_eq!(
                got,
                vec![PesOutput::Pes(AssembledPes {
                    pid: Pid::new(0x100).unwrap(),
                    start_packet: 0,
                    end_packet: pkts.len() as u64 - 1,
                    data: pes
                })]
            );
        }
    }

    #[test]
    fn reassembler_unbounded_and_loss() {
        let mut p = PesPacket::new(0xE0, PesHeader::default(), vec![7; 500]);
        p.unbounded = true;
        let pes = p.encode().unwrap();
        let mut cc = Counter::default();
        let mut pkts = packetize(Pid::new(0x101).unwrap(), &pes, None, &mut cc).unwrap();
        pkts.extend(packetize(Pid::new(0x101).unwrap(), &pes, None, &mut cc).unwrap());
        let mut a = PesAssembler::new(Pid::new(0x101).unwrap());
        let mut got = vec![];
        for (i, raw) in pkts.iter().enumerate() {
            if i == 4 {
                continue; // lose the 2nd packet of the second PES
            }
            got.extend(a.push(i as u64, raw));
        }
        got.extend(a.flush());
        assert!(matches!(&got[0], PesOutput::Pes(x) if x.data == pes));
        assert!(matches!(
            &got[1],
            PesOutput::Event(PesEvent::Discarded { .. })
        ));
        assert_eq!(got.len(), 2);
    }
}

//! SI table bodies (ARIB STD-B10 Part 2 §5.2.4–§5.2.15).
//!
//! Long-form tables implement [`LongBody`] (the bytes between
//! `last_section_number` and `CRC_32`); short-form ones implement
//! [`ShortBody`].  The section header (with `table_id_extension` =
//! network_id / transport_stream_id / service_id / … as noted per table)
//! is the `header` of the [`Long`]/[`Short`] wrapper from `isdb-ts`.
//! Every reserved / reserved_future_use field is kept verbatim; all loop
//! lengths are recomputed on encode.

use crate::descriptor::{Descriptor, decode_loop, loop_bytes};
use crate::time::{BcdDuration, MjdTime};
use isdb_ts::bits::{BitReader, BitWriter};
use isdb_ts::section::{Long, LongBody, LongHeader, Short, ShortBody, ShortHeader};
use isdb_ts::{Error, Result};

/// Read a `reserved(4) + length(12)` word and the descriptor loop it sizes.
fn rfu4_loop(r: &mut BitReader<'_>) -> Result<(u8, Vec<Descriptor>)> {
    let rfu = r.read_u8(4)?;
    let n = r.read_usize(12)?;
    Ok((rfu, decode_loop(r.bytes(n)?)?))
}

/// Write a `reserved(4) + length(12)` word followed by the loop bytes.
fn put_rfu4_bytes(w: &mut BitWriter, rfu: u8, what: &'static str, bytes: &[u8]) -> Result<()> {
    w.write("reserved_future_use", 4, u64::from(rfu))?;
    w.write(what, 12, bytes.len() as u64)?;
    w.bytes(bytes);
    Ok(())
}

fn put_rfu4_loop(w: &mut BitWriter, rfu: u8, what: &'static str, ds: &[Descriptor]) -> Result<()> {
    put_rfu4_bytes(w, rfu, what, &loop_bytes(ds)?)
}

// ---------------------------------------------------------------------------
// NIT / BAT
// ---------------------------------------------------------------------------

/// Transport stream entry of the NIT/BAT transport stream loop (ARIB
/// STD-B10 Part 2 Tables 5-3/5-4).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TransportStreamEntry {
    /// transport_stream_id (16).
    pub transport_stream_id: u16,
    /// original_network_id (16).
    pub original_network_id: u16,
    /// reserved_future_use (4), spec value '1111'.
    pub reserved_future_use: u8,
    /// Transport descriptors.
    pub descriptors: Vec<Descriptor>,
}

/// Shared shape of NIT and BAT bodies: first descriptor loop + transport
/// stream loop.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NetworkLoops {
    /// reserved_future_use (4) before the first descriptor loop length.
    pub reserved_future_use_1: u8,
    /// network_descriptors / bouquet_descriptors.
    pub descriptors: Vec<Descriptor>,
    /// reserved_future_use (4) before transport_stream_loop_length.
    pub reserved_future_use_2: u8,
    /// Transport stream loop.
    pub transport_streams: Vec<TransportStreamEntry>,
}

impl NetworkLoops {
    fn decode(body: &[u8], what: &'static str) -> Result<Self> {
        let mut r = BitReader::new(body, what);
        let (reserved_future_use_1, descriptors) = rfu4_loop(&mut r)?;
        let reserved_future_use_2 = r.read_u8(4)?;
        let n = r.read_usize(12)?;
        let ts_bytes = r.bytes(n)?;
        r.finish()?;
        let mut tr = BitReader::new(ts_bytes, what);
        let mut transport_streams = Vec::new();
        while tr.remaining_bytes() > 0 {
            let transport_stream_id = tr.u16()?;
            let original_network_id = tr.u16()?;
            let (reserved_future_use, descriptors) = rfu4_loop(&mut tr)?;
            transport_streams.push(TransportStreamEntry {
                transport_stream_id,
                original_network_id,
                reserved_future_use,
                descriptors,
            });
        }
        Ok(Self {
            reserved_future_use_1,
            descriptors,
            reserved_future_use_2,
            transport_streams,
        })
    }

    fn encode(&self, out: &mut Vec<u8>, first_len: &'static str) -> Result<()> {
        let mut ts = BitWriter::new();
        for t in &self.transport_streams {
            ts.write("transport_stream_id", 16, u64::from(t.transport_stream_id))?;
            ts.write("original_network_id", 16, u64::from(t.original_network_id))?;
            put_rfu4_loop(
                &mut ts,
                t.reserved_future_use,
                "transport_descriptors_length",
                &t.descriptors,
            )?;
        }
        let mut w = BitWriter::new();
        put_rfu4_loop(
            &mut w,
            self.reserved_future_use_1,
            first_len,
            &self.descriptors,
        )?;
        put_rfu4_bytes(
            &mut w,
            self.reserved_future_use_2,
            "transport_stream_loop_length",
            &ts.into_bytes(),
        )?;
        out.extend_from_slice(&w.into_bytes());
        Ok(())
    }
}

/// network_information_section body, table_id 0x40 (actual) / 0x41
/// (other); `table_id_extension` is network_id.  ARIB STD-B10 Part 2
/// §5.2.4, Table 5-3.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NitBody(pub NetworkLoops);

impl LongBody for NitBody {
    fn accepts_table_id(t: u8) -> bool {
        t == 0x40 || t == 0x41
    }
    fn decode(_: &LongHeader, body: &[u8]) -> Result<Self> {
        Ok(Self(NetworkLoops::decode(
            body,
            "network_information_section",
        )?))
    }
    fn encode(&self, out: &mut Vec<u8>) -> Result<()> {
        self.0.encode(out, "network_descriptors_length")
    }
}

/// bouquet_association_section body, table_id 0x4A; `table_id_extension`
/// is bouquet_id.  ARIB STD-B10 Part 2 §5.2.5, Table 5-4.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BatBody(pub NetworkLoops);

impl LongBody for BatBody {
    fn accepts_table_id(t: u8) -> bool {
        t == 0x4A
    }
    fn decode(_: &LongHeader, body: &[u8]) -> Result<Self> {
        Ok(Self(NetworkLoops::decode(
            body,
            "bouquet_association_section",
        )?))
    }
    fn encode(&self, out: &mut Vec<u8>) -> Result<()> {
        self.0.encode(out, "bouquet_descriptors_length")
    }
}

// ---------------------------------------------------------------------------
// SDT
// ---------------------------------------------------------------------------

/// Service entry of the SDT service loop (ARIB STD-B10 Part 2 Table 5-5).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SdtService {
    /// service_id (16).
    pub service_id: u16,
    /// reserved_future_use (3), spec value '111'.
    pub reserved_future_use: u8,
    /// EIT_user_defined_flags (3); '111' means no use.
    pub eit_user_defined_flags: u8,
    /// EIT_schedule_flag (1).
    pub eit_schedule_flag: bool,
    /// EIT_present_following_flag (1).
    pub eit_present_following_flag: bool,
    /// running_status (3), Table 5-6.
    pub running_status: u8,
    /// free_CA_mode (1).
    pub free_ca_mode: bool,
    /// Service descriptors.
    pub descriptors: Vec<Descriptor>,
}

/// service_description_section body, table_id 0x42 (actual) / 0x46
/// (other); `table_id_extension` is transport_stream_id.  ARIB STD-B10
/// Part 2 §5.2.6, Table 5-5.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SdtBody {
    /// original_network_id (16).
    pub original_network_id: u16,
    /// reserved_future_use (8), spec value 0xFF.
    pub reserved_future_use: u8,
    /// Service loop.
    pub services: Vec<SdtService>,
}

impl LongBody for SdtBody {
    fn accepts_table_id(t: u8) -> bool {
        t == 0x42 || t == 0x46
    }
    fn decode(_: &LongHeader, body: &[u8]) -> Result<Self> {
        let mut r = BitReader::new(body, "service_description_section");
        let original_network_id = r.u16()?;
        let reserved_future_use = r.u8()?;
        let mut services = Vec::new();
        while r.remaining_bytes() > 0 {
            let service_id = r.u16()?;
            let reserved_future_use = r.read_u8(3)?;
            let eit_user_defined_flags = r.read_u8(3)?;
            let eit_schedule_flag = r.bool()?;
            let eit_present_following_flag = r.bool()?;
            let running_status = r.read_u8(3)?;
            let free_ca_mode = r.bool()?;
            let n = r.read_usize(12)?;
            let descriptors = decode_loop(r.bytes(n)?)?;
            services.push(SdtService {
                service_id,
                reserved_future_use,
                eit_user_defined_flags,
                eit_schedule_flag,
                eit_present_following_flag,
                running_status,
                free_ca_mode,
                descriptors,
            });
        }
        Ok(Self {
            original_network_id,
            reserved_future_use,
            services,
        })
    }
    fn encode(&self, out: &mut Vec<u8>) -> Result<()> {
        let mut w = BitWriter::new();
        w.write(
            "original_network_id",
            16,
            u64::from(self.original_network_id),
        )?;
        w.write(
            "reserved_future_use",
            8,
            u64::from(self.reserved_future_use),
        )?;
        for s in &self.services {
            w.write("service_id", 16, u64::from(s.service_id))?;
            w.write("reserved_future_use", 3, u64::from(s.reserved_future_use))?;
            w.write(
                "EIT_user_defined_flags",
                3,
                u64::from(s.eit_user_defined_flags),
            )?;
            w.bool(s.eit_schedule_flag);
            w.bool(s.eit_present_following_flag);
            w.write("running_status", 3, u64::from(s.running_status))?;
            w.bool(s.free_ca_mode);
            let d = loop_bytes(&s.descriptors)?;
            w.write("descriptors_loop_length", 12, d.len() as u64)?;
            w.bytes(&d);
        }
        out.extend_from_slice(&w.into_bytes());
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// EIT
// ---------------------------------------------------------------------------

/// Event entry of the EIT event loop (ARIB STD-B10 Part 2 Table 5-7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EitEvent {
    /// event_id (16).
    pub event_id: u16,
    /// start_time (40), MJD + BCD JST; all '1' if undefined.
    pub start_time: MjdTime,
    /// duration (24), BCD hhmmss; all '1' if undefined.
    pub duration: BcdDuration,
    /// running_status (3), Table 5-6.
    pub running_status: u8,
    /// free_CA_mode (1).
    pub free_ca_mode: bool,
    /// Event descriptors.
    pub descriptors: Vec<Descriptor>,
}

/// event_information_section body, table_id 0x4E/0x4F (present/following
/// actual/other), 0x50–0x5F (schedule actual), 0x60–0x6F (schedule other);
/// `table_id_extension` is service_id.  ARIB STD-B10 Part 2 §5.2.7,
/// Table 5-7.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EitBody {
    /// transport_stream_id (16).
    pub transport_stream_id: u16,
    /// original_network_id (16).
    pub original_network_id: u16,
    /// segment_last_section_number (8).
    pub segment_last_section_number: u8,
    /// last_table_id (8).
    pub last_table_id: u8,
    /// Event loop.
    pub events: Vec<EitEvent>,
}

impl LongBody for EitBody {
    fn accepts_table_id(t: u8) -> bool {
        (0x4E..=0x6F).contains(&t)
    }
    fn decode(_: &LongHeader, body: &[u8]) -> Result<Self> {
        let mut r = BitReader::new(body, "event_information_section");
        let transport_stream_id = r.u16()?;
        let original_network_id = r.u16()?;
        let segment_last_section_number = r.u8()?;
        let last_table_id = r.u8()?;
        let mut events = Vec::new();
        while r.remaining_bytes() > 0 {
            let event_id = r.u16()?;
            let start_time = MjdTime::from_u40(r.read(40)?);
            let duration = BcdDuration(r.read_u32(24)?);
            let running_status = r.read_u8(3)?;
            let free_ca_mode = r.bool()?;
            let n = r.read_usize(12)?;
            let descriptors = decode_loop(r.bytes(n)?)?;
            events.push(EitEvent {
                event_id,
                start_time,
                duration,
                running_status,
                free_ca_mode,
                descriptors,
            });
        }
        Ok(Self {
            transport_stream_id,
            original_network_id,
            segment_last_section_number,
            last_table_id,
            events,
        })
    }
    fn encode(&self, out: &mut Vec<u8>) -> Result<()> {
        let mut w = BitWriter::new();
        w.write(
            "transport_stream_id",
            16,
            u64::from(self.transport_stream_id),
        )?;
        w.write(
            "original_network_id",
            16,
            u64::from(self.original_network_id),
        )?;
        w.write(
            "segment_last_section_number",
            8,
            u64::from(self.segment_last_section_number),
        )?;
        w.write("last_table_id", 8, u64::from(self.last_table_id))?;
        for e in &self.events {
            w.write("event_id", 16, u64::from(e.event_id))?;
            w.write("start_time", 40, e.start_time.checked_u40()?)?;
            w.write("duration", 24, u64::from(e.duration.0))?;
            w.write("running_status", 3, u64::from(e.running_status))?;
            w.bool(e.free_ca_mode);
            let d = loop_bytes(&e.descriptors)?;
            w.write("descriptors_loop_length", 12, d.len() as u64)?;
            w.bytes(&d);
        }
        out.extend_from_slice(&w.into_bytes());
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// TDT / TOT / RST
// ---------------------------------------------------------------------------

/// time_date_section body, table_id 0x70 (short form, no CRC_32).  ARIB
/// STD-B10 Part 2 §5.2.8, Table 5-8.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TdtBody {
    /// JST_time (40).
    pub jst_time: MjdTime,
}

impl ShortBody for TdtBody {
    const HAS_CRC: bool = false;
    fn accepts_table_id(t: u8) -> bool {
        t == 0x70
    }
    fn decode(_: &ShortHeader, body: &[u8]) -> Result<Self> {
        let mut r = BitReader::new(body, "time_date_section");
        let jst_time = MjdTime::from_u40(r.read(40)?);
        r.finish()?;
        Ok(Self { jst_time })
    }
    fn encode(&self, out: &mut Vec<u8>) -> Result<()> {
        out.extend_from_slice(&self.jst_time.checked_u40()?.to_be_bytes()[3..]);
        Ok(())
    }
}

/// time_offset_section body, table_id 0x73 (short form with CRC_32).  ARIB
/// STD-B10 Part 2 §5.2.9, Table 5-9.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TotBody {
    /// JST_time (40).
    pub jst_time: MjdTime,
    /// reserved (4), spec value '1111'.
    pub reserved: u8,
    /// Descriptors (descriptors_loop_length).
    pub descriptors: Vec<Descriptor>,
}

impl ShortBody for TotBody {
    const HAS_CRC: bool = true;
    fn accepts_table_id(t: u8) -> bool {
        t == 0x73
    }
    fn decode(_: &ShortHeader, body: &[u8]) -> Result<Self> {
        let mut r = BitReader::new(body, "time_offset_section");
        let jst_time = MjdTime::from_u40(r.read(40)?);
        let (reserved, descriptors) = rfu4_loop(&mut r)?;
        r.finish()?;
        Ok(Self {
            jst_time,
            reserved,
            descriptors,
        })
    }
    fn encode(&self, out: &mut Vec<u8>) -> Result<()> {
        let mut w = BitWriter::new();
        w.write("JST_time", 40, self.jst_time.checked_u40()?)?;
        put_rfu4_loop(
            &mut w,
            self.reserved,
            "descriptors_loop_length",
            &self.descriptors,
        )?;
        out.extend_from_slice(&w.into_bytes());
        Ok(())
    }
}

/// Entry of the RST loop (ARIB STD-B10 Part 2 Table 5-10).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RstEntry {
    /// transport_stream_id (16).
    pub transport_stream_id: u16,
    /// original_network_id (16).
    pub original_network_id: u16,
    /// service_id (16).
    pub service_id: u16,
    /// event_id (16).
    pub event_id: u16,
    /// reserved_future_use (5), spec value all '1'.
    pub reserved_future_use: u8,
    /// running_status (3), Table 5-6.
    pub running_status: u8,
}

/// running_status_section body, table_id 0x71 (short form, no CRC_32).
/// ARIB STD-B10 Part 2 §5.2.10, Table 5-10.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RstBody {
    /// Event status loop.
    pub entries: Vec<RstEntry>,
}

impl ShortBody for RstBody {
    const HAS_CRC: bool = false;
    fn accepts_table_id(t: u8) -> bool {
        t == 0x71
    }
    fn decode(_: &ShortHeader, body: &[u8]) -> Result<Self> {
        if body.len() / 9 * 9 != body.len() {
            return Err(Error::invalid(
                "running_status_section",
                format!("{} bytes is not a multiple of 9", body.len()),
            ));
        }
        let mut r = BitReader::new(body, "running_status_section");
        let mut entries = Vec::new();
        while r.remaining_bytes() > 0 {
            entries.push(RstEntry {
                transport_stream_id: r.u16()?,
                original_network_id: r.u16()?,
                service_id: r.u16()?,
                event_id: r.u16()?,
                reserved_future_use: r.read_u8(5)?,
                running_status: r.read_u8(3)?,
            });
        }
        Ok(Self { entries })
    }
    fn encode(&self, out: &mut Vec<u8>) -> Result<()> {
        let mut w = BitWriter::new();
        for e in &self.entries {
            w.write("transport_stream_id", 16, u64::from(e.transport_stream_id))?;
            w.write("original_network_id", 16, u64::from(e.original_network_id))?;
            w.write("service_id", 16, u64::from(e.service_id))?;
            w.write("event_id", 16, u64::from(e.event_id))?;
            w.write("reserved_future_use", 5, u64::from(e.reserved_future_use))?;
            w.write("running_status", 3, u64::from(e.running_status))?;
        }
        out.extend_from_slice(&w.into_bytes());
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// ST
// ---------------------------------------------------------------------------

/// stuffing_section, table_id 0x72 — ARIB STD-B10 Part 2 §5.2.11,
/// Table 5-11.  `section_syntax_indicator` "may take either the value '1'
/// or '0'" and no CRC_32 is defined, so the whole section after the 3-byte
/// header is `data_byte`s; the type therefore codes the complete section.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StuffingSection {
    /// table_id (8), 0x72.
    pub table_id: u8,
    /// section_syntax_indicator (1), either value.
    pub section_syntax_indicator: bool,
    /// reserved_future_use (1), spec value '1'.
    pub reserved_future_use: bool,
    /// reserved (2), spec value '11'.
    pub reserved: u8,
    /// data_byte loop.
    pub data: Vec<u8>,
}

impl StuffingSection {
    /// Decode a complete stuffing section.
    pub fn decode(sec: &[u8]) -> Result<Self> {
        let mut r = BitReader::new(sec, "stuffing_section");
        let table_id = r.u8()?;
        if table_id != 0x72 {
            // Not a stuffing_section (0x72).
            return Err(Error::NotAccepted {
                what: "table_id",
                value: table_id.into(),
            });
        }
        let section_syntax_indicator = r.bool()?;
        let reserved_future_use = r.bool()?;
        let reserved = r.read_u8(2)?;
        let len = r.read_usize(12)?;
        if sec.len() != 3 + len {
            return Err(Error::invalid(
                "stuffing_section",
                format!(
                    "buffer is {} bytes, section_length says {}",
                    sec.len(),
                    3 + len
                ),
            ));
        }
        Ok(Self {
            table_id,
            section_syntax_indicator,
            reserved_future_use,
            reserved,
            data: r.rest()?.to_vec(),
        })
    }

    /// Encode the complete section (section_length recomputed).
    pub fn encode(&self) -> Result<Vec<u8>> {
        let mut w = BitWriter::new();
        w.write("table_id", 8, u64::from(self.table_id))?;
        w.bool(self.section_syntax_indicator);
        w.bool(self.reserved_future_use);
        w.write("reserved", 2, u64::from(self.reserved))?;
        if self.data.len() > isdb_ts::section::MAX_SECTION_LENGTH {
            return Err(Error::Overflow {
                what: "section_length",
                value: self.data.len() as u64,
                bits: 12,
            });
        }
        w.write("section_length", 12, self.data.len() as u64)?;
        w.bytes(&self.data);
        Ok(w.into_bytes())
    }
}

// ---------------------------------------------------------------------------
// PCAT
// ---------------------------------------------------------------------------

/// (start_time, duration) of the PCAT schedule loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PcatSchedule {
    /// start_time (40), MJD + BCD JST.
    pub start_time: MjdTime,
    /// duration (24), BCD.
    pub duration: BcdDuration,
}

/// Content version entry of the PCAT (ARIB STD-B10 Part 2 Table 5-12).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PcatContentVersion {
    /// content_version (16).
    pub content_version: u16,
    /// content_minor_version (16).
    pub content_minor_version: u16,
    /// version_indicator (2).
    pub version_indicator: u8,
    /// reserved_future_use (2) before content_descriptor_length, spec '11'.
    pub reserved_future_use_1: u8,
    /// reserved_future_use (4) before schedule_description_length, spec '1111'.
    pub reserved_future_use_2: u8,
    /// Schedule loop.
    pub schedules: Vec<PcatSchedule>,
    /// Descriptor loop.
    pub descriptors: Vec<Descriptor>,
}

/// partial_content_announcement_section body, table_id 0xC2;
/// `table_id_extension` is service_id.  ARIB STD-B10 Part 2 §5.2.12,
/// Table 5-12.
///
/// `content_descriptor_length` "gives the total length in bytes of the
/// following schedule loop and descriptor loop": it is coded here as the
/// size of those two loops, excluding the 16-bit
/// reserved_future_use/schedule_description_length word (see
/// SPEC-NOTES.md).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PcatBody {
    /// transport_stream_id (16).
    pub transport_stream_id: u16,
    /// original_network_id (16).
    pub original_network_id: u16,
    /// content_id (32).
    pub content_id: u32,
    /// Content version loop (num_of_content_version, 8 bits).
    pub content_versions: Vec<PcatContentVersion>,
}

impl LongBody for PcatBody {
    fn accepts_table_id(t: u8) -> bool {
        t == 0xC2
    }
    fn decode(_: &LongHeader, body: &[u8]) -> Result<Self> {
        let mut r = BitReader::new(body, "partial_content_announcement_section");
        let transport_stream_id = r.u16()?;
        let original_network_id = r.u16()?;
        let content_id = r.u32()?;
        let n = r.u8()? as usize;
        let mut content_versions = Vec::with_capacity(n);
        for _ in 0..n {
            let content_version = r.u16()?;
            let content_minor_version = r.u16()?;
            let version_indicator = r.read_u8(2)?;
            let reserved_future_use_1 = r.read_u8(2)?;
            let content_descriptor_length = r.read_usize(12)?;
            let reserved_future_use_2 = r.read_u8(4)?;
            let schedule_description_length = r.read_usize(12)?;
            if schedule_description_length > content_descriptor_length {
                return Err(Error::invalid(
                    "partial_content_announcement_section",
                    "schedule_description_length exceeds content_descriptor_length",
                ));
            }
            if schedule_description_length / 8 * 8 != schedule_description_length {
                return Err(Error::invalid(
                    "partial_content_announcement_section",
                    "schedule_description_length is not a multiple of 8",
                ));
            }
            let mut schedules = Vec::new();
            let sched = r.bytes(schedule_description_length)?;
            let mut sr = BitReader::new(sched, "PCAT schedule loop");
            while sr.remaining_bytes() > 0 {
                schedules.push(PcatSchedule {
                    start_time: MjdTime::from_u40(sr.read(40)?),
                    duration: BcdDuration(sr.read_u32(24)?),
                });
            }
            let descriptors =
                decode_loop(r.bytes(content_descriptor_length - schedule_description_length)?)?;
            content_versions.push(PcatContentVersion {
                content_version,
                content_minor_version,
                version_indicator,
                reserved_future_use_1,
                reserved_future_use_2,
                schedules,
                descriptors,
            });
        }
        r.finish()?;
        Ok(Self {
            transport_stream_id,
            original_network_id,
            content_id,
            content_versions,
        })
    }
    fn encode(&self, out: &mut Vec<u8>) -> Result<()> {
        let mut w = BitWriter::new();
        w.write(
            "transport_stream_id",
            16,
            u64::from(self.transport_stream_id),
        )?;
        w.write(
            "original_network_id",
            16,
            u64::from(self.original_network_id),
        )?;
        w.write("content_id", 32, u64::from(self.content_id))?;
        w.write(
            "num_of_content_version",
            8,
            self.content_versions.len() as u64,
        )?;
        for c in &self.content_versions {
            let d = loop_bytes(&c.descriptors)?;
            let sched_len = c.schedules.len() * 8;
            w.write("content_version", 16, u64::from(c.content_version))?;
            w.write(
                "content_minor_version",
                16,
                u64::from(c.content_minor_version),
            )?;
            w.write("version_indicator", 2, u64::from(c.version_indicator))?;
            w.write("reserved_future_use", 2, u64::from(c.reserved_future_use_1))?;
            w.write(
                "content_descriptor_length",
                12,
                (sched_len + d.len()) as u64,
            )?;
            w.write("reserved_future_use", 4, u64::from(c.reserved_future_use_2))?;
            w.write("schedule_description_length", 12, sched_len as u64)?;
            for s in &c.schedules {
                w.write("start_time", 40, s.start_time.checked_u40()?)?;
                w.write("duration", 24, u64::from(s.duration.0))?;
            }
            w.bytes(&d);
        }
        out.extend_from_slice(&w.into_bytes());
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// BIT
// ---------------------------------------------------------------------------

/// Broadcaster entry of the BIT (ARIB STD-B10 Part 2 Table 5-13).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BitBroadcaster {
    /// broadcaster_id (8).
    pub broadcaster_id: u8,
    /// reserved_future_use (4), spec value '1111'.
    pub reserved_future_use: u8,
    /// Broadcaster descriptors.
    pub descriptors: Vec<Descriptor>,
}

/// broadcaster_information_section body, table_id 0xC4;
/// `table_id_extension` is original_network_id.  ARIB STD-B10 Part 2
/// §5.2.13, Table 5-13.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BitBody {
    /// reserved_future_use (3), spec value '111'.
    pub reserved_future_use: u8,
    /// broadcast_view_propriety (1).
    pub broadcast_view_propriety: bool,
    /// First descriptor loop.
    pub first_descriptors: Vec<Descriptor>,
    /// Broadcaster loop.
    pub broadcasters: Vec<BitBroadcaster>,
}

impl LongBody for BitBody {
    fn accepts_table_id(t: u8) -> bool {
        t == 0xC4
    }
    fn decode(_: &LongHeader, body: &[u8]) -> Result<Self> {
        let mut r = BitReader::new(body, "broadcaster_information_section");
        let reserved_future_use = r.read_u8(3)?;
        let broadcast_view_propriety = r.bool()?;
        let n = r.read_usize(12)?;
        let first_descriptors = decode_loop(r.bytes(n)?)?;
        let mut broadcasters = Vec::new();
        while r.remaining_bytes() > 0 {
            let broadcaster_id = r.u8()?;
            let (reserved_future_use, descriptors) = rfu4_loop(&mut r)?;
            broadcasters.push(BitBroadcaster {
                broadcaster_id,
                reserved_future_use,
                descriptors,
            });
        }
        Ok(Self {
            reserved_future_use,
            broadcast_view_propriety,
            first_descriptors,
            broadcasters,
        })
    }
    fn encode(&self, out: &mut Vec<u8>) -> Result<()> {
        let mut w = BitWriter::new();
        w.write(
            "reserved_future_use",
            3,
            u64::from(self.reserved_future_use),
        )?;
        w.bool(self.broadcast_view_propriety);
        let d = loop_bytes(&self.first_descriptors)?;
        w.write("first_descriptors_length", 12, d.len() as u64)?;
        w.bytes(&d);
        for b in &self.broadcasters {
            w.write("broadcaster_id", 8, u64::from(b.broadcaster_id))?;
            put_rfu4_loop(
                &mut w,
                b.reserved_future_use,
                "broadcaster_descriptors_length",
                &b.descriptors,
            )?;
        }
        out.extend_from_slice(&w.into_bytes());
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// NBIT
// ---------------------------------------------------------------------------

/// Information entry of the NBIT (ARIB STD-B10 Part 2 Table 5-14).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NbitInformation {
    /// information_id (16).
    pub information_id: u16,
    /// information_type (4), Table 5-15.
    pub information_type: u8,
    /// description_body_location (2), Table 5-16.
    pub description_body_location: u8,
    /// reserved_future_use (2), spec value '11'.
    pub reserved_future_use_1: u8,
    /// user_defined (8).
    pub user_defined: u8,
    /// key_id loop (number_of_keys, 8 bits).
    pub key_ids: Vec<u16>,
    /// reserved_future_use (4), spec value '1111'.
    pub reserved_future_use_2: u8,
    /// Descriptors.
    pub descriptors: Vec<Descriptor>,
}

/// network_board_information_section body, table_id 0xC5 (board
/// information body) / 0xC6 (reference information);
/// `table_id_extension` is original_network_id.  ARIB STD-B10 Part 2
/// §5.2.14, Table 5-14.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NbitBody {
    /// Information loop.
    pub informations: Vec<NbitInformation>,
}

impl LongBody for NbitBody {
    fn accepts_table_id(t: u8) -> bool {
        t == 0xC5 || t == 0xC6
    }
    fn decode(_: &LongHeader, body: &[u8]) -> Result<Self> {
        let mut r = BitReader::new(body, "network_board_information_section");
        let mut informations = Vec::new();
        while r.remaining_bytes() > 0 {
            let information_id = r.u16()?;
            let information_type = r.read_u8(4)?;
            let description_body_location = r.read_u8(2)?;
            let reserved_future_use_1 = r.read_u8(2)?;
            let user_defined = r.u8()?;
            let n = r.u8()? as usize;
            let mut key_ids = Vec::with_capacity(n);
            for _ in 0..n {
                key_ids.push(r.u16()?);
            }
            let (reserved_future_use_2, descriptors) = rfu4_loop(&mut r)?;
            informations.push(NbitInformation {
                information_id,
                information_type,
                description_body_location,
                reserved_future_use_1,
                user_defined,
                key_ids,
                reserved_future_use_2,
                descriptors,
            });
        }
        Ok(Self { informations })
    }
    fn encode(&self, out: &mut Vec<u8>) -> Result<()> {
        let mut w = BitWriter::new();
        for i in &self.informations {
            w.write("information_id", 16, u64::from(i.information_id))?;
            w.write("information_type", 4, u64::from(i.information_type))?;
            w.write(
                "description_body_location",
                2,
                u64::from(i.description_body_location),
            )?;
            w.write("reserved_future_use", 2, u64::from(i.reserved_future_use_1))?;
            w.write("user_defined", 8, u64::from(i.user_defined))?;
            w.write("number_of_keys", 8, i.key_ids.len() as u64)?;
            for k in &i.key_ids {
                w.write("key_id", 16, u64::from(*k))?;
            }
            put_rfu4_loop(
                &mut w,
                i.reserved_future_use_2,
                "descriptors_loop_length",
                &i.descriptors,
            )?;
        }
        out.extend_from_slice(&w.into_bytes());
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// LDT
// ---------------------------------------------------------------------------

/// Description entry of the LDT (ARIB STD-B10 Part 2 Table 5-17).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LdtDescription {
    /// description_id (16).
    pub description_id: u16,
    /// reserved_future_use (12), spec value all '1'.
    pub reserved_future_use: u16,
    /// Descriptors.
    pub descriptors: Vec<Descriptor>,
}

/// linked_description_section body, table_id 0xC7; `table_id_extension`
/// is original_service_id.  ARIB STD-B10 Part 2 §5.2.15, Table 5-17.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LdtBody {
    /// transport_stream_id (16).
    pub transport_stream_id: u16,
    /// original_network_id (16).
    pub original_network_id: u16,
    /// Description loop.
    pub descriptions: Vec<LdtDescription>,
}

impl LongBody for LdtBody {
    fn accepts_table_id(t: u8) -> bool {
        t == 0xC7
    }
    fn decode(_: &LongHeader, body: &[u8]) -> Result<Self> {
        let mut r = BitReader::new(body, "linked_description_section");
        let transport_stream_id = r.u16()?;
        let original_network_id = r.u16()?;
        let mut descriptions = Vec::new();
        while r.remaining_bytes() > 0 {
            let description_id = r.u16()?;
            let reserved_future_use = r.read_u16(12)?;
            let n = r.read_usize(12)?;
            let descriptors = decode_loop(r.bytes(n)?)?;
            descriptions.push(LdtDescription {
                description_id,
                reserved_future_use,
                descriptors,
            });
        }
        Ok(Self {
            transport_stream_id,
            original_network_id,
            descriptions,
        })
    }
    fn encode(&self, out: &mut Vec<u8>) -> Result<()> {
        let mut w = BitWriter::new();
        w.write(
            "transport_stream_id",
            16,
            u64::from(self.transport_stream_id),
        )?;
        w.write(
            "original_network_id",
            16,
            u64::from(self.original_network_id),
        )?;
        for d in &self.descriptions {
            w.write("description_id", 16, u64::from(d.description_id))?;
            w.write("reserved_future_use", 12, u64::from(d.reserved_future_use))?;
            let b = loop_bytes(&d.descriptors)?;
            w.write("descriptors_loop_length", 12, b.len() as u64)?;
            w.bytes(&b);
        }
        out.extend_from_slice(&w.into_bytes());
        Ok(())
    }
}

/// Network Information Table section (B10 Part 2 §5.2.4).
pub type Nit = Long<NitBody>;
/// Bouquet Association Table section (B10 Part 2 §5.2.5).
pub type Bat = Long<BatBody>;
/// Service Description Table section (B10 Part 2 §5.2.6).
pub type Sdt = Long<SdtBody>;
/// Event Information Table section (B10 Part 2 §5.2.7).
pub type Eit = Long<EitBody>;
/// Time and Date Table section (B10 Part 2 §5.2.8).
pub type Tdt = Short<TdtBody>;
/// Time Offset Table section (B10 Part 2 §5.2.9).
pub type Tot = Short<TotBody>;
/// Running Status Table section (B10 Part 2 §5.2.10).
pub type Rst = Short<RstBody>;
/// Partial Content Announcement Table section (B10 Part 2 §5.2.12).
pub type Pcat = Long<PcatBody>;
/// Broadcaster Information Table section (B10 Part 2 §5.2.13).
pub type Bit = Long<BitBody>;
/// Network Board Information Table section (B10 Part 2 §5.2.14).
pub type Nbit = Long<NbitBody>;
/// Linked Description Table section (B10 Part 2 §5.2.15).
pub type Ldt = Long<LdtBody>;

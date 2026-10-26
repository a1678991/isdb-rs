//! Descriptors with tags 0xC0–0xFE (ARIB STD-B10 Part 2 §6.2.20–§6.2.54).

use super::{
    Code3, DescriptorBody, code3, finish, len8_bytes, len8_string, multiple_of, put_len8, string,
};
use isdb_text::AribString;
use isdb_ts::bits::{BitReader, BitWriter};
use isdb_ts::{Error, Result};

/// hierarchical_transmission_descriptor, tag 0xC0 — ARIB STD-B10 Part 2
/// §6.2.22, Table 6-37.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HierarchicalTransmission {
    /// reserved_future_use (7), spec value all '1'.
    pub reserved_future_use_1: u8,
    /// quality_level (1): '1' high quality, '0' low quality.
    pub quality_level: bool,
    /// reserved_future_use (3), spec value '111'.
    pub reserved_future_use_2: u8,
    /// reference_PID (13).
    pub reference_pid: u16,
}

impl DescriptorBody for HierarchicalTransmission {
    const TAG: u8 = 0xC0;
    const NAME: &'static str = "hierarchical_transmission_descriptor";
    fn decode(data: &[u8]) -> Result<Self> {
        let mut r = BitReader::new(data, Self::NAME);
        let d = Self {
            reserved_future_use_1: r.read(7)? as u8,
            quality_level: r.bool()?,
            reserved_future_use_2: r.read(3)? as u8,
            reference_pid: r.read(13)? as u16,
        };
        r.finish()?;
        Ok(d)
    }
    fn encode(&self, out: &mut Vec<u8>) -> Result<()> {
        let mut w = BitWriter::new();
        w.write(
            "reserved_future_use",
            7,
            u64::from(self.reserved_future_use_1),
        )?;
        w.bool(self.quality_level);
        w.write(
            "reserved_future_use",
            3,
            u64::from(self.reserved_future_use_2),
        )?;
        w.write("reference_PID", 13, u64::from(self.reference_pid))?;
        finish(w, out);
        Ok(())
    }
}

/// Per-component entry of the digital copy control descriptor (ARIB
/// STD-B10 Part 2 §6.2.23, Table 6-38 inner loop).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ComponentCopyControl {
    /// component_tag (8).
    pub component_tag: u8,
    /// digital_recording_control_data (2), Table 6-39.
    pub digital_recording_control_data: u8,
    /// reserved_future_use (1), spec value '1'.
    pub reserved_future_use: bool,
    /// user_defined (4).
    pub user_defined: u8,
    /// maximum_bitrate (8), present iff maximum_bitrate_flag = 1.
    pub maximum_bitrate: Option<u8>,
}

/// digital_copy_control_descriptor, tag 0xC1 — ARIB STD-B10 Part 2 §6.2.23,
/// Table 6-38.  `maximum_bitrate_flag` / `component_control_flag` are the
/// presence of the corresponding `Option`s.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DigitalCopyControl {
    /// digital_recording_control_data (2), Table 6-39.
    pub digital_recording_control_data: u8,
    /// user_defined (4).
    pub user_defined: u8,
    /// maximum_bitrate (8), present iff maximum_bitrate_flag = 1.
    pub maximum_bitrate: Option<u8>,
    /// Component control loop, present iff component_control_flag = 1.
    pub component_control: Option<Vec<ComponentCopyControl>>,
}

impl DescriptorBody for DigitalCopyControl {
    const TAG: u8 = 0xC1;
    const NAME: &'static str = "digital_copy_control_descriptor";
    fn decode(data: &[u8]) -> Result<Self> {
        let mut r = BitReader::new(data, Self::NAME);
        let digital_recording_control_data = r.read(2)? as u8;
        let maximum_bitrate_flag = r.bool()?;
        let component_control_flag = r.bool()?;
        let user_defined = r.read(4)? as u8;
        let maximum_bitrate = if maximum_bitrate_flag {
            Some(r.u8()?)
        } else {
            None
        };
        let component_control = if component_control_flag {
            let loop_bytes = len8_bytes(&mut r)?;
            let mut cr =
                BitReader::new(loop_bytes, "digital_copy_control_descriptor component loop");
            let mut v = Vec::new();
            while cr.remaining_bytes() > 0 {
                let component_tag = cr.u8()?;
                let digital_recording_control_data = cr.read(2)? as u8;
                let mbf = cr.bool()?;
                let reserved_future_use = cr.bool()?;
                let user_defined = cr.read(4)? as u8;
                let maximum_bitrate = if mbf { Some(cr.u8()?) } else { None };
                v.push(ComponentCopyControl {
                    component_tag,
                    digital_recording_control_data,
                    reserved_future_use,
                    user_defined,
                    maximum_bitrate,
                });
            }
            Some(v)
        } else {
            None
        };
        r.finish()?;
        Ok(Self {
            digital_recording_control_data,
            user_defined,
            maximum_bitrate,
            component_control,
        })
    }
    fn encode(&self, out: &mut Vec<u8>) -> Result<()> {
        let mut w = BitWriter::new();
        w.write(
            "digital_recording_control_data",
            2,
            u64::from(self.digital_recording_control_data),
        )?;
        w.bool(self.maximum_bitrate.is_some());
        w.bool(self.component_control.is_some());
        w.write("user_defined", 4, u64::from(self.user_defined))?;
        if let Some(m) = self.maximum_bitrate {
            w.write("maximum_bitrate", 8, u64::from(m))?;
        }
        if let Some(cc) = &self.component_control {
            let mut cw = BitWriter::new();
            for c in cc {
                cw.write("component_tag", 8, u64::from(c.component_tag))?;
                cw.write(
                    "digital_recording_control_data",
                    2,
                    u64::from(c.digital_recording_control_data),
                )?;
                cw.bool(c.maximum_bitrate.is_some());
                cw.bool(c.reserved_future_use);
                cw.write("user_defined", 4, u64::from(c.user_defined))?;
                if let Some(m) = c.maximum_bitrate {
                    cw.write("maximum_bitrate", 8, u64::from(m))?;
                }
            }
            put_len8(&mut w, "component_control_length", &cw.into_bytes())?;
        }
        finish(w, out);
        Ok(())
    }
}

/// audio_component_descriptor, tag 0xC4 — ARIB STD-B10 Part 2 §6.2.26,
/// Table 6-42.  `ES_multi_lingual_flag` is the presence of `language_2`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AudioComponent {
    /// reserved_future_use (4), spec value '1111'.
    pub reserved_future_use_1: u8,
    /// stream_content (4).
    pub stream_content: u8,
    /// component_type (8), Table 6-43.
    pub component_type: u8,
    /// component_tag (8).
    pub component_tag: u8,
    /// stream_type (8).
    pub stream_type: u8,
    /// simulcast_group_tag (8); 0xFF when not simulcast.
    pub simulcast_group_tag: u8,
    /// main_component_flag (1).
    pub main_component_flag: bool,
    /// quality_indicator (2), Table 6-44.
    pub quality_indicator: u8,
    /// sampling_rate (3), Table 6-45.
    pub sampling_rate: u8,
    /// reserved_future_use (1), spec value '1'.
    pub reserved_future_use_2: bool,
    /// ISO_639_language_code (24).
    pub language: Code3,
    /// ISO_639_language_code_2 (24), present iff ES_multi_lingual_flag = 1.
    pub language_2: Option<Code3>,
    /// text_char loop.
    pub text: AribString,
}

impl DescriptorBody for AudioComponent {
    const TAG: u8 = 0xC4;
    const NAME: &'static str = "audio_component_descriptor";
    fn decode(data: &[u8]) -> Result<Self> {
        let mut r = BitReader::new(data, Self::NAME);
        let reserved_future_use_1 = r.read(4)? as u8;
        let stream_content = r.read(4)? as u8;
        let component_type = r.u8()?;
        let component_tag = r.u8()?;
        let stream_type = r.u8()?;
        let simulcast_group_tag = r.u8()?;
        let es_multi_lingual_flag = r.bool()?;
        let main_component_flag = r.bool()?;
        let quality_indicator = r.read(2)? as u8;
        let sampling_rate = r.read(3)? as u8;
        let reserved_future_use_2 = r.bool()?;
        let language = code3(&mut r)?;
        let language_2 = if es_multi_lingual_flag {
            Some(code3(&mut r)?)
        } else {
            None
        };
        let n = r.remaining_bytes();
        let text = string(&mut r, n)?;
        Ok(Self {
            reserved_future_use_1,
            stream_content,
            component_type,
            component_tag,
            stream_type,
            simulcast_group_tag,
            main_component_flag,
            quality_indicator,
            sampling_rate,
            reserved_future_use_2,
            language,
            language_2,
            text,
        })
    }
    fn encode(&self, out: &mut Vec<u8>) -> Result<()> {
        let mut w = BitWriter::new();
        w.write(
            "reserved_future_use",
            4,
            u64::from(self.reserved_future_use_1),
        )?;
        w.write("stream_content", 4, u64::from(self.stream_content))?;
        w.write("component_type", 8, u64::from(self.component_type))?;
        w.write("component_tag", 8, u64::from(self.component_tag))?;
        w.write("stream_type", 8, u64::from(self.stream_type))?;
        w.write(
            "simulcast_group_tag",
            8,
            u64::from(self.simulcast_group_tag),
        )?;
        w.bool(self.language_2.is_some());
        w.bool(self.main_component_flag);
        w.write("quality_indicator", 2, u64::from(self.quality_indicator))?;
        w.write("sampling_rate", 3, u64::from(self.sampling_rate))?;
        w.bool(self.reserved_future_use_2);
        w.bytes(&self.language);
        if let Some(l2) = &self.language_2 {
            w.bytes(l2);
        }
        w.bytes(&self.text.0);
        finish(w, out);
        Ok(())
    }
}

/// hyperlink_descriptor, tag 0xC5 — ARIB STD-B10 Part 2 §6.2.29, Table 6-49.
/// The selector area (Tables 6-52 to 6-58) is kept raw.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Hyperlink {
    /// hyper_linkage_type (8), Table 6-50.
    pub hyper_linkage_type: u8,
    /// link_destination_type (8), Table 6-51.
    pub link_destination_type: u8,
    /// selector_byte loop.
    pub selector: Vec<u8>,
    /// private_data loop.
    pub private_data: Vec<u8>,
}

impl DescriptorBody for Hyperlink {
    const TAG: u8 = 0xC5;
    const NAME: &'static str = "hyperlink_descriptor";
    fn decode(data: &[u8]) -> Result<Self> {
        let mut r = BitReader::new(data, Self::NAME);
        let hyper_linkage_type = r.u8()?;
        let link_destination_type = r.u8()?;
        let selector = len8_bytes(&mut r)?.to_vec();
        let private_data = r.rest()?.to_vec();
        Ok(Self {
            hyper_linkage_type,
            link_destination_type,
            selector,
            private_data,
        })
    }
    fn encode(&self, out: &mut Vec<u8>) -> Result<()> {
        let mut w = BitWriter::new();
        w.write("hyper_linkage_type", 8, u64::from(self.hyper_linkage_type))?;
        w.write(
            "link_destination_type",
            8,
            u64::from(self.link_destination_type),
        )?;
        put_len8(&mut w, "selector_length", &self.selector)?;
        w.bytes(&self.private_data);
        finish(w, out);
        Ok(())
    }
}

/// data_content_descriptor, tag 0xC7 — ARIB STD-B10 Part 2 §6.2.28,
/// Table 6-48.  The selector area (Annex J) is kept raw.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DataContent {
    /// data_component_id (16).
    pub data_component_id: u16,
    /// entry_component (8).
    pub entry_component: u8,
    /// selector_byte loop.
    pub selector: Vec<u8>,
    /// component_ref loop (its length is num_of_component_ref).
    pub component_refs: Vec<u8>,
    /// ISO_639_language_code (24).
    pub language: Code3,
    /// text_char loop.
    pub text: AribString,
}

impl DescriptorBody for DataContent {
    const TAG: u8 = 0xC7;
    const NAME: &'static str = "data_content_descriptor";
    fn decode(data: &[u8]) -> Result<Self> {
        let mut r = BitReader::new(data, Self::NAME);
        let data_component_id = r.u16()?;
        let entry_component = r.u8()?;
        let selector = len8_bytes(&mut r)?.to_vec();
        let component_refs = len8_bytes(&mut r)?.to_vec();
        let language = code3(&mut r)?;
        let text = len8_string(&mut r)?;
        r.finish()?;
        Ok(Self {
            data_component_id,
            entry_component,
            selector,
            component_refs,
            language,
            text,
        })
    }
    fn encode(&self, out: &mut Vec<u8>) -> Result<()> {
        let mut w = BitWriter::new();
        w.write("data_component_id", 16, u64::from(self.data_component_id))?;
        w.write("entry_component", 8, u64::from(self.entry_component))?;
        put_len8(&mut w, "selector_length", &self.selector)?;
        put_len8(&mut w, "num_of_component_ref", &self.component_refs)?;
        w.bytes(&self.language);
        put_len8(&mut w, "text_length", &self.text.0)?;
        finish(w, out);
        Ok(())
    }
}

/// video_decode_control_descriptor, tag 0xC8 — ARIB STD-B10 Part 2
/// §6.2.30, Table 6-59 (v5.13: last 2 bits are transfer_characteristics,
/// Table 6-60-1; earlier editions had reserved_future_use).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoDecodeControl {
    /// still_picture_flag (1).
    pub still_picture_flag: bool,
    /// sequence_end_code_flag (1).
    pub sequence_end_code_flag: bool,
    /// video_encode_format (4), Table 6-60.
    pub video_encode_format: u8,
    /// transfer_characteristics (2), Table 6-60-1.
    pub transfer_characteristics: u8,
}

impl DescriptorBody for VideoDecodeControl {
    const TAG: u8 = 0xC8;
    const NAME: &'static str = "video_decode_control_descriptor";
    fn decode(data: &[u8]) -> Result<Self> {
        let mut r = BitReader::new(data, Self::NAME);
        let d = Self {
            still_picture_flag: r.bool()?,
            sequence_end_code_flag: r.bool()?,
            video_encode_format: r.read(4)? as u8,
            transfer_characteristics: r.read(2)? as u8,
        };
        r.finish()?;
        Ok(d)
    }
    fn encode(&self, out: &mut Vec<u8>) -> Result<()> {
        let mut w = BitWriter::new();
        w.bool(self.still_picture_flag);
        w.bool(self.sequence_end_code_flag);
        w.write(
            "video_encode_format",
            4,
            u64::from(self.video_encode_format),
        )?;
        w.write(
            "transfer_characteristics",
            2,
            u64::from(self.transfer_characteristics),
        )?;
        finish(w, out);
        Ok(())
    }
}

/// One transmission type of the TS information descriptor (ARIB STD-B10
/// Part 2 §6.2.42).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TransmissionType {
    /// transmission_type_info (8).
    pub transmission_type_info: u8,
    /// service_id loop (its length is num_of_service).
    pub service_ids: Vec<u16>,
}

/// TS_information_descriptor, tag 0xCD — ARIB STD-B10 Part 2 §6.2.42,
/// Table 6-82.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TsInformation {
    /// remote_control_key_id (8).
    pub remote_control_key_id: u8,
    /// ts_name_char loop (length_of_ts_name, 6 bits).
    pub ts_name: AribString,
    /// Transmission type loop (transmission_type_count, 2 bits).
    pub transmission_types: Vec<TransmissionType>,
    /// Trailing reserved_future_use bytes (spec value 0xFF).
    pub reserved_future_use: Vec<u8>,
}

impl DescriptorBody for TsInformation {
    const TAG: u8 = 0xCD;
    const NAME: &'static str = "TS_information_descriptor";
    fn decode(data: &[u8]) -> Result<Self> {
        let mut r = BitReader::new(data, Self::NAME);
        let remote_control_key_id = r.u8()?;
        let length_of_ts_name = r.read(6)? as usize;
        let transmission_type_count = r.read(2)? as usize;
        let ts_name = string(&mut r, length_of_ts_name)?;
        let mut transmission_types = Vec::with_capacity(transmission_type_count);
        for _ in 0..transmission_type_count {
            let transmission_type_info = r.u8()?;
            let n = r.u8()? as usize;
            let mut service_ids = Vec::with_capacity(n);
            for _ in 0..n {
                service_ids.push(r.u16()?);
            }
            transmission_types.push(TransmissionType {
                transmission_type_info,
                service_ids,
            });
        }
        let reserved_future_use = r.rest()?.to_vec();
        Ok(Self {
            remote_control_key_id,
            ts_name,
            transmission_types,
            reserved_future_use,
        })
    }
    fn encode(&self, out: &mut Vec<u8>) -> Result<()> {
        let mut w = BitWriter::new();
        w.write(
            "remote_control_key_id",
            8,
            u64::from(self.remote_control_key_id),
        )?;
        w.write("length_of_ts_name", 6, self.ts_name.0.len() as u64)?;
        w.write(
            "transmission_type_count",
            2,
            self.transmission_types.len() as u64,
        )?;
        w.bytes(&self.ts_name.0);
        for t in &self.transmission_types {
            w.write(
                "transmission_type_info",
                8,
                u64::from(t.transmission_type_info),
            )?;
            w.write("num_of_service", 8, t.service_ids.len() as u64)?;
            for s in &t.service_ids {
                w.write("service_id", 16, u64::from(*s))?;
            }
        }
        w.bytes(&self.reserved_future_use);
        finish(w, out);
        Ok(())
    }
}

/// Broadcaster entry (original_network_id, broadcaster_id) of the extended
/// broadcaster descriptor (ARIB STD-B10 Part 2 §6.2.43).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NetworkBroadcaster {
    /// original_network_id (16).
    pub original_network_id: u16,
    /// broadcaster_id (8).
    pub broadcaster_id: u8,
}

/// Body of an extended broadcaster descriptor for broadcaster_type 0x1
/// (terrestrial TV) or 0x2 (terrestrial sound), ARIB STD-B10 Part 2
/// §6.2.43, Table 6-83.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TerrestrialBroadcaster {
    /// terrestrial_broadcaster_id / terrestrial_sound_broadcaster_id (16).
    pub terrestrial_broadcaster_id: u16,
    /// affiliation_id / sound_broadcast_affiliation_id loop (count: 4 bits).
    pub affiliation_ids: Vec<u8>,
    /// Broadcaster loop (count: number_of_broadcaster_id_loop, 4 bits).
    pub broadcasters: Vec<NetworkBroadcaster>,
    /// private_data_byte loop.
    pub private_data: Vec<u8>,
}

impl TerrestrialBroadcaster {
    fn decode(r: &mut BitReader<'_>) -> Result<Self> {
        let terrestrial_broadcaster_id = r.u16()?;
        let n_aff = r.read(4)? as usize;
        let n_bc = r.read(4)? as usize;
        let affiliation_ids = r.bytes(n_aff)?.to_vec();
        let mut broadcasters = Vec::with_capacity(n_bc);
        for _ in 0..n_bc {
            broadcasters.push(NetworkBroadcaster {
                original_network_id: r.u16()?,
                broadcaster_id: r.u8()?,
            });
        }
        Ok(Self {
            terrestrial_broadcaster_id,
            affiliation_ids,
            broadcasters,
            private_data: r.rest()?.to_vec(),
        })
    }
    fn encode(&self, w: &mut BitWriter) -> Result<()> {
        w.write(
            "terrestrial_broadcaster_id",
            16,
            u64::from(self.terrestrial_broadcaster_id),
        )?;
        w.write(
            "number_of_affiliation_id_loop",
            4,
            self.affiliation_ids.len() as u64,
        )?;
        w.write(
            "number_of_broadcaster_id_loop",
            4,
            self.broadcasters.len() as u64,
        )?;
        w.bytes(&self.affiliation_ids);
        for b in &self.broadcasters {
            w.write("original_network_id", 16, u64::from(b.original_network_id))?;
            w.write("broadcaster_id", 8, u64::from(b.broadcaster_id))?;
        }
        w.bytes(&self.private_data);
        Ok(())
    }
}

/// broadcaster_type-dependent part of the extended broadcaster descriptor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExtendedBroadcasterBody {
    /// broadcaster_type 0x1: digital terrestrial television broadcast.
    TerrestrialTv(TerrestrialBroadcaster),
    /// broadcaster_type 0x2: digital terrestrial sound broadcast.
    TerrestrialSound(TerrestrialBroadcaster),
    /// Any other broadcaster_type (4 bits) with its reserved_future_use bytes.
    Other {
        /// broadcaster_type (4), not 0x1 or 0x2.
        broadcaster_type: u8,
        /// reserved_future_use bytes.
        data: Vec<u8>,
    },
}

/// extended_broadcaster_descriptor, tag 0xCE — ARIB STD-B10 Part 2 §6.2.43,
/// Table 6-83.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtendedBroadcaster {
    /// reserved_future_use (4) after broadcaster_type, spec value '1111'.
    pub reserved_future_use: u8,
    /// broadcaster_type (Table 6-84) and the dependent fields.
    pub body: ExtendedBroadcasterBody,
}

impl ExtendedBroadcaster {
    /// broadcaster_type (4), Table 6-84.
    pub fn broadcaster_type(&self) -> u8 {
        match &self.body {
            ExtendedBroadcasterBody::TerrestrialTv(_) => 0x1,
            ExtendedBroadcasterBody::TerrestrialSound(_) => 0x2,
            ExtendedBroadcasterBody::Other {
                broadcaster_type, ..
            } => *broadcaster_type,
        }
    }
}

impl DescriptorBody for ExtendedBroadcaster {
    const TAG: u8 = 0xCE;
    const NAME: &'static str = "extended_broadcaster_descriptor";
    fn decode(data: &[u8]) -> Result<Self> {
        let mut r = BitReader::new(data, Self::NAME);
        let broadcaster_type = r.read(4)? as u8;
        let reserved_future_use = r.read(4)? as u8;
        let body = match broadcaster_type {
            0x1 => ExtendedBroadcasterBody::TerrestrialTv(TerrestrialBroadcaster::decode(&mut r)?),
            0x2 => {
                ExtendedBroadcasterBody::TerrestrialSound(TerrestrialBroadcaster::decode(&mut r)?)
            }
            _ => ExtendedBroadcasterBody::Other {
                broadcaster_type,
                data: r.rest()?.to_vec(),
            },
        };
        Ok(Self {
            reserved_future_use,
            body,
        })
    }
    fn encode(&self, out: &mut Vec<u8>) -> Result<()> {
        let mut w = BitWriter::new();
        let bt = self.broadcaster_type();
        if matches!(&self.body, ExtendedBroadcasterBody::Other { .. }) && (bt == 1 || bt == 2) {
            return Err(Error::invalid(
                Self::NAME,
                "Other body with broadcaster_type 0x1/0x2",
            ));
        }
        w.write("broadcaster_type", 4, u64::from(bt))?;
        w.write(
            "reserved_future_use",
            4,
            u64::from(self.reserved_future_use),
        )?;
        match &self.body {
            ExtendedBroadcasterBody::TerrestrialTv(t)
            | ExtendedBroadcasterBody::TerrestrialSound(t) => t.encode(&mut w)?,
            ExtendedBroadcasterBody::Other { data, .. } => w.bytes(data),
        }
        finish(w, out);
        Ok(())
    }
}

/// logo_transmission_type-dependent part of the logo transmission
/// descriptor (ARIB STD-B10 Part 2 §6.2.44, Tables 6-85/6-86).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LogoTransmissionBody {
    /// 0x01: CDT transmission scheme 1.
    Cdt1 {
        /// reserved_future_use (7), spec value all '1'.
        reserved_future_use_1: u8,
        /// logo_id (9).
        logo_id: u16,
        /// reserved_future_use (4), spec value '1111'.
        reserved_future_use_2: u8,
        /// logo_version (12).
        logo_version: u16,
        /// download_data_id (16).
        download_data_id: u16,
    },
    /// 0x02: CDT transmission scheme 2.
    Cdt2 {
        /// reserved_future_use (7), spec value all '1'.
        reserved_future_use: u8,
        /// logo_id (9).
        logo_id: u16,
    },
    /// 0x03: simple logo system (logo_char loop).
    Simple(AribString),
    /// Other logo_transmission_type with its reserved_future_use bytes.
    Other {
        /// logo_transmission_type (8), not 0x01–0x03.
        logo_transmission_type: u8,
        /// reserved_future_use bytes.
        data: Vec<u8>,
    },
}

/// logo_transmission_descriptor, tag 0xCF — ARIB STD-B10 Part 2 §6.2.44,
/// Table 6-85.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogoTransmission {
    /// logo_transmission_type and dependent fields.
    pub body: LogoTransmissionBody,
}

impl LogoTransmission {
    /// logo_transmission_type (8), Table 6-86.
    pub fn logo_transmission_type(&self) -> u8 {
        match &self.body {
            LogoTransmissionBody::Cdt1 { .. } => 0x01,
            LogoTransmissionBody::Cdt2 { .. } => 0x02,
            LogoTransmissionBody::Simple(_) => 0x03,
            LogoTransmissionBody::Other {
                logo_transmission_type,
                ..
            } => *logo_transmission_type,
        }
    }
}

impl DescriptorBody for LogoTransmission {
    const TAG: u8 = 0xCF;
    const NAME: &'static str = "logo_transmission_descriptor";
    fn decode(data: &[u8]) -> Result<Self> {
        let mut r = BitReader::new(data, Self::NAME);
        let t = r.u8()?;
        let body = match t {
            0x01 => {
                let b = LogoTransmissionBody::Cdt1 {
                    reserved_future_use_1: r.read(7)? as u8,
                    logo_id: r.read(9)? as u16,
                    reserved_future_use_2: r.read(4)? as u8,
                    logo_version: r.read(12)? as u16,
                    download_data_id: r.u16()?,
                };
                r.finish()?;
                b
            }
            0x02 => {
                let b = LogoTransmissionBody::Cdt2 {
                    reserved_future_use: r.read(7)? as u8,
                    logo_id: r.read(9)? as u16,
                };
                r.finish()?;
                b
            }
            0x03 => LogoTransmissionBody::Simple(AribString(r.rest()?.to_vec())),
            _ => LogoTransmissionBody::Other {
                logo_transmission_type: t,
                data: r.rest()?.to_vec(),
            },
        };
        Ok(Self { body })
    }
    fn encode(&self, out: &mut Vec<u8>) -> Result<()> {
        let mut w = BitWriter::new();
        let t = self.logo_transmission_type();
        if matches!(&self.body, LogoTransmissionBody::Other { .. }) && (1..=3).contains(&t) {
            return Err(Error::invalid(
                Self::NAME,
                "Other body with logo_transmission_type 0x01–0x03",
            ));
        }
        w.write("logo_transmission_type", 8, u64::from(t))?;
        match &self.body {
            LogoTransmissionBody::Cdt1 {
                reserved_future_use_1,
                logo_id,
                reserved_future_use_2,
                logo_version,
                download_data_id,
            } => {
                w.write("reserved_future_use", 7, u64::from(*reserved_future_use_1))?;
                w.write("logo_id", 9, u64::from(*logo_id))?;
                w.write("reserved_future_use", 4, u64::from(*reserved_future_use_2))?;
                w.write("logo_version", 12, u64::from(*logo_version))?;
                w.write("download_data_id", 16, u64::from(*download_data_id))?;
            }
            LogoTransmissionBody::Cdt2 {
                reserved_future_use,
                logo_id,
            } => {
                w.write("reserved_future_use", 7, u64::from(*reserved_future_use))?;
                w.write("logo_id", 9, u64::from(*logo_id))?;
            }
            LogoTransmissionBody::Simple(s) => w.bytes(&s.0),
            LogoTransmissionBody::Other { data, .. } => w.bytes(data),
        }
        finish(w, out);
        Ok(())
    }
}

/// series_descriptor, tag 0xD5 — ARIB STD-B10 Part 2 §6.2.33, Table 6-65.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Series {
    /// series_id (16).
    pub series_id: u16,
    /// repeat_label (4).
    pub repeat_label: u8,
    /// program_pattern (3), Table 6-66.
    pub program_pattern: u8,
    /// expire_date_valid_flag (1).
    pub expire_date_valid_flag: bool,
    /// expire_date (16): lower 16 bits of MJD.
    pub expire_date: u16,
    /// episode_number (12).
    pub episode_number: u16,
    /// last_episode_number (12).
    pub last_episode_number: u16,
    /// series_name_char loop.
    pub series_name: AribString,
}

impl DescriptorBody for Series {
    const TAG: u8 = 0xD5;
    const NAME: &'static str = "series_descriptor";
    fn decode(data: &[u8]) -> Result<Self> {
        let mut r = BitReader::new(data, Self::NAME);
        let series_id = r.u16()?;
        let repeat_label = r.read(4)? as u8;
        let program_pattern = r.read(3)? as u8;
        let expire_date_valid_flag = r.bool()?;
        let expire_date = r.u16()?;
        let episode_number = r.read(12)? as u16;
        let last_episode_number = r.read(12)? as u16;
        let series_name = AribString(r.rest()?.to_vec());
        Ok(Self {
            series_id,
            repeat_label,
            program_pattern,
            expire_date_valid_flag,
            expire_date,
            episode_number,
            last_episode_number,
            series_name,
        })
    }
    fn encode(&self, out: &mut Vec<u8>) -> Result<()> {
        let mut w = BitWriter::new();
        w.write("series_id", 16, u64::from(self.series_id))?;
        w.write("repeat_label", 4, u64::from(self.repeat_label))?;
        w.write("program_pattern", 3, u64::from(self.program_pattern))?;
        w.bool(self.expire_date_valid_flag);
        w.write("expire_date", 16, u64::from(self.expire_date))?;
        w.write("episode_number", 12, u64::from(self.episode_number))?;
        w.write(
            "last_episode_number",
            12,
            u64::from(self.last_episode_number),
        )?;
        w.bytes(&self.series_name.0);
        finish(w, out);
        Ok(())
    }
}

/// (service_id, event_id) pair of the event group descriptor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EventRef {
    /// service_id (16).
    pub service_id: u16,
    /// event_id (16).
    pub event_id: u16,
}

/// Other-network event of the event group descriptor (group_type 4/5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NetworkEventRef {
    /// original_network_id (16).
    pub original_network_id: u16,
    /// transport_stream_id (16).
    pub transport_stream_id: u16,
    /// service_id (16).
    pub service_id: u16,
    /// event_id (16).
    pub event_id: u16,
}

/// event_group_descriptor, tag 0xD6 — ARIB STD-B10 Part 2 §6.2.34,
/// Table 6-67.  For group_type 4/5 the tail is `other_network_events`,
/// otherwise `private_data` (the other one must be empty).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EventGroup {
    /// group_type (4), Table 6-68.
    pub group_type: u8,
    /// Event loop (event_count, 4 bits).
    pub events: Vec<EventRef>,
    /// Other-network event loop (group_type 4 or 5).
    pub other_network_events: Vec<NetworkEventRef>,
    /// private_data_byte loop (other group types).
    pub private_data: Vec<u8>,
}

impl DescriptorBody for EventGroup {
    const TAG: u8 = 0xD6;
    const NAME: &'static str = "event_group_descriptor";
    fn decode(data: &[u8]) -> Result<Self> {
        let mut r = BitReader::new(data, Self::NAME);
        let group_type = r.read(4)? as u8;
        let event_count = r.read(4)? as usize;
        let mut events = Vec::with_capacity(event_count);
        for _ in 0..event_count {
            events.push(EventRef {
                service_id: r.u16()?,
                event_id: r.u16()?,
            });
        }
        let mut other_network_events = Vec::new();
        let mut private_data = Vec::new();
        if group_type == 4 || group_type == 5 {
            let n = multiple_of(Self::NAME, r.remaining_bytes(), 8)?;
            for _ in 0..n {
                other_network_events.push(NetworkEventRef {
                    original_network_id: r.u16()?,
                    transport_stream_id: r.u16()?,
                    service_id: r.u16()?,
                    event_id: r.u16()?,
                });
            }
        } else {
            private_data = r.rest()?.to_vec();
        }
        Ok(Self {
            group_type,
            events,
            other_network_events,
            private_data,
        })
    }
    fn encode(&self, out: &mut Vec<u8>) -> Result<()> {
        let other = self.group_type == 4 || self.group_type == 5;
        if (other && !self.private_data.is_empty())
            || (!other && !self.other_network_events.is_empty())
        {
            return Err(Error::invalid(Self::NAME, "tail does not match group_type"));
        }
        let mut w = BitWriter::new();
        w.write("group_type", 4, u64::from(self.group_type))?;
        w.write("event_count", 4, self.events.len() as u64)?;
        for e in &self.events {
            w.write("service_id", 16, u64::from(e.service_id))?;
            w.write("event_id", 16, u64::from(e.event_id))?;
        }
        for e in &self.other_network_events {
            w.write("original_network_id", 16, u64::from(e.original_network_id))?;
            w.write("transport_stream_id", 16, u64::from(e.transport_stream_id))?;
            w.write("service_id", 16, u64::from(e.service_id))?;
            w.write("event_id", 16, u64::from(e.event_id))?;
        }
        w.bytes(&self.private_data);
        finish(w, out);
        Ok(())
    }
}

/// One table entry of the SI parameter / SI prime_ts descriptors.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TableDescription {
    /// table_id (8).
    pub table_id: u8,
    /// table_description_byte loop.
    pub description: Vec<u8>,
}

fn decode_table_descriptions(r: &mut BitReader<'_>) -> Result<Vec<TableDescription>> {
    let mut v = Vec::new();
    while r.remaining_bytes() > 0 {
        let table_id = r.u8()?;
        let description = len8_bytes(r)?.to_vec();
        v.push(TableDescription {
            table_id,
            description,
        });
    }
    Ok(v)
}

fn encode_table_descriptions(w: &mut BitWriter, v: &[TableDescription]) -> Result<()> {
    for t in v {
        w.write("table_id", 8, u64::from(t.table_id))?;
        put_len8(w, "table_description_length", &t.description)?;
    }
    Ok(())
}

/// SI_parameter_descriptor, tag 0xD7 — ARIB STD-B10 Part 2 §6.2.35,
/// Table 6-69.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SiParameter {
    /// parameter_version (8).
    pub parameter_version: u8,
    /// update_time (16): lower 16 bits of MJD.
    pub update_time: u16,
    /// Table loop.
    pub tables: Vec<TableDescription>,
}

impl DescriptorBody for SiParameter {
    const TAG: u8 = 0xD7;
    const NAME: &'static str = "SI_parameter_descriptor";
    fn decode(data: &[u8]) -> Result<Self> {
        let mut r = BitReader::new(data, Self::NAME);
        let parameter_version = r.u8()?;
        let update_time = r.u16()?;
        let tables = decode_table_descriptions(&mut r)?;
        Ok(Self {
            parameter_version,
            update_time,
            tables,
        })
    }
    fn encode(&self, out: &mut Vec<u8>) -> Result<()> {
        let mut w = BitWriter::new();
        w.write("parameter_version", 8, u64::from(self.parameter_version))?;
        w.write("update_time", 16, u64::from(self.update_time))?;
        encode_table_descriptions(&mut w, &self.tables)?;
        finish(w, out);
        Ok(())
    }
}

/// broadcaster_name_descriptor, tag 0xD8 — ARIB STD-B10 Part 2 §6.2.36,
/// Table 6-70.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BroadcasterName {
    /// The `char` loop.
    pub name: AribString,
}

impl DescriptorBody for BroadcasterName {
    const TAG: u8 = 0xD8;
    const NAME: &'static str = "broadcaster_name_descriptor";
    fn decode(data: &[u8]) -> Result<Self> {
        Ok(Self {
            name: AribString(data.to_vec()),
        })
    }
    fn encode(&self, out: &mut Vec<u8>) -> Result<()> {
        out.extend_from_slice(&self.name.0);
        Ok(())
    }
}

/// CA unit of a component group (ARIB STD-B10 Part 2 §6.2.37).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CaUnit {
    /// CA_unit_id (4), Table 6-74.
    pub ca_unit_id: u8,
    /// component_tag loop (num_of_component, 4 bits).
    pub component_tags: Vec<u8>,
}

/// One group of the component group descriptor (ARIB STD-B10 Part 2 §6.2.37).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ComponentGroupEntry {
    /// component_group_id (4), Table 6-73.
    pub component_group_id: u8,
    /// CA unit loop (num_of_CA_unit, 4 bits).
    pub ca_units: Vec<CaUnit>,
    /// total_bit_rate (8), present iff total_bit_rate_flag = 1.
    pub total_bit_rate: Option<u8>,
    /// text_char loop.
    pub text: AribString,
}

/// component_group_descriptor, tag 0xD9 — ARIB STD-B10 Part 2 §6.2.37,
/// Table 6-71.  Every group's `total_bit_rate` presence must equal
/// `total_bit_rate_flag`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ComponentGroup {
    /// component_group_type (3), Table 6-72.
    pub component_group_type: u8,
    /// total_bit_rate_flag (1).
    pub total_bit_rate_flag: bool,
    /// Group loop (num_of_group, 4 bits).
    pub groups: Vec<ComponentGroupEntry>,
}

impl DescriptorBody for ComponentGroup {
    const TAG: u8 = 0xD9;
    const NAME: &'static str = "component_group_descriptor";
    fn decode(data: &[u8]) -> Result<Self> {
        let mut r = BitReader::new(data, Self::NAME);
        let component_group_type = r.read(3)? as u8;
        let total_bit_rate_flag = r.bool()?;
        let num_of_group = r.read(4)? as usize;
        let mut groups = Vec::with_capacity(num_of_group);
        for _ in 0..num_of_group {
            let component_group_id = r.read(4)? as u8;
            let num_of_ca_unit = r.read(4)? as usize;
            let mut ca_units = Vec::with_capacity(num_of_ca_unit);
            for _ in 0..num_of_ca_unit {
                let ca_unit_id = r.read(4)? as u8;
                let n = r.read(4)? as usize;
                ca_units.push(CaUnit {
                    ca_unit_id,
                    component_tags: r.bytes(n)?.to_vec(),
                });
            }
            let total_bit_rate = if total_bit_rate_flag {
                Some(r.u8()?)
            } else {
                None
            };
            let text = len8_string(&mut r)?;
            groups.push(ComponentGroupEntry {
                component_group_id,
                ca_units,
                total_bit_rate,
                text,
            });
        }
        r.finish()?;
        Ok(Self {
            component_group_type,
            total_bit_rate_flag,
            groups,
        })
    }
    fn encode(&self, out: &mut Vec<u8>) -> Result<()> {
        let mut w = BitWriter::new();
        w.write(
            "component_group_type",
            3,
            u64::from(self.component_group_type),
        )?;
        w.bool(self.total_bit_rate_flag);
        w.write("num_of_group", 4, self.groups.len() as u64)?;
        for g in &self.groups {
            if g.total_bit_rate.is_some() != self.total_bit_rate_flag {
                return Err(Error::invalid(
                    Self::NAME,
                    "total_bit_rate presence differs from total_bit_rate_flag",
                ));
            }
            w.write("component_group_id", 4, u64::from(g.component_group_id))?;
            w.write("num_of_CA_unit", 4, g.ca_units.len() as u64)?;
            for u in &g.ca_units {
                w.write("CA_unit_id", 4, u64::from(u.ca_unit_id))?;
                w.write("num_of_component", 4, u.component_tags.len() as u64)?;
                w.bytes(&u.component_tags);
            }
            if let Some(t) = g.total_bit_rate {
                w.write("total_bit_rate", 8, u64::from(t))?;
            }
            put_len8(&mut w, "text_length", &g.text.0)?;
        }
        finish(w, out);
        Ok(())
    }
}

/// SI_prime_ts_descriptor, tag 0xDA — ARIB STD-B10 Part 2 §6.2.38,
/// Table 6-75.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SiPrimeTs {
    /// parameter_version (8).
    pub parameter_version: u8,
    /// update_time (16): lower 16 bits of MJD.
    pub update_time: u16,
    /// SI_prime_ts_network_id (16).
    pub si_prime_ts_network_id: u16,
    /// SI_prime_transport_stream_id (16).
    pub si_prime_transport_stream_id: u16,
    /// Table loop.
    pub tables: Vec<TableDescription>,
}

impl DescriptorBody for SiPrimeTs {
    const TAG: u8 = 0xDA;
    const NAME: &'static str = "SI_prime_ts_descriptor";
    fn decode(data: &[u8]) -> Result<Self> {
        let mut r = BitReader::new(data, Self::NAME);
        Ok(Self {
            parameter_version: r.u8()?,
            update_time: r.u16()?,
            si_prime_ts_network_id: r.u16()?,
            si_prime_transport_stream_id: r.u16()?,
            tables: decode_table_descriptions(&mut r)?,
        })
    }
    fn encode(&self, out: &mut Vec<u8>) -> Result<()> {
        let mut w = BitWriter::new();
        w.write("parameter_version", 8, u64::from(self.parameter_version))?;
        w.write("update_time", 16, u64::from(self.update_time))?;
        w.write(
            "SI_prime_ts_network_id",
            16,
            u64::from(self.si_prime_ts_network_id),
        )?;
        w.write(
            "SI_prime_transport_stream_id",
            16,
            u64::from(self.si_prime_transport_stream_id),
        )?;
        encode_table_descriptions(&mut w, &self.tables)?;
        finish(w, out);
        Ok(())
    }
}

/// board_information_descriptor, tag 0xDB — ARIB STD-B10 Part 2 §6.2.39,
/// Table 6-76.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BoardInformation {
    /// title_char loop.
    pub title: AribString,
    /// text_char loop.
    pub text: AribString,
}

impl DescriptorBody for BoardInformation {
    const TAG: u8 = 0xDB;
    const NAME: &'static str = "board_information_descriptor";
    fn decode(data: &[u8]) -> Result<Self> {
        let mut r = BitReader::new(data, Self::NAME);
        let title = len8_string(&mut r)?;
        let text = len8_string(&mut r)?;
        r.finish()?;
        Ok(Self { title, text })
    }
    fn encode(&self, out: &mut Vec<u8>) -> Result<()> {
        let mut w = BitWriter::new();
        put_len8(&mut w, "title_length", &self.title.0)?;
        put_len8(&mut w, "text_length", &self.text.0)?;
        finish(w, out);
        Ok(())
    }
}

/// One description entry of the LDT linkage descriptor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LdtLinkageEntry {
    /// description_id (16).
    pub description_id: u16,
    /// reserved_future_use (4), spec value '1111'.
    pub reserved_future_use: u8,
    /// description_type (4), Table 6-78.
    pub description_type: u8,
    /// user_defined (8).
    pub user_defined: u8,
}

/// LDT_linkage_descriptor, tag 0xDC — ARIB STD-B10 Part 2 §6.2.40,
/// Table 6-77.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LdtLinkage {
    /// original_service_id (16).
    pub original_service_id: u16,
    /// transport_stream_id (16).
    pub transport_stream_id: u16,
    /// original_network_id (16).
    pub original_network_id: u16,
    /// Description loop.
    pub descriptions: Vec<LdtLinkageEntry>,
}

impl DescriptorBody for LdtLinkage {
    const TAG: u8 = 0xDC;
    const NAME: &'static str = "LDT_linkage_descriptor";
    fn decode(data: &[u8]) -> Result<Self> {
        let mut r = BitReader::new(data, Self::NAME);
        let original_service_id = r.u16()?;
        let transport_stream_id = r.u16()?;
        let original_network_id = r.u16()?;
        let n = multiple_of(Self::NAME, r.remaining_bytes(), 4)?;
        let mut descriptions = Vec::with_capacity(n);
        for _ in 0..n {
            descriptions.push(LdtLinkageEntry {
                description_id: r.u16()?,
                reserved_future_use: r.read(4)? as u8,
                description_type: r.read(4)? as u8,
                user_defined: r.u8()?,
            });
        }
        Ok(Self {
            original_service_id,
            transport_stream_id,
            original_network_id,
            descriptions,
        })
    }
    fn encode(&self, out: &mut Vec<u8>) -> Result<()> {
        let mut w = BitWriter::new();
        w.write(
            "original_service_id",
            16,
            u64::from(self.original_service_id),
        )?;
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
            w.write("reserved_future_use", 4, u64::from(d.reserved_future_use))?;
            w.write("description_type", 4, u64::from(d.description_type))?;
            w.write("user_defined", 8, u64::from(d.user_defined))?;
        }
        finish(w, out);
        Ok(())
    }
}

/// connected_transmission_descriptor, tag 0xDD — ARIB STD-B10 Part 2
/// §6.2.41, Table 6-79.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ConnectedTransmission {
    /// connected_transmission_group_id (16).
    pub connected_transmission_group_id: u16,
    /// segment_type (2), Table 6-80.
    pub segment_type: u8,
    /// modulation_type_A (2), Table 6-81.
    pub modulation_type_a: u8,
    /// modulation_type_B (2), Table 6-81.
    pub modulation_type_b: u8,
    /// modulation_type_C (2), Table 6-81.
    pub modulation_type_c: u8,
    /// additional_connected_transmission_info loop.
    pub additional_info: Vec<u8>,
}

impl DescriptorBody for ConnectedTransmission {
    const TAG: u8 = 0xDD;
    const NAME: &'static str = "connected_transmission_descriptor";
    fn decode(data: &[u8]) -> Result<Self> {
        let mut r = BitReader::new(data, Self::NAME);
        Ok(Self {
            connected_transmission_group_id: r.u16()?,
            segment_type: r.read(2)? as u8,
            modulation_type_a: r.read(2)? as u8,
            modulation_type_b: r.read(2)? as u8,
            modulation_type_c: r.read(2)? as u8,
            additional_info: r.rest()?.to_vec(),
        })
    }
    fn encode(&self, out: &mut Vec<u8>) -> Result<()> {
        let mut w = BitWriter::new();
        w.write(
            "connected_transmission_group_id",
            16,
            u64::from(self.connected_transmission_group_id),
        )?;
        w.write("segment_type", 2, u64::from(self.segment_type))?;
        w.write("modulation_type_A", 2, u64::from(self.modulation_type_a))?;
        w.write("modulation_type_B", 2, u64::from(self.modulation_type_b))?;
        w.write("modulation_type_C", 2, u64::from(self.modulation_type_c))?;
        w.bytes(&self.additional_info);
        finish(w, out);
        Ok(())
    }
}

/// content_availability_descriptor, tag 0xDE — ARIB STD-B10 Part 2 §6.2.45,
/// Table 6-87.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ContentAvailability {
    /// reserved_future_use (1), spec value '1'.
    pub reserved_future_use: bool,
    /// copy_restriction_mode (1).
    pub copy_restriction_mode: bool,
    /// image_constraint_token (1).
    pub image_constraint_token: bool,
    /// retention_mode (1).
    pub retention_mode: bool,
    /// retention_state (3), Table 6-88.
    pub retention_state: u8,
    /// encryption_mode (1).
    pub encryption_mode: bool,
    /// Trailing reserved_future_use bytes (spec value 0xFF).
    pub reserved_future_use_bytes: Vec<u8>,
}

impl DescriptorBody for ContentAvailability {
    const TAG: u8 = 0xDE;
    const NAME: &'static str = "content_availability_descriptor";
    fn decode(data: &[u8]) -> Result<Self> {
        let mut r = BitReader::new(data, Self::NAME);
        Ok(Self {
            reserved_future_use: r.bool()?,
            copy_restriction_mode: r.bool()?,
            image_constraint_token: r.bool()?,
            retention_mode: r.bool()?,
            retention_state: r.read(3)? as u8,
            encryption_mode: r.bool()?,
            reserved_future_use_bytes: r.rest()?.to_vec(),
        })
    }
    fn encode(&self, out: &mut Vec<u8>) -> Result<()> {
        let mut w = BitWriter::new();
        w.bool(self.reserved_future_use);
        w.bool(self.copy_restriction_mode);
        w.bool(self.image_constraint_token);
        w.bool(self.retention_mode);
        w.write("retention_state", 3, u64::from(self.retention_state))?;
        w.bool(self.encryption_mode);
        w.bytes(&self.reserved_future_use_bytes);
        finish(w, out);
        Ok(())
    }
}

/// access_control_descriptor, tag 0xF6 — ARIB STD-B10 Part 2 §6.2.54,
/// Table 6-100.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AccessControl {
    /// CA_system_id (16).
    pub ca_system_id: u16,
    /// transmission_type (3), Table 6-101.
    pub transmission_type: u8,
    /// PID (13).
    pub pid: u16,
    /// private_data_byte loop.
    pub private_data: Vec<u8>,
}

impl DescriptorBody for AccessControl {
    const TAG: u8 = 0xF6;
    const NAME: &'static str = "access_control_descriptor";
    fn decode(data: &[u8]) -> Result<Self> {
        let mut r = BitReader::new(data, Self::NAME);
        Ok(Self {
            ca_system_id: r.u16()?,
            transmission_type: r.read(3)? as u8,
            pid: r.read(13)? as u16,
            private_data: r.rest()?.to_vec(),
        })
    }
    fn encode(&self, out: &mut Vec<u8>) -> Result<()> {
        let mut w = BitWriter::new();
        w.write("CA_system_id", 16, u64::from(self.ca_system_id))?;
        w.write("transmission_type", 3, u64::from(self.transmission_type))?;
        w.write("PID", 13, u64::from(self.pid))?;
        w.bytes(&self.private_data);
        finish(w, out);
        Ok(())
    }
}

/// terrestrial_delivery_system_descriptor, tag 0xFA — ARIB STD-B10 Part 2
/// §6.2.31, Table 6-61.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TerrestrialDeliverySystem {
    /// area_code (12).
    pub area_code: u16,
    /// guard_interval (2), Table 6-62.
    pub guard_interval: u8,
    /// transmission_mode (2), Table 6-63.
    pub transmission_mode: u8,
    /// frequency loop (16 bits each, unit 1/7 MHz).
    pub frequencies: Vec<u16>,
}

impl TerrestrialDeliverySystem {
    /// Centre frequency in Hz for a `frequency` value (unit 1/7 MHz, §6.2.31).
    pub fn frequency_hz(f: u16) -> f64 {
        f64::from(f) * 1e6 / 7.0
    }
}

impl DescriptorBody for TerrestrialDeliverySystem {
    const TAG: u8 = 0xFA;
    const NAME: &'static str = "terrestrial_delivery_system_descriptor";
    fn decode(data: &[u8]) -> Result<Self> {
        let mut r = BitReader::new(data, Self::NAME);
        let area_code = r.read(12)? as u16;
        let guard_interval = r.read(2)? as u8;
        let transmission_mode = r.read(2)? as u8;
        let n = multiple_of(Self::NAME, r.remaining_bytes(), 2)?;
        let mut frequencies = Vec::with_capacity(n);
        for _ in 0..n {
            frequencies.push(r.u16()?);
        }
        Ok(Self {
            area_code,
            guard_interval,
            transmission_mode,
            frequencies,
        })
    }
    fn encode(&self, out: &mut Vec<u8>) -> Result<()> {
        let mut w = BitWriter::new();
        w.write("area_code", 12, u64::from(self.area_code))?;
        w.write("guard_interval", 2, u64::from(self.guard_interval))?;
        w.write("transmission_mode", 2, u64::from(self.transmission_mode))?;
        for f in &self.frequencies {
            w.write("frequency", 16, u64::from(*f))?;
        }
        finish(w, out);
        Ok(())
    }
}

/// partial_reception_descriptor, tag 0xFB — ARIB STD-B10 Part 2 §6.2.32,
/// Table 6-64.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PartialReception {
    /// service_id loop.
    pub service_ids: Vec<u16>,
}

impl DescriptorBody for PartialReception {
    const TAG: u8 = 0xFB;
    const NAME: &'static str = "partial_reception_descriptor";
    fn decode(data: &[u8]) -> Result<Self> {
        multiple_of(Self::NAME, data.len(), 2)?;
        Ok(Self {
            service_ids: data
                .chunks(2)
                .map(|c| u16::from_be_bytes([c[0], c[1]]))
                .collect(),
        })
    }
    fn encode(&self, out: &mut Vec<u8>) -> Result<()> {
        for s in &self.service_ids {
            out.extend_from_slice(&s.to_be_bytes());
        }
        Ok(())
    }
}

/// (area_code, reserved_future_use) of the emergency information descriptor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AreaCode {
    /// area_code (12).
    pub area_code: u16,
    /// reserved_future_use (4), spec value '1111'.
    pub reserved_future_use: u8,
}

/// One service of the emergency information descriptor.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EmergencyService {
    /// service_id (16).
    pub service_id: u16,
    /// start_end_flag (1).
    pub start_end_flag: bool,
    /// signal_level (1).
    pub signal_level: bool,
    /// reserved_future_use (6), spec value all '1'.
    pub reserved_future_use: u8,
    /// area_code loop (its byte size is area_code_length).
    pub area_codes: Vec<AreaCode>,
}

/// emergency_information_descriptor, tag 0xFC — ARIB STD-B10 Part 2
/// §6.2.24, Table 6-40.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EmergencyInformation {
    /// Service loop.
    pub services: Vec<EmergencyService>,
}

impl DescriptorBody for EmergencyInformation {
    const TAG: u8 = 0xFC;
    const NAME: &'static str = "emergency_information_descriptor";
    fn decode(data: &[u8]) -> Result<Self> {
        let mut r = BitReader::new(data, Self::NAME);
        let mut services = Vec::new();
        while r.remaining_bytes() > 0 {
            let service_id = r.u16()?;
            let start_end_flag = r.bool()?;
            let signal_level = r.bool()?;
            let reserved_future_use = r.read(6)? as u8;
            let codes = len8_bytes(&mut r)?;
            multiple_of(Self::NAME, codes.len(), 2)?;
            let area_codes = codes
                .chunks(2)
                .map(|c| AreaCode {
                    area_code: (u16::from(c[0]) << 4) | u16::from(c[1] >> 4),
                    reserved_future_use: c[1] & 0x0F,
                })
                .collect();
            services.push(EmergencyService {
                service_id,
                start_end_flag,
                signal_level,
                reserved_future_use,
                area_codes,
            });
        }
        Ok(Self { services })
    }
    fn encode(&self, out: &mut Vec<u8>) -> Result<()> {
        let mut w = BitWriter::new();
        for s in &self.services {
            w.write("service_id", 16, u64::from(s.service_id))?;
            w.bool(s.start_end_flag);
            w.bool(s.signal_level);
            w.write("reserved_future_use", 6, u64::from(s.reserved_future_use))?;
            let mut cw = BitWriter::new();
            for a in &s.area_codes {
                cw.write("area_code", 12, u64::from(a.area_code))?;
                cw.write("reserved_future_use", 4, u64::from(a.reserved_future_use))?;
            }
            put_len8(&mut w, "area_code_length", &cw.into_bytes())?;
        }
        finish(w, out);
        Ok(())
    }
}

/// data_component_descriptor, tag 0xFD — ARIB STD-B10 Part 2 §6.2.20,
/// Table 6-32.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DataComponent {
    /// data_component_id (16).
    pub data_component_id: u16,
    /// additional_data_component_info loop (Annex J).
    pub additional_data_component_info: Vec<u8>,
}

impl DescriptorBody for DataComponent {
    const TAG: u8 = 0xFD;
    const NAME: &'static str = "data_component_descriptor";
    fn decode(data: &[u8]) -> Result<Self> {
        let mut r = BitReader::new(data, Self::NAME);
        Ok(Self {
            data_component_id: r.u16()?,
            additional_data_component_info: r.rest()?.to_vec(),
        })
    }
    fn encode(&self, out: &mut Vec<u8>) -> Result<()> {
        out.extend_from_slice(&self.data_component_id.to_be_bytes());
        out.extend_from_slice(&self.additional_data_component_info);
        Ok(())
    }
}

/// system_management_descriptor, tag 0xFE — ARIB STD-B10 Part 2 §6.2.21,
/// Tables 6-33/6-34.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SystemManagement {
    /// system_management_id (16), structured per Table 6-34.
    pub system_management_id: u16,
    /// additional_identification_info loop.
    pub additional_identification_info: Vec<u8>,
}

impl SystemManagement {
    /// broadcasting_flag (2 MSBs of system_management_id), Table 6-35.
    pub fn broadcasting_flag(&self) -> u8 {
        (self.system_management_id >> 14) as u8
    }
    /// broadcasting_identifier (next 6 bits), Table 6-36.
    pub fn broadcasting_identifier(&self) -> u8 {
        ((self.system_management_id >> 8) & 0x3F) as u8
    }
    /// additional_broadcasting_identification (low 8 bits).
    pub fn additional_broadcasting_identification(&self) -> u8 {
        self.system_management_id as u8
    }
}

impl DescriptorBody for SystemManagement {
    const TAG: u8 = 0xFE;
    const NAME: &'static str = "system_management_descriptor";
    fn decode(data: &[u8]) -> Result<Self> {
        let mut r = BitReader::new(data, Self::NAME);
        Ok(Self {
            system_management_id: r.u16()?,
            additional_identification_info: r.rest()?.to_vec(),
        })
    }
    fn encode(&self, out: &mut Vec<u8>) -> Result<()> {
        out.extend_from_slice(&self.system_management_id.to_be_bytes());
        out.extend_from_slice(&self.additional_identification_info);
        Ok(())
    }
}

// ---- typed views of table-coded fields (raw fields stay authoritative)

use crate::values::{
    AudioComponentType, CodedValue, DigitalRecordingControl, MaximumBitrate, SamplingRate,
    TransferCharacteristics, UserDefinedAnnexF, VideoEncodeFormat,
};

impl ComponentCopyControl {
    /// digital_recording_control_data as B10 Table 6-39.
    pub fn recording_control(&self) -> DigitalRecordingControl {
        DigitalRecordingControl::from_code(self.digital_recording_control_data)
    }
    /// user_defined read with the B10 Annex F (informative) layout.
    pub fn user_defined_annex_f(&self) -> UserDefinedAnnexF {
        UserDefinedAnnexF::from_code(self.user_defined)
    }
    /// maximum_bitrate (1/4 Mbit/s units).
    pub fn maximum_bitrate_typed(&self) -> Option<MaximumBitrate> {
        self.maximum_bitrate.map(MaximumBitrate)
    }
}

impl DigitalCopyControl {
    /// digital_recording_control_data as B10 Table 6-39.
    pub fn recording_control(&self) -> DigitalRecordingControl {
        DigitalRecordingControl::from_code(self.digital_recording_control_data)
    }
    /// user_defined read with the B10 Annex F (informative) layout
    /// (copy_control_type + APS_control_data).
    pub fn user_defined_annex_f(&self) -> UserDefinedAnnexF {
        UserDefinedAnnexF::from_code(self.user_defined)
    }
    /// maximum_bitrate (1/4 Mbit/s units).
    pub fn maximum_bitrate_typed(&self) -> Option<MaximumBitrate> {
        self.maximum_bitrate.map(MaximumBitrate)
    }
}

impl AudioComponent {
    /// component_type as B10 Table 6-43 (dialog control, audio for a person
    /// with disabilities, audio mode).
    pub fn audio_component_type(&self) -> AudioComponentType {
        AudioComponentType::from_code(self.component_type)
    }
    /// sampling_rate as B10 Table 6-45.
    pub fn sampling_rate_typed(&self) -> SamplingRate {
        SamplingRate::from_code(self.sampling_rate)
    }
}

impl VideoDecodeControl {
    /// video_encode_format as B10 Table 6-60.
    pub fn encode_format(&self) -> VideoEncodeFormat {
        VideoEncodeFormat::from_code(self.video_encode_format)
    }
    /// transfer_characteristics as B10 Table 6-60-1.
    pub fn transfer(&self) -> TransferCharacteristics {
        TransferCharacteristics::from_code(self.transfer_characteristics)
    }
}

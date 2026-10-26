//! Descriptors with tags 0x09–0x58 (ARIB STD-B10 Part 2 §6.2.1–§6.2.25 and
//! H.222.0 §2.6.16 for the conditional access descriptor).

use super::{Code3, DescriptorBody, code3, finish, len8_string, multiple_of, put_len8, string};
use crate::time::{BcdOffset, MjdTime};
use isdb_text::AribString;
use isdb_ts::Result;
use isdb_ts::bits::{BitReader, BitWriter};

/// conditional_access_descriptor, tag 0x09 — ITU-T H.222.0 §2.6.16,
/// Table 2-58 (referenced by ARIB STD-B10 Part 1 Table 5-3, Part 2 Table 6-1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConditionalAccess {
    /// CA_system_ID (16).
    pub ca_system_id: u16,
    /// reserved (3), spec value '111'.
    pub reserved: u8,
    /// CA_PID (13).
    pub ca_pid: u16,
    /// private_data_byte loop.
    pub private_data: Vec<u8>,
}

impl DescriptorBody for ConditionalAccess {
    const TAG: u8 = 0x09;
    const NAME: &'static str = "conditional_access_descriptor";
    fn decode(data: &[u8]) -> Result<Self> {
        let mut r = BitReader::new(data, Self::NAME);
        let ca_system_id = r.u16()?;
        let reserved = r.read(3)? as u8;
        let ca_pid = r.read(13)? as u16;
        Ok(Self {
            ca_system_id,
            reserved,
            ca_pid,
            private_data: r.rest()?.to_vec(),
        })
    }
    fn encode(&self, out: &mut Vec<u8>) -> Result<()> {
        let mut w = BitWriter::new();
        w.write("CA_system_ID", 16, u64::from(self.ca_system_id))?;
        w.write("reserved", 3, u64::from(self.reserved))?;
        w.write("CA_PID", 13, u64::from(self.ca_pid))?;
        w.bytes(&self.private_data);
        finish(w, out);
        Ok(())
    }
}

macro_rules! text_descriptor {
    ($(#[$m:meta])* $name:ident, $tag:literal, $sname:literal, $field:ident) => {
        $(#[$m])*
        #[derive(Debug, Clone, PartialEq, Eq, Default)]
        pub struct $name {
            /// The `char` loop (ARIB 8-unit coded, Annex A).
            pub $field: AribString,
        }
        impl DescriptorBody for $name {
            const TAG: u8 = $tag;
            const NAME: &'static str = $sname;
            fn decode(data: &[u8]) -> Result<Self> {
                Ok(Self { $field: AribString(data.to_vec()) })
            }
            fn encode(&self, out: &mut Vec<u8>) -> Result<()> {
                out.extend_from_slice(&self.$field.0);
                Ok(())
            }
        }
    };
}

text_descriptor!(
    /// network_name_descriptor, tag 0x40 — ARIB STD-B10 Part 2 §6.2.11, Table 6-21.
    NetworkName, 0x40, "network_name_descriptor", name
);
text_descriptor!(
    /// bouquet_name_descriptor, tag 0x47 — ARIB STD-B10 Part 2 §6.2.1, Table 6-2.
    BouquetName, 0x47, "bouquet_name_descriptor", name
);

/// One entry of the service list descriptor (ARIB STD-B10 Part 2 §6.2.14).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServiceListEntry {
    /// service_id (16).
    pub service_id: u16,
    /// service_type (8), Table 6-25.
    pub service_type: u8,
}

/// service_list_descriptor, tag 0x41 — ARIB STD-B10 Part 2 §6.2.14, Table 6-26.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ServiceList {
    /// The service loop.
    pub services: Vec<ServiceListEntry>,
}

impl DescriptorBody for ServiceList {
    const TAG: u8 = 0x41;
    const NAME: &'static str = "service_list_descriptor";
    fn decode(data: &[u8]) -> Result<Self> {
        let n = multiple_of(Self::NAME, data.len(), 3)?;
        let mut r = BitReader::new(data, Self::NAME);
        let mut services = Vec::with_capacity(n);
        for _ in 0..n {
            services.push(ServiceListEntry {
                service_id: r.u16()?,
                service_type: r.u8()?,
            });
        }
        Ok(Self { services })
    }
    fn encode(&self, out: &mut Vec<u8>) -> Result<()> {
        for s in &self.services {
            out.extend_from_slice(&s.service_id.to_be_bytes());
            out.push(s.service_type);
        }
        Ok(())
    }
}

/// stuffing_descriptor, tag 0x42 — ARIB STD-B10 Part 2 §6.2.17, Table 6-29.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Stuffing {
    /// stuffing_byte loop (any value).
    pub bytes: Vec<u8>,
}

impl DescriptorBody for Stuffing {
    const TAG: u8 = 0x42;
    const NAME: &'static str = "stuffing_descriptor";
    fn decode(data: &[u8]) -> Result<Self> {
        Ok(Self {
            bytes: data.to_vec(),
        })
    }
    fn encode(&self, out: &mut Vec<u8>) -> Result<()> {
        out.extend_from_slice(&self.bytes);
        Ok(())
    }
}

/// satellite_delivery_system_descriptor, tag 0x43 — ARIB STD-B10 Part 2
/// §6.2.6, Table 6-8 (BCD-coded fields kept raw).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SatelliteDeliverySystem {
    /// frequency (32): 8 BCD digits, GHz with the decimal point after the 3rd.
    pub frequency: u32,
    /// orbital_position (16): 4 BCD digits, degrees, point after the 3rd.
    pub orbital_position: u16,
    /// west_east_flag (1): 0 west, 1 east.
    pub west_east_flag: bool,
    /// polarization (2), Table 6-9.
    pub polarization: u8,
    /// modulation (5), Table 6-10.
    pub modulation: u8,
    /// symbol_rate (28) ("system_rate" in Table 6-8): 7 BCD digits.
    pub symbol_rate: u32,
    /// FEC_inner (4), Table 6-11.
    pub fec_inner: u8,
}

impl DescriptorBody for SatelliteDeliverySystem {
    const TAG: u8 = 0x43;
    const NAME: &'static str = "satellite_delivery_system_descriptor";
    fn decode(data: &[u8]) -> Result<Self> {
        let mut r = BitReader::new(data, Self::NAME);
        let d = Self {
            frequency: r.u32()?,
            orbital_position: r.u16()?,
            west_east_flag: r.bool()?,
            polarization: r.read(2)? as u8,
            modulation: r.read(5)? as u8,
            symbol_rate: r.read(28)? as u32,
            fec_inner: r.read(4)? as u8,
        };
        r.finish()?;
        Ok(d)
    }
    fn encode(&self, out: &mut Vec<u8>) -> Result<()> {
        let mut w = BitWriter::new();
        w.write("frequency", 32, u64::from(self.frequency))?;
        w.write("orbital_position", 16, u64::from(self.orbital_position))?;
        w.bool(self.west_east_flag);
        w.write("polarization", 2, u64::from(self.polarization))?;
        w.write("modulation", 5, u64::from(self.modulation))?;
        w.write("symbol_rate", 28, u64::from(self.symbol_rate))?;
        w.write("FEC_inner", 4, u64::from(self.fec_inner))?;
        finish(w, out);
        Ok(())
    }
}

/// service_descriptor, tag 0x48 — ARIB STD-B10 Part 2 §6.2.13, Table 6-24.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Service {
    /// service_type (8), Table 6-25.
    pub service_type: u8,
    /// Service provider name (`char` loop after service_provider_name_length).
    pub provider_name: AribString,
    /// Service name (`char` loop after service_name_length).
    pub service_name: AribString,
}

impl DescriptorBody for Service {
    const TAG: u8 = 0x48;
    const NAME: &'static str = "service_descriptor";
    fn decode(data: &[u8]) -> Result<Self> {
        let mut r = BitReader::new(data, Self::NAME);
        let service_type = r.u8()?;
        let provider_name = len8_string(&mut r)?;
        let service_name = len8_string(&mut r)?;
        r.finish()?;
        Ok(Self {
            service_type,
            provider_name,
            service_name,
        })
    }
    fn encode(&self, out: &mut Vec<u8>) -> Result<()> {
        let mut w = BitWriter::new();
        w.write("service_type", 8, u64::from(self.service_type))?;
        put_len8(
            &mut w,
            "service_provider_name_length",
            &self.provider_name.0,
        )?;
        put_len8(&mut w, "service_name_length", &self.service_name.0)?;
        finish(w, out);
        Ok(())
    }
}

/// country_availability_descriptor, tag 0x49 — ARIB STD-B10 Part 2 §6.2.5,
/// Table 6-7.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CountryAvailability {
    /// country_availability_flag (1).
    pub country_availability_flag: bool,
    /// reserved_future_use (7), spec value all '1'.
    pub reserved_future_use: u8,
    /// country_code loop (ISO 3166, 24 bits each).
    pub country_codes: Vec<Code3>,
}

impl DescriptorBody for CountryAvailability {
    const TAG: u8 = 0x49;
    const NAME: &'static str = "country_availability_descriptor";
    fn decode(data: &[u8]) -> Result<Self> {
        let mut r = BitReader::new(data, Self::NAME);
        let country_availability_flag = r.bool()?;
        let reserved_future_use = r.read(7)? as u8;
        let n = multiple_of(Self::NAME, r.remaining_bytes(), 3)?;
        let mut country_codes = Vec::with_capacity(n);
        for _ in 0..n {
            country_codes.push(code3(&mut r)?);
        }
        Ok(Self {
            country_availability_flag,
            reserved_future_use,
            country_codes,
        })
    }
    fn encode(&self, out: &mut Vec<u8>) -> Result<()> {
        let mut w = BitWriter::new();
        w.bool(self.country_availability_flag);
        w.write(
            "reserved_future_use",
            7,
            u64::from(self.reserved_future_use),
        )?;
        for c in &self.country_codes {
            w.bytes(c);
        }
        finish(w, out);
        Ok(())
    }
}

/// linkage_descriptor, tag 0x4A — ARIB STD-B10 Part 2 §6.2.8, Table 6-13.
/// `private_data` is kept raw (for linkage_type 0x0B its layout is Table
/// 6-14-1).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Linkage {
    /// transport_stream_id (16).
    pub transport_stream_id: u16,
    /// original_network_id (16).
    pub original_network_id: u16,
    /// service_id (16); 0x0000 when linkage_type is 0x04.
    pub service_id: u16,
    /// linkage_type (8), Table 6-14.
    pub linkage_type: u8,
    /// private_data_byte loop.
    pub private_data: Vec<u8>,
}

impl DescriptorBody for Linkage {
    const TAG: u8 = 0x4A;
    const NAME: &'static str = "linkage_descriptor";
    fn decode(data: &[u8]) -> Result<Self> {
        let mut r = BitReader::new(data, Self::NAME);
        Ok(Self {
            transport_stream_id: r.u16()?,
            original_network_id: r.u16()?,
            service_id: r.u16()?,
            linkage_type: r.u8()?,
            private_data: r.rest()?.to_vec(),
        })
    }
    fn encode(&self, out: &mut Vec<u8>) -> Result<()> {
        out.extend_from_slice(&self.transport_stream_id.to_be_bytes());
        out.extend_from_slice(&self.original_network_id.to_be_bytes());
        out.extend_from_slice(&self.service_id.to_be_bytes());
        out.push(self.linkage_type);
        out.extend_from_slice(&self.private_data);
        Ok(())
    }
}

/// Service triplet (transport_stream_id, original_network_id, service_id).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServiceTriplet {
    /// transport_stream_id (16).
    pub transport_stream_id: u16,
    /// original_network_id (16).
    pub original_network_id: u16,
    /// service_id (16).
    pub service_id: u16,
}

/// NVOD_reference_descriptor, tag 0x4B — ARIB STD-B10 Part 2 §6.2.10,
/// Table 6-20.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NvodReference {
    /// The service loop.
    pub services: Vec<ServiceTriplet>,
}

impl DescriptorBody for NvodReference {
    const TAG: u8 = 0x4B;
    const NAME: &'static str = "NVOD_reference_descriptor";
    fn decode(data: &[u8]) -> Result<Self> {
        let n = multiple_of(Self::NAME, data.len(), 6)?;
        let mut r = BitReader::new(data, Self::NAME);
        let mut services = Vec::with_capacity(n);
        for _ in 0..n {
            services.push(ServiceTriplet {
                transport_stream_id: r.u16()?,
                original_network_id: r.u16()?,
                service_id: r.u16()?,
            });
        }
        Ok(Self { services })
    }
    fn encode(&self, out: &mut Vec<u8>) -> Result<()> {
        for s in &self.services {
            out.extend_from_slice(&s.transport_stream_id.to_be_bytes());
            out.extend_from_slice(&s.original_network_id.to_be_bytes());
            out.extend_from_slice(&s.service_id.to_be_bytes());
        }
        Ok(())
    }
}

/// time_shifted_service_descriptor, tag 0x4C — ARIB STD-B10 Part 2 §6.2.19,
/// Table 6-31.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeShiftedService {
    /// reference_service_id (16).
    pub reference_service_id: u16,
}

impl DescriptorBody for TimeShiftedService {
    const TAG: u8 = 0x4C;
    const NAME: &'static str = "time_shifted_service_descriptor";
    fn decode(data: &[u8]) -> Result<Self> {
        let mut r = BitReader::new(data, Self::NAME);
        let reference_service_id = r.u16()?;
        r.finish()?;
        Ok(Self {
            reference_service_id,
        })
    }
    fn encode(&self, out: &mut Vec<u8>) -> Result<()> {
        out.extend_from_slice(&self.reference_service_id.to_be_bytes());
        Ok(())
    }
}

/// short_event_descriptor, tag 0x4D — ARIB STD-B10 Part 2 §6.2.15,
/// Table 6-27.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ShortEvent {
    /// ISO_639_language_code (24).
    pub language: Code3,
    /// event_name_char loop.
    pub event_name: AribString,
    /// text_char loop.
    pub text: AribString,
}

impl DescriptorBody for ShortEvent {
    const TAG: u8 = 0x4D;
    const NAME: &'static str = "short_event_descriptor";
    fn decode(data: &[u8]) -> Result<Self> {
        let mut r = BitReader::new(data, Self::NAME);
        let language = code3(&mut r)?;
        let event_name = len8_string(&mut r)?;
        let text = len8_string(&mut r)?;
        r.finish()?;
        Ok(Self {
            language,
            event_name,
            text,
        })
    }
    fn encode(&self, out: &mut Vec<u8>) -> Result<()> {
        let mut w = BitWriter::new();
        w.bytes(&self.language);
        put_len8(&mut w, "event_name_length", &self.event_name.0)?;
        put_len8(&mut w, "text_length", &self.text.0)?;
        finish(w, out);
        Ok(())
    }
}

/// One item of the extended event descriptor (ARIB STD-B10 Part 2 §6.2.7).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ExtendedEventItem {
    /// item_description_char loop.
    pub description: AribString,
    /// item_char loop.
    pub item: AribString,
}

/// extended_event_descriptor, tag 0x4E — ARIB STD-B10 Part 2 §6.2.7,
/// Table 6-12.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ExtendedEvent {
    /// descriptor_number (4).
    pub descriptor_number: u8,
    /// last_descriptor_number (4).
    pub last_descriptor_number: u8,
    /// ISO_639_language_code (24).
    pub language: Code3,
    /// Item loop (its byte size is `length_of_items`).
    pub items: Vec<ExtendedEventItem>,
    /// text_char loop.
    pub text: AribString,
}

impl DescriptorBody for ExtendedEvent {
    const TAG: u8 = 0x4E;
    const NAME: &'static str = "extended_event_descriptor";
    fn decode(data: &[u8]) -> Result<Self> {
        let mut r = BitReader::new(data, Self::NAME);
        let descriptor_number = r.read(4)? as u8;
        let last_descriptor_number = r.read(4)? as u8;
        let language = code3(&mut r)?;
        let items_bytes = super::len8_bytes(&mut r)?;
        let mut ir = BitReader::new(items_bytes, "extended_event_descriptor items");
        let mut items = Vec::new();
        while ir.remaining_bytes() > 0 {
            let description = len8_string(&mut ir)?;
            let item = len8_string(&mut ir)?;
            items.push(ExtendedEventItem { description, item });
        }
        let text = len8_string(&mut r)?;
        r.finish()?;
        Ok(Self {
            descriptor_number,
            last_descriptor_number,
            language,
            items,
            text,
        })
    }
    fn encode(&self, out: &mut Vec<u8>) -> Result<()> {
        let mut items = BitWriter::new();
        for it in &self.items {
            put_len8(&mut items, "item_description_length", &it.description.0)?;
            put_len8(&mut items, "item_length", &it.item.0)?;
        }
        let items = items.into_bytes();
        let mut w = BitWriter::new();
        w.write("descriptor_number", 4, u64::from(self.descriptor_number))?;
        w.write(
            "last_descriptor_number",
            4,
            u64::from(self.last_descriptor_number),
        )?;
        w.bytes(&self.language);
        put_len8(&mut w, "length_of_items", &items)?;
        put_len8(&mut w, "text_length", &self.text.0)?;
        finish(w, out);
        Ok(())
    }
}

/// time_shifted_event_descriptor, tag 0x4F — ARIB STD-B10 Part 2 §6.2.18,
/// Table 6-30.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeShiftedEvent {
    /// reference_service_id (16).
    pub reference_service_id: u16,
    /// reference_event_id (16).
    pub reference_event_id: u16,
}

impl DescriptorBody for TimeShiftedEvent {
    const TAG: u8 = 0x4F;
    const NAME: &'static str = "time_shifted_event_descriptor";
    fn decode(data: &[u8]) -> Result<Self> {
        let mut r = BitReader::new(data, Self::NAME);
        let d = Self {
            reference_service_id: r.u16()?,
            reference_event_id: r.u16()?,
        };
        r.finish()?;
        Ok(d)
    }
    fn encode(&self, out: &mut Vec<u8>) -> Result<()> {
        out.extend_from_slice(&self.reference_service_id.to_be_bytes());
        out.extend_from_slice(&self.reference_event_id.to_be_bytes());
        Ok(())
    }
}

/// component_descriptor, tag 0x50 — ARIB STD-B10 Part 2 §6.2.3, Table 6-4.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Component {
    /// reserved_future_use (4), spec value '1111'.
    pub reserved_future_use: u8,
    /// stream_content (4), Table 6-5.
    pub stream_content: u8,
    /// component_type (8), Table 6-5.
    pub component_type: u8,
    /// component_tag (8).
    pub component_tag: u8,
    /// ISO_639_language_code (24).
    pub language: Code3,
    /// text_char loop.
    pub text: AribString,
}

impl DescriptorBody for Component {
    const TAG: u8 = 0x50;
    const NAME: &'static str = "component_descriptor";
    fn decode(data: &[u8]) -> Result<Self> {
        let mut r = BitReader::new(data, Self::NAME);
        let reserved_future_use = r.read(4)? as u8;
        let stream_content = r.read(4)? as u8;
        let component_type = r.u8()?;
        let component_tag = r.u8()?;
        let language = code3(&mut r)?;
        let n = r.remaining_bytes();
        let text = string(&mut r, n)?;
        Ok(Self {
            reserved_future_use,
            stream_content,
            component_type,
            component_tag,
            language,
            text,
        })
    }
    fn encode(&self, out: &mut Vec<u8>) -> Result<()> {
        let mut w = BitWriter::new();
        w.write(
            "reserved_future_use",
            4,
            u64::from(self.reserved_future_use),
        )?;
        w.write("stream_content", 4, u64::from(self.stream_content))?;
        w.write("component_type", 8, u64::from(self.component_type))?;
        w.write("component_tag", 8, u64::from(self.component_tag))?;
        w.bytes(&self.language);
        w.bytes(&self.text.0);
        finish(w, out);
        Ok(())
    }
}

/// stream_identifier_descriptor, tag 0x52 — ARIB STD-B10 Part 2 §6.2.16,
/// Table 6-28.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamIdentifier {
    /// component_tag (8).
    pub component_tag: u8,
}

impl DescriptorBody for StreamIdentifier {
    const TAG: u8 = 0x52;
    const NAME: &'static str = "stream_identifier_descriptor";
    fn decode(data: &[u8]) -> Result<Self> {
        let mut r = BitReader::new(data, Self::NAME);
        let component_tag = r.u8()?;
        r.finish()?;
        Ok(Self { component_tag })
    }
    fn encode(&self, out: &mut Vec<u8>) -> Result<()> {
        out.push(self.component_tag);
        Ok(())
    }
}

/// CA_identifier_descriptor, tag 0x53 — ARIB STD-B10 Part 2 §6.2.2, Table 6-3.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CaIdentifier {
    /// CA_system_id loop.
    pub ca_system_ids: Vec<u16>,
}

impl DescriptorBody for CaIdentifier {
    const TAG: u8 = 0x53;
    const NAME: &'static str = "CA_identifier_descriptor";
    fn decode(data: &[u8]) -> Result<Self> {
        multiple_of(Self::NAME, data.len(), 2)?;
        Ok(Self {
            ca_system_ids: data
                .chunks(2)
                .map(|c| u16::from_be_bytes([c[0], c[1]]))
                .collect(),
        })
    }
    fn encode(&self, out: &mut Vec<u8>) -> Result<()> {
        for id in &self.ca_system_ids {
            out.extend_from_slice(&id.to_be_bytes());
        }
        Ok(())
    }
}

/// One genre entry of the content descriptor (ARIB STD-B10 Part 2 §6.2.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContentItem {
    /// content_nibble_level_1 (4), Annex H.
    pub content_nibble_level_1: u8,
    /// content_nibble_level_2 (4), Annex H.
    pub content_nibble_level_2: u8,
    /// First user_nibble (4).
    pub user_nibble_1: u8,
    /// Second user_nibble (4).
    pub user_nibble_2: u8,
}

/// content_descriptor, tag 0x54 — ARIB STD-B10 Part 2 §6.2.4, Table 6-6.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Content {
    /// The genre loop.
    pub items: Vec<ContentItem>,
}

impl DescriptorBody for Content {
    const TAG: u8 = 0x54;
    const NAME: &'static str = "content_descriptor";
    fn decode(data: &[u8]) -> Result<Self> {
        multiple_of(Self::NAME, data.len(), 2)?;
        Ok(Self {
            items: data
                .chunks(2)
                .map(|c| ContentItem {
                    content_nibble_level_1: c[0] >> 4,
                    content_nibble_level_2: c[0] & 0x0F,
                    user_nibble_1: c[1] >> 4,
                    user_nibble_2: c[1] & 0x0F,
                })
                .collect(),
        })
    }
    fn encode(&self, out: &mut Vec<u8>) -> Result<()> {
        let mut w = BitWriter::new();
        for it in &self.items {
            w.write(
                "content_nibble_level_1",
                4,
                u64::from(it.content_nibble_level_1),
            )?;
            w.write(
                "content_nibble_level_2",
                4,
                u64::from(it.content_nibble_level_2),
            )?;
            w.write("user_nibble", 4, u64::from(it.user_nibble_1))?;
            w.write("user_nibble", 4, u64::from(it.user_nibble_2))?;
        }
        finish(w, out);
        Ok(())
    }
}

/// One rating of the parental rating descriptor (ARIB STD-B10 Part 2 §6.2.12).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParentalRatingEntry {
    /// country_code (24).
    pub country_code: Code3,
    /// rating (8), Table 6-23.
    pub rating: u8,
}

/// parental_rating_descriptor, tag 0x55 — ARIB STD-B10 Part 2 §6.2.12,
/// Table 6-22.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ParentalRating {
    /// The rating loop.
    pub ratings: Vec<ParentalRatingEntry>,
}

impl DescriptorBody for ParentalRating {
    const TAG: u8 = 0x55;
    const NAME: &'static str = "parental_rating_descriptor";
    fn decode(data: &[u8]) -> Result<Self> {
        multiple_of(Self::NAME, data.len(), 4)?;
        Ok(Self {
            ratings: data
                .chunks(4)
                .map(|c| ParentalRatingEntry {
                    country_code: [c[0], c[1], c[2]],
                    rating: c[3],
                })
                .collect(),
        })
    }
    fn encode(&self, out: &mut Vec<u8>) -> Result<()> {
        for e in &self.ratings {
            out.extend_from_slice(&e.country_code);
            out.push(e.rating);
        }
        Ok(())
    }
}

/// One region of the local time offset descriptor (ARIB STD-B10 Part 2
/// §6.2.25).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalTimeOffsetEntry {
    /// country_code (24).
    pub country_code: Code3,
    /// country_region_id (6).
    pub country_region_id: u8,
    /// reserved (1), spec value '1'.
    pub reserved: bool,
    /// local_time_offset_polarity (1): '0' local time ahead of JST, '1' behind.
    pub local_time_offset_polarity: bool,
    /// local_time_offset (16), 4 BCD digits hhmm.
    pub local_time_offset: BcdOffset,
    /// time_of_change (40), MJD + BCD JST.
    pub time_of_change: MjdTime,
    /// next_time_offset (16), 4 BCD digits hhmm.
    pub next_time_offset: BcdOffset,
}

/// local_time_offset_descriptor, tag 0x58 — ARIB STD-B10 Part 2 §6.2.25,
/// Table 6-41.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LocalTimeOffset {
    /// The region loop.
    pub entries: Vec<LocalTimeOffsetEntry>,
}

impl DescriptorBody for LocalTimeOffset {
    const TAG: u8 = 0x58;
    const NAME: &'static str = "local_time_offset_descriptor";
    fn decode(data: &[u8]) -> Result<Self> {
        let n = multiple_of(Self::NAME, data.len(), 13)?;
        let mut r = BitReader::new(data, Self::NAME);
        let mut entries = Vec::with_capacity(n);
        for _ in 0..n {
            entries.push(LocalTimeOffsetEntry {
                country_code: code3(&mut r)?,
                country_region_id: r.read(6)? as u8,
                reserved: r.bool()?,
                local_time_offset_polarity: r.bool()?,
                local_time_offset: BcdOffset(r.u16()?),
                time_of_change: MjdTime::from_u40(r.read(40)?),
                next_time_offset: BcdOffset(r.u16()?),
            });
        }
        Ok(Self { entries })
    }
    fn encode(&self, out: &mut Vec<u8>) -> Result<()> {
        let mut w = BitWriter::new();
        for e in &self.entries {
            w.bytes(&e.country_code);
            w.write("country_region_id", 6, u64::from(e.country_region_id))?;
            w.bool(e.reserved);
            w.bool(e.local_time_offset_polarity);
            w.write("local_time_offset", 16, u64::from(e.local_time_offset.0))?;
            w.write("time_of_change", 40, e.time_of_change.checked_u40()?)?;
            w.write("next_time_offset", 16, u64::from(e.next_time_offset.0))?;
        }
        finish(w, out);
        Ok(())
    }
}

impl Component {
    /// component_type as B10 Table 6-5 when stream_content is 0x1 (MPEG-2
    /// video); `None` for other stream_content values.
    pub fn video_component_type(&self) -> Option<crate::values::VideoComponentType> {
        use crate::values::CodedValue;
        (self.stream_content == 0x1)
            .then(|| crate::values::VideoComponentType::from_code(self.component_type))
    }
}

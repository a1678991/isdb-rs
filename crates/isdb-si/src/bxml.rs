//! The BML / data-carousel signalling structures carried inside SI
//! descriptors (ARIB STD-B24 Vol. 2 §9.3 and Vol. 3 Annex C; Vol. 4
//! Table 7-1 for the AIT):
//!
//! * [`AdditionalAribBxmlInfo`] — `additional_arib_bxml_info()` (Vol. 2
//!   §9.3.2 Table 9-12), the `additional_data_component_info` of the PMT
//!   data component descriptor (0xFD, data_component_id 0x0007 / 0x000C /
//!   0x000D) of a BML component; with `additional_arib_carousel_info()`
//!   (Vol. 3 Annex C.1).
//! * [`AribBxmlInfo`] — `arib_bxml_info()` (Vol. 2 §9.3.3 Table 9-13), the
//!   selector of the EIT data content descriptor (0xC7); with
//!   `arib_carousel_info()` (Vol. 3 Annex C.2.1).
//! * [`AitIdentifierInfo`] — `ait_identifier_info()` (Vol. 4 §7.3.2
//!   Table 7-1), the `additional_data_component_info` of the data component
//!   descriptor of a dedicated AIT component.
//! * [`BmlControl`] — `control_data_byte()` of the DII Control descriptor
//!   (Vol. 2 §9.3.5 Table 9-15).
//!
//! The data component descriptor and data content descriptor codecs keep
//! these bytes raw ([`crate::descriptor::DataComponent`],
//! [`crate::descriptor::DataContent`]); these structures decode and encode
//! them. Codecs are lossless (reserved bits kept, lengths recomputed).

use isdb_ts::Result;
use isdb_ts::bits::{BitReader, BitWriter};

/// data_component_id "Multimedia coding for digital terrestrial
/// broadcasting (A profile)" (B10 Annex J Table J-1).
pub const DATA_COMPONENT_ID_TERRESTRIAL_A: u16 = 0x000C;
/// data_component_id "C profile" (one-seg, B10 Annex J Table J-1).
pub const DATA_COMPONENT_ID_TERRESTRIAL_C: u16 = 0x000D;
/// data_component_id "ARIB-XML-base multimedia coding" (BS/CS BML, B10
/// Annex J Table J-1).
pub const DATA_COMPONENT_ID_BXML: u16 = 0x0007;
/// transmission_format "Data carousel and event message transmission
/// methods" (Vol. 2 Table 9-12 / 9-13; the only defined value).
pub const TRANSMISSION_FORMAT_CAROUSEL: u8 = 0b00;

/// BML version numbers (bml_major_version / bml_minor_version and the
/// optional B-XML pair) sent when default_version_flag = 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BmlVersions {
    /// bml_major_version.
    pub bml_major: u16,
    /// bml_minor_version.
    pub bml_minor: u16,
    /// bxml_major_version / bxml_minor_version, present when use_xml = 1.
    pub bxml: Option<(u16, u16)>,
}

fn read_versions(r: &mut BitReader<'_>, use_xml: bool) -> Result<BmlVersions> {
    let bml_major = r.u16()?;
    let bml_minor = r.u16()?;
    let bxml = if use_xml {
        Some((r.u16()?, r.u16()?))
    } else {
        None
    };
    Ok(BmlVersions {
        bml_major,
        bml_minor,
        bxml,
    })
}

fn write_versions(w: &mut BitWriter, v: &BmlVersions, use_xml: bool) -> Result<()> {
    w.write("bml_major_version", 16, u64::from(v.bml_major))?;
    w.write("bml_minor_version", 16, u64::from(v.bml_minor))?;
    if use_xml {
        let (ma, mi) = v.bxml.unwrap_or((0, 0));
        w.write("bxml_major_version", 16, u64::from(ma))?;
        w.write("bxml_minor_version", 16, u64::from(mi))?;
    }
    Ok(())
}

/// `additional_arib_carousel_info()` (Vol. 3 Annex C.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdditionalAribCarouselInfo {
    /// data_event_id (4): the data event whose DIIs and event messages are
    /// valid; all bits 1 = "DIIs and the event messages having a
    /// data_event_id identifier of any value … are valid".
    pub data_event_id: u8,
    /// event_section_flag: event messages are transmitted in this
    /// component.
    pub event_section: bool,
    /// reserved (3), spec value '111'.
    pub reserved: u8,
}

/// The entry-point part of [`AdditionalAribBxmlInfo`] (present when
/// entry_point_flag = 1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BxmlEntryPoint {
    /// auto_start_flag: "The first BML/B-XML document is invoked
    /// immediately".
    pub auto_start: bool,
    /// document_resolution (4), [`crate::values::DocumentResolution`].
    pub document_resolution: u8,
    /// use_xml.
    pub use_xml: bool,
    /// independent_flag.
    pub independent: bool,
    /// style_for_tv_flag.
    pub style_for_tv: bool,
    /// reserved_future_use (4), spec value '1111'. (ARIB TR-B14 is
    /// understood to define a `start_priority` for Hybridcast inside these
    /// bits; the local texts do not give its position.)
    pub reserved: u8,
    /// BML versions when default_version_flag = 0; `None` =
    /// default_version_flag 1 ("Use default values for version numbers",
    /// "defined in an operational standard regulation").
    pub versions: Option<BmlVersions>,
}

/// entry_point_flag and what follows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BxmlEntry {
    /// entry_point_flag = 1: "The component stream transmits a BML/B-XML
    /// document to be invoked" first.
    Entry(BxmlEntryPoint),
    /// entry_point_flag = 0, with the 5 reserved_future_use bits (spec
    /// value '11111').
    NotEntry {
        /// reserved_future_use (5).
        reserved: u8,
    },
}

/// `additional_arib_bxml_info()` (Vol. 2 §9.3.2 Table 9-12).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdditionalAribBxmlInfo {
    /// transmission_format (2): '00' carousel + event message.
    pub transmission_format: u8,
    /// entry_point_flag and its dependent fields.
    pub entry: BxmlEntry,
    /// Carousel fields, present when transmission_format = '00'.
    pub carousel: Option<AdditionalAribCarouselInfo>,
    /// ondemand_retrieval_flag (with the carousel fields).
    pub ondemand_retrieval: bool,
    /// file_storable_flag (with the carousel fields).
    pub file_storable: bool,
    /// reserved_future_use (6) after file_storable_flag, spec value
    /// '111111'.
    pub reserved: u8,
}

impl AdditionalAribBxmlInfo {
    /// Decode the additional_data_component_info bytes.
    pub fn decode(data: &[u8]) -> Result<Self> {
        let mut r = BitReader::new(data, "additional_arib_bxml_info");
        let transmission_format = r.read_u8(2)?;
        let entry_point_flag = r.bool()?;
        let entry = if entry_point_flag {
            let auto_start = r.bool()?;
            let document_resolution = r.read_u8(4)?;
            let use_xml = r.bool()?;
            let default_version_flag = r.bool()?;
            let independent = r.bool()?;
            let style_for_tv = r.bool()?;
            let reserved = r.read_u8(4)?;
            let versions = if default_version_flag {
                None
            } else {
                Some(read_versions(&mut r, use_xml)?)
            };
            BxmlEntry::Entry(BxmlEntryPoint {
                auto_start,
                document_resolution,
                use_xml,
                independent,
                style_for_tv,
                reserved,
                versions,
            })
        } else {
            BxmlEntry::NotEntry {
                reserved: r.read_u8(5)?,
            }
        };
        let (carousel, ondemand_retrieval, file_storable, reserved) =
            if transmission_format == TRANSMISSION_FORMAT_CAROUSEL {
                let c = AdditionalAribCarouselInfo {
                    data_event_id: r.read_u8(4)?,
                    event_section: r.bool()?,
                    reserved: r.read_u8(3)?,
                };
                (Some(c), r.bool()?, r.bool()?, r.read_u8(6)?)
            } else {
                (None, false, false, 0)
            };
        r.finish()?;
        Ok(Self {
            transmission_format,
            entry,
            carousel,
            ondemand_retrieval,
            file_storable,
            reserved,
        })
    }

    /// Encode to additional_data_component_info bytes.
    pub fn encode(&self) -> Result<Vec<u8>> {
        let mut w = BitWriter::new();
        w.write(
            "transmission_format",
            2,
            u64::from(self.transmission_format),
        )?;
        match &self.entry {
            BxmlEntry::Entry(e) => {
                w.bool(true);
                w.bool(e.auto_start);
                w.write("document_resolution", 4, u64::from(e.document_resolution))?;
                w.bool(e.use_xml);
                w.bool(e.versions.is_none());
                w.bool(e.independent);
                w.bool(e.style_for_tv);
                w.write("reserved_future_use", 4, u64::from(e.reserved))?;
                if let Some(v) = &e.versions {
                    write_versions(&mut w, v, e.use_xml)?;
                }
            }
            BxmlEntry::NotEntry { reserved } => {
                w.bool(false);
                w.write("reserved_future_use", 5, u64::from(*reserved))?;
            }
        }
        if self.transmission_format == TRANSMISSION_FORMAT_CAROUSEL {
            let c = self.carousel.unwrap_or(AdditionalAribCarouselInfo {
                data_event_id: 0,
                event_section: false,
                reserved: 0b111,
            });
            w.write("data_event_id", 4, u64::from(c.data_event_id))?;
            w.bool(c.event_section);
            w.write("reserved", 3, u64::from(c.reserved))?;
            w.bool(self.ondemand_retrieval);
            w.bool(self.file_storable);
            w.write("reserved_future_use", 6, u64::from(self.reserved))?;
        }
        Ok(w.into_bytes())
    }
}

/// One carousel of `arib_carousel_info()` (Vol. 3 Annex C.2.1). Each
/// optional field is present when its flag is '1'.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CarouselInfo {
    /// component_tag of the carousel's component.
    pub component_tag: u8,
    /// event_section_flag.
    pub event_section: bool,
    /// reserved_future_use (3), spec value '111'.
    pub reserved: u8,
    /// component_size (bytes of the whole carousel), when
    /// component_size_flag = 1.
    pub component_size: Option<u32>,
    /// transaction_id, when default_transaction_id_flag = 1.
    pub transaction_id: Option<u32>,
    /// timeout_value_DII (ms; 0xFFFFFFFF = none), when
    /// default_timeout_DII_flag = 1.
    pub timeout_dii: Option<u32>,
    /// leak_rate (22 bits, × 50 byte/s) and its 2 reserved bits, when
    /// default_leak_rate_flag = 1.
    pub leak_rate: Option<(u32, u8)>,
}

/// `arib_bxml_info()` (Vol. 2 §9.3.3 Table 9-13), the selector of the EIT
/// data content descriptor of a BML content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AribBxmlInfo {
    /// transmission_format (2).
    pub transmission_format: u8,
    /// reserved_future_use (1) after transmission_format.
    pub reserved_1: bool,
    /// auto_start_flag.
    pub auto_start: bool,
    /// document_resolution (4).
    pub document_resolution: u8,
    /// use_xml.
    pub use_xml: bool,
    /// independent_flag.
    pub independent: bool,
    /// content_id and content_version, when content_id_flag = 1.
    pub content_id: Option<(u32, u16)>,
    /// associated_contents_flag.
    pub associated_contents: bool,
    /// reserved_future_use (1) before style_for_tv_flag.
    pub reserved_2: bool,
    /// style_for_tv_flag.
    pub style_for_tv: bool,
    /// update_flag.
    pub update: bool,
    /// ISO_639_language_code.
    pub language: [u8; 3],
    /// BML versions when default_version_flag = 0.
    pub versions: Option<BmlVersions>,
    /// Carousels (transmission_format '00').
    pub carousels: Vec<CarouselInfo>,
    /// ondemand_retrieval_flag.
    pub ondemand_retrieval: bool,
    /// file_storable_flag.
    pub file_storable: bool,
    /// reserved_future_use (6) at the end, spec value '111111'.
    pub reserved_3: u8,
}

impl AribBxmlInfo {
    /// Decode selector bytes.
    pub fn decode(data: &[u8]) -> Result<Self> {
        let mut r = BitReader::new(data, "arib_bxml_info");
        let transmission_format = r.read_u8(2)?;
        let reserved_1 = r.bool()?;
        let auto_start = r.bool()?;
        let document_resolution = r.read_u8(4)?;
        let use_xml = r.bool()?;
        let default_version_flag = r.bool()?;
        let independent = r.bool()?;
        let content_id_flag = r.bool()?;
        let associated_contents = r.bool()?;
        let reserved_2 = r.bool()?;
        let style_for_tv = r.bool()?;
        let update = r.bool()?;
        let l = r.bytes(3)?;
        let language = [l[0], l[1], l[2]];
        let content_id = if content_id_flag {
            Some((r.u32()?, r.u16()?))
        } else {
            None
        };
        let versions = if default_version_flag {
            None
        } else {
            Some(read_versions(&mut r, use_xml)?)
        };
        let mut carousels = Vec::new();
        let (ondemand_retrieval, file_storable, reserved_3) =
            if transmission_format == TRANSMISSION_FORMAT_CAROUSEL {
                let n = r.u8()? as usize;
                for _ in 0..n {
                    let component_tag = r.u8()?;
                    let event_section = r.bool()?;
                    let reserved = r.read_u8(3)?;
                    let size_f = r.bool()?;
                    let tid_f = r.bool()?;
                    let to_f = r.bool()?;
                    let lr_f = r.bool()?;
                    let component_size = if size_f { Some(r.u32()?) } else { None };
                    let transaction_id = if tid_f { Some(r.u32()?) } else { None };
                    let timeout_dii = if to_f { Some(r.u32()?) } else { None };
                    let leak_rate = if lr_f {
                        Some((r.read_u32(22)?, r.read_u8(2)?))
                    } else {
                        None
                    };
                    carousels.push(CarouselInfo {
                        component_tag,
                        event_section,
                        reserved,
                        component_size,
                        transaction_id,
                        timeout_dii,
                        leak_rate,
                    });
                }
                (r.bool()?, r.bool()?, r.read_u8(6)?)
            } else {
                (false, false, 0)
            };
        r.finish()?;
        Ok(Self {
            transmission_format,
            reserved_1,
            auto_start,
            document_resolution,
            use_xml,
            independent,
            content_id,
            associated_contents,
            reserved_2,
            style_for_tv,
            update,
            language,
            versions,
            carousels,
            ondemand_retrieval,
            file_storable,
            reserved_3,
        })
    }

    /// Encode to selector bytes.
    pub fn encode(&self) -> Result<Vec<u8>> {
        let mut w = BitWriter::new();
        w.write(
            "transmission_format",
            2,
            u64::from(self.transmission_format),
        )?;
        w.bool(self.reserved_1);
        w.bool(self.auto_start);
        w.write(
            "document_resolution",
            4,
            u64::from(self.document_resolution),
        )?;
        w.bool(self.use_xml);
        w.bool(self.versions.is_none());
        w.bool(self.independent);
        w.bool(self.content_id.is_some());
        w.bool(self.associated_contents);
        w.bool(self.reserved_2);
        w.bool(self.style_for_tv);
        w.bool(self.update);
        w.bytes(&self.language);
        if let Some((id, ver)) = self.content_id {
            w.write("content_id", 32, u64::from(id))?;
            w.write("content_version", 16, u64::from(ver))?;
        }
        if let Some(v) = &self.versions {
            write_versions(&mut w, v, self.use_xml)?;
        }
        if self.transmission_format == TRANSMISSION_FORMAT_CAROUSEL {
            w.write("num_of_carousels", 8, self.carousels.len() as u64)?;
            for c in &self.carousels {
                w.write("component_tag", 8, u64::from(c.component_tag))?;
                w.bool(c.event_section);
                w.write("reserved_future_use", 3, u64::from(c.reserved))?;
                w.bool(c.component_size.is_some());
                w.bool(c.transaction_id.is_some());
                w.bool(c.timeout_dii.is_some());
                w.bool(c.leak_rate.is_some());
                if let Some(s) = c.component_size {
                    w.write("component_size", 32, u64::from(s))?;
                }
                if let Some(t) = c.transaction_id {
                    w.write("transaction_id", 32, u64::from(t))?;
                }
                if let Some(t) = c.timeout_dii {
                    w.write("timeout_value_DII", 32, u64::from(t))?;
                }
                if let Some((l, rsv)) = c.leak_rate {
                    w.write("leak_rate", 22, u64::from(l))?;
                    w.write("reserved", 2, u64::from(rsv))?;
                }
            }
            w.bool(self.ondemand_retrieval);
            w.bool(self.file_storable);
            w.write("reserved_future_use", 6, u64::from(self.reserved_3))?;
        }
        Ok(w.into_bytes())
    }
}

/// One entry of `ait_identifier_info()` (Vol. 4 §7.3.2 Table 7-1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AitIdentifier {
    /// application_type carried in the AIT (0x0010 HTML5).
    pub application_type: u16,
    /// transport_type: 0 = the AIT is a file in a data carousel (XML or
    /// section form), 1 = "AIT section transmission" in this component.
    pub section_transport: bool,
    /// application_priority (2): "the start priority of the application
    /// type … When priority is not appointed, it is 00".
    pub application_priority: u8,
    /// AIT_version_number (5): "version_number described in AIT".
    pub ait_version: u8,
}

/// `ait_identifier_info()`: the additional_data_component_info of the data
/// component descriptor of a dedicated AIT component (Vol. 4 §7.3).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AitIdentifierInfo {
    /// Entries (one per application_type).
    pub entries: Vec<AitIdentifier>,
}

impl AitIdentifierInfo {
    /// Decode.
    pub fn decode(data: &[u8]) -> Result<Self> {
        let mut r = BitReader::new(data, "ait_identifier_info");
        let mut entries = Vec::new();
        while r.remaining_bytes() > 0 {
            entries.push(AitIdentifier {
                application_type: r.u16()?,
                section_transport: r.bool()?,
                application_priority: r.read_u8(2)?,
                ait_version: r.read_u8(5)?,
            });
        }
        r.finish()?;
        Ok(Self { entries })
    }

    /// Encode.
    pub fn encode(&self) -> Result<Vec<u8>> {
        let mut w = BitWriter::new();
        for e in &self.entries {
            w.write("application_type", 16, u64::from(e.application_type))?;
            w.bool(e.section_transport);
            w.write("application_priority", 2, u64::from(e.application_priority))?;
            w.write("AIT_version_number", 5, u64::from(e.ait_version))?;
        }
        Ok(w.into_bytes())
    }
}

/// `control_data_byte()` of the DII Control descriptor for BML (Vol. 2
/// §9.3.5 Table 9-15).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BmlControl {
    /// use_xml (the Table 9-15 semantics text reads inverted against Table
    /// 9-12; the value is kept as coded).
    pub use_xml: bool,
    /// reserved (7).
    pub reserved: u8,
    /// bml_major_version, bml_minor_version and, when use_xml = 1, the
    /// B-XML versions.
    pub versions: BmlVersions,
}

impl BmlControl {
    /// Decode control_data_byte.
    pub fn decode(data: &[u8]) -> Result<Self> {
        let mut r = BitReader::new(data, "control_data_byte");
        let use_xml = r.bool()?;
        let reserved = r.read_u8(7)?;
        let versions = read_versions(&mut r, use_xml)?;
        r.finish()?;
        Ok(Self {
            use_xml,
            reserved,
            versions,
        })
    }

    /// Encode control_data_byte.
    pub fn encode(&self) -> Result<Vec<u8>> {
        let mut w = BitWriter::new();
        w.bool(self.use_xml);
        w.write("reserved", 7, u64::from(self.reserved))?;
        write_versions(&mut w, &self.versions, self.use_xml)?;
        Ok(w.into_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn additional_bxml_info_roundtrip() {
        let info = AdditionalAribBxmlInfo {
            transmission_format: 0,
            entry: BxmlEntry::Entry(BxmlEntryPoint {
                auto_start: true,
                document_resolution: 0b0011,
                use_xml: false,
                independent: false,
                style_for_tv: false,
                reserved: 0xF,
                versions: None,
            }),
            carousel: Some(AdditionalAribCarouselInfo {
                data_event_id: 0,
                event_section: false,
                reserved: 0b111,
            }),
            ondemand_retrieval: false,
            file_storable: false,
            reserved: 0x3F,
        };
        let b = info.encode().unwrap();
        // 00 1 1 0011 | 0 1 0 0 1111 | 0000 0 111 | 0 0 111111
        assert_eq!(b, vec![0x33, 0x4F, 0x07, 0x3F]);
        assert_eq!(AdditionalAribBxmlInfo::decode(&b).unwrap(), info);
        let BxmlEntry::Entry(entry) = info.entry.clone() else {
            unreachable!()
        };
        let with_versions = AdditionalAribBxmlInfo {
            entry: BxmlEntry::Entry(BxmlEntryPoint {
                versions: Some(BmlVersions {
                    bml_major: 3,
                    bml_minor: 0,
                    bxml: None,
                }),
                ..entry
            }),
            ..info.clone()
        };
        let b = with_versions.encode().unwrap();
        assert_eq!(b.len(), 8);
        assert_eq!(AdditionalAribBxmlInfo::decode(&b).unwrap(), with_versions);
        let non_entry = AdditionalAribBxmlInfo {
            entry: BxmlEntry::NotEntry { reserved: 0x1F },
            ..info
        };
        let b = non_entry.encode().unwrap();
        assert_eq!(b, vec![0x1F, 0x07, 0x3F]);
        assert_eq!(AdditionalAribBxmlInfo::decode(&b).unwrap(), non_entry);
    }

    #[test]
    fn bxml_info_roundtrip() {
        let info = AribBxmlInfo {
            transmission_format: 0,
            reserved_1: true,
            auto_start: true,
            document_resolution: 0b0011,
            use_xml: false,
            independent: false,
            content_id: None,
            associated_contents: false,
            reserved_2: true,
            style_for_tv: false,
            update: false,
            language: *b"jpn",
            versions: None,
            carousels: vec![CarouselInfo {
                component_tag: 0x40,
                event_section: false,
                reserved: 0b111,
                component_size: Some(12345),
                transaction_id: None,
                timeout_dii: None,
                leak_rate: Some((100, 0b11)),
            }],
            ondemand_retrieval: false,
            file_storable: false,
            reserved_3: 0x3F,
        };
        let b = info.encode().unwrap();
        assert_eq!(AribBxmlInfo::decode(&b).unwrap(), info);
    }

    #[test]
    fn ait_identifier_info_roundtrip() {
        let i = AitIdentifierInfo {
            entries: vec![AitIdentifier {
                application_type: 0x0010,
                section_transport: true,
                application_priority: 0,
                ait_version: 7,
            }],
        };
        let b = i.encode().unwrap();
        assert_eq!(b, vec![0x00, 0x10, 0x87]);
        assert_eq!(AitIdentifierInfo::decode(&b).unwrap(), i);
    }

    #[test]
    fn bml_control_roundtrip() {
        let c = BmlControl {
            use_xml: false,
            reserved: 0x7F,
            versions: BmlVersions {
                bml_major: 1,
                bml_minor: 0,
                bxml: None,
            },
        };
        let b = c.encode().unwrap();
        assert_eq!(b, vec![0x7F, 0, 1, 0, 0]);
        assert_eq!(BmlControl::decode(&b).unwrap(), c);
    }
}

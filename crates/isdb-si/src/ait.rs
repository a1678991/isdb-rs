//! Application Information Table (AIT) and its descriptors (ARIB STD-B24
//! Vol. 4 "Application control" Ch. 5, Tables 5-1 … 5-7; descriptor tags
//! from ARIB STD-B10 Part 1 Table 5-5 "Allocation of tag values of
//! descriptors used in AIT"), the table that tells a Hybridcast receiver
//! which HTML5 application to start (IPTVFJ STD-0010 §7, STD-0013 §5.2).
//!
//! The ARIB AIT differs from the DVB one (ETSI TS 102 809): there is no
//! test_application_flag (`application_type` is a full 16 bits, in the
//! long-section `table_id_extension` position), and the application
//! identifier is `organization_id` (16) + `application_id` (32) (Vol. 4
//! Table 5-2). The descriptor tags live in their own space (Vol. 4 §5.3 /
//! B10 Part 1 Table 5-5), so they are a separate enum from
//! [`crate::descriptor::Descriptor`].
//!
//! Codecs are lossless: reserved fields are kept and every loop length is
//! recomputed on encode.

use crate::dsmcc::{frame, split_loop};
use isdb_ts::bits::{BitReader, BitWriter};
use isdb_ts::section::{Long, LongBody, LongHeader};
use isdb_ts::{Error, Result};

/// table_id of the AIT (Vol. 4 §5.1: "0x74 is given to indicate this
/// table").
pub const TABLE_ID_AIT: u8 = 0x74;
/// application_type "Integrated Broadcast Broadband HTML5 application"
/// (Vol. 4 §5.1 application_type table).
pub const APPLICATION_TYPE_HTML5: u16 = 0x0010;
/// application_type "ARIB-J application".
pub const APPLICATION_TYPE_ARIB_J: u16 = 0x0001;

/// application_control_code values (Vol. 4 §5.1).
pub mod control_code {
    /// AUTOSTART "This indicates to start the application".
    pub const AUTOSTART: u8 = 0x01;
    /// PRESENT "the application is in a feasible state".
    pub const PRESENT: u8 = 0x02;
    /// KILL "This indicates to end the application".
    pub const KILL: u8 = 0x04;
    /// PREFETCH "acquire and hold the application" (not used by
    /// Hybridcast: STD-0013 §5.2.3).
    pub const PREFETCH: u8 = 0x05;
}

/// protocol_id values of the transport protocol descriptor (Vol. 4
/// §5.3.2).
pub mod protocol_id {
    /// HTTP / HTTPS.
    pub const HTTP: u16 = 0x0003;
    /// Data carousel.
    pub const DATA_CAROUSEL: u16 = 0x0004;
}

/// One application profile entry of the application descriptor (Vol. 4
/// Table 5-3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApplicationProfile {
    /// application_profile (16).
    pub profile: u16,
    /// version.major.
    pub major: u8,
    /// version.minor.
    pub minor: u8,
    /// version.micro.
    pub micro: u8,
}

/// 0x00 application_descriptor (Vol. 4 §5.3.1 Table 5-3): "always placed
/// one every application".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplicationDescriptor {
    /// application_profile loop (application_profiles_length implied).
    pub profiles: Vec<ApplicationProfile>,
    /// service_bound_flag: '1' = "the application is related to only
    /// current service, and the application is ended if the change to
    /// other services is made".
    pub service_bound: bool,
    /// visibility (2): '00' invisible, '01' visible to applications only,
    /// '11' visible to the user and applications ('10' reserved).
    pub visibility: u8,
    /// reserved_for_future_use (5).
    pub reserved: u8,
    /// application_priority.
    pub priority: u8,
    /// transport_protocol_label loop.
    pub transport_protocol_labels: Vec<u8>,
}

/// 0x02 transport_protocol_descriptor (Vol. 4 §5.3.2 Table 5-4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransportProtocolDescriptor {
    /// protocol_id ([`protocol_id`]).
    pub protocol_id: u16,
    /// transport_protocol_label.
    pub label: u8,
    /// selector_byte ([`HttpSelector`] for HTTP/HTTPS).
    pub selector: Vec<u8>,
}

/// One URL base with its extensions in the HTTP/HTTPS selector (Vol. 4
/// §5.3.2 "Selector byte for HTTP/HTTPS transport").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpUrl {
    /// URL_base_byte: "the base part of the URL to acquire the
    /// application".
    pub base: Vec<u8>,
    /// URL_extension_byte entries ("the existence of locations that can
    /// acquire the application in the same URL base domain").
    pub extensions: Vec<Vec<u8>>,
}

/// The HTTP/HTTPS selector: a loop of [`HttpUrl`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HttpSelector {
    /// URL bases.
    pub urls: Vec<HttpUrl>,
}

impl HttpSelector {
    /// Decode selector bytes.
    pub fn decode(data: &[u8]) -> Result<Self> {
        let mut r = BitReader::new(data, "HTTP transport selector");
        let mut urls = Vec::new();
        while r.remaining_bytes() > 0 {
            let n = r.u8()? as usize;
            let base = r.bytes(n)?.to_vec();
            let c = r.u8()? as usize;
            let mut extensions = Vec::with_capacity(c);
            for _ in 0..c {
                let l = r.u8()? as usize;
                extensions.push(r.bytes(l)?.to_vec());
            }
            urls.push(HttpUrl { base, extensions });
        }
        Ok(Self { urls })
    }

    /// Encode to selector bytes.
    pub fn encode(&self) -> Result<Vec<u8>> {
        let mut w = BitWriter::new();
        for u in &self.urls {
            w.write("URL_base_length", 8, u.base.len() as u64)?;
            w.bytes(&u.base);
            w.write("URL_extension_count", 8, u.extensions.len() as u64)?;
            for e in &u.extensions {
                w.write("URL_extension_length", 8, e.len() as u64)?;
                w.bytes(e);
            }
        }
        Ok(w.into_bytes())
    }
}

/// 0x15 simple_application_location_descriptor (Vol. 4 §5.3.3 Table 5-5;
/// the tag printed there, 0x05, is a misprint for the B10 Part 1 Table 5-5
/// value 0x15): "one … must be placed every application".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SimpleApplicationLocation {
    /// initial_path_bytes: "the relative path to route the location that
    /// can acquire the application shown by the transmission protocol
    /// descriptor".
    pub initial_path: Vec<u8>,
}

/// One entry of the application boundary and permission descriptor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundaryEntry {
    /// permission_bitmap values ("Higher 3-bit indicates a change of the
    /// bitmap. The assignment of the function bitmap is specified in
    /// operation"; STD-0013: 0x1FFF or 0 with the upper bits '000').
    pub permission_bitmaps: Vec<u16>,
    /// managed_URL entries ("The domain or its subdirectory"); none =
    /// all domains.
    pub managed_urls: Vec<Vec<u8>>,
}

/// 0x30 application_boundary_and_permission_descriptor (Vol. 4 §5.3.4
/// Table 5-6). Absent: "the application boundary becomes infinite and
/// every access to the broadcast resource is admitted".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundaryAndPermission {
    /// Entries.
    pub entries: Vec<BoundaryEntry>,
}

/// A descriptor of an AIT descriptor loop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AitDescriptor {
    /// 0x00 application_descriptor.
    Application(ApplicationDescriptor),
    /// 0x02 transport_protocol_descriptor.
    TransportProtocol(TransportProtocolDescriptor),
    /// 0x15 simple_application_location_descriptor.
    SimpleApplicationLocation(SimpleApplicationLocation),
    /// 0x30 application_boundary_and_permission_descriptor.
    BoundaryAndPermission(BoundaryAndPermission),
    /// 0x31 autostart_priority_descriptor (Vol. 4 §5.3.5 Table 5-7): the
    /// start priority against the data broadcasting of the service; absent
    /// = "the priority is the lowest including data broadcasting".
    AutostartPriority {
        /// autostart_priority.
        priority: u8,
    },
    /// Any other tag (0x32 cache control, 0x33 randomized latency, 0x34
    /// external application control, 0x35–0x37 playback / expiration,
    /// company private), kept raw.
    Unknown {
        /// descriptor_tag.
        tag: u8,
        /// descriptor body.
        data: Vec<u8>,
    },
}

impl AitDescriptor {
    /// descriptor_tag.
    pub fn tag(&self) -> u8 {
        match self {
            AitDescriptor::Application(_) => 0x00,
            AitDescriptor::TransportProtocol(_) => 0x02,
            AitDescriptor::SimpleApplicationLocation(_) => 0x15,
            AitDescriptor::BoundaryAndPermission(_) => 0x30,
            AitDescriptor::AutostartPriority { .. } => 0x31,
            AitDescriptor::Unknown { tag, .. } => *tag,
        }
    }

    /// Decode one body given its tag.
    pub fn decode_body(tag: u8, data: &[u8]) -> Result<Self> {
        const WHAT: &str = "AIT descriptor";
        let mut r = BitReader::new(data, WHAT);
        let d = match tag {
            0x00 => {
                let pl = r.u8()? as usize;
                if !pl.is_multiple_of(5) {
                    return Err(Error::invalid(
                        WHAT,
                        format!("application_profiles_length {pl} is not a multiple of 5"),
                    ));
                }
                let mut profiles = Vec::with_capacity(pl / 5);
                for _ in 0..pl / 5 {
                    profiles.push(ApplicationProfile {
                        profile: r.u16()?,
                        major: r.u8()?,
                        minor: r.u8()?,
                        micro: r.u8()?,
                    });
                }
                let service_bound = r.bool()?;
                let visibility = r.read(2)? as u8;
                let reserved = r.read(5)? as u8;
                let priority = r.u8()?;
                let transport_protocol_labels = r.rest()?.to_vec();
                AitDescriptor::Application(ApplicationDescriptor {
                    profiles,
                    service_bound,
                    visibility,
                    reserved,
                    priority,
                    transport_protocol_labels,
                })
            }
            0x02 => AitDescriptor::TransportProtocol(TransportProtocolDescriptor {
                protocol_id: r.u16()?,
                label: r.u8()?,
                selector: r.rest()?.to_vec(),
            }),
            0x15 => AitDescriptor::SimpleApplicationLocation(SimpleApplicationLocation {
                initial_path: r.rest()?.to_vec(),
            }),
            0x30 => {
                let mut entries = Vec::new();
                while r.remaining_bytes() > 0 {
                    let pc = r.u8()? as usize;
                    let mut permission_bitmaps = Vec::with_capacity(pc);
                    for _ in 0..pc {
                        permission_bitmaps.push(r.u16()?);
                    }
                    let uc = r.u8()? as usize;
                    let mut managed_urls = Vec::with_capacity(uc);
                    for _ in 0..uc {
                        let l = r.u8()? as usize;
                        managed_urls.push(r.bytes(l)?.to_vec());
                    }
                    entries.push(BoundaryEntry {
                        permission_bitmaps,
                        managed_urls,
                    });
                }
                AitDescriptor::BoundaryAndPermission(BoundaryAndPermission { entries })
            }
            0x31 => AitDescriptor::AutostartPriority { priority: r.u8()? },
            _ => {
                let data = r.rest()?.to_vec();
                AitDescriptor::Unknown { tag, data }
            }
        };
        r.finish()?;
        Ok(d)
    }

    fn encode_body(&self, out: &mut Vec<u8>) -> Result<()> {
        match self {
            AitDescriptor::Application(a) => {
                let mut w = BitWriter::new();
                w.write(
                    "application_profiles_length",
                    8,
                    (a.profiles.len() * 5) as u64,
                )?;
                for p in &a.profiles {
                    w.write("application_profile", 16, p.profile as u64)?;
                    w.write("version.major", 8, p.major as u64)?;
                    w.write("version.minor", 8, p.minor as u64)?;
                    w.write("version.micro", 8, p.micro as u64)?;
                }
                w.bool(a.service_bound);
                w.write("visibility", 2, a.visibility as u64)?;
                w.write("reserved_for_future_use", 5, a.reserved as u64)?;
                w.write("application_priority", 8, a.priority as u64)?;
                w.bytes(&a.transport_protocol_labels);
                out.extend_from_slice(&w.into_bytes());
            }
            AitDescriptor::TransportProtocol(t) => {
                out.extend_from_slice(&t.protocol_id.to_be_bytes());
                out.push(t.label);
                out.extend_from_slice(&t.selector);
            }
            AitDescriptor::SimpleApplicationLocation(l) => out.extend_from_slice(&l.initial_path),
            AitDescriptor::BoundaryAndPermission(b) => {
                let mut w = BitWriter::new();
                for e in &b.entries {
                    w.write(
                        "permission_bitmap_count",
                        8,
                        e.permission_bitmaps.len() as u64,
                    )?;
                    for p in &e.permission_bitmaps {
                        w.write("permission_bitmap", 16, *p as u64)?;
                    }
                    w.write("managed_URL_count", 8, e.managed_urls.len() as u64)?;
                    for u in &e.managed_urls {
                        w.write("managed_URL_length", 8, u.len() as u64)?;
                        w.bytes(u);
                    }
                }
                out.extend_from_slice(&w.into_bytes());
            }
            AitDescriptor::AutostartPriority { priority } => out.push(*priority),
            AitDescriptor::Unknown { data, .. } => out.extend_from_slice(data),
        }
        Ok(())
    }

    /// Append tag, length and body.
    pub fn encode(&self, out: &mut Vec<u8>) -> Result<()> {
        let mut body = Vec::new();
        self.encode_body(&mut body)?;
        frame(self.tag(), &body, out)
    }
}

/// Decode an AIT descriptor loop.
pub fn decode_ait_descriptors(bytes: &[u8]) -> Result<Vec<AitDescriptor>> {
    split_loop(bytes, "AIT descriptor loop")?
        .into_iter()
        .map(|(t, b)| AitDescriptor::decode_body(t, b))
        .collect()
}

/// Encode an AIT descriptor loop.
pub fn encode_ait_descriptors(ds: &[AitDescriptor]) -> Result<Vec<u8>> {
    let mut v = Vec::new();
    for d in ds {
        d.encode(&mut v)?;
    }
    Ok(v)
}

/// One application of the AIT application loop (Vol. 4 Table 5-1 with the
/// application_identifier of Table 5-2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AitApplication {
    /// organization_id (16): "the organization which made application"
    /// (Hybridcast: assigned by the IPTV Forum; 0 is never sent, STD-0013
    /// §5.2.1).
    pub organization_id: u16,
    /// application_id (32): unique within the organization.
    pub application_id: u32,
    /// application_control_code ([`control_code`]).
    pub control_code: u8,
    /// reserved_for_future_use (4).
    pub reserved: u8,
    /// Application descriptor loop.
    pub descriptors: Vec<AitDescriptor>,
}

/// Body of an application_information_section (Vol. 4 Table 5-1). The
/// `application_type` is the section header's `table_id_extension`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AitBody {
    /// reserved_for_future_use (4) before common_descriptors_length.
    pub reserved_1: u8,
    /// Common descriptor loop ("applied to all application in AIT
    /// subtable").
    pub common_descriptors: Vec<AitDescriptor>,
    /// reserved_for_future_use (4) before application_loop_length.
    pub reserved_2: u8,
    /// Application loop.
    pub applications: Vec<AitApplication>,
}

impl AitBody {
    /// An empty AIT body with the reserved bits '1111'.
    pub fn new() -> Self {
        Self {
            reserved_1: 0xF,
            common_descriptors: Vec::new(),
            reserved_2: 0xF,
            applications: Vec::new(),
        }
    }

    /// The AIT section for `application_type` (table_id 0x74,
    /// section_syntax_indicator 1, current_next_indicator 1).
    pub fn section(self, application_type: u16, version: u8) -> Ait {
        Long {
            header: LongHeader::new(TABLE_ID_AIT, application_type, version & 0x1F),
            body: self,
        }
    }
}

impl LongBody for AitBody {
    fn accepts_table_id(t: u8) -> bool {
        t == TABLE_ID_AIT
    }
    fn decode(_: &LongHeader, body: &[u8]) -> Result<Self> {
        let mut r = BitReader::new(body, "application_information_section");
        let reserved_1 = r.read(4)? as u8;
        let cl = r.read(12)? as usize;
        let common_descriptors = decode_ait_descriptors(r.bytes(cl)?)?;
        let reserved_2 = r.read(4)? as u8;
        let al = r.read(12)? as usize;
        let apps = r.bytes(al)?;
        r.finish()?;
        let mut ar = BitReader::new(apps, "AIT application loop");
        let mut applications = Vec::new();
        while ar.remaining_bytes() > 0 {
            let organization_id = ar.u16()?;
            let application_id = ar.u32()?;
            let control_code = ar.u8()?;
            let reserved = ar.read(4)? as u8;
            let dl = ar.read(12)? as usize;
            let descriptors = decode_ait_descriptors(ar.bytes(dl)?)?;
            applications.push(AitApplication {
                organization_id,
                application_id,
                control_code,
                reserved,
                descriptors,
            });
        }
        Ok(Self {
            reserved_1,
            common_descriptors,
            reserved_2,
            applications,
        })
    }
    fn encode(&self, out: &mut Vec<u8>) -> Result<()> {
        let mut apps = BitWriter::new();
        for a in &self.applications {
            apps.write("organization_id", 16, a.organization_id as u64)?;
            apps.write("application_id", 32, a.application_id as u64)?;
            apps.write("application_control_code", 8, a.control_code as u64)?;
            apps.write("reserved_for_future_use", 4, a.reserved as u64)?;
            let d = encode_ait_descriptors(&a.descriptors)?;
            apps.write("application_descriptors_loop_length", 12, d.len() as u64)?;
            apps.bytes(&d);
        }
        let apps = apps.into_bytes();
        let common = encode_ait_descriptors(&self.common_descriptors)?;
        let mut w = BitWriter::new();
        w.write("reserved_for_future_use", 4, self.reserved_1 as u64)?;
        w.write("common_descriptors_length", 12, common.len() as u64)?;
        w.bytes(&common);
        w.write("reserved_for_future_use", 4, self.reserved_2 as u64)?;
        w.write("application_loop_length", 12, apps.len() as u64)?;
        w.bytes(&apps);
        out.extend_from_slice(&w.into_bytes());
        Ok(())
    }
}

/// Application Information Table section (table_id 0x74).
pub type Ait = Long<AitBody>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ait_roundtrip() {
        let mut body = AitBody::new();
        body.common_descriptors
            .push(AitDescriptor::TransportProtocol(
                TransportProtocolDescriptor {
                    protocol_id: protocol_id::HTTP,
                    label: 0,
                    selector: HttpSelector {
                        urls: vec![HttpUrl {
                            base: b"https://example.com/".to_vec(),
                            extensions: vec![b"a/".to_vec()],
                        }],
                    }
                    .encode()
                    .unwrap(),
                },
            ));
        body.applications.push(AitApplication {
            organization_id: 16,
            application_id: 0x0000_0001,
            control_code: control_code::AUTOSTART,
            reserved: 0xF,
            descriptors: vec![
                AitDescriptor::Application(ApplicationDescriptor {
                    profiles: vec![ApplicationProfile {
                        profile: 0x0002,
                        major: 1,
                        minor: 1,
                        micro: 1,
                    }],
                    service_bound: true,
                    visibility: 0b11,
                    reserved: 0x1F,
                    priority: 1,
                    transport_protocol_labels: vec![0],
                }),
                AitDescriptor::SimpleApplicationLocation(SimpleApplicationLocation {
                    initial_path: b"index.html".to_vec(),
                }),
                AitDescriptor::BoundaryAndPermission(BoundaryAndPermission {
                    entries: vec![BoundaryEntry {
                        permission_bitmaps: vec![0x1FFF],
                        managed_urls: vec![b"https://example.com".to_vec()],
                    }],
                }),
                AitDescriptor::AutostartPriority { priority: 1 },
                AitDescriptor::Unknown {
                    tag: 0x80,
                    data: vec![1, 2, 3],
                },
            ],
        });
        let sec = body.clone().section(APPLICATION_TYPE_HTML5, 3);
        let bytes = sec.encode().unwrap();
        assert_eq!(bytes[0], 0x74);
        assert_eq!(&bytes[3..5], &[0x00, 0x10]);
        let back = Ait::decode(&bytes).unwrap();
        assert_eq!(back.body, body);
        assert_eq!(back.encode().unwrap(), bytes);
        // application_identifier is 6 bytes: organization_id(16) application_id(32)
        let apps_start = 8 + 2 + 2;
        let _ = apps_start;
        let AitDescriptor::TransportProtocol(t) = &back.body.common_descriptors[0] else {
            panic!()
        };
        let sel = HttpSelector::decode(&t.selector).unwrap();
        assert_eq!(sel.urls[0].base, b"https://example.com/");
        assert_eq!(sel.urls[0].extensions, vec![b"a/".to_vec()]);
    }

    #[test]
    fn application_profiles_length_must_be_multiple_of_5() {
        assert!(AitDescriptor::decode_body(0x00, &[4, 0, 2, 1, 1, 0x80, 1]).is_err());
    }
}

//! MMT-SI tables (ARIB STD-B60 v1.14 §7.3.3) and TLV-SI tables (§5.2):
//! the MPT and PLT of PA messages, the section-format tables of M2
//! section / short section messages (MH-EIT, MH-SDT, MH-TOT, MH-BIT, ECM,
//! EMM, CAT), and the TLV-NIT and AMT of transmission-control packets.
//!
//! Section-format tables are checked against their CRC-32 (MPEG-2 CRC,
//! §7.2.3.2 "shall follow ITU-T Recommendation H.220"; MH-TOT as a short
//! section carries one too, Table 7-25) before they are decoded.

use crate::desc::{self, Descriptor};
use isdb_si::time::{BcdDuration, MjdTime};
use isdb_ts::Pid;
use isdb_ts::crc::crc32;

/// `MMT_general_location_info` (Table 7-9).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Location {
    /// 00h: MMTP packet `packet_id` in the same IP data flow.
    SameFlow(u16),
    /// 01h: MMTP packet in an IPv4 data flow.
    Ipv4 {
        /// Source address.
        src: [u8; 4],
        /// Destination address.
        dst: [u8; 4],
        /// Destination port.
        port: u16,
        /// MMTP packet id.
        packet_id: u16,
    },
    /// 02h: MMTP packet in an IPv6 data flow.
    Ipv6 {
        /// Source address.
        src: [u8; 16],
        /// Destination address.
        dst: [u8; 16],
        /// Destination port.
        port: u16,
        /// MMTP packet id.
        packet_id: u16,
    },
    /// 03h: an MPEG-2 TS packet of a broadcast network.
    Mpeg2Ts {
        /// Network id.
        network_id: u16,
        /// Transport stream id.
        ts_id: u16,
        /// PID.
        pid: Pid,
    },
    /// 04h: an MPEG-2 TS packet in an IPv6 data flow.
    Mpeg2TsIpv6 {
        /// Destination port.
        port: u16,
        /// PID.
        pid: Pid,
    },
    /// 05h: a URL.
    Url(String),
}

impl Location {
    /// Parse one location; returns it and the bytes it took.
    pub fn parse(b: &[u8]) -> Option<(Location, usize)> {
        let u16at = |p: usize| Some(u16::from_be_bytes([*b.get(p)?, *b.get(p + 1)?]));
        Some(match *b.first()? {
            0x00 => (Location::SameFlow(u16at(1)?), 3),
            0x01 => (
                Location::Ipv4 {
                    src: b.get(1..5)?.try_into().ok()?,
                    dst: b.get(5..9)?.try_into().ok()?,
                    port: u16at(9)?,
                    packet_id: u16at(11)?,
                },
                13,
            ),
            0x02 => (
                Location::Ipv6 {
                    src: b.get(1..17)?.try_into().ok()?,
                    dst: b.get(17..33)?.try_into().ok()?,
                    port: u16at(33)?,
                    packet_id: u16at(35)?,
                },
                37,
            ),
            0x03 => (
                Location::Mpeg2Ts {
                    network_id: u16at(1)?,
                    ts_id: u16at(3)?,
                    pid: Pid::from_wire(u16at(5)?),
                },
                7,
            ),
            0x04 => (
                Location::Mpeg2TsIpv6 {
                    port: u16at(33)?,
                    pid: Pid::from_wire(u16at(35)?),
                },
                37,
            ),
            0x05 => {
                let n = usize::from(*b.get(1)?);
                (
                    Location::Url(String::from_utf8_lossy(b.get(2..2 + n)?).into_owned()),
                    2 + n,
                )
            }
            _ => return None,
        })
    }

    /// The MMTP packet id this location designates, if any.
    pub fn packet_id(&self) -> Option<u16> {
        match self {
            Location::SameFlow(p) => Some(*p),
            Location::Ipv4 { packet_id, .. } | Location::Ipv6 { packet_id, .. } => Some(*packet_id),
            _ => None,
        }
    }
}

/// One asset of an MPT (Table 7-6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asset {
    /// `identifier_type`.
    pub identifier_type: u8,
    /// `asset_id_scheme`.
    pub asset_id_scheme: u32,
    /// `asset_id_byte`s.
    pub asset_id: Vec<u8>,
    /// `asset_type`, four characters (Table 7-8: hev1, hvc1, mp4a, stpp,
    /// aapp, asgd, aagd).
    pub asset_type: [u8; 4],
    /// `asset_clock_relation_flag`.
    pub clock_relation: bool,
    /// Its locations.
    pub locations: Vec<Location>,
    /// Its descriptors.
    pub descriptors: Vec<Descriptor>,
}

impl Asset {
    /// The asset type as text.
    pub fn type_str(&self) -> String {
        String::from_utf8_lossy(&self.asset_type).into_owned()
    }

    /// The MMTP packet id of its first location.
    pub fn packet_id(&self) -> Option<u16> {
        self.locations.iter().find_map(Location::packet_id)
    }

    /// Its component tag (MH-stream identifier descriptor).
    pub fn component_tag(&self) -> Option<u16> {
        self.descriptors.iter().find_map(|d| match d {
            Descriptor::StreamIdentifier { component_tag } => Some(*component_tag),
            _ => None,
        })
    }
}

/// An MMT package table (Table 7-6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mpt {
    /// `table_id` (20h complete, 11h–1Fh subset).
    pub table_id: u8,
    /// `version`.
    pub version: u8,
    /// `MPT_mode`.
    pub mode: u8,
    /// `MMT_package_id_byte`s; the low 16 bits are the service id.
    pub package_id: Vec<u8>,
    /// MPT descriptors.
    pub descriptors: Vec<Descriptor>,
    /// Assets.
    pub assets: Vec<Asset>,
}

impl Mpt {
    /// Parse an MPT from its table bytes (table_id onwards).
    pub fn parse(b: &[u8]) -> Option<Mpt> {
        let table_id = *b.first()?;
        let version = *b.get(1)?;
        let len = usize::from(u16::from_be_bytes([*b.get(2)?, *b.get(3)?]));
        let b = b.get(..4 + len)?;
        let mode = *b.get(4)? & 0x03;
        let pl = usize::from(*b.get(5)?);
        let package_id = b.get(6..6 + pl)?.to_vec();
        let mut p = 6 + pl;
        let dl = usize::from(u16::from_be_bytes([*b.get(p)?, *b.get(p + 1)?]));
        let descriptors = desc::decode_all(b.get(p + 2..p + 2 + dl)?);
        p += 2 + dl;
        let n = usize::from(*b.get(p)?);
        p += 1;
        let mut assets = Vec::with_capacity(n);
        for _ in 0..n {
            let identifier_type = *b.get(p)?;
            let asset_id_scheme = u32::from_be_bytes(b.get(p + 1..p + 5)?.try_into().ok()?);
            let il = usize::from(*b.get(p + 5)?);
            let asset_id = b.get(p + 6..p + 6 + il)?.to_vec();
            p += 6 + il;
            let asset_type: [u8; 4] = b.get(p..p + 4)?.try_into().ok()?;
            let clock_relation = *b.get(p + 4)? & 1 != 0;
            let lc = usize::from(*b.get(p + 5)?);
            p += 6;
            let mut locations = Vec::with_capacity(lc);
            for _ in 0..lc {
                let (l, used) = Location::parse(b.get(p..)?)?;
                locations.push(l);
                p += used;
            }
            let al = usize::from(u16::from_be_bytes([*b.get(p)?, *b.get(p + 1)?]));
            let descriptors = desc::decode_all(b.get(p + 2..p + 2 + al)?);
            p += 2 + al;
            assets.push(Asset {
                identifier_type,
                asset_id_scheme,
                asset_id,
                asset_type,
                clock_relation,
                locations,
                descriptors,
            });
        }
        Some(Mpt {
            table_id,
            version,
            mode,
            package_id,
            descriptors,
            assets,
        })
    }

    /// The service id: the package id's low 16 bits (§7.3.3.1).
    pub fn service_id(&self) -> Option<u16> {
        let n = self.package_id.len();
        (n >= 2).then(|| u16::from_be_bytes([self.package_id[n - 2], self.package_id[n - 1]]))
    }
}

/// A package list table (Table 7-11): where each package's PA message
/// travels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plt {
    /// `version`.
    pub version: u8,
    /// (package id, location of its PA message).
    pub packages: Vec<(Vec<u8>, Location)>,
    /// IP services: (transport file id, location type).
    pub ip_deliveries: Vec<(u32, u8)>,
}

impl Plt {
    /// Parse a PLT from its table bytes.
    pub fn parse(b: &[u8]) -> Option<Plt> {
        let version = *b.get(1)?;
        let len = usize::from(u16::from_be_bytes([*b.get(2)?, *b.get(3)?]));
        let b = b.get(..4 + len)?;
        let n = usize::from(*b.get(4)?);
        let mut p = 5;
        let mut packages = Vec::with_capacity(n);
        for _ in 0..n {
            let l = usize::from(*b.get(p)?);
            let id = b.get(p + 1..p + 1 + l)?.to_vec();
            let (loc, used) = Location::parse(b.get(p + 1 + l..)?)?;
            packages.push((id, loc));
            p += 1 + l + used;
        }
        let mut ip_deliveries = Vec::new();
        if let Some(&m) = b.get(p) {
            p += 1;
            for _ in 0..m {
                let id = u32::from_be_bytes(b.get(p..p + 4)?.try_into().ok()?);
                let lt = *b.get(p + 4)?;
                p += 5 + match lt {
                    0x01 => 10,
                    0x02 => 34,
                    0x05 => 1 + usize::from(*b.get(p + 5)?),
                    _ => return None,
                };
                let dl = usize::from(u16::from_be_bytes([*b.get(p)?, *b.get(p + 1)?]));
                p += 2 + dl;
                ip_deliveries.push((id, lt));
            }
        }
        Some(Plt {
            version,
            packages,
            ip_deliveries,
        })
    }
}

/// The long-form section header shared by MH-EIT, MH-SDT, MH-BIT, ECM,
/// EMM, CAT, TLV-NIT and AMT.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SectionHeader {
    /// `table_id`.
    pub table_id: u8,
    /// `table_id_extension` (service id, TLV stream id, network id …).
    pub extension: u16,
    /// `version_number`.
    pub version: u8,
    /// `current_next_indicator`.
    pub current: bool,
    /// `section_number`.
    pub section_number: u8,
    /// `last_section_number`.
    pub last_section_number: u8,
}

/// Why a section was not decoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SectionError {
    /// Shorter than its header or its `section_length`.
    Truncated,
    /// The CRC-32 does not match.
    Crc,
    /// The body does not follow the table's structure.
    Malformed,
}

/// Check a long-form section (CRC over the whole section) and read its
/// header; returns the header and the bytes between the header and the
/// CRC.
pub fn long_section(b: &[u8]) -> Result<(SectionHeader, &[u8]), SectionError> {
    if b.len() < 3 {
        return Err(SectionError::Truncated);
    }
    let len = (usize::from(b[1] & 0x0F) << 8) | usize::from(b[2]);
    if len < 9 || b.len() < 3 + len {
        return Err(SectionError::Truncated);
    }
    let s = &b[..3 + len];
    if crc32(s) != 0 {
        return Err(SectionError::Crc);
    }
    Ok((
        SectionHeader {
            table_id: s[0],
            extension: u16::from_be_bytes([s[3], s[4]]),
            version: (s[5] >> 1) & 0x1F,
            current: s[5] & 1 != 0,
            section_number: s[6],
            last_section_number: s[7],
        },
        &s[8..s.len() - 4],
    ))
}

fn u16_at(b: &[u8], p: usize) -> Result<u16, SectionError> {
    b.get(p..p + 2)
        .map(|x| u16::from_be_bytes([x[0], x[1]]))
        .ok_or(SectionError::Malformed)
}

fn len12(b: &[u8], p: usize) -> Result<usize, SectionError> {
    Ok(usize::from(u16_at(b, p)? & 0x0FFF))
}

fn slice(b: &[u8], from: usize, n: usize) -> Result<&[u8], SectionError> {
    b.get(from..from + n).ok_or(SectionError::Malformed)
}

/// One event of an MH-EIT (Table 7-18).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    /// `event_id`.
    pub event_id: u16,
    /// `start_time` (JST, MJD + BCD).
    pub start: MjdTime,
    /// `duration` (BCD).
    pub duration: BcdDuration,
    /// `running_status`.
    pub running_status: u8,
    /// `free_CA_mode`.
    pub free_ca: bool,
    /// Its descriptors.
    pub descriptors: Vec<Descriptor>,
}

impl Event {
    /// The MH-short event descriptor's name and text, if present.
    pub fn short_event(&self) -> Option<(&str, &str)> {
        self.descriptors.iter().find_map(|d| match d {
            Descriptor::ShortEvent { name, text, .. } => Some((name.as_str(), text.as_str())),
            _ => None,
        })
    }
}

/// One MH-EIT section (Table 7-18).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Eit {
    /// The section header (`extension` = service id).
    pub header: SectionHeader,
    /// `tlv_stream_id`.
    pub tlv_stream_id: u16,
    /// `original_network_id`.
    pub original_network_id: u16,
    /// `segment_last_section_number`.
    pub segment_last_section_number: u8,
    /// `last_table_id`.
    pub last_table_id: u8,
    /// Events.
    pub events: Vec<Event>,
}

impl Eit {
    /// Decode an MH-EIT section (table id 8Bh p/f, 8Ch–9Bh schedule).
    pub fn parse(b: &[u8]) -> Result<Eit, SectionError> {
        let (header, body) = long_section(b)?;
        let tlv_stream_id = u16_at(body, 0)?;
        let original_network_id = u16_at(body, 2)?;
        let segment_last_section_number = *body.get(4).ok_or(SectionError::Malformed)?;
        let last_table_id = *body.get(5).ok_or(SectionError::Malformed)?;
        let mut p = 6;
        let mut events = Vec::new();
        while p < body.len() {
            let h = slice(body, p, 12)?;
            let mut st = [0u8; 8];
            st[3..].copy_from_slice(&h[2..7]);
            let dl = len12(h, 10)?;
            events.push(Event {
                event_id: u16::from_be_bytes([h[0], h[1]]),
                start: MjdTime::from_u40(u64::from_be_bytes(st)),
                duration: BcdDuration(u32::from_be_bytes([0, h[7], h[8], h[9]])),
                running_status: h[10] >> 5,
                free_ca: h[10] & 0x10 != 0,
                descriptors: desc::decode_all(slice(body, p + 12, dl)?),
            });
            p += 12 + dl;
        }
        Ok(Eit {
            header,
            tlv_stream_id,
            original_network_id,
            segment_last_section_number,
            last_table_id,
            events,
        })
    }
}

/// One service of an MH-SDT (Table 7-23).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SdtService {
    /// `service_id`.
    pub service_id: u16,
    /// `EIT_user_defined_flags`.
    pub eit_user_defined_flags: u8,
    /// `EIT_schedule_flag`.
    pub eit_schedule: bool,
    /// `EIT_present_following_flag`.
    pub eit_present_following: bool,
    /// `running_status`.
    pub running_status: u8,
    /// `free_CA_mode`.
    pub free_ca: bool,
    /// Its descriptors.
    pub descriptors: Vec<Descriptor>,
}

impl SdtService {
    /// The MH-service descriptor's type, provider and name.
    pub fn service(&self) -> Option<(u8, &str, &str)> {
        self.descriptors.iter().find_map(|d| match d {
            Descriptor::Service {
                service_type,
                provider,
                name,
            } => Some((*service_type, provider.as_str(), name.as_str())),
            _ => None,
        })
    }
}

/// One MH-SDT section (Table 7-23).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sdt {
    /// The section header (`extension` = TLV stream id).
    pub header: SectionHeader,
    /// `original_network_id`.
    pub original_network_id: u16,
    /// Services.
    pub services: Vec<SdtService>,
}

impl Sdt {
    /// Decode an MH-SDT section (9Fh actual, A0h other).
    pub fn parse(b: &[u8]) -> Result<Sdt, SectionError> {
        let (header, body) = long_section(b)?;
        let original_network_id = u16_at(body, 0)?;
        let mut p = 3;
        let mut services = Vec::new();
        while p < body.len() {
            let h = slice(body, p, 5)?;
            let dl = len12(h, 3)?;
            services.push(SdtService {
                service_id: u16::from_be_bytes([h[0], h[1]]),
                eit_user_defined_flags: (h[2] >> 2) & 7,
                eit_schedule: h[2] & 2 != 0,
                eit_present_following: h[2] & 1 != 0,
                running_status: h[3] >> 5,
                free_ca: h[3] & 0x10 != 0,
                descriptors: desc::decode_all(slice(body, p + 5, dl)?),
            });
            p += 5 + dl;
        }
        Ok(Sdt {
            header,
            original_network_id,
            services,
        })
    }
}

/// An MH-TOT (Table 7-25): the current JST time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tot {
    /// `JST_time`.
    pub jst: MjdTime,
    /// Its descriptors.
    pub descriptors: Vec<Descriptor>,
}

impl Tot {
    /// Decode an MH-TOT short section (table id A1h; CRC over the whole
    /// section).
    pub fn parse(b: &[u8]) -> Result<Tot, SectionError> {
        if b.len() < 3 {
            return Err(SectionError::Truncated);
        }
        let len = (usize::from(b[1] & 0x0F) << 8) | usize::from(b[2]);
        if len < 11 || b.len() < 3 + len {
            return Err(SectionError::Truncated);
        }
        let s = &b[..3 + len];
        if crc32(s) != 0 {
            return Err(SectionError::Crc);
        }
        let mut st = [0u8; 8];
        st[3..].copy_from_slice(&s[3..8]);
        let dl = len12(s, 8)?;
        Ok(Tot {
            jst: MjdTime::from_u40(u64::from_be_bytes(st)),
            descriptors: desc::decode_all(slice(s, 10, dl)?),
        })
    }
}

/// One MH-BIT section (Table 7-21).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bit {
    /// The section header (`extension` = original network id).
    pub header: SectionHeader,
    /// `broadcast_view_propriety`.
    pub broadcast_view_propriety: bool,
    /// First-loop descriptors.
    pub descriptors: Vec<Descriptor>,
    /// (broadcaster id, descriptors).
    pub broadcasters: Vec<(u8, Vec<Descriptor>)>,
}

impl Bit {
    /// Decode an MH-BIT section (table id 9Dh).
    pub fn parse(b: &[u8]) -> Result<Bit, SectionError> {
        let (header, body) = long_section(b)?;
        let first = u16_at(body, 0)?;
        let fl = usize::from(first & 0x0FFF);
        let descriptors = desc::decode_all(slice(body, 2, fl)?);
        let mut p = 2 + fl;
        let mut broadcasters = Vec::new();
        while p < body.len() {
            let id = *body.get(p).ok_or(SectionError::Malformed)?;
            let dl = len12(body, p + 1)?;
            broadcasters.push((id, desc::decode_all(slice(body, p + 3, dl)?)));
            p += 3 + dl;
        }
        Ok(Bit {
            header,
            broadcast_view_propriety: first & 0x1000 != 0,
            descriptors,
            broadcasters,
        })
    }
}

/// TLV-SI descriptor tags (Table 4-4) decoded here: 8-bit tag, 8-bit
/// length (§5.3).
pub mod tlv_tag {
    /// Service list descriptor (Table 5-4).
    pub const SERVICE_LIST: u8 = 0x41;
    /// Satellite delivery system descriptor (Table 5-6).
    pub const SATELLITE_DELIVERY: u8 = 0x43;
    /// Network name descriptor.
    pub const NETWORK_NAME: u8 = 0x40;
}

/// A TLV-SI descriptor (8-bit tag and length, §5.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TlvDescriptor {
    /// `descriptor_tag`.
    pub tag: u8,
    /// The body.
    pub body: Vec<u8>,
}

impl TlvDescriptor {
    /// Split a TLV-SI descriptor loop.
    pub fn split(b: &[u8]) -> Vec<TlvDescriptor> {
        let mut out = Vec::new();
        let mut p = 0;
        while let (Some(&tag), Some(&len)) = (b.get(p), b.get(p + 1)) {
            let Some(body) = b.get(p + 2..p + 2 + usize::from(len)) else {
                break;
            };
            out.push(TlvDescriptor {
                tag,
                body: body.to_vec(),
            });
            p += 2 + usize::from(len);
        }
        out
    }

    /// A service list descriptor's (service id, service type) pairs.
    pub fn service_list(&self) -> Option<Vec<(u16, u8)>> {
        (self.tag == tlv_tag::SERVICE_LIST).then(|| {
            self.body
                .as_chunks::<3>()
                .0
                .iter()
                .map(|c| (u16::from_be_bytes([c[0], c[1]]), c[2]))
                .collect()
        })
    }

    /// A satellite delivery system descriptor's frequency in kHz (eight
    /// BCD digits, unit 10 kHz: Table 5-6) and its symbol rate field.
    pub fn satellite_frequency_khz(&self) -> Option<u64> {
        if self.tag != tlv_tag::SATELLITE_DELIVERY || self.body.len() < 4 {
            return None;
        }
        let mut v = 0u64;
        for &byte in &self.body[..4] {
            for nib in [byte >> 4, byte & 0x0F] {
                if nib > 9 {
                    return None;
                }
                v = v * 10 + u64::from(nib);
            }
        }
        Some(v * 10)
    }

    /// A network name descriptor's text (UTF-8, as MMT-SI text: SPEC-NOTES
    /// "tlv-si-text").
    pub fn network_name(&self) -> Option<String> {
        (self.tag == tlv_tag::NETWORK_NAME).then(|| crate::desc::text(&self.body))
    }
}

/// One TLV stream of a TLV-NIT.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NitStream {
    /// `tlv_stream_id`.
    pub tlv_stream_id: u16,
    /// `original_network_id`.
    pub original_network_id: u16,
    /// Its descriptors.
    pub descriptors: Vec<TlvDescriptor>,
}

/// One TLV-NIT section (Table 5-1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Nit {
    /// The section header (`extension` = network id).
    pub header: SectionHeader,
    /// Network descriptors.
    pub descriptors: Vec<TlvDescriptor>,
    /// TLV streams.
    pub streams: Vec<NitStream>,
}

impl Nit {
    /// Decode a TLV-NIT section (40h actual, 41h other).
    pub fn parse(b: &[u8]) -> Result<Nit, SectionError> {
        let (header, body) = long_section(b)?;
        let nl = len12(body, 0)?;
        let descriptors = TlvDescriptor::split(slice(body, 2, nl)?);
        let ll = len12(body, 2 + nl)?;
        let lp = slice(body, 4 + nl, ll)?;
        let mut streams = Vec::new();
        let mut p = 0;
        while p < lp.len() {
            let h = slice(lp, p, 6)?;
            let dl = len12(h, 4)?;
            streams.push(NitStream {
                tlv_stream_id: u16::from_be_bytes([h[0], h[1]]),
                original_network_id: u16::from_be_bytes([h[2], h[3]]),
                descriptors: TlvDescriptor::split(slice(lp, p + 6, dl)?),
            });
            p += 6 + dl;
        }
        Ok(Nit {
            header,
            descriptors,
            streams,
        })
    }
}

/// One service of an AMT: the IP flow addresses of a service (Table 5-2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AmtService {
    /// `service_id`.
    pub service_id: u16,
    /// Source address and mask (4 bytes for IPv4, 16 for IPv6).
    pub src: (Vec<u8>, u8),
    /// Destination address and mask.
    pub dst: (Vec<u8>, u8),
}

/// One AMT section (Table 5-2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Amt {
    /// The section header.
    pub header: SectionHeader,
    /// Services.
    pub services: Vec<AmtService>,
}

impl Amt {
    /// Decode an AMT section (table id FEh).
    pub fn parse(b: &[u8]) -> Result<Amt, SectionError> {
        let (header, body) = long_section(b)?;
        let n = usize::from(u16_at(body, 0)? >> 6);
        let mut p = 2;
        let mut services = Vec::with_capacity(n);
        for _ in 0..n {
            let service_id = u16_at(body, p)?;
            let f = u16_at(body, p + 2)?;
            let v6 = f & 0x8000 != 0;
            let loop_len = usize::from(f & 0x03FF);
            let l = slice(body, p + 4, loop_len)?;
            let a = if v6 { 16 } else { 4 };
            if l.len() < 2 * a + 2 {
                return Err(SectionError::Malformed);
            }
            services.push(AmtService {
                service_id,
                src: (l[..a].to_vec(), l[a]),
                dst: (l[a + 1..2 * a + 1].to_vec(), l[2 * a + 1]),
            });
            p += 4 + loop_len;
        }
        Ok(Amt { header, services })
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Append section_length and a CRC to a section whose length bytes
    /// are placeholders.
    pub(crate) fn seal(mut s: Vec<u8>) -> Vec<u8> {
        let len = s.len() - 3 + 4;
        s[1] = (s[1] & 0xF0) | ((len >> 8) as u8 & 0x0F);
        s[2] = len as u8;
        let c = crc32(&s);
        s.extend(c.to_be_bytes());
        s
    }

    #[test]
    fn spec_mpt_as_seen_on_air() {
        // An MPT's start as transmitted (PA message, packet 0xA001): table
        // 20h, version 41h, length, mode 00, package id 0x0097, no MPT
        // descriptors, one HEVC asset on packet 0xA121 with a stream id
        let mut t = vec![0x20, 0x41, 0, 0, 0xFC, 2, 0x00, 0x97, 0, 0, 1];
        t.extend([0x00, 0, 0, 0, 0, 2, 0x00, 0x00]);
        t.extend(*b"hev1");
        t.extend([0xFE, 1, 0x00, 0xA1, 0x21]);
        t.extend([0, 5, 0x80, 0x11, 2, 0x00, 0x00]);
        let len = t.len() - 4;
        t[2..4].copy_from_slice(&(len as u16).to_be_bytes());
        let m = Mpt::parse(&t).unwrap();
        assert_eq!(m.service_id(), Some(0x0097));
        assert_eq!(m.assets.len(), 1);
        let a = &m.assets[0];
        assert_eq!(a.type_str(), "hev1");
        assert_eq!(a.packet_id(), Some(0xA121));
        assert_eq!(a.component_tag(), Some(0));
    }

    #[test]
    fn eit_with_one_event() {
        let mut s = vec![
            0x8B, 0xF0, 0, 0x00, 0x97, 0xC1, 0, 0, 0xB0, 0x70, 0x00, 0x0B, 0, 0x8B,
        ];
        s.extend([
            0x12, 0x34, 0xEF, 0x9A, 0x16, 0x00, 0x00, 0x01, 0x30, 0x00, 0x80, 0x00,
        ]);
        let s = seal(s);
        let e = Eit::parse(&s).unwrap();
        assert_eq!(e.header.extension, 0x0097);
        assert_eq!(e.tlv_stream_id, 0xB070);
        let ev = &e.events[0];
        assert_eq!(ev.event_id, 0x1234);
        assert_eq!(ev.duration.seconds(), Some(5400));
        assert_eq!(ev.running_status, 4);
        let mut bad = s.clone();
        bad[10] ^= 1;
        assert_eq!(Eit::parse(&bad), Err(SectionError::Crc));
    }

    #[test]
    fn tot_as_seen_on_air() {
        // MH-TOT of an M2 short section message on packet 0x8005:
        // a1 700b ef9a 160146 f000 + CRC
        let s = [
            0xA1, 0x70, 0x0B, 0xEF, 0x9A, 0x16, 0x01, 0x46, 0xF0, 0x00, 0x38, 0x34, 0x70, 0x0B,
        ];
        let t = Tot::parse(&s).unwrap();
        assert_eq!(t.jst.hms(), Some((16, 1, 46)));
        assert!(t.descriptors.is_empty());
    }

    #[test]
    fn nit_and_amt() {
        let mut s = vec![
            0x40, 0xF0, 0, 0x00, 0x0B, 0xC1, 0, 0, 0xF0, 0x00, 0xF0, 0x0B,
        ];
        s.extend([
            0xB0, 0x70, 0x00, 0x0B, 0xF0, 0x05, 0x41, 0x03, 0x00, 0x97, 0x01,
        ]);
        let n = Nit::parse(&seal(s)).unwrap();
        assert_eq!(n.header.extension, 0x000B);
        assert_eq!(n.streams[0].tlv_stream_id, 0xB070);
        assert_eq!(
            n.streams[0].descriptors[0].service_list(),
            Some(vec![(0x0097, 1)])
        );
        let mut a = vec![
            0xFE,
            0xF0,
            0,
            0x00,
            0x00,
            0xC1,
            0,
            0,
            0b0000_0000,
            0b0100_0000,
        ];
        a.extend([0x00, 0x97, 0x00, 10, 10, 0, 0, 1, 24, 239, 0, 0, 1, 32]);
        let m = Amt::parse(&seal(a)).unwrap();
        assert_eq!(m.services[0].service_id, 0x0097);
        assert_eq!(m.services[0].dst, (vec![239, 0, 0, 1], 32));
    }
}

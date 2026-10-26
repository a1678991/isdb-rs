//! MMT-SI descriptors (ARIB STD-B60 v1.14 §7.4, Table 4-10): a 16-bit
//! `descriptor_tag`, then a `descriptor_length` whose width "is varied
//! with the descriptor" (§7.4.3). The widths used here are those of each
//! descriptor's structure in §7.4.3, §10.3.3–§10.3.5 and §11.2.2 (16 bits
//! for 0002h and F000h–FFFFh, 8 bits for the others), with Table 4-10's
//! ranges for tags it only reserves: 4000h–6FFFh 16 bits, 7000h–7FFFh 32
//! bits (SPEC-NOTES "descriptor-length").
//!
//! The descriptors a receiver needs to list services, events and assets
//! are decoded ([`Descriptor`]); every other one is kept raw.

/// MMT-SI text: UCS in UTF-8 without BOM (§7.1, "the provisions of ARIB
/// STD-B62 shall be followed"; B62 is not local). Invalid sequences
/// become U+FFFD.
pub fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// Width in bytes of the `descriptor_length` field of `tag`.
pub fn length_width(tag: u16) -> usize {
    match tag {
        // Dependency descriptor (Table 7-32): 16 bits although Table 4-10
        // puts 0000h–3FFFh in the "8-bit length" range
        0x0002 => 2,
        0x4000..=0x6FFF => 2,
        0x7000..=0x7FFF => 4,
        0xF000..=0xFFFF => 2,
        _ => 1,
    }
}

/// A descriptor: its tag and body (the bytes after the length field).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Raw {
    /// `descriptor_tag`.
    pub tag: u16,
    /// The body.
    pub body: Vec<u8>,
}

/// Split a descriptor loop. `Err` with the offset of the first descriptor
/// that does not fit (the ones before it are returned too).
pub fn split(b: &[u8]) -> (Vec<Raw>, Option<usize>) {
    let mut out = Vec::new();
    let mut p = 0;
    while p < b.len() {
        let Some(t) = b.get(p..p + 2) else {
            return (out, Some(p));
        };
        let tag = u16::from_be_bytes([t[0], t[1]]);
        let w = length_width(tag);
        let Some(l) = b.get(p + 2..p + 2 + w) else {
            return (out, Some(p));
        };
        let len = l.iter().fold(0usize, |a, &x| (a << 8) | usize::from(x));
        let start = p + 2 + w;
        let Some(body) = b.get(start..start + len) else {
            return (out, Some(p));
        };
        out.push(Raw {
            tag,
            body: body.to_vec(),
        });
        p = start + len;
    }
    (out, None)
}

/// Descriptor tags (Table 4-10) used by name in this crate.
pub mod tag {
    /// MPU timestamp descriptor (§7.4.3.5).
    pub const MPU_TIMESTAMP: u16 = 0x0001;
    /// Dependency descriptor (§7.4.3.6).
    pub const DEPENDENCY: u16 = 0x0002;
    /// Access control descriptor (§7.4.3.7).
    pub const ACCESS_CONTROL: u16 = 0x8004;
    /// Scrambler descriptor (§7.4.3.8).
    pub const SCRAMBLER: u16 = 0x8005;
    /// MH-MPEG-4 audio descriptor (§7.4.3.11).
    pub const MPEG4_AUDIO: u16 = 0x8008;
    /// MH-HEVC descriptor (§7.4.3.13).
    pub const HEVC: u16 = 0x800A;
    /// MH-service list descriptor (§7.4.3.16).
    pub const SERVICE_LIST: u16 = 0x800D;
    /// Video component descriptor (§7.4.3.19).
    pub const VIDEO_COMPONENT: u16 = 0x8010;
    /// MH-stream identifier descriptor (§7.4.3.20).
    pub const STREAM_IDENTIFIER: u16 = 0x8011;
    /// MH-content descriptor (§7.4.3.21).
    pub const CONTENT: u16 = 0x8012;
    /// MH-audio component descriptor (§7.4.3.23).
    pub const AUDIO_COMPONENT: u16 = 0x8014;
    /// MH-broadcaster name descriptor (§7.4.3.27).
    pub const BROADCASTER_NAME: u16 = 0x8018;
    /// MH-service descriptor (§7.4.3.28).
    pub const SERVICE: u16 = 0x8019;
    /// MH-data coding system descriptor (§9.3).
    pub const DATA_CODING: u16 = 0x8020;
    /// MPU extended timestamp descriptor (§7.4.3.35).
    pub const MPU_EXTENDED_TIMESTAMP: u16 = 0x8026;
    /// MH-short event descriptor (§7.4.3.17).
    pub const SHORT_EVENT: u16 = 0xF001;
    /// MH-extended event descriptor (§7.4.3.18).
    pub const EXTENDED_EVENT: u16 = 0xF002;
}

/// One entry of an MPU timestamp descriptor: the presentation time of an
/// MPU's first access unit in presentation order (§7.4.3.5), an NTP
/// 64-bit timestamp.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MpuTimestamp {
    /// MPU sequence number.
    pub mpu_sequence_number: u32,
    /// Presentation time (NTP 64-bit: seconds since 1900 in the high 32
    /// bits, fraction in the low 32).
    pub presentation_time: u64,
}

/// One MPU of an MPU extended timestamp descriptor (§7.4.3.35).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MpuExtended {
    /// MPU sequence number.
    pub mpu_sequence_number: u32,
    /// Leap indicator of the MPU presentation time.
    pub leap_indicator: u8,
    /// Decoding time of the MPU's first access unit before its
    /// presentation time, in timescale units.
    pub decoding_time_offset: u16,
    /// Per access unit in decoding order: (DTS–PTS offset, PTS offset).
    /// The PTS offset is the descriptor's default when it gives one.
    pub access_units: Vec<(u16, Option<u16>)>,
}

/// An MPU extended timestamp descriptor (§7.4.3.35).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtendedTimestamp {
    /// Time units per second, when the descriptor gives one.
    pub timescale: Option<u32>,
    /// The PTS offset common to every access unit (pts_offset_type 1).
    pub default_pts_offset: Option<u16>,
    /// The MPUs it describes.
    pub mpus: Vec<MpuExtended>,
}

/// A decoded descriptor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Descriptor {
    /// MPU timestamp descriptor.
    MpuTimestamp(Vec<MpuTimestamp>),
    /// MPU extended timestamp descriptor.
    ExtendedTimestamp(ExtendedTimestamp),
    /// Access control descriptor: CA system id and where its ECM (in an
    /// MPT) or EMM (in a CAT) travels (§7.4.3.7).
    AccessControl {
        /// `CA_system_ID`.
        ca_system_id: u16,
        /// Location of the related ECM / EMM.
        location: Option<crate::table::Location>,
    },
    /// Scrambler descriptor (§7.4.3.8): layer type (`01` MMTP packet,
    /// `10` IP packet) and scramble system id (1 AES-128, 2 Camellia-128).
    Scrambler {
        /// `layer_type`.
        layer_type: u8,
        /// `scramble_system_id`.
        system_id: u8,
    },
    /// MH-stream identifier descriptor (§7.4.3.20).
    StreamIdentifier {
        /// `component_tag`.
        component_tag: u16,
    },
    /// MH-service descriptor (§7.4.3.28).
    Service {
        /// `service_type`.
        service_type: u8,
        /// Provider name.
        provider: String,
        /// Service name.
        name: String,
    },
    /// MH-service list descriptor (§7.4.3.16): (service id, type).
    ServiceList(Vec<(u16, u8)>),
    /// MH-short event descriptor (§7.4.3.17).
    ShortEvent {
        /// ISO 639 language code.
        language: [u8; 3],
        /// Event name.
        name: String,
        /// Event text.
        text: String,
    },
    /// MH-content descriptor (§7.4.3.21): (level 1, level 2, user)
    /// nibbles per genre.
    Content(Vec<(u8, u8, u8)>),
    /// MH-HEVC descriptor (§7.4.3.13): the fields that name the stream.
    Hevc {
        /// `profile_idc`.
        profile_idc: u8,
        /// `tier_flag`.
        tier: bool,
        /// `level_idc`.
        level_idc: u8,
    },
    /// MH-broadcaster name descriptor (§7.4.3.27).
    BroadcasterName(String),
    /// Any other descriptor, kept raw.
    Other(Raw),
}

impl Descriptor {
    /// Decode one raw descriptor. A body too short for its structure is
    /// kept raw.
    pub fn decode(r: &Raw) -> Descriptor {
        Self::try_decode(r).unwrap_or_else(|| Descriptor::Other(r.clone()))
    }

    fn try_decode(r: &Raw) -> Option<Descriptor> {
        let b = &r.body[..];
        Some(match r.tag {
            tag::MPU_TIMESTAMP => Descriptor::MpuTimestamp(
                b.as_chunks::<12>()
                    .0
                    .iter()
                    .map(|c| MpuTimestamp {
                        mpu_sequence_number: u32::from_be_bytes([c[0], c[1], c[2], c[3]]),
                        presentation_time: u64::from_be_bytes([
                            c[4], c[5], c[6], c[7], c[8], c[9], c[10], c[11],
                        ]),
                    })
                    .collect(),
            ),
            tag::MPU_EXTENDED_TIMESTAMP => Descriptor::ExtendedTimestamp(extended(b)?),
            tag::ACCESS_CONTROL => Descriptor::AccessControl {
                ca_system_id: u16::from_be_bytes([*b.first()?, *b.get(1)?]),
                location: crate::table::Location::parse(b.get(2..)?).map(|(l, _)| l),
            },
            tag::SCRAMBLER => Descriptor::Scrambler {
                layer_type: *b.first()? >> 6,
                system_id: *b.get(1)?,
            },
            tag::STREAM_IDENTIFIER => Descriptor::StreamIdentifier {
                component_tag: u16::from_be_bytes([*b.first()?, *b.get(1)?]),
            },
            tag::SERVICE => {
                let service_type = *b.first()?;
                let pl = usize::from(*b.get(1)?);
                let provider = b.get(2..2 + pl)?.to_vec();
                let nl = usize::from(*b.get(2 + pl)?);
                let name = b.get(3 + pl..3 + pl + nl)?.to_vec();
                Descriptor::Service {
                    service_type,
                    provider: text(&provider),
                    name: text(&name),
                }
            }
            tag::SERVICE_LIST => Descriptor::ServiceList(
                b.as_chunks::<3>()
                    .0
                    .iter()
                    .map(|c| (u16::from_be_bytes([c[0], c[1]]), c[2]))
                    .collect(),
            ),
            tag::SHORT_EVENT => {
                let language = [*b.first()?, *b.get(1)?, *b.get(2)?];
                let nl = usize::from(*b.get(3)?);
                let name = b.get(4..4 + nl)?.to_vec();
                let tl = usize::from(u16::from_be_bytes([*b.get(4 + nl)?, *b.get(5 + nl)?]));
                let body = b.get(6 + nl..6 + nl + tl)?;
                Descriptor::ShortEvent {
                    language,
                    name: text(&name),
                    text: text(body),
                }
            }
            tag::CONTENT => Descriptor::Content(
                b.as_chunks::<2>()
                    .0
                    .iter()
                    .map(|c| (c[0] >> 4, c[0] & 0x0F, c[1]))
                    .collect(),
            ),
            // profile byte, compatibility (4), four flags + copied_44bits
            // (6 bytes), then level_idc at byte 11
            tag::HEVC => Descriptor::Hevc {
                profile_idc: *b.first()? & 0x1F,
                tier: *b.first()? & 0x20 != 0,
                level_idc: *b.get(11)?,
            },
            tag::BROADCASTER_NAME => Descriptor::BroadcasterName(text(b)),
            _ => return None,
        })
    }
}

/// Decode an MPU extended timestamp descriptor body (§7.4.3.35).
fn extended(b: &[u8]) -> Option<ExtendedTimestamp> {
    let flags = *b.first()?;
    let pts_offset_type = (flags >> 1) & 3;
    let mut p = 1;
    let timescale = if flags & 1 != 0 {
        let t = b.get(p..p + 4)?;
        p += 4;
        Some(u32::from_be_bytes([t[0], t[1], t[2], t[3]]))
    } else {
        None
    };
    let default_pts_offset = if pts_offset_type == 1 {
        let t = b.get(p..p + 2)?;
        p += 2;
        Some(u16::from_be_bytes([t[0], t[1]]))
    } else {
        None
    };
    let mut mpus = Vec::new();
    while p < b.len() {
        let h = b.get(p..p + 8)?;
        let n = usize::from(h[7]);
        p += 8;
        let mut aus = Vec::with_capacity(n);
        for _ in 0..n {
            let d = b.get(p..p + 2)?;
            p += 2;
            let pts = if pts_offset_type == 2 {
                let o = b.get(p..p + 2)?;
                p += 2;
                Some(u16::from_be_bytes([o[0], o[1]]))
            } else {
                default_pts_offset
            };
            aus.push((u16::from_be_bytes([d[0], d[1]]), pts));
        }
        mpus.push(MpuExtended {
            mpu_sequence_number: u32::from_be_bytes([h[0], h[1], h[2], h[3]]),
            leap_indicator: h[4] >> 6,
            decoding_time_offset: u16::from_be_bytes([h[5], h[6]]),
            access_units: aus,
        });
    }
    Some(ExtendedTimestamp {
        timescale,
        default_pts_offset,
        mpus,
    })
}

static SPLIT_ERRORS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Descriptor loops that did not split to their length, since the
/// program started (a loop whose descriptors overrun it).
pub fn split_errors() -> u64 {
    SPLIT_ERRORS.load(std::sync::atomic::Ordering::Relaxed)
}

/// Decode a whole descriptor loop.
pub fn decode_all(b: &[u8]) -> Vec<Descriptor> {
    let (d, err) = split(b);
    if err.is_some() {
        SPLIT_ERRORS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    d.iter().map(Descriptor::decode).collect()
}

/// NTP 64-bit timestamp to seconds since the Unix epoch (with fraction).
pub fn ntp_to_unix(t: u64) -> f64 {
    const NTP_UNIX_OFFSET: f64 = 2_208_988_800.0;
    (t >> 32) as f64 + (t & 0xFFFF_FFFF) as f64 / 4_294_967_296.0 - NTP_UNIX_OFFSET
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spec_length_widths_follow_the_descriptor_structures() {
        // §7.4.3: 16-bit lengths for the dependency, linkage, short and
        // extended event, stuffing, broadcast id, network identification
        // and event message descriptors; 8-bit for the rest
        for t in [
            0x0002, 0xF000, 0xF001, 0xF002, 0xF003, 0xF004, 0xF005, 0xF006,
        ] {
            assert_eq!(length_width(t), 2, "{t:#06x}");
        }
        for t in [
            0x0001, 0x8004, 0x8005, 0x8010, 0x8019, 0x8026, 0x8029, 0x8035,
        ] {
            assert_eq!(length_width(t), 1, "{t:#06x}");
        }
        assert_eq!(length_width(0x7000), 4);
    }

    #[test]
    fn splits_mixed_widths() {
        // MPU timestamp (8-bit length) then a short event (16-bit length)
        let mut b = vec![0x00, 0x01, 12];
        b.extend([0, 0, 0, 5, 0xE0, 0, 0, 0, 0x80, 0, 0, 0]);
        b.extend([0xF0, 0x01, 0x00, 7, b'j', b'p', b'n', 1, 0x41, 0x00, 0x00]);
        let (d, err) = split(&b);
        assert_eq!(err, None);
        assert_eq!(d.len(), 2);
        let ts = Descriptor::decode(&d[0]);
        assert_eq!(
            ts,
            Descriptor::MpuTimestamp(vec![MpuTimestamp {
                mpu_sequence_number: 5,
                presentation_time: 0xE000_0000_8000_0000
            }])
        );
        match Descriptor::decode(&d[1]) {
            Descriptor::ShortEvent {
                language,
                name,
                text,
            } => {
                assert_eq!(&language, b"jpn");
                assert_eq!(name, "A");
                assert!(text.is_empty());
            }
            o => panic!("{o:?}"),
        }
    }

    #[test]
    fn extended_timestamp_with_per_au_offsets() {
        // pts_offset_type 2, timescale 90000, one MPU of two access units
        let mut b = vec![0b0000_0101];
        b.extend(90000u32.to_be_bytes());
        b.extend(7u32.to_be_bytes());
        b.extend([0x00, 0x0B, 0xB8, 2]);
        b.extend([0x00, 0x10, 0x0B, 0xB8, 0x00, 0x20, 0x0B, 0xB8]);
        let e = extended(&b).unwrap();
        assert_eq!(e.timescale, Some(90000));
        assert_eq!(e.mpus[0].mpu_sequence_number, 7);
        assert_eq!(e.mpus[0].decoding_time_offset, 3000);
        assert_eq!(
            e.mpus[0].access_units,
            vec![(16, Some(3000)), (32, Some(3000))]
        );
    }

    #[test]
    fn ntp_epoch() {
        assert_eq!(ntp_to_unix(2_208_988_800u64 << 32), 0.0);
    }
}

//! SI descriptors (ARIB STD-B10 Part 2 §6, Table 6-1; tag values Part 1
//! §5.3, Table 5-3).
//!
//! Generic layout (ARIB STD-B10 Part 2 §6.2): `descriptor_tag` (8)
//! `descriptor_length` (8) followed by `descriptor_length` bytes.  Every
//! typed descriptor decodes *all* of its bytes or fails (no silent
//! truncation), keeps reserved / reserved_future_use bits verbatim, and
//! re-encodes byte-identically; `descriptor_length` and the inner length
//! fields are recomputed on encode.  Tags without a typed decoder in this
//! crate are kept as [`Descriptor::Unknown`].
//!
//! Note: this layer does its own tag/length framing; it does not (yet)
//! build on the raw H.222.0 form `isdb_ts::descriptor::Descriptor` that
//! the PSI loops of `isdb-ts` use.

mod arib;
mod dvb;

pub use arib::*;
pub use dvb::*;

use isdb_text::AribString;
use isdb_ts::bits::{BitReader, BitWriter};
use isdb_ts::{Error, Result};

/// 3-character ISO 639-2 language code or ISO 3166 country code, each
/// character coded in 8 bits per ISO/IEC 8859-1 (ARIB STD-B10 Part 2
/// §6.2.3, §6.2.5), e.g. `*b"jpn"`, `*b"JPN"`.
pub type Code3 = [u8; 3];

/// Body codec of one typed descriptor (everything after
/// `descriptor_length`).
pub trait DescriptorBody: Sized {
    /// `descriptor_tag` (ARIB STD-B10 Part 1 Table 5-3).
    const TAG: u8;
    /// Syntax name used in error messages.
    const NAME: &'static str;
    /// Decode exactly `data` (the `descriptor_length` bytes); fails if the
    /// syntax does not account for every byte.
    fn decode(data: &[u8]) -> Result<Self>;
    /// Append the body bytes (length prefixes inside the body are
    /// recomputed).
    fn encode(&self, out: &mut Vec<u8>) -> Result<()>;
}

macro_rules! descriptors {
    ($( $(#[$m:meta])* $tag:literal => $variant:ident($ty:ty), )*) => {
        /// A decoded descriptor (ARIB STD-B10 Part 2 §6.2).
        #[derive(Debug, Clone, PartialEq, Eq)]
        pub enum Descriptor {
            $( $(#[$m])* $variant($ty), )*
            /// Descriptor whose tag has no typed decoder here; kept raw.
            Unknown {
                /// descriptor_tag.
                tag: u8,
                /// The `descriptor_length` data bytes.
                data: Vec<u8>,
            },
        }

        /// Tags decoded into a typed [`Descriptor`] variant.
        pub const TYPED_TAGS: &[u8] = &[$($tag),*];

        $(
            impl From<$ty> for Descriptor {
                fn from(d: $ty) -> Self { Descriptor::$variant(d) }
            }
        )*

        impl Descriptor {
            /// descriptor_tag of this descriptor.
            pub fn tag(&self) -> u8 {
                match self {
                    $( Descriptor::$variant(_) => $tag, )*
                    Descriptor::Unknown { tag, .. } => *tag,
                }
            }

            /// Syntax name (e.g. `"service_descriptor"`), or `"unknown"`.
            pub fn name(&self) -> &'static str {
                match self {
                    $( Descriptor::$variant(_) => <$ty as DescriptorBody>::NAME, )*
                    Descriptor::Unknown { .. } => "unknown",
                }
            }

            /// Decode a descriptor body given its tag.  Unknown tags give
            /// [`Descriptor::Unknown`]; a typed tag whose body violates its
            /// syntax is an error.
            pub fn decode_body(tag: u8, data: &[u8]) -> Result<Self> {
                Ok(match tag {
                    $( $tag => Descriptor::$variant(<$ty as DescriptorBody>::decode(data)?), )*
                    _ => Descriptor::Unknown { tag, data: data.to_vec() },
                })
            }

            fn encode_body(&self, out: &mut Vec<u8>) -> Result<()> {
                match self {
                    $( Descriptor::$variant(d) => d.encode(out), )*
                    Descriptor::Unknown { data, .. } => {
                        out.extend_from_slice(data);
                        Ok(())
                    }
                }
            }
        }
    };
}

descriptors! {
    /// 0x09 conditional_access_descriptor (H.222.0 §2.6.16 Table 2-58).
    0x09 => ConditionalAccess(ConditionalAccess),
    /// 0x40 network_name_descriptor (B10 §6.2.11).
    0x40 => NetworkName(NetworkName),
    /// 0x41 service_list_descriptor (B10 §6.2.14).
    0x41 => ServiceList(ServiceList),
    /// 0x42 stuffing_descriptor (B10 §6.2.17).
    0x42 => Stuffing(Stuffing),
    /// 0x43 satellite_delivery_system_descriptor (B10 §6.2.6).
    0x43 => SatelliteDeliverySystem(SatelliteDeliverySystem),
    /// 0x47 bouquet_name_descriptor (B10 §6.2.1).
    0x47 => BouquetName(BouquetName),
    /// 0x48 service_descriptor (B10 §6.2.13).
    0x48 => Service(Service),
    /// 0x49 country_availability_descriptor (B10 §6.2.5).
    0x49 => CountryAvailability(CountryAvailability),
    /// 0x4A linkage_descriptor (B10 §6.2.8).
    0x4A => Linkage(Linkage),
    /// 0x4B NVOD_reference_descriptor (B10 §6.2.10).
    0x4B => NvodReference(NvodReference),
    /// 0x4C time_shifted_service_descriptor (B10 §6.2.19).
    0x4C => TimeShiftedService(TimeShiftedService),
    /// 0x4D short_event_descriptor (B10 §6.2.15).
    0x4D => ShortEvent(ShortEvent),
    /// 0x4E extended_event_descriptor (B10 §6.2.7).
    0x4E => ExtendedEvent(ExtendedEvent),
    /// 0x4F time_shifted_event_descriptor (B10 §6.2.18).
    0x4F => TimeShiftedEvent(TimeShiftedEvent),
    /// 0x50 component_descriptor (B10 §6.2.3).
    0x50 => Component(Component),
    /// 0x52 stream_identifier_descriptor (B10 §6.2.16).
    0x52 => StreamIdentifier(StreamIdentifier),
    /// 0x53 CA_identifier_descriptor (B10 §6.2.2).
    0x53 => CaIdentifier(CaIdentifier),
    /// 0x54 content_descriptor (B10 §6.2.4).
    0x54 => Content(Content),
    /// 0x55 parental_rating_descriptor (B10 §6.2.12).
    0x55 => ParentalRating(ParentalRating),
    /// 0x58 local_time_offset_descriptor (B10 §6.2.25).
    0x58 => LocalTimeOffset(LocalTimeOffset),
    /// 0xC0 hierarchical_transmission_descriptor (B10 §6.2.22).
    0xC0 => HierarchicalTransmission(HierarchicalTransmission),
    /// 0xC1 digital_copy_control_descriptor (B10 §6.2.23).
    0xC1 => DigitalCopyControl(DigitalCopyControl),
    /// 0xC4 audio_component_descriptor (B10 §6.2.26).
    0xC4 => AudioComponent(AudioComponent),
    /// 0xC5 hyperlink_descriptor (B10 §6.2.29).
    0xC5 => Hyperlink(Hyperlink),
    /// 0xC7 data_content_descriptor (B10 §6.2.28).
    0xC7 => DataContent(DataContent),
    /// 0xC8 video_decode_control_descriptor (B10 §6.2.30).
    0xC8 => VideoDecodeControl(VideoDecodeControl),
    /// 0xCD TS_information_descriptor (B10 §6.2.42).
    0xCD => TsInformation(TsInformation),
    /// 0xCE extended_broadcaster_descriptor (B10 §6.2.43).
    0xCE => ExtendedBroadcaster(ExtendedBroadcaster),
    /// 0xCF logo_transmission_descriptor (B10 §6.2.44).
    0xCF => LogoTransmission(LogoTransmission),
    /// 0xD5 series_descriptor (B10 §6.2.33).
    0xD5 => Series(Series),
    /// 0xD6 event_group_descriptor (B10 §6.2.34).
    0xD6 => EventGroup(EventGroup),
    /// 0xD7 SI_parameter_descriptor (B10 §6.2.35).
    0xD7 => SiParameter(SiParameter),
    /// 0xD8 broadcaster_name_descriptor (B10 §6.2.36).
    0xD8 => BroadcasterName(BroadcasterName),
    /// 0xD9 component_group_descriptor (B10 §6.2.37).
    0xD9 => ComponentGroup(ComponentGroup),
    /// 0xDA SI_prime_ts_descriptor (B10 §6.2.38).
    0xDA => SiPrimeTs(SiPrimeTs),
    /// 0xDB board_information_descriptor (B10 §6.2.39).
    0xDB => BoardInformation(BoardInformation),
    /// 0xDC LDT_linkage_descriptor (B10 §6.2.40).
    0xDC => LdtLinkage(LdtLinkage),
    /// 0xDD connected_transmission_descriptor (B10 §6.2.41).
    0xDD => ConnectedTransmission(ConnectedTransmission),
    /// 0xDE content_availability_descriptor (B10 §6.2.45).
    0xDE => ContentAvailability(ContentAvailability),
    /// 0xF6 access_control_descriptor (B10 §6.2.54).
    0xF6 => AccessControl(AccessControl),
    /// 0xFA terrestrial_delivery_system_descriptor (B10 §6.2.31).
    0xFA => TerrestrialDeliverySystem(TerrestrialDeliverySystem),
    /// 0xFB partial_reception_descriptor (B10 §6.2.32).
    0xFB => PartialReception(PartialReception),
    /// 0xFC emergency_information_descriptor (B10 §6.2.24).
    0xFC => EmergencyInformation(EmergencyInformation),
    /// 0xFD data_component_descriptor (B10 §6.2.20).
    0xFD => DataComponent(DataComponent),
    /// 0xFE system_management_descriptor (B10 §6.2.21).
    0xFE => SystemManagement(SystemManagement),
}

impl Descriptor {
    /// Append `tag`, computed `descriptor_length` and body (B10 Part 2
    /// §6.2).  Fails if the body exceeds 255 bytes.
    pub fn encode(&self, out: &mut Vec<u8>) -> Result<()> {
        let mut body = Vec::new();
        self.encode_body(&mut body)?;
        if body.len() > 255 {
            return Err(Error::Overflow {
                what: "descriptor_length",
                value: body.len() as u64,
                bits: 8,
            });
        }
        out.push(self.tag());
        out.push(body.len() as u8);
        out.extend_from_slice(&body);
        Ok(())
    }

    /// Encoded size in bytes (2 + descriptor_length).
    pub fn encoded_len(&self) -> Result<usize> {
        let mut v = Vec::new();
        self.encode(&mut v)?;
        Ok(v.len())
    }

    /// Decode one complete descriptor (`tag`, `length`, data) that must span
    /// all of `bytes`.
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let v = decode_loop(bytes)?;
        match <[Descriptor; 1]>::try_from(v) {
            Ok([d]) => Ok(d),
            Err(v) => Err(Error::invalid(
                "descriptor",
                format!("expected one descriptor, found {}", v.len()),
            )),
        }
    }
}

/// Decode a descriptor loop (`for(i=0;i<N;i++){ descriptor() }`) that
/// exactly fills `bytes` (ARIB STD-B10 Part 2 §6.2).
pub fn decode_loop(bytes: &[u8]) -> Result<Vec<Descriptor>> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes.len() - i < 2 {
            return Err(Error::Truncated {
                what: "descriptor header",
                need: i + 2,
                have: bytes.len(),
            });
        }
        let tag = bytes[i];
        let len = bytes[i + 1] as usize;
        let start = i + 2;
        if start + len > bytes.len() {
            return Err(Error::Truncated {
                what: "descriptor",
                need: start + len,
                have: bytes.len(),
            });
        }
        out.push(Descriptor::decode_body(tag, &bytes[start..start + len])?);
        i = start + len;
    }
    Ok(out)
}

/// Encode a descriptor loop (without any length prefix).
pub fn encode_loop(ds: &[Descriptor], out: &mut Vec<u8>) -> Result<()> {
    for d in ds {
        d.encode(out)?;
    }
    Ok(())
}

/// Encode a descriptor loop into a fresh buffer.
pub fn loop_bytes(ds: &[Descriptor]) -> Result<Vec<u8>> {
    let mut v = Vec::new();
    encode_loop(ds, &mut v)?;
    Ok(v)
}

// ---------------------------------------------------------------------------
// shared helpers for the descriptor bodies
// ---------------------------------------------------------------------------

pub(crate) fn code3(r: &mut BitReader<'_>) -> Result<Code3> {
    let b = r.bytes(3)?;
    Ok([b[0], b[1], b[2]])
}

/// `n` bytes as an [`AribString`].
pub(crate) fn string(r: &mut BitReader<'_>, n: usize) -> Result<AribString> {
    Ok(AribString(r.bytes(n)?.to_vec()))
}

/// 8-bit length followed by that many bytes.
pub(crate) fn len8_bytes<'a>(r: &mut BitReader<'a>) -> Result<&'a [u8]> {
    let n = r.u8()? as usize;
    r.bytes(n)
}

/// 8-bit length followed by an [`AribString`].
pub(crate) fn len8_string(r: &mut BitReader<'_>) -> Result<AribString> {
    Ok(AribString(len8_bytes(r)?.to_vec()))
}

pub(crate) fn put_len8(w: &mut BitWriter, what: &'static str, bytes: &[u8]) -> Result<()> {
    w.write(what, 8, bytes.len() as u64)?;
    w.bytes(bytes);
    Ok(())
}

pub(crate) fn finish(w: BitWriter, out: &mut Vec<u8>) {
    out.extend_from_slice(&w.into_bytes());
}

/// Require that the remaining length is a multiple of `unit` bytes.
pub(crate) fn multiple_of(what: &'static str, len: usize, unit: usize) -> Result<usize> {
    if len / unit * unit != len {
        return Err(Error::invalid(
            what,
            format!("loop of {len} bytes is not a multiple of {unit}"),
        ));
    }
    Ok(len / unit)
}

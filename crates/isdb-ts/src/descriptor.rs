//! Raw descriptors (H.222.0 §2.6, §2.6.1, Table 2-45).
//!
//! "All descriptors have a format which begins with an 8-bit tag value. The
//! tag value is followed by an 8-bit descriptor length and data fields."
//! (§2.6).  "descriptor_length … specifying the number of bytes of the
//! descriptor immediately following descriptor_length field" (§2.6.1).
//! Typed decoders live in higher layers (e.g. `isdb-si` for ARIB tags).

use crate::error::{Error, Result};

/// One descriptor (H.222.0 §2.6.1): tag and the `descriptor_length` bytes
/// after the length.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Descriptor {
    /// descriptor_tag.
    pub tag: u8,
    /// The bytes following descriptor_length (at most 255).
    pub data: Vec<u8>,
}

impl Descriptor {
    /// New descriptor.
    pub fn new(tag: u8, data: impl Into<Vec<u8>>) -> Self {
        Self {
            tag,
            data: data.into(),
        }
    }

    /// Encoded size (2 + data length).
    pub fn encoded_len(&self) -> usize {
        2 + self.data.len()
    }

    /// Append tag, length and data.
    pub fn encode(&self, out: &mut Vec<u8>) -> Result<()> {
        if self.data.len() > 255 {
            return Err(Error::Overflow {
                what: "descriptor_length",
                value: self.data.len() as u64,
                bits: 8,
            });
        }
        out.push(self.tag);
        out.push(self.data.len() as u8);
        out.extend_from_slice(&self.data);
        Ok(())
    }
}

/// Split a descriptor loop (e.g. the `program_info_length` bytes of a PMT,
/// Table 2-33) into descriptors (§2.6.1).  Errors if the last descriptor runs past the loop.
pub fn parse_loop(mut b: &[u8]) -> Result<Vec<Descriptor>> {
    let mut v = Vec::new();
    while !b.is_empty() {
        if b.len() < 2 {
            return Err(Error::Truncated {
                what: "descriptor header",
                need: 2,
                have: b.len(),
            });
        }
        let n = b[1] as usize;
        if b.len() < 2 + n {
            return Err(Error::Truncated {
                what: "descriptor_length",
                need: 2 + n,
                have: b.len(),
            });
        }
        v.push(Descriptor {
            tag: b[0],
            data: b[2..2 + n].to_vec(),
        });
        b = &b[2 + n..];
    }
    Ok(v)
}

/// Encode a descriptor loop (H.222.0 §2.6.1), without any loop-length field.
pub fn encode_loop(ds: &[Descriptor]) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(loop_len(ds));
    for d in ds {
        d.encode(&mut out)?;
    }
    Ok(out)
}

/// Encoded size of a descriptor loop (H.222.0 §2.6.1: 2 + descriptor_length each).
pub fn loop_len(ds: &[Descriptor]) -> usize {
    ds.iter().map(Descriptor::encoded_len).sum()
}

/// Category of a descriptor_tag per H.222.0 Table 2-45.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TagClass {
    /// 0, 1 and 45–63: ITU-T Rec. H.222.0 | ISO/IEC 13818-1 reserved.
    Reserved,
    /// 2–18 and 27–44: defined by H.222.0; the name from Table 2-45.
    Defined(&'static str),
    /// 19–26: "Defined in ISO/IEC 13818-6".
    Iso13818_6,
    /// 64–255: User Private (ARIB STD-B10 descriptors live here).
    UserPrivate,
}

/// Classify `tag` per Table 2-45.
pub fn tag_class(tag: u8) -> TagClass {
    const NAMES: [&str; 17] = [
        "video_stream_descriptor",
        "audio_stream_descriptor",
        "hierarchy_descriptor",
        "registration_descriptor",
        "data_stream_alignment_descriptor",
        "target_background_grid_descriptor",
        "video_window_descriptor",
        "CA_descriptor",
        "ISO_639_language_descriptor",
        "system_clock_descriptor",
        "multiplex_buffer_utilization_descriptor",
        "copyright_descriptor",
        "maximum_bitrate_descriptor",
        "private_data_indicator_descriptor",
        "smoothing_buffer_descriptor",
        "STD_descriptor",
        "IBP_descriptor",
    ];
    const NAMES2: [&str; 18] = [
        "MPEG-4_video_descriptor",
        "MPEG-4_audio_descriptor",
        "IOD_descriptor",
        "SL_descriptor",
        "FMC_descriptor",
        "external_ES_ID_descriptor",
        "MuxCode_descriptor",
        "FmxBufferSize_descriptor",
        "multiplexbuffer_descriptor",
        "content_labeling_descriptor",
        "metadata_pointer_descriptor",
        "metadata_descriptor",
        "metadata_STD_descriptor",
        "AVC video descriptor",
        "IPMP_descriptor",
        "AVC timing and HRD descriptor",
        "MPEG-2_AAC_audio_descriptor",
        "FlexMuxTiming_descriptor",
    ];
    match tag {
        0 | 1 | 45..=63 => TagClass::Reserved,
        2..=18 => TagClass::Defined(NAMES[(tag - 2) as usize]),
        19..=26 => TagClass::Iso13818_6,
        27..=44 => TagClass::Defined(NAMES2[(tag - 27) as usize]),
        64..=255 => TagClass::UserPrivate,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// §2.6: tag(8) length(8) data; Table 2-45 tag ranges.
    #[test]
    fn spec_2_6_descriptor_format_and_table2_45_ranges() {
        let b = [0x09, 0x04, 0x00, 0x05, 0xE1, 0x00, 0x52, 0x01, 0xAA];
        let ds = parse_loop(&b).unwrap();
        assert_eq!(
            ds,
            vec![
                Descriptor::new(0x09, [0x00, 0x05, 0xE1, 0x00]),
                Descriptor::new(0x52, [0xAA])
            ]
        );
        assert_eq!(encode_loop(&ds).unwrap(), b);
        assert!(parse_loop(&b[..8]).is_err());
        assert!(parse_loop(&[0x09]).is_err());
        assert_eq!(tag_class(0), TagClass::Reserved);
        assert_eq!(tag_class(2), TagClass::Defined("video_stream_descriptor"));
        assert_eq!(tag_class(9), TagClass::Defined("CA_descriptor"));
        assert_eq!(tag_class(18), TagClass::Defined("IBP_descriptor"));
        assert_eq!(tag_class(19), TagClass::Iso13818_6);
        assert_eq!(tag_class(26), TagClass::Iso13818_6);
        assert_eq!(tag_class(27), TagClass::Defined("MPEG-4_video_descriptor"));
        assert_eq!(tag_class(40), TagClass::Defined("AVC video descriptor"));
        assert_eq!(tag_class(44), TagClass::Defined("FlexMuxTiming_descriptor"));
        assert_eq!(tag_class(45), TagClass::Reserved);
        assert_eq!(tag_class(63), TagClass::Reserved);
        assert_eq!(tag_class(64), TagClass::UserPrivate);
        assert_eq!(tag_class(255), TagClass::UserPrivate);
    }
}

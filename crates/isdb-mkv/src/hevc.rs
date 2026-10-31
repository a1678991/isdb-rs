//! What a Matroska / MP4 HEVC track needs from the bitstream: the
//! `HEVCDecoderConfigurationRecord` (ISO/IEC 14496-15 §8.3.3.1, `hvcC`)
//! built from the parameter sets, and the picture size from the SPS
//! (H.265 §7.3.2.2); the picture order count of a slice (§7.3.6.1), for
//! placing pictures after a loss.

/// The fields of a sequence parameter set the record and the track
/// header need.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sps {
    /// The 12 bytes of `general_profile_tier_level` after
    /// `sps_max_sub_layers_minus1`/`sps_temporal_id_nesting_flag`:
    /// profile space, tier, profile idc, compatibility flags (32), the
    /// 48 constraint bits, level idc.
    pub general_ptl: [u8; 12],
    /// `sps_max_sub_layers_minus1` + 1.
    pub sub_layers: u8,
    /// `sps_temporal_id_nesting_flag`.
    pub temporal_id_nested: bool,
    /// `chroma_format_idc` (1 = 4:2:0).
    pub chroma_format: u8,
    /// `bit_depth_luma_minus8` + 8.
    pub bit_depth_luma: u8,
    /// `bit_depth_chroma_minus8` + 8.
    pub bit_depth_chroma: u8,
    /// The luma width and height after the conformance window.
    pub width: u32,
    /// See `width`.
    pub height: u32,
    /// `separate_colour_plane_flag`.
    pub separate_colour_plane: bool,
    /// `log2_max_pic_order_cnt_lsb_minus4` + 4: the width of a slice's
    /// `slice_pic_order_cnt_lsb`.
    pub log2_max_poc_lsb: u8,
}

/// The fields of a picture parameter set (H.265 §7.3.2.3.1) a slice
/// header's parse needs up to its picture order count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pps {
    /// `dependent_slice_segments_enabled_flag`.
    pub dependent_slice_segments: bool,
    /// `output_flag_present_flag`.
    pub output_flag_present: bool,
    /// `num_extra_slice_header_bits`.
    pub extra_slice_header_bits: u8,
}

/// RBSP reader over a NAL unit's payload: emulation prevention bytes
/// (`00 00 03`) dropped.
struct Rbsp {
    b: Vec<u8>,
    pos: usize,
}

impl Rbsp {
    fn new(nal_payload: &[u8]) -> Self {
        let mut b = Vec::with_capacity(nal_payload.len());
        let mut zeros = 0;
        for &x in nal_payload {
            if zeros >= 2 && x == 3 {
                zeros = 0;
                continue;
            }
            zeros = if x == 0 { zeros + 1 } else { 0 };
            b.push(x);
        }
        Self { b, pos: 0 }
    }

    fn bit(&mut self) -> Option<u32> {
        let v = (self.b.get(self.pos / 8)? >> (7 - self.pos % 8)) & 1;
        self.pos += 1;
        Some(u32::from(v))
    }

    fn read(&mut self, n: u32) -> Option<u32> {
        let mut v = 0;
        for _ in 0..n {
            v = v << 1 | self.bit()?;
        }
        Some(v)
    }

    /// `ue(v)`.
    fn ue(&mut self) -> Option<u32> {
        let mut zeros = 0;
        while self.bit()? == 0 {
            zeros += 1;
            if zeros > 31 {
                return None;
            }
        }
        Some((1u32 << zeros) - 1 + self.read(zeros)?)
    }

    fn skip(&mut self, n: usize) {
        self.pos += n;
    }
}

/// Parse an SPS NAL unit (with its 2-byte NAL header).
#[must_use]
pub fn parse_sps(nal: &[u8]) -> Option<Sps> {
    if (nal.first()? >> 1) & 0x3f != 33 {
        return None;
    }
    let mut r = Rbsp::new(nal.get(2..)?);
    r.read(4)?; // sps_video_parameter_set_id
    let max_sub_layers_minus1 = r.read(3)?;
    let temporal_id_nested = r.bit()? == 1;
    // profile_tier_level(1, max_sub_layers_minus1): the general part is
    // 88 bits, byte-aligned here (4 + 3 + 1 bits came before).
    let start = r.pos / 8;
    let general_ptl: [u8; 12] = r.b.get(start..start + 12)?.try_into().ok()?;
    r.skip(96);
    let mut present = Vec::new();
    for _ in 0..max_sub_layers_minus1 {
        let profile = r.bit()? == 1;
        let level = r.bit()? == 1;
        present.push((profile, level));
    }
    if max_sub_layers_minus1 > 0 {
        r.skip(2 * (8 - max_sub_layers_minus1 as usize));
    }
    for (profile, level) in present {
        if profile {
            r.skip(88);
        }
        if level {
            r.skip(8);
        }
    }
    r.ue()?; // sps_seq_parameter_set_id
    let chroma_format = r.ue()?;
    let separate_colour_plane = chroma_format == 3 && r.bit()? == 1;
    let mut width = r.ue()?;
    let mut height = r.ue()?;
    if r.bit()? == 1 {
        // conformance_window: offsets in chroma units.
        let (sub_w, sub_h) = match chroma_format {
            1 => (2, 2),
            2 => (2, 1),
            _ => (1, 1),
        };
        let (left, right, top, bottom) = (r.ue()?, r.ue()?, r.ue()?, r.ue()?);
        width = width.checked_sub(sub_w * (left + right))?;
        height = height.checked_sub(sub_h * (top + bottom))?;
    }
    let bit_depth_luma = r.ue()? + 8;
    let bit_depth_chroma = r.ue()? + 8;
    let log2_max_poc_lsb = r.ue()? + 4;
    if log2_max_poc_lsb > 16 {
        return None;
    }
    Some(Sps {
        general_ptl,
        sub_layers: u8::try_from(max_sub_layers_minus1 + 1).ok()?,
        temporal_id_nested,
        chroma_format: u8::try_from(chroma_format).ok()?,
        bit_depth_luma: u8::try_from(bit_depth_luma).ok()?,
        bit_depth_chroma: u8::try_from(bit_depth_chroma).ok()?,
        width,
        height,
        separate_colour_plane,
        log2_max_poc_lsb: u8::try_from(log2_max_poc_lsb).ok()?,
    })
}

/// Parse a PPS NAL unit (with its 2-byte NAL header).
#[must_use]
pub fn parse_pps(nal: &[u8]) -> Option<Pps> {
    if (nal.first()? >> 1) & 0x3f != 34 {
        return None;
    }
    let mut r = Rbsp::new(nal.get(2..)?);
    r.ue()?; // pps_pic_parameter_set_id
    r.ue()?; // pps_seq_parameter_set_id
    let dependent_slice_segments = r.bit()? == 1;
    let output_flag_present = r.bit()? == 1;
    let extra_slice_header_bits = u8::try_from(r.read(3)?).ok()?;
    Some(Pps {
        dependent_slice_segments,
        output_flag_present,
        extra_slice_header_bits,
    })
}

/// The `slice_pic_order_cnt_lsb` of the first slice segment of a picture
/// (a VCL NAL unit with its 2-byte header and
/// `first_slice_segment_in_pic_flag` set; H.265 §7.3.6.1), under its
/// SPS and PPS: 0 for an IDR picture, `None` for any other slice (or a
/// header cut short). Only the slice header's head is read, so a slice
/// whose data was cut after it still gives it.
#[must_use]
pub fn first_slice_poc_lsb(nal: &[u8], sps: &Sps, pps: &Pps) -> Option<u32> {
    let ty = (nal.first()? >> 1) & 0x3f;
    if ty >= 32 {
        return None;
    }
    let mut r = Rbsp::new(nal.get(2..)?);
    if r.bit()? != 1 {
        return None; // not the picture's first slice segment
    }
    if (16..=23).contains(&ty) {
        r.bit()?; // no_output_of_prior_pics_flag
    }
    r.ue()?; // slice_pic_parameter_set_id
    r.skip(usize::from(pps.extra_slice_header_bits)); // slice_reserved_flag
    r.ue()?; // slice_type
    if pps.output_flag_present {
        r.bit()?; // pic_output_flag
    }
    if sps.separate_colour_plane {
        r.read(2)?; // colour_plane_id
    }
    if matches!(ty, 19 | 20) {
        return Some(0); // IDR_W_RADL, IDR_N_LP
    }
    r.read(u32::from(sps.log2_max_poc_lsb))
}

/// The `HEVCDecoderConfigurationRecord` of one VPS / SPS / PPS set (NAL
/// units with their headers, no start codes), NAL lengths 4 bytes. The
/// fields an SPS does not give (`min_spatial_segmentation_idc`,
/// `parallelismType`, `avgFrameRate`, `constantFrameRate`) are 0, as
/// ffmpeg writes them.
#[must_use]
pub fn hvcc(vps: &[&[u8]], sps: &[&[u8]], pps: &[&[u8]], sei: &[&[u8]]) -> Option<Vec<u8>> {
    let s = parse_sps(sps.first()?)?;
    let mut out = Vec::with_capacity(64);
    out.push(1); // configurationVersion
    out.extend_from_slice(&s.general_ptl);
    out.extend_from_slice(&[0xf0, 0x00]); // min_spatial_segmentation_idc 0
    out.push(0xfc); // parallelismType 0
    out.push(0xfc | s.chroma_format);
    out.push(0xf8 | (s.bit_depth_luma - 8));
    out.push(0xf8 | (s.bit_depth_chroma - 8));
    out.extend_from_slice(&[0, 0]); // avgFrameRate
    out.push(
        (s.sub_layers & 7) << 3 // numTemporalLayers
        | u8::from(s.temporal_id_nested) << 2
        | 3, // lengthSizeMinusOne
    );
    let arrays: Vec<(u8, &[&[u8]])> = [(32, vps), (33, sps), (34, pps), (39, sei)]
        .into_iter()
        .filter(|(_, n)| !n.is_empty())
        .collect();
    out.push(u8::try_from(arrays.len()).ok()?);
    for (ty, nals) in arrays {
        out.push(0x80 | ty); // array_completeness 1
        out.extend_from_slice(&u16::try_from(nals.len()).ok()?.to_be_bytes());
        for n in nals {
            out.extend_from_slice(&u16::try_from(n.len()).ok()?.to_be_bytes());
            out.extend_from_slice(n);
        }
    }
    Some(out)
}

/// An Annex B access unit to 4-byte length-prefixed NAL units (ISO/IEC
/// 14496-15 sample form), each NAL unit handed to `keep` first (`false`
/// drops it).
#[must_use]
pub fn annex_b_to_length(au: &[u8], mut keep: impl FnMut(&[u8]) -> bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(au.len());
    for nal in annex_b_nals(au) {
        if nal.is_empty() || !keep(nal) {
            continue;
        }
        let Ok(n) = u32::try_from(nal.len()) else {
            continue;
        };
        out.extend_from_slice(&n.to_be_bytes());
        out.extend_from_slice(nal);
    }
    out
}

/// The NAL units of an Annex B byte stream (start codes of 3 or 4 bytes;
/// trailing zero bytes of a unit dropped).
pub fn annex_b_nals(b: &[u8]) -> impl Iterator<Item = &[u8]> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 3 <= b.len() {
        if b[i] == 0 && b[i + 1] == 0 && b[i + 2] == 1 {
            starts.push(i + 3);
            i += 3;
        } else {
            i += 1;
        }
    }
    let ends: Vec<usize> = starts
        .iter()
        .skip(1)
        .map(|&s| s - 3)
        .chain(std::iter::once(b.len()))
        .collect();
    starts.into_iter().zip(ends).map(move |(s, e)| {
        let mut e = e;
        while e > s && b[e - 1] == 0 {
            e -= 1;
        }
        &b[s..e]
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    /// BS17's (NHK BSP4K) VPS and SPS.
    const VPS: &str = "40010c06ffff022000000300b00000030000030099000018824090";
    const SPS: &str = "420106022000000300b000000300000300990000a001e020021c4d8d18826490a594e0284244826ca500000303e90000ea60803e9c00814bc00004c4b4000004c5ec5c00004c4b4000004c5ec5c00004c4b4000004c5ec5c00004c4b4000004c5ec440";

    #[test]
    fn sps_of_bs17() {
        let s = parse_sps(&hex(SPS)).unwrap();
        assert_eq!((s.width, s.height), (3840, 2160));
        assert_eq!(
            (s.chroma_format, s.bit_depth_luma, s.bit_depth_chroma),
            (1, 10, 10)
        );
        assert_eq!(s.sub_layers, 4);
        assert!(!s.temporal_id_nested);
        assert_eq!(s.general_ptl.to_vec(), hex("0220000000b0000000000099"));
        assert_eq!(s.log2_max_poc_lsb, 16);
        assert!(!s.separate_colour_plane);
    }

    /// BS17's PPS and the heads of the first slices of a CRA and the
    /// RASL_R, RASL_R, RASL_N after it from the recording (ffmpeg's
    /// `trace_headers`: `slice_pic_order_cnt_lsb` 40856, 40852, 40850,
    /// 40849).
    #[test]
    fn slice_poc_of_bs17() {
        let sps = parse_sps(&hex(SPS)).unwrap();
        let pps = parse_pps(&hex("4401c076")).unwrap();
        assert_eq!(
            pps,
            Pps {
                dependent_slice_segments: false,
                output_flag_present: false,
                extra_slice_header_bits: 0,
            }
        );
        let poc = |h: &str| first_slice_poc_lsb(&hex(h), &sps, &pps);
        assert_eq!(poc("2a01ae7e60488219"), Some(40856));
        assert_eq!(poc("1202f3f286894272"), Some(40852));
        assert_eq!(poc("1203f3f246d5148c"), Some(40850));
        assert_eq!(poc("1004f3f2244f4232"), Some(40849));
        // Cut short; not a picture's first slice; not a slice; an IDR.
        assert_eq!(poc("1202f3"), None);
        assert_eq!(poc("12027ff286894272"), None);
        assert_eq!(poc("4401c076"), None);
        assert_eq!(poc("2601ac"), Some(0));
    }

    #[test]
    fn hvcc_header_equals_ffmpegs() {
        let vps = hex(VPS);
        let sps = hex(SPS);
        let pps = hex("4401c076");
        let c = hvcc(&[&vps], &[&sps], &[&pps], &[]).unwrap();
        // ffmpeg's hvcC of the same stream: the 23 header bytes.
        assert_eq!(
            &c[..23],
            &hex("010220000000b0000000000099f000fcfdfafa00002303")[..]
        );
        assert_eq!(c[22], 3);
        assert_eq!(&c[23..26], &[0xa0, 0, 1], "the VPS array, complete");
    }

    #[test]
    fn annex_b_round_trip() {
        let au = hex("00000001460150000001400102000001420103000000");
        let out = annex_b_to_length(&au, |n| (n[0] >> 1) & 0x3f != 35);
        assert_eq!(out, hex("0000000340010200000003420103"));
    }
}

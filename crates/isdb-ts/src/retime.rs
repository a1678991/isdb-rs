//! In-place retiming of a 188-byte Transport Stream packet: the PCR, the
//! PTS/DTS of a PES header that starts in the packet, and the
//! continuity_counter.
//!
//! These are the edits a re-multiplexer makes to packets it passes through.
//! H.222.0 Annex D.0.4: "correction of PCRs is necessary in a re-multiplex
//! operation, creating a new Transport Stream from one or more Transport
//! Streams. This correction is accomplished by adding a correction term to
//! the PCR" ([`add_to_pcr`]). A source that is played in a loop also needs
//! every timestamp moved by the loop period at each pass
//! ([`shift_pes_timestamps`]) and its counters renumbered
//! ([`set_continuity_counter`]).
//!
//! Each function edits only the bits of its field. Every other bit of the
//! packet stays as it was: the reserved bits between PCR base and
//! extension (Table 2-6), the PTS/DTS prefixes and marker bits
//! (Table 2-21), the OPCR, and the rest of the header. The arithmetic is
//! modular: PCR modulo [`PCR_MODULUS`](crate::pcr::PCR_MODULUS) =
//! 2^33·300 (§2.4.2.2 equations 2-1..2-3), PTS/DTS modulo 2^33 (§2.4.3.7,
//! equations 2-11/2-12). A correction backwards by `d` is therefore an
//! addition of `PCR_MODULUS − d` or of `2^33 − d`.
//!
//! The functions take the packet as `[u8; PACKET_SIZE]`, the type
//! [`Header::of_packet`](crate::packet::Header::of_packet) and
//! [`CcChecker::push`](crate::cc::CcChecker::push) use, and find the fields
//! through [`View::parse`].

use crate::cc::ContinuityCounter;
use crate::packet::{PACKET_SIZE, View};
use crate::pcr::{BASE_MODULUS, Pcr, Timestamp, pcr_wrap_add, ts90k_wrap_add};
use crate::pes::{START_CODE_PREFIX, StreamIdClass, classify_stream_id, has_optional_header};

/// Offset of the PCR field from the start of the packet, when there is one:
/// 4 header bytes, adaptation_field_length, the flags byte (Table 2-6).
const PCR_OFFSET: usize = 6;

/// Offset of PTS_DTS_flags' byte in a PES packet (Table 2-21): start code
/// prefix (3), stream_id (1), PES_packet_length (2), '10' … original_or_copy
/// (1), then PTS_DTS_flags in the top 2 bits.
const PES_FLAGS2: usize = 7;
/// Offset of PES_header_data_length in a PES packet (Table 2-21).
const PES_HEADER_DATA_LENGTH: usize = 8;
/// Offset of the first optional field (the PTS, if present) in a PES packet.
const PES_OPTIONAL_FIELDS: usize = 9;

/// Byte offset of the PCR field (program_clock_reference_base, reserved,
/// program_clock_reference_extension; H.222.0 §2.4.3.4 Table 2-6) in
/// `packet`, or `None` if the packet carries no PCR.
///
/// A packet carries a PCR when adaptation_field_control is '10' or '11'
/// (Table 2-2), adaptation_field_length is greater than 0 (Table 2-6
/// `if (adaptation_field_length > 0)`), PCR_flag is '1' (§2.4.3.5), and the
/// adaptation field is long enough to hold the 6-byte field (length ≥ 7).
/// A packet whose adaptation_field_length runs past the packet carries none,
/// as for [`View::pcr`], which this agrees with.
///
/// The offset is always 6 when present (the PCR is the first optional field
/// of the adaptation field); it is returned so that callers index the field
/// without restating the layout.
pub fn pcr_field(packet: &[u8; PACKET_SIZE]) -> Option<usize> {
    let af = View::parse(packet).ok()?.adaptation_field?;
    // `af` starts at adaptation_field_length; af[1] is the flags byte.
    (af.len() >= 2 + Pcr::LEN && af[1] & 0x10 != 0).then_some(PCR_OFFSET)
}

/// Add `d` ticks of 27 MHz to the packet's PCR in place: `PCR := (PCR + d)
/// mod 2^33·300` (H.222.0 §2.4.2.2 equations 2-1..2-3; the correction term
/// ΔPCR of Annex D.0.4). Returns `false`, with the packet unchanged, when
/// [`pcr_field`] finds no PCR.
///
/// The value is read as `PCR_base × 300 + PCR_ext` (equation 2-1) and
/// written back split per equations 2-2/2-3, so the extension written is
/// always in 0..300, even if the one read was in the out-of-range 300..=511
/// (see SPEC-NOTES "retiming"). The 6 reserved bits between base and
/// extension (Table 2-6) are kept as they were, and the OPCR, if any, is not
/// touched: it carries the original encoder's clock (§2.4.3.5). `d` may
/// exceed [`PCR_MODULUS`](crate::pcr::PCR_MODULUS); it is reduced first. To subtract `d`, add
/// `PCR_MODULUS − d % PCR_MODULUS`.
pub fn add_to_pcr(packet: &mut [u8; PACKET_SIZE], d: u64) -> bool {
    let Some(at) = pcr_field(packet) else {
        return false;
    };
    let Some(field) = packet.get_mut(at..at + Pcr::LEN) else {
        return false;
    };
    let (base, ext) = read_pcr(field);
    let t = pcr_wrap_add(base * 300 + u64::from(ext), d);
    write_pcr(field, t / 300, t % 300);
    true
}

/// The 33-bit base and 9-bit extension of a 6-byte PCR field (Table 2-6:
/// base 33, reserved 6, extension 9, MSB first).
fn read_pcr(f: &[u8]) -> (u64, u16) {
    let base = u64::from(f[0]) << 25
        | u64::from(f[1]) << 17
        | u64::from(f[2]) << 9
        | u64::from(f[3]) << 1
        | u64::from(f[4] >> 7);
    let ext = u16::from(f[4] & 1) << 8 | u16::from(f[5]);
    (base, ext)
}

/// Write `base` (< 2^33) and `ext` (< 300) into a 6-byte PCR field, keeping
/// the 6 reserved bits (bits 6..1 of byte 4).
fn write_pcr(f: &mut [u8], base: u64, ext: u64) {
    debug_assert!(base < BASE_MODULUS && ext < 300);
    // Each `as u8` takes the 8 (or fewer) bits selected by the shift and mask.
    #[allow(clippy::cast_possible_truncation)]
    {
        f[0] = (base >> 25) as u8;
        f[1] = (base >> 17) as u8;
        f[2] = (base >> 9) as u8;
        f[3] = (base >> 1) as u8;
        f[4] = ((base & 1) as u8) << 7 | (f[4] & 0x7E) | ((ext >> 8) & 1) as u8;
        f[5] = ext as u8;
    }
}

/// Result of [`shift_pes_timestamps`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PesShift {
    /// A PES header starts in the packet and its PTS (and DTS, if present)
    /// were moved.
    Shifted,
    /// Nothing to move: the packet does not start a PES packet that has
    /// PTS/DTS fields. Sections, packets without payload_unit_start_indicator
    /// and PES packets without the optional header or with PTS_DTS_flags
    /// '00' end here. The packet is unchanged.
    NoHeader,
    /// A PES packet starts in the packet, but its header ends after the last
    /// byte of the packet before the PTS/DTS fields do, so they could not be
    /// moved here. The packet is unchanged.
    Truncated,
}

/// Add `d90` ticks of 90 kHz to the PTS and DTS of the PES packet that
/// starts in `packet`, in place: `PTS := (PTS + d90) mod 2^33`, and the same
/// for the DTS (H.222.0 §2.4.3.7, equations 2-11/2-12).
///
/// Only packets with payload_unit_start_indicator '1' are looked at: for PES
/// data that bit says "the payload of this Transport Stream packet will
/// commence with the first byte of a PES packet" (§2.4.3.3). The payload has
/// to start with packet_start_code_prefix 0x000001 followed by a stream_id
/// of Table 2-22 that has the optional header ([`has_optional_header`], the
/// first `if` of Table 2-21). PTS_DTS_flags '10' moves the PTS, '11' moves
/// the PTS and the DTS (Table 2-21); '00' and the forbidden '01' carry
/// neither.
///
/// Only the 33 value bits change. The 4-bit prefix of each field ('0010',
/// '0011', '0001') and its three marker bits stay as they were
/// (Table 2-21), and so does everything else in the header, ESCR included.
/// `d90` may exceed 2^33; it is reduced first. To subtract `d90`, add
/// `2^33 − d90 % 2^33`.
///
/// Returns [`PesShift::NoHeader`] for a payload that is not such a PES
/// header: a PSI section (its pointer_field and table_id cannot read as
/// 0x000001 followed by a stream_id ≥ 0xBC, see SPEC-NOTES "retiming"), a
/// packet without payload, a payload whose transport_scrambling_control is
/// not '00' (the header is not readable), or a header whose
/// PES_header_data_length is too short for the PTS/DTS its flags announce
/// (malformed: the bytes are left alone). Returns [`PesShift::Truncated`]
/// when what the packet holds of the payload is consistent with such a header
/// but ends before the PTS/DTS fields do. In both cases the packet is
/// unchanged.
pub fn shift_pes_timestamps(packet: &mut [u8; PACKET_SIZE], d90: u64) -> PesShift {
    let Ok(view) = View::parse(packet) else {
        return PesShift::NoHeader;
    };
    let h = view.header;
    if !h.payload_unit_start_indicator || h.transport_scrambling_control.is_scrambled() {
        return PesShift::NoHeader;
    }
    let Some(payload) = view.payload else {
        return PesShift::NoHeader;
    };
    let start = PACKET_SIZE - payload.len();
    let p = &mut packet[start..];
    match pes_timestamp_count(p) {
        Locate::No => PesShift::NoHeader,
        Locate::Short => PesShift::Truncated,
        Locate::At(n) => {
            for i in 0..n {
                let at = PES_OPTIONAL_FIELDS + i * Timestamp::LEN;
                shift_timestamp(&mut p[at..at + Timestamp::LEN], d90);
            }
            PesShift::Shifted
        }
    }
}

/// Where the PTS/DTS fields of a PES payload are.
enum Locate {
    /// Not a PES header with PTS/DTS.
    No,
    /// Consistent with one, but the payload ends first.
    Short,
    /// `n` (1 or 2) 5-byte fields from [`PES_OPTIONAL_FIELDS`] on, all inside
    /// the payload.
    At(usize),
}

/// Check the start of a payload `p` against Table 2-21 as far as it goes.
fn pes_timestamp_count(p: &[u8]) -> Locate {
    if p.is_empty() {
        return Locate::No;
    }
    if p.iter().zip(START_CODE_PREFIX).any(|(&b, s)| b != s) {
        return Locate::No;
    }
    if let Some(&id) = p.get(3)
        && (classify_stream_id(id) == StreamIdClass::NotStreamId || !has_optional_header(id))
    {
        return Locate::No;
    }
    let n = match p.get(PES_FLAGS2).map(|b| b >> 6) {
        Some(0b10) => 1,
        Some(0b11) => 2,
        Some(_) => return Locate::No,
        None => return Locate::Short,
    };
    match p.get(PES_HEADER_DATA_LENGTH) {
        Some(&hdl) if usize::from(hdl) < n * Timestamp::LEN => Locate::No,
        Some(_) if p.len() >= PES_OPTIONAL_FIELDS + n * Timestamp::LEN => Locate::At(n),
        _ => Locate::Short,
    }
}

/// Add `d90` to the 33-bit value of a 5-byte PTS/DTS field (Table 2-21:
/// prefix 4, [32..30] 3, marker, [29..15] 15, marker, [14..0] 15, marker),
/// keeping the prefix and the marker bits.
fn shift_timestamp(f: &mut [u8], d90: u64) {
    let v = u64::from((f[0] >> 1) & 0x07) << 30
        | u64::from(f[1]) << 22
        | u64::from(f[2] >> 1) << 15
        | u64::from(f[3]) << 7
        | u64::from(f[4] >> 1);
    let v = ts90k_wrap_add(v, d90 % BASE_MODULUS);
    // Each `as u8` takes the 8 (or fewer) bits selected by the shift and mask.
    #[allow(clippy::cast_possible_truncation)]
    {
        f[0] = (f[0] & 0xF1) | (((v >> 30) & 0x07) as u8) << 1;
        f[1] = (v >> 22) as u8;
        f[2] = (f[2] & 0x01) | (((v >> 15) & 0x7F) as u8) << 1;
        f[3] = (v >> 7) as u8;
        f[4] = (f[4] & 0x01) | ((v & 0x7F) as u8) << 1;
    }
}

/// The continuity_counter of `packet` (H.222.0 §2.4.3.3, the low 4 bits of
/// byte 3, Table 2-2). The same value as
/// `Header::of_packet(packet).continuity_counter`.
pub const fn continuity_counter(packet: &[u8; PACKET_SIZE]) -> ContinuityCounter {
    ContinuityCounter::from_wire(packet[3])
}

/// Overwrite the continuity_counter of `packet` (H.222.0 §2.4.3.3,
/// Table 2-2) with `cc`; the rest of byte 3 (transport_scrambling_control,
/// adaptation_field_control) is kept.
///
/// Which value is continuous is the caller's business: §2.4.3.3 increments
/// the counter only on packets with payload ("shall not be incremented when
/// the adaptation_field_control of the packet equals '00' or '10'"), see
/// [`Header::has_payload`](crate::packet::Header::has_payload) and [`crate::cc::Counter`].
pub fn set_continuity_counter(packet: &mut [u8; PACKET_SIZE], cc: ContinuityCounter) {
    packet[3] = (packet[3] & 0xF0) | cc.get();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Pid;
    use crate::bits::{BitReader, BitWriter};
    use crate::cc::Counter;
    use crate::packet::{AdaptationFields, Header, NULL_PACKET, Packet};
    use crate::packetize::{Packing, packetize_sections};
    use crate::pcr::PCR_MODULUS;
    use crate::pes::{PesHeader, PesPacket, PtsDts, packetize, stream_id};
    use crate::psi::{Pat, PatEntry};

    fn pid(v: u16) -> Pid {
        Pid::new(v).unwrap()
    }

    /// An adaptation-only packet ('10') with `pcr` and, if given, an OPCR.
    fn pcr_packet(pcr: Pcr, opcr: Option<Pcr>) -> [u8; PACKET_SIZE] {
        let fields = AdaptationFields {
            pcr: Some(pcr),
            opcr,
            ..AdaptationFields::default()
        };
        Packet::with_adaptation(
            pid(0x0100),
            false,
            ContinuityCounter::ZERO,
            Some(fields),
            &[],
        )
        .unwrap()
        .encode()
        .unwrap()
    }

    fn decoded_pcr(p: &[u8; PACKET_SIZE]) -> Pcr {
        View::parse(p).unwrap().pcr().unwrap()
    }

    /// The first TS packet of a PES packet with `pts_dts` on PID 0x0111.
    fn pes_first_packet(sid: u8, pts_dts: PtsDts) -> [u8; PACKET_SIZE] {
        let header = PesHeader {
            pts_dts,
            ..PesHeader::default()
        };
        let pes = PesPacket::new(sid, header, vec![0xA5; 400])
            .encode()
            .unwrap();
        packetize(pid(0x0111), &pes, None, &mut Counter::default()).unwrap()[0]
    }

    fn pes_pts_dts(p: &[u8; PACKET_SIZE]) -> PtsDts {
        let payload = View::parse(p).unwrap().payload.unwrap();
        let (h, _) = PesHeader::decode(&payload[6..]).unwrap();
        h.pts_dts
    }

    #[test]
    fn spec_table_2_6_pcr_field_offset() {
        let p = pcr_packet(Pcr::from_ticks(12_345), None);
        assert_eq!(pcr_field(&p), Some(6));
        assert_eq!(
            Pcr::decode(&p[6..12]).unwrap(),
            Pcr::from_ticks(12_345),
            "offset 6 is program_clock_reference_base"
        );
        // No adaptation field ('01').
        assert_eq!(pcr_field(&NULL_PACKET), None);
        // adaptation_field_length 0: the single stuffing byte, no flags.
        let mut p = NULL_PACKET;
        p[3] = 0x30; // '11'
        p[4] = 0;
        p[5] = 0x10; // would be PCR_flag if the AF had a flags byte
        assert_eq!(pcr_field(&p), None);
        // PCR_flag set but the AF too short for the field.
        p[4] = 6;
        assert_eq!(pcr_field(&p), None);
        p[4] = 7;
        assert_eq!(pcr_field(&p), Some(6));
        // PCR_flag clear.
        p[5] = 0x00;
        assert_eq!(pcr_field(&p), None);
        // adaptation_field_control '00' (reserved): no adaptation field.
        let mut p = pcr_packet(Pcr::from_ticks(1), None);
        p[3] &= 0xCF;
        assert_eq!(pcr_field(&p), None);
        // adaptation_field_length past the packet.
        let mut p = pcr_packet(Pcr::from_ticks(1), None);
        p[4] = 184;
        assert_eq!(pcr_field(&p), None);
    }

    /// Table 2-6 / eq. 2-1..2-3: the sum wraps at 2^33·300, the extension
    /// carries into the base at 300, the reserved bits and the OPCR stay.
    #[test]
    fn spec_table_2_6_pcr_add_wraps() {
        let opcr = Pcr {
            base: 0x1_2345_6789,
            reserved: 0x3F,
            extension: 123,
        };
        let pcr = Pcr {
            base: BASE_MODULUS - 1,
            reserved: 0b10_1010, // non-'1' reserved bits must survive
            extension: 299,
        };
        let mut p = pcr_packet(pcr, Some(opcr));
        let before = p;
        assert_eq!(pcr.ticks(), PCR_MODULUS - 1);
        assert!(add_to_pcr(&mut p, 1));
        let got = decoded_pcr(&p);
        assert_eq!((got.base, got.extension), (0, 0), "wrap to zero");
        assert_eq!(got.reserved, 0b10_1010);
        // ext carry: 299 + 1 → base + 1, ext 0.
        assert!(add_to_pcr(&mut p, 299));
        assert!(add_to_pcr(&mut p, 1));
        let got = decoded_pcr(&p);
        assert_eq!((got.base, got.extension), (1, 0));
        // d larger than the modulus is reduced.
        assert!(add_to_pcr(&mut p, 3 * PCR_MODULUS + 301));
        let got = decoded_pcr(&p);
        assert_eq!((got.base, got.extension), (2, 1));
        // Only the 6 PCR bytes changed; the OPCR is untouched.
        for (i, (a, b)) in before.iter().zip(&p).enumerate() {
            if !(6..12).contains(&i) {
                assert_eq!(a, b, "byte {i}");
            }
        }
        let fields = Packet::decode(&p).unwrap();
        assert_eq!(
            fields.adaptation_field.unwrap().fields().unwrap().opcr,
            Some(opcr)
        );
        // Packet without PCR: false and unchanged.
        let mut n = NULL_PACKET;
        assert!(!add_to_pcr(&mut n, 5));
        assert_eq!(n, NULL_PACKET);
    }

    /// Eq. 2-1: an out-of-range extension (300..=511) is read as base·300 +
    /// ext and the sum written back in range.
    #[test]
    fn pcr_out_of_range_extension_is_normalised() {
        let mut p = pcr_packet(
            Pcr {
                base: 10,
                reserved: 0x3F,
                extension: 511,
            },
            None,
        );
        assert!(add_to_pcr(&mut p, 0));
        let got = decoded_pcr(&p);
        assert_eq!(got.ticks(), 10 * 300 + 511);
        assert_eq!((got.base, got.extension), (11, 211));
    }

    /// Table 2-21: PTS only ('10'), prefix '0010' and markers kept, value
    /// shifted mod 2^33.
    #[test]
    fn spec_table_2_21_pts_only_prefix_and_markers_kept() {
        let mut p = pes_first_packet(stream_id::AUDIO_FIRST, PtsDts::pts_only(BASE_MODULUS - 2));
        let before = p;
        assert_eq!(shift_pes_timestamps(&mut p, 5), PesShift::Shifted);
        let PtsDts::Pts(t) = pes_pts_dts(&p) else {
            panic!("PTS_DTS_flags changed");
        };
        assert_eq!(t.value, 3);
        assert_eq!(t.prefix, Timestamp::PREFIX_PTS_ONLY);
        assert_eq!(t.markers, 0b111);
        // Only the 5 PTS bytes (payload offset 9..14, packet offset 13..18)
        // may differ.
        for (i, (a, b)) in before.iter().zip(&p).enumerate() {
            if !(13..18).contains(&i) {
                assert_eq!(a, b, "byte {i}");
            }
        }
    }

    /// Table 2-21: PTS+DTS ('11'), prefixes '0011'/'0001' and markers kept.
    /// Non-'1' markers and an odd prefix are kept verbatim too.
    #[test]
    fn spec_table_2_21_pts_dts_prefixes_and_markers_kept() {
        let pts = 0x1_0000_0000 + 0x7FFF;
        let dts = 0x0_ABCD_EF01;
        let mut p = pes_first_packet(stream_id::VIDEO_FIRST, PtsDts::pts_and_dts(pts, dts));
        assert_eq!(shift_pes_timestamps(&mut p, 90_000), PesShift::Shifted);
        let PtsDts::PtsDts(tp, td) = pes_pts_dts(&p) else {
            panic!("PTS_DTS_flags changed");
        };
        assert_eq!(tp.value, pts + 90_000);
        assert_eq!(td.value, dts + 90_000);
        assert_eq!((tp.prefix, td.prefix), (0b0011, 0b0001));
        assert_eq!((tp.markers, td.markers), (0b111, 0b111));

        // Odd markers / prefix bits (kept losslessly by the decoder) stay.
        let odd = PtsDts::PtsDts(
            Timestamp {
                prefix: 0b1011,
                value: 7,
                markers: 0b010,
            },
            Timestamp {
                prefix: 0b0101,
                value: BASE_MODULUS - 1,
                markers: 0b101,
            },
        );
        let mut p = pes_first_packet(stream_id::PRIVATE_STREAM_1, odd);
        assert_eq!(
            shift_pes_timestamps(&mut p, BASE_MODULUS + 1),
            PesShift::Shifted
        );
        let PtsDts::PtsDts(tp, td) = pes_pts_dts(&p) else {
            panic!("PTS_DTS_flags changed");
        };
        assert_eq!((tp.prefix, tp.value, tp.markers), (0b1011, 8, 0b010));
        assert_eq!((td.prefix, td.value, td.markers), (0b0101, 0, 0b101));
    }

    /// The bit packing agrees with `Timestamp::read`/`write` for every bit.
    #[test]
    fn shift_timestamp_matches_the_codec() {
        for bit in 0..33 {
            for prefix in [0b0010, 0b0011, 0b0001] {
                let t = Timestamp {
                    prefix,
                    value: 1 << bit,
                    markers: 0b111,
                };
                let mut w = BitWriter::new();
                t.write(&mut w).unwrap();
                let mut f = w.into_bytes();
                shift_timestamp(&mut f, (1 << bit) + 1);
                let back = Timestamp::read(&mut BitReader::new(&f, "t")).unwrap();
                assert_eq!(back.value, ((2u64 << bit) + 1) % BASE_MODULUS);
                assert_eq!((back.prefix, back.markers), (prefix, 0b111));
            }
        }
    }

    #[test]
    fn sections_are_untouched() {
        let pat = Pat::section(1, 0, vec![PatEntry::new(1, pid(0x01F0))])
            .encode()
            .unwrap();
        let pkts = packetize_sections(
            Pid::PAT,
            &[pat],
            Packing::OnePerPacket,
            &mut Counter::default(),
        )
        .unwrap();
        let mut p = pkts[0];
        assert_ne!(p[1] & 0x40, 0, "PUSI set");
        assert_eq!(shift_pes_timestamps(&mut p, 12_345), PesShift::NoHeader);
        assert_eq!(p, pkts[0]);
        // A section whose pointer_field is 0 and table_id 0, as close to
        // 00 00 01 as a PAT gets: section_syntax_indicator '1' makes the
        // third byte ≥ 0x80, never 0x01.
        assert_eq!(&p[4..6], &[0x00, 0x00]);
        assert!(p[6] >= 0x80);
    }

    #[test]
    fn non_pusi_and_non_pes_packets_are_untouched() {
        let first = pes_first_packet(stream_id::VIDEO_FIRST, PtsDts::pts_only(1));
        // Same packet with PUSI cleared.
        let mut p = first;
        p[1] &= !0x40;
        let before = p;
        assert_eq!(shift_pes_timestamps(&mut p, 1), PesShift::NoHeader);
        assert_eq!(p, before);
        // A continuation packet of the same PES.
        let header = PesHeader {
            pts_dts: PtsDts::pts_only(1),
            ..PesHeader::default()
        };
        let pes = PesPacket::new(stream_id::VIDEO_FIRST, header, vec![0; 400])
            .encode()
            .unwrap();
        let pkts = packetize(pid(0x0111), &pes, None, &mut Counter::default()).unwrap();
        let mut p = pkts[1];
        assert_eq!(shift_pes_timestamps(&mut p, 1), PesShift::NoHeader);
        assert_eq!(p, pkts[1]);
        // Null packet, PCR-only packet (no payload).
        let mut p = NULL_PACKET;
        assert_eq!(shift_pes_timestamps(&mut p, 1), PesShift::NoHeader);
        let mut p = pcr_packet(Pcr::from_ticks(9), None);
        let before = p;
        assert_eq!(shift_pes_timestamps(&mut p, 1), PesShift::NoHeader);
        assert_eq!(p, before);
        // PTS_DTS_flags '00'.
        let mut p = pes_first_packet(stream_id::VIDEO_FIRST, PtsDts::None);
        let before = p;
        assert_eq!(shift_pes_timestamps(&mut p, 1), PesShift::NoHeader);
        assert_eq!(p, before);
        // stream_ids without the optional header (Table 2-21 first `if`):
        // the bytes after PES_packet_length are data, not flags.
        for sid in [
            stream_id::PRIVATE_STREAM_2,
            stream_id::PADDING_STREAM,
            stream_id::ECM_STREAM,
            stream_id::DSMCC_STREAM,
        ] {
            let mut p = first;
            p[4 + 3] = sid;
            let before = p;
            assert_eq!(
                shift_pes_timestamps(&mut p, 1),
                PesShift::NoHeader,
                "{sid:#x}"
            );
            assert_eq!(p, before);
        }
        // 00 00 01 followed by a value below 0xBC is not a stream_id.
        let mut p = first;
        p[4 + 3] = 0xB3;
        assert_eq!(shift_pes_timestamps(&mut p, 1), PesShift::NoHeader);
        // Scrambled payload: not readable.
        let mut p = first;
        p[3] |= 0xC0;
        let before = p;
        assert_eq!(shift_pes_timestamps(&mut p, 1), PesShift::NoHeader);
        assert_eq!(p, before);
        // PES_header_data_length too short for the flagged PTS.
        let mut p = first;
        p[4 + 8] = 4;
        let before = p;
        assert_eq!(shift_pes_timestamps(&mut p, 1), PesShift::NoHeader);
        assert_eq!(p, before);
    }

    /// The PES header begins near the end of the packet (a long adaptation
    /// field in front): the PTS/DTS fields do not fit.
    #[test]
    fn truncated_header_is_reported_and_untouched() {
        let header = PesHeader {
            pts_dts: PtsDts::pts_and_dts(1000, 900),
            ..PesHeader::default()
        };
        let pes = PesPacket::new(stream_id::VIDEO_FIRST, header, Vec::new())
            .encode()
            .unwrap();
        assert_eq!(pes.len(), 19);
        for keep in 1..pes.len() {
            // A first packet whose payload holds only `keep` bytes of the PES.
            let p = Packet::with_adaptation(
                pid(0x0111),
                true,
                ContinuityCounter::ZERO,
                Some(AdaptationFields::default()),
                &pes[..keep],
            )
            .unwrap();
            let raw = p.encode().unwrap();
            let mut q = raw;
            assert_eq!(
                shift_pes_timestamps(&mut q, 1),
                PesShift::Truncated,
                "keep {keep}"
            );
            assert_eq!(q, raw, "keep {keep}");
        }
        // The whole header fits: shifted.
        let p = Packet::with_adaptation(
            pid(0x0111),
            true,
            ContinuityCounter::ZERO,
            Some(AdaptationFields::default()),
            &pes,
        )
        .unwrap();
        let mut q = p.encode().unwrap();
        assert_eq!(shift_pes_timestamps(&mut q, 1), PesShift::Shifted);
        assert_eq!(pes_pts_dts(&q), PtsDts::pts_and_dts(1001, 901));
        // PTS-only header cut inside the PTS.
        let header = PesHeader {
            pts_dts: PtsDts::pts_only(5),
            ..PesHeader::default()
        };
        let pes = PesPacket::new(stream_id::AUDIO_FIRST, header, Vec::new())
            .encode()
            .unwrap();
        let p = Packet::with_adaptation(
            pid(0x0112),
            true,
            ContinuityCounter::ZERO,
            Some(AdaptationFields::default()),
            &pes[..12],
        )
        .unwrap();
        let mut q = p.encode().unwrap();
        assert_eq!(shift_pes_timestamps(&mut q, 1), PesShift::Truncated);
    }

    #[test]
    fn spec_2_4_3_3_continuity_counter_get_set() {
        let mut p = pcr_packet(Pcr::from_ticks(1), None);
        p[3] |= 0xC0; // scrambling bits must survive
        let afc_and_tsc = p[3] & 0xF0;
        for v in 0..16 {
            let cc = ContinuityCounter::new(v).unwrap();
            set_continuity_counter(&mut p, cc);
            assert_eq!(continuity_counter(&p), cc);
            assert_eq!(Header::of_packet(&p).continuity_counter, cc);
            assert_eq!(p[3] & 0xF0, afc_and_tsc);
        }
    }
}

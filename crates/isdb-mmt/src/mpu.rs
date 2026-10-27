//! MPU payloads (MMTP payload type 00h, ARIB STD-B60 v1.14 §6.3.2,
//! Table 6-1) and their MFUs: the payload header, aggregated and divided
//! MFUs, and per-asset reassembly with loss detection.
//!
//! Broadcasting sends no MPU metadata or movie fragment metadata (§6.2:
//! "MPU metadata and movie fragment metadata shall not be transmitted");
//! every payload is an MFU ([`FragmentType::Mfu`], Table 6-2 code 2). An
//! MFU of timed data is a sample or sub-sample: for HEVC one NAL unit with
//! a 32-bit length prefix (§8.1.1), for MPEG-4 audio one AudioMuxElement
//! (§8.2.1), for captions one subsample with the header of Table 9-1.
//!
//! An MFU divided over several payloads ([`FragmentationIndicator`]
//! `Head`, `Middle`, `Last`, Table 6-3 codes 01, 10, 11; fragment_counter
//! counting the payloads still to come) is reassembled; a gap in the MMTP
//! packet sequence numbers of the packet id drops the MFU being
//! reassembled and is counted.

use std::fmt;

/// Largest MFU kept while reassembling: a NAL unit of a 4K picture is a
/// few MB at most.
pub const MAX_MFU: usize = 16 << 20;

/// `fragment_type` of an MPU payload: ARIB STD-B60 v1.14 §6.3.2, Table
/// 6-2 (4 bits).
///
/// A lossless coded enum: codes 0–2 are named, 3–15 ("provided by the
/// Ministry or private standardization organization") are kept in
/// [`FragmentType::Reserved`], so a decoded header re-encodes bit-exactly.
/// The `Reserved` payload is a [`ReservedCode`] only `from_code` can
/// build, so every value is canonical and `==` is wire equality.
/// Table 6-2 note 1: codes 0 and 1 "are provided in MMT Standard, but are
/// not used in this standard" (§6.2: the metadata is not transmitted), so
/// on air every payload is [`Mfu`](FragmentType::Mfu).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FragmentType {
    /// 0: "MPU metadata. ftyp, mmpu, moov, meta box are involved."
    MpuMetadata,
    /// 1: "Movie fragment metadata. moof box and, mdat box excluding media
    /// data are involved."
    MovieFragmentMetadata,
    /// 2: "MFU. Sample or sub-sample (media data with time), or items
    /// (media data without time) are involved."
    Mfu,
    /// 3–15: "provided by the Ministry or private standardization
    /// organization"; the 4-bit code as decoded.
    Reserved(ReservedCode),
}

/// A fragment type Table 6-2 does not assign (3–15), as decoded.
///
/// Only [`FragmentType::from_code`] builds one, after it has excluded the
/// three assigned codes: the field is private, so a
/// [`Reserved`](FragmentType::Reserved) value is canonical by construction
/// (one value per wire code) and `==` on the enum is wire equality.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ReservedCode(u8);

impl ReservedCode {
    /// `c` must be a reserved code of Table 6-2, already masked to 4 bits;
    /// only `from_code` upholds this.
    pub(crate) const fn new_unchecked(c: u8) -> Self {
        Self(c)
    }

    /// The 4-bit code.
    pub const fn get(self) -> u8 {
        self.0
    }
}

impl fmt::Display for ReservedCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

impl fmt::Binary for ReservedCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Binary::fmt(&self.0, f)
    }
}

impl fmt::LowerHex for ReservedCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::LowerHex::fmt(&self.0, f)
    }
}

/// A reserved code cannot be spelled as a [`FragmentType`] outside this
/// crate; the only way in is [`FragmentType::from_code`], so a decoded
/// value is canonical:
///
/// ```compile_fail,E0624
/// use isdb_mmt::mpu::{FragmentType, ReservedCode};
/// let _ = FragmentType::Reserved(ReservedCode::new_unchecked(7));
/// ```
///
/// ```
/// use isdb_mmt::mpu::FragmentType;
/// assert_eq!(FragmentType::from_code(7), FragmentType::from_code(0xF7));
/// assert_eq!(FragmentType::from_code(7).code(), 7);
/// ```
#[doc(hidden)]
pub const _FRAGMENT_TYPE_CANONICAL: () = ();

impl FragmentType {
    /// The three assigned codes, in Table 6-2 order.
    pub const ASSIGNED: [FragmentType; 3] = [
        FragmentType::MpuMetadata,
        FragmentType::MovieFragmentMetadata,
        FragmentType::Mfu,
    ];

    /// From the 4-bit field (`code` is masked to the width; total).
    pub const fn from_code(code: u8) -> Self {
        match code & 0x0F {
            0 => FragmentType::MpuMetadata,
            1 => FragmentType::MovieFragmentMetadata,
            2 => FragmentType::Mfu,
            c => FragmentType::Reserved(ReservedCode::new_unchecked(c)),
        }
    }

    /// The 4-bit code.
    pub const fn code(self) -> u8 {
        match self {
            FragmentType::MpuMetadata => 0,
            FragmentType::MovieFragmentMetadata => 1,
            FragmentType::Mfu => 2,
            FragmentType::Reserved(c) => c.get(),
        }
    }
}

impl fmt::Display for FragmentType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.code())
    }
}

/// `fragmentation_indicator` of an MPU payload: ARIB STD-B60 v1.14
/// §6.3.2, Table 6-3 (2 bits). Table 6-3 assigns all four codes, so the
/// enum needs no reserved variant and `from_code` is still total.
///
/// The variant names follow the table's words: "perfect form", "header
/// part of divided data", "neither header part nor last part", "last part
/// of divided data".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FragmentationIndicator {
    /// 00: "This involves one or more than one data in perfect form."
    Complete,
    /// 01: "This involves header part of divided data."
    Head,
    /// 10: "This involves a part of divided data which is neither header
    /// part nor last part."
    Middle,
    /// 11: "This involves last part of divided data."
    Last,
}

impl FragmentationIndicator {
    /// The four codes, in Table 6-3 order.
    pub const ALL: [FragmentationIndicator; 4] = [
        FragmentationIndicator::Complete,
        FragmentationIndicator::Head,
        FragmentationIndicator::Middle,
        FragmentationIndicator::Last,
    ];

    /// From the 2-bit field (`code` is masked to the width; total).
    pub const fn from_code(code: u8) -> Self {
        match code & 0x03 {
            0b00 => FragmentationIndicator::Complete,
            0b01 => FragmentationIndicator::Head,
            0b10 => FragmentationIndicator::Middle,
            _ => FragmentationIndicator::Last,
        }
    }

    /// The 2-bit code.
    pub const fn code(self) -> u8 {
        match self {
            FragmentationIndicator::Complete => 0b00,
            FragmentationIndicator::Head => 0b01,
            FragmentationIndicator::Middle => 0b10,
            FragmentationIndicator::Last => 0b11,
        }
    }

    /// Whether this payload completes a data unit: `Complete` (whole) or
    /// `Last` (the end of a divided one).
    pub const fn completes(self) -> bool {
        matches!(
            self,
            FragmentationIndicator::Complete | FragmentationIndicator::Last
        )
    }
}

impl fmt::Display for FragmentationIndicator {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:02b}", self.code())
    }
}

/// The header of an MPU payload (Table 6-1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PayloadHeader {
    /// `fragment_type` (Table 6-2).
    pub fragment_type: FragmentType,
    /// `timed_flag`.
    pub timed: bool,
    /// `fragmentation_indicator` (Table 6-3).
    pub fragmentation: FragmentationIndicator,
    /// `aggregation_flag`.
    pub aggregated: bool,
    /// `fragment_counter`.
    pub fragment_counter: u8,
    /// `MPU_sequence_number`.
    pub mpu_sequence_number: u32,
}

/// The per-MFU header of timed data (Table 6-1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MfuHeader {
    /// `movie_fragment_sequence_number`.
    pub movie_fragment_sequence_number: u32,
    /// `sample_number`.
    pub sample_number: u32,
    /// `offset` within the sample.
    pub offset: u32,
    /// `priority`.
    pub priority: u8,
    /// `dependency_counter`.
    pub dependency_counter: u8,
}

/// One data unit of a payload: its MFU header (timed) or item id (not
/// timed), and its bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataUnit<'a> {
    /// Timed: the MFU header.
    pub mfu: Option<MfuHeader>,
    /// Not timed: `item_id`.
    pub item_id: Option<u32>,
    /// The bytes (a whole MFU, or a fragment of a divided one).
    pub data: &'a [u8],
}

/// Parse the header of an MPU payload (its first 8 bytes, which stay in
/// the clear when the payload is scrambled: SPEC-NOTES "scrambled-mpu").
pub fn parse_header(p: &[u8]) -> Option<PayloadHeader> {
    let len = usize::from(u16::from_be_bytes([*p.first()?, *p.get(1)?]));
    p.get(..2 + len)?;
    let f = *p.get(2)?;
    Some(PayloadHeader {
        fragment_type: FragmentType::from_code(f >> 4),
        timed: f & 0x08 != 0,
        fragmentation: FragmentationIndicator::from_code(f >> 1),
        aggregated: f & 1 != 0,
        fragment_counter: *p.get(3)?,
        mpu_sequence_number: u32::from_be_bytes(p.get(4..8)?.try_into().ok()?),
    })
}

/// Parse an MPU payload: its header and data units.
pub fn parse(p: &[u8]) -> Option<(PayloadHeader, Vec<DataUnit<'_>>)> {
    let h = parse_header(p)?;
    let len = usize::from(u16::from_be_bytes([p[0], p[1]]));
    let p = &p[..2 + len];
    let mut units = Vec::new();
    if h.fragment_type != FragmentType::Mfu {
        return Some((h, units));
    }
    let mut rest = p.get(8..)?;
    loop {
        let unit_bytes = if h.aggregated {
            let n = usize::from(u16::from_be_bytes([*rest.first()?, *rest.get(1)?]));
            let u = rest.get(2..2 + n)?;
            rest = &rest[2 + n..];
            u
        } else {
            std::mem::take(&mut rest)
        };
        if h.timed {
            let m = unit_bytes.get(..14)?;
            units.push(DataUnit {
                mfu: Some(MfuHeader {
                    movie_fragment_sequence_number: u32::from_be_bytes(m[0..4].try_into().ok()?),
                    sample_number: u32::from_be_bytes(m[4..8].try_into().ok()?),
                    offset: u32::from_be_bytes(m[8..12].try_into().ok()?),
                    priority: m[12],
                    dependency_counter: m[13],
                }),
                item_id: None,
                data: &unit_bytes[14..],
            });
        } else {
            units.push(DataUnit {
                mfu: None,
                item_id: Some(u32::from_be_bytes(unit_bytes.get(..4)?.try_into().ok()?)),
                data: &unit_bytes[4..],
            });
        }
        if rest.is_empty() {
            break;
        }
    }
    Some((h, units))
}

/// A whole MFU, as delivered to a sink.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mfu {
    /// MMTP packet id of its asset.
    pub packet_id: u16,
    /// Its MPU's sequence number.
    pub mpu_sequence_number: u32,
    /// The MFU header (timed data).
    pub header: Option<MfuHeader>,
    /// Item id (non-timed data).
    pub item_id: Option<u32>,
    /// The MMTP random access point flag of its first packet.
    pub rap: bool,
    /// A scrambled payload passed through as is (see
    /// [`Reassembler::pass_scrambled`]): `data` is the payload after its
    /// clear 8-byte header, still encrypted; `header` and `item_id` are
    /// `None`.
    pub scrambled: bool,
    /// The bytes.
    pub data: Vec<u8>,
}

/// Per-asset counters.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AssetStats {
    /// MMTP packets.
    pub packets: u64,
    /// Packets lost (gaps in the packet sequence numbers).
    pub packets_lost: u64,
    /// Packets with the scramble-information header extension (B60 Table
    /// 6-9 type 0001h).
    pub scrambled_packets: u64,
    /// Whole MFUs delivered.
    pub mfus: u64,
    /// MFUs of scrambled payloads, counted from the clear payload header
    /// and not delivered (their MFU headers and data are scrambled).
    pub scrambled_mfus: u64,
    /// MFUs dropped (a fragment lost, or oversize).
    pub mfus_dropped: u64,
    /// MPUs started (MPU sequence number changes).
    pub mpus: u64,
    /// MPU sequence numbers that did not follow the previous one + 1.
    pub mpu_sequence_jumps: u64,
    /// First and last MPU sequence number seen.
    pub mpu_range: Option<(u32, u32)>,
    /// Payloads that do not parse.
    pub malformed: u64,
}

#[derive(Debug, Default)]
struct State {
    last_seq: Option<u32>,
    mpu: Option<u32>,
    partial: Option<Mfu>,
    stats: AssetStats,
}

/// Reassembles the MFUs of every MPU packet id.
#[derive(Debug, Default)]
pub struct Reassembler {
    assets: std::collections::BTreeMap<u16, State>,
    /// Deliver each scrambled payload (after its clear header, still
    /// encrypted) as an [`Mfu`] with `scrambled` set, for writing out on
    /// request. Off by default. Nothing is ever descrambled.
    pub pass_scrambled: bool,
}

impl Reassembler {
    /// A reassembler with nothing seen.
    pub fn new() -> Self {
        Self::default()
    }

    /// The counters of each packet id.
    pub fn stats(&self) -> impl Iterator<Item = (u16, &AssetStats)> {
        self.assets.iter().map(|(k, s)| (*k, &s.stats))
    }

    /// Feed one MMTP packet of payload type 00h: its header and payload.
    /// Whole MFUs go to `out`.
    pub fn push(&mut self, h: &isdb_tlv::mmtp::Header, payload: &[u8], out: &mut Vec<Mfu>) {
        let s = self.assets.entry(h.packet_id).or_default();
        s.stats.packets += 1;
        if h.scramble_info {
            s.stats.scrambled_packets += 1;
        }
        let gap = match s.last_seq {
            Some(l) if h.sequence != l.wrapping_add(1) => {
                s.stats.packets_lost +=
                    u64::from(h.sequence.wrapping_sub(l).wrapping_sub(1)).min(1 << 20);
                true
            }
            _ => false,
        };
        s.last_seq = Some(h.sequence);
        if gap && s.partial.take().is_some() {
            s.stats.mfus_dropped += 1;
        }
        let Some(ph) = parse_header(payload) else {
            s.stats.malformed += 1;
            return;
        };
        if s.mpu != Some(ph.mpu_sequence_number) {
            if let Some(prev) = s.mpu
                && ph.mpu_sequence_number != prev.wrapping_add(1)
            {
                s.stats.mpu_sequence_jumps += 1;
            }
            s.mpu = Some(ph.mpu_sequence_number);
            s.stats.mpus += 1;
            let r = s
                .stats
                .mpu_range
                .get_or_insert((ph.mpu_sequence_number, ph.mpu_sequence_number));
            r.1 = ph.mpu_sequence_number;
        }
        if h.scramble_info {
            // Only the payload header is in the clear: count what it says
            // and deliver nothing (no descrambling). An aggregated payload
            // holds an unknown number of MFUs; it counts as one.
            if ph.fragmentation.completes() {
                s.stats.scrambled_mfus += 1;
            }
            if self.pass_scrambled {
                out.push(Mfu {
                    packet_id: h.packet_id,
                    mpu_sequence_number: ph.mpu_sequence_number,
                    header: None,
                    item_id: None,
                    rap: h.rap,
                    scrambled: true,
                    data: payload.get(8..).unwrap_or_default().to_vec(),
                });
            }
            return;
        }
        let Some((_, units)) = parse(payload) else {
            s.stats.malformed += 1;
            return;
        };
        for u in units {
            let mfu = || Mfu {
                packet_id: h.packet_id,
                mpu_sequence_number: ph.mpu_sequence_number,
                header: u.mfu,
                item_id: u.item_id,
                rap: h.rap,
                scrambled: false,
                data: u.data.to_vec(),
            };
            match ph.fragmentation {
                FragmentationIndicator::Complete => {
                    if s.partial.take().is_some() {
                        s.stats.mfus_dropped += 1;
                    }
                    s.stats.mfus += 1;
                    out.push(mfu());
                }
                FragmentationIndicator::Head => {
                    if s.partial.take().is_some() {
                        s.stats.mfus_dropped += 1;
                    }
                    s.partial = Some(mfu());
                }
                // The packet sequence numbers detect a lost fragment; the
                // fragment counter is not checked, because beyond 255
                // fragments it is "the remainder … divided by 255"
                // (§6.3.2) and a 4K NAL unit takes thousands of payloads.
                // (Without a partial MFU, this is the rest of one already
                // dropped — counted when its start was lost — or of one
                // begun before the stream was joined.)
                FragmentationIndicator::Middle | FragmentationIndicator::Last => {
                    if let Some(m) = s.partial.as_mut() {
                        if m.data.len() + u.data.len() > MAX_MFU {
                            s.partial = None;
                            s.stats.mfus_dropped += 1;
                        } else {
                            m.data.extend_from_slice(u.data);
                            if ph.fragmentation == FragmentationIndicator::Last
                                && let Some(m) = s.partial.take()
                            {
                                s.stats.mfus += 1;
                                out.push(m);
                            }
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use FragmentationIndicator::{Complete, Head, Last, Middle};
    use isdb_tlv::mmtp::{FecType, Header, PayloadType};

    /// An MPU payload carrying one timed MFU (or fragment).
    pub(crate) fn payload(
        frag: FragmentationIndicator,
        counter: u8,
        mpu: u32,
        sample: u32,
        data: &[u8],
    ) -> Vec<u8> {
        let mut p = vec![
            0,
            0,
            (FragmentType::Mfu.code() << 4) | 0x08 | (frag.code() << 1),
            counter,
        ];
        p.extend(mpu.to_be_bytes());
        p.extend(0u32.to_be_bytes());
        p.extend(sample.to_be_bytes());
        p.extend(0u32.to_be_bytes());
        p.extend([0, 0]);
        p.extend(data);
        let len = (p.len() - 2) as u16;
        p[..2].copy_from_slice(&len.to_be_bytes());
        p
    }

    pub(crate) fn header(id: u16, seq: u32) -> Header {
        Header {
            fec_type: FecType::NotProtected,
            rap: false,
            payload_type: PayloadType::Mpu,
            packet_id: id,
            sequence: seq,
            scramble_info: false,
        }
    }

    #[test]
    fn spec_payload_header_as_seen_on_air() {
        // 058c 2c 00 0021b0ec …: length 1420, MFU, timed, middle fragment
        let mut p = vec![0x05, 0x8C, 0x2C, 0x00, 0x00, 0x21, 0xB0, 0xEC];
        p.resize(2 + 0x58C, 0);
        let (h, u) = parse(&p).unwrap();
        assert_eq!(
            (h.fragment_type, h.timed, h.fragmentation, h.aggregated),
            (FragmentType::Mfu, true, Middle, false)
        );
        assert_eq!(h.mpu_sequence_number, 0x0021_B0EC);
        assert_eq!(u.len(), 1);
    }

    /// Table 6-2 (the `fragment_type` of Table 6-1): every 4-bit code
    /// round-trips through `from_code` / `code`; 0–2 are named, 3–15 are
    /// `Reserved`.
    #[test]
    fn spec_b60_table_6_1_fragment_types() {
        assert_eq!(
            FragmentType::ASSIGNED.map(FragmentType::code),
            [0, 1, 2],
            "Table 6-2 order"
        );
        for c in 0..=0x0Fu8 {
            let t = FragmentType::from_code(c);
            assert_eq!(t.code(), c, "{t:?}");
            match t {
                FragmentType::Reserved(r) => {
                    assert!((3..=15).contains(&r.get()) && r.get() == c);
                }
                named => assert!(FragmentType::ASSIGNED.contains(&named)),
            }
        }
        assert_eq!(FragmentType::from_code(2), FragmentType::Mfu);
        // masked to the field width
        assert_eq!(FragmentType::from_code(0x12), FragmentType::Mfu);
        assert_eq!(FragmentType::from_code(0xFF), FragmentType::from_code(0x0F));
        assert_eq!(FragmentType::from_code(0xFF).code(), 0x0F);
    }

    /// `from_code` is total and masks to the 4-bit field: every `u8`
    /// decodes to the canonical value of its low bits (the `Reserved`
    /// payload can only come from `from_code`, so `==` is wire equality).
    #[test]
    fn prop_fragment_type_from_code_masks_to_width() {
        for c in 0..=255u8 {
            assert_eq!(
                FragmentType::from_code(c),
                FragmentType::from_code(c & 0x0F),
                "{c:#04x}"
            );
            assert_eq!(FragmentType::from_code(c).code(), c & 0x0F, "{c:#04x}");
        }
    }

    /// Table 6-3: the four fragmentation indicators 00–11 round-trip
    /// through `from_code` / `code`, in table order, and `completes()`
    /// holds for a whole data unit and the last part of a divided one.
    #[test]
    fn spec_b60_fragmentation_indicators() {
        assert_eq!(
            FragmentationIndicator::ALL.map(FragmentationIndicator::code),
            [0b00, 0b01, 0b10, 0b11]
        );
        for c in 0..=3u8 {
            let i = FragmentationIndicator::from_code(c);
            assert_eq!(i.code(), c);
            assert_eq!(i, FragmentationIndicator::ALL[usize::from(c)]);
            assert_eq!(i.completes(), matches!(c, 0b00 | 0b11));
        }
        // masked to the field width
        assert_eq!(FragmentationIndicator::from_code(0xFD), Head);
    }

    /// A parsed payload header's codes are the bits they came from, for
    /// every fragment type and fragmentation indicator (lossless).
    #[test]
    fn prop_payload_header_codes_round_trip() {
        for ft in 0..=0x0Fu8 {
            for fi in 0..=3u8 {
                let p = [0, 6, (ft << 4) | (fi << 1), 0, 0, 0, 0, 0];
                let h = parse_header(&p).unwrap();
                assert_eq!((h.fragment_type.code(), h.fragmentation.code()), (ft, fi));
            }
        }
    }

    #[test]
    fn divided_mfu_and_a_loss() {
        let mut r = Reassembler::new();
        let mut out = Vec::new();
        r.push(&header(9, 1), &payload(Head, 2, 5, 1, b"ab"), &mut out);
        r.push(&header(9, 2), &payload(Middle, 1, 5, 1, b"cd"), &mut out);
        r.push(&header(9, 3), &payload(Last, 0, 5, 1, b"ef"), &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].data, b"abcdef");
        // packet 5 lost: the MFU begun at 4 is dropped
        r.push(&header(9, 4), &payload(Head, 1, 6, 1, b"xx"), &mut out);
        r.push(&header(9, 6), &payload(Last, 0, 6, 1, b"yy"), &mut out);
        assert_eq!(out.len(), 1);
        let st = r.stats().next().unwrap().1.clone();
        assert_eq!((st.packets_lost, st.mfus_dropped, st.mpus), (1, 1, 2));
    }

    proptest::proptest! {
        #[test]
        fn prop_divided_mfus_reassemble(sizes in proptest::collection::vec(1usize..2000, 1..20), cut in 1usize..700) {
            let mut r = Reassembler::new();
            let mut out = Vec::new();
            let mut seq = 0u32;
            let mut want = Vec::new();
            for (i, n) in sizes.iter().enumerate() {
                let data: Vec<u8> = (0..*n).map(|k| (k + i) as u8).collect();
                let pieces: Vec<&[u8]> = data.chunks(cut).collect();
                let m = pieces.len();
                for (j, piece) in pieces.iter().enumerate() {
                    let frag = if m == 1 { Complete } else if j == 0 { Head } else if j == m - 1 { Last } else { Middle };
                    seq += 1;
                    r.push(&header(1, seq), &payload(frag, (m - 1 - j) as u8, 0, i as u32, piece), &mut out);
                }
                want.push(data);
            }
            let got: Vec<Vec<u8>> = out.into_iter().map(|m| m.data).collect();
            proptest::prop_assert_eq!(got, want);
        }
    }
}

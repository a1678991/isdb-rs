//! MPU payloads (MMTP payload type 00h, ARIB STD-B60 v1.14 §6.3.2,
//! Table 6-1) and their MFUs: the payload header, aggregated and divided
//! MFUs, and per-asset reassembly with loss detection.
//!
//! Broadcasting sends no MPU metadata or movie fragment metadata (§6.2:
//! "MPU metadata and movie fragment metadata shall not be transmitted");
//! every payload is an MFU (fragment_type 2). An MFU of timed data is a
//! sample or sub-sample: for HEVC one NAL unit with a 32-bit length
//! prefix (§8.1.1), for MPEG-4 audio one AudioMuxElement (§8.2.1), for
//! captions one subsample with the header of Table 9-1.
//!
//! An MFU divided over several payloads (fragmentation indicator 01, 10,
//! 11; fragment_counter counting the payloads still to come) is
//! reassembled; a gap in the MMTP packet sequence numbers of the packet id
//! drops the MFU being reassembled and is counted.

/// Largest MFU kept while reassembling: a NAL unit of a 4K picture is a
/// few MB at most.
pub const MAX_MFU: usize = 16 << 20;

/// The header of an MPU payload (Table 6-1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PayloadHeader {
    /// `fragment_type` (0 MPU metadata, 1 movie fragment metadata, 2 MFU).
    pub fragment_type: u8,
    /// `timed_flag`.
    pub timed: bool,
    /// `fragmentation_indicator`.
    pub fragmentation: u8,
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
        fragment_type: f >> 4,
        timed: f & 0x08 != 0,
        fragmentation: (f >> 1) & 3,
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
    if h.fragment_type != 2 {
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
            if matches!(ph.fragmentation, 0b00 | 0b11) {
                s.stats.scrambled_mfus += 1;
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
                data: u.data.to_vec(),
            };
            match ph.fragmentation {
                0b00 => {
                    if s.partial.take().is_some() {
                        s.stats.mfus_dropped += 1;
                    }
                    s.stats.mfus += 1;
                    out.push(mfu());
                }
                0b01 => {
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
                _ => {
                    if let Some(m) = s.partial.as_mut() {
                        if m.data.len() + u.data.len() > MAX_MFU {
                            s.partial = None;
                            s.stats.mfus_dropped += 1;
                        } else {
                            m.data.extend_from_slice(u.data);
                            if ph.fragmentation == 0b11
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
    use isdb_tlv::mmtp::Header;

    /// An MPU payload carrying one timed MFU (or fragment).
    pub(crate) fn payload(frag: u8, counter: u8, mpu: u32, sample: u32, data: &[u8]) -> Vec<u8> {
        let mut p = vec![0, 0, 0x20 | 0x08 | (frag << 1), counter];
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
            fec_type: 0,
            rap: false,
            payload_type: 0,
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
            (2, true, 2, false)
        );
        assert_eq!(h.mpu_sequence_number, 0x0021_B0EC);
        assert_eq!(u.len(), 1);
    }

    #[test]
    fn divided_mfu_and_a_loss() {
        let mut r = Reassembler::new();
        let mut out = Vec::new();
        r.push(&header(9, 1), &payload(1, 2, 5, 1, b"ab"), &mut out);
        r.push(&header(9, 2), &payload(2, 1, 5, 1, b"cd"), &mut out);
        r.push(&header(9, 3), &payload(3, 0, 5, 1, b"ef"), &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].data, b"abcdef");
        // packet 5 lost: the MFU begun at 4 is dropped
        r.push(&header(9, 4), &payload(1, 1, 6, 1, b"xx"), &mut out);
        r.push(&header(9, 6), &payload(3, 0, 6, 1, b"yy"), &mut out);
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
                    let frag = if m == 1 { 0 } else if j == 0 { 1 } else if j == m - 1 { 3 } else { 2 };
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

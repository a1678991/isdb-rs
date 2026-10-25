//! TMCC of ISDB-S3: the information codec (ARIB STD-B44 v2.1 §3.11,
//! Figs. 3.11-1 to 3.11-9) and the signal coding (§3.2 (3) Fig. 3.2-5,
//! §3.5, §3.6 (2)).
//!
//! Information: 9 422 bits per frame, fields in the order of Fig. 3.11-1,
//! each MSB first (SPEC-NOTES "TMCC bit order"):
//!
//! | Field | Bits |
//! |---|---|
//! | order of change | 8 |
//! | transmission mode/slot: 8 × (modulation 4, code rate 4, slots 8, OBO 8) | 192 |
//! | stream type per relative stream #0…#15 | 16 × 8 |
//! | packet format: length 16 × 16, sync-pattern bit length 16 × 8, sync pattern 16 × 32 | 896 |
//! | pointer/slot: (top 16, last 16) per slot #1…#120 | 3 840 |
//! | relative stream number per slot #1…#120 | 120 × 4 |
//! | transmission stream ID per relative stream #0…#15 | 16 × 16 |
//! | transmit/receive control: startup 1, uplink 3, reserved 4 | 8 |
//! | extension: identification 16, field 3 598 | 3 614 |
//!
//! Signal: TMCC information + BCH parity (192) = 9 614 bits, energy
//! dispersed with the X¹⁵ + X¹⁴ + 1 PRBS (state `100000000001110`, output
//! from stage 15, once per frame over those 9 614 bits); then the rate-61/120 LDPC code of the
//! main signal over 1 870 zeros, the 9 614 bits and 11 330 zeros; the zeros
//! are not sent: 9 614 + 22 066 = 31 680 bits per frame.

use crate::params::{CodeRate, Modulation, SLOTS, TMCC_INFO_BITS};
use isdb_fec::prbs::B44Prbs;

/// Leading zeros of the shortened TMCC LDPC word (Fig. 3.5-1).
pub const NULL_HEAD: usize = 1_870;
/// Trailing zeros.
pub const NULL_TAIL: usize = 11_330;
/// TMCC data bits after BCH: 9 422 + 192.
pub const DATA_BITS: usize = TMCC_INFO_BITS + isdb_fec::bch::PARITY;

/// One transmission mode (Fig. 3.11-2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mode {
    /// Raw 4-bit modulation code (Table 3.11-1); 1111 = unused.
    pub modulation_code: u8,
    /// Raw 4-bit code-rate code (Table 3.11-2).
    pub rate_code: u8,
    /// Slots allocated, dummies included (a multiple of 5).
    pub slots: u8,
    /// Satellite output back-off, 0.1 dB units.
    pub obo: u8,
}

impl Mode {
    /// An unused mode: 1111 / 1111 / 0 / 0 (§3.11 (2)).
    pub const UNUSED: Self = Self {
        modulation_code: 0b1111,
        rate_code: 0b1111,
        slots: 0,
        obo: 0,
    };
    /// Modulation, if the code is assigned.
    pub fn modulation(&self) -> Option<Modulation> {
        Modulation::from_code(self.modulation_code)
    }
    /// Code rate, if the code is assigned.
    pub fn rate(&self) -> Option<CodeRate> {
        CodeRate::from_code(self.rate_code)
    }
    /// Whether this mode carries slots.
    pub fn is_used(&self) -> bool {
        self.slots > 0 && self.modulation().is_some() && self.rate().is_some()
    }
}

/// Packet format of a relative stream (Fig. 3.11-4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PacketFormat {
    /// Packet length, bytes (0x00BC for TS, 0 for TLV).
    pub length: u16,
    /// Synchronisation pattern length, bits.
    pub sync_bits: u8,
    /// Synchronisation pattern, left-aligned.
    pub sync: u32,
}

/// The decoded TMCC information.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TmccInfo {
    /// Order of change.
    pub change: u8,
    /// Transmission modes 1…8.
    pub modes: [Mode; 8],
    /// Stream type per relative stream (Table 3.11-4: 1 TS, 2 TLV, FF none).
    pub stream_type: [u8; 16],
    /// Packet format per relative stream.
    pub packet_format: [PacketFormat; 16],
    /// (top, last) pointer per slot #1…#120; 0xFFFF = none.
    pub pointers: [(u16, u16); SLOTS],
    /// Relative stream number per slot #1…#120.
    pub slot_stream: [u8; SLOTS],
    /// Transmission stream ID (TS_ID / TLV stream ID) per relative stream.
    pub stream_id: [u16; 16],
    /// Startup control (EWS) flag.
    pub ews: bool,
    /// Uplink control information (3 bits).
    pub uplink: u8,
    /// Reserved bits of the transmit/receive control (4 bits).
    pub reserved: u8,
    /// Extension identification (0 = no extension).
    pub extension_id: u16,
    /// Whether the 3 598-bit extension field is all ones (the value when
    /// `extension_id` is 0).
    pub extension_all_ones: bool,
}

/// Stream type: MPEG-2 TS.
pub const STREAM_TS: u8 = 0x01;
/// Stream type: TLV.
pub const STREAM_TLV: u8 = 0x02;

struct Reader<'a> {
    b: &'a [u8],
    pos: usize,
}

impl Reader<'_> {
    fn take(&mut self, n: usize) -> u32 {
        let mut v = 0u32;
        for _ in 0..n {
            v = (v << 1) | u32::from(self.b[self.pos] & 1);
            self.pos += 1;
        }
        v
    }
}

fn put(out: &mut Vec<u8>, v: u32, n: usize) {
    for i in (0..n).rev() {
        out.push(((v >> i) & 1) as u8);
    }
}

impl TmccInfo {
    /// Parse 9 422 information bits.
    pub fn parse(bits: &[u8]) -> Self {
        assert_eq!(bits.len(), TMCC_INFO_BITS);
        let mut r = Reader { b: bits, pos: 0 };
        let change = r.take(8) as u8;
        let modes = std::array::from_fn(|_| Mode {
            modulation_code: r.take(4) as u8,
            rate_code: r.take(4) as u8,
            slots: r.take(8) as u8,
            obo: r.take(8) as u8,
        });
        let stream_type = std::array::from_fn(|_| r.take(8) as u8);
        let lengths: [u16; 16] = std::array::from_fn(|_| r.take(16) as u16);
        let sync_bits: [u8; 16] = std::array::from_fn(|_| r.take(8) as u8);
        let syncs: [u32; 16] = std::array::from_fn(|_| r.take(32));
        let packet_format = std::array::from_fn(|i| PacketFormat {
            length: lengths[i],
            sync_bits: sync_bits[i],
            sync: syncs[i],
        });
        let pointers = std::array::from_fn(|_| (r.take(16) as u16, r.take(16) as u16));
        let slot_stream = std::array::from_fn(|_| r.take(4) as u8);
        let stream_id = std::array::from_fn(|_| r.take(16) as u16);
        let ews = r.take(1) == 1;
        let uplink = r.take(3) as u8;
        let reserved = r.take(4) as u8;
        let extension_id = r.take(16) as u16;
        let extension_all_ones = bits[r.pos..].iter().all(|&b| b & 1 == 1);
        debug_assert_eq!(bits.len() - r.pos, 3_598);
        Self {
            change,
            modes,
            stream_type,
            packet_format,
            pointers,
            slot_stream,
            stream_id,
            ews,
            uplink,
            reserved,
            extension_id,
            extension_all_ones,
        }
    }

    /// The 9 422 information bits (extension field all ones when
    /// `extension_all_ones`, else zeros).
    pub fn bits(&self) -> Vec<u8> {
        let mut o = Vec::with_capacity(TMCC_INFO_BITS);
        put(&mut o, self.change.into(), 8);
        for m in &self.modes {
            put(&mut o, m.modulation_code.into(), 4);
            put(&mut o, m.rate_code.into(), 4);
            put(&mut o, m.slots.into(), 8);
            put(&mut o, m.obo.into(), 8);
        }
        for &t in &self.stream_type {
            put(&mut o, t.into(), 8);
        }
        for f in &self.packet_format {
            put(&mut o, f.length.into(), 16);
        }
        for f in &self.packet_format {
            put(&mut o, f.sync_bits.into(), 8);
        }
        for f in &self.packet_format {
            put(&mut o, f.sync, 32);
        }
        for &(t, l) in &self.pointers {
            put(&mut o, t.into(), 16);
            put(&mut o, l.into(), 16);
        }
        for &s in &self.slot_stream {
            put(&mut o, s.into(), 4);
        }
        for &id in &self.stream_id {
            put(&mut o, id.into(), 16);
        }
        put(&mut o, u32::from(self.ews), 1);
        put(&mut o, self.uplink.into(), 3);
        put(&mut o, self.reserved.into(), 4);
        put(&mut o, self.extension_id.into(), 16);
        let fill = u8::from(self.extension_all_ones);
        o.resize(TMCC_INFO_BITS, fill);
        o
    }

    /// Consistency of the slot allocation (§3.11 (2)): used modes come
    /// first, their slots are multiples of 5 summing to 120, and modes are
    /// ordered by modulation (most points first) then rate (highest first).
    pub fn allocation_ok(&self) -> bool {
        let used: Vec<&Mode> = self.modes.iter().filter(|m| m.is_used()).collect();
        let n_used = used.len();
        if self.modes[n_used..].iter().any(|m| *m != Mode::UNUSED) {
            return false;
        }
        let sum: usize = used.iter().map(|m| m.slots as usize).sum();
        if sum != SLOTS || used.iter().any(|m| m.slots % 5 != 0) {
            return false;
        }
        used.windows(2).all(|w| {
            let (a, b) = (w[0], w[1]);
            (a.modulation(), a.rate()) >= (b.modulation(), b.rate())
                && (a.modulation() != b.modulation() || a.rate() > b.rate())
        })
    }
}

/// Energy-disperse (or undo) the 9 614 TMCC data bits in place (§3.6 (2)).
pub fn disperse(data: &mut [u8]) {
    let mut g = B44Prbs::tmcc();
    for b in data {
        *b ^= g.next_bit();
    }
}

/// The 31 680 TMCC signal bits of `info` (Fig. 3.2-5): BCH, dispersal,
/// shortened LDPC, nulls removed.
pub fn encode(info: &TmccInfo) -> Vec<u8> {
    let mut d = isdb_fec::bch::encode(&info.bits());
    disperse(&mut d);
    let code = crate::ldpc::code(CodeRate::new(61).expect("61/120"));
    let mut word = vec![0u8; NULL_HEAD];
    word.extend_from_slice(&d);
    word.resize(NULL_HEAD + DATA_BITS + NULL_TAIL, 0);
    let p = code.parity(&word);
    let mut out = d;
    out.extend(p);
    out
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A plausible one-mode TLV frame: 16APSK 93/120 in all 120 slots.
    pub(crate) fn sample() -> TmccInfo {
        let mut modes = [Mode::UNUSED; 8];
        modes[0] = Mode {
            modulation_code: Modulation::Apsk16.code(),
            rate_code: CodeRate::new(93).unwrap().code(),
            slots: 120,
            obo: 0,
        };
        let mut stream_type = [0xFF; 16];
        stream_type[0] = STREAM_TLV;
        let mut stream_id = [0xFFFF; 16];
        stream_id[0] = 0x0001;
        TmccInfo {
            change: 7,
            modes,
            stream_type,
            packet_format: [PacketFormat::default(); 16],
            pointers: [(0, 0xFFFF); SLOTS],
            slot_stream: [0; SLOTS],
            stream_id,
            ews: false,
            uplink: 0b010,
            reserved: 0b1111,
            extension_id: 0,
            extension_all_ones: true,
        }
    }

    #[test]
    fn spec_fig_3_11_1_field_sizes_sum_to_9422() {
        assert_eq!(
            8 + 192 + 128 + 896 + 3_840 + 480 + 256 + 8 + 3_614,
            TMCC_INFO_BITS
        );
        assert_eq!(sample().bits().len(), TMCC_INFO_BITS);
    }

    #[test]
    fn info_round_trips() {
        let t = sample();
        assert_eq!(TmccInfo::parse(&t.bits()), t);
        assert!(t.allocation_ok());
    }

    #[test]
    fn spec_fig_3_2_5_signal_length() {
        let s = encode(&sample());
        assert_eq!(s.len(), crate::params::TMCC_SIGNAL_BITS);
    }
}

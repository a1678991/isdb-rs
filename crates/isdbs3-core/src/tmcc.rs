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

/// A code one of Tables 3.11-1, 3.11-2 or 3.11-4 does not assign, as
/// decoded.
///
/// Only the `from_code` of [`ModulationCode`], [`RateCode`] and
/// [`StreamType`] builds one, after it has excluded every assigned code and
/// the "not allocated" code: the field is private, so a `Reserved(c)` is
/// canonical by construction (one value per wire code, never the spelling
/// of `Unused` / `None`) and `==` on the enums is wire equality.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ReservedCode(u8);

impl ReservedCode {
    /// `c` must be a reserved code of its table, already masked to the
    /// field width; only the `from_code`s uphold this.
    pub(crate) const fn new_unchecked(c: u8) -> Self {
        Self(c)
    }

    /// The code, in the width of its field (4 or 8 bits).
    pub const fn get(self) -> u8 {
        self.0
    }
}

impl std::fmt::Display for ReservedCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.0, f)
    }
}

impl std::fmt::Binary for ReservedCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Binary::fmt(&self.0, f)
    }
}

impl std::fmt::LowerHex for ReservedCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::LowerHex::fmt(&self.0, f)
    }
}

/// A reserved code cannot be spelled as a TMCC code outside this crate;
/// the only way in is `from_code`, so a decoded value is canonical:
///
/// ```compile_fail,E0624
/// use isdbs3_core::{ModulationCode, tmcc::ReservedCode};
/// let _ = ModulationCode::Reserved(ReservedCode::new_unchecked(0b0110));
/// ```
///
/// ```
/// use isdbs3_core::ModulationCode;
/// assert_eq!(ModulationCode::from_code(0b0110), ModulationCode::from_code(0xF6));
/// assert_eq!(ModulationCode::from_code(0b0110).code(), 0b0110);
/// ```
#[doc(hidden)]
pub const _TMCC_CODES_CANONICAL: () = ();

/// Modulation-scheme field of a transmission mode (Table 3.11-1), 4 bits.
///
/// A lossless coded enum: the five assigned codes carry a [`Modulation`],
/// `1111` is "No scheme allocated" (the value of an unused mode, §3.11
/// (2)), and `0000` / `0110`–`1110` are reserved and kept as received so
/// the TMCC word round-trips bit-exactly. The `Reserved` payload is a
/// [`ReservedCode`] only `from_code` can build, so every value is canonical
/// and `==` is wire equality.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ModulationCode {
    /// `0001` π/2-shift BPSK … `0101` 32APSK.
    Assigned(Modulation),
    /// `1111`: no scheme allocated.
    Unused,
    /// `0000`, `0110`–`1110`: reserved; the 4-bit code as received.
    Reserved(ReservedCode),
}

impl ModulationCode {
    /// From the 4-bit field (higher bits are masked off).
    pub const fn from_code(c: u8) -> Self {
        let c = c & 0b1111;
        match Modulation::from_code(c) {
            Some(m) => Self::Assigned(m),
            None if c == 0b1111 => Self::Unused,
            None => Self::Reserved(ReservedCode::new_unchecked(c)),
        }
    }
    /// The 4-bit field.
    pub const fn code(self) -> u8 {
        match self {
            Self::Assigned(m) => m.code(),
            Self::Unused => 0b1111,
            Self::Reserved(c) => c.get(),
        }
    }
    /// The modulation, if the code is assigned.
    pub const fn modulation(self) -> Option<Modulation> {
        match self {
            Self::Assigned(m) => Some(m),
            Self::Unused | Self::Reserved(_) => None,
        }
    }
}

impl std::fmt::Display for ModulationCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Assigned(m) => f.write_str(m.name()),
            Self::Unused => f.write_str("none (1111)"),
            Self::Reserved(c) => write!(f, "reserved ({c:04b})"),
        }
    }
}

/// Inner-coding-rate field of a transmission mode (Table 3.11-2), 4 bits.
///
/// A lossless coded enum: `0001`–`1011` carry a [`CodeRate`], `1111` is
/// "No scheme allocated", and `0000` / `1100`–`1110` are reserved and kept
/// as received (a [`ReservedCode`] only `from_code` can build, so `==` is
/// wire equality).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RateCode {
    /// `0001` 41/120 … `1011` 109/120, in the order of Table 3.4-2.
    Assigned(CodeRate),
    /// `1111`: no scheme allocated.
    Unused,
    /// `0000`, `1100`–`1110`: reserved; the 4-bit code as received.
    Reserved(ReservedCode),
}

impl RateCode {
    /// From the 4-bit field (higher bits are masked off), read per B44
    /// v2.1 Table 3.11-2: `0001` = 41/120 … `1011` = 109/120 in table
    /// order. The table prints 101/120 twice, at `1001` and `1010`; `1010`
    /// is read as 105/120, the only rate missing (SPEC-NOTES "TMCC
    /// code-rate table"; [`CodeRate::from_code`]).
    pub fn from_code(c: u8) -> Self {
        let c = c & 0b1111;
        match CodeRate::from_code(c) {
            Some(r) => Self::Assigned(r),
            None if c == 0b1111 => Self::Unused,
            None => Self::Reserved(ReservedCode::new_unchecked(c)),
        }
    }
    /// The 4-bit field.
    pub fn code(self) -> u8 {
        match self {
            Self::Assigned(r) => r.code(),
            Self::Unused => 0b1111,
            Self::Reserved(c) => c.get(),
        }
    }
    /// The code rate, if the code is assigned.
    pub const fn rate(self) -> Option<CodeRate> {
        match self {
            Self::Assigned(r) => Some(r),
            Self::Unused | Self::Reserved(_) => None,
        }
    }
}

impl std::fmt::Display for RateCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Assigned(r) => write!(f, "{r}"),
            Self::Unused => f.write_str("none (1111)"),
            Self::Reserved(c) => write!(f, "reserved ({c:04b})"),
        }
    }
}

/// One transmission mode (Fig. 3.11-2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mode {
    /// Modulation scheme (Table 3.11-1).
    pub modulation_code: ModulationCode,
    /// Inner coding rate (Table 3.11-2).
    pub rate_code: RateCode,
    /// Slots allocated, dummies included (a multiple of 5).
    pub slots: u8,
    /// Satellite output back-off, 0.1 dB units (Table 3.11-3).
    pub obo: u8,
}

impl Mode {
    /// An unused mode: 1111 / 1111 / 0 / 0 (§3.11 (2)).
    pub const UNUSED: Self = Self {
        modulation_code: ModulationCode::Unused,
        rate_code: RateCode::Unused,
        slots: 0,
        obo: 0,
    };
    /// Modulation, if the code is assigned.
    pub const fn modulation(&self) -> Option<Modulation> {
        self.modulation_code.modulation()
    }
    /// Code rate, if the code is assigned.
    pub const fn rate(&self) -> Option<CodeRate> {
        self.rate_code.rate()
    }
    /// Whether this mode carries slots: an assigned modulation and rate
    /// with a non-zero slot count.
    pub const fn is_used(&self) -> bool {
        self.slots > 0
            && matches!(
                (self.modulation_code, self.rate_code),
                (ModulationCode::Assigned(_), RateCode::Assigned(_))
            )
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
    /// Stream type per relative stream #0…#15 (Table 3.11-4).
    pub stream_type: [StreamType; 16],
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

/// Stream type of a relative stream (Table 3.11-4), 8 bits.
///
/// A lossless coded enum: `0x01` TS, `0x02` TLV, `0xFF` "No type
/// allocated", and `0x00` / `0x03`–`0xFE` reserved and kept as received (a
/// [`ReservedCode`] only `from_code` can build, so `==` is wire equality).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StreamType {
    /// `00000001`: MPEG-2 TS.
    Ts,
    /// `00000010`: TLV.
    Tlv,
    /// `11111111`: no type allocated.
    None,
    /// `00000000`, `00000011`–`11111110`: reserved; the code as received.
    Reserved(ReservedCode),
}

impl StreamType {
    /// From the 8-bit field.
    pub const fn from_code(c: u8) -> Self {
        match c {
            0x01 => Self::Ts,
            0x02 => Self::Tlv,
            0xFF => Self::None,
            c => Self::Reserved(ReservedCode::new_unchecked(c)),
        }
    }
    /// The 8-bit field.
    pub const fn code(self) -> u8 {
        match self {
            Self::Ts => 0x01,
            Self::Tlv => 0x02,
            Self::None => 0xFF,
            Self::Reserved(c) => c.get(),
        }
    }
}

impl std::fmt::Display for StreamType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Ts => f.write_str("TS"),
            Self::Tlv => f.write_str("TLV"),
            Self::None => f.write_str("none (0xff)"),
            Self::Reserved(c) => write!(f, "reserved ({c:#04x})"),
        }
    }
}

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

    /// An `n`-bit field into a `u8`, `n <= 8` (asserted once here).
    fn take_u8(&mut self, n: usize) -> u8 {
        assert!(n <= 8, "take_u8: {n} bits");
        // n <= 8 is asserted above, so the value fits.
        #[allow(clippy::cast_possible_truncation)]
        let v = self.take(n) as u8;
        v
    }

    /// An `n`-bit field into a `u16`, `n <= 16` (asserted once here).
    fn take_u16(&mut self, n: usize) -> u16 {
        assert!(n <= 16, "take_u16: {n} bits");
        // n <= 16 is asserted above, so the value fits.
        #[allow(clippy::cast_possible_truncation)]
        let v = self.take(n) as u16;
        v
    }
}

fn put(out: &mut Vec<u8>, v: u32, n: usize) {
    for i in (0..n).rev() {
        out.push(u8::from((v >> i) & 1 == 1));
    }
}

impl TmccInfo {
    /// Parse 9 422 information bits.
    ///
    /// # Panics
    ///
    /// If `bits` does not hold [`TMCC_INFO_BITS`] bits.
    pub fn parse(bits: &[u8]) -> Self {
        assert_eq!(bits.len(), TMCC_INFO_BITS);
        let mut r = Reader { b: bits, pos: 0 };
        let change = r.take_u8(8);
        let modes = std::array::from_fn(|_| Mode {
            modulation_code: ModulationCode::from_code(r.take_u8(4)),
            rate_code: RateCode::from_code(r.take_u8(4)),
            slots: r.take_u8(8),
            obo: r.take_u8(8),
        });
        let stream_type = std::array::from_fn(|_| StreamType::from_code(r.take_u8(8)));
        let lengths: [u16; 16] = std::array::from_fn(|_| r.take_u16(16));
        let sync_bits: [u8; 16] = std::array::from_fn(|_| r.take_u8(8));
        let syncs: [u32; 16] = std::array::from_fn(|_| r.take(32));
        let packet_format = std::array::from_fn(|i| PacketFormat {
            length: lengths[i],
            sync_bits: sync_bits[i],
            sync: syncs[i],
        });
        let pointers = std::array::from_fn(|_| (r.take_u16(16), r.take_u16(16)));
        let slot_stream = std::array::from_fn(|_| r.take_u8(4));
        let stream_id = std::array::from_fn(|_| r.take_u16(16));
        let ews = r.take(1) == 1;
        let uplink = r.take_u8(3);
        let reserved = r.take_u8(4);
        let extension_id = r.take_u16(16);
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
            put(&mut o, m.modulation_code.code().into(), 4);
            put(&mut o, m.rate_code.code().into(), 4);
            put(&mut o, m.slots.into(), 8);
            put(&mut o, m.obo.into(), 8);
        }
        for &t in &self.stream_type {
            put(&mut o, t.code().into(), 8);
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
    let code = crate::ldpc::code(CodeRate::TMCC);
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
            modulation_code: ModulationCode::Assigned(Modulation::Apsk16),
            rate_code: RateCode::Assigned(CodeRate::new(93).unwrap()),
            slots: 120,
            obo: 0,
        };
        let mut stream_type = [StreamType::None; 16];
        stream_type[0] = StreamType::Tlv;
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

    /// Table 3.11-1: every 4-bit code round-trips; 0001–0101 are the five
    /// modulations in order, 1111 is "no scheme allocated", the rest are
    /// reserved.
    #[test]
    fn spec_table_3_11_1_modulation_codes() {
        for c in 0..=0b1111u8 {
            assert_eq!(ModulationCode::from_code(c).code(), c, "{c:04b}");
        }
        let assigned = [
            (0b0001, Modulation::Bpsk),
            (0b0010, Modulation::Qpsk),
            (0b0011, Modulation::Psk8),
            (0b0100, Modulation::Apsk16),
            (0b0101, Modulation::Apsk32),
        ];
        for (c, m) in assigned {
            assert_eq!(ModulationCode::from_code(c), ModulationCode::Assigned(m));
            assert_eq!(ModulationCode::Assigned(m).code(), c);
        }
        assert_eq!(ModulationCode::from_code(0b1111), ModulationCode::Unused);
        for c in [0b0000u8].into_iter().chain(0b0110..=0b1110) {
            assert!(
                matches!(ModulationCode::from_code(c), ModulationCode::Reserved(r) if r.get() == c),
                "{c:04b}"
            );
        }
        // the field is 4 bits: higher bits are masked
        assert_eq!(ModulationCode::from_code(0xF4).code(), 0b0100);
        assert_eq!(
            ModulationCode::from_code(0xF6),
            ModulationCode::from_code(0b0110)
        );
        assert_eq!(ModulationCode::from_code(0xF6).code(), 0b0110);
    }

    /// Tables 3.11-1, 3.11-2, 3.11-4: `from_code` is total and masks to the
    /// field width, so every `u8` decodes to the canonical value of its low
    /// bits (the `Reserved` payloads can only come from `from_code`, so
    /// `==` is wire equality).
    #[test]
    fn prop_tmcc_codes_from_code_mask_to_width() {
        for c in 0..=255u8 {
            assert_eq!(
                ModulationCode::from_code(c),
                ModulationCode::from_code(c & 0b1111)
            );
            assert_eq!(ModulationCode::from_code(c).code(), c & 0b1111);
            assert_eq!(RateCode::from_code(c), RateCode::from_code(c & 0b1111));
            assert_eq!(RateCode::from_code(c).code(), c & 0b1111);
            assert_eq!(StreamType::from_code(c), StreamType::from_code(c));
            assert_eq!(StreamType::from_code(c).code(), c);
        }
    }

    /// Table 3.11-2: every 4-bit code round-trips; 0001–1011 are the
    /// eleven rates of Table 3.4-2 in order, so 1010 is 105/120 (the
    /// standard prints 101/120 twice; SPEC-NOTES "TMCC code-rate table").
    #[test]
    fn spec_table_3_11_2_rate_codes() {
        for c in 0..=0b1111u8 {
            assert_eq!(RateCode::from_code(c).code(), c, "{c:04b}");
        }
        for (i, r) in crate::params::RATES.into_iter().enumerate() {
            let c = u8::try_from(i).unwrap() + 1;
            let rate = CodeRate::new(r).unwrap();
            assert_eq!(RateCode::from_code(c), RateCode::Assigned(rate), "{c:04b}");
            assert_eq!(RateCode::Assigned(rate).code(), c);
        }
        assert_eq!(
            RateCode::from_code(0b1010).rate().map(CodeRate::numerator),
            Some(105)
        );
        assert_eq!(RateCode::from_code(0b1111), RateCode::Unused);
        for c in [0b0000u8].into_iter().chain(0b1100..=0b1110) {
            assert!(
                matches!(RateCode::from_code(c), RateCode::Reserved(r) if r.get() == c),
                "{c:04b}"
            );
        }
        assert_eq!(RateCode::from_code(0xF7).code(), 0b0111);
        assert_eq!(RateCode::from_code(0xFC), RateCode::from_code(0b1100));
        assert_eq!(RateCode::from_code(0xFC).code(), 0b1100);
    }

    /// Table 3.11-4: every 8-bit code round-trips; 0x01 TS, 0x02 TLV,
    /// 0xFF "no type allocated", the rest reserved.
    #[test]
    fn spec_table_3_11_4_stream_types() {
        for c in 0..=0xFFu8 {
            assert_eq!(StreamType::from_code(c).code(), c, "{c:#04x}");
        }
        assert_eq!(StreamType::from_code(0x01), StreamType::Ts);
        assert_eq!(StreamType::from_code(0x02), StreamType::Tlv);
        assert_eq!(StreamType::from_code(0xFF), StreamType::None);
        for c in [0x00u8].into_iter().chain(0x03..=0xFE) {
            assert!(
                matches!(StreamType::from_code(c), StreamType::Reserved(r) if r.get() == c),
                "{c:#04x}"
            );
        }
    }

    /// A reserved modulation or rate code, or an unused one with slots,
    /// is not a used mode; the word still round-trips.
    #[test]
    fn reserved_codes_are_kept_and_not_used() {
        let mut t = sample();
        t.modes[1] = Mode {
            modulation_code: ModulationCode::from_code(0b0110),
            rate_code: RateCode::from_code(0b1100),
            slots: 5,
            obo: 1,
        };
        t.stream_type[1] = StreamType::from_code(0x7E);
        assert!(!t.modes[1].is_used());
        assert!(!t.allocation_ok());
        assert_eq!(TmccInfo::parse(&t.bits()), t);
        assert!(
            !Mode {
                slots: 5,
                ..Mode::UNUSED
            }
            .is_used()
        );
    }
}

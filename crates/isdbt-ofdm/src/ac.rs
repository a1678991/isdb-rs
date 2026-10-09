//! AC (auxiliary channel) signals (ARIB STD-B31 §3.13.4, §3.16;
//! Attachment Chapter 6 for the carrier mapping order).
//!
//! Every AC carrier carries one DBPSK bit per OFDM symbol; symbol 0 is the
//! differential reference (B'0 = W), so a frame conveys B1..B203 per
//! carrier.  When no additional information is sent every bit is the
//! stuffing value 1 (§3.13.4: "information 1 is inserted as stuffing
//! bits").  Seismic-motion-warning information (§3.16.6) is a 204-bit word
//! carried identically on all AC carriers of segment No. 0.

use crate::dsc::{self, ParityStatus};
use crate::frame::FrameLayout;
use crate::tmcc::{sync_word, BitReader, BitWriter};

/// AC content of one OFDM frame: for each AC carrier of the layout (in
/// ascending frequency, [`FrameLayout::ac`]) the bits B0..B203 (B0 is
/// ignored: the reference is W).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcFrame {
    /// Per carrier, 204 bits.
    pub carriers: Vec<[u8; 204]>,
}

impl AcFrame {
    /// All-stuffing content (§3.13.4): every bit 1.
    pub fn stuffing(layout: &FrameLayout) -> Self {
        Self { carriers: vec![[1u8; 204]; layout.ac.len()] }
    }

    /// Map a serial AC data stream (Attachment §6.2.3.2/§6.3.4.2): bits
    /// fill symbol 1 carriers from lower to higher frequency regardless of
    /// AC1/AC2, then symbol 2, …, symbol 203; missing bits are stuffed with
    /// 1, excess bits are discarded.
    pub fn from_stream(layout: &FrameLayout, bits: &[u8]) -> Self {
        let mut f = Self::stuffing(layout);
        let n = layout.ac.len();
        for (idx, &b) in bits.iter().enumerate().take(n * 203) {
            f.carriers[idx % n][1 + idx / n] = b & 1;
        }
        f
    }

    /// Inverse of [`Self::from_stream`] (B1..B203 of all carriers, symbol-major).
    pub fn to_stream(&self) -> Vec<u8> {
        let n = self.carriers.len();
        (0..n * 203).map(|idx| self.carriers[idx % n][1 + idx / n]).collect()
    }

    /// Stuffing everywhere except the AC carriers of segment No. 0, which
    /// all carry the seismic-motion-warning word (§3.16.6).
    pub fn with_seismic(layout: &FrameLayout, warning: &SeismicWarning, odd_frame: bool) -> Self {
        let mut f = Self::stuffing(layout);
        let word = warning.to_bits(odd_frame);
        for (c, a) in f.carriers.iter_mut().zip(&layout.ac) {
            if a.seg == 0 {
                *c = word;
            }
        }
        f
    }
}

/// Configuration identification B1–B3 (Table 3-33): true if the code
/// announces seismic-motion-warning information (001 or 110).
pub fn is_seismic_config(b: [u8; 3]) -> bool {
    b == [0, 0, 1] || b == [1, 1, 0]
}

/// Seismic-motion-warning information (Table 3-34), lossless.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SeismicWarning {
    /// Start/ending flag B17–B18 (Table 3-36): 00 information available, 11 not.
    pub start_end: u8,
    /// Update flag B19–B20 (§3.16.6.3).
    pub update: u8,
    /// Signal identification B21–B23 (Table 3-37).
    pub signal_id: u8,
    /// Detailed information B24–B111 (88 bits, Tables 3-38..3-41).
    pub detail: [u8; 88],
}

/// Typed view of [`SeismicWarning::detail`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SeismicDetail {
    /// Signal identification 000–011 (Table 3-38).
    Warning {
        /// Current time, low 31 bits of elapsed seconds (B24–B54).
        current_time: u32,
        /// Page classification B55 and seismic motion information B56–B111.
        page: SeismicPage,
    },
    /// Signal identification 111 (Table 3-41).
    NotAvailable {
        /// Identification of broadcasting organisations B56–B66 (11 bits).
        broadcaster_id: u16,
    },
}

/// Seismic motion information B56–B111 by page classification (B55).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SeismicPage {
    /// Page "0" (Table 3-39): area bits B56..B111 (56 bits, B56 = MSB);
    /// a bit is 0 if the area is a target of the warning.
    Areas(u64),
    /// Page "1" (Table 3-40).
    Epicenter {
        /// B56: total number of seismic motion information (false = 1, true = 2).
        two_reports: bool,
        /// B57: seismic motion information identification.
        second: bool,
        /// B58–B66: seismic motion warning identification (9 bits).
        warning_id: u16,
        /// B67: information type (true = cancelled).
        cancelled: bool,
        /// B68: southern latitude.
        south: bool,
        /// B69–B78: latitude × 10 (10 bits).
        latitude_x10: u16,
        /// B79: west longitude.
        west: bool,
        /// B80–B90: longitude × 10 (11 bits).
        longitude_x10: u16,
        /// B91–B100: depth in km (10 bits).
        depth_km: u16,
        /// B101–B110: occurrence time, low 10 bits of elapsed seconds.
        occurrence_time: u16,
        /// B111: reserved ("1").
        reserved: bool,
    },
}

impl SeismicDetail {
    /// Encode to B24..B111.
    pub fn to_bits(&self) -> [u8; 88] {
        let mut w = BitWriter::default();
        match *self {
            SeismicDetail::Warning { current_time, page } => {
                w.put(current_time & 0x7fff_ffff, 31);
                match page {
                    SeismicPage::Areas(a) => {
                        w.put(0, 1);
                        w.put((a >> 32) as u32 & 0xff_ffff, 24);
                        w.put(a as u32, 32);
                    }
                    SeismicPage::Epicenter {
                        two_reports,
                        second,
                        warning_id,
                        cancelled,
                        south,
                        latitude_x10,
                        west,
                        longitude_x10,
                        depth_km,
                        occurrence_time,
                        reserved,
                    } => {
                        w.put(1, 1);
                        w.put(two_reports as u32, 1);
                        w.put(second as u32, 1);
                        w.put(warning_id as u32, 9);
                        w.put(cancelled as u32, 1);
                        w.put(south as u32, 1);
                        w.put(latitude_x10 as u32, 10);
                        w.put(west as u32, 1);
                        w.put(longitude_x10 as u32, 11);
                        w.put(depth_km as u32, 10);
                        w.put(occurrence_time as u32, 10);
                        w.put(reserved as u32, 1);
                    }
                }
            }
            SeismicDetail::NotAvailable { broadcaster_id } => {
                w.put(u32::MAX, 32); // B24–B55 reserved "1"
                w.put(broadcaster_id as u32, 11);
                w.put(u32::MAX, 32); // B67–B111 reserved "1" (45 bits)
                w.put(u32::MAX, 13);
            }
        }
        w.bits.try_into().expect("88 bits")
    }

    /// Typed view for a signal identification; `None` for 100–110.
    pub fn from_bits(signal_id: u8, bits: &[u8; 88]) -> Option<Self> {
        let mut r = BitReader { bits, pos: 0 };
        match signal_id {
            0..=3 => {
                let current_time = r.get(31);
                let page = if r.get(1) == 0 {
                    let hi = r.get(24) as u64;
                    SeismicPage::Areas((hi << 32) | r.get(32) as u64)
                } else {
                    SeismicPage::Epicenter {
                        two_reports: r.get(1) == 1,
                        second: r.get(1) == 1,
                        warning_id: r.get(9) as u16,
                        cancelled: r.get(1) == 1,
                        south: r.get(1) == 1,
                        latitude_x10: r.get(10) as u16,
                        west: r.get(1) == 1,
                        longitude_x10: r.get(11) as u16,
                        depth_km: r.get(10) as u16,
                        occurrence_time: r.get(10) as u16,
                        reserved: r.get(1) == 1,
                    }
                };
                Some(SeismicDetail::Warning { current_time, page })
            }
            7 => {
                r.get(32);
                Some(SeismicDetail::NotAvailable { broadcaster_id: r.get(11) as u16 })
            }
            _ => None,
        }
    }
}

/// CRC of §3.16.6.6, g(x) = x^10 + x^9 + x^5 + x^4 + x + 1, registers
/// initialised to 0, over `bits` (B21..B111); returns 10 bits MSB first
/// (Fig. 3-35 shift-register divider).
pub fn crc10(bits: &[u8]) -> [u8; 10] {
    const G: u16 = (1 << 9) | (1 << 5) | (1 << 4) | (1 << 1) | 1;
    let mut r: u16 = 0;
    for &b in bits {
        let fb = (b as u16 & 1) ^ ((r >> 9) & 1);
        r = (r << 1) & 0x3ff;
        if fb == 1 {
            r ^= G;
        }
    }
    std::array::from_fn(|i| ((r >> (9 - i)) & 1) as u8)
}

impl SeismicWarning {
    /// "No detailed information" (start/end 11, update 11, signal id 111,
    /// Table 3-41) for broadcaster `id`.
    pub fn not_available(broadcaster_id: u16) -> Self {
        Self {
            start_end: 0b11,
            update: 0b11,
            signal_id: 0b111,
            detail: SeismicDetail::NotAvailable { broadcaster_id }.to_bits(),
        }
    }

    /// Typed detail (None for reserved signal identifications).
    pub fn typed_detail(&self) -> Option<SeismicDetail> {
        SeismicDetail::from_bits(self.signal_id, &self.detail)
    }

    /// The 204-bit AC word (B0 = 0 placeholder): B1–B16 = TMCC sync word
    /// of this frame (configuration identification 001/110 + 13-bit sync,
    /// §3.16.6.1), B17–B111 fields, B112–B121 CRC, B122–B203 parity of the
    /// shortened (187,105) code over B17–B121.
    pub fn to_bits(&self, odd_frame: bool) -> [u8; 204] {
        let mut out = [1u8; 204];
        out[0] = 0;
        out[1..17].copy_from_slice(&sync_word(odd_frame));
        let mut w = BitWriter::default();
        w.put(self.start_end as u32, 2);
        w.put(self.update as u32, 2);
        w.put(self.signal_id as u32, 3);
        out[17..24].copy_from_slice(&w.bits);
        out[24..112].copy_from_slice(&self.detail);
        let crc = crc10(&out[21..112]);
        out[112..122].copy_from_slice(&crc);
        let par = dsc::parity(&out[17..122]);
        out[122..204].copy_from_slice(&par);
        out
    }

    /// Decode a 204-bit AC word.  Returns the warning, the configuration
    /// identification bits, CRC validity and parity status.
    pub fn decode(bits: &[u8; 204]) -> SeismicDecoded {
        let mut word = [0u8; 187];
        word.copy_from_slice(&bits[17..204]);
        let parity = dsc::decode(&mut word);
        let mut r = BitReader { bits: &word, pos: 0 };
        let start_end = r.get(2) as u8;
        let update = r.get(2) as u8;
        let signal_id = r.get(3) as u8;
        let detail: [u8; 88] = word[7..95].try_into().expect("88");
        // CRC over B21..B111 = word[4..95]; CRC bits B112..B121 = word[95..105].
        let crc_ok = crc10(&word[4..95]).as_slice() == &word[95..105];
        SeismicDecoded {
            warning: SeismicWarning { start_end, update, signal_id, detail },
            config_id: [bits[1], bits[2], bits[3]],
            crc_ok,
            parity,
        }
    }
}

/// Decoded seismic-warning AC word.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SeismicDecoded {
    /// Fields.
    pub warning: SeismicWarning,
    /// Configuration identification B1–B3.
    pub config_id: [u8; 3],
    /// CRC (§3.16.6.6) matches.
    pub crc_ok: bool,
    /// Parity (§3.16.6.7) status.
    pub parity: ParityStatus,
}

//! ARIB 8-unit character coding (ARIB STD-B24 v6.4 Vol.1 Part 2 Ch.7, §7.1),
//! the text encoding of every ARIB SI string field (ARIB STD-B10 Annex A:
//! "Characters and control codes used in SI are in accordance with the
//! 8-unit character code specified in sub-clause 7.1 …").
//!
//! * [`AribString`] keeps the raw bytes (lossless);
//!   [`AribString::decode`] maps them to Unicode and reports every code that
//!   could not be represented exactly as an [`Issue`] with its byte offset.
//! * [`encode()`] / [`encode_with()`] produce a conformant byte sequence from
//!   Unicode text, such that decoding it yields the same text.
//!
//! * [`DecodeOptions`] / [`EncodeOptions`] select the initial state
//!   ([`InitialState::Si`], the default, or [`InitialState::Caption`]) and
//!   the width mapping ([`WidthMode::CharacterSize`], the default, or
//!   [`WidthMode::AnnexE`]).
//! * Modules: [`sets`] (Tables 7-1..7-3), [`tables`] (Tables 7-4..7-9,
//!   Annex E), [`addsym`] (Tables 7-10, 7-11, 7-19), [`control`]
//!   (Tables 7-14..7-17), [`mod@decode`], [`mod@encode`].
//!
//! The crate README covers the stack position, verification (spec vectors,
//! property tests, synthetic SI strings, opt-in capture strings) and known
//! limitations (DRCS, JIS X 0213 planes, PUA symbols).  Interpretations of
//! the spec are recorded in this crate's `SPEC-NOTES.md`.
//!
//! # Example
//!
//! ```
//! use isdb_text::{AribString, encode};
//!
//! // Audio component text as broadcasters send it: ESC 07/12 (LS3R, G3 =
//! // katakana in the SI initial state), then 39 46 6C 2A of Table 7-6.
//! let s = AribString(vec![0x1b, 0x7c, 0xb9, 0xc6, 0xec, 0xaa]);
//! let d = s.decode();
//! assert_eq!(d.text, "ステレオ");
//! assert!(d.issues.is_empty());
//!
//! // LS1 + "TEST!" at normal size: alphanumerics fill a full display block.
//! assert_eq!(AribString(vec![0x0e, 0x54, 0x45, 0x53, 0x54, 0x21]).to_string_lossy(), "ＴＥＳＴ！");
//!
//! // Encoding round-trips through the decoder.
//! let bytes = encode("ニュース TEST！ 7:00")?;
//! assert_eq!(bytes.decode().text, "ニュース TEST！ 7:00");
//! # Ok::<(), isdb_text::EncodeError>(())
//! ```

// Narrowing casts are proven or fallible here (docs/plans/type-safety.md section 2).
#![warn(clippy::cast_possible_truncation)]
pub mod addsym;
pub mod control;
pub mod decode;
pub mod encode;
pub mod sets;
pub mod tables;

pub use decode::{DecodeOptions, Decoded, Issue, IssueKind, WidthMode, decode_with};
pub use encode::{EncodeError, EncodeOptions, encode, encode_with};
pub use sets::{CharSize, GraphicSet, InitialState, Shift};

/// Raw ARIB-coded string bytes as carried in SI (lossless).
///
/// ARIB STD-B24 Vol.1 Part 2 §7.1 8-unit code; ARIB STD-B10 Annex A.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct AribString(pub Vec<u8>);

impl AribString {
    /// Raw bytes.
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    /// Decode from the SI initial state with the size-aware width mapping
    /// ([`DecodeOptions::default`]). Never panics.
    ///
    /// ARIB STD-B24 Vol.1 Part 2 §7.1, Annex E.4.
    pub fn decode(&self) -> Decoded {
        decode::decode_with(&self.0, &DecodeOptions::default())
    }

    /// Decode with explicit options. Never panics.
    ///
    /// ARIB STD-B24 Vol.1 Part 2 §7.1, Annex E.4.
    pub fn decode_with(&self, opts: &DecodeOptions) -> Decoded {
        decode::decode_with(&self.0, opts)
    }

    /// Decoded text, with U+FFFD for codes that have no mapping and the
    /// issues dropped.
    pub fn to_string_lossy(&self) -> String {
        self.decode().text
    }

    /// Encode Unicode text ([`encode()`]).
    pub fn encode(text: &str) -> Result<AribString, EncodeError> {
        encode::encode(text)
    }

    /// Encode Unicode text with options ([`encode_with()`]).
    pub fn encode_with(text: &str, opts: &EncodeOptions) -> Result<AribString, EncodeError> {
        encode::encode_with(text, opts)
    }
}

impl From<Vec<u8>> for AribString {
    fn from(v: Vec<u8>) -> Self {
        AribString(v)
    }
}

impl From<&[u8]> for AribString {
    fn from(v: &[u8]) -> Self {
        AribString(v.to_vec())
    }
}

// Compile the README's code blocks as doctests.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;

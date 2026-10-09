//! 8-unit code → Unicode decoder.
//!
//! ARIB STD-B24 v6.4 Vol.1 Part 2 §7.1 (code structure, Tables 7-1 … 7-3,
//! control functions Tables 7-14 … 7-18) and Annex E.4 (conversion of the
//! 8-bit code to UCS).

use std::collections::HashMap;
use std::fmt;

use crate::control as c;
use crate::sets::{CharSize, GraphicSet, InitialState};
use crate::tables;

/// How the width of alphanumeric characters and SP is mapped to Unicode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum WidthMode {
    /// Follow the character size in effect (Table 7-16 SSZ/MSZ/NSZ/SZX):
    /// in a narrow field (SSZ, MSZ, SZX tiny) the alphanumeric set maps to
    /// U+0021…U+007E and SP to U+0020; in a normal-size field (NSZ, the
    /// initial size, and the other SZX sizes) they occupy a full display
    /// block and map to the full-width forms U+FF01…U+FF5E / U+FFE5 and
    /// SP to IDEOGRAPHIC SPACE U+3000. Kana and Kanji are unaffected.
    #[default]
    CharacterSize,
    /// Annex E.4 literally: alphanumeric characters always map to their
    /// JIS X 0201 (half-width) code points and SP to U+0020, regardless of
    /// the character size.
    AnnexE,
}

/// Decoder options.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct DecodeOptions {
    /// Initial designation/invocation state (default: SI).
    pub initial: InitialState,
    /// Width mapping (default: follow the character size).
    pub width: WidthMode,
}

/// Result of decoding an ARIB string: the text and everything that could
/// not be represented exactly.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Decoded {
    /// Decoded text (Unicode, NFC is not applied).
    pub text: String,
    /// Problems found, in byte order.
    pub issues: Vec<Issue>,
}

/// A decoding problem at a byte offset of the input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issue {
    /// Offset of the first byte of the offending code in the input (for
    /// codes inside a macro body: the offset of the macro invocation).
    pub offset: usize,
    /// What went wrong.
    pub kind: IssueKind,
}

/// Kinds of decoding problems.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum IssueKind {
    /// Escape sequence not listed in Table 7-1 / Table 7-2, or a final byte
    /// not listed in Table 7-3 (the designation is not applied).
    UnknownEscape(Vec<u8>),
    /// A C0/C1 code left blank in Table 7-14 ("undefined"), or 10/0 / 15/15.
    UndefinedControl(u8),
    /// A control code whose parameters do not match Tables 7-15/7-16/7-17.
    MalformedControl(u8),
    /// The input ended inside a multi-byte code or control sequence.
    Truncated,
    /// A code that is blank in its set's code table (U+FFFD is output).
    /// `code` is the GL form (`0x21..=0x7E`, or `row+0x20 << 8 | cell+0x20`).
    UndefinedCode {
        /// Set in which the code was looked up.
        set: GraphicSet,
        /// GL-form code.
        code: u16,
    },
    /// A code of a set whose repertoire this crate does not map (JIS
    /// compatible Kanji Plane 1 outside rows 1–84, Plane 2): U+FFFD is
    /// output.
    UnsupportedCode {
        /// Set.
        set: GraphicSet,
        /// GL-form code.
        code: u16,
    },
    /// A DRCS character (Table 7-3, Annex D): U+FFFD is output as a
    /// placeholder.
    Drcs {
        /// DRCS set number (0 = 2-byte DRCS-0, 1..=15 = DRCS-1..15).
        set: u8,
        /// GL-form code.
        code: u16,
    },
    /// A mosaic character; Annex E.4: "Any character in the mosaic set is
    /// ignored." Nothing is output.
    Mosaic {
        /// Mosaic set.
        set: GraphicSet,
        /// GL-form code.
        code: u8,
    },
    /// A macro code with no definition and no default macro statement
    /// (Table 7-18 Note 2): nothing is output.
    UndefinedMacro(u8),
    /// Macro execution nested too deeply (a macro invoking itself) or
    /// expanding to more than [`MACRO_WORK_LIMIT`] bytes; the rest of the
    /// expansion is skipped.
    MacroLimit,
    /// SS2/SS3 not followed by a graphic character.
    LoneSingleShift,
    /// Non-spacing character(s) (§7.1.1.4) with no following spacing
    /// character to combine with; output at the end as is.
    DanglingNonSpacing,
    /// RPC 04/0 ("repeat to the end of line"): the line length is unknown
    /// here, the character is output once.
    RepeatToEndOfLine,
}

impl fmt::Display for Issue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "@{}: {:?}", self.offset, self.kind)
    }
}

/// Default macro code strings, ARIB STD-B24 Vol.1 Part 2 Table 7-18.
/// F1 Kanji, F2 Alphanumeric, F3 Hiragana, F4 Katakana, F5..F8 Mosaic A..D,
/// F9 Macro, F10..F24 DRCS-1..15.
fn default_macro(code: u8) -> Option<Vec<u8>> {
    use GraphicSet as G;
    let f = |k: u8| -> G {
        match k {
            1 => G::Kanji,
            2 => G::Alphanumeric,
            3 => G::Hiragana,
            4 => G::Katakana,
            5 => G::MosaicA,
            6 => G::MosaicB,
            7 => G::MosaicC,
            8 => G::MosaicD,
            9 => G::Macro,
            k => G::Drcs(k - 9),
        }
    };
    // (F for G0, G1, G2); G3 is always F9 (Macro), then LS0, LS2R.
    let g: [u8; 3] = match code {
        0x60 => [1, 2, 3],
        0x61 => [1, 4, 3],
        0x62 => [1, 10, 3],
        0x63 => [5, 7, 8],
        0x64 => [5, 6, 8],
        0x65 => [5, 10, 8],
        0x66 => [10, 11, 12],
        0x67 => [13, 14, 15],
        0x68 => [16, 17, 18],
        0x69 => [19, 20, 21],
        0x6A => [22, 23, 24],
        0x6B => [1, 11, 3],
        0x6C => [1, 12, 3],
        0x6D => [1, 13, 3],
        0x6E => [4, 3, 2],
        0x6F => [2, 5, 10],
        _ => return None,
    };
    let mut v = Vec::new();
    for (n, k) in g.iter().enumerate() {
        v.extend(f(*k).designation(n as u8));
    }
    v.extend(G::Macro.designation(3));
    v.push(c::LS0);
    v.extend([c::ESC, 0x7D]);
    Some(v)
}

const MAX_MACRO_DEPTH: u32 = 4;

/// Upper bound on the bytes processed through macro expansion per input
/// byte (plus a constant), so nested macros cannot blow up decoding time.
pub const MACRO_WORK_LIMIT: usize = 64;

struct Decoder {
    g: [GraphicSet; 4],
    gl: usize,
    gr: usize,
    size: CharSize,
    width: WidthMode,
    out: String,
    issues: Vec<Issue>,
    marks: Vec<(char, usize)>,
    repeat: Option<usize>,
    macros: HashMap<u8, Vec<u8>>,
    /// Remaining bytes that macro bodies may still expand to.
    budget: usize,
}

/// Decode with options. Never panics.
pub fn decode_with(bytes: &[u8], opts: &DecodeOptions) -> Decoded {
    let (gl, gr) = opts.initial.invocations();
    let mut d = Decoder {
        g: opts.initial.designations(),
        gl: gl as usize,
        gr: gr as usize,
        size: CharSize::Normal,
        width: opts.width,
        out: String::with_capacity(bytes.len() * 2),
        issues: Vec::new(),
        marks: Vec::new(),
        repeat: None,
        macros: HashMap::new(),
        budget: bytes.len() * MACRO_WORK_LIMIT + 4096,
    };
    d.run(bytes, None, 0);
    if let Some(&(_, at)) = d.marks.first() {
        d.issue(at, IssueKind::DanglingNonSpacing);
        let marks: Vec<char> = d.marks.drain(..).map(|(m, _)| m).collect();
        d.out.extend(marks);
    }
    Decoded {
        text: d.out,
        issues: d.issues,
    }
}

fn is_graphic(b: u8) -> bool {
    matches!(b, 0x21..=0x7E | 0xA1..=0xFE)
}

impl Decoder {
    fn issue(&mut self, offset: usize, kind: IssueKind) {
        self.issues.push(Issue { offset, kind });
    }

    /// Output a spacing character (honouring RPC), then any pending
    /// non-spacing characters, which §7.1.1.4 places before the character
    /// they combine with and UCS places after it.
    fn emit(&mut self, ch: char) {
        let n = self.repeat.take().unwrap_or(1);
        for _ in 0..n {
            self.out.push(ch);
        }
        for (m, _) in self.marks.drain(..) {
            self.out.push(m);
        }
    }

    fn emit_placeholder(&mut self) {
        self.emit('\u{FFFD}');
    }

    fn narrow(&self) -> bool {
        self.width == WidthMode::AnnexE || self.size.is_narrow()
    }

    /// Decode a sequence. `origin`: offset to report for issues (macro
    /// bodies); `None` = use the byte's own offset.
    fn run(&mut self, b: &[u8], origin: Option<usize>, depth: u32) {
        let mut i = 0;
        while i < b.len() {
            let at = origin.unwrap_or(i);
            let x = b[i];
            i += match x {
                0x21..=0x7E => self.graphic(b, i, self.gl, at, depth),
                0xA1..=0xFE => self.graphic(b, i, self.gr, at, depth),
                c::SP => {
                    let ch = if self.narrow() { ' ' } else { '\u{3000}' };
                    self.emit(ch);
                    1
                }
                c::LS0 => {
                    self.gl = 0;
                    1
                }
                c::LS1 => {
                    self.gl = 1;
                    1
                }
                c::SS2 | c::SS3 => {
                    let elem = if x == c::SS2 { 2 } else { 3 };
                    match b.get(i + 1) {
                        Some(&n) if is_graphic(n) => 1 + self.graphic(b, i + 1, elem, at, depth),
                        Some(_) => {
                            self.issue(at, IssueKind::LoneSingleShift);
                            1
                        }
                        None => {
                            self.issue(at, IssueKind::Truncated);
                            1
                        }
                    }
                }
                c::ESC => self.escape(b, i, at),
                c::APR => {
                    // Table 7-15: active position down + first position of the line.
                    self.flush_dangling();
                    self.out.push('\n');
                    1
                }
                c::SSZ => self.set_size(CharSize::Small),
                c::MSZ => self.set_size(CharSize::Middle),
                c::NSZ => self.set_size(CharSize::Normal),
                c::SZX => match b.get(i + 1) {
                    Some(&p) => {
                        self.size = CharSize::Szx(p);
                        2
                    }
                    None => self.truncated(b, i, at),
                },
                c::RPC => match b.get(i + 1) {
                    Some(&p) => {
                        let n = (p & 0x3F) as usize;
                        if n == 0 {
                            self.issue(at, IssueKind::RepeatToEndOfLine);
                            self.repeat = None;
                        } else {
                            self.repeat = Some(n);
                        }
                        2
                    }
                    None => self.truncated(b, i, at),
                },
                c::COL | c::CDC => match b.get(i + 1) {
                    Some(0x20) if i + 2 < b.len() => 3,
                    Some(0x20) => self.truncated(b, i, at),
                    Some(_) => 2,
                    None => self.truncated(b, i, at),
                },
                c::TIME => self.time(b, i, at),
                c::CSI => self.csi(b, i, at),
                c::MACRO => self.macro_cmd(b, i, at, depth),
                _ => match c::fixed_params(x) {
                    Some(n) if i + n < b.len() => 1 + n,
                    Some(_) => self.truncated(b, i, at),
                    None => {
                        self.issue(at, IssueKind::UndefinedControl(x));
                        1
                    }
                },
            };
        }
    }

    fn set_size(&mut self, s: CharSize) -> usize {
        self.size = s;
        1
    }

    fn truncated(&mut self, b: &[u8], i: usize, at: usize) -> usize {
        self.issue(at, IssueKind::Truncated);
        b.len() - i
    }

    fn flush_dangling(&mut self) {
        if let Some(&(_, at)) = self.marks.first() {
            self.issue(at, IssueKind::DanglingNonSpacing);
            let marks: Vec<char> = self.marks.drain(..).map(|(m, _)| m).collect();
            self.out.extend(marks);
        }
    }

    /// A graphic character of code element `elem` starting at `b[i]`.
    /// Returns the number of bytes consumed.
    fn graphic(&mut self, b: &[u8], i: usize, elem: usize, at: usize, depth: u32) -> usize {
        let set = self.g[elem];
        let c1 = b[i] & 0x7F;
        if set.bytes_per_char() == 2 {
            let gr = b[i] & 0x80;
            match b.get(i + 1) {
                Some(&n) if is_graphic(n) && n & 0x80 == gr => {
                    self.char2(set, c1, n & 0x7F, at);
                    2
                }
                Some(_) => {
                    // Second byte not in the same area: not a character.
                    self.issue(at, IssueKind::Truncated);
                    1
                }
                None => {
                    self.issue(at, IssueKind::Truncated);
                    1
                }
            }
        } else {
            self.char1(set, c1, at, depth);
            1
        }
    }

    fn char2(&mut self, set: GraphicSet, c1: u8, c2: u8, at: usize) {
        let (row, cell) = (c1 - 0x20, c2 - 0x20);
        let code = (c1 as u16) << 8 | c2 as u16;
        let ch = match set {
            GraphicSet::Kanji => tables::kanji(row, cell),
            GraphicSet::AdditionalSymbols => tables::additional_symbols(row, cell),
            GraphicSet::JisKanjiPlane1 if row <= 84 => match tables::kanji(row, cell) {
                Some(ch) => Some(ch),
                None => {
                    self.issue(at, IssueKind::UnsupportedCode { set, code });
                    return self.emit_placeholder();
                }
            },
            GraphicSet::Drcs(n) => {
                self.issue(at, IssueKind::Drcs { set: n, code });
                return self.emit_placeholder();
            }
            _ => {
                self.issue(at, IssueKind::UnsupportedCode { set, code });
                return self.emit_placeholder();
            }
        };
        match ch {
            Some(m) if tables::is_non_spacing(m) => self.marks.push((m, at)),
            Some(ch) => self.emit(ch),
            None => {
                self.issue(at, IssueKind::UndefinedCode { set, code });
                self.emit_placeholder();
            }
        }
    }

    fn char1(&mut self, set: GraphicSet, c1: u8, at: usize, depth: u32) {
        use GraphicSet as G;
        let ch = match set {
            G::Alphanumeric | G::ProportionalAlphanumeric => tables::alphanumeric(c1).map(|ch| {
                if self.narrow() {
                    ch
                } else {
                    tables::fullwidth(ch)
                }
            }),
            G::Hiragana | G::ProportionalHiragana => tables::hiragana(c1),
            G::Katakana | G::ProportionalKatakana => tables::katakana(c1),
            G::JisX0201Katakana => tables::jis_x0201_katakana(c1),
            G::MosaicA | G::MosaicB | G::MosaicC | G::MosaicD => {
                self.issue(at, IssueKind::Mosaic { set, code: c1 });
                self.repeat = None;
                return;
            }
            G::Drcs(n) => {
                self.issue(
                    at,
                    IssueKind::Drcs {
                        set: n,
                        code: c1 as u16,
                    },
                );
                return self.emit_placeholder();
            }
            G::Macro => return self.run_macro(c1, at, depth),
            // A 2-byte set never reaches here.
            _ => None,
        };
        match ch {
            Some(ch) => self.emit(ch),
            None => {
                self.issue(
                    at,
                    IssueKind::UndefinedCode {
                        set,
                        code: c1 as u16,
                    },
                );
                self.emit_placeholder();
            }
        }
    }

    /// Macro character (§7.1.1.6): execute its definition, or the default
    /// macro statement of Table 7-18.
    fn run_macro(&mut self, code: u8, at: usize, depth: u32) {
        let body = self
            .macros
            .get(&code)
            .cloned()
            .or_else(|| default_macro(code));
        match body {
            Some(body) if depth >= MAX_MACRO_DEPTH || body.len() > self.budget => {
                self.issue(at, IssueKind::MacroLimit)
            }
            Some(body) => {
                self.budget -= body.len();
                self.run(&body, Some(at), depth + 1)
            }
            None => self.issue(at, IssueKind::UndefinedMacro(code)),
        }
    }

    /// ESC sequences: Table 7-1 (LS2, LS3, LS1R, LS2R, LS3R) and Table 7-2
    /// (designations). Returns bytes consumed.
    fn escape(&mut self, b: &[u8], i: usize, at: usize) -> usize {
        // ISO/IEC 2022 shape: ESC I* F, I in 02/0..02/15, F in 03/0..07/14.
        let mut j = i + 1;
        while j < b.len() && (0x20..=0x2F).contains(&b[j]) {
            j += 1;
        }
        let Some(&f) = b.get(j) else {
            return self.truncated(b, i, at);
        };
        if !(0x30..=0x7E).contains(&f) {
            // Not an escape sequence at all; drop the ESC and the intermediates.
            self.issue(at, IssueKind::UnknownEscape(b[i..j].to_vec()));
            return j - i;
        }
        let seq = &b[i..=j];
        let inter = &b[i + 1..j];
        let ok = match (inter, f) {
            ([], 0x6E) => {
                self.gl = 2;
                true
            }
            ([], 0x6F) => {
                self.gl = 3;
                true
            }
            ([], 0x7E) => {
                self.gr = 1;
                true
            }
            ([], 0x7D) => {
                self.gr = 2;
                true
            }
            ([], 0x7C) => {
                self.gr = 3;
                true
            }
            // 1-byte G set: ESC 02/8+n F.   1-byte DRCS: ESC 02/8+n 02/0 F.
            ([g @ 0x28..=0x2B], f) => self.designate(g - 0x28, false, false, f),
            ([g @ 0x28..=0x2B, 0x20], f) => self.designate(g - 0x28, false, true, f),
            // 2-byte G set: ESC 02/4 F (G0), ESC 02/4 02/9+n F (G1..G3).
            ([0x24], f) => self.designate(0, true, false, f),
            ([0x24, g @ 0x29..=0x2B], f) => self.designate(g - 0x28, true, false, f),
            // 2-byte DRCS: ESC 02/4 02/8+n 02/0 F.
            ([0x24, g @ 0x28..=0x2B, 0x20], f) => self.designate(g - 0x28, true, true, f),
            _ => false,
        };
        if !ok {
            self.issue(at, IssueKind::UnknownEscape(seq.to_vec()));
        }
        seq.len()
    }

    fn designate(&mut self, n: u8, two_byte: bool, drcs: bool, f: u8) -> bool {
        match GraphicSet::from_designation(two_byte, drcs, f) {
            Some(set) => {
                self.g[n as usize] = set;
                true
            }
            None => false,
        }
    }

    /// TIME (Table 7-16): `TIME 02/0 P2`, `TIME 02/8 P2`, or
    /// `TIME 02/9 P… I… F` (F = 04/0..04/3).
    fn time(&mut self, b: &[u8], i: usize, at: usize) -> usize {
        match b.get(i + 1) {
            None => self.truncated(b, i, at),
            Some(0x20) | Some(0x28) => {
                if i + 2 < b.len() {
                    3
                } else {
                    self.truncated(b, i, at)
                }
            }
            Some(0x29) => match self.sequence_end(b, i + 2) {
                Some(end) => end - i,
                None => self.truncated(b, i, at),
            },
            Some(_) => {
                self.issue(at, IssueKind::MalformedControl(c::TIME));
                2
            }
        }
    }

    /// Index after the final byte of a parameter sequence starting at
    /// `j`: parameters/intermediates 02/0..03/15, final 04/0..07/14.
    fn sequence_end(&self, b: &[u8], mut j: usize) -> Option<usize> {
        while j < b.len() {
            match b[j] {
                0x20..=0x3F => j += 1,
                0x40..=0x7E => return Some(j + 1),
                _ => return Some(j),
            }
        }
        None
    }

    /// CSI (Table 7-17): `CSI P… I F`.
    fn csi(&mut self, b: &[u8], i: usize, at: usize) -> usize {
        match self.sequence_end(b, i + 1) {
            Some(end) => {
                if !(0x40..=0x7E).contains(&b[end - 1]) || end == i + 1 {
                    self.issue(at, IssueKind::MalformedControl(c::CSI));
                }
                end - i
            }
            None => self.truncated(b, i, at),
        }
    }

    /// MACRO (Table 7-16): `MACRO 04/0 MC body MACRO 04/15` defines,
    /// `MACRO 04/1 …` defines and executes once, `MACRO 04/15` ends.
    fn macro_cmd(&mut self, b: &[u8], i: usize, at: usize, depth: u32) -> usize {
        match b.get(i + 1) {
            None => self.truncated(b, i, at),
            Some(0x4F) => 2,
            Some(&p @ (0x40 | 0x41)) => {
                let Some(&mc) = b.get(i + 2) else {
                    return self.truncated(b, i, at);
                };
                let start = i + 3;
                let end = b[start.min(b.len())..]
                    .windows(2)
                    .position(|w| w == [c::MACRO, 0x4F])
                    .map(|p| start + p);
                let Some(end) = end else {
                    return self.truncated(b, i, at);
                };
                self.macros.insert(mc & 0x7F, b[start..end].to_vec());
                if p == 0x41 {
                    self.run_macro(mc & 0x7F, at, depth);
                }
                end + 2 - i
            }
            Some(_) => {
                self.issue(at, IssueKind::MalformedControl(c::MACRO));
                2
            }
        }
    }
}

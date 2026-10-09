//! Unicode → 8-unit code encoder.
//!
//! Produces a byte sequence that starts from the initial state
//! ([`InitialState`]) and uses only the designations and invocations of
//! ARIB STD-B24 v6.4 Vol.1 Part 2 Tables 7-1 and 7-2, the graphic sets
//! Kanji (Table 7-4), Alphanumeric (7-5), Katakana (7-6), Hiragana (7-7),
//! JIS X 0201 katakana (7-9) and Additional symbols (7-10, 7-11), the
//! character-size controls MSZ / NSZ (Table 7-16), SP and APR (Table 7-15).
//!
//! The choice among equivalent representations (which set, which code
//! element, locking vs single shift) is a shortest-path search over the
//! code-extension state, so the output is close to minimal in length.
//! The guarantee is `decode_with(encode_with(s, o), o').text == s` where
//! `o'` has the same initial state and width mode as `o`.

use std::collections::HashMap;

use crate::AribString;
use crate::control as c;
use crate::decode::WidthMode;
use crate::sets::{GraphicSet, InitialState, Shift};
use crate::tables;

/// Encoder options.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct EncodeOptions {
    /// Initial state assumed by the receiver (default: SI).
    pub initial: InitialState,
    /// Width mapping the receiver will apply (default: follow the character
    /// size, so ASCII is sent in MSZ and full-width alphanumerics may be
    /// sent with the alphanumeric set in NSZ).
    pub width: WidthMode,
}

/// Why a string cannot be encoded.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EncodeError {
    /// The character is in none of the sets the encoder uses.
    #[error("character {ch:?} (U+{:04X}) at byte {index} is not representable in the ARIB 8-unit code", *ch as u32)]
    Unencodable {
        /// Byte index in the input `&str`.
        index: usize,
        /// The character.
        ch: char,
    },
    /// A combining character (Table E-1) with no preceding base character
    /// to attach to; the 8-unit code sends non-spacing characters *before*
    /// their base (§7.1.1.4), so one at the start of the text or of a line
    /// has no representation.
    #[error("combining character {ch:?} at byte {index} has no base character")]
    NoBase {
        /// Byte index in the input `&str`.
        index: usize,
        /// The character.
        ch: char,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Size {
    Narrow,
    Wide,
    Any,
}

#[derive(Debug, Clone, Copy)]
enum Cand {
    /// A graphic character: set, GL-form code bytes, length.
    Graphic {
        set: GraphicSet,
        code: [u8; 2],
        size: Size,
    },
    /// SP (02/0).
    Space { size: Size },
    /// APR (00/13).
    Newline,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct State {
    g: [GraphicSet; 4],
    gl: u8,
    gr: u8,
    narrow: bool,
}

fn candidates(ch: char, width: WidthMode) -> Vec<Cand> {
    use GraphicSet as G;
    let mut v = Vec::new();
    if ch == '\n' {
        return vec![Cand::Newline];
    }
    if let Some(&(r, k, _)) = tables::NON_SPACING.iter().find(|&&(_, _, m)| m == ch) {
        return vec![Cand::Graphic {
            set: G::Kanji,
            code: [r + 0x20, k + 0x20],
            size: Size::Any,
        }];
    }
    let one = |set, table, size| {
        tables::one_byte_reverse(table, ch).map(|b| Cand::Graphic {
            set,
            code: [b, 0],
            size,
        })
    };
    v.extend(one(G::Hiragana, tables::HIRAGANA, Size::Wide));
    v.extend(one(G::Katakana, tables::KATAKANA, Size::Wide));
    v.extend(one(G::Alphanumeric, tables::ALPHANUMERIC, Size::Narrow));
    if width == WidthMode::CharacterSize {
        let half = tables::ALPHANUMERIC
            .chars()
            .find(|&h| tables::fullwidth(h) == ch);
        if let Some(h) = half {
            v.extend(
                tables::one_byte_reverse(tables::ALPHANUMERIC, h).map(|b| Cand::Graphic {
                    set: G::Alphanumeric,
                    code: [b, 0],
                    size: Size::Wide,
                }),
            );
        }
    }
    if let Some(&(r, k)) = tables::kanji_reverse().get(&ch) {
        v.push(Cand::Graphic {
            set: G::Kanji,
            code: [r + 0x20, k + 0x20],
            size: Size::Wide,
        });
    }
    if let Some(&(r, k)) = tables::additional_reverse().get(&ch) {
        v.push(Cand::Graphic {
            set: G::AdditionalSymbols,
            code: [r + 0x20, k + 0x20],
            size: Size::Wide,
        });
    }
    v.extend(one(
        G::JisX0201Katakana,
        tables::JIS_X0201_KATAKANA,
        Size::Narrow,
    ));
    match (ch, width) {
        (' ', _) => v.push(Cand::Space { size: Size::Narrow }),
        ('\u{3000}', WidthMode::CharacterSize) => v.push(Cand::Space { size: Size::Wide }),
        _ => {}
    }
    v
}

/// Ways to make `set` usable from state `s`: (prefix bytes, new state,
/// emit in GR?, state after the character).
fn ways(s: &State, set: GraphicSet) -> Vec<(Vec<u8>, State, bool)> {
    let mut v = Vec::new();
    if s.g[s.gl as usize] == set {
        v.push((Vec::new(), *s, false));
    }
    if s.g[s.gr as usize] == set {
        v.push((Vec::new(), *s, true));
    }
    if !v.is_empty() {
        return v;
    }
    for n in 0..4u8 {
        let (pre, s1) = if s.g[n as usize] == set {
            (Vec::new(), *s)
        } else {
            let mut s1 = *s;
            s1.g[n as usize] = set;
            (set.designation(n), s1)
        };
        if s1.gl == n {
            v.push((pre.clone(), s1, false));
        }
        if s1.gr == n {
            v.push((pre.clone(), s1, true));
        }
        if s1.gl == n || s1.gr == n {
            continue;
        }
        let ls = [Shift::Ls0, Shift::Ls1, Shift::Ls2, Shift::Ls3][n as usize];
        let mut p = pre.clone();
        p.extend_from_slice(ls.bytes());
        v.push((p, State { gl: n, ..s1 }, false));
        if n > 0 {
            let lsr = [Shift::Ls1r, Shift::Ls1r, Shift::Ls2r, Shift::Ls3r][n as usize];
            let mut p = pre.clone();
            p.extend_from_slice(lsr.bytes());
            v.push((p, State { gr: n, ..s1 }, true));
        }
        if n >= 2 {
            // Single shift: GL for one character, invocation state unchanged.
            let ss = if n == 2 { Shift::Ss2 } else { Shift::Ss3 };
            let mut p = pre;
            p.extend_from_slice(ss.bytes());
            v.push((p, s1, false));
        }
    }
    v
}

struct Node {
    state: State,
    cost: usize,
    prev: usize,
    bytes: Vec<u8>,
}

/// States costing more than the cheapest one by this much are dropped.
const PRUNE: usize = 10;

/// Encode `text` from the default initial state (SI) for the default
/// width mapping. See [`encode_with`].
pub fn encode(text: &str) -> Result<AribString, EncodeError> {
    encode_with(text, &EncodeOptions::default())
}

/// Encode `text` as an 8-unit code string.
///
/// ARIB STD-B24 Vol.1 Part 2 §7.1; see the module documentation for the
/// subset of the code used. Combining characters of Table E-1 following a
/// base character are sent as the Kanji set's non-spacing characters in
/// front of it (§7.1.1.4). `'\n'` is sent as APR.
pub fn encode_with(text: &str, opts: &EncodeOptions) -> Result<AribString, EncodeError> {
    // Tokenise: move each run of combining characters in front of its base.
    let mut toks: Vec<(usize, char)> = Vec::new();
    let mut base_at: Option<usize> = None; // index in toks of the last base
    for (idx, ch) in text.char_indices() {
        if tables::is_non_spacing(ch) {
            match base_at {
                Some(b) => {
                    // Insert before the base, after earlier marks of the same base.
                    let pos = toks.len() - 1;
                    toks.insert(pos, (idx, ch));
                    base_at = Some(b + 1);
                }
                None => return Err(EncodeError::NoBase { index: idx, ch }),
            }
        } else {
            toks.push((idx, ch));
            base_at = if ch == '\n' {
                None
            } else {
                Some(toks.len() - 1)
            };
        }
    }

    let (gl, gr) = opts.initial.invocations();
    let init = State {
        g: opts.initial.designations(),
        gl,
        gr,
        narrow: false,
    };
    let mut layers: Vec<Vec<Node>> = vec![vec![Node {
        state: init,
        cost: 0,
        prev: usize::MAX,
        bytes: Vec::new(),
    }]];

    for &(idx, ch) in &toks {
        let cands = candidates(ch, opts.width);
        if cands.is_empty() {
            return Err(EncodeError::Unencodable { index: idx, ch });
        }
        let prev = layers.last().expect("non-empty");
        let mut next: Vec<Node> = Vec::new();
        let mut index: HashMap<State, usize> = HashMap::new();
        let mut push =
            |state: State, cost: usize, prev: usize, bytes: Vec<u8>| match index.get(&state) {
                Some(&i) if next[i].cost <= cost => {}
                Some(&i) => {
                    next[i] = Node {
                        state,
                        cost,
                        prev,
                        bytes,
                    }
                }
                None => {
                    index.insert(state, next.len());
                    next.push(Node {
                        state,
                        cost,
                        prev,
                        bytes,
                    });
                }
            };
        for (pi, node) in prev.iter().enumerate() {
            for cand in &cands {
                let size = match cand {
                    Cand::Graphic { size, .. } | Cand::Space { size } => *size,
                    Cand::Newline => Size::Any,
                };
                let mut s0 = node.state;
                let mut pre = Vec::new();
                match size {
                    Size::Narrow if !s0.narrow => {
                        pre.push(c::MSZ);
                        s0.narrow = true;
                    }
                    Size::Wide if s0.narrow => {
                        pre.push(c::NSZ);
                        s0.narrow = false;
                    }
                    _ => {}
                }
                match *cand {
                    Cand::Newline => {
                        pre.push(c::APR);
                        push(s0, node.cost + pre.len(), pi, pre);
                    }
                    Cand::Space { .. } => {
                        pre.push(c::SP);
                        push(s0, node.cost + pre.len(), pi, pre);
                    }
                    Cand::Graphic { set, code, .. } => {
                        let n = set.bytes_per_char();
                        for (p, s1, in_gr) in ways(&s0, set) {
                            let mut bytes = pre.clone();
                            bytes.extend(p);
                            let hi = if in_gr { 0x80 } else { 0 };
                            bytes.extend(code[..n].iter().map(|b| b | hi));
                            push(s1, node.cost + bytes.len(), pi, bytes);
                        }
                    }
                }
            }
        }
        let best = next.iter().map(|n| n.cost).min().unwrap_or(0);
        next.retain(|n| n.cost <= best + PRUNE);
        layers.push(next);
    }

    // Backtrack from the cheapest final node (first on ties).
    let last = layers.last().expect("non-empty");
    let mut i = (0..last.len()).min_by_key(|&i| last[i].cost).unwrap_or(0);
    let mut chunks = Vec::new();
    for layer in layers.iter().rev() {
        let node = &layer[i];
        if node.prev == usize::MAX {
            break;
        }
        chunks.push(&node.bytes);
        i = node.prev;
    }
    let out = chunks.into_iter().rev().flatten().copied().collect();
    Ok(AribString(out))
}

//! The captions of ISDB-S3 / advanced BS (ARIB STD-B60 §9, TTML documents
//! per ARIB STD-B62's profile of W3C TTML) to plain cues, and the cues to
//! SubRip (`.srt`) and WebVTT (`.vtt`).
//!
//! A caption MFU carries one whole TTML document (`isdb_mmt::es::Subtitle`
//! with data type 0). On the BS recordings each document holds one
//! `<div>` of one or more `<p>` lines, timed in one of two ways:
//!
//! - `begin` **and** `end` on the `<div>` (or below): a closed cue;
//! - `begin` alone: the cue stays up until the next document — the
//!   following document replaces it, and a document whose body is empty
//!   (`<div><p><span/></p></div>`, no time) clears the screen.
//!
//! The `begin`/`end` clock values are offsets from the start of the
//! current event (programme): BS11 18:00 event, captions at 00:42:13;
//! BS17 18:30 event, captions at 00:13:39 — both with TOT 18:43. A
//! document's time is therefore resolved by the caller, who knows the
//! event start and its own media clock; [`Timeline`] takes the document
//! with that resolution and the arrival time (for an untimed clear).
//!
//! Two outputs:
//!
//! - [`to_srt`] and the plain [`Cue::lines`]: text, one line per `<p>`,
//!   `<br/>` breaking a line — what a SubRip / Matroska `S_TEXT/UTF8`
//!   track can hold;
//! - [`to_vtt`]: the document as WebVTT keeps it — each `<p>` a cue at
//!   its region's place (`line`/`position`/`size` from `tts:origin` and
//!   `tts:extent` on the root canvas), its spans as classes of a `STYLE`
//!   block (colour, background, the outline as a text shadow, font family
//!   and size, line height, weight and style, underline), `<br/>` and
//!   ruby as they are. What WebVTT cannot express is dropped, see
//!   [`style`].

pub mod style;
pub mod xml;

pub use style::{Paragraph, Run, Style};

use std::fmt::Write as _;

/// A TTML time expression in milliseconds (W3C TTML2 §10.3.1): clock time
/// `hh:mm:ss(.fff)` or `hh:mm:ss:frames`, offset time `N(.n)(h|m|s|ms|f|t)`.
/// Frames and ticks use `frame_rate` / `tick_rate` (TTML's defaults 30 and
/// 1 when the document names none).
#[must_use]
pub fn parse_time(s: &str, frame_rate: f64, tick_rate: f64) -> Option<i64> {
    let s = s.trim();
    #[allow(
        clippy::cast_possible_truncation,
        reason = "bounded to ±9e15 before the cast"
    )]
    let ms = |x: f64| -> Option<i64> {
        let v = (x * 1000.0).round();
        (v.is_finite() && v.abs() < 9.0e15).then_some(v as i64)
    };
    if s.contains(':') {
        let parts: Vec<&str> = s.split(':').collect();
        let (h, m, sec) = match parts.as_slice() {
            [h, m, sec] => (*h, *m, *sec),
            [h, m, sec, frames] => {
                let f: f64 = frames.parse().ok()?;
                let base = parse_hms(h, m, sec)?;
                return ms(base + f / frame_rate);
            }
            _ => return None,
        };
        return ms(parse_hms(h, m, sec)?);
    }
    let unit_at = s.find(|c: char| c.is_ascii_alphabetic())?;
    let (num, unit) = s.split_at(unit_at);
    let n: f64 = num.parse().ok()?;
    let secs = match unit {
        "h" => n * 3600.0,
        "m" => n * 60.0,
        "s" => n,
        "ms" => n / 1000.0,
        "f" => n / frame_rate,
        "t" => n / tick_rate,
        _ => return None,
    };
    ms(secs)
}

fn parse_hms(h: &str, m: &str, s: &str) -> Option<f64> {
    let digits = |x: &str| !x.is_empty() && x.bytes().all(|b| b.is_ascii_digit());
    let (whole, frac) = s.split_once('.').unwrap_or((s, ""));
    if !digits(h) || !digits(m) || !digits(whole) || !(frac.is_empty() || digits(frac)) {
        return None;
    }
    let h: f64 = h.parse().ok()?;
    let m: f64 = m.parse().ok()?;
    let s: f64 = s.parse().ok()?;
    (m < 60.0 && s < 61.0).then_some(h * 3600.0 + m * 60.0 + s)
}

/// One TTML document as caption lines with its own times (milliseconds on
/// the document's clock; `None` when the document leaves them open).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Document {
    /// `xml:lang` of the root (`ja` on air), when given.
    pub lang: Option<String>,
    /// The timed blocks in document order.
    pub blocks: Vec<Block>,
}

/// A run of lines that shares one time interval.
#[derive(Debug, Clone, PartialEq)]
pub struct Block {
    /// The begin, document clock.
    pub begin: Option<i64>,
    /// The end, document clock.
    pub end: Option<i64>,
    /// The lines (empty: a clear).
    pub lines: Vec<String>,
    /// The same `<p>`s with their styled runs and regions.
    pub paragraphs: Vec<Paragraph>,
}

impl Document {
    /// Whether the document shows nothing (a clear).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.blocks.iter().all(|b| b.lines.is_empty())
    }
}

/// Why a document is not a caption document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// Not well-formed XML (in the supported subset).
    Xml(xml::Error),
    /// The root is not `tt`.
    NotTtml,
    /// A `begin`/`end`/`dur` value does not parse.
    Time(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Xml(e) => e.fmt(f),
            Error::NotTtml => f.write_str("the root element is not tt"),
            Error::Time(t) => write!(f, "the time expression {t:?} does not parse"),
        }
    }
}

impl std::error::Error for Error {}

/// Parse one TTML document (UTF-8 bytes, a BOM allowed).
///
/// # Errors
///
/// See [`Error`].
pub fn parse(bytes: &[u8]) -> Result<Document, Error> {
    let text = String::from_utf8_lossy(bytes);
    let text = text.trim_start_matches('\u{feff}');
    let root = xml::parse(text).map_err(Error::Xml)?;
    if root.local_name() != "tt" {
        return Err(Error::NotTtml);
    }
    let rate = |name: &str, default: f64| {
        root.attr(name)
            .and_then(|v| v.split_whitespace().next()?.parse::<f64>().ok())
            .filter(|r| *r > 0.0)
            .unwrap_or(default)
    };
    let mut frame_rate = rate("frameRate", 30.0);
    if let Some(m) = root.attr("frameRateMultiplier") {
        let mut it = m.split_whitespace().filter_map(|x| x.parse::<f64>().ok());
        if let (Some(n), Some(d)) = (it.next(), it.next())
            && d > 0.0
        {
            frame_rate *= n / d;
        }
    }
    let tick_rate = rate("tickRate", 1.0);
    let mut doc = Document {
        lang: root.attr("lang").map(str::to_owned),
        blocks: Vec::new(),
    };
    let times = Times {
        frame_rate,
        tick_rate,
        styles: style::Sheet::of(&root),
    };
    for body in root.elements().filter(|e| e.local_name() == "body") {
        let inherited = times.styles.resolve(body, &Style::default());
        times.walk(body, None, None, &inherited, &mut doc.blocks)?;
    }
    Ok(doc)
}

struct Times {
    frame_rate: f64,
    tick_rate: f64,
    styles: style::Sheet,
}

impl Times {
    fn get(&self, e: &xml::Element, name: &str) -> Result<Option<i64>, Error> {
        e.attr(name)
            .map(|v| {
                parse_time(v, self.frame_rate, self.tick_rate).ok_or_else(|| Error::Time(v.into()))
            })
            .transpose()
    }

    /// The element's own interval inside its parent's (TTML's `par`
    /// timing: a child's begin is relative to its parent's begin).
    fn interval(
        &self,
        e: &xml::Element,
        parent_begin: Option<i64>,
        parent_end: Option<i64>,
    ) -> Result<(Option<i64>, Option<i64>), Error> {
        let origin = parent_begin.unwrap_or(0);
        let begin = self.get(e, "begin")?.map(|b| origin + b);
        let start = begin.or(parent_begin);
        let mut end = self.get(e, "end")?.map(|t| origin + t);
        if let Some(d) = self.get(e, "dur")? {
            let by_dur = start.map(|s| s + d);
            end = match (end, by_dur) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            };
        }
        let end = match (end, parent_end) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        Ok((start, end))
    }

    /// Collect the `<p>` lines below `e`, grouping the consecutive ones
    /// that share an interval.
    fn walk(
        &self,
        e: &xml::Element,
        parent_begin: Option<i64>,
        parent_end: Option<i64>,
        inherited: &Style,
        out: &mut Vec<Block>,
    ) -> Result<(), Error> {
        let (begin, end) = self.interval(e, parent_begin, parent_end)?;
        if e.local_name() == "p" {
            let mut lines = vec![String::new()];
            self.text(e, &mut lines);
            let lines: Vec<String> = lines
                .into_iter()
                .map(|l| l.trim().to_owned())
                .filter(|l| !l.is_empty())
                .collect();
            let para = self.styles.paragraph(e, inherited);
            let paragraphs: Vec<Paragraph> = para.into_iter().collect();
            match out.last_mut() {
                Some(b) if b.begin == begin && b.end == end => {
                    b.lines.extend(lines);
                    b.paragraphs.extend(paragraphs);
                }
                _ => out.push(Block {
                    begin,
                    end,
                    lines,
                    paragraphs,
                }),
            }
            return Ok(());
        }
        let before = out.len();
        for c in e.elements() {
            let style = self.styles.resolve(c, inherited);
            self.walk(c, begin, end, &style, out)?;
        }
        // A timed container with no lines below (a `div` holding only an
        // empty `p`) still marks its time: a clear.
        if out.len() == before && e.local_name() == "div" {
            out.push(Block {
                begin,
                end,
                lines: Vec::new(),
                paragraphs: Vec::new(),
            });
        }
        Ok(())
    }

    fn text(&self, e: &xml::Element, lines: &mut Vec<String>) {
        for n in &e.children {
            match n {
                xml::Node::Text(t) => {
                    // XML whitespace inside a line collapses (TTML's
                    // default xml:space="default").
                    let cur = lines.last_mut().expect("one line at least");
                    let mut prev_space = cur.ends_with(' ');
                    for c in t.chars() {
                        if matches!(c, ' ' | '\t' | '\n' | '\r') {
                            if !prev_space && !cur.is_empty() {
                                cur.push(' ');
                            }
                            prev_space = true;
                        } else {
                            cur.push(c);
                            prev_space = false;
                        }
                    }
                }
                xml::Node::Element(c) => match c.local_name() {
                    "br" => lines.push(String::new()),
                    // ARIB ruby: the reading text is dropped, the base
                    // text kept.
                    "rt" | "rp" => {}
                    "metadata" | "set" | "animate" | "image" => {}
                    _ => self.text(c, lines),
                },
            }
        }
    }
}

/// A cue on the output clock (milliseconds).
#[derive(Debug, Clone, PartialEq)]
pub struct Cue {
    /// Shown from.
    pub start: i64,
    /// Shown until.
    pub end: i64,
    /// The lines.
    pub lines: Vec<String>,
    /// The same text as styled paragraphs at their regions (WebVTT).
    pub paragraphs: Vec<Paragraph>,
}

/// The documents of one caption stream into cues: a cue left open by its
/// document ends at the next document's time (or the next one's arrival
/// when that one names no time), and at most `max_open` after its start.
#[derive(Debug, Clone)]
pub struct Timeline {
    cues: Vec<Cue>,
    open: Option<(i64, Vec<String>, Vec<Paragraph>)>,
    /// The longest a cue may stay open (ms).
    pub max_open: i64,
}

impl Default for Timeline {
    fn default() -> Self {
        Self {
            cues: Vec::new(),
            open: None,
            max_open: 10_000,
        }
    }
}

impl Timeline {
    /// An empty timeline.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Take one document. `to_output` maps a document clock value to the
    /// output clock (ms), `None` when it cannot; `arrival` is the
    /// document's arrival on the output clock (the untimed clear's time).
    pub fn push(
        &mut self,
        doc: &Document,
        to_output: impl Fn(i64) -> Option<i64>,
        arrival: Option<i64>,
    ) {
        let first_time = doc
            .blocks
            .iter()
            .find_map(|b| b.begin.and_then(&to_output))
            .or(arrival);
        // Whatever this document brings replaces what was up.
        if let Some(t) = first_time {
            self.close(t);
        }
        for b in &doc.blocks {
            let start = b.begin.and_then(&to_output).or(arrival);
            let Some(start) = start else { continue };
            if b.lines.is_empty() {
                self.close(start);
                continue;
            }
            match b.end.and_then(&to_output) {
                Some(end) if end > start => self.cues.push(Cue {
                    start,
                    end,
                    lines: b.lines.clone(),
                    paragraphs: b.paragraphs.clone(),
                }),
                Some(_) => {}
                None => {
                    self.close(start);
                    self.open = Some((start, b.lines.clone(), b.paragraphs.clone()));
                }
            }
        }
    }

    fn close(&mut self, at: i64) {
        if let Some((start, lines, paragraphs)) = self.open.take() {
            let end = at.min(start + self.max_open);
            if end > start {
                self.cues.push(Cue {
                    start,
                    end,
                    lines,
                    paragraphs,
                });
            }
        }
    }

    /// The cues, the open one closed at `end` (or after `max_open`),
    /// sorted by start.
    #[must_use]
    pub fn finish(mut self, end: Option<i64>) -> Vec<Cue> {
        if let Some((start, ..)) = &self.open {
            let at = end.unwrap_or(start + self.max_open);
            self.close(at);
        }
        self.cues.sort_by_key(|c| (c.start, c.end));
        self.cues
    }
}

pub(crate) fn stamp(ms: i64, frac_sep: char) -> String {
    let ms = ms.max(0);
    format!(
        "{:02}:{:02}:{:02}{frac_sep}{:03}",
        ms / 3_600_000,
        ms / 60_000 % 60,
        ms / 1000 % 60,
        ms % 1000
    )
}

/// The cues as SubRip.
#[must_use]
pub fn to_srt(cues: &[Cue]) -> String {
    let mut out = String::new();
    for (i, c) in cues.iter().enumerate() {
        let _ = write!(
            out,
            "{}\r\n{} --> {}\r\n{}\r\n\r\n",
            i + 1,
            stamp(c.start, ','),
            stamp(c.end, ','),
            c.lines.join("\r\n")
        );
    }
    out
}

/// The cues as WebVTT, styled and placed as their TTML has them (see
/// [`style::write_vtt`]; a cue without paragraphs is written as its plain
/// lines).
#[must_use]
pub fn to_vtt(cues: &[Cue]) -> String {
    style::write_vtt(cues)
}

#[cfg(test)]
mod tests {
    use super::*;

    const BS17: &str = r#"<?xml version="1.0" encoding="utf-8"?><tt xmlns="http://www.w3.org/ns/ttml" xml:lang="ja"><head/><body><div begin="00:13:39.667" end="00:13:42.334"><p region="r390"><span>礼拝のあと</span><span>　</span><span>ジェーンさんは➡</span></p></div></body></tt>"#;
    const BS11_OPEN: &str = r#"<tt xmlns="http://www.w3.org/ns/ttml" xml:lang="ja"><body><div begin="00:42:13.854"><p><span>中には</span><span>、</span></p><p><span>二行目</span></p></div></body></tt>"#;
    const BS11_CLEAR: &str = r#"<tt xmlns="http://www.w3.org/ns/ttml"><head/><body><div><p><span/></p></div></body></tt>"#;

    #[test]
    fn time_expressions() {
        assert_eq!(parse_time("00:13:39.667", 30.0, 1.0), Some(819_667));
        assert_eq!(parse_time("01:00:00:15", 30.0, 1.0), Some(3_600_500));
        assert_eq!(parse_time("1.5s", 30.0, 1.0), Some(1500));
        assert_eq!(parse_time("250ms", 30.0, 1.0), Some(250));
        assert_eq!(parse_time("90000t", 30.0, 90000.0), Some(1000));
        assert_eq!(parse_time("2m", 30.0, 1.0), Some(120_000));
        assert_eq!(parse_time("00:61:00", 30.0, 1.0), None);
        assert_eq!(parse_time("x", 30.0, 1.0), None);
    }

    #[test]
    fn closed_and_open_documents() {
        let d = parse(BS17.as_bytes()).unwrap();
        assert_eq!(d.lang.as_deref(), Some("ja"));
        assert_eq!(d.blocks.len(), 1);
        assert_eq!(
            (d.blocks[0].begin, d.blocks[0].end),
            (Some(819_667), Some(822_334))
        );
        assert_eq!(d.blocks[0].lines, vec!["礼拝のあと　ジェーンさんは➡"]);
        assert_eq!(d.blocks[0].paragraphs.len(), 1);
        let d = parse(BS11_OPEN.as_bytes()).unwrap();
        assert_eq!(d.blocks[0].end, None);
        assert_eq!(d.blocks[0].lines, vec!["中には、", "二行目"]);
        let d = parse(BS11_CLEAR.as_bytes()).unwrap();
        assert!(d.is_empty());
        assert_eq!(d.blocks.len(), 1);
        assert_eq!(d.blocks[0].begin, None);
    }

    #[test]
    fn open_cue_ends_at_the_next_document() {
        let mut t = Timeline::new();
        let shift = |ms: i64| Some(ms - 2_533_000);
        t.push(&parse(BS11_OPEN.as_bytes()).unwrap(), shift, Some(0));
        // The untimed clear: its arrival time.
        t.push(&parse(BS11_CLEAR.as_bytes()).unwrap(), shift, Some(3_000));
        t.push(
            &parse(BS11_OPEN.replace("42:13.854", "42:20.000").as_bytes()).unwrap(),
            shift,
            Some(5_000),
        );
        let cues = t.finish(Some(20_000));
        assert_eq!(cues.len(), 2);
        assert_eq!((cues[0].start, cues[0].end), (854, 3_000));
        assert_eq!(
            (cues[1].start, cues[1].end),
            (7_000, 17_000),
            "max_open caps the last"
        );
    }

    #[test]
    fn nested_begin_is_relative_and_br_breaks() {
        let d = parse(
            br#"<tt xmlns="http://www.w3.org/ns/ttml"><body begin="10s"><div><p begin="1s" dur="2s">a<br/>b <span>c</span><ruby><rb>d</rb><rt>e</rt></ruby></p></div></body></tt>"#,
        )
        .unwrap();
        assert_eq!(d.blocks.len(), 1);
        assert_eq!(
            (d.blocks[0].begin, d.blocks[0].end),
            (Some(11_000), Some(13_000))
        );
        assert_eq!(d.blocks[0].lines, vec!["a", "b cd"]);
    }

    #[test]
    fn writers() {
        let cues = vec![Cue {
            start: 3_723_004,
            end: 3_725_000,
            lines: vec!["a<b>".into(), "c --> d".into()],
            paragraphs: Vec::new(),
        }];
        assert_eq!(
            to_srt(&cues),
            "1\r\n01:02:03,004 --> 01:02:05,000\r\na<b>\r\nc --> d\r\n\r\n"
        );
        assert_eq!(
            to_vtt(&cues),
            "WEBVTT\n\n01:02:03.004 --> 01:02:05.000\na&lt;b&gt;\nc --&gt; d\n\n"
        );
    }
}

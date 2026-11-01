//! TTML styling and layout as WebVTT keeps them.
//!
//! Style resolution follows TTML (§8.4): a `<style>` element's
//! attributes, then the styles its own `style` attribute references (in
//! order), make a referential style; an element takes the referenced
//! styles in order, then its inline `tts:*` attributes; the inheritable
//! properties pass from `region`/`body`/`div`/`p` to the spans. The
//! region of a `<p>` (its own or an ancestor's `region` attribute) gives
//! its place: `tts:origin` and `tts:extent` in pixels of the root canvas
//! (`tts:extent` on `<tt>`, else ARIB-TT's 3840×2160 — the BS 4K
//! documents carry pixel regions up to 3128×1916 and no root extent).
//!
//! The WebVTT form ([`write_vtt`], WebVTT §4–§8):
//!
//! | TTML | WebVTT |
//! |---|---|
//! | `<p>` in a region | one cue: `line:Y%` (top edge, `line` alignment start), `position:X%,line-left`, `size:W%`, `align:` from `tts:textAlign` (start) |
//! | `tts:writingMode` `tb*` | `vertical:rl` / `lr` |
//! | a span's style | `<c.sN>…</c>`, `::cue(.sN)` in the `STYLE` block |
//! | `tts:color`, `tts:backgroundColor` | `color`, `background-color` (`rgba`) |
//! | `tts:textOutline` (colour, thickness) | `text-shadow`: the outline as eight offset shadows |
//! | `tts:fontFamily`, `fontSize` (height), `lineHeight` | `font-family`, `font-size` / `line-height` in `vh` of the canvas |
//! | `tts:fontWeight`, `fontStyle`, `textDecoration` underline | `font-weight`, `font-style`, `text-decoration` |
//! | `<br/>`, `<ruby>`/`<rt>` | a line break, `<ruby>…<rt>…</rt></ruby>` |
//!
//! Dropped, WebVTT having no equivalent: `arib-tt:letter-spacing` (no
//! `letter-spacing` on `::cue`), a font size's width when it differs from
//! its height (ARIB's half-width `72px 144px`: WebVTT scales glyphs by
//! height only), `tts:origin` beyond the line/position model (the box is
//! the region's; WebVTT moves it only to avoid overlaps), animations
//! (`<set>`, `<animate>`), images. ARIB font names (丸ゴシック) are kept
//! first in `font-family`, with a generic `sans-serif` after them.

use crate::Cue;
use crate::xml::{Element, Node};
use std::collections::BTreeMap;
use std::fmt::Write as _;

/// The caption plane when the document names none (ARIB-TT, 4K).
pub const DEFAULT_CANVAS: (f64, f64) = (3840.0, 2160.0);

/// The resolved styling of a run (each field `None`: not set).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Style {
    /// `tts:color` as RGBA.
    pub color: Option<[u8; 4]>,
    /// `tts:backgroundColor` as RGBA (not inherited).
    pub background: Option<[u8; 4]>,
    /// `tts:textOutline`: colour and thickness (px).
    pub outline: Option<([u8; 4], f64)>,
    /// `tts:fontFamily`, as listed.
    pub font_family: Option<String>,
    /// `tts:fontSize`: width and height (px; equal when one value).
    pub font_size: Option<(f64, f64)>,
    /// `tts:lineHeight` (px).
    pub line_height: Option<f64>,
    /// `tts:fontWeight` bold.
    pub bold: Option<bool>,
    /// `tts:fontStyle` italic / oblique.
    pub italic: Option<bool>,
    /// `tts:textDecoration` underline.
    pub underline: Option<bool>,
    /// `tts:textAlign`.
    pub text_align: Option<String>,
    /// `tts:writingMode`.
    pub writing_mode: Option<String>,
}

impl Style {
    /// `over` laid onto `self` (its set fields win).
    fn over(&self, over: &Style) -> Style {
        Style {
            color: over.color.or(self.color),
            background: over.background.or(self.background),
            outline: over.outline.or(self.outline),
            font_family: over
                .font_family
                .clone()
                .or_else(|| self.font_family.clone()),
            font_size: over.font_size.or(self.font_size),
            line_height: over.line_height.or(self.line_height),
            bold: over.bold.or(self.bold),
            italic: over.italic.or(self.italic),
            underline: over.underline.or(self.underline),
            text_align: over.text_align.clone().or_else(|| self.text_align.clone()),
            writing_mode: over
                .writing_mode
                .clone()
                .or_else(|| self.writing_mode.clone()),
        }
    }

    /// What a child inherits (TTML: all but the background).
    fn inherited(&self) -> Style {
        Style {
            background: None,
            ..self.clone()
        }
    }

    /// The `tts:*` attributes of one element.
    fn of_attributes(e: &Element) -> Style {
        let mut s = Style::default();
        for (k, v) in &e.attributes {
            let Some(name) = k.strip_prefix("tts:") else {
                continue;
            };
            match name {
                "color" => s.color = parse_color(v),
                "backgroundColor" => s.background = parse_color(v),
                "textOutline" => s.outline = parse_outline(v, s.color),
                "fontFamily" => s.font_family = Some(v.trim().to_owned()),
                "fontSize" => s.font_size = parse_size(v),
                "lineHeight" => s.line_height = parse_length(v),
                "fontWeight" => s.bold = Some(v.trim() == "bold"),
                "fontStyle" => s.italic = Some(matches!(v.trim(), "italic" | "oblique")),
                "textDecoration" => {
                    s.underline = Some(v.split_whitespace().any(|t| t == "underline"));
                }
                "textAlign" => s.text_align = Some(v.trim().to_owned()),
                "writingMode" => s.writing_mode = Some(v.trim().to_owned()),
                _ => {}
            }
        }
        s
    }
}

/// A run of text with one style, or a line break, or ruby.
#[derive(Debug, Clone, PartialEq)]
pub enum Run {
    /// Text.
    Text(String, Style),
    /// `<br/>`.
    Break,
    /// `<ruby>`: base text, reading, the style of the base.
    Ruby(String, String, Style),
}

/// One `<p>`: its runs and its place on the canvas (percentages).
#[derive(Debug, Clone, PartialEq)]
pub struct Paragraph {
    /// The runs in order.
    pub runs: Vec<Run>,
    /// The region's left / top edge (% of the canvas width / height).
    pub origin: Option<(f64, f64)>,
    /// The region's width / height (% of the canvas width / height).
    pub extent: Option<(f64, f64)>,
    /// The paragraph's own style (alignment, writing mode, background).
    pub style: Style,
}

/// A `<region>`: origin and extent (px), its own style.
type Region = (Option<(f64, f64)>, Option<(f64, f64)>, Style);

/// The styles and regions of one document.
#[derive(Debug, Clone, Default)]
pub struct Sheet {
    /// Referential styles by `xml:id`, resolved.
    styles: BTreeMap<String, Style>,
    /// Regions by `xml:id`.
    regions: BTreeMap<String, Region>,
    /// The root canvas (px).
    canvas: (f64, f64),
}

impl Sheet {
    /// The `<styling>` and `<layout>` of the document `tt`.
    #[must_use]
    pub fn of(tt: &Element) -> Sheet {
        let canvas = tt
            .attr("extent")
            .and_then(parse_size)
            .filter(|&(w, h)| w > 0.0 && h > 0.0)
            .unwrap_or(DEFAULT_CANVAS);
        let mut sheet = Sheet {
            canvas,
            ..Sheet::default()
        };
        let head = tt.elements().find(|e| e.local_name() == "head");
        let defs: BTreeMap<String, &Element> = head
            .into_iter()
            .flat_map(|h| h.elements())
            .filter(|e| e.local_name() == "styling")
            .flat_map(Element::elements)
            .filter(|e| e.local_name() == "style")
            .filter_map(|e| Some((e.attr("id")?.to_owned(), e)))
            .collect();
        for id in defs.keys() {
            let s = resolve_ref(id, &defs, &mut Vec::new());
            sheet.styles.insert(id.clone(), s);
        }
        for r in head
            .into_iter()
            .flat_map(|h| h.elements())
            .filter(|e| e.local_name() == "layout")
            .flat_map(Element::elements)
            .filter(|e| e.local_name() == "region")
        {
            let Some(id) = r.attr("id") else { continue };
            let own = sheet.resolve(r, &Style::default());
            sheet.regions.insert(
                id.to_owned(),
                (
                    r.attr("origin").and_then(parse_size),
                    r.attr("extent").and_then(parse_size),
                    own,
                ),
            );
        }
        sheet
    }

    /// The style of `e` within `inherited`: the referenced styles, then
    /// its inline attributes, over what it inherits.
    #[must_use]
    pub fn resolve(&self, e: &Element, inherited: &Style) -> Style {
        let mut s = inherited.inherited();
        for id in e.attr("style").unwrap_or_default().split_whitespace() {
            if let Some(r) = self.styles.get(id) {
                s = s.over(r);
            }
        }
        s.over(&Style::of_attributes(e))
    }

    /// One `<p>` (its style resolved into `inherited`, the parent's) as
    /// runs and a place; `None` when it holds no text.
    #[must_use]
    pub fn paragraph(&self, p: &Element, inherited: &Style) -> Option<Paragraph> {
        let region = p.attr("region").and_then(|r| self.regions.get(r));
        // The region's style sits between the parent's and the `p`'s own.
        let base = match region {
            Some((.., rs)) => inherited.over(&rs.inherited()),
            None => inherited.clone(),
        };
        let style = self.resolve(p, &base);
        let mut runs = Vec::new();
        self.runs(p, &style, &mut runs);
        // Leading / trailing XML whitespace of the paragraph goes.
        trim_runs(&mut runs);
        if !runs
            .iter()
            .any(|r| matches!(r, Run::Text(t, _) | Run::Ruby(t, ..) if !t.is_empty()))
        {
            return None;
        }
        // Places as % of the canvas, lengths as % of its height (vh).
        let (cw, ch) = self.canvas;
        let pct = |(x, y): (f64, f64)| (x / cw * 100.0, y / ch * 100.0);
        for r in &mut runs {
            if let Run::Text(_, s) | Run::Ruby(_, _, s) = r {
                scale(s, ch);
            }
        }
        let mut style = style;
        scale(&mut style, ch);
        Some(Paragraph {
            runs,
            origin: region.and_then(|r| r.0).map(pct),
            extent: region.and_then(|r| r.1).map(pct),
            style,
        })
    }

    fn runs(&self, e: &Element, style: &Style, out: &mut Vec<Run>) {
        for n in &e.children {
            match n {
                Node::Text(t) => push_text(out, &collapse(t), style),
                Node::Element(c) => match c.local_name() {
                    "br" => out.push(Run::Break),
                    "ruby" => {
                        let s = self.resolve(c, style);
                        let (mut base, mut reading) = (String::new(), String::new());
                        for k in c.elements() {
                            let mut t = String::new();
                            text_of(k, &mut t);
                            match k.local_name() {
                                "rt" => reading.push_str(&t),
                                "rp" => {}
                                _ => base.push_str(&t),
                            }
                        }
                        for n in &c.children {
                            if let Node::Text(t) = n {
                                base.push_str(&collapse(t));
                            }
                        }
                        out.push(Run::Ruby(base, reading, s));
                    }
                    "metadata" | "set" | "animate" | "image" | "rt" | "rp" => {}
                    _ => {
                        let s = self.resolve(c, style);
                        self.runs(c, &s, out);
                    }
                },
            }
        }
    }
}

/// A referential style with the styles it references (§8.4.1.1); a
/// reference loop stops at the repeat.
fn resolve_ref(id: &str, defs: &BTreeMap<String, &Element>, seen: &mut Vec<String>) -> Style {
    let Some(e) = defs.get(id) else {
        return Style::default();
    };
    if seen.iter().any(|s| s == id) {
        return Style::default();
    }
    seen.push(id.to_owned());
    let mut s = Style::default();
    for r in e.attr("style").unwrap_or_default().split_whitespace() {
        s = s.over(&resolve_ref(r, defs, seen));
    }
    s.over(&Style::of_attributes(e))
}

fn text_of(e: &Element, out: &mut String) {
    for n in &e.children {
        match n {
            Node::Text(t) => out.push_str(&collapse(t)),
            Node::Element(c) => text_of(c, out),
        }
    }
}

/// XML whitespace collapsed to single spaces (`xml:space="default"`).
fn collapse(t: &str) -> String {
    let mut out = String::with_capacity(t.len());
    let mut space = false;
    for c in t.chars() {
        if matches!(c, ' ' | '\t' | '\n' | '\r') {
            space = true;
        } else {
            if space && !out.is_empty() {
                out.push(' ');
            }
            space = false;
            out.push(c);
        }
    }
    if space && !out.is_empty() {
        out.push(' ');
    }
    // A run made only of XML whitespace still separates words.
    if out.is_empty() && !t.is_empty() {
        out.push(' ');
    }
    out
}

/// Append text, merging with the previous run of the same style.
fn push_text(out: &mut Vec<Run>, t: &str, style: &Style) {
    if t.is_empty() {
        return;
    }
    if let Some(Run::Text(prev, s)) = out.last_mut()
        && s == style
    {
        prev.push_str(t);
        return;
    }
    out.push(Run::Text(t.to_owned(), style.clone()));
}

/// Trim the XML whitespace at the ends of a paragraph and around breaks.
fn trim_runs(runs: &mut Vec<Run>) {
    let mut at_line_start = true;
    for i in 0..runs.len() {
        match &mut runs[i] {
            Run::Break => at_line_start = true,
            Run::Text(t, _) => {
                if at_line_start {
                    *t = t.trim_start_matches(' ').to_owned();
                }
                at_line_start = t.is_empty() && at_line_start;
                let next_breaks = runs.get(i + 1).is_none_or(|r| matches!(r, Run::Break));
                if let Run::Text(t, _) = &mut runs[i]
                    && next_breaks
                {
                    *t = t.trim_end_matches(' ').to_owned();
                }
            }
            Run::Ruby(..) => at_line_start = false,
        }
    }
    runs.retain(|r| !matches!(r, Run::Text(t, _) if t.is_empty()));
}

/// A style's px lengths as % of the canvas height (vh).
fn scale(s: &mut Style, canvas_h: f64) {
    let vh = |px: f64| px / canvas_h * 100.0;
    s.font_size = s.font_size.map(|(w, h)| (vh(w), vh(h)));
    s.line_height = s.line_height.map(vh);
    s.outline = s.outline.map(|(c, t)| (c, vh(t)));
}

/// `#rrggbb`, `#rrggbbaa`, `rgb(r,g,b)`, `rgba(r,g,b,a)` or a TTML named
/// colour.
#[must_use]
pub fn parse_color(v: &str) -> Option<[u8; 4]> {
    let v = v.trim();
    if let Some(h) = v.strip_prefix('#') {
        let byte = |i: usize| u8::from_str_radix(h.get(i..i + 2)?, 16).ok();
        return match h.len() {
            6 => Some([byte(0)?, byte(2)?, byte(4)?, 255]),
            8 => Some([byte(0)?, byte(2)?, byte(4)?, byte(6)?]),
            _ => None,
        };
    }
    if let Some(args) = v
        .strip_prefix("rgba(")
        .or_else(|| v.strip_prefix("rgb("))
        .and_then(|a| a.strip_suffix(')'))
    {
        let n: Vec<u8> = args
            .split(',')
            .map(|x| x.trim().parse::<u8>().ok())
            .collect::<Option<_>>()?;
        return match n.as_slice() {
            [r, g, b] => Some([*r, *g, *b, 255]),
            [r, g, b, a] => Some([*r, *g, *b, *a]),
            _ => None,
        };
    }
    Some(match v {
        "transparent" => [0, 0, 0, 0],
        "black" => [0, 0, 0, 255],
        "silver" => [192, 192, 192, 255],
        "gray" => [128, 128, 128, 255],
        "white" => [255, 255, 255, 255],
        "maroon" => [128, 0, 0, 255],
        "red" => [255, 0, 0, 255],
        "purple" => [128, 0, 128, 255],
        "fuchsia" | "magenta" => [255, 0, 255, 255],
        "green" => [0, 128, 0, 255],
        "lime" => [0, 255, 0, 255],
        "olive" => [128, 128, 0, 255],
        "yellow" => [255, 255, 0, 255],
        "navy" => [0, 0, 128, 255],
        "blue" => [0, 0, 255, 255],
        "teal" => [0, 128, 128, 255],
        "aqua" | "cyan" => [0, 255, 255, 255],
        _ => return None,
    })
}

/// A pixel length (`12px`, `12`); other units are not resolved.
fn parse_length(v: &str) -> Option<f64> {
    let v = v.trim();
    v.strip_suffix("px")
        .unwrap_or(v)
        .trim()
        .parse()
        .ok()
        .filter(|x: &f64| x.is_finite())
}

/// One or two pixel lengths (`w h`; one value is both).
fn parse_size(v: &str) -> Option<(f64, f64)> {
    let mut it = v.split_whitespace().map(parse_length);
    let a = it.next()??;
    let b = it.next().unwrap_or(Some(a))?;
    Some((a, b))
}

/// `tts:textOutline`: `[colour] thickness [blur]`; without a colour the
/// text's own.
fn parse_outline(v: &str, text: Option<[u8; 4]>) -> Option<([u8; 4], f64)> {
    let parts: Vec<&str> = v.split_whitespace().collect();
    if parts.first() == Some(&"none") {
        return None;
    }
    match parts.as_slice() {
        [c, t, ..] if parse_color(c).is_some() => Some((parse_color(c)?, parse_length(t)?)),
        [t, ..] => Some((text.unwrap_or([0, 0, 0, 255]), parse_length(t)?)),
        _ => None,
    }
}

fn css_color(c: [u8; 4]) -> String {
    if c[3] == 255 {
        format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2])
    } else {
        format!(
            "rgba({},{},{},{:.3})",
            c[0],
            c[1],
            c[2],
            f64::from(c[3]) / 255.0
        )
    }
}

/// A number with at most 3 decimals and no trailing zeros.
fn num(x: f64) -> String {
    let s = format!("{x:.3}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" { "0".into() } else { s.into() }
}

/// The CSS declarations a run's style gives `::cue(.class)`.
fn css(s: &Style) -> String {
    let mut d = Vec::new();
    if let Some(c) = s.color {
        d.push(format!("color: {}", css_color(c)));
    }
    if let Some(c) = s.background {
        d.push(format!("background-color: {}", css_color(c)));
    }
    if let Some((c, t)) = s.outline
        && t > 0.0
    {
        // An outline of thickness t: shadows at the eight compass points.
        let t = num(t);
        let c = css_color(c);
        let shadows: Vec<String> = [
            (1, 0),
            (-1, 0),
            (0, 1),
            (0, -1),
            (1, 1),
            (-1, -1),
            (1, -1),
            (-1, 1),
        ]
        .iter()
        .map(|(x, y)| {
            let p = |k: i32| match k {
                0 => "0".to_owned(),
                1 => format!("{t}vh"),
                _ => format!("-{t}vh"),
            };
            format!("{} {} {c}", p(*x), p(*y))
        })
        .collect();
        d.push(format!("text-shadow: {}", shadows.join(", ")));
    }
    if let Some(f) = &s.font_family {
        let mut fams: Vec<String> = f
            .split(',')
            .map(|x| x.trim().trim_matches(['"', '\'']).to_owned())
            .filter(|x| !x.is_empty())
            .map(|x| match x.as_str() {
                "default" | "proportionalSansSerif" | "sansSerif" => "sans-serif".to_owned(),
                "monospaceSansSerif" | "monospace" | "monospaceSerif" => "monospace".to_owned(),
                "proportionalSerif" | "serif" => "serif".to_owned(),
                _ => format!("\"{}\"", x.replace('"', "")),
            })
            .collect();
        if !fams.iter().any(|f| !f.starts_with('"')) {
            fams.push("sans-serif".into());
        }
        d.push(format!("font-family: {}", fams.join(", ")));
    }
    if let Some((_, h)) = s.font_size {
        d.push(format!("font-size: {}vh", num(h)));
    }
    if let Some(l) = s.line_height {
        d.push(format!("line-height: {}vh", num(l)));
    }
    if let Some(b) = s.bold {
        d.push(format!(
            "font-weight: {}",
            if b { "bold" } else { "normal" }
        ));
    }
    if let Some(i) = s.italic {
        d.push(format!(
            "font-style: {}",
            if i { "italic" } else { "normal" }
        ));
    }
    if s.underline == Some(true) {
        d.push("text-decoration: underline".into());
    }
    d.join("; ")
}

fn escape(t: &str) -> String {
    t.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace("-->", "--&gt;")
}

/// The cue settings of a paragraph's place.
fn settings(p: &Paragraph) -> String {
    let mut s = Vec::new();
    let vertical = match p.style.writing_mode.as_deref() {
        Some("tbrl" | "tb") => Some("rl"),
        Some("tblr") => Some("lr"),
        _ => None,
    };
    if let Some(v) = vertical {
        s.push(format!("vertical:{v}"));
    }
    if let Some((x, y)) = p.origin {
        let (x, y) = (x.clamp(0.0, 100.0), y.clamp(0.0, 100.0));
        if vertical.is_some() {
            s.push(format!("line:{}%,start", num(x)));
            s.push(format!("position:{}%,line-left", num(y)));
        } else {
            s.push(format!("line:{}%,start", num(y)));
            s.push(format!("position:{}%,line-left", num(x)));
        }
        if let Some((w, h)) = p.extent {
            let size = if vertical.is_some() {
                h.min(100.0 - y)
            } else {
                w.min(100.0 - x)
            };
            s.push(format!("size:{}%", num(size.max(0.0))));
        }
    }
    let align = match p.style.text_align.as_deref() {
        Some("center") => "center",
        Some("end") => "end",
        Some("right") => "right",
        Some("left") => "left",
        _ if p.origin.is_some() => "start",
        _ => "",
    };
    if !align.is_empty() {
        s.push(format!("align:{align}"));
    }
    s.join(" ")
}

/// The cues as WebVTT: a `STYLE` block of one class per distinct style,
/// then per cue one WebVTT cue per paragraph (same times, each at its
/// region), or the plain lines of a cue without paragraphs.
#[must_use]
pub fn write_vtt(cues: &[Cue]) -> String {
    // Distinct styles → class names, in first-use order.
    let mut classes: Vec<(String, String)> = Vec::new();
    let mut class_of = |s: &Style| -> String {
        let decl = css(s);
        if decl.is_empty() {
            return String::new();
        }
        if let Some((name, _)) = classes.iter().find(|(_, d)| *d == decl) {
            return name.clone();
        }
        let name = format!("s{}", classes.len() + 1);
        classes.push((name.clone(), decl));
        name
    };
    let mut body = String::new();
    for c in cues {
        let stamps = format!(
            "{} --> {}",
            crate::stamp(c.start, '.'),
            crate::stamp(c.end, '.')
        );
        if c.paragraphs.is_empty() {
            let text = c
                .lines
                .iter()
                .map(|l| escape(l))
                .collect::<Vec<_>>()
                .join("\n");
            let _ = write!(body, "{stamps}\n{text}\n\n");
            continue;
        }
        for p in &c.paragraphs {
            let set = settings(p);
            let mut text = String::new();
            // Adjacent text of one class is one span (styles WebVTT
            // cannot tell apart — ARIB's half-width size — merge).
            let mut open: Option<String> = None;
            let close = |text: &mut String, open: &mut Option<String>| {
                if open.take().is_some_and(|c| !c.is_empty()) {
                    text.push_str("</c>");
                }
            };
            for r in &p.runs {
                match r {
                    Run::Break => {
                        close(&mut text, &mut open);
                        text.push('\n');
                    }
                    Run::Text(t, s) => {
                        let class = class_of(s);
                        if open.as_ref() != Some(&class) {
                            close(&mut text, &mut open);
                            if !class.is_empty() {
                                let _ = write!(text, "<c.{class}>");
                            }
                            open = Some(class);
                        }
                        text.push_str(&escape(t));
                    }
                    Run::Ruby(base, reading, s) => {
                        close(&mut text, &mut open);
                        let class = class_of(s);
                        let ruby =
                            format!("<ruby>{}<rt>{}</rt></ruby>", escape(base), escape(reading));
                        if class.is_empty() {
                            text.push_str(&ruby);
                        } else {
                            let _ = write!(text, "<c.{class}>{ruby}</c>");
                        }
                    }
                }
            }
            close(&mut text, &mut open);
            // A cue's text has no blank line (it would end the cue).
            let text = text
                .split('\n')
                .map(|l| if l.trim().is_empty() { "\u{3000}" } else { l })
                .collect::<Vec<_>>()
                .join("\n");
            if set.is_empty() {
                let _ = write!(body, "{stamps}\n{text}\n\n");
            } else {
                let _ = write!(body, "{stamps} {set}\n{text}\n\n");
            }
        }
    }
    let mut out = String::from("WEBVTT\n\n");
    if !classes.is_empty() {
        out.push_str("STYLE\n");
        for (name, decl) in &classes {
            let _ = writeln!(out, "::cue(.{name}) {{ {decl} }}");
        }
        out.push('\n');
    }
    out.push_str(&body);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A BS17 document, as broadcast.
    const BS17: &str = r##"<?xml version="1.0" encoding="utf-8"?><tt xmlns="http://www.w3.org/ns/ttml" xmlns:tts="http://www.w3.org/ns/ttml#styling" xmlns:arib-tt="http://www.arib.or.jp/ns/arib-tt" xml:lang="ja"><head><styling><style xml:id="writingMode1" tts:writingMode="lrtb"/><style xml:id="fontFamily1" tts:fontFamily="丸ゴシック" tts:fontStyle="normal" tts:fontWeight="normal"/><style xml:id="normalSize1" tts:fontSize="144px 144px" tts:lineHeight="240px" arib-tt:letter-spacing="16px"/><style xml:id="foreColor1" tts:color="#ffff00ff"/><style xml:id="backColor1" tts:backgroundColor="#00000080"/><style xml:id="middleSize1" tts:fontSize="72px 144px" tts:lineHeight="240px" arib-tt:letter-spacing="8px"/></styling><layout><region xml:id="r800" tts:origin="792px 1556px" tts:extent="960px 240px"/><region xml:id="r810" tts:origin="872px 1796px" tts:extent="2160px 240px"/></layout></head><body><div begin="00:13:42.334" end="00:13:47.334"><p xml:id="P760-1" region="r800"><span style="writingMode1 fontFamily1 normalSize1 foreColor1 backColor1">子どもたちに</span></p><p xml:id="P760-2" region="r810"><span style="writingMode1 fontFamily1 normalSize1 foreColor1 backColor1">庭で採れたリンゴをお裾分け</span><span style="writingMode1 fontFamily1 middleSize1 foreColor1 backColor1">。</span></p></div></body></tt>"##;

    /// A BS11 document: inline-referenced styles with an outline.
    const BS11: &str = r##"<tt xml:lang="ja" xmlns="http://www.w3.org/ns/ttml" xmlns:tts="http://www.w3.org/ns/ttml#styling" xmlns:arib-tt="http://www.arib.or.jp/ns/arib-tt"><head><styling><style xml:id="s1" tts:fontFamily="丸ゴシック" tts:fontSize="120px 120px" tts:color="#FFFFFFFF" tts:backgroundColor="#00000080" tts:lineHeight="200px" tts:textOutline="#000000FF 4px 0px" arib-tt:letter-spacing="12px"/><style xml:id="s2" tts:fontFamily="丸ゴシック" tts:fontSize="60px 120px" tts:color="#FFFFFFFF" tts:backgroundColor="#00000080" tts:lineHeight="200px"/></styling><layout><region xml:id="r1" tts:origin="968px 1516px" tts:extent="1836px 200px"/></layout></head><body><div begin="00:42:13.854"><p region="r1"><span style="s1">中には</span><span style="s2">、</span></p></div></body></tt>"##;

    #[test]
    fn bs17_paragraphs_keep_regions_and_styles() {
        let d = crate::parse(BS17.as_bytes()).unwrap();
        let ps = &d.blocks[0].paragraphs;
        assert_eq!(ps.len(), 2);
        let (x, y) = ps[0].origin.unwrap();
        assert!(
            (x - 20.625).abs() < 1e-9 && (y - 72.037).abs() < 1e-3,
            "{x} {y}"
        );
        let (w, _) = ps[1].extent.unwrap();
        assert!((w - 56.25).abs() < 1e-9);
        let Run::Text(t, s) = &ps[1].runs[0] else {
            panic!()
        };
        assert_eq!(t, "庭で採れたリンゴをお裾分け");
        assert_eq!(s.color, Some([255, 255, 0, 255]));
        assert_eq!(s.background, Some([0, 0, 0, 128]));
        assert_eq!(s.font_family.as_deref(), Some("丸ゴシック"));
        let (fw, fh) = s.font_size.unwrap();
        assert!((fw - 6.6667).abs() < 1e-3 && (fh - 6.6667).abs() < 1e-3);
        // The half-width full stop: its own run, half width, full height.
        let Run::Text(t, s) = &ps[1].runs[1] else {
            panic!()
        };
        assert_eq!(t, "。");
        assert!((s.font_size.unwrap().0 - 3.3333).abs() < 1e-3);
        // The plain lines are unchanged.
        assert_eq!(
            d.blocks[0].lines,
            vec!["子どもたちに", "庭で採れたリンゴをお裾分け。"]
        );
    }

    #[test]
    fn vtt_of_bs17_places_each_paragraph() {
        let d = crate::parse(BS17.as_bytes()).unwrap();
        let mut t = crate::Timeline::new();
        t.push(&d, |ms| Some(ms - 822_334 + 1_000), None);
        let vtt = write_vtt(&t.finish(None));
        let expected = "WEBVTT\n\nSTYLE\n\
::cue(.s1) { color: #ffff00; background-color: rgba(0,0,0,0.502); font-family: \"丸ゴシック\", sans-serif; font-size: 6.667vh; line-height: 11.111vh; font-weight: normal; font-style: normal }\n\n\
00:00:01.000 --> 00:00:06.000 line:72.037%,start position:20.625%,line-left size:25% align:start\n\
<c.s1>子どもたちに</c>\n\n\
00:00:01.000 --> 00:00:06.000 line:83.148%,start position:22.708%,line-left size:56.25% align:start\n\
<c.s1>庭で採れたリンゴをお裾分け。</c>\n\n";
        assert_eq!(vtt, expected);
    }

    #[test]
    fn vtt_of_bs11_has_the_outline_as_shadows() {
        let d = crate::parse(BS11.as_bytes()).unwrap();
        let mut t = crate::Timeline::new();
        t.push(&d, |ms| Some(ms - 2_533_854), None);
        let vtt = write_vtt(&t.finish(Some(3_000)));
        assert!(vtt.contains("::cue(.s1) { color: #ffffff; background-color: rgba(0,0,0,0.502); text-shadow: 0.185vh 0 #000000, -0.185vh 0 #000000, 0 0.185vh #000000, 0 -0.185vh #000000, 0.185vh 0.185vh #000000, -0.185vh -0.185vh #000000, 0.185vh -0.185vh #000000, -0.185vh 0.185vh #000000;"), "{vtt}");
        assert!(vtt.contains("<c.s1>中には</c><c.s2>、</c>"), "{vtt}");
        assert!(
            vtt.contains("line:70.185%,start position:25.208%,line-left size:47.812% align:start"),
            "{vtt}"
        );
    }

    #[test]
    fn style_references_chain_and_inline_wins() {
        let d = crate::parse(
            r##"<tt xmlns="http://www.w3.org/ns/ttml" xmlns:tts="http://www.w3.org/ns/ttml#styling" tts:extent="1920px 1080px"><head><styling><style xml:id="a" tts:color="red"/><style xml:id="b" style="a" tts:fontSize="54px"/></styling></head><body><div begin="0s" end="1s"><p tts:textAlign="center"><span style="b" tts:color="#00ff00">x</span><br/>y<ruby>漢<rt>かん</rt></ruby></p></div></body></tt>"##
                .as_bytes(),
        )
        .unwrap();
        let p = &d.blocks[0].paragraphs[0];
        let Run::Text(_, s) = &p.runs[0] else {
            panic!()
        };
        assert_eq!(s.color, Some([0, 255, 0, 255]), "inline over the reference");
        assert_eq!(s.font_size, Some((5.0, 5.0)), "54 px of 1080: 5 vh");
        assert_eq!(p.runs[1], Run::Break);
        assert!(matches!(&p.runs[3], Run::Ruby(b, r, _) if b == "漢" && r == "かん"));
        let mut t = crate::Timeline::new();
        t.push(&d, Some, None);
        let vtt = write_vtt(&t.finish(None));
        assert!(vtt.contains("00:00:00.000 --> 00:00:01.000 align:center\n<c.s1>x</c>\ny<ruby>漢<rt>かん</rt></ruby>\n"), "{vtt}");
    }

    #[test]
    fn colours() {
        assert_eq!(parse_color("#00000080"), Some([0, 0, 0, 128]));
        assert_eq!(parse_color("#FFFFFF"), Some([255, 255, 255, 255]));
        assert_eq!(parse_color("rgba(1,2,3,4)"), Some([1, 2, 3, 4]));
        assert_eq!(parse_color("yellow"), Some([255, 255, 0, 255]));
        assert_eq!(parse_color("#12"), None);
        assert_eq!(css_color([0, 0, 0, 128]), "rgba(0,0,0,0.502)");
    }
}

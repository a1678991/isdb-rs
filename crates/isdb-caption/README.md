# isdb-caption

The captions of ISDB-S3 / advanced BS to SubRip and WebVTT. A caption
asset (`stpp`) carries one TTML document per MFU (ARIB STD-B60 §9, Table
9-1; `isdb_mmt::es::Subtitle` unwraps it); this crate reads the document
into timed lines and writes them as `.srt` / `.vtt`.

| Item | Contents |
|---|---|
| `xml` | the XML subset broadcast TTML uses (elements, attributes, text, predefined entities, character references; no DTD) — no dependency |
| `parse_time` | TTML time expressions: clock `hh:mm:ss.fff`, `hh:mm:ss:ff` (frames), offset `N(h\|m\|s\|ms\|f\|t)` |
| `parse` | a document to `Block`s with their `begin`/`end` (nested times resolved): plain `lines`, and `paragraphs` — each `<p>` as styled runs at its region |
| `style` | TTML style resolution (referential styles and their chains, inline `tts:*`, inheritance, regions on the root canvas) and the WebVTT writer |
| `Timeline` | documents to cues: an open cue (no `end`) ends at the next document, an untimed empty document clears at its arrival |
| `to_srt`, `to_vtt` | the writers |

`to_srt` (and a Matroska `S_TEXT/UTF8` track) is text: one line per `<p>`,
`<br/>` breaking a line, ruby readings dropped.

`to_vtt` keeps the document as WebVTT can hold it: each `<p>` a cue at its
region (`line`/`position`/`size` as percentages of the canvas —
`tts:extent` on `<tt>`, else ARIB-TT's 3840×2160), its spans as classes of
a `STYLE` block — colour, background, the outline (`tts:textOutline`, as
eight text shadows), font family, size and line height (in `vh`), weight,
style, underline — `<br/>` and ruby as they are, `tts:writingMode` `tb*`
as `vertical`. WebVTT has no letter spacing, no horizontal glyph scale
(ARIB's half-width `72px 144px`) and no free box placement, so those go.

Players differ in what they render of it. Browsers (`<video><track>`),
Safari / AVFoundation and the web players built on them draw the
positions and styles — headless Chromium draws BS17's two-region caption
in its two places, yellow on the broadcast's translucent box. ffmpeg's
WebVTT decoder (and so mpv, and anything that converts with ffmpeg)
skips `STYLE` blocks and cue settings (its `webvttdec.c` "@todo … cue
settings") and shows the text bottom-centred, as plain text.

## Timing on air

Two patterns on the BS recordings (2026-10-26):

- BS17 (NHK BSP4K): `begin` and `end` on each `<div>` — closed cues;
- BS11 (Shop Channel): `begin` alone; the next document replaces the
  cue, and a document `<div><p><span/></p></div>` with no time clears it.

The clock values (`00:42:13.854`) count from an origin the MPT names (ARIB
STD-B60 Table 9-7): on these recordings a `reference_start_time` that moves
with every event, so documents of different events share clock values and
one mapping does not hold across a recording. The caller resolves each
document's clock against its media (`isdb-remux` from
`isdb_mmt::es::SubtitleInfo`) and passes the mapping to `Timeline::push`.

`cargo run -p isdb-caption --example ttml2srt -- [--vtt] FILE.ttml...`
converts extracted documents on their own clock.

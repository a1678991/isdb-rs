//! SI-style strings without broadcast data.
//!
//! * `si_style_byte_sequences`: hand-built 8-unit code byte sequences in the
//!   style broadcast SI encoders use (locking shifts into GR, single shifts,
//!   re-designations of G0–G3 to the additional symbols set and back,
//!   character size changes, APR line breaks, JIS X 0201 katakana), each
//!   with the text it must decode to.  Codes: ARIB STD-B24 v6.4 Vol.1
//!   Part 2 Table 7-1 (invocations), 7-2 (designations), 7-3 (final bytes),
//!   7-4 (Kanji = JIS X 0208), 7-5 (alphanumeric), 7-6 (katakana), 7-7
//!   (hiragana), 7-9 (JIS X 0201 katakana), 7-10/7-11/7-19 (additional
//!   symbols / Kanji), 7-15 (APR, SP), 7-16 (SSZ/MSZ/NSZ).  The texts are
//!   fictional.
//! * `encoder_strings`: fictional SI texts (service / event names, event
//!   text with line breaks, extended event items) encoded by
//!   [`isdb_text::encode`].
//!
//! Every string must decode with zero issues, and re-encoding the decoded
//! text must decode to the same text (the assertion the old live-capture
//! fixture made).  `capture_si_strings` repeats this on the SI strings of a
//! real capture when `ISDB_RS_CAPTURE_TS` names one.

use isdb_text::{AribString, encode};

const ESC: u8 = 0x1B;
/// LS0 (Table 7-1): G0 → GL.
const LS0: u8 = 0x0F;
/// LS1 (Table 7-1): G1 → GL.
const LS1: u8 = 0x0E;
/// SS2 / SS3 (Table 7-1): G2 / G3 → GL for one character.
const SS2: u8 = 0x19;
const SS3: u8 = 0x1D;
/// APR (Table 7-15): active position return (line break).
const APR: u8 = 0x0D;
/// SSZ / MSZ / NSZ (Table 7-16): small / middle / normal size.
const SSZ: u8 = 0x88;
const MSZ: u8 = 0x89;
const NSZ: u8 = 0x8A;

/// (field, bytes, expected text).
fn si_style() -> Vec<(&'static str, Vec<u8>, &'static str)> {
    let gr = |codes: &[u8]| codes.iter().map(|c| c | 0x80).collect::<Vec<u8>>();
    let cat = |parts: &[&[u8]]| parts.concat();
    vec![
        (
            // LS3R (ESC 07/12): katakana (G3) into GR; Kanji in GL (G0).
            "service_name: LS3R katakana + Kanji",
            cat(&[
                &[ESC, 0x7C],
                &gr(&[0x35, 0x73, 0x57, 0x6B]), // サンプル (Table 7-6)
                &[0x4A, 0x7C, 0x41, 0x77],      // 放送 (Table 7-4)
            ]),
            "サンプル放送",
        ),
        (
            // G0 ← additional symbols (ESC 02/4 03/11), 90-54 [字], then
            // G0 ← Kanji again (ESC 02/4 04/2).
            "event_name: G0 additional symbol",
            cat(&[
                &[0x32, 0x4D, 0x36, 0x75, 0x37, 0x60, 0x3E, 0x6C], // 架空劇場
                &[ESC, 0x24, 0x3B, 0x7A, 0x56],                    // 90-54
                &[ESC, 0x24, 0x42],
            ]),
            "架空劇場\u{1F211}",
        ),
        (
            // LS1 alphanumeric at normal size (full-width), SP at normal
            // size (U+3000), katakana via LS3R in GR, MSZ: SP and
            // alphanumerics half-width, NSZ, LS0.
            "event_name: LS1 + MSZ/NSZ",
            cat(&[
                &[LS1, 0x54, 0x45, 0x53, 0x54, 0x20], // ＴＥＳＴ
                &[ESC, 0x7C],
                &gr(&[0x4B, 0x65, 0x79, 0x39]), // ニュース (ー = 07/9)
                &[MSZ, 0x20, 0x37, 0x3A, 0x30, 0x30, NSZ, LS0],
            ]),
            "ＴＥＳＴ\u{3000}ニュース 7:00",
        ),
        (
            // Hiragana in GR (LS2R is the SI initial state), APR.
            "text: hiragana GR + APR",
            cat(&[
                &[0x48, 0x56, 0x41, 0x48],            // 番組
                &gr(&[0x4E, 0x2A, 0x37, 0x69, 0x3B]), // のおしらせ
                &[APR],
                &gr(&[0x4F, 0x38, 0x5E, 0x6A, 0x47, 0x39]), // はじまりです
            ]),
            "番組のおしらせ\nはじまりです",
        ),
        (
            // SS3 / SS2 single shifts of katakana / hiragana into GL.
            "text: SS2/SS3",
            cat(&[
                &[0x3B, 0x6E, 0x38, 0x33], // 試験
                &[SS3, 0x46, SS3, 0x39, SS3, 0x48, SS2, 0x4E],
            ]),
            "試験テストの",
        ),
        (
            // G2 ← additional symbols (ESC 02/4 02/10 03/11) + SS2 90-54,
            // G3 ← additional symbols (ESC 02/4 02/11 03/11) + SS3 90-48
            // [HV]; then G2 ← hiragana (ESC 02/10 03/0), G3 ← katakana
            // (ESC 02/11 03/1).
            "event_name: G2/G3 additional symbols via SS2/SS3",
            cat(&[
                &[0x48, 0x56, 0x41, 0x48], // 番組
                &[ESC, 0x24, 0x2A, 0x3B, SS2, 0x7A, 0x56],
                &[ESC, 0x24, 0x2B, 0x3B, SS3, 0x7A, 0x50],
                &[ESC, 0x2A, 0x30, ESC, 0x2B, 0x31],
                &gr(&[0x4E]), // の (GR still invokes G2)
            ]),
            "番組\u{1F211}\u{1F14A}の",
        ),
        (
            // G1 ← additional symbols (ESC 02/4 02/9 03/11), LS1R (ESC
            // 07/14) into GR: 90-57 [S]; G1 ← alphanumeric (ESC 02/9
            // 04/10), LS2R (ESC 07/13) back to hiragana.
            "event_name: G1 additional symbols via LS1R",
            cat(&[
                &[0x45, 0x37, 0x35, 0x24], // 天気
                &[ESC, 0x24, 0x29, 0x3B, ESC, 0x7E],
                &gr(&[0x7A, 0x59]),
                &[ESC, 0x29, 0x4A, ESC, 0x7D],
                &gr(&[0x4E]), // の
            ]),
            "天気\u{1F142}の",
        ),
        (
            // G1 ← JIS X 0201 katakana (ESC 02/9 04/9), LS1, LS0, G1 ←
            // alphanumeric.
            "ts_name: JIS X 0201 katakana",
            cat(&[
                &[ESC, 0x29, 0x49, LS1, 0x43, 0x3D, 0x44, LS0],
                &[ESC, 0x29, 0x4A],
                &[0x36, 0x49], // 局
            ]),
            "ﾃｽﾄ局",
        ),
        (
            // LS2 (ESC 06/14) / LS3 (ESC 06/15) into GL, LS0 back.
            "text: LS2/LS3 into GL",
            cat(&[
                &[ESC, 0x6E, 0x2B, 0x4E],       // かの
                &[ESC, 0x6F, 0x46, 0x39, 0x48], // テスト
                &[LS0, 0x42, 0x68, 0x32, 0x73], // 第回
            ]),
            "かのテスト第回",
        ),
        (
            // SSZ: alphanumerics in a small-size field map to ASCII.
            "audio text: SSZ",
            cat(&[
                &[SSZ, LS1, 0x41, 0x42, 0x43, LS0, NSZ],
                &[0x32, 0x72, 0x40, 0x62], // 解説
            ]),
            "ABC解説",
        ),
        (
            // Additional Kanji 85-1 (Table 7-11, U+3402) through G0.
            "item: additional Kanji row 85",
            cat(&[
                &[ESC, 0x24, 0x3B, 0x75, 0x21, ESC, 0x24, 0x42],
                &[0x44, 0x2E], // 町
            ]),
            "\u{3402}町",
        ),
    ]
}

fn assert_round_trip(label: &str, bytes: &[u8]) -> String {
    let d = AribString(bytes.to_vec()).decode();
    assert!(d.issues.is_empty(), "{label}: issues {:?}", d.issues);
    let e = encode(&d.text).unwrap_or_else(|e| panic!("{label}: encode error {e}"));
    let back = e.decode();
    assert!(
        back.issues.is_empty(),
        "{label}: re-encode issues {:?}",
        back.issues
    );
    assert_eq!(back.text, d.text, "{label}: re-encode changes the text");
    d.text
}

#[test]
fn si_style_byte_sequences() {
    for (label, bytes, want) in si_style() {
        let text = assert_round_trip(label, &bytes);
        println!("{label}\n    {}", text.replace('\n', "⏎"));
        assert_eq!(text, want, "{label}");
    }
}

/// Fictional SI texts as they appear in SDT / NIT / EIT fields.
const SI_TEXTS: &[(&str, &str)] = &[
    ("network_name", "テストネット"),
    ("ts_name", "テストテレビ"),
    ("service_name", "テストテレビ１"),
    ("service_name", "テストワンセグ"),
    ("event_name", "サンプル番組　第１回「はじまり」"),
    ("event_name", "テスト・ニュース 7:00"),
    ("event_name", "Sample Movie Theater"),
    ("event_name", "架空劇場\u{1F211}\u{1F14A}"),
    (
        "text",
        "架空の町を旅する試験用の番組です。\n実在の番組とは関係ありません。",
    ),
    ("text", "ABC 123 ｱｲｳ ＡＢＣ　かなカナ漢字 ★記号"),
    ("item_description", "出演者"),
    ("item", "テスト太郎、サンプル花子、ダミー次郎"),
    ("audio text", "主音声"),
    ("audio text", "副音声（解説）"),
    ("audio text", "ステレオ"),
    ("logo", "ﾃｽﾄ"),
];

#[test]
fn encoder_strings() {
    let mut all = Vec::new();
    for (field, text) in SI_TEXTS {
        let bytes = encode(text).unwrap_or_else(|e| panic!("{field} {text:?}: {e}"));
        let back = assert_round_trip(field, &bytes.0);
        assert_eq!(&back, text, "{field}");
        all.extend_from_slice(&bytes.0);
    }
    // The encoder output exercises locking shifts, character size changes,
    // APR and the additional symbols designation.
    let has = |seq: &[u8]| all.windows(seq.len()).any(|w| w == seq);
    for (what, seq) in [
        ("LS1", &[LS1][..]),
        ("LS0", &[LS0]),
        ("MSZ", &[MSZ]),
        ("NSZ", &[NSZ]),
        ("APR", &[APR]),
        ("LS3R", &[ESC, 0x7C]),
    ] {
        assert!(has(seq), "encoder output has no {what}");
    }
    // ESC 02/4 [I] 03/11: a 2-byte designation of the additional symbols set.
    assert!(
        all.windows(3).any(|w| w == [ESC, 0x24, 0x3B])
            || all
                .windows(4)
                .any(|w| w[0] == ESC && w[1] == 0x24 && w[3] == 0x3B),
        "encoder output has no additional symbols designation"
    );
}

// ---- independent extraction from a capture (opt-in) ----

/// CRC_32 of ITU-T H.222.0 Annex A (poly 0x04C11DB7, init all ones, no
/// reflection); a section including its CRC_32 field yields 0.
fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc ^= (b as u32) << 24;
        for _ in 0..8 {
            crc = if crc & 0x8000_0000 != 0 {
                (crc << 1) ^ 0x04C1_1DB7
            } else {
                crc << 1
            };
        }
    }
    crc
}

/// Sections of `pids`, reassembled per H.222.0 §2.4.3.2 (TS packet header:
/// PID, payload_unit_start_indicator, adaptation_field_control,
/// continuity_counter) and §2.4.4.2 (pointer_field), CRC-checked.
fn sections(ts: &[u8], pids: &[u16]) -> Vec<Vec<u8>> {
    use std::collections::HashMap;
    let mut buf: HashMap<u16, Option<Vec<u8>>> = HashMap::new();
    let mut cc: HashMap<u16, u8> = HashMap::new();
    let mut out = Vec::new();
    let drain = |b: &mut Option<Vec<u8>>, out: &mut Vec<Vec<u8>>| {
        while let Some(v) = b {
            if v.len() < 3 {
                return;
            }
            if v[0] == 0xFF {
                *b = None;
                return;
            }
            let len = (((v[1] & 0x0F) as usize) << 8 | v[2] as usize) + 3;
            if v.len() < len {
                return;
            }
            let sec: Vec<u8> = v.drain(..len).collect();
            if crc32(&sec) == 0 {
                out.push(sec);
            }
        }
    };
    let packets = (0..ts.len() / 188).map(|i| &ts[i * 188..(i + 1) * 188]);
    for pkt in packets.filter(|p| p[0] == 0x47 && p[1] & 0x80 == 0) {
        let pid = ((pkt[1] & 0x1F) as u16) << 8 | pkt[2] as u16;
        if !pids.contains(&pid) {
            continue;
        }
        let afc = (pkt[3] >> 4) & 3;
        let c = pkt[3] & 0x0F;
        let mut p = 4;
        if afc & 2 != 0 {
            p += 1 + pkt[4] as usize;
        }
        if afc & 1 == 0 || p >= 188 {
            continue;
        }
        let b = buf.entry(pid).or_insert(None);
        match cc.insert(pid, c) {
            Some(prev) if prev == c => continue, // duplicate packet
            Some(prev) if (prev + 1) & 0x0F != c => *b = None,
            _ => {}
        }
        let payload = &pkt[p..];
        if pkt[1] & 0x40 != 0 {
            let ptr = payload[0] as usize;
            if let Some(v) = b.as_mut() {
                v.extend_from_slice(&payload[1..(1 + ptr).min(payload.len())]);
                drain(b, &mut out);
            }
            *b = Some(payload[(1 + ptr).min(payload.len())..].to_vec());
        } else if let Some(v) = b.as_mut() {
            v.extend_from_slice(payload);
        }
        drain(b, &mut out);
    }
    out
}

fn descriptors(mut d: &[u8]) -> Vec<(u8, &[u8])> {
    let mut v = Vec::new();
    while d.len() >= 2 && d.len() >= 2 + d[1] as usize {
        v.push((d[0], &d[2..2 + d[1] as usize]));
        d = &d[2 + d[1] as usize..];
    }
    v
}

/// SI string fields (ARIB STD-B10 §6.2.11 network name, §6.2.13 service,
/// §6.2.15 short_event, §6.2.42 ts_information) of every section.
fn capture_strings(ts: &[u8]) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    // NIT, SDT and EIT PIDs incl. 0x26/0x27 for terrestrial EIT (ARIB STD-B10 §5.1.3 Table 5-1).
    for sec in sections(ts, &[0x10, 0x11, 0x12, 0x26, 0x27]) {
        let body = &sec[8..sec.len() - 4];
        let u12 = |b: &[u8], i: usize| ((b[i] & 0x0F) as usize) << 8 | b[i + 1] as usize;
        match sec[0] {
            0x40 | 0x41 => {
                let nl = u12(body, 0);
                for (tag, d) in descriptors(&body[2..2 + nl]) {
                    if tag == 0x40 {
                        out.push(d.to_vec());
                    }
                }
                let mut p = 4 + nl;
                while p + 6 <= body.len() {
                    let dl = u12(body, p + 4);
                    for (tag, d) in descriptors(&body[p + 6..(p + 6 + dl).min(body.len())]) {
                        if tag == 0xCD && d.len() >= 2 {
                            let n = (d[1] >> 2) as usize;
                            out.push(d[2..(2 + n).min(d.len())].to_vec());
                        }
                    }
                    p += 6 + dl;
                }
            }
            0x42 | 0x46 => {
                let mut p = 3;
                while p + 5 <= body.len() {
                    let dl = u12(body, p + 3);
                    for (tag, d) in descriptors(&body[p + 5..(p + 5 + dl).min(body.len())]) {
                        if tag == 0x48 {
                            let pl = d[1] as usize;
                            out.push(d[2..2 + pl].to_vec());
                            let sl = d[2 + pl] as usize;
                            out.push(d[3 + pl..3 + pl + sl].to_vec());
                        }
                    }
                    p += 5 + dl;
                }
            }
            0x4E..=0x6F => {
                let mut p = 6;
                while p + 12 <= body.len() {
                    let dl = u12(body, p + 10);
                    for (tag, d) in descriptors(&body[p + 12..(p + 12 + dl).min(body.len())]) {
                        if tag == 0x4D {
                            let nl = d[3] as usize;
                            out.push(d[4..4 + nl].to_vec());
                            let tl = d[4 + nl] as usize;
                            out.push(d[5 + nl..5 + nl + tl].to_vec());
                        }
                    }
                    p += 12 + dl;
                }
            }
            _ => {}
        }
    }
    out.retain(|s| !s.is_empty());
    out.sort();
    out.dedup();
    out
}

/// Environment variable naming the capture file.
const CAPTURE_ENV: &str = "ISDB_RS_CAPTURE_TS";

/// Runs only when `ISDB_RS_CAPTURE_TS` names a descrambled ISDB-T capture
/// (prints a `SKIP` line and passes otherwise): extracts the SI strings
/// with its own section parser (no `isdb-ts`), and decodes and round-trips
/// all of them.
#[test]
fn capture_si_strings() {
    let Some(path) = std::env::var_os(CAPTURE_ENV) else {
        eprintln!("SKIP: {CAPTURE_ENV} not set (path of a descrambled ISDB-T capture)");
        return;
    };
    let ts = match std::fs::read(&path) {
        Ok(ts) => ts,
        Err(e) => {
            eprintln!("SKIP: {CAPTURE_ENV}={}: {e}", path.to_string_lossy());
            return;
        }
    };
    let strings = capture_strings(&ts);
    println!("{} distinct SI strings in the capture", strings.len());
    assert!(!strings.is_empty(), "no SI string in the capture");
    for s in &strings {
        let d = AribString(s.clone()).decode();
        assert!(d.issues.is_empty(), "{s:02x?}: {:?}", d.issues);
        let back = encode(&d.text).expect("encodable").decode();
        assert_eq!(back.text, d.text);
    }
}

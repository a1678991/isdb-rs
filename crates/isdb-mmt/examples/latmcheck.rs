//! Take every AudioMuxElement of a LOAS file (`.latm` from `isdb tlv
//! extract` and the like) apart with `es::latm_frame`, and write
//! the raw AAC frames as ADTS so ffmpeg can check them (development tool).
//!
//! cargo run --release -p isdb-mmt --example latmcheck -- IN.latm [OUT.aac]

#![allow(
    clippy::cast_possible_truncation,
    reason = "ADTS fields of a frame under 8 KiB, masked by their widths"
)]

use isdb_mmt::es;

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("usage: latmcheck IN.latm [OUT.aac]");
    let out = args.next();
    let d = std::fs::read(&path).unwrap();
    let (mut n, mut bad, mut config) = (0usize, 0usize, None::<Vec<u8>>);
    let mut adts = Vec::new();
    let mut i = 0;
    while i + 3 <= d.len() {
        if d[i] != 0x56 || d[i + 1] & 0xe0 != 0xe0 {
            i += 1;
            continue;
        }
        let len = usize::from(d[i + 1] & 0x1f) << 8 | usize::from(d[i + 2]);
        let Some(e) = d.get(i + 3..i + 3 + len) else {
            break;
        };
        i += 3 + len;
        n += 1;
        let Some(f) = es::latm_frame(e) else {
            bad += 1;
            continue;
        };
        if let Some(c) = f.config {
            config = Some(c);
        }
        let Some(c) = &config else { continue };
        // ADTS: profile = object type - 1, frequency index, channels.
        let object = c[0] >> 3;
        let freq = (c[0] & 7) << 1 | c[1] >> 7;
        let ch = (c[1] >> 3) & 0xf;
        let fl = f.payload.len() + 7;
        adts.extend([
            0xff,
            0xf1,
            (object - 1) << 6 | freq << 2 | ch >> 2,
            (ch & 3) << 6 | (fl >> 11) as u8,
            (fl >> 3) as u8,
            ((fl & 7) as u8) << 5 | 0x1f,
            0xfc,
        ]);
        adts.extend_from_slice(&f.payload);
    }
    println!("{path}: {n} element(s), {bad} not taken apart, config {config:02x?}");
    if let Some(o) = out {
        std::fs::write(o, adts).unwrap();
    }
}

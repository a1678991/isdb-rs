//! The whole `isdb mux` output is pinned: the multiplex-frame TS
//! (`--out-ts`, every slot of every OFDM frame in slot order, nulls
//! included) of the shipped `bench-minimal.toml` and `bench.toml` (media
//! streams turned into `declared` ones, fixed clock), 96 OFDM frames
//! (≈ 22 s: every table, PCR and the EIT/TOT clock have run), must keep the
//! SHA-256 below. It guards refactors of the multiplexer's timeline
//! (docs/plans/isdbs-next/media-and-margin.md §3.5: the `Schedule`
//! refactor keeps the ISDB-T output byte-identical).
//!
//! With media (`ISDB_RS_MEDIA_DIR`, or `crates/isdbt-mux/examples/media`
//! holding `video.m2v` and `audio.m2a`), `bench.toml` with its real media
//! is muxed as well. Its output depends on the media files, so it is pinned
//! per input: [`PINNED_MEDIA`] maps the SHA-256 of the two inputs to the
//! output's; for inputs not listed the test prints both hashes (record them
//! before a refactor, compare after) and passes. Without media it prints
//! `SKIP`.

use std::path::{Path, PathBuf};
use std::process::Command;

const ISDB: &str = env!("CARGO_BIN_EXE_isdb");
/// OFDM frames muxed (≥ 90: the BIT's 20 s interval is covered).
const FRAMES: &str = "96";

/// (config, SHA-256 of the `--out-ts` output) with declared streams.
const PINNED: [(&str, &str); 2] = [
    (
        "bench-minimal",
        "7d3f18516548b6f6891d395bafafe19a9b054306cd58b83d15d74b7fe1a1d5ac",
    ),
    (
        "bench",
        "7e8f68890b71cdd06701209ac42d1efec7874f4c7128f3ed7b82060296ccc21a",
    ),
];

/// (SHA-256 of video.m2v, of audio.m2a, of the `bench.toml` output with
/// them): the media the bench uses (an MPEG-2 video ES and an MPEG-4 ADTS
/// without crc_check); other media print their hashes instead.
const PINNED_MEDIA: [(&str, &str, &str); 1] = [(
    "52ea385b25fd1307bfef4d9876d7e84c4e340b138329cf78f38bbcbeb9223040",
    "9ffc4bffd61a3d7342b30060b0adbededde0943680d985f4a065a376674593ed",
    "62c4f902c892655fa4f4f3a344a52bdc50bbdf3e9b9062d6c978b5be30b4896a",
)];

fn tmp(name: &str) -> PathBuf {
    let d = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("isdb-cli-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn example(name: &str) -> String {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../isdbt-mux/examples")
        .join(format!("{name}.toml"));
    std::fs::read_to_string(p).unwrap()
}

/// `text` with the clock fixed (TOT/TDT/EIT then do not depend on now).
fn fixed_clock(text: &str) -> String {
    assert!(text.contains("source = \"system\""));
    text.replace(
        "source = \"system\"",
        "start = \"2026-10-08T12:00:00\"\nsource = \"fixed\"",
    )
}

/// `text` with its media streams turned into `declared` ones (PSI/SI/PCR
/// only), as `isdbt-mux/tests/si_fixtures.rs` does.
fn declared(text: &str) -> String {
    let out: Vec<String> = text
        .lines()
        .filter(|l| {
            let t = l.trim_start();
            !t.starts_with("source = \"media/") && !t.starts_with("frames_per_pes")
        })
        .map(|l| {
            l.replace(
                "kind = \"mpeg2-video\"",
                "kind = \"declared\"\n  stream_type = 0x02",
            )
            .replace(
                "kind = \"aac-adts\"",
                "kind = \"declared\"\n  stream_type = 0x0F",
            )
        })
        .collect();
    out.join("\n")
}

/// SHA-256 (FIPS 180-4) of `data`, as lowercase hex.
fn sha256(data: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let mut msg = data.to_vec();
    let bits = (data.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bits.to_be_bytes());
    for block in msg.as_chunks::<64>().0 {
        let mut w = [0u32; 64];
        for (i, c) in block.as_chunks::<4>().0.iter().enumerate() {
            w[i] = u32::from_be_bytes(*c);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let t1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (x, y) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) {
            *x = x.wrapping_add(y);
        }
    }
    h.iter().map(|x| format!("{x:08x}")).collect()
}

#[test]
fn sha256_known_answers() {
    // FIPS 180-4 examples (NIST CSRC "SHA256.pdf"): "abc", the empty string
    // and the 448-bit two-block message.
    assert_eq!(
        sha256(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_eq!(
        sha256(b""),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    assert_eq!(
        sha256(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
        "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
    );
}

/// `isdb mux -c <text> --frames FRAMES --out-ts … <extra>`; the SHA-256
/// of the output.
fn mux_hash(label: &str, text: &str, extra: &[&str]) -> String {
    let d = tmp(&format!("mux-fixture-{label}"));
    let cfg = d.join(format!("{label}.toml"));
    std::fs::write(&cfg, text).unwrap();
    let out = d.join("full.ts");
    let o = Command::new(ISDB)
        .args([
            "mux",
            "-c",
            cfg.to_str().unwrap(),
            "--frames",
            FRAMES,
            "--out-ts",
            out.to_str().unwrap(),
        ])
        .args(extra)
        .output()
        .expect("run isdb");
    assert!(
        o.status.success(),
        "isdb mux {label}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    let ts = std::fs::read(&out).unwrap();
    assert!(
        !ts.is_empty() && ts.len().is_multiple_of(188),
        "{label}: {} bytes",
        ts.len()
    );
    let h = sha256(&ts);
    eprintln!("{label}: {} packets, sha256 {h}", ts.len() / 188);
    std::fs::remove_dir_all(&d).unwrap();
    h
}

#[test]
fn mux_output_of_the_examples_is_unchanged() {
    let mut differ = Vec::new();
    for (label, want) in PINNED {
        let got = mux_hash(label, &declared(&fixed_clock(&example(label))), &[]);
        if got != want {
            differ.push(format!("{label}: sha256 {got}, pinned {want}"));
        }
    }
    assert!(
        differ.is_empty(),
        "the `isdb mux` output changed:\n{}",
        differ.join("\n")
    );
}

/// The media directory: `ISDB_RS_MEDIA_DIR`, else
/// `crates/isdbt-mux/examples/media`; `None` when neither holds both files.
fn media_dir() -> Option<PathBuf> {
    let dir = std::env::var_os("ISDB_RS_MEDIA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../isdbt-mux/examples/media")
        });
    ["video.m2v", "audio.m2a"]
        .iter()
        .all(|f| dir.join(f).is_file())
        .then_some(dir)
}

#[test]
fn mux_output_with_media_is_unchanged() {
    let Some(dir) = media_dir() else {
        eprintln!(
            "SKIP mux_output_with_media_is_unchanged: no video.m2v and audio.m2a in \
             ISDB_RS_MEDIA_DIR or crates/isdbt-mux/examples/media"
        );
        return;
    };
    let file = |f: &str| {
        let p = dir.join(f);
        (
            toml::Value::String(p.to_str().unwrap().to_string()).to_string(),
            sha256(&std::fs::read(&p).unwrap()),
        )
    };
    let ((video, video_sha), (audio, audio_sha)) = (file("video.m2v"), file("audio.m2a"));
    let text = fixed_clock(&example("bench"));
    assert!(text.contains("\"media/video.m2v\"") && text.contains("\"media/audio.m2a\""));
    let text = text
        .replace("\"media/video.m2v\"", &video)
        .replace("\"media/audio.m2a\"", &audio);
    // Own media is often an MPEG-4 ADTS without crc_check: send it as it
    // is (the multiplexing does not depend on the policy).
    let got = mux_hash("bench-media", &text, &["--adts-compliance", "warn"]);
    match PINNED_MEDIA
        .iter()
        .find(|(v, a, _)| *v == video_sha && *a == audio_sha)
    {
        Some((_, _, want)) => assert_eq!(&got, want, "the `isdb mux` output with media changed"),
        None => eprintln!(
            "NOTE mux_output_with_media_is_unchanged: no pinned output for these inputs \
             (video.m2v {video_sha}, audio.m2a {audio_sha}); output sha256 {got}"
        ),
    }
}

//! Tezuka's maia-sdr recorder, driven over its HTTP API (`pluto` backend,
//! `recorder=maia`).
//!
//! maia-sdr (in the Tezuka firmware for PlutoSDR-family boards) records the
//! AD936x RX samples by DMA into a 128 MiB buffer in the board's DDR, at
//! the rate and LO libiio programmed. Its HTTP server (port 80) is used
//! as observed with maia-httpd 0.6.0 (Tezuka v0.3.24):
//!
//! * `GET /api`: the whole state as JSON (`ddc.enabled`, `recorder.state`);
//! * `PATCH /api/recorder` with `{"mode":"IQ16bit","maximum_duration":S,
//!   "prepend_timestamp":false}`, then `{"state_change":"Start"}`; the
//!   state returns to `Stopped` when the duration or the buffer is used up;
//! * `GET /recording`: a tar of `NAME.sigmf-meta` and `NAME.sigmf-data`
//!   (`ci16_le`, 12-bit samples left-aligned in 16 bits).

use anyhow::{Context as _, Result, bail};
use serde_json::Value;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

/// Samples of the recorder buffer in 16-bit mode: 128 MiB / 4 bytes.
pub(crate) const MAX_SAMPLES: usize = 33_554_432;

/// One HTTP/1.1 request with `Connection: close`; the body of a 200
/// response.
fn http(host: &str, method: &str, path: &str, body: Option<&str>) -> Result<Vec<u8>> {
    let what = || format!("maia-sdr {method} http://{host}{path}");
    let mut s = TcpStream::connect((host, 80)).with_context(what)?;
    s.set_read_timeout(Some(Duration::from_secs(30)))?;
    s.set_write_timeout(Some(Duration::from_secs(30)))?;
    let mut req = format!("{method} {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n");
    if let Some(b) = body {
        req += &format!(
            "Content-Type: application/json\r\nContent-Length: {}\r\n",
            b.len()
        );
    }
    req += "\r\n";
    req += body.unwrap_or("");
    s.write_all(req.as_bytes()).with_context(what)?;
    let mut resp = Vec::new();
    s.read_to_end(&mut resp).with_context(what)?;
    let end = resp
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .with_context(|| format!("{}: no HTTP header", what()))?;
    let head = String::from_utf8_lossy(&resp[..end]).to_string();
    let status = head.lines().next().unwrap_or_default();
    if status.split_whitespace().nth(1) != Some("200") {
        bail!("{}: {status}", what());
    }
    let mut length = None;
    for l in head.lines().skip(1) {
        if let Some((k, v)) = l.split_once(':') {
            let k = k.trim().to_ascii_lowercase();
            if k == "transfer-encoding" {
                bail!("{}: {l} not supported", what());
            }
            if k == "content-length" {
                length = v.trim().parse::<usize>().ok();
            }
        }
    }
    let body = resp.split_off(end + 4);
    if let Some(n) = length
        && body.len() != n
    {
        bail!("{}: {} of {n} bytes received", what(), body.len());
    }
    Ok(body)
}

fn json(host: &str, method: &str, path: &str, body: Option<&str>) -> Result<Value> {
    let b = http(host, method, path, body)?;
    serde_json::from_slice(&b).with_context(|| format!("maia-sdr {path}: not JSON"))
}

/// The files of a ustar archive: (name, contents).
fn untar(t: &[u8]) -> Result<Vec<(String, &[u8])>> {
    let mut out = Vec::new();
    let mut i = 0;
    while i + 512 <= t.len() {
        let h = &t[i..i + 512];
        if h.iter().all(|&b| b == 0) {
            break;
        }
        let field = |r: std::ops::Range<usize>| {
            let f = &h[r];
            let n = f.iter().position(|&b| b == 0).unwrap_or(f.len());
            String::from_utf8_lossy(&f[..n]).trim().to_string()
        };
        let name = field(0..100);
        let size = usize::from_str_radix(&field(124..136), 8)
            .with_context(|| format!("tar entry {name:?}: bad size"))?;
        let start = i + 512;
        if start + size > t.len() {
            bail!("tar entry {name:?}: truncated");
        }
        if h[156] == b'0' || h[156] == 0 {
            out.push((name, &t[start..start + size]));
        }
        i = start + size.div_ceil(512) * 512;
    }
    Ok(out)
}

/// Record `samples` samples at the programmed `rate_hz` with the maia-sdr
/// recorder of `host`: interleaved components (12 bits left-aligned).
pub(crate) fn record(host: &str, rate_hz: f64, samples: usize) -> Result<Vec<i16>> {
    if samples > MAX_SAMPLES {
        bail!("maia-sdr recorder holds at most {MAX_SAMPLES} samples of 16 bits");
    }
    let api = json(host, "GET", "/api", None)?;
    if api["ddc"]["enabled"].as_bool() == Some(true) {
        bail!("maia-sdr DDC is enabled: the recorder would record its output; disable it first");
    }
    if api["recorder"]["state"].as_str() != Some("Stopped") {
        bail!(
            "maia-sdr recorder is {} (want Stopped)",
            api["recorder"]["state"]
        );
    }
    let seconds = samples as f64 / rate_hz;
    let settings = serde_json::json!({
        "mode": "IQ16bit",
        "prepend_timestamp": false,
        // A little longer than needed; the excess is cut off below.
        "maximum_duration": seconds * 1.01 + 0.001,
    })
    .to_string();
    json(host, "PATCH", "/api/recorder", Some(&settings))?;
    json(
        host,
        "PATCH",
        "/api/recorder",
        Some(r#"{"state_change":"Start"}"#),
    )?;
    let t = Instant::now();
    loop {
        std::thread::sleep(Duration::from_millis(50));
        let r = json(host, "GET", "/api/recorder", None)?;
        if r["state"].as_str() == Some("Stopped") {
            break;
        }
        if t.elapsed() > Duration::from_secs_f64(seconds + 10.0) {
            let _ = json(
                host,
                "PATCH",
                "/api/recorder",
                Some(r#"{"state_change":"Stop"}"#),
            );
            bail!("maia-sdr recorder did not stop ({})", r["state"]);
        }
    }
    let tar = http(host, "GET", "/recording", None)?;
    let files = untar(&tar)?;
    let get = |suffix: &str| {
        files
            .iter()
            .find(|(n, _)| n.ends_with(suffix))
            .map(|(_, d)| *d)
            .with_context(|| format!("maia-sdr recording has no *{suffix}"))
    };
    let meta: Value = serde_json::from_slice(get(".sigmf-meta")?)
        .context("maia-sdr recording metadata is not JSON")?;
    let g = &meta["global"];
    if g["core:datatype"].as_str() != Some("ci16_le") {
        bail!(
            "maia-sdr recording datatype {} (want ci16_le)",
            g["core:datatype"]
        );
    }
    let rec_rate = g["core:sample_rate"].as_f64().unwrap_or(0.0);
    if (rec_rate / rate_hz - 1.0).abs() > 10e-6 {
        bail!("maia-sdr recorded at {rec_rate} Hz, the device was set to {rate_hz:.2} Hz");
    }
    let data = get(".sigmf-data")?;
    if data.len() < samples * 4 {
        bail!(
            "maia-sdr recorded {} samples, {samples} requested",
            data.len() / 4
        );
    }
    Ok(data[..samples * 4]
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&b| i16::from_le_bytes(b))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn untar_reads_ustar_entries() {
        let mut t = Vec::new();
        let mut entry = |name: &str, data: &[u8], kind: u8| {
            let mut h = [0u8; 512];
            h[..name.len()].copy_from_slice(name.as_bytes());
            let size = format!("{:011o}\0", data.len());
            h[124..136].copy_from_slice(size.as_bytes());
            h[156] = kind;
            t.extend_from_slice(&h);
            t.extend_from_slice(data);
            t.resize(t.len().div_ceil(512) * 512, 0);
        };
        entry("recording/", b"", b'5');
        entry("recording/recording.sigmf-meta", b"{}", b'0');
        entry("recording/recording.sigmf-data", &[1, 2, 3, 4], b'0');
        t.extend_from_slice(&[0u8; 1024]);
        let f = untar(&t).unwrap();
        assert_eq!(
            f,
            vec![
                ("recording/recording.sigmf-meta".to_string(), &b"{}"[..]),
                (
                    "recording/recording.sigmf-data".to_string(),
                    &[1u8, 2, 3, 4][..]
                ),
            ]
        );
        assert!(untar(&t[..2050]).is_err());
    }
}

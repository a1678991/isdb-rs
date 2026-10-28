//! Tezuka's maia-sdr recorder, driven over its HTTP API (`pluto` backend,
//! `recorder=maia`).
//!
//! maia-sdr (in the Tezuka firmware for PlutoSDR-family boards) records the
//! AD936x RX1 samples by DMA into a buffer in the board's DDR, at the rate
//! and LO libiio programmed. The buffer is 128 MiB from 0x06000000: the
//! device tree reserves 256 MiB (`maia_sdr_recording`, and
//! `/sys/class/maia-sdr/maia-sdr-recording/device/recording_size` says
//! 0x10000000), but the gateware's DMA stops at the end address it was
//! built with, 0x0E000000 in maia-hdl's `maia_iio_lite` configuration,
//! which Tezuka v0.3.24 uses ("size only 0x8000000"). Every mode stopped
//! at exactly 128 MiB on a PlutoSky R2. Its HTTP server (port 80) is used
//! as observed with maia-httpd 0.6.0 (Tezuka v0.3.24):
//!
//! * `GET /api`: the whole state as JSON (`ddc.enabled`, `recorder.state`);
//! * `PATCH /api/recorder` with `{"mode":M,"maximum_duration":S,
//!   "prepend_timestamp":false}`, then `{"state_change":"Start"}`; the
//!   state returns to `Stopped` when the duration or the buffer is used up
//!   (`maximum_duration` is the setting last written, not a capacity);
//! * `GET /recording`: a tar of `NAME.sigmf-meta` and `NAME.sigmf-data`.
//!
//! The modes, as measured (BS1 at 40 Msps, against 16-bit captures of
//! the same signal):
//!
//! | `mode` | in DDR | `sigmf-data` | value of 1.0 |
//! |---|---|---|---|
//! | `IQ16bit` | 4 bytes/sample | `ci16_le`, 12 bits left-aligned (low 4 bits 0) | 32768 |
//! | `IQ12bit` | 3 bytes/sample | `ci16_le`, unpacked by maia-httpd, sign-extended | 2048 |
//! | `IQ8bit` | 2 bytes/sample | `ci8`, the 12-bit sample / 16 rounded down (mean −0.47 LSB) | 128 |

use crate::{CaptureFormat, RadioError, Result};
use serde_json::Value;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};
use tracing::debug;

/// The backend whose errors these are.
const BACKEND: &str = "pluto";

/// A reply of maia-sdr this module cannot use.
fn reply(what: impl Into<String>, detail: impl Into<String>) -> RadioError {
    RadioError::Reply {
        backend: BACKEND,
        what: what.into(),
        detail: detail.into(),
    }
}

/// Bytes of the recorder buffer (Tezuka v0.3.24: the `maia_iio_lite`
/// gateware's DMA range).
pub(crate) const BUFFER_BYTES: u64 = 128 << 20;

/// A recorder mode: 8, 12 or 16 bits per component.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Mode(pub(crate) u8);

impl Mode {
    /// The API name (`IQ16bit`).
    fn name(self) -> &'static str {
        match self.0 {
            8 => "IQ8bit",
            12 => "IQ12bit",
            _ => "IQ16bit",
        }
    }

    /// Bytes per sample in the DDR buffer.
    fn stored_bytes(self) -> u64 {
        match self.0 {
            8 => 2,
            12 => 3,
            _ => 4,
        }
    }

    /// The samples the buffer holds.
    pub(crate) fn max_samples(self) -> u64 {
        BUFFER_BYTES / self.stored_bytes()
    }

    /// The format of the downloaded samples.
    pub(crate) fn format(self) -> CaptureFormat {
        match self.0 {
            8 => CaptureFormat::Cs8,
            12 => CaptureFormat::Cs16 { full_scale: 2048.0 },
            _ => CaptureFormat::Cs16 {
                full_scale: 32768.0,
            },
        }
    }

    /// The SigMF datatype of the downloaded samples.
    fn datatype(self) -> &'static str {
        if self.0 == 8 { "ci8" } else { "ci16_le" }
    }

    /// How the samples were made, for the capture method.
    pub(crate) fn describe(self) -> &'static str {
        match self.0 {
            8 => "IQ8bit: 12-bit samples / 16 rounded down, mean -0.5 LSB",
            12 => "IQ12bit: 12-bit samples, sign-extended on download",
            _ => "IQ16bit: 12-bit samples left-aligned",
        }
    }
}

/// One HTTP/1.1 request with `Connection: close`; the body of a 200
/// response.
fn http(host: &str, method: &str, path: &str, body: Option<&str>) -> Result<Vec<u8>> {
    let what = || format!("maia-sdr {method} http://{host}{path}");
    let failed = |e: std::io::Error| RadioError::device(BACKEND, what(), e);
    let mut s = TcpStream::connect((host, 80)).map_err(failed)?;
    s.set_read_timeout(Some(Duration::from_secs(30)))
        .map_err(failed)?;
    s.set_write_timeout(Some(Duration::from_secs(30)))
        .map_err(failed)?;
    let mut req = format!("{method} {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n");
    if let Some(b) = body {
        req += &format!(
            "Content-Type: application/json\r\nContent-Length: {}\r\n",
            b.len()
        );
    }
    req += "\r\n";
    req += body.unwrap_or("");
    s.write_all(req.as_bytes()).map_err(failed)?;
    let mut resp = Vec::new();
    s.read_to_end(&mut resp).map_err(failed)?;
    let end = resp
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| reply(what(), "no HTTP header"))?;
    let head = String::from_utf8_lossy(&resp[..end]).to_string();
    let status = head.lines().next().unwrap_or_default();
    if status.split_whitespace().nth(1) != Some("200") {
        return Err(reply(what(), status));
    }
    let mut length = None;
    for l in head.lines().skip(1) {
        if let Some((k, v)) = l.split_once(':') {
            let k = k.trim().to_ascii_lowercase();
            if k == "transfer-encoding" {
                return Err(reply(what(), format!("{l} not supported")));
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
        return Err(reply(
            what(),
            format!("{} of {n} bytes received", body.len()),
        ));
    }
    debug!(
        method,
        path,
        host,
        bytes = body.len(),
        "maia-sdr {method} {path}: {status}, {} bytes",
        body.len()
    );
    Ok(body)
}

fn json(host: &str, method: &str, path: &str, body: Option<&str>) -> Result<Value> {
    let b = http(host, method, path, body)?;
    serde_json::from_slice(&b).map_err(|e| {
        RadioError::device(BACKEND, format!("maia-sdr {path}: read the JSON reply"), e)
    })
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
        let size = usize::from_str_radix(&field(124..136), 8).map_err(|_| {
            reply(
                "maia-sdr recording",
                format!("tar entry {name:?}: bad size"),
            )
        })?;
        let start = i + 512;
        if start + size > t.len() {
            return Err(reply(
                "maia-sdr recording",
                format!("tar entry {name:?}: truncated"),
            ));
        }
        if h[156] == b'0' || h[156] == 0 {
            out.push((name, &t[start..start + size]));
        }
        i = start + size.div_ceil(512) * 512;
    }
    Ok(out)
}

/// Record `samples` samples at the programmed `rate_hz` with the maia-sdr
/// recorder of `host` in `mode` and write their bytes ([`Mode::format`])
/// to `out`.
pub(crate) fn record(
    host: &str,
    rate_hz: f64,
    samples: u64,
    mode: Mode,
    out: &mut dyn Write,
) -> Result<()> {
    if samples > mode.max_samples() {
        return Err(RadioError::CaptureLength {
            backend: BACKEND,
            samples,
            limit: mode.max_samples(),
            why: format!(
                "the maia-sdr recorder buffer in {}, {} MiB",
                mode.name(),
                BUFFER_BYTES >> 20
            ),
        });
    }
    let api = json(host, "GET", "/api", None)?;
    if api["ddc"]["enabled"].as_bool() == Some(true) {
        return Err(reply(
            "maia-sdr recorder",
            "the DDC is enabled: the recorder would record its output; disable it first",
        ));
    }
    if api["recorder"]["state"].as_str() != Some("Stopped") {
        return Err(reply(
            "maia-sdr recorder",
            format!("state {} (want Stopped)", api["recorder"]["state"]),
        ));
    }
    let seconds = samples as f64 / rate_hz;
    let settings = serde_json::json!({
        "mode": mode.name(),
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
            return Err(reply(
                "maia-sdr recorder",
                format!("did not stop (state {})", r["state"]),
            ));
        }
    }
    let tar = http(host, "GET", "/recording", None)?;
    let files = untar(&tar)?;
    let get = |suffix: &str| {
        files
            .iter()
            .find(|(n, _)| n.ends_with(suffix))
            .map(|(_, d)| *d)
            .ok_or_else(|| reply("maia-sdr recording", format!("no *{suffix}")))
    };
    let meta: Value = serde_json::from_slice(get(".sigmf-meta")?)
        .map_err(|e| RadioError::device(BACKEND, "read the maia-sdr recording metadata", e))?;
    let g = &meta["global"];
    if g["core:datatype"].as_str() != Some(mode.datatype()) {
        return Err(reply(
            "maia-sdr recording",
            format!(
                "{} recording datatype {} (want {})",
                mode.name(),
                g["core:datatype"],
                mode.datatype()
            ),
        ));
    }
    let rec_rate = g["core:sample_rate"].as_f64().unwrap_or(0.0);
    if (rec_rate / rate_hz - 1.0).abs() > 10e-6 {
        return Err(reply(
            "maia-sdr recording",
            format!("recorded at {rec_rate} Hz, the device was set to {rate_hz:.2} Hz"),
        ));
    }
    let data = get(".sigmf-data")?;
    let bps = mode.format().bytes_per_sample();
    let wanted = usize::try_from(samples)
        .ok()
        .and_then(|n| n.checked_mul(bps))
        .and_then(|n| data.get(..n));
    let Some(wanted) = wanted else {
        return Err(reply(
            "maia-sdr recording",
            format!("{} samples recorded, {samples} requested", data.len() / bps),
        ));
    };
    out.write_all(wanted)
        .map_err(|e| RadioError::io("write the maia-sdr recording", e))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modes() {
        assert_eq!(Mode(16).max_samples(), 33_554_432);
        assert_eq!(Mode(12).max_samples(), 44_739_242);
        assert_eq!(Mode(8).max_samples(), 67_108_864);
        assert_eq!(Mode(8).format(), CaptureFormat::Cs8);
        assert_eq!(
            Mode(12).format(),
            CaptureFormat::Cs16 { full_scale: 2048.0 }
        );
        assert_eq!(
            Mode(16).format(),
            CaptureFormat::Cs16 {
                full_scale: 32768.0
            }
        );
        assert_eq!(
            (Mode(8).name(), Mode(12).name(), Mode(16).name()),
            ("IQ8bit", "IQ12bit", "IQ16bit")
        );
    }

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
        assert!(matches!(
            untar(&t[..2050]),
            Err(RadioError::Reply {
                backend: "pluto",
                ..
            })
        ));
    }
}

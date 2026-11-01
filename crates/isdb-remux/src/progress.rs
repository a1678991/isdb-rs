//! The progress of a run over its input, on stderr: bytes read, the
//! percentage and the time left when the input's size is known (a file),
//! the rate. On a terminal one line rewritten in place twice a second; to
//! a log (stderr not a terminal) a line every [`LOG_EVERY`].

use std::io::{IsTerminal, Read, Write};
use std::time::{Duration, Instant};

/// How often a terminal's line is redrawn.
pub const TTY_EVERY: Duration = Duration::from_millis(500);
/// How often a line goes to a log.
pub const LOG_EVERY: Duration = Duration::from_secs(10);

/// A reader that reports how far it has got.
pub struct Progress<R> {
    inner: R,
    /// The input's size when known.
    total: Option<u64>,
    read: u64,
    start: Instant,
    last: Instant,
    tty: bool,
    every: Duration,
    /// Off: a plain pass-through.
    enabled: bool,
    /// A line is on the terminal (to be ended at the end).
    drawn: bool,
}

impl<R: Read> Progress<R> {
    /// Report on `inner`, of `total` bytes when known (nothing when not
    /// `enabled`).
    pub fn new(inner: R, total: Option<u64>, enabled: bool) -> Self {
        let tty = std::io::stderr().is_terminal();
        let now = Instant::now();
        Self {
            inner,
            total: total.filter(|&t| t > 0),
            read: 0,
            start: now,
            last: now,
            tty,
            every: if tty { TTY_EVERY } else { LOG_EVERY },
            drawn: false,
            enabled,
        }
    }

    /// The status line.
    #[must_use]
    pub fn line(&self) -> String {
        line(self.read, self.total, self.start.elapsed())
    }

    fn report(&mut self) {
        let l = self.line();
        let mut e = std::io::stderr().lock();
        if self.tty {
            // \r, the line, clear to its end
            let _ = write!(e, "\r{l}\x1b[K");
            self.drawn = true;
        } else {
            let _ = writeln!(e, "{l}");
        }
        let _ = e.flush();
    }

    /// End the status line (a final one, then a newline on a terminal).
    pub fn finish(&mut self) {
        if self.enabled && self.tty {
            self.report();
            let _ = writeln!(std::io::stderr());
            self.drawn = false;
        }
    }
}

impl<R: Read> Read for Progress<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.read += n as u64;
        if self.enabled && self.last.elapsed() >= self.every {
            self.last = Instant::now();
            self.report();
        }
        Ok(n)
    }
}

impl<R> Drop for Progress<R> {
    fn drop(&mut self) {
        // A run that ends early (an error) leaves the line ended.
        if self.drawn {
            let _ = writeln!(std::io::stderr());
        }
    }
}

/// `1.23 GB / 9.03 GB (13.6 %), 152.4 MB/s, 52 s left` (the share and the
/// time left only with a known `total`).
#[must_use]
#[allow(clippy::cast_precision_loss, reason = "a display")]
pub fn line(read: u64, total: Option<u64>, elapsed: Duration) -> String {
    let t = elapsed.as_secs_f64();
    let rate = if t > 0.0 { read as f64 / t } else { 0.0 };
    let mut s = match total {
        Some(total) => format!(
            "{} / {} ({:.1} %)",
            size(read),
            size(total),
            read as f64 * 100.0 / total as f64
        ),
        None => size(read),
    };
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a non-negative byte rate"
    )]
    s.push_str(&format!(", {}/s", size(rate as u64)));
    if let Some(total) = total
        && rate > 0.0
        && read < total
    {
        s.push_str(&format!(
            ", {} left",
            duration((total - read) as f64 / rate)
        ));
    }
    s
}

#[allow(clippy::cast_precision_loss, reason = "a display")]
fn size(b: u64) -> String {
    let b = b as f64;
    if b >= 1e9 {
        format!("{:.2} GB", b / 1e9)
    } else if b >= 1e6 {
        format!("{:.1} MB", b / 1e6)
    } else if b >= 1e3 {
        format!("{:.0} kB", b / 1e3)
    } else {
        format!("{b:.0} B")
    }
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "a display of a positive duration"
)]
fn duration(secs: f64) -> String {
    let s = secs.round() as u64;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else if s >= 60 {
        format!("{}:{:02}", s / 60, s % 60)
    } else {
        format!("{s} s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines() {
        let d = Duration::from_secs(10);
        assert_eq!(
            line(1_230_000_000, Some(9_030_000_000), d),
            "1.23 GB / 9.03 GB (13.6 %), 123.0 MB/s, 1:03 left"
        );
        assert_eq!(line(5_000_000, None, d), "5.0 MB, 500 kB/s");
        assert_eq!(line(10, Some(10), d), "10 B / 10 B (100.0 %), 1 B/s");
        assert_eq!(duration(3725.0), "1:02:05");
    }

    #[test]
    fn reads_through() {
        let data = vec![7u8; 100_000];
        let mut p = Progress::new(&data[..], Some(100_000), false);
        let mut out = Vec::new();
        p.read_to_end(&mut out).unwrap();
        assert_eq!(out, data);
        assert_eq!(p.read, 100_000);
    }
}

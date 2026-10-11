//! Playback keys on the terminal: space pauses / resumes, ← / → seek back /
//! forward, ↑ / ↓ change the play rate.
//!
//! The terminal is put in non-canonical mode without echo for as long as
//! the [`Keys`] value lives (signals still work: Ctrl-C stops as before)
//! and restored when it is dropped.

use std::sync::mpsc::{Receiver, Sender, channel};

/// A playback command.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Command {
    /// Pause when playing, resume when paused.
    TogglePause,
    /// Jump this many seconds (negative: back).
    Seek(f64),
    /// Next faster play rate.
    Faster,
    /// Next slower play rate.
    Slower,
}

/// The keys, for the start-up hint; `seek_s` is the seek step.
pub fn help(seek_s: f64) -> String {
    format!("space pause/resume, ←/→ seek ±{seek_s} s, ↑/↓ play rate")
}

/// Command of a key sequence read from the terminal, if any. `seek_s` is
/// the seek step.
pub fn parse(bytes: &[u8], seek_s: f64) -> Option<Command> {
    match bytes {
        b" " => Some(Command::TogglePause),
        [0x1B, b'[' | b'O', k] => match k {
            b'A' => Some(Command::Faster),
            b'B' => Some(Command::Slower),
            b'C' => Some(Command::Seek(seek_s)),
            b'D' => Some(Command::Seek(-seek_s)),
            _ => None,
        },
        _ => None,
    }
}

/// Reads keys from the terminal on its own thread while it lives.
pub struct Keys {
    /// Commands, in key order (until taken).
    rx: Option<Receiver<Command>>,
    #[cfg(unix)]
    saved: Option<nix::sys::termios::Termios>,
}

impl Keys {
    /// Start reading keys when stdin is a terminal (None otherwise, or
    /// when its mode cannot be set).
    pub fn start(seek_s: f64) -> Option<Self> {
        use std::io::IsTerminal;
        if !std::io::stdin().is_terminal() {
            return None;
        }
        Self::start_unix(seek_s)
    }

    #[cfg(unix)]
    fn start_unix(seek_s: f64) -> Option<Self> {
        use nix::sys::termios::{
            LocalFlags, SetArg, SpecialCharacterIndices, tcgetattr, tcsetattr,
        };
        let stdin = std::io::stdin();
        let saved = tcgetattr(&stdin).ok()?;
        let mut raw = saved.clone();
        raw.local_flags
            .remove(LocalFlags::ICANON | LocalFlags::ECHO);
        raw.control_chars[SpecialCharacterIndices::VMIN as usize] = 1;
        raw.control_chars[SpecialCharacterIndices::VTIME as usize] = 0;
        tcsetattr(&stdin, SetArg::TCSANOW, &raw).ok()?;
        let (tx, rx) = channel();
        std::thread::Builder::new()
            .name("keys".into())
            .spawn(move || read_keys(&tx, seek_s))
            .ok()?;
        Some(Self {
            rx: Some(rx),
            saved: Some(saved),
        })
    }

    #[cfg(not(unix))]
    fn start_unix(_seek_s: f64) -> Option<Self> {
        None
    }
}

impl Keys {
    /// The commands (once): the terminal stays in key mode while `self`
    /// lives.
    pub fn take_rx(&mut self) -> Option<Receiver<Command>> {
        self.rx.take()
    }
}

impl Drop for Keys {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some(t) = self.saved.take() {
            use nix::sys::termios::{SetArg, tcsetattr};
            let _ = tcsetattr(std::io::stdin(), SetArg::TCSANOW, &t);
        }
    }
}

/// Read stdin byte by byte and send the commands of the key sequences.
fn read_keys(tx: &Sender<Command>, seek_s: f64) {
    use std::io::Read;
    let mut stdin = std::io::stdin().lock();
    let mut seq: Vec<u8> = Vec::new();
    let mut b = [0u8; 1];
    while let Ok(1) = stdin.read(&mut b) {
        // an escape sequence is ESC, '[' or 'O', then one final byte
        if b[0] == 0x1B {
            seq.clear();
        }
        seq.push(b[0]);
        let pending = seq[0] == 0x1B && seq.len() < 3;
        if pending {
            continue;
        }
        if let Some(c) = parse(&seq, seek_s)
            && tx.send(c).is_err()
        {
            return;
        }
        seq.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_map_to_commands() {
        assert_eq!(parse(b" ", 10.0), Some(Command::TogglePause));
        assert_eq!(parse(b"\x1b[C", 10.0), Some(Command::Seek(10.0)));
        assert_eq!(parse(b"\x1b[D", 10.0), Some(Command::Seek(-10.0)));
        assert_eq!(parse(b"\x1bOA", 10.0), Some(Command::Faster));
        assert_eq!(parse(b"\x1b[B", 10.0), Some(Command::Slower));
        assert_eq!(parse(b"x", 10.0), None);
        assert_eq!(parse(b"\x1b[Z", 10.0), None);
    }
}

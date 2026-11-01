//! The chain's input from a `Symbols` word stream: the blocks' symbols
//! and the losses between them, as [`super::Chain::push`] and
//! [`super::Chain::loss`] take them.

use isdb_frontend::format::{Event, Reader};
use std::io::Read;

/// One item of the input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Input {
    /// A whole block's symbols and the AGC gain of its header (Q6.10).
    Symbols(Vec<[i8; 2]>, u16),
    /// Symbols missing before the next block: the sequence gap × the
    /// block length (0 for a resynchronisation without a gap).
    Loss(u64),
}

/// Read the `Symbols` words of `r` and call `f` on every block and loss,
/// in order, until `f` returns `false` or the stream ends. The first
/// loss event (joining the stream mid-block) is not reported. Returns
/// the symbols delivered.
///
/// # Errors
///
/// A read error of `r`.
pub fn for_each(mut r: impl Read, mut f: impl FnMut(Input) -> bool) -> std::io::Result<u64> {
    let mut reader = Reader::new();
    let mut ev = Vec::new();
    let mut buf = vec![0u8; 1 << 20];
    let (mut started, mut n, mut block) = (false, 0u64, 0u64);
    let mut handle = |ev: &mut Vec<Event>, n: &mut u64| -> bool {
        for e in ev.drain(..) {
            let item = match e {
                Event::Block { symbols, header } => {
                    started = true;
                    block = symbols.len() as u64;
                    *n += block;
                    Input::Symbols(symbols, header.gain)
                }
                Event::Loss { blocks, .. } if started => Input::Loss(blocks * block),
                Event::Loss { .. } | Event::Truncated { .. } => continue,
            };
            if !f(item) {
                return false;
            }
        }
        true
    };
    loop {
        let k = r.read(&mut buf)?;
        if k == 0 {
            break;
        }
        reader.feed_bytes(&buf[..k], &mut ev);
        if !handle(&mut ev, &mut n) {
            return Ok(n);
        }
    }
    reader.finish(&mut ev);
    handle(&mut ev, &mut n);
    Ok(n)
}

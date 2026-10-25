//! Replay one cut of the `Symbols` loss test and print what the reader
//! delivers, block by block (development tool for the failing proptest
//! seed `bits = 5, cut_at = 208, cut_len = 282`).
//!
//! cargo run --release -p isdb-frontend --example formatcut -- [BITS CUT_AT CUT_LEN]

use isdb_frontend::format::{Event, HEADER_WORDS, Packer, Reader, TRAILER_WORDS};
use isdb_frontend::params::{Design, Params};

fn main() {
    let a: Vec<usize> = std::env::args()
        .skip(1)
        .map(|s| s.parse().unwrap())
        .collect();
    let (bits, cut_at, cut_len) = match a[..] {
        [b, c, l] => (b as u8, c, l),
        _ => (5, 208, 282),
    };
    let l = 256usize;
    let mut d = Design::new(40e6, 28.86e6, 0.35);
    d.bits = bits;
    d.block_log2 = 8;
    let p = Params::design(&d).unwrap();
    let syms: Vec<[i8; 2]> = (0..l * 8)
        .map(|i| [(i % 3) as i8 - 1, (i % 2) as i8])
        .collect();
    let mut pk = Packer::new(&p);
    let mut words = Vec::new();
    for &s in &syms {
        pk.push(s, 1024, 1 << 24, &mut words);
    }
    let block_words = HEADER_WORDS + l * 2 * usize::from(bits) / 16 + TRAILER_WORDS;
    let cut_at = cut_at.min(words.len() - 1);
    let cut_end = (cut_at + cut_len).min(words.len());
    println!(
        "{} words, {block_words} per block; cut words {cut_at}..{cut_end} (blocks {}..={})",
        words.len(),
        cut_at / block_words,
        (cut_end - 1) / block_words
    );
    let mut damaged: Vec<u16> = words[..cut_at].to_vec();
    damaged.extend_from_slice(&words[cut_end..]);
    let mut r = Reader::new();
    let mut ev = Vec::new();
    for chunk in damaged.chunks(97) {
        r.feed(chunk, &mut ev);
    }
    r.finish(&mut ev);
    for e in &ev {
        match e {
            Event::Block { header, symbols } => {
                let s = header.seq as usize;
                let ok = s < 8 && symbols[..] == syms[s * l..(s + 1) * l];
                println!(
                    "block seq {s}: {}",
                    if ok {
                        "symbols correct"
                    } else {
                        "SYMBOLS WRONG"
                    }
                );
            }
            Event::Loss { blocks, words } => {
                println!("loss: {blocks} blocks, {words} words skipped")
            }
            Event::Truncated { words } => println!("truncated: {words} words"),
        }
    }
}

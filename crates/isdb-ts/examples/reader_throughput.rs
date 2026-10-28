//! Throughput of the packet readers (H.222.0 §2.4.3 sync and framing) over
//! a synthetic in-memory stream, for before/after comparisons of the
//! per-packet path: `cargo run --release -p isdb-ts --example
//! reader_throughput [MiB]`.
#![allow(clippy::print_stdout)]

use isdb_ts::reader::{PacketFormat, SliceReader, StreamReader};
use std::time::Instant;

fn stream(bytes: usize, fmt: PacketFormat) -> Vec<u8> {
    let mut unit = vec![0xA5u8; fmt.size()];
    unit[fmt.sync_offset()] = 0x47;
    unit[fmt.sync_offset() + 1] = 0x1F;
    unit[fmt.sync_offset() + 2] = 0xFF;
    unit[fmt.sync_offset() + 3] = 0x10;
    unit.iter()
        .copied()
        .cycle()
        .take(bytes / fmt.size() * fmt.size())
        .collect()
}

fn main() {
    let mib: usize = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(256);
    for fmt in PacketFormat::ALL {
        let data = stream(mib << 20, fmt);
        let best = |f: &dyn Fn() -> u64| {
            (0..5)
                .map(|_| {
                    let t = Instant::now();
                    let n = f();
                    (t.elapsed().as_secs_f64(), n)
                })
                .fold((f64::MAX, 0), |a, b| if b.0 < a.0 { b } else { a })
        };
        let (ts, n) = best(&|| {
            SliceReader::new(&data)
                .map(|p| u64::from(p.packet[3]) + p.trailer.len() as u64)
                .sum::<u64>()
        });
        let (tr, m) = best(&|| {
            StreamReader::new(&data[..])
                .map(|p| {
                    let p = p.unwrap();
                    u64::from(p.packet()[3]) + p.trailer().len() as u64
                })
                .sum::<u64>()
        });
        assert_eq!(n, m);
        let mb = data.len() as f64 / 1e6;
        println!(
            "{fmt:?}: slice {:.0} MB/s, stream {:.0} MB/s",
            mb / ts,
            mb / tr
        );
    }
}

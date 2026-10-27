//! A short run of the fast front end for an instruction-level profiler
//! (`valgrind --tool=callgrind`): synthetic ISDB-S3-like input at 40 Msps
//! (development tool; plan §0.10).
//!
//! cargo build --release -p isdb-frontend --example feprof

use isdb_frontend::fast::FastFrontend;
use isdb_frontend::params::{Design, Params};
fn main() {
    let n: usize = std::env::args()
        .nth(1)
        .map_or(2_000_000, |s| s.parse().unwrap());
    let mut d = Design::new(40e6, 33.7561e6, 0.03);
    d.trim = -0.16;
    d.carrier_offset = -130e3;
    let p = Params::design(&d).unwrap();
    let mut st = 1u32;
    let x: Vec<[i16; 2]> = (0..n)
        .map(|_| {
            st = st.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            [((st >> 16) as i16) >> 5, ((st & 0xFFFF) as i16) >> 5]
        })
        .collect();
    let mut f = FastFrontend::new(p).unwrap();
    let mut acc = 0i64;
    let mut out = Vec::new();
    for c in x.chunks(1 << 16) {
        out.clear();
        f.process(c, &mut out);
        acc += out.iter().map(|y| i64::from(y.zk[0])).sum::<i64>();
    }
    println!("{acc}");
}

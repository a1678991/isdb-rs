//! Largest coefficient magnitude of the designed banks, per system and
//! sample rate (development tool; plan §0.10).

use isdb_frontend::params::{Design, Params};
fn main() {
    for (name, rs, a) in [("ISDB-S", 28.86e6, 0.35), ("ISDB-S3", 33.7561e6, 0.03)] {
        for fs in [36e6, 40e6, 57.72e6, 61.44e6] {
            for trim in [0.0, -0.17] {
                let mut d = Design::new(fs, rs, a);
                d.trim = trim;
                let p = Params::design(&d).unwrap();
                let m = |b: &Vec<Vec<i32>>| b.iter().flatten().map(|c| c.abs()).max().unwrap();
                println!(
                    "{name} {:.2} Msps trim {trim}: |h| {} |g| {} |k| {}",
                    fs / 1e6,
                    m(&p.h),
                    m(&p.g),
                    m(&p.k)
                );
            }
        }
    }
}

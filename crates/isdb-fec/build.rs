//! Sets `cfg(isdb_fec_portable_simd)` when the compiler builds the
//! `std::simd` API the LDPC kernels use (a nightly toolchain with
//! `#![feature(portable_simd)]` in the shape this crate expects), unless
//! `ISDB_FEC_NO_NIGHTLY_SIMD` is set. Otherwise the portable array kernels
//! are used: same results, stable compilers.

use std::path::PathBuf;
use std::process::Command;

/// The `std::simd` items the kernels use, compiled as a probe.
const PROBE: &str = r"#![feature(portable_simd)]
use std::simd::cmp::{SimdOrd, SimdPartialEq, SimdPartialOrd};
use std::simd::num::SimdInt;
use std::simd::{Select, Simd};
pub fn probe<const W: usize>(a: [i8; W], b: [i8; W]) -> [i8; W] {
    let (x, y) = (Simd::<i8, W>::from_array(a), Simd::<i8, W>::from_array(b));
    let m = x.simd_lt(y) | x.simd_eq(y);
    let z = m.select(x.saturating_sub(y), y.saturating_add(x)).saturating_abs();
    (z.simd_max(Simd::splat(0)) >> Simd::splat(7)).to_array()
}
pub fn both() -> ([i8; 1], [i8; 32]) { (probe([1], [2]), probe([1; 32], [2; 32])) }
";

fn main() {
    println!("cargo::rustc-check-cfg=cfg(isdb_fec_portable_simd)");
    println!("cargo::rerun-if-env-changed=ISDB_FEC_NO_NIGHTLY_SIMD");
    println!("cargo::rerun-if-changed=build.rs");
    if std::env::var_os("ISDB_FEC_NO_NIGHTLY_SIMD").is_some() {
        return;
    }
    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR"));
    let src = out.join("simd_probe.rs");
    if std::fs::write(&src, PROBE).is_err() {
        return;
    }
    let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let ok = Command::new(rustc)
        .args([
            "--edition=2024",
            "--crate-type=lib",
            "--emit=metadata",
            "-o",
        ])
        .arg(out.join("simd_probe.rmeta"))
        .arg(&src)
        .output()
        .is_ok_and(|o| o.status.success());
    if ok {
        println!("cargo::rustc-cfg=isdb_fec_portable_simd");
    }
}

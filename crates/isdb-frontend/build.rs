//! Sets `cfg(isdb_frontend_avx512)` when the compiler accepts the AVX-512
//! target features and intrinsics the front end's AVX-512 kernel uses
//! (stable from Rust 1.89; the workspace's minimum is 1.88, where the
//! kernel is left out and `simd::Isa::Avx512` falls back to AVX2).

use std::path::PathBuf;
use std::process::Command;

/// The AVX-512 items the kernel uses, compiled as a probe.
const PROBE: &str = r#"
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx512f,avx512bw,avx512vl")]
pub fn probe(a: [i16; 16]) -> i32 {
    use std::arch::x86_64::*;
    let y = _mm256_setr_epi16(
        a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7],
        a[8], a[9], a[10], a[11], a[12], a[13], a[14], a[15],
    );
    let z = _mm512_inserti64x4::<1>(_mm512_castsi256_si512(y), y);
    let m = _mm512_madd_epi16(z, z);
    let s = _mm256_add_epi32(_mm512_castsi512_si256(m), _mm512_extracti64x4_epi64::<1>(m));
    _mm256_extract_epi32::<0>(s)
}
"#;

fn main() {
    println!("cargo::rustc-check-cfg=cfg(isdb_frontend_avx512)");
    println!("cargo::rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() != Ok("x86_64") {
        return;
    }
    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR"));
    let src = out.join("avx512_probe.rs");
    if std::fs::write(&src, PROBE).is_err() {
        return;
    }
    let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let mut cmd = Command::new(rustc);
    cmd.args([
        "--edition=2024",
        "--crate-type=lib",
        "--emit=metadata",
        "-o",
    ])
    .arg(out.join("avx512_probe.rmeta"))
    .arg(&src);
    if let Ok(target) = std::env::var("TARGET") {
        cmd.args(["--target", &target]);
    }
    if cmd.output().is_ok_and(|o| o.status.success()) {
        println!("cargo::rustc-cfg=isdb_frontend_avx512");
    }
}

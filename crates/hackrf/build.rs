//! Links the system libhackrf.
fn main() {
    // libhackrf from the system package (Arch: hackrf; Debian/Ubuntu: libhackrf-dev).
    println!("cargo:rustc-link-lib=dylib=hackrf");
}

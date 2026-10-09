//! Links the system libhackrf.
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=DOCS_RS");
    // docs.rs has no libhackrf; rustdoc does not link, so skip it there.
    if std::env::var_os("DOCS_RS").is_some() {
        return;
    }
    // libhackrf from the system package (Arch: hackrf; Debian/Ubuntu:
    // libhackrf-dev; Homebrew: hackrf). The dynamic linker's default
    // search path is used; set RUSTFLAGS="-L <dir>" for other prefixes.
    println!("cargo:rustc-link-lib=dylib=hackrf");
}

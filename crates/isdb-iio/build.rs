//! Links the system libiio.
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=DOCS_RS");
    // docs.rs has no libiio; rustdoc does not link, so skip it there.
    if std::env::var_os("DOCS_RS").is_some() {
        return;
    }
    // libiio 0.x from the system package (Arch: libiio; Debian/Ubuntu:
    // libiio-dev; Homebrew: libiio). The dynamic linker's default search
    // path is used; set RUSTFLAGS="-L <dir>" for other prefixes.
    println!("cargo:rustc-link-lib=dylib=iio");
}

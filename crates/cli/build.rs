//! Finds what the program carries inside it: the control planes and the runner agent, built by `./build.sh`, and
//! the changelog. In this repository they are where the build leaves them; in the crate published to crates.io
//! they come along in `embedded/`, already built, so that `cargo install superci` needs nothing but Rust. A copy of
//! the repository built elsewhere (a coverage tool's isolated copy) finds them in the one `SUPERCI_BUILT` names.
use std::path::Path;

fn main() {
    let here = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let here = Path::new(&here);
    for (name, built, packed) in [
        ("SUPERCI_WORKER_JS", "../plane-cloudflare/build/index.js", "plane-cloudflare.js"),
        ("SUPERCI_WORKER_WASM", "../plane-cloudflare/build/index_bg.wasm", "plane-cloudflare.wasm"),
        ("SUPERCI_RUNNERS_JS", "../runners-cloudflare/build/index.js", "runners-cloudflare.js"),
        ("SUPERCI_RUNNERS_WASM", "../runners-cloudflare/build/index_bg.wasm", "runners-cloudflare.wasm"),
        ("SUPERCI_PLANE_AWS", "../plane-aws/build/bootstrap.zip", "plane-aws.zip"),
        ("SUPERCI_PLANE_MODAL", "../plane-modal/build/superci-plane", "plane-modal"),
        ("SUPERCI_CACHE_X86_64", "../../target/x86_64-unknown-linux-musl/release/superci-cache", "cache-x86_64"),
        ("SUPERCI_CACHE_AARCH64", "../../target/aarch64-unknown-linux-musl/release/superci-cache", "cache-aarch64"),
        ("SUPERCI_CACHE_WINDOWS", "../../target/x86_64-pc-windows-gnu/release/superci-cache.exe", "cache-windows.exe"),
        ("SUPERCI_CHANGELOG", "../../CHANGELOG.md", "CHANGELOG.md"),
    ] {
        let elsewhere = std::env::var("SUPERCI_BUILT").ok().map(|root| Path::new(&root).join("crates/cli").join(built));
        let (built, packed) = (here.join(built), here.join("embedded").join(packed));
        println!("cargo:rerun-if-changed={}", built.display());
        println!("cargo:rerun-if-changed={}", packed.display());
        println!("cargo:rerun-if-env-changed=SUPERCI_BUILT");
        let found = if built.exists() { built } else if packed.exists() { packed } else if let Some(e) = elsewhere.filter(|e| e.exists()) { e } else {
            panic!("{} is not built: run ./build.sh in the repository first (it builds what the program carries inside it)", built.display())
        };
        println!("cargo:rustc-env={name}={}", found.display());
    }
}

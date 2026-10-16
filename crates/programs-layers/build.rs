//! Finds what this crate carries, built by `./build.sh`: in this repository where the build leaves it; in the crate
//! published to crates.io in `embedded/`, already built; in a copy of the repository built elsewhere (a coverage
//! tool's isolated copy), in the one `SUPERCI_BUILT` names.
use std::path::Path;

fn main() {
    let here = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let here = Path::new(&here);
    for (name, built, packed) in [
        ("LAYERS_X86_64", "../../target/x86_64-unknown-linux-musl/release/superci-layers", "layers-x86_64"),
        ("LAYERS_AARCH64", "../../target/aarch64-unknown-linux-musl/release/superci-layers", "layers-aarch64"),
    ] {
        let elsewhere = std::env::var("SUPERCI_BUILT").ok().map(|root| Path::new(&root).join("crates/programs-layers").join(built));
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

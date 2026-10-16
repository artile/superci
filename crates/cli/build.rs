//! Finds the changelog the program carries (what `./build.sh` builds is in the superci-programs-* crates). In this
//! repository where it is; in the crate published to crates.io in `embedded/`; in a copy of the repository built
//! elsewhere (a coverage tool's isolated copy), in the one `SUPERCI_BUILT` names.
use std::path::Path;

fn main() {
    let here = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let here = Path::new(&here);
    for (name, built, packed) in [
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

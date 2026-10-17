//! The reader where it runs: Linux with FUSE, recreated as a privileged container. A small image (a squashfs of
//! random files) packed by the program built here, served from the container itself, shown by the reader and mounted
//! as a machine mounts it; then read whole, with a changed piece, another index, a hot list, a small cache and a full
//! disk (`reader.sh`). What the program does in there is measured as any test's, where something measures it (its
//! settings carried into the container). Only where Docker runs Linux containers of this computer's own kind (or with
//! a Linux build named by `IMAGE_READER_LINUX_BIN`); skipped elsewhere.
use std::process::Command;

fn program() -> String { std::env::var("IMAGE_READER_LINUX_BIN").unwrap_or_else(|_| env!("CARGO_BIN_EXE_image-reader").to_string()) }

fn here() -> bool {
    if !cfg!(target_os = "linux") && std::env::var_os("IMAGE_READER_LINUX_BIN").is_none() { eprintln!("not a Linux computer: skipped"); return false }
    if !Command::new("docker").args(["info", "-f", "{{.OSType}}"]).output().is_ok_and(|o| o.status.success()) { eprintln!("no Docker here: skipped"); return false }
    true
}

/// What measures the tests, carried into a container: its settings, and its folder mounted at the same path.
fn measured() -> Vec<String> {
    let mut args = vec![];
    for (k, v) in std::env::vars().filter(|(k, _)| k.starts_with("SUPERCOV_")) {
        if k.ends_with("_DIR") { std::fs::create_dir_all(&v).ok(); args.extend(["-v".to_string(), format!("{v}:{v}")]) }
        args.extend(["-e".to_string(), format!("{k}={v}")]);
    }
    args
}

fn random(n: usize, seed: u64) -> Vec<u8> {
    let mut x = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    (0..n).map(|_| { x ^= x << 13; x ^= x >> 7; x ^= x << 17; (x >> 24) as u8 }).collect()
}

#[test]
fn an_image_is_shown_from_its_pieces_and_mounted_as_a_machine_mounts_it() {
    if !here() { return }
    let t = std::env::temp_dir().join(format!("image-reader-fuse-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&t);
    std::fs::create_dir_all(t.join("tree/bin")).unwrap();
    std::fs::create_dir_all(t.join("tree/data")).unwrap();
    // A tree as the harness by hand has it: one big file, eight tools, a line.
    let mut files: Vec<(String, Vec<u8>)> = vec![("./data/big.bin".into(), random(30_000_000, 1)), ("./hello.txt".into(), b"hello\n".to_vec())];
    files.extend((1..=8).map(|i| (format!("./bin/tool{i}"), random(700_000, 10 + i as u64))));
    files.sort();
    let mut expected = String::new();
    for (p, b) in &files {
        std::fs::write(t.join("tree").join(&p[2..]), b).unwrap();
        use sha2::Digest;
        expected.push_str(&format!("{}  {p}\n", sha2::Sha256::digest(b).iter().map(|x| format!("{x:02x}")).collect::<String>()));
    }
    std::fs::write(t.join("expected.txt"), expected).unwrap();
    std::fs::write(t.join("hot.json"), "[3,1,0]").unwrap();
    std::fs::copy(program(), t.join("reader")).unwrap();
    std::fs::copy(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/reader.sh"), t.join("test.sh")).unwrap();
    // Packed there (squashfs-tools in the container), then read as on a machine.
    let script = "set -e; apt-get update -qq >/dev/null && apt-get install -y -qq squashfs-tools python3-minimal >/dev/null; mksquashfs /t/tree /t/small.sqsh -comp zstd -b 131072 -noappend -quiet -no-progress >/dev/null; \
        /t/reader pack /t/small.sqsh /t/out --piece 1048576 --hot /t/hot.json --reader /t/reader >/dev/null; bash /t/test.sh";
    let mut args: Vec<String> = ["run", "--rm", "--privileged", "-v", &format!("{}:/t", t.display())].iter().map(|s| s.to_string()).collect();
    args.extend(measured());
    // (Ubuntu 26.04: a C library as new as the one the program was built against, as machines have.)
    args.extend(["ubuntu:26.04".to_string(), "bash".into(), "-c".into(), script.into()]);
    let o = Command::new("docker").args(&args).output().unwrap();
    let said = String::from_utf8_lossy(&o.stdout).into_owned() + &String::from_utf8_lossy(&o.stderr);
    let _ = std::fs::remove_dir_all(&t);
    assert!(o.status.success() && said.contains(", failed 0"), "{said}");
}

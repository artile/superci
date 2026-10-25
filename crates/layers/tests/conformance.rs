//! containerd's own conformance suites for snapshotters and content stores (`conformance/`, in Go), run against the
//! program built here, as containerd reaches it: what the program does under them is measured as any test's. Linux,
//! as root (through sudo where the test is not run as root), with Go; skipped elsewhere.
use std::process::Command;

#[test]
fn containerds_own_suites_pass_against_the_program() {
    if !cfg!(target_os = "linux") { eprintln!("not a Linux computer: skipped"); return }
    if !Command::new("go").arg("version").output().is_ok_and(|o| o.status.success()) { eprintln!("no Go here: skipped"); return }
    let root = unsafe { libc::geteuid() } == 0;
    if !root && !Command::new("sudo").args(["-n", "true"]).status().is_ok_and(|s| s.success()) { eprintln!("not root, and no sudo without a password: skipped"); return }
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("conformance");
    let suite = std::env::temp_dir().join(format!("superci-layers-conformance-{}", std::process::id()));
    let built = Command::new("go").args(["test", "-c", "-o"]).arg(&suite).arg(".").current_dir(&dir).output().unwrap();
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    // (Through sudo, what the suite needs given on its command line, a coverage tool's settings too: sudo keeps only
    // some of the environment.)
    let mut run = if root { Command::new(&suite) } else {
        let mut c = Command::new("sudo");
        c.args(["-n", "env", &format!("SUPERCI_LAYERS={}", env!("CARGO_BIN_EXE_superci-layers"))]).args(std::env::vars().filter(|(k, _)| k.starts_with("SUPERCOV_") || k == "TMPDIR").map(|(k, v)| format!("{k}={v}"))).arg(&suite);
        c
    };
    let out = run.args(["-test.count=1"]).env("SUPERCI_LAYERS", env!("CARGO_BIN_EXE_superci-layers")).output().unwrap();
    let _ = std::fs::remove_file(&suite);
    assert!(out.status.success(), "{}\n{}", String::from_utf8_lossy(&out.stdout).lines().rev().take(60).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n"), String::from_utf8_lossy(&out.stderr));
}

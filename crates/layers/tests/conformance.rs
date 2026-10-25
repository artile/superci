//! containerd's own conformance suites for snapshotters and content stores (`conformance/`, in Go), run against the
//! program built here, as containerd reaches it: what the program does under them is measured as any test's. Linux,
//! as root (sudo, keeping the environment, where the test is not run as root), with Go; skipped elsewhere.
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
    let mut run = if root { Command::new(&suite) } else { let mut c = Command::new("sudo"); c.arg("-E").arg(&suite); c };
    let out = run.args(["-test.count=1"]).env("SUPERCI_LAYERS", env!("CARGO_BIN_EXE_superci-layers")).output().unwrap();
    let _ = std::fs::remove_file(&suite);
    assert!(out.status.success(), "{}\n{}", String::from_utf8_lossy(&out.stdout).lines().rev().take(60).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n"), String::from_utf8_lossy(&out.stderr));
}

//! `superci runs create` as it runs, against a control plane served on this computer (`superci_testkit`), the test
//! playing the machine's runner: the working tree sent, every line of the job printed once and in order (what the
//! runner dropped as it went, from the step's whole log), and how the run ended.
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use superci_testkit::{serve, Runner};

fn checkout() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("superci-e2e-{}-{}", std::process::id(), superci_core_free_id()));
    std::fs::create_dir_all(dir.join(".github/workflows")).unwrap();
    std::fs::write(dir.join(".github/workflows/ci.yml"), "jobs:\n  tests:\n    runs-on: superci\n    steps:\n      - run: make\n      - run: make test\n").unwrap();
    std::fs::write(dir.join("Makefile"), "test:\n\techo ok\n").unwrap();
    for args in [&["init", "-q"][..], &["add", "-A"], &["-c", "user.name=t", "-c", "user.email=t@example.com", "commit", "-qm", "t"]] {
        assert!(Command::new("git").current_dir(&dir).args(args).status().unwrap().success());
    }
    dir
}

fn superci_core_free_id() -> u128 { std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos() }

#[test]
fn a_run_from_a_computer_prints_every_line_once_in_order_and_ends_with_how_each_step_went() {
    let served = serve(|_| {});
    let dir = checkout();
    let home = dir.join(".home");
    let mut cli = Command::new(env!("CARGO_BIN_EXE_superci")).current_dir(&dir).args(["runs", "create", "--workflow=.github/workflows/ci.yml"])
        .env("SUPERCI_HOME", &home).env("SUPERCI_PLANE", &served.url).env("SUPERCI_KEY", &served.run_key).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    // The machine started; its runner (played here) takes the job.
    let run = loop {
        if let Some((k, _)) = served.store.under("run:").into_iter().next() { break k.trim_start_matches("run:").to_string() }
        std::thread::sleep(std::time::Duration::from_millis(50));
    };
    let runner = loop { if let Some(r) = Runner::of_run(&served, &run) { break r } std::thread::sleep(std::time::Duration::from_millis(50)) };
    let line = |n: u64| format!("build line {n}");
    let (a, b) = ("aaaaaaaa-0000-0000-0000-000000000001", "bbbbbbbb-0000-0000-0000-000000000002");
    assert_eq!(runner.steps(&[(a, "Run make", 1, "inProgress", None, None), (b, "Run make test", 2, "pending", None, None)]), 200);
    // Lines 1-3 as they come, then 7-8: the runner dropped 4-6 (it holds 1,024 at most).
    assert_eq!(runner.lines(a, 1, &(1..=3).map(line).collect::<Vec<_>>()), 200);
    std::thread::sleep(std::time::Duration::from_millis(1800));
    assert_eq!(runner.lines(a, 7, &(7..=8).map(line).collect::<Vec<_>>()), 200);
    std::thread::sleep(std::time::Duration::from_millis(1800));
    // The step ends: its whole log; then the next step, and the job.
    assert_eq!(runner.log(1, &(1..=8).map(|n| format!("2026-10-15T10:00:0{}.0000000Z {}", n % 10, line(n))).collect::<Vec<_>>().join("\n")), 200);
    assert_eq!(runner.steps(&[(a, "Run make", 1, "completed", Some("succeeded"), Some(1)), (b, "Run make test", 2, "inProgress", None, None)]), 200);
    assert_eq!(runner.lines(b, 1, &["ok".to_string()]), 200);
    assert_eq!(runner.log(2, "2026-10-15T10:00:09.0000000Z ok"), 200);
    assert_eq!(runner.steps(&[(a, "Run make", 1, "completed", Some("succeeded"), Some(1)), (b, "Run make test", 2, "completed", Some("succeeded"), Some(2))]), 200);
    assert_eq!(runner.finish("succeeded"), 200);
    let out = cli.wait_with_output().unwrap();
    let said = String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{said}");
    // Every line once, in the order printed: 1-3 as they came, the gap said, 7-8, then 4-6 once the step ended.
    let printed: Vec<&str> = said.lines().map(str::trim).collect();
    let at = |s: &str| printed.iter().position(|l| l.ends_with(s)).unwrap_or_else(|| panic!("no {s:?} in:\n{said}"));
    for n in 1..=8 { assert_eq!(printed.iter().filter(|l| l.ends_with(&line(n))).count(), 1, "{n} once:\n{said}") }
    assert!(at(&line(3)) < at("lines 4–6 of this step follow when it ends") && at("follow when it ends") < at(&line(7)) && at(&line(8)) < at("which came late:") && at("which came late:") < at(&line(4)), "{said}");
    // How it ended: each step, then the run.
    let has = |s: &str| printed.iter().position(|l| l.contains(s)).unwrap_or_else(|| panic!("no {s:?} in:\n{said}"));
    assert!(has("✓ Run make 7 s") < has("✓ Run make test 7 s") && said.contains("The run ended: success"), "{said}");
    let _ = std::fs::remove_dir_all(&dir);
}

//! `superci runs create` as it runs, against a control plane served on this computer (`superci_testkit`), the test
//! playing the machine's runner: the working tree sent, every line of the job printed once and in order (what the
//! runner dropped as it went, from the step's whole log), and how the run ended.
use std::process::{Command, Stdio};
use superci_testkit::{serve, Runner};

fn checkout() -> std::path::PathBuf {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!("superci-e2e-{}-{}-{}", std::process::id(), N.fetch_add(1, std::sync::atomic::Ordering::SeqCst), superci_core_free_id()));
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
    let cli = Command::new(env!("CARGO_BIN_EXE_superci")).current_dir(&dir).args(["runs", "create", "--workflow=.github/workflows/ci.yml"])
        .env("SUPERCI_HOME", &home).env("SUPERCI_PLANE", &served.url).env("SUPERCI_KEY", &served.run_key).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    // The machine started; its runner (played here) takes the job.
    let run = loop {
        if let Some((k, _)) = served.store.under("run:").into_iter().next() { break k.trim_start_matches("run:").to_string() }
        std::thread::sleep(std::time::Duration::from_millis(50));
    };
    let runner = loop { if let Some(r) = Runner::of_run(&served, &run) { break r } std::thread::sleep(std::time::Duration::from_millis(50)) };
    let line = |n: u64| format!("build line {n}");
    let (a, b) = ("aaaaaaaa-0000-0000-0000-000000000001", "bbbbbbbb-0000-0000-0000-000000000002");
    runner.take();
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

/// Plays a one-step run to its end: `lines` as they come (all of them), its whole log, and how it ended.
fn play(served: &superci_testkit::Served, run: &str, lines: &[String], result: &str) {
    let runner = loop { if let Some(r) = Runner::of_run(served, run) { break r } std::thread::sleep(std::time::Duration::from_millis(50)) };
    let a = "cccccccc-0000-0000-0000-000000000003";
    runner.take();
    runner.steps(&[(a, "Run make test", 1, "inProgress", None, None)]);
    runner.lines(a, 1, lines);
    runner.log(1, &lines.iter().map(|l| format!("2026-10-15T10:00:01.0000000Z {l}")).collect::<Vec<_>>().join("\n"));
    runner.steps(&[(a, "Run make test", 1, "completed", Some(if result == "succeeded" { "succeeded" } else { "failed" }), Some(1))]);
    runner.finish(result);
}

fn run_of(served: &superci_testkit::Served) -> String {
    loop {
        if let Some((k, _)) = served.store.under("run:").into_iter().next() { return k.trim_start_matches("run:").to_string() }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

fn superci(served: &superci_testkit::Served, dir: &std::path::Path, args: &[&str]) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_superci"));
    c.current_dir(dir).args(args).env("SUPERCI_HOME", dir.join(".home")).env("SUPERCI_PLANE", &served.url).env("SUPERCI_KEY", &served.run_key).stdout(Stdio::piped()).stderr(Stdio::piped());
    c
}

#[test]
fn quiet_a_failed_run_says_how_each_step_went_and_the_end_of_the_one_that_failed_and_fails() {
    let served = serve(|_| {});
    let dir = checkout();
    let cli = superci(&served, &dir, &["runs", "create", "--workflow=.github/workflows/ci.yml", "--quiet"]).spawn().unwrap();
    let run = run_of(&served);
    let mut lines: Vec<String> = (1..=60).map(|n| format!("test {n} ok")).collect();
    lines.push("test 61 FAILED - expected 2, got 3".into());
    play(&served, &run, &lines, "failed");
    let out = cli.wait_with_output().unwrap();
    let said = String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{said}");
    assert!(said.contains("✗ Run make test") && said.contains("test 61 FAILED") && said.contains("The run ended: failure"), "{said}");
    assert!(!said.contains("test 1 ok") && said.contains("test 60 ok"), "only the last lines of the failed step:\n{said}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_run_started_and_left_is_followed_later_to_its_end() {
    let served = serve(|_| {});
    let dir = checkout();
    let out = superci(&served, &dir, &["runs", "create", "--workflow=.github/workflows/ci.yml", "--detach"]).output().unwrap();
    let said = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(out.status.success() && said.contains("Follow it: superci runs wait"), "{said}{}", String::from_utf8_lossy(&out.stderr));
    let run = run_of(&served);
    let wait = superci(&served, &dir, &["runs", "wait", &run]).spawn().unwrap();
    play(&served, &run, &["all good".to_string()], "succeeded");
    let out = wait.wait_with_output().unwrap();
    let said = String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success() && said.contains("all good") && said.contains("followed from here"), "{said}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn with_a_key_for_coding_agents_what_ran_is_read_and_a_jobs_whole_log_is_kept_in_a_file() {
    let served = serve(|_| {});
    let dir = checkout();
    let cli = superci(&served, &dir, &["runs", "create", "--workflow=.github/workflows/ci.yml", "--quiet"]).spawn().unwrap();
    let run = run_of(&served);
    play(&served, &run, &(1..=5).map(|n| format!("line {n}")).collect::<Vec<_>>(), "succeeded");
    assert!(cli.wait_with_output().unwrap().status.success());
    let read = |args: &[&str]| { let o = superci(&served, &dir, args).output().unwrap(); (o.status.success(), String::from_utf8_lossy(&o.stdout).to_string() + &String::from_utf8_lossy(&o.stderr)) };
    let (ok, jobs) = read(&["jobs", "list"]);
    assert!(ok && jobs.contains("passed") && jobs.contains("tests"), "{jobs}");
    let job = jobs.split_whitespace().next().unwrap().to_string();
    let (ok, runs) = read(&["runs", "list"]);
    assert!(ok && runs.contains(&run), "{runs}");
    let (ok, usage) = read(&["usage", "retrieve"]);
    assert!(ok && usage.contains("The last 24 hours: 1 jobs, 1 ran"), "{usage}");
    let file = dir.join("job.log");
    let (ok, kept) = read(&["jobs", "retrieve", &job, &format!("--out={}", file.display())]);
    assert!(ok && kept.contains("Its whole log"), "{kept}");
    let text = std::fs::read_to_string(&file).unwrap();
    assert!((1..=5).all(|n| text.contains(&format!("line {n}"))), "{text}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_machine_that_never_begins_its_job_is_started_again_twice_then_the_run_fails_saying_why() {
    let served = serve(|_| {});
    let dir = checkout();
    let cli = Command::new(env!("CARGO_BIN_EXE_superci")).current_dir(&dir).args(["runs", "create", "--workflow=.github/workflows/ci.yml"])
        .env("SUPERCI_HOME", dir.join(".home")).env("SUPERCI_PLANE", &served.url).env("SUPERCI_KEY", &served.run_key).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    while served.machines.0.lock().unwrap().is_empty() { std::thread::sleep(std::time::Duration::from_millis(50)) }
    // Its runner never takes the job. Within three minutes that is waited for; after, its machine is started anew.
    served.later(60_000);
    served.upkeep();
    assert_eq!(served.machines.0.lock().unwrap().len(), 1);
    for starts in [2, 3] {
        served.later(4 * 60_000);
        served.upkeep();
        assert_eq!(served.machines.0.lock().unwrap().len(), starts);
    }
    served.later(4 * 60_000);
    served.upkeep();
    let job = served.store.under("job:").into_iter().next().unwrap().1;
    assert_eq!((job["state"].as_str(), job["error"].as_str()), (Some("failed"), Some("Cloudflare did not start a machine for it, three times")), "{job}");
    // The run from this computer ends with it.
    let out = cli.wait_with_output().unwrap();
    let said = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert!(!out.status.success() && said.contains("did not start a machine for it, three times"), "{said}");
}

#[test]
fn a_job_that_runs_past_its_longest_time_is_ended_by_the_upkeep_and_the_run_with_it() {
    let served = serve(|_| {});
    let dir = checkout();
    let cli = superci(&served, &dir, &["runs", "create", "--workflow=.github/workflows/ci.yml", "--quiet"]).spawn().unwrap();
    let run = run_of(&served);
    let runner = loop { if let Some(r) = Runner::of_run(&served, &run) { break r } std::thread::sleep(std::time::Duration::from_millis(50)) };
    runner.take();
    runner.steps(&[("cccccccc-0000-0000-0000-000000000003", "Run make test", 1, "inProgress", None, None)]);
    // Within its longest time (6 hours here, and 5 minutes more), it runs on.
    served.later(6 * 3_600_000);
    served.upkeep();
    assert_eq!(served.store.under("job:")[0].1["state"], "running");
    served.later(10 * 60_000);
    served.upkeep();
    let job = served.store.under("job:")[0].1.clone();
    assert_eq!(job["state"], "swept", "{job}");
    let out = cli.wait_with_output().unwrap();
    let said = String::from_utf8_lossy(&out.stdout);
    assert!(!out.status.success() && said.contains("failure (it ran past its longest time (6 hours), and its machine was ended)"), "{said}");
}

#[test]
fn a_failed_runs_tests_are_read_from_its_log_names_files_errors_and_counts() {
    let served = serve(|_| {});
    let dir = checkout();
    let cli = superci(&served, &dir, &["runs", "create", "--workflow=.github/workflows/ci.yml", "--quiet"]).spawn().unwrap();
    let run = run_of(&served);
    let vitest: Vec<String> = [" ❯ src/sum.test.ts (3 tests | 1 failed) 12ms", "⎯⎯⎯⎯⎯⎯⎯ Failed Tests 1 ⎯⎯⎯⎯⎯⎯⎯", "", " FAIL  src/sum.test.ts > sums > adds two numbers",
        "AssertionError: expected 3 to be 4 // Object.is equality", "", "⎯⎯⎯⎯⎯⎯⎯⎯⎯⎯⎯⎯⎯[1/1]⎯", " Test Files  1 failed | 4 passed (5)", "      Tests  1 failed | 41 passed (42)"].iter().map(|s| s.to_string()).collect();
    play(&served, &run, &vitest, "failed");
    let _ = cli.wait_with_output();
    // As a coding agent asks: the failed test, its file and error, and the counts.
    let o = superci(&served, &dir, &["runs", "retrieve", &run, "--tests", "--json"]).output().unwrap();
    let got: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap_or_else(|_| panic!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)));
    let tests = &got["runs"][0]["job_list"][0]["tests"];
    assert_eq!((tests["runners"][0].as_str(), tests["failed"].as_u64(), tests["passed"].as_u64()), (Some("vitest"), Some(1), Some(41)), "{got}");
    assert_eq!((tests["failures"][0]["name"].as_str(), tests["failures"][0]["file"].as_str()), (Some("sums > adds two numbers"), Some("src/sum.test.ts")));
    assert!(tests["failures"][0]["error"].as_str().unwrap().starts_with("AssertionError: expected 3 to be 4"));
    // As a person reads it.
    let said = String::from_utf8_lossy(&superci(&served, &dir, &["runs", "retrieve", &run, "--tests"]).output().unwrap().stdout).into_owned();
    assert!(said.contains("Tests: 1 failed, 41 passed (vitest)") && said.contains("✗ sums > adds two numbers  (src/sum.test.ts)") && said.contains("AssertionError: expected 3 to be 4"), "{said}");
}

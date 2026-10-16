//! `superci runs create` as it runs, against a control plane served on this computer (`superci_testkit`), the test
//! playing the machine's runner: the working tree sent, every line of the job printed once and in order (what the
//! runner dropped as it went, from the step's whole log), and how the run ended.
mod common;
use std::process::{Command, Stdio};
use superci_testkit::{serve, Browser, Runner};

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
    // (Its log has no tests: no line of them.)
    assert!(!said.contains("Tests:"), "{said}");
    // Once it ran, how its start went (where the time from sending it to its first step went).
    assert!(has("Running (started in ") < has(&line(1)) && said.contains("its machine was asked for "), "{said}");
    // Its end said after its lines, the late ones too.
    assert!(at(&line(4)) < has("Ended: success"), "{said}");
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
fn a_dry_run_checks_the_workflow_its_job_and_the_tree_and_starts_nothing() {
    let served = serve(|_| {});
    let dir = checkout();
    let o = superci(&served, &dir, &["runs", "create", "--workflow=.github/workflows/ci.yml", "--dry-run"]).output().unwrap();
    let said = String::from_utf8_lossy(&o.stdout).into_owned();
    assert!(o.status.success() && said.starts_with("Would send the working tree (") && said.contains("and run .github/workflows/ci.yml: its job ") && said.contains("Nothing was sent and no machine was started."), "{said}{}", String::from_utf8_lossy(&o.stderr));
    let o = superci(&served, &dir, &["runs", "create", "--workflow=.github/workflows/ci.yml", "--job=nope", "--dry-run"]).output().unwrap();
    assert!(!o.status.success() && String::from_utf8_lossy(&o.stderr).contains("has no job nope (its jobs: "), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(served.store.under("run:").is_empty() && served.store.under("rtree").is_empty() && served.machines.0.lock().unwrap().is_empty());
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
        "AssertionError: expected 3 to be 4 // Object.is equality", "", "⎯⎯⎯⎯⎯⎯⎯⎯⎯⎯⎯⎯⎯[1/1]⎯", " Test Files  1 failed | 4 passed (5)", "      Tests  1 failed | 41 passed (42)",
        // Then Playwright in two processes at once, each line marked by its own, then a long teardown.
        "[slot 1] Running 3 tests using 1 worker", "[slot 2] Running 2 tests using 1 worker", "[slot 2]   1) [chromium] › e2e/billing.spec.ts:21:3 › shows the invoice ───", "[slot 1]   3 passed (4.0s)",
        "[slot 2]     Error: expect(locator).toHaveText(expected) failed", "[slot 2]   1 failed", "[slot 2]   1 passed (5.1s)"].iter().map(|s| s.to_string()).chain((1..=50).map(|n| format!("stopping container {n}"))).collect();
    play(&served, &run, &vitest, "failed");
    // Quiet, the failed step's tests are said, though its last lines are the teardown's.
    let out = cli.wait_with_output().unwrap();
    let quiet = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(quiet.contains("✗ [chromium] shows the invoice (e2e/billing.spec.ts:21:3)\n        Error: expect(locator).toHaveText(expected) failed") && quiet.contains("✗ sums > adds two numbers (src/sum.test.ts)"), "{quiet}");
    assert!(quiet.contains("Tests: 2 failed, 45 passed (vitest, playwright)"), "{quiet}");
    // As a coding agent asks: the failed test, its file and error, and the counts.
    let o = superci(&served, &dir, &["runs", "retrieve", &run, "--tests", "--json"]).output().unwrap();
    let got: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap_or_else(|_| panic!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)));
    let tests = &got["runs"][0]["job_list"][0]["tests"];
    assert_eq!((tests["runners"][0].as_str(), tests["runners"][1].as_str(), tests["failed"].as_u64(), tests["passed"].as_u64()), (Some("vitest"), Some("playwright"), Some(2), Some(45)), "{got}");
    assert_eq!((tests["failures"][0]["name"].as_str(), tests["failures"][0]["file"].as_str()), (Some("sums > adds two numbers"), Some("src/sum.test.ts")));
    assert!(tests["failures"][0]["error"].as_str().unwrap().starts_with("AssertionError: expected 3 to be 4"));
    // As a person reads it.
    let said = String::from_utf8_lossy(&superci(&served, &dir, &["runs", "retrieve", &run, "--tests"]).output().unwrap().stdout).into_owned();
    assert!(said.contains("Tests: 2 failed, 45 passed (vitest, playwright)") && said.contains("✗ [chromium] shows the invoice  (e2e/billing.spec.ts:21:3)") && said.contains("✗ sums > adds two numbers  (src/sum.test.ts)") && said.contains("AssertionError: expected 3 to be 4"), "{said}");
}

#[test]
fn a_step_with_no_log_of_its_own_still_gives_its_tests_and_quiet_says_each_failure_whole_and_the_counts() {
    // A control plane with its own AWS account (logs into its bucket); a step whose whole log never came.
    let served = serve(|c| {
        (c.aws_own, c.aws_own_creds) = (Some(("111111111111".into(), "us-east-1".into())), Some(superci_core::aws::Credentials { access_key_id: "ASIAX".into(), secret_access_key: "secret".into(), session_token: Some("tok".into()), expires_at_ms: u64::MAX }));
        c.routing.order = vec![superci_core::plane::Pool::of("cloudflare")];
    });
    let bucket = superci_core::cache::bucket(superci_testkit::PLANE, "us-east-1");
    served.github.put_object(&format!("{bucket}/{}", superci_core::cache::program_key("x86_64")), b"program");
    let dir = checkout();
    let cli = superci(&served, &dir, &["runs", "create", "--workflow=.github/workflows/ci.yml", "--quiet"]).env("SUPERCI_STAND_IN", &served.aws.url).spawn().unwrap();
    let run = run_of(&served);
    let runner = loop { if let Some(r) = Runner::of_run(&served, &run) { break r } std::thread::sleep(std::time::Duration::from_millis(50)) };
    let (a, b) = ("aaaaaaaa-0000-0000-0000-000000000001", "bbbbbbbb-0000-0000-0000-000000000002");
    runner.take();
    runner.steps(&[(a, "Run make", 1, "inProgress", None, None), (b, "Run the tests", 2, "pending", None, None)]);
    runner.log(1, "2026-10-16T10:00:01.0000000Z built");
    runner.steps(&[(a, "Run make", 1, "completed", Some("succeeded"), Some(1)), (b, "Run the tests", 2, "inProgress", None, None)]);
    let diff: Vec<String> = (1..=20).map(|n| format!("[slot 1]       - expected line {n}")).collect();
    let lines: Vec<String> = ["[slot 1] Running 3 tests using 1 worker", "[slot 1]   1) [chromium] › e2e/settings.spec.ts:9:3 › saves ───", "[slot 1]", "[slot 1]     Error: expect(received).toEqual(expected)"].iter().map(|s| s.to_string())
        .chain(diff).chain(["[slot 1]   1 failed", "[slot 1]   2 passed (3.1s)"].iter().map(|s| s.to_string())).collect();
    runner.lines(b, 1, &lines);
    runner.steps(&[(a, "Run make", 1, "completed", Some("succeeded"), Some(1)), (b, "Run the tests", 2, "completed", Some("failed"), None)]);
    runner.finish("failed");
    let out = cli.wait_with_output().unwrap();
    let quiet = String::from_utf8_lossy(&out.stdout).into_owned();
    // Quiet: the failure with its whole error, and what the tests came to.
    assert!(quiet.contains("✗ [chromium] saves (e2e/settings.spec.ts:9:3)") && quiet.contains("Error: expect(received).toEqual(expected)") && quiet.contains("- expected line 20"), "{quiet}");
    assert!(quiet.contains("Tests: 1 failed, 2 passed (playwright)"), "{quiet}");
    // Its tests read from the job's whole log, the step with no log of its own in it.
    let o = superci(&served, &dir, &["runs", "retrieve", &run, "--tests", "--json"]).env("SUPERCI_STAND_IN", &served.aws.url).output().unwrap();
    let got: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap_or_else(|_| panic!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)));
    let tests = &got["runs"][0]["job_list"][0]["tests"];
    assert_eq!((tests["failed"].as_u64(), tests["passed"].as_u64(), tests["failures"][0]["file"].as_str()), (Some(1), Some(2), Some("e2e/settings.spec.ts:9:3")), "{got}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_run_whose_test_runner_found_no_test_says_so() {
    let served = serve(|_| {});
    let dir = checkout();
    let cli = superci(&served, &dir, &["runs", "create", "--workflow=.github/workflows/ci.yml", "--quiet"]).spawn().unwrap();
    let run = run_of(&served);
    play(&served, &run, &["Running 0 tests using 0 workers".to_string(), "Error: No tests found.".to_string(), "Make sure that arguments are regular expressions matching test files.".to_string()], "failed");
    let quiet = String::from_utf8_lossy(&cli.wait_with_output().unwrap().stdout).into_owned();
    assert!(quiet.contains("Tests: 0 failed, 0 passed (playwright)") && quiet.contains("Playwright found no test to run: a `file:line` must be the first line of a test"), "{quiet}");
    let said = String::from_utf8_lossy(&superci(&served, &dir, &["runs", "retrieve", &run, "--tests"]).output().unwrap().stdout).into_owned();
    assert!(said.contains("Playwright found no test to run"), "{said}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_green_run_whose_slot_ran_no_tests_says_so_lists_what_ran_and_its_job_is_found_by_its_run() {
    let served = serve(|_| {});
    let dir = checkout();
    let cli = superci(&served, &dir, &["runs", "create", "--workflow=.github/workflows/ci.yml", "--quiet"]).spawn().unwrap();
    let run = run_of(&served);
    // Three slots: one's `file:line` no longer points at a test, and no tests found is allowed there.
    let lines: Vec<String> = ["[slot 1] Running 2 tests using 1 worker", "[slot 1]   ✓  1 [chromium] › e2e/cart.spec.ts:12:3 › adds to cart (2.0s)", "[slot 1]   ✘  2 [chromium] › e2e/cart.spec.ts:30:3 › known bug (1.0s)", "[slot 1]   2 passed (3.0s)",
        "[slot 2] Running 0 tests using 0 workers", "[slot 3] Running 1 test using 1 worker", "[slot 3]   ✓  1 [chromium] › e2e/login.spec.ts:5:3 › logs in (1.0s)", "[slot 3]   1 passed (1.0s)"].iter().map(|s| s.to_string()).collect();
    play(&served, &run, &lines, "succeeded");
    let out = cli.wait_with_output().unwrap();
    let quiet = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(out.status.success(), "{quiet}");
    assert!(quiet.contains("Tests: 0 failed, 3 passed (playwright)") && quiet.contains("[slot 2] Playwright ran no tests."), "{quiet}");
    assert!(quiet.contains("What ran:") && quiet.contains("✓ [chromium] adds to cart  (e2e/cart.spec.ts:12:3)") && quiet.contains("✓ (failed as expected) [chromium] known bug"), "{quiet}");
    // Followed without --quiet, the same at its end.
    let all = String::from_utf8_lossy(&superci(&served, &dir, &["runs", "wait", &run]).output().unwrap().stdout).into_owned();
    assert!(all.contains("Tests: 0 failed, 3 passed (playwright)") && all.contains("[slot 2] Playwright ran no tests."), "{all}");
    // A passed job's tests too, with what was said of them.
    let said = String::from_utf8_lossy(&superci(&served, &dir, &["runs", "retrieve", &run, "--tests"]).output().unwrap().stdout).into_owned();
    assert!(said.contains("[slot 2] Playwright ran no tests."), "{said}");
    // Its job by its run, for a script: no id read out of text.
    let file = dir.join("job.log");
    let o = superci(&served, &dir, &["jobs", "retrieve", &format!("--run={run}"), &format!("--out={}", file.display())]).output().unwrap();
    assert!(o.status.success() && std::fs::read_to_string(&file).unwrap().contains("[slot 2] Running 0 tests"), "{}", String::from_utf8_lossy(&o.stderr));
    let o = superci(&served, &dir, &["jobs", "retrieve", &format!("--run={run}"), "--job=nope"]).output().unwrap();
    assert!(!o.status.success() && String::from_utf8_lossy(&o.stderr).contains("has no job nope (its jobs: tests)"), "{}", String::from_utf8_lossy(&o.stderr));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_failed_run_followed_with_json_gives_its_jobs_ids() {
    let served = serve(|_| {});
    let dir = checkout();
    let cli = superci(&served, &dir, &["runs", "create", "--workflow=.github/workflows/ci.yml", "--json"]).spawn().unwrap();
    let run = run_of(&served);
    play(&served, &run, &["broken".to_string()], "failed");
    let out = cli.wait_with_output().unwrap();
    assert!(!out.status.success());
    let got: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|_| panic!("{}", String::from_utf8_lossy(&out.stdout)));
    assert_eq!((got["ok"].as_bool(), got["result"].as_str(), got["run_id"].as_str()), (Some(false), Some("failure"), Some(run.as_str())), "{got}");
    let job = got["jobs"][0]["job_id"].as_str().unwrap_or_else(|| panic!("{got}")).to_string();
    assert!(got["error"].as_str().unwrap().contains("The run ended: failure"));
    let o = superci(&served, &dir, &["jobs", "retrieve", &job]).output().unwrap();
    assert!(o.status.success() && String::from_utf8_lossy(&o.stdout).contains("broken"), "{}", String::from_utf8_lossy(&o.stdout));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_job_on_a_machine_without_superci_s_cache_is_told_of_a_miss_as_github_says_it_and_artifacts_are_refused_saying_why() {
    let served = serve(|_| {});
    let dir = checkout();
    let o = superci(&served, &dir, &["runs", "create", "--workflow=.github/workflows/ci.yml", "--detach"]).output().unwrap();
    assert!(o.status.success());
    let run = run_of(&served);
    let runner = loop { if let Some(r) = Runner::of_run(&served, &run) { break r } std::thread::sleep(std::time::Duration::from_millis(50)) };
    let results = runner.at.trim_end_matches("/_apis/v1").to_string() + "/twirp/github.actions.results.api.v1.";
    let call = |what: &str| { let (status, _, body) = http("POST", &format!("{results}{what}"), br#"{"key":"setup-zig","version":"1"}"#); (status, serde_json::from_slice::<serde_json::Value>(&body).unwrap_or_default()) };
    // (JSON each time: the cache's client asks again five times for an answer that is not.)
    for what in ["CacheService/GetCacheEntryDownloadURL", "CacheService/CreateCacheEntry", "CacheService/FinalizeCacheEntryUpload"] {
        assert_eq!(call(what), (200, serde_json::json!({ "ok": false })), "{what}");
    }
    let (status, said) = call("ArtifactService/CreateArtifact");
    assert!(status == 501 && said["code"] == "unimplemented" && said["msg"].as_str().unwrap().contains("AWS machines only"), "{status} {said}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_job_starts_on_an_aws_spot_machine_as_aws_answered_its_first_pool_full_then_a_fleet_and_a_cancel_ends_it() {
    // A control plane with its own AWS account: a job's machine is asked of AWS, which answers as it did on Test (the
    // first pool, c8a.large in us-east-1f, had no machine; the rest were asked in one request, and AWS gave one).
    let served = serve(|c| {
        (c.aws_own, c.aws_own_creds) = (Some(("111111111111".into(), "us-east-1".into())), Some(superci_core::aws::Credentials { access_key_id: "ASIAX".into(), secret_access_key: "secret".into(), session_token: Some("tok".into()), expires_at_ms: u64::MAX }));
    });
    served.aws.replaying(include_str!("../fixtures/aws/launch/spot.jsonl"), &[]);
    // (Two CPUs: the machines asked for on Test.)
    let dir = checkout_with(&[(".github/workflows/ci.yml", "jobs:\n  tests:\n    runs-on: superci/cpu=2\n    steps:\n      - run: make test\n")]);
    let o = superci(&served, &dir, &["runs", "create", "--workflow=.github/workflows/ci.yml", "--detach"]).env("SUPERCI_STAND_IN", &served.aws.url).output().unwrap();
    assert!(o.status.success(), "{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
    let run = run_of(&served);
    let job = loop {
        if let Some((_, j)) = served.store.under("job:").into_iter().next().filter(|(_, j)| j["machine_id"].is_string() || j["state"] == "failed") { break j }
        std::thread::sleep(std::time::Duration::from_millis(50));
    };
    let calls: Vec<String> = served.aws.asked.lock().unwrap().iter().map(|a| { let asked = a["asked"].as_str().unwrap_or_default(); format!("{} {}", a["status"], asked.split('&').find(|p| p.starts_with("Action=")).unwrap_or(a["target"].as_str().unwrap_or_default())) }).collect();
    assert_eq!((job["cloud"].as_str(), job["machine_id"].as_str(), job["machine_type"].as_str(), job["zone"].as_str()), (Some("aws"), Some("i-00000000000000001"), Some("c8a.large"), Some("us-east-1d")), "{job}\n{calls:?}\n{:?}", served.aws.unanswered.lock().unwrap());
    // How its start went: the full pool, then the request for the rest.
    let asks = job["asks"].as_array().cloned().unwrap_or_default();
    assert!(asks.iter().any(|a| a["said"] == "InsufficientInstanceCapacity" && a["zone"] == "us-east-1f"), "{job}");
    // Cancelled before its runner began: its machine is ended in AWS.
    let o = superci(&served, &dir, &["runs", "cancel", &run]).output().unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let ended = served.aws.asked.lock().unwrap().iter().any(|a| a["asked"].as_str().unwrap_or_default().contains("Action=TerminateInstances") && a["asked"].as_str().unwrap_or_default().contains("i-00000000000000001"));
    assert!(ended, "{:?}", served.aws.asked.lock().unwrap().iter().map(|a| a["asked"].as_str().unwrap_or_default().chars().take(60).collect::<String>()).collect::<Vec<_>>());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_job_whose_label_says_spot_false_starts_on_an_aws_on_demand_machine_as_aws_answered() {
    let served = serve(|c| {
        (c.aws_own, c.aws_own_creds) = (Some(("111111111111".into(), "us-east-1".into())), Some(superci_core::aws::Credentials { access_key_id: "ASIAX".into(), secret_access_key: "secret".into(), session_token: Some("tok".into()), expires_at_ms: u64::MAX }));
    });
    served.aws.replaying(include_str!("../fixtures/aws/launch/on-demand.jsonl"), &[]);
    let dir = checkout_with(&[(".github/workflows/ci.yml", "jobs:\n  tests:\n    runs-on: superci/cpu=2/spot=false\n    steps:\n      - run: make test\n")]);
    let o = superci(&served, &dir, &["runs", "create", "--workflow=.github/workflows/ci.yml", "--detach"]).env("SUPERCI_STAND_IN", &served.aws.url).output().unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let job = loop {
        if let Some((_, j)) = served.store.under("job:").into_iter().next().filter(|(_, j)| j["machine_id"].is_string() || j["state"] == "failed") { break j }
        std::thread::sleep(std::time::Duration::from_millis(50));
    };
    assert_eq!((job["cloud"].as_str(), job["on_demand"].as_bool(), job["machine_id"].as_str(), job["machine_type"].as_str(), job["zone"].as_str()), (Some("aws"), Some(true), Some("i-00000000000000002"), Some("m7a.large"), Some("us-east-1a")), "{job}\n{:?}", served.aws.unanswered.lock().unwrap());
    // (No spot price asked: an on-demand machine is asked by its types' order.)
    assert!(!served.aws.asked.lock().unwrap().iter().any(|a| a["asked"].as_str().unwrap_or_default().contains("DescribeSpotPriceHistory")));
    let _ = std::fs::remove_dir_all(&dir);
}

/// A checkout with these files, committed.
fn checkout_with(files: &[(&str, &str)]) -> std::path::PathBuf {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!("superci-e2e-files-{}-{}-{}", std::process::id(), N.fetch_add(1, std::sync::atomic::Ordering::SeqCst), superci_core_free_id()));
    for (p, text) in files { let f = dir.join(p); std::fs::create_dir_all(f.parent().unwrap()).unwrap(); std::fs::write(f, text).unwrap(); }
    for args in [&["init", "-q"][..], &["add", "-A"], &["-c", "user.name=t", "-c", "user.email=t@example.com", "commit", "-qm", "t"]] { assert!(Command::new("git").current_dir(&dir).args(args).status().unwrap().success()) }
    dir
}

/// A runner's context data as plain JSON (`{t: 2, d: [{k, v}]}` a mapping, `{t: 1, a: […]}` a list).
fn plain(v: &serde_json::Value) -> serde_json::Value {
    match v["t"].as_u64() {
        Some(2) => serde_json::Value::Object(v["d"].as_array().into_iter().flatten().map(|e| (e["k"].as_str().unwrap_or_default().to_string(), plain(&e["v"]))).collect()),
        Some(1) => serde_json::Value::Array(v["a"].as_array().into_iter().flatten().map(plain).collect()),
        _ => v.clone(),
    }
}

/// The runner of the job shown as `name`, once it is started.
fn runner_of(served: &superci_testkit::Served, run: &str, name: &str) -> Runner {
    let end = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        if let Some(r) = Runner::of_job(served, run, name) { return r }
        let names: Vec<String> = served.store.under(&format!("run:{run}")).into_iter().next().map(|(_, r)| r["jobs"].as_array().into_iter().flatten().map(|j| format!("{}:{}", j["name"], j["state"])).collect()).unwrap_or_default();
        assert!(std::time::Instant::now() < end, "no job {name} started: {names:?}");
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

#[test]
fn a_job_that_calls_a_workflow_of_the_repository_runs_its_jobs_with_their_inputs_and_secrets_and_gives_its_outputs_on() {
    let served = serve(|_| {});
    let dir = checkout_with(&[
        (".github/workflows/ci.yml", "jobs:\n  build:\n    uses: ./.github/workflows/build.yml\n    with:\n      target: web\n    secrets: inherit\n  after:\n    needs: build\n    runs-on: superci\n    steps:\n      - run: echo ${{ needs.build.outputs.artifact }}\n"),
        (".github/workflows/build.yml", "on:\n  workflow_call:\n    inputs:\n      target:\n        type: string\n        required: true\n      fast:\n        type: boolean\n        default: true\n    outputs:\n      artifact:\n        value: ${{ jobs.compile.outputs.name }}\njobs:\n  compile:\n    runs-on: superci\n    outputs:\n      name: ${{ steps.c.outputs.name }}\n    steps:\n      - id: c\n        run: echo \"name=${{ inputs.target }}-bundle\" >> $GITHUB_OUTPUT\n  test:\n    needs: compile\n    runs-on: superci\n    steps:\n      - run: echo testing ${{ needs.compile.outputs.name }}\n"),
    ]);
    let o = superci(&served, &dir, &["runs", "create", "--workflow=.github/workflows/ci.yml", "--detach", "--secret=DEPLOY_TOKEN=s3cret-value"]).output().unwrap();
    assert!(o.status.success(), "{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
    let run = run_of(&served);
    // The called workflow's first job: its inputs (given and by default), and the run's secrets (inherited).
    let compile = runner_of(&served, &run, "build / compile");
    let message = compile.handed(&served);
    assert_eq!(plain(&message["contextData"]["inputs"]), serde_json::json!({ "target": "web", "fast": true }));
    assert_eq!(message["variables"]["DEPLOY_TOKEN"], serde_json::json!({ "value": "s3cret-value", "isSecret": true }));
    compile.take();
    compile.finish_with("succeeded", &[("name", "web-bundle")]);
    // Its next job, after it: what it needs named as in its own workflow.
    let test = runner_of(&served, &run, "build / test");
    assert_eq!(plain(&test.handed(&served)["contextData"]["needs"])["compile"]["outputs"]["name"], "web-bundle");
    test.take();
    test.finish("succeeded");
    // The job that needs the call: its result, and the called workflow's output made of its jobs'.
    let after = runner_of(&served, &run, "after");
    let needs = plain(&after.handed(&served)["contextData"]["needs"]);
    assert_eq!((needs["build"]["result"].as_str(), needs["build"]["outputs"]["artifact"].as_str()), (Some("success"), Some("web-bundle")), "{needs}");
    after.take();
    after.finish("succeeded");
}

#[test]
fn a_matrix_made_by_an_expression_is_read_once_the_job_it_needs_has_ended() {
    let served = serve(|_| {});
    let dir = checkout_with(&[(".github/workflows/ci.yml", "jobs:\n  setup:\n    runs-on: superci\n    outputs:\n      shards: ${{ steps.s.outputs.shards }}\n    steps:\n      - id: s\n        run: echo 'shards=[1,2,3]' >> $GITHUB_OUTPUT\n  test:\n    needs: setup\n    runs-on: superci\n    strategy:\n      matrix:\n        shard: ${{ fromJSON(needs.setup.outputs.shards) }}\n        os: [linux]\n    steps:\n      - run: echo shard ${{ matrix.shard }}\n")]);
    let o = superci(&served, &dir, &["runs", "create", "--workflow=.github/workflows/ci.yml", "--detach"]).output().unwrap();
    assert!(o.status.success(), "{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
    let run = run_of(&served);
    let setup = runner_of(&served, &run, "setup");
    setup.take();
    setup.finish_with("succeeded", &[("shards", "[1,2,3]")]);
    // Three jobs of it, each its combination.
    for shard in 1..=3 {
        let r = runner_of(&served, &run, &format!("test ({shard}, linux)"));
        assert_eq!(plain(&r.handed(&served)["contextData"]["matrix"]), serde_json::json!({ "shard": shard as f64, "os": "linux" }));
        r.take();
        r.finish("succeeded");
    }
}

#[test]
fn a_job_that_may_fail_lets_the_run_go_on_and_one_past_its_timeout_is_ended() {
    let served = serve(|_| {});
    let dir = checkout_with(&[(".github/workflows/ci.yml", "jobs:\n  flaky:\n    runs-on: superci\n    continue-on-error: true\n    steps:\n      - run: exit 1\n  after:\n    needs: flaky\n    runs-on: superci\n    timeout-minutes: 1\n    steps:\n      - run: sleep 600\n")]);
    let o = superci(&served, &dir, &["runs", "create", "--workflow=.github/workflows/ci.yml", "--detach"]).output().unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let run = run_of(&served);
    // It fails; as on GitHub, the job that needs it sees a success and runs.
    let flaky = runner_of(&served, &run, "flaky");
    flaky.take();
    flaky.finish("failed");
    let after = runner_of(&served, &run, "after");
    assert_eq!(plain(&after.handed(&served)["contextData"]["needs"])["flaky"]["result"], "success");
    let record = served.store.under(&format!("run:{run}"))[0].1.clone();
    let flaky_said = record["jobs"].as_array().unwrap().iter().find(|j| j["name"] == "flaky").unwrap().clone();
    assert_eq!((flaky_said["result"].as_str(), flaky_said["failed"].as_bool()), (Some("success"), Some(true)), "{flaky_said}");
    // It runs past its minute: the upkeep ends it, and says why.
    after.take();
    after.steps(&[("eeeeeeee-0000-0000-0000-000000000005", "Run sleep 600", 1, "inProgress", None, None)]);
    served.later(30_000);
    served.upkeep();
    assert!(served.store.under("job:").iter().all(|(_, j)| j["error"].as_str().is_none_or(|e| !e.contains("timeout-minutes"))), "not yet");
    served.later(60_000);
    served.upkeep();
    let ended = served.store.under("job:").into_iter().map(|(_, j)| j).find(|j| j["name"] == "after").unwrap();
    assert_eq!((ended["state"].as_str(), ended["error"].as_str()), (Some("failed"), Some("it ran past its timeout-minutes (1), and its machine was ended")), "{ended}");
    let record = served.store.under(&format!("run:{run}"))[0].1.clone();
    assert_eq!(record["jobs"].as_array().unwrap().iter().find(|j| j["name"] == "after").unwrap()["result"], "failure");
}

#[test]
fn a_run_from_a_computer_is_followed_in_the_dashboard_from_runs_to_its_jobs_log() {
    let Some(mut b) = Browser::open(1300, 900) else { eprintln!("no Chrome here: skipped"); return };
    let served = serve(|_| {});
    let dir = checkout();
    let started = superci(&served, &dir, &["runs", "create", "--workflow=.github/workflows/ci.yml", "--detach"]).output().unwrap();
    assert!(started.status.success(), "{}", String::from_utf8_lossy(&started.stderr));
    let run = run_of(&served);
    play(&served, &run, &["compiling".to_string(), "test 1 passed".to_string()], "succeeded");
    let home = common::signed_in(&served);
    let (mut child, link) = common::dashboard(&home);
    let shows = |words: &str| format!("document.querySelector('main') && document.querySelector('main').innerText.includes({})", serde_json::json!(words));
    b.goto(&link);
    b.wait(&shows("Usage"));
    // Runs: the run, by its workflow; its page: its job; the job's page: its steps, and their lines.
    b.click("nav a", "Runs");
    b.wait(r#"document.querySelector('main a[href*="p=run&"]') !== null"#);
    b.eval(r#"document.querySelector('main a[href*="p=run&"]').click()"#);
    b.wait(&shows("tests"));
    b.wait(r#"document.querySelector('main a[href*="p=job&"]') !== null"#);
    b.eval(r#"document.querySelector('main a[href*="p=job&"]').click()"#);
    b.wait("document.querySelector('details.lstep') !== null");
    b.click("details.lstep summary", "Run make test");
    b.wait("[...document.querySelectorAll('.ll .lx')].some(l => l.textContent === 'test 1 passed')");
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn overviews_period_and_the_runs_and_jobs_tabs_switch_in_the_browser() {
    let Some(mut b) = Browser::open(1300, 900) else { eprintln!("no Chrome here: skipped"); return };
    let served = serve(|_| {});
    let dir = checkout();
    let started = superci(&served, &dir, &["runs", "create", "--workflow=.github/workflows/ci.yml", "--detach"]).output().unwrap();
    assert!(started.status.success(), "{}", String::from_utf8_lossy(&started.stderr));
    play(&served, &run_of(&served), &["ok".to_string()], "succeeded");
    let home = common::signed_in(&served);
    let (mut child, link) = common::dashboard(&home);
    let shows = |words: &str| format!("document.querySelector('main') && document.querySelector('main').innerText.includes({})", serde_json::json!(words));
    let on = |period: &str| format!(r#"document.querySelector('nav.seg-links a[aria-current="true"]') && document.querySelector('nav.seg-links a[aria-current="true"]').textContent === {}"#, serde_json::json!(period));
    b.goto(&link);
    b.wait(&shows("Usage"));
    // The period: 24 hours unless chosen; 7 days, then 30, each marked and kept in the address.
    b.wait(&on("24 hours"));
    b.click("nav.seg-links a", "7 days");
    b.wait(&format!("{} && new URLSearchParams(location.search).get('usage') === 'week'", on("7 days")));
    b.click("nav.seg-links a", "30 days");
    b.wait(&format!("{} && new URLSearchParams(location.search).get('usage') === 'month'", on("30 days")));
    // Runs and Jobs: one tab each, the run there and its job here.
    b.click("nav a", "Runs");
    b.wait(r#"document.querySelector('a.tab[aria-selected="true"]') && document.querySelector('a.tab[aria-selected="true"]').textContent === 'Runs'"#);
    b.wait(&shows("ci.yml"));
    b.click("a.tab", "Jobs");
    b.wait(r#"document.querySelector('a.tab[aria-selected="true"]') && document.querySelector('a.tab[aria-selected="true"]').textContent === 'Jobs'"#);
    b.wait(&shows("tests"));
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_job_whose_spot_machine_aws_took_back_is_started_again_off_spot_and_the_run_goes_on() {
    let served = serve(|_| {});
    let dir = checkout();
    let cli = superci(&served, &dir, &["runs", "create", "--workflow=.github/workflows/ci.yml", "--quiet"]).spawn().unwrap();
    let run = run_of(&served);
    let first = loop { if let Some(r) = Runner::of_run(&served, &run) { break r } std::thread::sleep(std::time::Duration::from_millis(50)) };
    let token = |served: &superci_testkit::Served| served.store.under(&format!("run:{run}"))[0].1["jobs"][0]["token"].as_str().unwrap_or_default().to_string();
    let was = token(&served);
    first.take();
    first.steps(&[("eeeeeeee-0000-0000-0000-000000000005", "Run make", 1, "inProgress", None, None)]);
    // AWS takes its spot machine back (its machine said so, with its link: here, noted on its job), and the runner
    // ends the job as failed.
    let key = served.store.under("job:").into_iter().map(|(k, _)| k).find(|k| !k.contains(':') || k.starts_with("job:")).unwrap();
    { let mut m = served.store.0.lock().unwrap(); let mut j: serde_json::Value = serde_json::from_str(&m[&key]).unwrap(); j["interrupted"] = true.into(); m.insert(key.clone(), j.to_string()); }
    first.finish("failed");
    // Started again, on a machine of its own off spot machines: a new job of the run, and the first said so.
    let end = std::time::Instant::now() + std::time::Duration::from_secs(15);
    while token(&served) == was || token(&served).is_empty() { assert!(std::time::Instant::now() < end, "not started again: {}", served.store.under(&format!("run:{run}"))[0].1); std::thread::sleep(std::time::Duration::from_millis(50)) }
    let first_job = served.store.under(&key)[0].1.clone();
    assert!(first_job["error"].as_str().unwrap_or_default().contains("started again, off spot machines") && first_job.get("rerun").is_none(), "{first_job}");
    let again = served.store.under("job:").into_iter().find(|(k, _)| *k != key).map(|(_, j)| j).unwrap();
    assert_eq!(again["no_spot"], true, "{again}");
    play(&served, &run, &["ok".to_string()], "succeeded");
    let out = cli.wait_with_output().unwrap();
    let said = String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success() && said.contains("The run ended: success"), "{said}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_job_taken_back_that_succeeded_anyway_is_not_started_again_nor_one_started_again_already() {
    let served = serve(|_| {});
    let dir = checkout();
    let interrupted = |served: &superci_testkit::Served, job: &str| { let mut m = served.store.0.lock().unwrap(); let mut j: serde_json::Value = serde_json::from_str(&m[job]).unwrap(); j["interrupted"] = true.into(); m.insert(job.to_string(), j.to_string()); };
    let jobs_of = |served: &superci_testkit::Served, run: &str| served.store.under("job:").into_iter().filter(|(_, j)| j["run_id"].as_u64().is_some_and(|r| r.to_string() == run)).map(|(k, _)| k).collect::<Vec<_>>();
    // Its machine taken back, but the job had succeeded: the run is over, a success, with that one job.
    let cli = superci(&served, &dir, &["runs", "create", "--workflow=.github/workflows/ci.yml", "--quiet"]).spawn().unwrap();
    let run = run_of(&served);
    let first = loop { if let Some(r) = Runner::of_run(&served, &run) { break r } std::thread::sleep(std::time::Duration::from_millis(50)) };
    first.take();
    interrupted(&served, &jobs_of(&served, &run)[0]);
    first.finish("succeeded");
    let said = String::from_utf8_lossy(&cli.wait_with_output().unwrap().stdout).into_owned();
    assert!(said.contains("The run ended: success") && jobs_of(&served, &run).len() == 1, "{said}");
    // Started again off spot once, and taken back again (an on-demand machine is not, but its record may say so): it
    // fails, not a third time.
    let cli = superci(&served, &dir, &["runs", "create", "--workflow=.github/workflows/ci.yml", "--quiet"]).spawn().unwrap();
    let second = loop { if let Some((k, _)) = served.store.under("run:").into_iter().find(|(k, _)| *k != format!("run:{run}")) { break k.trim_start_matches("run:").to_string() } std::thread::sleep(std::time::Duration::from_millis(50)) };
    let runner = loop { if let Some(r) = Runner::of_run(&served, &second) { break r } std::thread::sleep(std::time::Duration::from_millis(50)) };
    runner.take();
    interrupted(&served, &jobs_of(&served, &second)[0]);
    runner.finish("failed");
    let end = std::time::Instant::now() + std::time::Duration::from_secs(15);
    while jobs_of(&served, &second).len() < 2 { assert!(std::time::Instant::now() < end, "not started again"); std::thread::sleep(std::time::Duration::from_millis(50)) }
    let again = loop { if let Some(r) = Runner::of_run(&served, &second) { if r.at != runner.at { break r } } std::thread::sleep(std::time::Duration::from_millis(50)) };
    again.take();
    let off_spot = jobs_of(&served, &second).into_iter().find(|k| served.store.under(k)[0].1["no_spot"] == true).unwrap();
    interrupted(&served, &off_spot);
    again.finish("failed");
    let out = cli.wait_with_output().unwrap();
    let said = String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
    assert!(said.contains("The run ended: failure") && jobs_of(&served, &second).len() == 2, "{} jobs; {}\n{said}", jobs_of(&served, &second).len(), served.store.under(&format!("run:{second}"))[0].1);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_run_from_a_computer_is_cancelled_its_running_job_told_to_stop_the_job_waiting_for_it_never_started_and_an_always_job_runs() {
    let served = serve(|_| {});
    let dir = checkout_with(&[(".github/workflows/ci.yml", "jobs:\n  build:\n    runs-on: superci\n    steps:\n      - run: sleep 600\n  after:\n    needs: build\n    runs-on: superci\n    steps:\n      - run: echo after\n  report:\n    needs: build\n    if: always()\n    runs-on: superci\n    steps:\n      - run: echo report\n")]);
    let o = superci(&served, &dir, &["runs", "create", "--workflow=.github/workflows/ci.yml", "--detach"]).output().unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let run = run_of(&served);
    let build = runner_of(&served, &run, "build");
    let message = build.handed(&served);
    build.take();
    build.steps(&[("ffffffff-0000-0000-0000-000000000006", "Run sleep 600", 1, "inProgress", None, None)]);
    // Cancelled, with the key that started it.
    let o = superci(&served, &dir, &["runs", "cancel", &run]).output().unwrap();
    let said = String::from_utf8_lossy(&o.stdout).to_string() + &String::from_utf8_lossy(&o.stderr);
    assert!(o.status.success() && said.contains(&format!("Cancelled run {run}")), "{said}");
    // The running one's runner is told, as GitHub tells it, once; the jobs waiting for it wait on.
    let jobs = || served.store.under(&format!("run:{run}"))[0].1["jobs"].clone();
    let of = |key: &str| jobs().as_array().unwrap().iter().find(|j| j["key"] == key).cloned().unwrap();
    assert_eq!(of("after")["state"], "pending");
    let (status, told) = build.message();
    assert_eq!((status, told["messageType"].as_str()), (200, Some("JobCancellation")), "{told}");
    let body: serde_json::Value = serde_json::from_str(told["body"].as_str().unwrap()).unwrap();
    assert_eq!((body["jobId"].clone(), body["timeout"].as_str()), (message["jobId"].clone(), Some("00:05:00")));
    assert_eq!(build.message().0, 204, "told once");
    // It stops its steps and ends cancelled. As on GitHub, the job that waited for it ends cancelled, never started,
    // and the one whose `if` holds after a cancel (`always()`) runs; then the run is over, cancelled.
    build.finish("canceled");
    let after = of("after");
    assert_eq!((after["state"].as_str(), after["result"].as_str(), after.get("token")), (Some("done"), Some("cancelled"), None), "{after}");
    let report = runner_of(&served, &run, "report");
    report.take();
    report.finish("succeeded");
    let o = superci(&served, &dir, &["runs", "wait", &run, "--quiet"]).output().unwrap();
    let said = String::from_utf8_lossy(&o.stdout).to_string() + &String::from_utf8_lossy(&o.stderr);
    assert!(said.contains("The run ended: cancelled"), "{said}");
    assert_eq!(superci(&served, &dir, &["runs", "cancel", &run]).output().map(|o| String::from_utf8_lossy(&o.stdout).contains("was over already")).ok(), Some(true));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_run_cancelled_before_its_machine_began_its_job_ends_that_machine_at_once() {
    let served = serve(|_| {});
    let dir = checkout();
    let o = superci(&served, &dir, &["runs", "create", "--workflow=.github/workflows/ci.yml", "--detach"]).output().unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let run = run_of(&served);
    let _ = runner_of(&served, &run, "tests");
    let o = superci(&served, &dir, &["runs", "cancel", &run]).output().unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let job = served.store.under("job:").into_iter().map(|(_, j)| j).find(|j| j["provider"] == "local").unwrap();
    assert_eq!((job["state"].as_str(), job["ended_ms"].is_u64()), (Some("cancelled"), true), "{job}");
    let jobs = served.store.under(&format!("run:{run}"))[0].1["jobs"].clone();
    assert_eq!((jobs[0]["state"].as_str(), jobs[0]["result"].as_str()), (Some("done"), Some("cancelled")), "{jobs}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_running_run_is_cancelled_from_its_page_in_the_browser() {
    let Some(mut b) = Browser::open(1300, 900) else { eprintln!("no Chrome here: skipped"); return };
    let served = serve(|_| {});
    let dir = checkout();
    let o = superci(&served, &dir, &["runs", "create", "--workflow=.github/workflows/ci.yml", "--detach"]).output().unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let run = run_of(&served);
    let runner = runner_of(&served, &run, "tests");
    runner.take();
    runner.steps(&[("abababab-0000-0000-0000-000000000007", "Run make", 1, "inProgress", None, None)]);
    let home = common::signed_in(&served);
    let (mut child, link) = common::dashboard(&home);
    let shows = |words: &str| format!("document.querySelector('main') && document.querySelector('main').innerText.includes({})", serde_json::json!(words));
    b.goto(&link);
    b.wait(&shows("Usage"));
    b.click("nav a", "Runs");
    b.wait(r#"document.querySelector('main a[href*="p=run&"]') !== null"#);
    b.eval(r#"document.querySelector('main a[href*="p=run&"]').click()"#);
    // Running: its Cancel; clicked, its runner is told to stop, as GitHub tells one.
    b.wait(r#"[...document.querySelectorAll('main button')].some(x => x.textContent === 'Cancel run')"#);
    b.eval(r#"[...document.querySelectorAll('main button')].find(x => x.textContent === 'Cancel run').click()"#);
    let end = std::time::Instant::now() + std::time::Duration::from_secs(15);
    let told = loop {
        let (status, m) = runner.message();
        if status == 200 { break m }
        assert!(std::time::Instant::now() < end, "its runner was not told");
    };
    assert_eq!(told["messageType"], "JobCancellation", "{told}");
    runner.finish("canceled");
    b.wait(&shows("Cancelled"));
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_called_workflows_required_input_not_given_is_said_and_nothing_runs() {
    let served = serve(|_| {});
    let dir = checkout_with(&[
        (".github/workflows/ci.yml", "jobs:\n  build:\n    uses: ./.github/workflows/build.yml\n    with:\n      fast: false\n"),
        (".github/workflows/build.yml", "on:\n  workflow_call:\n    inputs:\n      target:\n        type: string\n        required: true\n      fast:\n        type: boolean\n        default: true\njobs:\n  compile:\n    runs-on: superci\n    steps:\n      - run: make ${{ inputs.target }}\n"),
    ]);
    let o = superci(&served, &dir, &["runs", "create", "--workflow=.github/workflows/ci.yml", "--quiet"]).output().unwrap();
    let said = String::from_utf8_lossy(&o.stdout).to_string() + &String::from_utf8_lossy(&o.stderr);
    assert!(!o.status.success() && said.contains("the called workflow's input target is required, and not given"), "{said}");
    assert!(served.machines.0.lock().unwrap().is_empty(), "no machine started");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A plain GET on this computer (the control plane, or the stand-in for S3): its status, its `location`, its body.
fn fetch(url: &str) -> (u16, String, Vec<u8>) { http("GET", url, &[]) }

/// A request on this computer: its status, its `location`, its body.
fn http(method: &str, url: &str, body: &[u8]) -> (u16, String, Vec<u8>) {
    use std::io::{Read, Write};
    let u = url.trim_start_matches("http://");
    let (host, rest) = u.split_once('/').unwrap_or((u, ""));
    let mut s = std::net::TcpStream::connect(host).unwrap();
    s.write_all(format!("{method} /{rest} HTTP/1.1\r\nhost: {host}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n", body.len()).as_bytes()).unwrap();
    s.write_all(body).unwrap();
    let mut out = vec![];
    let _ = s.read_to_end(&mut out);
    let at = out.windows(4).position(|w| w == b"\r\n\r\n").unwrap_or(out.len());
    let head = String::from_utf8_lossy(&out[..at]).to_string();
    let location = head.lines().find_map(|l| l.strip_prefix("location: ").or_else(|| l.strip_prefix("Location: "))).unwrap_or_default().to_string();
    (head.get(9..12).and_then(|c| c.parse().ok()).unwrap_or(0), location, out.get(at + 4..).unwrap_or_default().to_vec())
}

/// What a gzipped tar holds, unpacked as text (its headers name its files; small files' text follows).
fn gunzipped(bytes: &[u8]) -> String {
    // (macOS tar pads what it gzips with zeros, which gzip warns of.)
    let mut gz = Command::new("gzip").arg("-dc").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
    { use std::io::Write; gz.stdin.take().unwrap().write_all(bytes).unwrap(); }
    String::from_utf8_lossy(&gz.wait_with_output().unwrap().stdout).replace('\0', " ")
}

#[test]
fn a_second_run_from_the_same_checkout_sends_only_what_changed_on_top_of_the_tree_kept_in_the_bucket() {
    // A control plane with its own AWS account, whose bucket is ready (its cache program is there), machines its containers.
    let served = serve(|c| {
        (c.aws_own, c.aws_own_creds) = (Some(("111111111111".into(), "us-east-1".into())), Some(superci_core::aws::Credentials { access_key_id: "ASIAX".into(), secret_access_key: "secret".into(), session_token: Some("tok".into()), expires_at_ms: u64::MAX }));
        c.routing.order = vec![superci_core::plane::Pool::of("cloudflare")];
    });
    let bucket = superci_core::cache::bucket(superci_testkit::PLANE, "us-east-1");
    served.github.put_object(&format!("{bucket}/{}", superci_core::cache::program_key("x86_64")), b"program");
    let big: String = (0..4000).map(|n| format!("line {n} of a file that stays as it is\n")).collect();
    let dir = checkout_with(&[(".github/workflows/ci.yml", "jobs:\n  tests:\n    runs-on: superci\n    steps:\n      - run: make test\n"), ("big.txt", &big), ("small.txt", "one\n"), ("gone.txt", "soon gone\n")]);
    assert!(Command::new("git").current_dir(&dir).args(["remote", "add", "origin", "git@github.com:acme/app.git"]).status().unwrap().success());
    let home = common::temp("runs-base");
    let run = || {
        let o = superci(&served, &dir, &["runs", "create", "--workflow=.github/workflows/ci.yml", "--detach"]).env("SUPERCI_HOME", &home).env("SUPERCI_STAND_IN", &served.aws.url).output().unwrap();
        let said = String::from_utf8_lossy(&o.stdout).to_string() + &String::from_utf8_lossy(&o.stderr);
        assert!(o.status.success(), "{said}");
        // ("Run ID of … started.")
        let id = said.lines().find_map(|l| l.strip_prefix("Run ")).and_then(|l| l.split_whitespace().next()).unwrap_or_default().to_string();
        (said, id)
    };
    // The first: all of it, into the bucket, kept there as the base for the next.
    let (said, first) = run();
    assert!(said.contains("Sending the working tree (") && !said.contains("part"), "{said}");
    let kept: Vec<String> = served.github.objects().into_iter().map(|(k, _)| k).filter(|k| k.contains("/bases/")).collect();
    assert_eq!(kept.len(), 1, "{:?}", served.github.objects());
    // The run is of the repository its checkout's origin is on GitHub.
    assert_eq!(served.store.under(&format!("run:{first}"))[0].1["repository"], "acme/app");
    // The second, after a file changed and one removed: only those, on top of the base.
    std::fs::write(dir.join("small.txt"), "two\n").unwrap();
    std::fs::remove_file(dir.join("gone.txt")).unwrap();
    let (said, second) = run();
    assert!(said.contains("Sending what changed in the working tree (1 file, 1 removed,"), "{said}");
    // Its machine's checkout: the base first, then what changed and what was removed.
    let runner = loop { if let Some(r) = Runner::of_job(&served, &second, "tests") { break r } std::thread::sleep(std::time::Duration::from_millis(50)) };
    let tree = runner.at.trim_end_matches("/_apis/v1").to_string() + "/superci/tree";
    let part = |n: u32| {
        let (status, link, _) = fetch(&format!("{tree}?part={n}"));
        assert_eq!(status, 302, "part {n}");
        let (status, _, bytes) = fetch(&format!("{}/{}", served.aws.url, link.trim_start_matches("https://")));
        assert_eq!(status, 200, "{link}");
        gunzipped(&bytes)
    };
    let (base, changed) = (part(0), part(1));
    // (Sent while its machine started: the run was made first, and its machine told to wait until the computer said
    // all of it was there.)
    assert!(served.store.under("rtree-later:").is_empty(), "the computer said it was all sent");
    let token = runner.at.trim_end_matches("/_apis/v1").rsplit('/').next().unwrap().to_string();
    let handed = served.store.under(&format!("rjob:{token}"))[0].1.clone();
    let tree_id = handed["tree"].as_str().unwrap().to_string();
    let at = served.store.under(&format!("rtree-at:{tree_id}"))[0].1.to_string().trim_matches('"').to_string();
    served.store.0.lock().unwrap().insert(format!("rtree-later:{tree_id}"), at);
    assert_eq!(fetch(&format!("{tree}?part=0")).0, 425, "still on its way");
    served.store.0.lock().unwrap().remove(&format!("rtree-later:{tree_id}"));
    assert!(base.contains("big.txt") && base.contains("soon gone") && base.contains("one\n"), "the base as it was");
    assert!(changed.contains("small.txt") && changed.contains("two\n") && !changed.contains("big.txt") && changed.contains(".superci-removed") && changed.contains("gone.txt\n"), "{changed}");
    // Most of the tree changed: all of it again, kept as the next base.
    std::fs::write(dir.join("big.txt"), big.replace("stays", "moved")).unwrap();
    let (said, _) = run();
    assert!(said.contains("Sending the working tree (") && !said.contains("what changed"), "{said}");
    // The base kept gone from the bucket (its own rule removes old ones): all of it again.
    for (k, _) in served.github.objects().into_iter().filter(|(k, _)| k.contains("/bases/")) { served.github.remove_object(&k) }
    std::fs::write(dir.join("small.txt"), "three\n").unwrap();
    let (said, _) = run();
    assert!(said.contains("Sending the working tree (") && !said.contains("what changed"), "{said}");
    // Two changed since that base, nothing removed.
    std::fs::write(dir.join("small.txt"), "four\n").unwrap();
    std::fs::write(dir.join("new.txt"), "new\n").unwrap();
    let (said, _) = run();
    assert!(said.contains("Sending what changed in the working tree (2 files, 0 removed,"), "{said}");
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_working_tree_the_bucket_refused_cancels_its_run_and_says_why() {
    let served = serve(|c| {
        (c.aws_own, c.aws_own_creds) = (Some(("111111111111".into(), "us-east-1".into())), Some(superci_core::aws::Credentials { access_key_id: "ASIAX".into(), secret_access_key: "secret".into(), session_token: Some("tok".into()), expires_at_ms: u64::MAX }));
        c.routing.order = vec![superci_core::plane::Pool::of("cloudflare")];
    });
    let bucket = superci_core::cache::bucket(superci_testkit::PLANE, "us-east-1");
    served.github.put_object(&format!("{bucket}/{}", superci_core::cache::program_key("x86_64")), b"program");
    served.github.refuse_puts(true);
    let dir = checkout();
    let o = superci(&served, &dir, &["runs", "create", "--workflow=.github/workflows/ci.yml", "--detach"]).env("SUPERCI_STAND_IN", &served.aws.url).output().unwrap();
    let said = String::from_utf8_lossy(&o.stdout).to_string() + &String::from_utf8_lossy(&o.stderr);
    let run = run_of(&served);
    assert!(!o.status.success() && said.contains("The working tree was not sent (the bucket said 403") && said.contains(&format!("run {run} was cancelled")), "{said}");
    assert_eq!(served.store.under(&format!("run:{run}"))[0].1["cancelled"], true);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn what_a_run_from_a_computer_cannot_start_or_follow_is_said() {
    let served = serve(|_| {});
    let said = |dir: &std::path::Path, args: &[&str]| { let o = superci(&served, dir, args).output().unwrap(); (o.status.success(), String::from_utf8_lossy(&o.stdout).to_string() + &String::from_utf8_lossy(&o.stderr)) };
    // Not in a git checkout.
    let bare = common::temp("runs-bare");
    std::fs::create_dir_all(bare.join(".github/workflows")).unwrap();
    std::fs::write(bare.join(".github/workflows/ci.yml"), "jobs:\n  t:\n    runs-on: superci\n    steps:\n      - run: make\n").unwrap();
    let (ok, out) = said(&bare, &["runs", "create", "--workflow=.github/workflows/ci.yml"]);
    assert!(!ok && out.contains("Run it in a git checkout"), "{out}");
    // A workflow that is not there; one whose job calls a workflow that is not there.
    let dir = checkout_with(&[(".github/workflows/ci.yml", "jobs:\n  build:\n    uses: ./.github/workflows/build.yml\n  again:\n    uses: ./.github/workflows/build.yml\n  other:\n    uses: acme/shared/.github/workflows/x.yml@main\n")]);
    let (ok, out) = said(&dir, &["runs", "create", "--workflow=.github/workflows/nope.yml"]);
    assert!(!ok && out.contains(".github/workflows/nope.yml:"), "{out}");
    let (ok, out) = said(&dir, &["runs", "create", "--workflow=.github/workflows/ci.yml"]);
    assert!(!ok && out.contains(".github/workflows/build.yml, which a job calls:"), "{out}");
    assert!(served.machines.0.lock().unwrap().is_empty(), "no machine started");
    // Following: a run named wrong, and one that is not there.
    let (ok, out) = said(&dir, &["runs", "wait", "abc"]);
    assert!(!ok && out.contains("Say which run: `superci runs wait ID`"), "{out}");
    let (ok, out) = said(&dir, &["runs", "wait", "999"]);
    assert!(!ok && out.contains("No run 999 among this control plane's runs from a computer"), "{out}");
    // Its artifacts: of a run that is not there; one a run does not have.
    let (ok, out) = said(&dir, &["artifacts", "list", "--run=999"]);
    assert!(!ok && out.contains("No such run among this control plane's runs from a computer"), "{out}");
    let real = checkout();
    let o = superci(&served, &real, &["runs", "create", "--workflow=.github/workflows/ci.yml", "--detach"]).output().unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let run = run_of(&served);
    let (ok, out) = said(&real, &["artifacts", "list", &format!("--run={run}")]);
    assert!(ok && out.contains("No artifacts"), "{out}");
    let (ok, out) = said(&real, &["artifacts", "retrieve", "report", &format!("--run={run}")]);
    assert!(!ok && out.contains("The run has no artifact report."), "{out}");
    // A run with no jobs here, asked for its job; a run cancelled that is not here.
    let (ok, out) = said(&real, &["jobs", "retrieve", "--run=999"]);
    assert!(!ok && out.contains("No jobs of run 999 among the latest"), "{out}");
    let (ok, out) = said(&real, &["jobs", "retrieve", "--run=999", "--job=tests"]);
    assert!(!ok && out.contains("No jobs of run 999 among the latest"), "{out}");
    let (ok, out) = said(&real, &["runs", "cancel", "999"]);
    assert!(!ok && out.contains("No run 999 here"), "{out}");
    // A control plane's address that is not one, and a key it does not take.
    let o = superci(&served, &real, &["jobs", "list"]).env("SUPERCI_PLANE", "ftp://plane.example").output().unwrap();
    assert!(!o.status.success() && String::from_utf8_lossy(&o.stderr).contains("SUPERCI_PLANE is the control plane's address"), "{}", String::from_utf8_lossy(&o.stderr));
    let o = superci(&served, &real, &["jobs", "list"]).env("SUPERCI_KEY", "superci_read_not-a-key-of-its-own").output().unwrap();
    assert!(!o.status.success() && String::from_utf8_lossy(&o.stderr).contains("did not take SUPERCI_KEY"), "{}", String::from_utf8_lossy(&o.stderr));
    for d in [bare, dir, real] { let _ = std::fs::remove_dir_all(d); }
}

#[test]
fn a_steps_lines_that_came_late_are_printed_from_its_log_without_the_runners_times_and_others_as_they_are() {
    let served = serve(|_| {});
    let dir = checkout();
    let cli = superci(&served, &dir, &["runs", "create", "--workflow=.github/workflows/ci.yml"]).spawn().unwrap();
    let run = run_of(&served);
    let runner = loop { if let Some(r) = Runner::of_run(&served, &run) { break r } std::thread::sleep(std::time::Duration::from_millis(50)) };
    let a = "dddddddd-0000-0000-0000-000000000004";
    runner.take();
    runner.steps(&[(a, "Run make test", 1, "inProgress", None, None)]);
    runner.lines(a, 1, &["first".to_string()]);
    std::thread::sleep(std::time::Duration::from_millis(1800));
    // Its whole log: the first line with the runner's time (and the mark a log begins with), then lines that only
    // look like times: digits with no dash, a date with no time, letters, a time with nothing after it near.
    let log = ["\u{feff}2026-10-15T10:00:01.0000000Z first", "12345678901234567890123 counted", "2026-10-15 a date, no time, then more", "abcd-ef-ghTxxxxxxxxxxxx letters", "2026-10-15T10:00:02.0000000Zno space after the time at all here", "short"].join("\n");
    runner.log(1, &log);
    runner.steps(&[(a, "Run make test", 1, "completed", Some("succeeded"), Some(1))]);
    runner.finish("succeeded");
    let out = cli.wait_with_output().unwrap();
    let said = String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{said}");
    for l in ["12345678901234567890123 counted", "2026-10-15 a date, no time, then more", "abcd-ef-ghTxxxxxxxxxxxx letters", "2026-10-15T10:00:02.0000000Zno space after the time at all here", "short"] {
        assert!(said.lines().any(|s| s.trim_end().ends_with(l)), "{l:?} as it is in:\n{said}");
    }
    assert_eq!(said.lines().filter(|s| s.trim_end().ends_with("first")).count(), 1, "{said}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A zip as `upload-artifact` makes one (stored), of these files.
fn zip_of(files: &[(&str, &[u8])]) -> Vec<u8> {
    let (mut out, mut dir) = (vec![], vec![]);
    for (name, data) in files {
        let at = out.len() as u32;
        let (n, size) = (name.len() as u16, data.len() as u32);
        out.extend([0x50, 0x4b, 0x03, 0x04, 20, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        out.extend(size.to_le_bytes()); out.extend(size.to_le_bytes()); out.extend(n.to_le_bytes()); out.extend(0u16.to_le_bytes());
        out.extend(name.as_bytes()); out.extend(*data);
        dir.extend([0x50, 0x4b, 0x01, 0x02, 20, 0, 20, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        dir.extend(size.to_le_bytes()); dir.extend(size.to_le_bytes()); dir.extend(n.to_le_bytes());
        dir.extend([0u8; 12]); dir.extend(at.to_le_bytes()); dir.extend(name.as_bytes());
    }
    let (cd_at, cd_len, count) = (out.len() as u32, dir.len() as u32, files.len() as u16);
    out.extend(dir);
    out.extend([0x50, 0x4b, 0x05, 0x06, 0, 0, 0, 0]);
    out.extend(count.to_le_bytes()); out.extend(count.to_le_bytes()); out.extend(cd_len.to_le_bytes()); out.extend(cd_at.to_le_bytes()); out.extend([0, 0]);
    out
}

#[test]
fn a_run_from_a_computers_job_saves_and_restores_its_cache_keeps_artifacts_and_they_are_fetched_from_the_computer() {
    // A control plane with its own AWS account and its bucket ready; the job's machine given its link to the cache.
    let served = serve(|c| {
        (c.aws_own, c.aws_own_creds) = (Some(("111111111111".into(), "us-east-1".into())), Some(superci_core::aws::Credentials { access_key_id: "ASIAX".into(), secret_access_key: "secret".into(), session_token: Some("tok".into()), expires_at_ms: u64::MAX }));
        c.routing.order = vec![superci_core::plane::Pool::of("cloudflare")];
    });
    let bucket = superci_core::cache::bucket(superci_testkit::PLANE, "us-east-1");
    served.github.put_object(&format!("{bucket}/{}", superci_core::cache::program_key("x86_64")), b"program");
    let dir = checkout();
    let o = superci(&served, &dir, &["runs", "create", "--workflow=.github/workflows/ci.yml", "--detach"]).env("SUPERCI_STAND_IN", &served.aws.url).output().unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let run = run_of(&served);
    let runner = loop { if let Some(r) = Runner::of_run(&served, &run) { break r } std::thread::sleep(std::time::Duration::from_millis(50)) };
    let token = runner.handed(&served)["resources"]["endpoints"][0]["authorization"]["parameters"]["AccessToken"].as_str().unwrap().to_string();
    let machine = served.machines.0.lock().unwrap()[0].clone();
    let link = "machine-cache-link-000000000001";
    served.store.0.lock().unwrap().insert(format!("cachem:{link}"), serde_json::json!({ "region": "us-east-1", "arch": "x86_64", "until": u64::MAX / 4, "runner": machine }).to_string());
    let cache = |what: &str, body: serde_json::Value| { let (s, _, b) = http("POST", &format!("{}/cache/{link}/{what}", served.url), body.to_string().as_bytes()); (s, serde_json::from_slice::<serde_json::Value>(&b).unwrap_or_default()) };
    let s3 = |method: &str, url: &str, body: &[u8]| http(method, &format!("{}/{}", served.aws.url, url.trim_start_matches("https://")), body);
    let version = "a".repeat(64);
    // Its program, from the bucket.
    let (status, to, _) = fetch(&format!("{}/cache/{link}/program", served.url));
    assert!(status == 302 && to.contains(&bucket) && to.contains("cache-"), "{status} {to}");
    assert_eq!(fetch(&format!("{}/cache/not-a-machine-link-0000/program", served.url)).0, 404);
    // Nothing kept yet; then a save: begun, its piece's link, put there, finished.
    assert_eq!(cache("lookup", serde_json::json!({ "token": token, "key": "deps-1", "restore_keys": ["deps-"], "version": version })).1["hit"], false);
    assert_eq!(cache("begin", serde_json::json!({ "token": token, "key": "deps-1", "version": version })).1["ok"], true);
    let (status, said) = cache("begin", serde_json::json!({ "token": token, "key": "deps-1", "version": version }));
    assert_eq!((status, said["why"].as_str()), (409, Some("exists")), "{said}");
    assert_eq!(cache("begin", serde_json::json!({ "token": token, "key": "a,b", "version": version })).0, 400);
    for names in [serde_json::json!([]), serde_json::json!(["bad/name"]), serde_json::json!((0..65).map(|n| format!("p{n}")).collect::<Vec<_>>())] {
        assert_eq!(cache("sign", serde_json::json!({ "token": token, "key": "deps-1", "version": version, "names": names })).0, 400, "{names}");
    }
    assert_eq!(cache("sign", serde_json::json!({ "token": token, "key": "never-begun", "version": version, "names": ["p00000"] })).0, 404);
    let (_, signed) = cache("sign", serde_json::json!({ "token": token, "key": "deps-1", "version": version, "names": ["p00000"] }));
    assert_eq!(s3("PUT", signed["urls"][0].as_str().unwrap(), b"node_modules, packed").0, 200);
    assert_eq!(cache("finish", serde_json::json!({ "token": token, "key": "deps-1", "version": version, "pieces": [["p00000", 20]] })).1["size"], 20);
    // Restored by its key's beginning, and read back.
    let (_, hit) = cache("lookup", serde_json::json!({ "token": token, "key": "deps-2", "restore_keys": ["deps-"], "version": version }));
    assert_eq!((hit["hit"].as_bool(), hit["key"].as_str()), (Some(true), Some("deps-1")), "{hit}");
    assert_eq!(s3("GET", hit["pieces"][0]["url"].as_str().unwrap(), b"").2, b"node_modules, packed");
    // Without a job's token: refused; a token of no one's: refused.
    let (status, said) = cache("lookup", serde_json::json!({ "key": "deps-1", "version": version }));
    assert_eq!((status, said["ok"].as_bool()), (403, Some(false)), "{said}");
    assert_eq!(cache("lookup", serde_json::json!({ "token": "eyJhbGciOiJFUzI1NiJ9.e30.c2ln", "key": "deps-1", "version": version })).0, 403);
    assert_eq!(http("POST", &format!("{}/cache/not-a-machine-link-0000/lookup", served.url), b"{}").0, 404);
    // A tool's outputs (Turbo's), by the machine's job, with no token: one piece, then two more, and read back.
    assert_eq!(cache("tool-begin", serde_json::json!({ "tool": "make", "hash": "abc" })).0, 400);
    let hash = "b".repeat(64);
    assert_eq!(cache("tool-get", serde_json::json!({ "tool": "turbo", "hash": hash })).1["hit"], false);
    let (_, begun) = cache("tool-begin", serde_json::json!({ "tool": "turbo", "hash": hash }));
    assert_eq!(s3("PUT", begun["url"].as_str().unwrap(), b"dist ").0, 200);
    let (_, more) = cache("tool-sign", serde_json::json!({ "tool": "turbo", "hash": hash, "names": ["p00001"] }));
    assert_eq!(s3("PUT", more["urls"][0].as_str().unwrap(), b"files").0, 200);
    assert_eq!(cache("tool-sign", serde_json::json!({ "tool": "turbo", "hash": hash, "names": ["../x"] })).0, 400);
    assert_eq!(cache("tool-sign", serde_json::json!({ "tool": "turbo", "hash": hash, "names": [] })).0, 400);
    assert_eq!(cache("tool-sign", serde_json::json!({ "tool": "turbo", "hash": hash, "names": (0..65).map(|n| format!("p{n:05}")).collect::<Vec<_>>() })).0, 400);
    assert_eq!(cache("tool-finish", serde_json::json!({ "tool": "turbo", "hash": hash, "pieces": [["p00000", 5], ["p00001", 5]], "meta": "{\"duration\":12}" })).1["size"], 10);
    assert_eq!(cache("tool-begin", serde_json::json!({ "tool": "turbo", "hash": hash })).0, 409);
    assert_eq!(cache("tool-finish", serde_json::json!({ "tool": "nx", "hash": hash, "size": 3 })).0, 404);
    let (_, got) = cache("tool-get", serde_json::json!({ "tool": "turbo", "hash": hash }));
    assert_eq!((got["size"].as_u64(), got["meta"].as_str()), (Some(10), Some("{\"duration\":12}")), "{got}");
    let back: Vec<u8> = got["pieces"].as_array().unwrap().iter().flat_map(|p| s3("GET", p["url"].as_str().unwrap(), b"").2).collect();
    assert_eq!(back, b"dist files");
    // Its artifacts: kept in the run (a zip, a file that is not one, a zip naming a place outside its folder).
    let keep = |name: &str, bytes: &[u8]| {
        assert_eq!(cache("artifact-begin", serde_json::json!({ "token": token, "name": name })).1["ok"], true, "{name}");
        let (_, signed) = cache("artifact-sign", serde_json::json!({ "token": token, "name": name, "names": ["p00000"] }));
        assert_eq!(s3("PUT", signed["urls"][0].as_str().unwrap(), bytes).0, 200);
        cache("artifact-finish", serde_json::json!({ "token": token, "name": name, "pieces": [["p00000", bytes.len()]] })).1
    };
    let report = zip_of(&[("index.html", b"<h1>all passed</h1>"), ("data/results.json", b"{\"passed\":3}")]);
    assert!(keep("report", &report)["id"].as_u64().is_some());
    keep("notes", b"plain words, not a zip");
    keep("evil", &zip_of(&[("../outside.txt", b"x")]));
    // Zips as other tools make them: a folder's own entry, a file deflated (one stored block), packed in a way not
    // known, a damaged directory, zip64's markers, a name from the root or with a drive, and two bytes only.
    let with_method = |method: u16, data: &[u8]| { let mut z = zip_of(&[("f.txt", data)]); z[8] = method as u8; let cd = z.len() - 22 - (46 + 5); z[cd + 10] = method as u8; z };
    let deflated = [&[1u8, 5, 0, 0xfa, 0xff][..], b"hello"].concat();
    let mut damaged = zip_of(&[("a.txt", b"a")]);
    let cd = damaged.len() - 22 - (46 + 5);
    damaged[cd + 2] = 0;
    let mut zip64 = zip_of(&[("a.txt", b"a")]);
    let end = zip64.len() - 22;
    zip64[end + 10] = 0xff; zip64[end + 11] = 0xff;
    for (name, bytes) in [("folders", zip_of(&[("site/", b""), ("site/a.html", b"<p>a</p>")])), ("deflated", with_method(8, &deflated)), ("odd", with_method(99, b"?????")),
        ("damaged", damaged), ("zip64", zip64), ("rooted", zip_of(&[("/etc/x", b"x")])), ("drive", zip_of(&[("c:x", b"x")])), ("tiny", b"PK".to_vec())] { keep(name, &bytes); }
    assert_eq!(cache("artifact-begin", serde_json::json!({ "token": token, "name": "report" })).0, 409);
    assert_eq!(cache("artifact-begin", serde_json::json!({ "token": token, "name": "a/b:c" })).0, 400);
    assert_eq!(cache("artifact-sign", serde_json::json!({ "token": token, "name": "none", "names": ["p00000"] })).0, 404);
    assert_eq!(cache("artifact-sign", serde_json::json!({ "token": token, "name": "report", "names": ["p00001"] })).0, 404, "(kept already)");
    assert_eq!(cache("artifact-begin", serde_json::json!({ "token": token, "name": "late" })).1["ok"], true);
    assert_eq!(cache("artifact-sign", serde_json::json!({ "token": token, "name": "late", "names": [] })).0, 400);
    assert_eq!(cache("artifact-sign", serde_json::json!({ "token": token, "name": "late", "names": ["a/b"] })).0, 400);
    assert_eq!(cache("artifact-sign", serde_json::json!({ "token": token, "name": "late", "names": (0..65).map(|n| format!("p{n:05}")).collect::<Vec<_>>() })).0, 400);
    // A runner's link too short, or with what a link has not: nothing there.
    for at in ["/runner/short/superci/tree", "/runner/aaaaaaaaaaaaaaaa%21%21/superci/tree"] { assert_eq!(fetch(&format!("{}{at}", served.url)).0, 404, "{at}") }
    assert_eq!(cache("artifact-finish", serde_json::json!({ "token": token, "name": "none", "pieces": [] })).0, 404);
    assert_eq!(cache("artifacts", serde_json::json!({ "token": token })).1["artifacts"].as_array().map(Vec::len), Some(11), "(the three, and the eight as other tools make them)");
    assert_eq!(cache("artifact-get", serde_json::json!({ "token": token, "name": "report" })).1["size"], report.len());
    assert_eq!(cache("artifact-get", serde_json::json!({ "token": token, "name": "none" })).0, 404);
    // From the computer: listed, and each fetched: the zip unpacked, the file kept as it is, the bad zip refused.
    let cli = |args: &[&str]| { let o = superci(&served, &dir, args).env("SUPERCI_STAND_IN", &served.aws.url).output().unwrap(); (o.status.success(), String::from_utf8_lossy(&o.stdout).to_string() + &String::from_utf8_lossy(&o.stderr)) };
    let (ok, said) = cli(&["artifacts", "list", &format!("--run={run}")]);
    assert!(ok && said.contains("report") && said.contains("notes") && said.contains("evil"), "{said}");
    let out = common::temp("artifact");
    let (ok, said) = cli(&["artifacts", "retrieve", "report", &format!("--run={run}"), &format!("--out={}", out.display())]);
    assert!(ok && said.contains("2 files"), "{said}");
    assert_eq!(std::fs::read_to_string(out.join("data/results.json")).unwrap(), "{\"passed\":3}");
    let (ok, said) = cli(&["artifacts", "retrieve", "notes", &format!("--run={run}"), &format!("--out={}", out.join("n").display())]);
    assert!(ok && said.contains("1 file") && std::fs::read_to_string(out.join("n/notes")).unwrap() == "plain words, not a zip", "{said}");
    let (ok, said) = cli(&["artifacts", "retrieve", "evil", &format!("--run={run}"), &format!("--out={}", out.join("e").display())]);
    assert!(!ok && said.contains("evil could not be unpacked: a name outside its folder: ../outside.txt"), "{said}");
    // In the dashboard: the run's page lists them, each a download (the zip as it was kept).
    let home = common::signed_in(&served);
    let repository = served.store.under(&format!("run:{run}"))[0].1["repository"].as_str().unwrap().to_string();
    let (mut child, link) = common::dashboard(&home);
    let page_at = format!("/?p=run&repo={}&id={run}&attempt=0&fragment=1", url_encoded(&repository));
    let page = (0..40).map(|_| { std::thread::sleep(std::time::Duration::from_millis(250)); common::part(&link, &page_at).1 }).find(|p| p.contains("Its artifacts")).unwrap_or_default();
    assert!(page.contains(&format!("/run/artifact?run={run}&amp;name=report")) && page.contains("name=notes"), "{}", common::words(&page));
    let (status, zip) = common::part(&link, &format!("/run/artifact?run={run}&name=report"));
    assert!(status == 200 && zip.contains("data/results.json") && zip.len() == report.len(), "{status} {}", zip.len());
    for bad in ["/run/artifact?run=x&name=report", "/run/artifact?run=1&name="] { assert_eq!(common::part(&link, bad).0, 400, "{bad}") }
    // One whose name a file's cannot have, downloaded under a name it can.
    keep("test results (unit)", &zip_of(&[("r.txt", b"ok")]));
    let agent: ureq::Agent = ureq::Agent::config_builder().http_status_as_error(false).build().into();
    let (base, key) = link.split_once("/?k=").unwrap();
    let port = base.rsplit(':').next().unwrap();
    let r = agent.get(&format!("{base}/run/artifact?run={run}&name=test%20results%20%28unit%29")).header("cookie", &format!("superci_local_{port}={key}")).header("sec-fetch-site", "same-origin").call().unwrap();
    let disposition = r.headers().get("content-disposition").and_then(|v| v.to_str().ok()).unwrap_or_default().to_string();
    assert!(disposition.contains("test_results__unit_.zip"), "{} {disposition}", r.status());
    let _ = child.kill();
    for (name, want, ok) in [("folders", "1 file", true), ("deflated", "1 file", true), ("tiny", "1 file", true), ("odd", "f.txt is packed in a way this does not know (99)", false), ("damaged", "a damaged directory", false),
        ("zip64", "over 4 GB (zip64)", false), ("rooted", "a name outside its folder: /etc/x", false), ("drive", "a name outside its folder: c:x", false)] {
        let to = out.join(name);
        let (done, said) = cli(&["artifacts", "retrieve", name, &format!("--run={run}"), &format!("--out={}", to.display())]);
        assert!(done == ok && said.contains(want), "{name}: {said}");
    }
    assert_eq!(std::fs::read_to_string(out.join("folders/site/a.html")).unwrap(), "<p>a</p>");
    assert_eq!(std::fs::read_to_string(out.join("deflated/f.txt")).unwrap(), "hello");
    assert_eq!(std::fs::read(out.join("tiny/tiny")).unwrap(), b"PK");
    // Deleted by the job; gone from the bucket.
    assert_eq!(cache("artifact-delete", serde_json::json!({ "token": token, "name": "notes" })).1["ok"], true);
    assert_eq!(cache("artifact-delete", serde_json::json!({ "token": token, "name": "notes" })).0, 404);
    assert_eq!(cache("nothing", serde_json::json!({ "token": token })).0, 404);
    // The run's repository's cache entries, and removed.
    let (_, listed) = common::json(&home, &["caches", "list"]);
    assert_eq!((listed["repos"][0]["name"].as_str(), listed["repos"][0]["entries"].as_u64()), (Some(repository.as_str()), Some(2)), "{listed}");
    let (_, entries) = common::json(&home, &["caches", "list", &format!("--repo={repository}")]);
    assert!(entries["entries"].as_array().unwrap().iter().any(|e| e["key"] == "deps-1"), "{entries}");
    let o = common::superci(&home, &["caches", "delete", &format!("--repo={repository}"), "deps-1", "--confirm"]);
    assert!(o.status.success() && String::from_utf8_lossy(&o.stdout).contains("deps-1 is out of"), "{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
    let (_, entries) = common::json(&home, &["caches", "list", &format!("--repo={repository}")]);
    assert!(!entries.to_string().contains("deps-1"), "{entries}");
    assert_eq!(cache("lookup", serde_json::json!({ "token": token, "key": "deps-1", "version": version })).1["hit"], false);
    // The job played to its end, its tests in its log: what they say, with the run's artifacts to fetch.
    let step = "eeeeeeee-0000-0000-0000-000000000005";
    runner.take();
    runner.steps(&[(step, "Run cargo test", 1, "inProgress", None, None)]);
    let lines: Vec<String> = (1..=40).map(|n| format!("test tests::case_{n} ... ok")).chain(["test result: ok. 40 passed; 0 failed; 2 ignored; 0 measured; 0 filtered out".to_string()]).collect();
    runner.lines(step, 1, &lines);
    runner.log(1, &lines.iter().map(|l| format!("2026-10-15T10:00:01.0000000Z {l}")).collect::<Vec<_>>().join("\n"));
    runner.steps(&[(step, "Run cargo test", 1, "completed", Some("succeeded"), Some(1))]);
    runner.finish("succeeded");
    let job = loop {
        let done = served.store.under("job:").into_iter().find(|(_, j)| j["run_id"].to_string() == run && j["state"] == "done");
        if let Some((_, j)) = done { break j["job_id"].to_string() }
        std::thread::sleep(std::time::Duration::from_millis(50));
    };
    let said = String::from_utf8_lossy(&common::superci(&home, &["jobs", "retrieve", &job, "--tests"]).stdout).into_owned();
    assert!(said.contains("Tests: 0 failed, 40 passed, 2 skipped (cargo)") && said.contains(&format!("Artifact report: `superci artifacts retrieve report --run={run}`")), "{said}");
    // Its log's end only, when asked for a few bytes; then the run, over, is not cancelled again.
    let said = String::from_utf8_lossy(&common::superci(&home, &["jobs", "retrieve", &job, "--bytes=100"]).stdout).into_owned();
    assert!(said.contains("The end of its log (") && said.contains("40 passed"), "{said}");
    let said = String::from_utf8_lossy(&common::superci(&home, &["runs", "cancel", &run]).stdout).into_owned();
    assert!(said.contains(&format!("Run {run} was over already.")), "{said}");
    // A day later the upkeep forgets the run: its jobs' messages, lines and logs, its artifacts (and their pieces
    // in the bucket), its working tree. The cache keeps its entries for their days.
    let artifacts_in_bucket = || served.github.objects().into_iter().filter(|(k, _)| k.contains(&format!("run-{run}"))).count();
    assert!(artifacts_in_bucket() > 0);
    served.later(2 * 86_400_000);
    served.upkeep();
    for prefix in [format!("run:{run}"), "rjob:".into(), "rline:".into(), "rlog:".into(), format!("artifact:{run}"), "rtree".into()] {
        assert!(served.store.under(&prefix).is_empty(), "{prefix} kept: {:?}", served.store.under(&prefix).iter().map(|(k, _)| k).collect::<Vec<_>>());
    }
    assert_eq!(artifacts_in_bucket(), 0, "{:?}", served.github.objects());
    let (ok, said) = cli(&["artifacts", "list", &format!("--run={run}")]);
    assert!(!ok && said.contains("No such run"), "{said}");
    // Unused past its days (7): gone, its pieces from the bucket too.
    let pieces = || served.github.objects().into_iter().filter(|(k, _)| !k.contains("/bases/") && !k.contains("superci/cache-") && !k.contains("run-")).count();
    assert!(pieces() > 0, "{:?}", served.github.objects());
    served.later(8 * 86_400_000);
    served.upkeep();
    assert_eq!(pieces(), 0, "{:?}", served.github.objects());
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&out);
}

fn url_encoded(s: &str) -> String { url::form_urlencoded::byte_serialize(s.as_bytes()).collect() }

#[test]
fn jobs_if_conditions_are_read_as_github_reads_its_expressions() {
    let served = serve(|_| {});
    // Each job runs when its condition holds as GitHub would have it: numbers in hex and with exponents, negatives,
    // text and booleans as numbers, `&&` and `||` giving a side's value, `join` and `format`, an id with a dash,
    // a filter over an object.
    let holds = [
        "0x10 == 16 && 1.5e2 == 150 && -2 < 0 && 2.5 > 2",
        "'' == 0 && true == 1 && ' ' == 0 && null == 0 && '0x1F' == 31",
        "(false || 'x') == 'x' && (0 && 'y') == 0 && !(null)",
        "join(fromJSON('[1,2]')) == '1,2' && join(fromJSON('[\"a\",\"b\"]'), '-') == 'a-b' && join('one') == 'one'",
        "format('{0}{1}', 3.0, 0.5) == '30.5' && format('{{x}}') == '{x}'",
        "contains(github.*, 'push') && github.event_name >= 'PUSH' && 'b' > 'A'",
        "fromJSON('{\"my-key\": 1e3}').my-key == 1000",
    ];
    let fails = ["0x10 == 15", "'a' && ''", "-1 > 0 || 'NaN' == 0", "1e400 < 0"];
    let mut wf = String::from("jobs:\n");
    // (One job for all that hold: each is read, as each holds. A job each for what does not.)
    wf += &format!("  yes:\n    if: >-\n      ${{{{ {} }}}}\n    runs-on: superci\n    steps:\n      - run: echo\n", holds.map(|c| format!("({c})")).join(" && "));
    for (i, c) in fails.iter().enumerate() { wf += &format!("  no-{i}:\n    if: >-\n      ${{{{ {c} }}}}\n    runs-on: superci\n    steps:\n      - run: echo\n") }
    let dir = checkout_with(&[(".github/workflows/ci.yml", &wf)]);
    let o = superci(&served, &dir, &["runs", "create", "--workflow=.github/workflows/ci.yml", "--detach"]).output().unwrap();
    assert!(o.status.success(), "{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
    let run = run_of(&served);
    let jobs = served.store.under(&format!("run:{run}"))[0].1["jobs"].clone();
    let result = |name: &str| jobs.as_array().unwrap().iter().find(|j| j["key"] == name).map(|j| (j["state"].as_str().unwrap_or_default().to_string(), j["result"].as_str().unwrap_or_default().to_string())).unwrap_or_default();
    let (state, ended) = result("yes");
    assert!(state == "started" && ended.is_empty(), "started: {jobs}");
    for (i, c) in fails.iter().enumerate() { assert_eq!(result(&format!("no-{i}")).1, "skipped", "{c}: {jobs}") }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_tree_too_large_for_one_request_goes_in_parts_two_jobs_may_call_one_workflow_and_a_file_not_yaml_is_said() {
    let served = serve(|_| {});
    // 4 MB that does not pack smaller (random), through the control plane in parts of 3 MB.
    let mut noise = vec![0u8; 4_000_000];
    let mut x: u64 = 0x9e3779b97f4a7c15;
    for b in noise.iter_mut() { x ^= x << 13; x ^= x >> 7; x ^= x << 17; *b = x as u8 }
    let dir = checkout_with(&[
        (".github/workflows/ci.yml", "jobs:\n  web:\n    uses: ./.github/workflows/build.yml\n  api:\n    uses: ./.github/workflows/build.yml\n"),
        (".github/workflows/build.yml", "on:\n  workflow_call:\njobs:\n  compile:\n    runs-on: superci\n    steps:\n      - run: make\n"),
    ]);
    std::fs::write(dir.join("noise.bin"), &noise).unwrap();
    assert!(Command::new("git").current_dir(&dir).args(["add", "-A"]).status().unwrap().success());
    let o = superci(&served, &dir, &["runs", "create", "--workflow=.github/workflows/ci.yml", "--detach"]).output().unwrap();
    let said = String::from_utf8_lossy(&o.stdout).to_string() + &String::from_utf8_lossy(&o.stderr);
    assert!(o.status.success() && said.contains("2 parts)"), "{said}");
    let run = run_of(&served);
    let names: Vec<String> = served.store.under(&format!("run:{run}"))[0].1["jobs"].as_array().unwrap().iter().filter_map(|j| j["name"].as_str().map(str::to_string)).collect();
    assert!(names.contains(&"web / compile".to_string()) && names.contains(&"api / compile".to_string()), "{names:?}");
    // A workflow file that is not YAML: said by the control plane, nothing started.
    std::fs::write(dir.join(".github/workflows/bad.yml"), "jobs: [unclosed\n").unwrap();
    let o = superci(&served, &dir, &["runs", "create", "--workflow=.github/workflows/bad.yml"]).output().unwrap();
    let said = String::from_utf8_lossy(&o.stdout).to_string() + &String::from_utf8_lossy(&o.stderr);
    assert!(!o.status.success() && said.contains("the control plane said 400") && said.contains("not YAML"), "{said}");
    let _ = std::fs::remove_dir_all(&dir);
}

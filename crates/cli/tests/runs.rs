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
fn a_run_from_a_computer_is_cancelled_its_running_job_told_to_stop_and_the_job_waiting_for_it_never_started() {
    let served = serve(|_| {});
    let dir = checkout_with(&[(".github/workflows/ci.yml", "jobs:\n  build:\n    runs-on: superci\n    steps:\n      - run: sleep 600\n  after:\n    needs: build\n    runs-on: superci\n    steps:\n      - run: echo after\n")]);
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
    // The job that waited for it ends cancelled, never started; the running one's runner is told, as GitHub tells it,
    // once.
    let jobs = || served.store.under(&format!("run:{run}"))[0].1["jobs"].clone();
    let after = jobs().as_array().unwrap().iter().find(|j| j["key"] == "after").cloned().unwrap();
    assert_eq!((after["state"].as_str(), after["result"].as_str(), after.get("token")), (Some("done"), Some("cancelled"), None), "{after}");
    let (status, told) = build.message();
    assert_eq!((status, told["messageType"].as_str()), (200, Some("JobCancellation")), "{told}");
    let body: serde_json::Value = serde_json::from_str(told["body"].as_str().unwrap()).unwrap();
    assert_eq!((body["jobId"].clone(), body["timeout"].as_str()), (message["jobId"].clone(), Some("00:05:00")));
    assert_eq!(build.message().0, 204, "told once");
    // It stops its steps and ends cancelled: the run is over, cancelled.
    build.finish("canceled");
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

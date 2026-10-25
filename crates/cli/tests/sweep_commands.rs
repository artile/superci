//! The CLI's commands with each of their calls and store operations failing in turn (`superci_testkit::sweep`), as
//! a person or a coding agent runs them against a control plane served here. Whatever fails, no command panics, one
//! that fails says what failed (with `--json`, an object with "error"), the workspace's files stay readable, and a
//! plain `superci status` afterwards still works.
//!
//! Each command is swept by itself (what it needs done first is done before the fault is armed): a sweep's runs grow
//! with its calls times its length, so many short sweeps cost far less than one long one.
mod common;
use std::path::{Path, PathBuf};

use serde_json::Value;
use superci_testkit::{serve, sweep, Fault, Outcome, Served};

/// A control plane, the workspace that keeps it (its home) and the folder commands run in.
type Setup = (Served, PathBuf, PathBuf);

/// A served control plane and a workspace that keeps it, signed in to its AWS (the stand-in's): every outside call
/// the CLI makes goes to the stand-in, where it is counted.
fn plane() -> Setup {
    let served = serve(|_| {});
    let home = common::signed_in(&served);
    (served, home.clone(), home)
}

/// The same with a workspace that keeps only this computer's key (AWS's sign-in since ended), a run played on it.
fn plane_by_key() -> Setup {
    let served = serve(|_| {});
    let home = common::home(&served);
    std::fs::write(home.join("aws-at"), &served.aws.url).unwrap();
    played(&served);
    (served, home.clone(), home)
}

/// Signed in, a run played on it.
fn plane_played() -> Setup { let s = plane(); played(&s.0); s }

/// Signed in, its key not yet handed to the control plane (as on a computer that signed in again): each command finds
/// the control plane through AWS and hands it the key first, so a fault there stops the command where it stood.
fn plane_to_find() -> Setup {
    let s = plane_played();
    let file = s.1.join("workspaces/default/sign-ins.json");
    let mut kept: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    kept["key"]["planes"] = serde_json::json!([]);
    std::fs::write(&file, kept.to_string()).unwrap();
    s
}

/// One sweep at a time: each runs eight side by side already, and every command opens connections of its own (more at
/// once run this computer out of ports).
fn one_at_a_time() -> std::sync::MutexGuard<'static, ()> {
    static ONE: std::sync::Mutex<()> = std::sync::Mutex::new(());
    ONE.lock().unwrap_or_else(|e| e.into_inner())
}

/// What a command left: how it ended, what it printed.
struct Ran { args: Vec<String>, code: i32, out: String, err: String }

/// The CLI in workspace `home`, in the folder `dir`.
fn run_in(home: &Path, dir: &Path, args: &[&str], envs: &[(&str, &str)]) -> Ran {
    let o = common::command(home).current_dir(dir).args(args).envs(envs.iter().copied()).output().unwrap();
    Ran { args: args.iter().map(|a| a.to_string()).collect(), code: o.status.code().unwrap_or(-1), out: String::from_utf8_lossy(&o.stdout).into_owned(), err: String::from_utf8_lossy(&o.stderr).into_owned() }
}

/// What must hold of a command whatever failed: it ended by itself, with no panic; when it failed, it said what (as
/// JSON, an object saying "error", or a run's object with "ok": false).
fn held(out: &Outcome, r: &Ran) {
    let line = r.args.join(" ");
    out.check(r.code >= 0, || format!("`{line}` was killed: {}", r.err));
    out.check(!r.err.contains("panicked") && !r.err.contains("stack backtrace"), || format!("`{line}` panicked: {}", r.err));
    if r.code == 0 { return }
    let said = if r.args.iter().any(|a| a == "--json") {
        let v: Value = serde_json::from_str(&r.out).unwrap_or_default();
        // (A run that ended badly answers its own object, "ok": false, and what went wrong in "error".)
        v["error"].as_str().unwrap_or_default().to_string()
    } else { r.err.trim().trim_start_matches("superci:").trim().to_string() };
    out.check(said.trim().len() > 8, || format!("`{line}` failed ({}) without saying what: {}{}", r.code, r.out, r.err));
}

/// The workspace's files still read, and (once the fault has been met) a plain `superci status` works.
fn after(out: &Outcome, home: &Path, envs: &[(&str, &str)]) {
    for file in ["sign-ins.json", "workspace.json"] {
        if let Ok(text) = std::fs::read_to_string(home.join("workspaces/default").join(file)) {
            out.check(serde_json::from_str::<Value>(&text).is_ok(), || format!("{file} no longer reads: {text}"));
        }
    }
    let met = !out.served.faults().fired().is_empty();
    let r = run_in(home, home, &["status", "--json"], envs);
    held(out, &r);
    out.check(!met || r.code == 0, || format!("`superci status` fails after the fault: {}{}", r.out, r.err));
}

/// One run of a sweep: `before` done, then the fault armed and `command` run; counting stops before what is checked.
fn one(fault: Fault, (served, home, dir): Setup, envs: &[(&str, &str)], before: &[&[&str]], command: &[&str]) -> Outcome {
    for c in before { run_in(&home, &dir, c, envs); }
    served.arm(fault);
    let r = run_in(&home, &dir, command, envs);
    served.faults().stop();
    // (One fault looked into, `SUPERCI_SWEEP_ONLY`, or the clean runs with `SUPERCI_SWEEP_SAY`: what it said.)
    if std::env::var("SUPERCI_SWEEP_ONLY").is_ok() || (fault == Fault::None && std::env::var("SUPERCI_SWEEP_SAY").is_ok()) { eprintln!("$ superci {} → {}\n{}{}", r.args.join(" "), r.code, r.out, r.err) }
    let out = Outcome::new(served);
    held(&out, &r);
    after(&out, &home, envs);
    let _ = std::fs::remove_dir_all(&home);
    out
}

/// Each of `commands` swept by itself, in what `setup` makes, `before` done first in each run.
fn each(setup: impl Fn() -> Setup + Sync, envs: &[(&str, &str)], before: &[&[&str]], commands: &[&[&str]]) {
    for c in commands { sweep(&format!("superci {}", c.join(" ")), |fault| one(fault, setup(), envs, before, c)); }
}

/// A run of one job played to its end (from a checkout of its own), once: what the control plane kept of it, with the
/// address it was served at. Each run of a sweep is given a copy (`played`), not a run played again.
fn played_once() -> &'static (String, Vec<(String, String)>, Vec<(String, (String, u64, u64))>) {
    static ONCE: std::sync::OnceLock<(String, Vec<(String, String)>, Vec<(String, (String, u64, u64))>)> = std::sync::OnceLock::new();
    ONCE.get_or_init(|| {
        let served = serve(|_| {});
        let dir = common::temp("played");
        common::played(&served, &dir);
        let _ = std::fs::remove_dir_all(&dir);
        let store = served.store.0.lock().unwrap().iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        let params = served.aws.params.lock().unwrap().iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        (served.url.clone(), store, params)
    })
}

/// That run, as if played on this control plane.
fn played(served: &Served) {
    let (was, store, params) = played_once();
    let at = |v: &str| v.replace(was.as_str(), &served.url);
    served.store.0.lock().unwrap().extend(store.iter().map(|(k, v)| (k.clone(), at(v))));
    served.aws.params.lock().unwrap().extend(params.iter().map(|(k, (v, n, ms))| (k.clone(), (at(v), *n, *ms))));
}

/// The played run's id and its job's.
fn ids() -> (String, String) {
    let (_, store, _) = played_once();
    let job: Value = store.iter().find(|(k, _)| k.starts_with("job:")).map(|(_, v)| serde_json::from_str(v).unwrap()).unwrap();
    (job["run_id"].to_string(), job["job_id"].to_string())
}

#[test]
fn reading_jobs_and_runs_holds_with_each_call_and_store_operation_failing() {
    let _one = one_at_a_time();
    let (run, job) = ids();
    let out = format!("--out={}", common::temp("job-log").join("job.log").display());
    each(plane_by_key, &[], &[], &[
        &["status"], &["jobs", "list"], &["runs", "list", "--json"], &["usage", "retrieve", "--json"], &["jobs", "retrieve", &job], &["jobs", "retrieve", &job, "--tests", "--json"],
        &["jobs", "retrieve", &format!("--run={run}"), &out, "--json"], &["runs", "retrieve", &run, "--tests", "--json"],
    ]);
}

#[test]
fn reading_the_control_planes_settings_holds_with_each_call_and_store_operation_failing() {
    let _one = one_at_a_time();
    let (run, _) = ids();
    each(plane_by_key, &[], &[], &[
        &["deliveries", "list", "--json"], &["caches", "retrieve", "--json"], &["caches", "list", "--repo=acme/app", "--json"], &["keys", "list", "--json"], &["labels", "list", "--json"],
        &["name", "retrieve", "--json"], &["runners", "list", "--json"], &["connections", "list", "--json"], &["limits", "retrieve", "--json"], &["machine", "retrieve", "--json"],
        &["public_repos", "list", "--json"], &["artifacts", "list", &format!("--run={run}"), "--json"],
    ]);
}

/// A coding agent's: no workspace, a run key and the control plane's address given by name.
#[test]
fn reading_with_a_key_given_by_name_holds_with_each_call_and_store_operation_failing() {
    let _one = one_at_a_time();
    let (run, job) = ids();
    for c in [&["status", "--json"][..], &["jobs", "retrieve", &job, "--json"], &["runs", "retrieve", &run, "--tests", "--json"], &["caches", "retrieve", "--json"], &["artifacts", "list", &format!("--run={run}"), "--json"]] {
        sweep(&format!("superci {} with a key", c.join(" ")), |fault| {
            let (served, home, dir) = plane_by_key();
            let _ = std::fs::remove_dir_all(home.join("workspaces"));
            let (url, key) = (served.url.clone(), served.run_key.clone());
            one(fault, (served, home, dir), &[("SUPERCI_PLANE", &url), ("SUPERCI_KEY", &key)], &[], c)
        });
    }
}

/// Signed in to the control plane's AWS: a view that cannot be read finds the control plane again through AWS.
#[test]
fn reading_signed_in_holds_with_each_call_and_store_operation_failing() {
    let _one = one_at_a_time();
    let (run, _) = ids();
    each(plane_played, &[], &[], &[&["status", "--json"], &["runs", "retrieve", &run, "--json"], &["secrets", "list", "--json"], &["keys", "list"]]);
}

/// Each command finding the control plane through AWS first: where AWS fails, the command stops saying so.
#[test]
fn commands_that_find_the_control_plane_first_hold_with_each_call_and_store_operation_failing() {
    let _one = one_at_a_time();
    let (run, job) = ids();
    each(plane_to_find, &[("NPM_VALUE", "x")], &[], &[
        &["status", "--json"], &["jobs", "retrieve", &job, "--json"], &["runs", "retrieve", &run, "--json"], &["runs", "cancel", &run, "--json"], &["usage", "retrieve", "--json"],
        &["artifacts", "list", &format!("--run={run}"), "--json"], &["caches", "retrieve", "--json"], &["caches", "update", "--days=3", "--json"], &["secrets", "list", "--json"],
        &["secrets", "create", "NPM_TOKEN", "--value-env=NPM_VALUE", "--json"], &["keys", "list", "--json"], &["keys", "create", "agent2", "--json"], &["accounts", "update", "aws", "--monthly-usd=5", "--json"],
        &["deliveries", "list", "--json"], &["runners", "order", "cloudflare", "--json"], &["runners", "update", "cloudflare", "--max-jobs=3", "--json"], &["runners", "list", "--json"], &["labels", "create", "gpu", "--json"],
        &["name", "update", "Acme", "--json"], &["machine", "update", "--cpu=4", "--json"], &["limits", "update", "--max-cpu=8", "--json"], &["public_repos", "create", "acme/site", "--json"],
        &["connections", "list", "--json"], &["projects", "list", "--json"],
    ]);
}

#[test]
fn reading_what_aws_has_holds_with_each_call_and_store_operation_failing() {
    let _one = one_at_a_time();
    each(plane, &[], &[], &[&["planes", "list", "--json"], &["accounts", "list", "--json"], &["accounts", "retrieve", "aws", "--json"], &["images", "list", "--json"]]);
}

#[test]
fn changing_runners_and_limits_holds_with_each_call_and_store_operation_failing() {
    let _one = one_at_a_time();
    each(plane, &[], &[], &[
        &["runners", "update", "cloudflare", "--max-jobs=20", "--monthly-usd=50", "--location=weur", "--json"], &["runners", "order", "cloudflare", "--json"],
        &["limits", "update", "--max-hours=12", "--max-cpu=16", "--monthly-usd=100", "--daily-usd=10", "--json"], &["accounts", "update", "aws", "--monthly-usd=80", "--daily-usd=8", "--json"],
        &["machine", "update", "--cpu=4", "--ram=16", "--json"],
    ]);
}

#[test]
fn changing_the_cache_keys_labels_and_names_holds_with_each_call_and_store_operation_failing() {
    let _one = one_at_a_time();
    each(plane, &[], &[], &[
        &["caches", "update", "--size-gb=50", "--days=3", "--enabled=false", "--images=false", "--json"], &["caches", "delete", "--repo=acme/app", "--confirm", "--json"],
        &["keys", "create", "builder", "--runs", "--json"], &["keys", "create", "reader"], &["labels", "create", "gpu", "--json"], &["name", "update", "Acme", "--json"],
        &["public_repos", "create", "acme/site", "--json"],
    ]);
    // What takes out what was made just before.
    each(plane, &[], &[&["keys", "create", "builder", "--runs"]], &[&["keys", "delete", "builder", "--json"]]);
    each(plane, &[], &[&["labels", "create", "gpu"]], &[&["labels", "delete", "gpu", "--confirm", "--json"]]);
    each(plane, &[], &[&["public_repos", "create", "acme/site"]], &[&["public_repos", "delete", "acme/site", "--json"]]);
}

#[test]
fn keeping_secrets_holds_with_each_call_and_store_operation_failing() {
    let _one = one_at_a_time();
    let envs = [("NPM_VALUE", "s3cret-value")];
    each(plane, &envs, &[], &[&["secrets", "create", "NPM_TOKEN", "--value-env=NPM_VALUE", "--repo=acme/app", "--branch=release/*", "--json"]]);
    each(plane, &envs, &[&["secrets", "create", "NPM_TOKEN", "--value-env=NPM_VALUE"]], &[
        &["secrets", "update", "NPM_TOKEN", "--value-env=NPM_VALUE", "--json"], &["secrets", "list"], &["secrets", "delete", "NPM_TOKEN", "--confirm", "--json"],
    ]);
}

/// A control plane with GitHub connected for two organizations.
fn plane_with_github() -> Setup {
    let s = plane();
    s.0.connect_github();
    // A further organization's App, as the dashboard leaves one: that one may be disconnected.
    let mut more = superci_testkit::GitHub::app();
    (more["id"], more["owner"], more["slug"]) = (77.into(), "acme-corp".into(), "superci-acme-corp".into());
    s.0.aws.put(&format!("/superci/{}/GITHUB_APP_77", superci_testkit::PLANE), &more.to_string());
    s
}

#[test]
fn connecting_and_disconnecting_code_hosts_holds_with_each_call_and_store_operation_failing() {
    let _one = one_at_a_time();
    let project = superci_testkit::gitlab::PROJECT.to_string();
    let envs = [("SUPERCI_GITLAB_TOKEN", superci_testkit::gitlab::TOKEN)];
    each(plane_with_github, &envs, &[], &[&["connections", "create", "gitlab", "--json"], &["connections", "delete", "github", "acme-corp", "--confirm", "--json"], &["deliveries", "list"]]);
    each(plane_with_github, &envs, &[&["connections", "create", "gitlab"]], &[
        &["projects", "list", "--json"], &["projects", "update", &project, "--enabled=true", "--json"], &["projects", "update", "--all", "--enabled=true", "--json"],
        &["connections", "list"], &["connections", "delete", "gitlab", "--confirm", "--json"],
    ]);
}

#[test]
fn workspaces_hold_with_each_call_and_store_operation_failing() {
    let _one = one_at_a_time();
    each(plane, &[], &[], &[&["workspaces", "create", "Build", "Farm", "--json"], &["status", "--workspace=default", "--json"]]);
    each(plane, &[], &[&["workspaces", "create", "Build", "Farm"]], &[&["workspaces", "list", "--json"], &["workspaces", "update", "build-farm", "--id=farm", "--json"], &["workspaces", "delete", "build-farm", "--confirm", "--json"]]);
}

/// A zip as upload-artifact makes one (its files stored).
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

/// A control plane in its own AWS account with its bucket (the working tree goes there), its machines its own; the
/// played run kept, with an artifact in the bucket; and a checkout to run from.
fn plane_with_a_bucket() -> Setup {
    let served = serve(|c| {
        (c.aws_own, c.aws_own_creds) = (Some(("111111111111".into(), "us-east-1".into())), Some(superci_core::aws::Credentials { access_key_id: "ASIAX".into(), secret_access_key: "secret".into(), session_token: Some("tok".into()), expires_at_ms: u64::MAX }));
        c.routing.order = vec![superci_core::plane::Pool::of("cloudflare")];
    });
    let home = common::signed_in(&served);
    played(&served);
    let (run, _) = ids();
    let bucket = superci_core::cache::bucket(superci_testkit::PLANE, "us-east-1");
    // (Its cache's program in the bucket: the bucket is ready.)
    served.github.put_object(&format!("{bucket}/{}", superci_core::cache::program_key("x86_64")), b"program");
    let zip = zip_of(&[("index.html", b"<h1>all passed</h1>"), ("data/results.json", b"{}")]);
    served.github.put_object(&format!("{bucket}/c/run-{run}/report0000000000001/p00000"), &zip);
    served.store.0.lock().unwrap().insert(format!("artifact:{run}:report"), serde_json::json!({ "id": "report0000000000001", "region": "us-east-1", "pieces": [["p00000", zip.len()]], "size": zip.len(), "made": served.now_ms(), "done": true }).to_string());
    let dir = home.join("checkout");
    std::fs::create_dir_all(dir.join(".github/workflows")).unwrap();
    std::fs::write(dir.join(".github/workflows/ci.yml"), "jobs:\n  tests:\n    runs-on: superci\n    steps:\n      - run: make test\n").unwrap();
    std::fs::write(dir.join("Makefile"), "test:\n\techo ok\n").unwrap();
    for args in [&["init", "-q"][..], &["add", "-A"], &["-c", "user.name=t", "-c", "user.email=t@example.com", "commit", "-qm", "t"]] { assert!(std::process::Command::new("git").current_dir(&dir).args(args).status().unwrap().success()) }
    (served, home, dir)
}

/// Asked how a run stands often, out of touch for a few seconds at most, a run's lines not waited for long: what a
/// fault costs a run from a computer is said soon.
const SOON: [(&str, &str); 4] = [("SUPERCI_POLL_MS", "100"), ("SUPERCI_LOST_S", "2"), ("SUPERCI_GIVE_UP_S", "1"), ("SUPERCI_LOGS_WAIT_S", "1")];

/// Runs from a computer started: the working tree sent into the bucket while the run's machine starts, then (a run of
/// it kept) only what changed since, as a layer on it.
#[test]
fn starting_runs_from_a_computer_holds_with_each_call_and_store_operation_failing() {
    let _one = one_at_a_time();
    let workflow = "--workflow=.github/workflows/ci.yml";
    each(plane_with_a_bucket, &SOON, &[], &[&["runs", "create", workflow, "--detach", "--json"]]);
    each(plane_with_a_bucket, &SOON, &[&["runs", "create", workflow, "--detach"]], &[&["runs", "create", workflow, "--detach", "--event=pull_request", "--label=ready", "--secret=TOKEN=a;b", "--var=WHERE=x|y", "--repo=acme/app"]]);
}

/// A run from a computer that is over, followed to its end and cancelled; its artifacts listed and fetched.
#[test]
fn following_a_run_from_a_computer_and_fetching_its_artifacts_holds_with_each_call_and_store_operation_failing() {
    let _one = one_at_a_time();
    let (run, _) = ids();
    let out = format!("--out={}", common::temp("artifact").join("report").display());
    each(plane_with_a_bucket, &SOON, &[], &[
        &["runs", "wait", &run, "--json"], &["runs", "wait", &run, "--quiet"], &["runs", "cancel", &run, "--json"], &["artifacts", "list", &format!("--run={run}")],
        &["artifacts", "retrieve", "report", &format!("--run={run}"), &out, "--json"],
    ]);
}

/// Another control plane in the same account deleted, with what was made for it, as AWS answered a real account.
#[test]
fn deleting_another_control_plane_holds_with_each_call_and_store_operation_failing() {
    let _one = one_at_a_time();
    sweep("superci planes delete", |fault| {
        let s = plane();
        let other = superci_testkit::serve_as("testplane0002", |_| {});
        s.0.aws.replaying(include_str!("../fixtures/aws/planes/delete.jsonl"), &[("http://new-plane.test", &other.url)]);
        for (k, v) in other.aws.settings("testplane0002") { s.0.aws.put(&format!("/superci/testplane0002/{k}"), &v) }
        one(fault, s, &[("SUPERCI_PAUSE_MS", "0")], &[], &["planes", "delete", "testplane0002", "--confirm", "--json"])
    });
}

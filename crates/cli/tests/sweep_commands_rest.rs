//! What the CLI's commands do where something on this computer or the control plane's answer is not as it usually is:
//! no git, a tar or gzip that fails, a folder that cannot be written, a control plane that drops a connection or answers
//! what is not JSON, and the edges of each command's own checks. Each says what failed and stops, as the sweeps of
//! `sweep_commands.rs` hold commands to, where a stand-in's single fault cannot reach; and the runner image's commands
//! swept as those are.
mod common;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use superci_testkit::{serve, sweep, Fault, Outcome, Served};

/// What a scripted control plane does with a request: answers it, or drops the connection with no answer (as a
/// network that fails does).
enum Act { Say(u16, String), Drop }

/// A control plane scripted by `act` (given each request's method, target and body). Its address, and each request it
/// was asked (`METHOD TARGET`).
fn scripted(act: impl Fn(&str, &str, &[u8]) -> Act + Send + Sync + 'static) -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    let (act, asked) = (Arc::new(act), Arc::new(Mutex::new(vec![])));
    let kept = asked.clone();
    std::thread::spawn(move || for stream in listener.incoming().flatten() {
        let (act, asked) = (act.clone(), asked.clone());
        std::thread::spawn(move || {
            let Some((method, target, body)) = request(&stream) else { return };
            asked.lock().unwrap().push(format!("{method} {target}"));
            if let Act::Say(status, said) = act(&method, &target, &body) {
                let mut stream = stream;
                let _ = stream.write_all(format!("HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{said}", said.len()).as_bytes());
            }
        });
    });
    (url, kept)
}

/// A request as it came: its method, its target and its body.
fn request(mut stream: &TcpStream) -> Option<(String, String, Vec<u8>)> {
    let mut got = vec![];
    let mut byte = [0u8; 1];
    while !got.ends_with(b"\r\n\r\n") { if stream.read(&mut byte).ok()? == 0 { return None } got.push(byte[0]) }
    let head = String::from_utf8_lossy(&got).to_string();
    let length: usize = head.lines().find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").map(|n| n.trim().parse().unwrap_or(0))).unwrap_or(0);
    let mut body = vec![0u8; length];
    stream.read_exact(&mut body).ok()?;
    let mut first = head.split_whitespace();
    Some((first.next()?.to_string(), first.next()?.to_string(), body))
}

/// A control plane's health, as one new enough for runs from a computer says it.
fn health() -> Act { Act::Say(200, json!({ "version": "0.19.19", "plane": "testplane0001" }).to_string()) }

/// Asked how a run stands often, its lines not waited for long.
const SOON: [(&str, &str); 4] = [("SUPERCI_POLL_MS", "50"), ("SUPERCI_LOST_S", "2"), ("SUPERCI_GIVE_UP_S", "1"), ("SUPERCI_LOGS_WAIT_S", "1")];

/// The CLI run in `dir` with a run key for the control plane at `plane` (no workspace), these settings of its own, ended
/// if not over in a minute: whether it did well, and what it said (out, then errors).
fn run_at(dir: &Path, plane: &str, args: &[&str], envs: &[(&str, &str)]) -> (bool, String) {
    let home = dir.join(".superci-home");
    let child = common::command(&home).current_dir(dir).args(args).env("SUPERCI_PLANE", plane).env("SUPERCI_KEY", "superci_run_key_test").envs(SOON).envs(envs.iter().copied())
        .stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).spawn().unwrap();
    let pid = child.id();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || { let _ = tx.send(child.wait_with_output()); });
    let out = match rx.recv_timeout(std::time::Duration::from_secs(60)) {
        Ok(out) => out.unwrap(),
        Err(_) => { let _ = Command::new("kill").args(["-9", &pid.to_string()]).status(); panic!("{args:?}: not over in a minute") }
    };
    let said = String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    assert!(!said.contains("panicked"), "{args:?}: {said}");
    (out.status.success(), said)
}

/// A checkout with a workflow of one job, committed.
fn checkout(what: &str) -> PathBuf {
    let dir = common::temp(what);
    std::fs::create_dir_all(dir.join(".github/workflows")).unwrap();
    std::fs::write(dir.join(".github/workflows/ci.yml"), "on: [push, workflow_dispatch]\njobs:\n  tests:\n    runs-on: superci\n    steps:\n      - run: make test\n").unwrap();
    std::fs::write(dir.join("Makefile"), "test:\n\techo ok\n").unwrap();
    git(&dir, &["init", "-q"]);
    git(&dir, &["add", "-A"]);
    git(&dir, &["-c", "user.name=t", "-c", "user.email=t@example.com", "commit", "-qm", "t"]);
    dir
}

fn git(dir: &Path, args: &[&str]) { assert!(Command::new("git").current_dir(dir).args(args).status().unwrap().success(), "git {args:?}") }

/// A folder of programs to put first on the PATH: each a shell script (`name`, its lines), or a link to the system's own
/// (`name`, None).
fn programs(what: &str, each: &[(&str, Option<&str>)]) -> PathBuf {
    let dir = common::temp(what);
    for (name, script) in each {
        let at = dir.join(name);
        match script {
            Some(s) => {
                std::fs::write(&at, format!("#!/bin/sh\n{s}\n")).unwrap();
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&at, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
            None => {
                let real = String::from_utf8(Command::new("sh").args(["-c", &format!("command -v {name}")]).output().unwrap().stdout).unwrap();
                std::os::unix::fs::symlink(real.trim(), &at).unwrap();
            }
        }
    }
    dir
}

/// The PATH with `first` before the system's own folders (or alone).
fn path_with(first: &Path, alone: bool) -> String {
    if alone { first.display().to_string() } else { format!("{}:/usr/bin:/bin", first.display()) }
}

const WORKFLOW: &str = "--workflow=.github/workflows/ci.yml";

#[test]
fn a_checkout_git_cannot_be_run_in_and_one_that_is_not_one_say_so_before_anything_is_sent() {
    let dir = checkout("no-git");
    let (plane, asked) = scripted(|_, target, _| if target == "/health" { health() } else { Act::Say(500, "{}".into()) });
    // A git that names a folder that is not there as the checkout's top: every git and tar after it cannot start there.
    let bin = programs("bad-git", &[("git", Some("echo /nonexistent/superci-checkout"))]);
    let path = path_with(&bin, false);
    for args in [&["runs", "create", WORKFLOW, "--detach"][..], &["runs", "create", WORKFLOW, "--dry-run"]] {
        let (ok, said) = run_at(&dir, &plane, args, &[("PATH", &path)]);
        assert!(!ok && said.contains("git: "), "{said}");
    }
    assert!(!asked.lock().unwrap().iter().any(|a| a.starts_with("POST")), "nothing sent: {:?}", asked.lock().unwrap());
    // No git at all, and a folder that is gone.
    let (ok, said) = run_at(&dir, &plane, &["runs", "create", WORKFLOW], &[("PATH", &path_with(&programs("nothing", &[]), true))]);
    assert!(!ok && said.contains("Run it in a git checkout"), "{said}");
    let gone = dir.join("gone");
    std::fs::create_dir_all(&gone).unwrap();
    let o = Command::new("sh").current_dir(&dir).args(["-c", &format!("cd '{}' && rmdir '{}' && exec '{}' runs create {WORKFLOW}", gone.display(), gone.display(), env!("CARGO_BIN_EXE_superci"))])
        .env("SUPERCI_HOME", dir.join(".superci-home")).env("SUPERCI_PLANE", &plane).env("SUPERCI_KEY", "k").output().unwrap();
    assert!(!o.status.success() && !String::from_utf8_lossy(&o.stderr).trim().is_empty(), "{o:?}");
}

#[test]
fn an_events_payload_a_workflow_that_does_not_read_and_a_checkout_off_its_branch_are_said_as_they_stand() {
    let dir = checkout("event-edges");
    let (plane, _) = scripted(|_, target, _| if target == "/health" { health() } else { Act::Say(500, "{}".into()) });
    let run = |args: &[&str]| run_at(&dir, &plane, &[&["runs", "create"][..], args].concat(), &[]);
    // A payload file that is not there, one that is not JSON, and one whose object adds nothing to the repository's.
    let (ok, said) = run(&[WORKFLOW, "--event-payload=missing.json", "--dry-run"]);
    assert!(!ok && said.contains("missing.json: "), "{said}");
    std::fs::write(dir.join("not.json"), "not json").unwrap();
    let (ok, said) = run(&[WORKFLOW, "--event-payload=not.json", "--dry-run"]);
    assert!(!ok && said.contains("not.json is not JSON"), "{said}");
    std::fs::write(dir.join("empty.json"), r#"{"repository": {}}"#).unwrap();
    let (ok, said) = run(&[WORKFLOW, "--event-payload=empty.json", "--dry-run", "--job=tests"]);
    assert!(ok && said.contains("its job tests (and the jobs it needs)"), "{said}");
    // A workflow that does not read: started by hand, and tried.
    std::fs::write(dir.join("broken.yml"), "jobs: [unclosed\n").unwrap();
    let (ok, said) = run(&["--workflow=broken.yml", "--event=workflow_dispatch", "--dry-run"]);
    assert!(!ok && !said.trim().is_empty(), "{said}");
    let (ok, said) = run(&["--workflow=broken.yml", "--dry-run"]);
    assert!(!ok && said.contains("broken.yml: "), "{said}");
    // Origin's default branch, a checkout on no branch, and a workflow from outside the checkout.
    git(&dir, &["remote", "add", "origin", "https://github.com/acme/app.git"]);
    git(&dir, &["update-ref", "refs/remotes/origin/trunk", "HEAD"]);
    git(&dir, &["symbolic-ref", "refs/remotes/origin/HEAD", "refs/remotes/origin/trunk"]);
    git(&dir, &["checkout", "-q", "--detach"]);
    let outside = common::temp("outside");
    std::fs::write(outside.join("ci.yml"), "jobs:\n  tests:\n    runs-on: superci\n    steps:\n      - run: make test\n").unwrap();
    let (ok, said) = run(&[&format!("--workflow={}", outside.join("ci.yml").display()), "--event=pull_request", "--dry-run"]);
    assert!(ok && said.contains("ci.yml: its job tests"), "{said}");
}

/// A control plane that keeps runs' working trees in its bucket (this server, too) and takes each run, as `mode` has it
/// answer: `drop` drops the tree's first request, `garbage` answers it with what is not JSON; `far` gives a link to the
/// bucket nobody answers; `parts` keeps no bucket and refuses the parts; anything else answers as it should.
fn bucket_plane() -> (String, Arc<Mutex<&'static str>>, Arc<Mutex<Vec<String>>>) {
    let (mode, at) = (Arc::new(Mutex::new("")), Arc::new(Mutex::new(String::new())));
    let (m, a) = (mode.clone(), at.clone());
    let (url, asked) = scripted(move |method, target, _| {
        let (mode, url) = (*m.lock().unwrap(), a.lock().unwrap().clone());
        let bucket = if mode == "far" { "http://127.0.0.1:9".to_string() } else { format!("{url}/bucket") };
        match (method, target) {
            (_, "/health") => health(),
            ("POST", t) if t.starts_with("/runs/tree") && mode == "drop" => Act::Drop,
            ("POST", t) if t.starts_with("/runs/tree") && mode == "garbage" => Act::Say(200, "not json".into()),
            ("POST", t) if t.starts_with("/runs/tree") && t.contains("&layer=") => Act::Say(200, json!({ "url": format!("{bucket}/layer"), "base": true, "later": false, "parts": 2 }).to_string()),
            ("POST", t) if t.starts_with("/runs/tree") && mode == "parts" => if t.contains("&keep=") { Act::Say(200, "{}".into()) } else { Act::Say(500, r#"{"error":"full"}"#.into()) },
            ("POST", t) if t.starts_with("/runs/tree") => Act::Say(200, json!({ "url": format!("{bucket}/base"), "later": false }).to_string()),
            ("PUT", t) if t.starts_with("/bucket/") => Act::Say(200, "".into()),
            ("POST", "/runs") => Act::Say(200, json!({ "run_id": "1", "jobs": [] }).to_string()),
            _ => Act::Say(404, "{}".into()),
        }
    });
    *at.lock().unwrap() = url.clone();
    (url, mode, asked)
}

#[test]
fn the_working_tree_not_packed_or_not_sent_stops_the_run_saying_why_and_a_layer_on_it_too() {
    let dir = checkout("tree-edges");
    // A link in the tree (kept as where it points), a large file that stays, and one that goes.
    std::os::unix::fs::symlink("Makefile", dir.join("make-link")).unwrap();
    std::fs::write(dir.join("big.txt"), "x".repeat(200_000)).unwrap();
    std::fs::write(dir.join("goes.txt"), "goes").unwrap();
    let (plane, mode, _) = bucket_plane();
    let run = |m: &'static str, envs: &[(&str, &str)]| { *mode.lock().unwrap() = m; run_at(&dir, &plane, &["runs", "create", WORKFLOW, "--detach"], envs) };
    // No tar to pack it with; the control plane gone mid-way; its bucket out of reach; parts it refuses.
    let only_git = programs("only-git", &[("git", None)]);
    let (ok, said) = run("", &[("PATH", &path_with(&only_git, true))]);
    assert!(!ok && said.contains("tar: "), "{said}");
    let (ok, said) = run("drop", &[]);
    assert!(!ok && !said.trim().is_empty(), "{said}");
    let (ok, said) = run("garbage", &[]);
    assert!(!ok && said.contains("the control plane's answer"), "{said}");
    let (ok, said) = run("far", &[]);
    assert!(!ok && !said.trim().is_empty(), "{said}");
    let (ok, said) = run("parts", &[]);
    assert!(!ok && said.contains("the control plane said 500"), "{said}");
    // Sent whole once: kept as the base for the next run's.
    let (ok, said) = run("", &[]);
    assert!(ok && said.contains("Run 1"), "{said}");
    std::fs::write(dir.join("Makefile"), "test:\n\techo changed\n").unwrap();
    std::fs::remove_file(dir.join("goes.txt")).unwrap();
    // What changed, not packed: a tar that fails, one that fails at the list of what was removed, no folder for that
    // list, no gzip, a gzip that fails; then not sent.
    let failing_tar = programs("failing-tar", &[("tar", Some("exit 2"))]);
    let (ok, said) = run("", &[("PATH", &path_with(&failing_tar, false))]);
    assert!(!ok && said.contains("tar: "), "{said}");
    let picky_tar = programs("picky-tar", &[("tar", Some("case \"$PWD\" in *superci-removed*) echo refused >&2; exit 2;; esac\nexec /usr/bin/tar \"$@\""))]);
    let (ok, said) = run("", &[("PATH", &path_with(&picky_tar, false))]);
    assert!(!ok && said.contains("tar: refused"), "{said}");
    let file = dir.join("big.txt").display().to_string();
    let (ok, said) = run("", &[("TMPDIR", &file)]);
    assert!(!ok && !said.trim().is_empty(), "{said}");
    let no_gzip = programs("no-gzip", &[("git", None), ("tar", None)]);
    let (ok, said) = run("", &[("PATH", &path_with(&no_gzip, true))]);
    assert!(!ok && said.contains("gzip: "), "{said}");
    let failing_gzip = programs("failing-gzip", &[("gzip", Some("exit 1"))]);
    let (ok, said) = run("", &[("PATH", &path_with(&failing_gzip, false))]);
    assert!(!ok && said.contains("gzip failed"), "{said}");
    let (ok, said) = run("far", &[]);
    assert!(!ok && said.contains("Sending what changed"), "{said}");
    let (ok, said) = run("", &[]);
    assert!(ok && said.contains("1 removed"), "{said}");
}

/// A control plane whose runs stand as `run` says on each time it is asked (its number, from 0), each step's log as
/// `log` gives it (its job, its number; None: not there), with a bucket nobody answers for the working tree, sent
/// while the run's machine starts.
fn following_plane(run: impl Fn(usize) -> Act + Send + Sync + 'static, log: impl Fn(u64, u64) -> Option<String> + Send + Sync + 'static) -> (String, Arc<Mutex<Vec<String>>>) {
    let asked = Arc::new(Mutex::new(0usize));
    scripted(move |method, target, _| match (method, target) {
        (_, "/health") => health(),
        ("POST", t) if t.starts_with("/runs/tree") => Act::Say(200, json!({ "url": "http://127.0.0.1:9/tree", "later": true }).to_string()),
        ("POST", "/runs") => Act::Say(200, json!({ "run_id": "7", "jobs": [] }).to_string()),
        ("POST", t) if t.ends_with("/cancel") => Act::Say(200, "{}".into()),
        ("GET", t) if t.starts_with("/runs/8?") => Act::Say(200, "not json".into()),
        ("GET", t) if t.starts_with("/runs/7/log?") => {
            let n = |k: &str| t.split(['?', '&']).find_map(|p| p.strip_prefix(k)).and_then(|v| v.parse().ok()).unwrap_or(0);
            match log(n("job="), n("log=")) { Some(text) => Act::Say(200, text), None => Act::Say(500, "{}".into()) }
        }
        ("GET", t) if t.starts_with("/runs/7?") => { let mut a = asked.lock().unwrap(); *a += 1; run(*a - 1) }
        _ => Act::Say(404, "{}".into()),
    })
}

#[test]
fn a_run_followed_says_each_jobs_state_and_its_failed_steps_whatever_their_logs_hold() {
    // Its tree not sent (the bucket does not answer): the run is cancelled, and that is said once it is over.
    let dir = checkout("follow-edges");
    let (plane, asked) = following_plane(|_| Act::Say(200, json!({ "over": true, "result": "cancelled", "jobs": [] }).to_string()), |_, _| None);
    let (ok, said) = run_at(&dir, &plane, &["runs", "create", WORKFLOW], &[]);
    assert!(!ok && said.contains("The working tree was not sent") && said.contains("run 7 was cancelled"), "{said}");
    assert!(asked.lock().unwrap().iter().any(|a| a == "POST /runs/7/cancel"), "{:?}", asked.lock().unwrap());
    // Two jobs, one starting and one in a state of its own; then over, a step failed with no log, and one whose test
    // failed saying nothing more.
    let over = json!({ "over": true, "result": "failure", "jobs": [
        { "name": "a", "state": "completed", "result": "failure", "job_id": "11", "posts": [], "steps": [
            { "id": "s1", "name": "Build", "order": 1, "state": "completed", "result": "failed", "log": 1 },
            { "id": "s2", "name": "Test", "order": 2, "state": "completed", "result": "failed", "log": 2 }] },
        { "name": "b", "state": "completed", "result": "success", "posts": [], "steps": [] }] });
    let (plane, _) = following_plane(move |n| Act::Say(200, if n == 0 { json!({ "jobs": [{ "name": "a", "state": "launched", "machine": "m-1" }, { "name": "b", "state": "waiting" }] }) } else { over.clone() }.to_string()),
        |_, n| (n == 2).then(|| "test a::b ... FAILED\n".to_string()));
    let (ok, said) = run_at(&dir, &plane, &["runs", "wait", "7", "--quiet"], &[]);
    assert!(!ok && said.contains("[a] Its machine is starting (m-1)") && said.contains("[b] Waiting") && said.contains("Failed: Build, Test"), "{said}");
    // An answer that is not JSON.
    let (ok, said) = run_at(&dir, &plane, &["runs", "wait", "8"], &[]);
    assert!(!ok && said.contains("the control plane's answer"), "{said}");
}

/// A control plane whose run 1 kept one artifact, `report`, as `answer` has it answer for it (given the control
/// plane's address, where its one piece is).
fn artifact_plane(answer: impl Fn(&str) -> Act + Send + Sync + 'static) -> String {
    let at = Arc::new(Mutex::new(String::new()));
    let here = at.clone();
    let url = scripted(move |_, target, _| match target {
        "/health" => health(),
        "/piece" => Act::Say(200, "PK".into()),
        t if t.starts_with("/status") => Act::Say(200, json!({ "jobs": [] }).to_string()),
        t if t.starts_with("/runs/1/artifacts?name=report") => answer(&here.lock().unwrap()),
        _ => Act::Say(404, "{}".into()),
    }).0;
    *at.lock().unwrap() = url.clone();
    url
}

#[test]
fn an_artifact_that_cannot_be_fetched_or_put_in_place_is_said_and_one_of_no_pieces_is_kept_as_it_is() {
    let dir = common::temp("artifact-edges");
    let fetch = |plane: &str, out: &Path| run_at(&dir, plane, &["artifacts", "retrieve", "report", "--run=1", &format!("--out={}", out.display())], &[]);
    // The control plane gone, and an answer that is not JSON.
    let (ok, said) = fetch(&artifact_plane(|_| Act::Drop), &dir.join("a"));
    assert!(!ok && !said.trim().is_empty(), "{said}");
    let (ok, said) = fetch(&artifact_plane(|_| Act::Say(200, "not json".into())), &dir.join("b"));
    assert!(!ok && said.contains("the control plane's answer"), "{said}");
    // No pieces: an empty file, not a zip, kept as it is.
    let empty = artifact_plane(|_| Act::Say(200, json!({ "size": 0, "pieces": [] }).to_string()));
    let (ok, said) = fetch(&empty, &dir.join("c"));
    assert!(ok && dir.join("c/report").is_file(), "{said}");
    // Where it goes is a file; the file it is fetched into is a folder; where it is kept is a folder with files in it.
    std::fs::write(dir.join("file"), "").unwrap();
    let (ok, said) = fetch(&empty, &dir.join("file/d"));
    assert!(!ok && said.contains("file/d: "), "{said}");
    std::fs::create_dir_all(dir.join("e/.report.superci-download")).unwrap();
    let (ok, said) = fetch(&empty, &dir.join("e"));
    assert!(!ok && said.contains(".report.superci-download: "), "{said}");
    std::fs::create_dir_all(dir.join("f/report/inside")).unwrap();
    let (ok, said) = fetch(&empty, &dir.join("f"));
    assert!(!ok && !said.trim().is_empty(), "{said}");
    // A disk that is full (Linux's /dev/full) as its pieces are written.
    if Path::new("/dev/full").exists() {
        let full = artifact_plane(|url| Act::Say(200, json!({ "size": 2, "pieces": [{ "url": format!("{url}/piece") }] }).to_string()));
        std::fs::create_dir_all(dir.join("g")).unwrap();
        std::os::unix::fs::symlink("/dev/full", dir.join("g/.report.superci-download")).unwrap();
        let (ok, said) = fetch(&full, &dir.join("g"));
        assert!(!ok && !said.trim().is_empty(), "{said}");
    }
}

/// A control plane at `version` that reads as `status` says, its cache as `caches` says, each job's log a line.
fn reading_plane(version: &'static str, status: Value, caches: Value) -> String {
    scripted(move |_, target, _| match target {
        "/health" => Act::Say(200, json!({ "version": version, "plane": "testplane0001" }).to_string()),
        t if t.starts_with("/status") => Act::Say(200, status.to_string()),
        t if t.starts_with("/caches") => Act::Say(200, caches.to_string()),
        t if t.starts_with("/job/log?") && t.ends_with("&link=1") => Act::Say(404, "{}".into()),
        t if t.starts_with("/job/log?") => Act::Say(200, json!({ "log": "a line\n" }).to_string()),
        _ => Act::Say(404, "{}".into()),
    }).0
}

#[test]
fn what_a_control_plane_keeps_of_jobs_usage_and_its_cache_is_said_as_it_stands() {
    let home = common::temp("reading");
    let at = |plane: &str| [("SUPERCI_PLANE", plane.to_string()), ("SUPERCI_KEY", "superci_run_key_test".to_string())];
    let run = |plane: &str, args: &[&str]| { let envs = at(plane); cli(&home, args, &envs.iter().map(|(k, v)| (*k, v.as_str())).collect::<Vec<_>>()) };
    // A job, and one of a run that says no id of its own (an older control plane's); no usage counted yet.
    let status = json!({ "jobs": [{ "job_id": 5, "run_id": 9, "name": "a", "state": "completed", "conclusion": "success" }, { "run_id": 10, "name": "b", "state": "completed" }] });
    let now = 4_102_444_800_000u64;
    let caches = |images: Value| json!({ "on": true, "size_gb": 10, "days": 7, "max_age_days": 30, "now_ms": now, "images": images, "repos": [{ "name": "acme/app", "entries": 2, "bytes": 1_000_000, "used_ms": now - 5 * 86_400_000 }] });
    let plane = reading_plane("0.19.19", status.clone(), caches(json!({ "on": true, "layers": 3, "bytes": 5_000_000_000u64 })));
    let (code, said) = run(&plane, &["jobs", "retrieve", "5", &format!("--out={}", home.join("missing/job.log").display())]);
    assert!(code != 0 && said.contains("missing/job.log: "), "{said}");
    let (code, said) = run(&plane, &["jobs", "retrieve", "--run=10"]);
    assert!(code == 0 && said.contains("a line"), "{said}");
    let (code, said) = run(&plane, &["usage", "retrieve"]);
    assert!(code != 0 && said.contains("does not count usage yet"), "{said}");
    // The cache: its images kept (some layers now), not kept, or not said; an entry used days ago.
    let (code, said) = run(&plane, &["caches", "retrieve"]);
    assert!(code == 0 && said.contains("3 layers now, 5.0 GB") && said.contains("used 5 days ago"), "{said}");
    let (code, said) = run(&reading_plane("0.19.19", status.clone(), caches(json!({ "on": false }))), &["caches", "retrieve"]);
    assert!(code == 0 && said.contains("Docker images: not kept"), "{said}");
    let (code, said) = run(&reading_plane("0.19.19", status.clone(), caches(Value::Null)), &["caches", "retrieve"]);
    assert!(code == 0 && !said.contains("Docker images"), "{said}");
    // One too old for artifacts.
    let (code, said) = run(&reading_plane("0.17.0", status, Value::Null), &["artifacts", "list", "--run=1"]);
    assert!(code != 0 && said.contains("runs from a computer need 0.18.0 or newer"), "{said}");
}

#[test]
fn secrets_are_listed_with_how_long_ago_each_was_set_and_used_and_by_whom() {
    let served = serve(|_| {});
    let home = common::home(&served);
    std::fs::write(home.join("aws-at"), &served.aws.url).unwrap();
    let now = 4_102_444_800_000u64;
    let kept = json!({ "now_ms": now, "secrets": [
        { "name": "A_TOKEN", "said": "every repository", "updated_ms": now - 30 * 60_000, "used": { "at_ms": now - 90 * 60_000 } },
        { "name": "B_TOKEN", "said": "every repository", "updated_ms": now - 10 * 86_400_000, "used": {} },
        { "name": "C_TOKEN", "said": "every repository", "updated_ms": now, "used": { "at_ms": now - 3 * 86_400_000, "by": "AGENT", "run_key": true } }] });
    let plane = superci_testkit::front(&served.url, move |_, target, status, body| if target == "/secrets" { (200, kept.to_string().into_bytes()) } else { (status, body) });
    common::kept_at(&home, &plane);
    let (code, said) = cli(&home, &["secrets", "list"], &[]);
    assert!(code == 0, "{said}");
    for line in ["set 30 min ago, used 1 h ago by a computer", "set 10 days ago, not used yet", "set just now, used 3 days ago by the key agent"] { assert!(said.contains(line), "{line}: {said}") }
}

#[test]
fn a_control_plane_behind_this_version_says_what_to_update_to_and_an_update_says_from_what() {
    let served = serve(|_| {});
    let home = common::signed_in(&served);
    served.answer_as("0.18.0", &[]);
    let (code, said) = cli(&home, &["status"], &[]);
    assert!(code == 0 && said.contains("0.18.0 (update to"), "{said}");
    let (code, said) = cli(&home, &["planes", "update", "--dry-run"], &[]);
    assert!(code == 0 && said.contains("from 0.18.0 to"), "{said}");
    // One whose health does not answer: its version is not known.
    served.answer_as("", &[]);
    let plane = superci_testkit::front(&served.url, |_, target, status, body| if target == "/health" { (503, b"busy".to_vec()) } else { (status, body) });
    common::kept_at(&home, &plane);
    let (code, said) = cli(&home, &["planes", "update", "--dry-run"], &[]);
    assert!(code == 0 && said.contains(&format!("update the control plane in use (AWS · 111111111111 · us-east-1) to {}", env!("CARGO_PKG_VERSION"))), "{said}");
}

#[test]
fn a_control_plane_is_made_in_the_account_named_or_the_only_one_and_several_are_said() {
    let served = serve(|_| {});
    let home = common::signed_in(&served);
    let (code, said) = cli(&home, &["planes", "create", "aws", "--region=us-east-1", "--account=111111111111", "--dry-run"], &[]);
    assert!(code == 0 && said.contains("deploy a control plane to AWS, in us-east-1"), "{said}");
    let (code, said) = cli(&home, &["planes", "create", "cloudflare", "--account=2222222222222222222222222222222b", "--dry-run"], &[]);
    assert!(code == 0 && said.contains("in account 2222222222222222222222222222222b"), "{said}");
    // Signed in to Cloudflare, with two accounts: which one is asked.
    let file = home.join("workspaces/default/sign-ins.json");
    let mut kept: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    kept["cloudflare"] = json!({ "access": "cloudflare-access-for-tests", "expires_at_ms": 4102444800000u64, "refresh": "cloudflare-refresh-for-tests" });
    std::fs::write(&file, kept.to_string()).unwrap();
    let accounts = json!({ "result": [{ "id": "1111111111111111111111111111111a", "name": "Acme" }, { "id": "2222222222222222222222222222222b", "name": "Acme Labs" }], "result_info": { "page": 1, "per_page": 50, "total_pages": 1, "count": 2, "total_count": 2 }, "success": true, "errors": [], "messages": [] });
    let scripts = json!({ "result": [], "success": true, "errors": [], "messages": [] });
    let line = |url: &str, answer: &Value| json!({ "method": "GET", "url": url, "target": "", "asked": "", "status": 200, "headers": [["content-type", "application/json"]], "answer": answer.to_string() }).to_string();
    let recording = [line("https://api.cloudflare.com/client/v4/accounts?per_page=50", &accounts),
        line("https://api.cloudflare.com/client/v4/accounts/1111111111111111111111111111111a/workers/scripts", &scripts), line("https://api.cloudflare.com/client/v4/accounts/2222222222222222222222222222222b/workers/scripts", &scripts)].join("\n");
    served.aws.replaying(&recording, &[]);
    let said = refused(&home, &["planes", "create", "cloudflare", "--dry-run"], &[]);
    assert!(said.contains("Say which account: --account=ID. Yours: 1111111111111111111111111111111a (Acme), 2222222222222222222222222222222b (Acme Labs)"), "{said}");
}

#[test]
fn runners_are_added_to_a_control_plane_outside_aws_where_it_is_said_and_in_the_account_named() {
    let served = serve(|_| {});
    let home = common::signed_in(&served);
    let file = home.join("workspaces/default/sign-ins.json");
    let mut kept: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    kept["plane"] = json!({ "Cloudflare": { "account_id": "1111111111111111111111111111111a", "account_name": "Acme", "script": "superci", "url": served.url, "plane_id": "testplane0001", "label": "superci" } });
    std::fs::write(&file, kept.to_string()).unwrap();
    let said = refused(&home, &["runners", "create", "aws", "--dry-run"], &[]);
    assert!(said.contains("Say where its machines start"), "{said}");
    for (args, would) in [(&["runners", "create", "aws", "--region=eu-west-1", "--dry-run"][..], "let aws run jobs"), (&["runners", "create", "cloudflare", "--account=1111111111111111111111111111111a", "--dry-run"], "let cloudflare run jobs")] {
        let (code, said) = cli(&home, args, &[]);
        assert!(code == 0 && said.contains(would), "{args:?}: {said}");
    }
}

#[test]
fn debug_commands_say_what_aws_or_cloudflare_refused_and_what_they_were_given_that_does_not_read() {
    let served = serve(|_| {});
    let home = common::signed_in(&served);
    let error = superci_testkit::ec2_error("InternalError", "An internal error has occurred");
    // A launch whose image, or whose network, AWS will not say; a disk given that does not read.
    served.aws.fail("DescribeImages", 500, &error, Some(1));
    refused(&home, &["debug", "launch"], &[]);
    served.aws.fail_none();
    served.aws.fail("DescribeSubnets", 500, &error, None);
    refused(&home, &["debug", "launch"], &[]);
    served.aws.fail_none();
    served.aws.replaying(include_str!("../fixtures/aws/launch/spot.jsonl"), &[]);
    for disk in ["--extra-disk=many:125", "--extra-disk=100:fast"] {
        let (code, said) = cli(&home, &["debug", "launch", disk], &[]);
        assert!(code == 0, "{disk}: {said}");
    }
    // Links for a bucket, and for one whose name makes no address.
    let (code, said) = cli(&home, &["debug", "presign", "GET", "us-east-1", "bucket-for-tests", "p", "1"], &[]);
    assert!(code == 0 && said.contains("p/000000"), "{said}");
    refused(&home, &["debug", "presign", "GET", "us-east-1", "bad[bucket", "p", "1"], &[]);
    // A signed call to an address that does not read, and to one nobody answers.
    refused(&home, &["debug", "rest", "us-east-1", "s3", "GET", "not an address"], &[]);
    refused(&home, &["debug", "rest", "us-east-1", "s3", "GET", "http://127.0.0.1:9/a.txt"], &[]);
    // Cloudflare: a sign-in it will not renew; a body that is not there, or not JSON; its analytics as it answers them.
    let file = home.join("workspaces/default/sign-ins.json");
    let mut kept: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    kept["cloudflare"] = json!({ "access": "cloudflare-access-for-tests", "expires_at_ms": 0, "refresh": "cloudflare-refresh-for-tests" });
    std::fs::write(&file, kept.to_string()).unwrap();
    refused(&home, &["debug", "cloudflare", "GET", "/accounts"], &[]);
    kept["cloudflare"]["expires_at_ms"] = 4_102_444_800_000u64.into();
    std::fs::write(&file, kept.to_string()).unwrap();
    refused(&home, &["debug", "cloudflare", "POST", "/graphql", "missing.json"], &[]);
    std::fs::write(home.join("not.json"), "not json").unwrap();
    refused(&home, &["debug", "cloudflare", "POST", "/graphql", "not.json"], &[]);
    std::fs::write(home.join("query.json"), json!({ "query": "{ viewer { accounts { id } } }", "variables": {} }).to_string()).unwrap();
    served.aws.replaying(&json!({ "method": "POST", "url": "https://api.cloudflare.com/client/v4/graphql", "target": "", "asked": "", "status": 200, "headers": [["content-type", "application/json"]],
        "answer": json!({ "data": { "viewer": { "accounts": [] } }, "errors": null }).to_string() }).to_string(), &[]);
    let (code, said) = cli(&home, &["debug", "cloudflare", "POST", "/graphql", "query.json"], &[]);
    assert!(code == 0 && said.contains("viewer"), "{said}");
}

#[test]
fn a_change_signed_in_where_no_control_plane_is_yet_says_there_is_none() {
    let account = superci_testkit::Aws::serve();
    let home = common::aws_only(&account);
    for args in [&["secrets", "create", "NPM_TOKEN", "--value-env=HOME"][..], &["secrets", "delete", "NPM_TOKEN", "--confirm"], &["connections", "delete", "gitlab", "--confirm"],
        &["connections", "create", "github", "acme", "--dry-run"], &["connections", "create", "gitlab"]] {
        let said = refused(&home, args, &[("SUPERCI_GITLAB_TOKEN", "glpat-for-tests")]);
        assert!(said.contains("There is no control plane yet"), "{args:?}: {said}");
    }
    // Not signed in at all, a GitLab token given.
    let served = serve(|_| {});
    refused(&common::home(&served), &["connections", "create", "gitlab"], &[("SUPERCI_GITLAB_TOKEN", "glpat-for-tests")]);
}

#[test]
fn a_secrets_value_that_cannot_be_read_is_said() {
    let served = serve(|_| {});
    let home = common::signed_in(&served);
    // Standard input that is not text.
    let mut child = common::command(&home).args(["secrets", "create", "NPM_TOKEN"]).stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).spawn().unwrap();
    child.stdin.take().unwrap().write_all(&[0xff, 0xfe, 0x00, 0x80]).unwrap();
    let o = child.wait_with_output().unwrap();
    assert!(!o.status.success() && !String::from_utf8_lossy(&o.stderr).trim().is_empty(), "{o:?}");
    // At a terminal, ended (Ctrl-D) before anything was typed. (`script` gives the command a terminal of its own.)
    let line = format!("{} secrets create TYPED", env!("CARGO_BIN_EXE_superci"));
    let args: Vec<&str> = if cfg!(target_os = "macos") { vec!["-q", "/dev/null", "sh", "-c", &line] } else { vec!["-qec", &line, "/dev/null"] };
    let mut typed = Command::new("script").args(&args).env("SUPERCI_HOME", &home).env("SUPERCI_STAND_IN", &served.aws.url).env_remove("SUPERCI_PLANE").env_remove("SUPERCI_KEY")
        .stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).spawn().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(500));
    typed.stdin.as_mut().unwrap().write_all(&[4]).unwrap();
    let out = typed.wait_with_output().unwrap();
    let said = String::from_utf8_lossy(&out.stdout);
    assert!(said.contains("The value could not be read"), "{said}");
}

#[test]
fn workspaces_that_cannot_be_kept_found_or_signed_out_say_why() {
    let served = serve(|_| {});
    let root = common::temp("workspaces-edges");
    let run = |args: &[&str], envs: &[(&str, &str)]| cli(&root, args, &[&[("SUPERCI_HOME", root.to_str().unwrap())][..], envs].concat());
    // No home folder at all.
    let o = Command::new(env!("CARGO_BIN_EXE_superci")).args(["workspaces", "list"]).env_remove("SUPERCI_HOME").env_remove("HOME").env_remove("USERPROFILE").output().unwrap();
    assert!(!o.status.success() && String::from_utf8_lossy(&o.stderr).contains("it has no home folder"), "{o:?}");
    // A name that is none; a home that is a file.
    let (code, said) = run(&["workspaces", "create", "7"], &[]);
    assert!(code != 0 && said.contains("A name is 2 to 24 letters"), "{said}");
    std::fs::write(root.join("file"), "").unwrap();
    let (code, said) = cli(&root, &["workspaces", "create", "Build", "Farm"], &[("SUPERCI_HOME", root.join("file").to_str().unwrap())]);
    assert!(code != 0 && !said.trim().is_empty(), "{said}");
    // An empty workspace asked to look after a control plane: nothing signed in to find it with.
    let (code, said) = run(&["workspaces", "create", "Build", "Farm"], &[]);
    assert!(code == 0, "{said}");
    let (code, said) = run(&["workspaces", "update", "build-farm", "--plane=testplane0001"], &[]);
    assert!(code != 0 && !said.trim().is_empty(), "{said}");
    // One whose sign-ins cannot be removed (its folder will not let them go).
    let ws = root.join("workspaces/build-farm");
    std::fs::write(ws.join("sign-ins.json"), std::fs::read_to_string(common::home(&served).join("workspaces/default/sign-ins.json")).unwrap()).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&ws, std::fs::Permissions::from_mode(0o555)).unwrap();
    let (code, said) = run(&["workspaces", "delete", "build-farm", "--confirm"], &[("SUPERCI_STAND_IN", &served.aws.url)]);
    std::fs::set_permissions(&ws, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(code != 0 && !said.trim().is_empty(), "{said}");
    // A dashboard for a workspace that is not here.
    let (code, said) = run(&["dashboard", "--workspace=nobody", "--no-browser"], &[]);
    assert!(code != 0 && said.contains("nobody"), "{said}");
}

/// One run of a sweep (see `sweep_commands.rs`): the fault armed, `command` run in `home`; then what must hold of it
/// whatever failed (no panic; a failure said), and a plain `superci status` working once the fault has been met.
fn swept(fault: Fault, served: Served, home: &Path, command: &[&str]) -> Outcome {
    served.arm(fault);
    let o = common::command(home).current_dir(home).args(command).output().unwrap();
    served.faults().stop();
    let (code, said) = (o.status.code().unwrap_or(-1), String::from_utf8_lossy(&o.stdout).to_string() + &String::from_utf8_lossy(&o.stderr));
    if std::env::var("SUPERCI_SWEEP_ONLY").is_ok() { eprintln!("$ superci {} → {code}\n{said}", command.join(" ")) }
    let out = Outcome::new(served);
    out.check(code >= 0 && !said.contains("panicked"), || format!("`{}` panicked or was killed: {said}", command.join(" ")));
    let error = serde_json::from_str::<Value>(&String::from_utf8_lossy(&o.stdout)).ok().and_then(|v| v["error"].as_str().map(str::to_string)).unwrap_or_else(|| said.clone());
    out.check(code == 0 || error.trim().len() > 8, || format!("`{}` failed ({code}) without saying what: {said}", command.join(" ")));
    let met = !out.served.faults().fired().is_empty();
    let after = common::command(home).current_dir(home).args(["status", "--json"]).output().unwrap();
    out.check(!met || after.status.success(), || format!("`superci status` fails after the fault: {}", String::from_utf8_lossy(&after.stderr)));
    let _ = std::fs::remove_dir_all(home);
    out
}

/// One sweep at a time: each runs several CLIs side by side already (more at once run this computer out of ports).
fn one_at_a_time() -> std::sync::MutexGuard<'static, ()> {
    static ONE: Mutex<()> = Mutex::new(());
    ONE.lock().unwrap_or_else(|e| e.into_inner())
}

/// The runner image's copies made, made anew from a newer build, and removed, as AWS answered a real account, with
/// each of AWS's and GitHub's answers failing in turn.
#[test]
fn the_runner_images_copies_hold_with_each_call_failing() {
    let _one = one_at_a_time();
    let fixture = |name: &str| std::fs::read_to_string(format!("{}/fixtures/aws/images/{name}.jsonl", env!("CARGO_MANIFEST_DIR"))).unwrap();
    // (A copy of an older build than the newest: an update copies the newest in, as the copy at first did.)
    let copy = fixture("01-create").lines().last().unwrap().to_string();
    let behind = fixture("05-update-newest").replace("x64-20261006080844", "x64-20250101000000") + "\n" + &copy;
    for (recording, command) in [(fixture("01-create"), &["images", "create", "ca-central-1", "--json"][..]), (behind, &["images", "update", "ca-central-1", "--json"]), (fixture("06-delete"), &["images", "delete", "ca-central-1", "--confirm", "--json"])] {
        sweep(&format!("superci {}", command.join(" ")), |fault| {
            let served = serve(|_| {});
            let home = common::signed_in(&served);
            served.aws.replaying(&recording, &[]);
            swept(fault, served, &home, command)
        });
    }
}

/// Changes whose control plane is read again after them, the control plane no longer taking this computer's key
/// from its first read on, or at its last (as one whose key was replaced meanwhile): what failed is said.
#[test]
fn changes_read_back_after_they_are_made_say_so_when_the_control_plane_stops_taking_the_key_midway() {
    for command in [&["accounts", "update", "aws", "--monthly-usd=80"][..], &["labels", "create", "gpu"], &["name", "update", "Acme"], &["machine", "update", "--cpu=4"],
        &["limits", "update", "--monthly-usd=100"], &["connections", "delete", "github", "acme-corp", "--confirm"]] {
        // (Its reads of how the control plane stands counted; from the `from`-th on, refused.)
        let run = |from: usize| -> (usize, i32, String) {
            let served = serve(|_| {});
            let home = common::signed_in(&served);
            served.connect_github();
            let mut more = superci_testkit::GitHub::app();
            (more["id"], more["owner"], more["slug"]) = (77.into(), "acme-corp".into(), "superci-acme-corp".into());
            served.aws.put(&format!("/superci/{}/GITHUB_APP_77", superci_testkit::PLANE), &more.to_string());
            let reads = Arc::new(Mutex::new(0usize));
            let counted = reads.clone();
            let plane = superci_testkit::front(&served.url, move |method, target, status, body| {
                if method != "GET" || !target.starts_with("/status") { return (status, body) }
                let mut n = counted.lock().unwrap();
                *n += 1;
                if *n >= from { (401, br#"{"error":"this key is not taken here"}"#.to_vec()) } else { (status, body) }
            });
            common::kept_at(&home, &plane);
            let (code, said) = cli(&home, command, &[("SUPERCI_PAUSE_MS", "0")]);
            let n = *reads.lock().unwrap();
            let _ = std::fs::remove_dir_all(&home);
            (n, code, said)
        };
        let (reads, code, said) = run(usize::MAX);
        assert!(code == 0, "{command:?}: {said}");
        // (Refused from its first read on, and from its last: the one after the change.)
        for from in [1, reads] {
            let (_, code, said) = run(from);
            assert!(code == 0 || said.trim().trim_start_matches("superci:").trim().len() > 8, "{command:?}, refused from read {from} of {reads} on ({code}): {said}");
        }
    }
}

/// The CLI in workspace `home` (in its folder) with these settings of its own: how it ended, and what it said (out,
/// then errors). It never panics.
fn cli(home: &Path, args: &[&str], envs: &[(&str, &str)]) -> (i32, String) {
    let o = common::command(home).current_dir(home).args(args).envs(envs.iter().copied()).output().unwrap();
    let said = String::from_utf8_lossy(&o.stdout).to_string() + &String::from_utf8_lossy(&o.stderr);
    assert!(!said.contains("panicked"), "{args:?}: {said}");
    (o.status.code().unwrap_or(-1), said)
}

/// The same, failing and saying why: what it said.
fn refused(home: &Path, args: &[&str], envs: &[(&str, &str)]) -> String {
    let (code, said) = cli(home, args, envs);
    assert!(code != 0 && said.trim().trim_start_matches("superci:").trim().len() > 8, "{args:?} ({code}): {said}");
    said
}

#[test]
fn what_a_command_needs_that_the_workspace_has_not_is_said_and_nothing_is_done() {
    let served = serve(|_| {});
    let (empty, key_only) = (common::temp("empty-home"), common::home(&served));
    std::fs::write(key_only.join("aws-at"), &served.aws.url).unwrap();
    std::fs::write(empty.join("aws-at"), &served.aws.url).unwrap();
    let debug: [&[&str]; 9] = [&["debug", "abandon", "us-east-1", "testplane0009"], &["debug", "networks", "us-east-1"], &["debug", "budget"], &["debug", "launch"], &["debug", "presign", "GET", "us-east-1", "bucket", "p", "1"],
        &["debug", "rest", "us-east-1", "s3", "GET", "https://bucket.s3.us-east-1.amazonaws.com/"], &["debug", "ec2", "us-east-1", "DescribeVpcs"], &["debug", "cloudflare", "GET", "/accounts"], &["debug", "runner-service", "missing.json", "1", "http://127.0.0.1:9", "0", "jit"]];
    // Nothing kept: no control plane to look at, no sign-in to change one with.
    for args in debug.iter().copied().chain([&["runs", "create", WORKFLOW][..], &["accounts", "update", "aws", "--monthly-usd=5"], &["secrets", "delete", "NPM_TOKEN", "--confirm"],
        &["secrets", "create", "NPM_TOKEN", "--value-env=HOME"], &["workspaces", "update", "nobody", "--plane=testplane0001"]]) {
        refused(&empty, args, &[]);
    }
    // Only this computer's key: no cloud sign-in to change anything with.
    for args in debug.iter().copied().chain([&["accounts", "update", "aws", "--monthly-usd=5"][..], &["accounts", "retrieve", "aws"], &["images", "list"], &["images", "create", "us-east-1"],
        &["images", "update", "us-east-1"], &["images", "delete", "us-east-1", "--confirm"], &["secrets", "delete", "NPM_TOKEN", "--confirm"], &["secrets", "create", "NPM_TOKEN", "--value-env=HOME"],
        &["runners", "order", "cloudflare"], &["runners", "update", "cloudflare", "--max-jobs=3"], &["machine", "update", "--cpu=4"], &["limits", "update", "--monthly-usd=5"],
        &["connections", "delete", "gitlab", "--confirm"], &["jobs", "retrieve", "99999"]]) {
        refused(&key_only, args, &[]);
    }
    // A control plane named by the variable alone (no key), one that does not answer, and one the flag names otherwise.
    refused(&empty, &["runs", "list"], &[("SUPERCI_PLANE", "testplane0009")]);
    refused(&empty, &["runs", "list"], &[("SUPERCI_PLANE", "http://127.0.0.1:9"), ("SUPERCI_KEY", "superci_run_key_test")]);
    let given = [("SUPERCI_PLANE", served.url.as_str()), ("SUPERCI_KEY", served.run_key.as_str())];
    let said = refused(&empty, &["runs", "list", "--plane=testplane0009"], &given);
    assert!(said.contains("--plane names the control plane testplane0009"), "{said}");
    // What a run from a computer is given that does not read.
    for (args, says) in [(&["runs", "create"][..], "Say which workflow"), (&["runs", "create", WORKFLOW, "--secret=TOKEN"], "--secret is NAME=VALUE"), (&["runs", "create", WORKFLOW, "--input=WHERE"], "--input is NAME=VALUE"),
        (&["projects", "update", "1", "--enabled=maybe"], "--enabled is true or false"), (&["accounts", "update", "aws", "--monthly-usd=lots"], "A limit is dollars"), (&["accounts", "update", "aws", "--daily-usd=lots"], "A limit is dollars"),
        (&["limits", "update", "--daily-usd=lots"], "A limit is dollars")] {
        let said = refused(&common::signed_in(&served), args, &[]);
        assert!(said.contains(says), "{args:?}: {said}");
    }
}

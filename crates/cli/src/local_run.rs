//! `superci runs create`: a workflow's job run from this computer, as GitHub would run it, with the working tree as it
//! is and nothing pushed. The tree goes to the control plane in parts, the control plane starts a machine as for any
//! job, and the machine's runner takes the job from the control plane (see `superci_core::runner_service`). This
//! waits for the job and prints its lines.
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use futures::executor::block_on;
use serde_json::{json, Value};
use superci_core::io::{Http, Request};

use crate::commands::Done;

type Result<T> = std::result::Result<T, String>;

/// A part of the tree is at most this much (the control plane takes a request of a few MB).
const PART: usize = 3_000_000;
/// The tree, packed, is at most this much when it goes through the control plane's store, and when it goes to its
/// bucket.
const MOST: usize = 150_000_000;
const MOST_IN_BUCKET: usize = 1_000_000_000;
/// The first control plane that runs a workflow from a computer and keeps its artifacts.
pub const SINCE: &str = "0.18.0";

/// Whether the control plane at `plane` knows runs from a computer: one of an older version answers "not here".
pub fn new_enough(version: Option<&str>) -> Result<()> {
    match version {
        Some(v) if crate::view::older(Some(v), SINCE) => Err(format!("The control plane is at {v}; runs from a computer need {SINCE} or newer. Update it: `superci planes update`.")),
        _ => Ok(()),
    }
}

fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git").current_dir(dir).args(args).output().map_err(|e| format!("git: {e}"))?;
    if !out.status.success() { return Err(format!("git {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim())) }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// The working tree's files: what git tracks and the new files it does not ignore (an ignored `.env` stays here).
fn tree_files(root: &Path) -> Result<Vec<Vec<u8>>> {
    let listed = Command::new("git").current_dir(root).args(["ls-files", "-z", "--cached", "--others", "--exclude-standard"]).output().map_err(|e| format!("git: {e}"))?;
    Ok(listed.stdout.split(|b| *b == 0).filter(|f| !f.is_empty()).filter(|f| root.join(String::from_utf8_lossy(f).as_ref()).symlink_metadata().is_ok_and(|m| !m.is_dir())).map(<[u8]>::to_vec).collect())
}

/// Each file's content hash (a link's: where it points), by its path: what a base kept before is told apart by.
fn hashes(root: &Path, files: &[Vec<u8>]) -> std::collections::BTreeMap<String, (String, u64)> {
    use sha2::Digest;
    files.iter().map(|f| {
        let path = String::from_utf8_lossy(f).into_owned();
        let at = root.join(&path);
        let (bytes, size) = match std::fs::read_link(&at) { Ok(to) => (format!("link:{}", to.display()).into_bytes(), 0), Err(_) => { let b = std::fs::read(&at).unwrap_or_default(); let n = b.len() as u64; (b, n) } };
        #[cfg(unix)]
        let mode = at.symlink_metadata().map(|m| { use std::os::unix::fs::PermissionsExt; m.permissions().mode() & 0o111 }).unwrap_or(0);
        #[cfg(not(unix))]
        let mode = 0u32;
        let mut h = sha2::Sha256::new();
        h.update(&bytes);
        h.update(mode.to_le_bytes());
        (path, (format!("{:x}", h.finalize()), size))
    }).collect()
}

/// The files given of the working tree as a gzipped tar, as they are on disk now, with `removed` (paths
/// gone since a base) as `.superci-removed`. By the system's own tar.
fn pack_files(root: &Path, files: &[Vec<u8>], removed: &[String]) -> Result<Vec<u8>> {
    if removed.is_empty() { return tar_of(root, files, true) }
    // What was removed since the base, as an archive of its own after this one, the two compressed as one (the
    // machine reads both in turn, past the end of the first).
    let mut raw = tar_of(root, files, false)?;
    let extra = std::env::temp_dir().join(format!("superci-removed-{}", superci_core::crypto::random_token(8)));
    std::fs::create_dir_all(&extra).map_err(|e| e.to_string())?;
    std::fs::write(extra.join(".superci-removed"), removed.join("\n") + "\n").map_err(|e| e.to_string())?;
    let list = tar_of(&extra, &[b".superci-removed".to_vec()], false);
    let _ = std::fs::remove_dir_all(&extra);
    raw.extend(list?);
    let mut gz = Command::new("gzip").arg("-c").stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().map_err(|e| format!("gzip: {e}"))?;
    let mut stdin = gz.stdin.take().ok_or("gzip took no input")?;
    let writer = std::thread::spawn(move || stdin.write_all(&raw));
    let out = gz.wait_with_output().map_err(|e| format!("gzip: {e}"))?;
    let _ = writer.join();
    if !out.status.success() { return Err("gzip failed".into()) }
    Ok(out.stdout)
}

fn tar_of(root: &Path, files: &[Vec<u8>], compress: bool) -> Result<Vec<u8>> {
    let files: Vec<&[u8]> = files.iter().map(Vec::as_slice).collect();
    let args = [if compress { "-czf" } else { "-cf" }, "-", "--no-xattrs", "--null", "-T", "-"];
    let mut tar = Command::new("tar").current_dir(root).args(args).env("COPYFILE_DISABLE", "1")
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().map_err(|e| format!("tar: {e}"))?;
    let list: Vec<u8> = files.iter().flat_map(|f| f.iter().copied().chain([0])).collect();
    let mut stdin = tar.stdin.take().ok_or("tar took no input")?;
    let writer = std::thread::spawn(move || stdin.write_all(&list));
    let out = tar.wait_with_output().map_err(|e| format!("tar: {e}"))?;
    let _ = writer.join();
    if !out.status.success() { return Err(format!("tar: {}", String::from_utf8_lossy(&out.stderr).trim())) }
    Ok(out.stdout)
}

/// What this computer last kept as a base for a repository's runs on a control plane: its name and each file's hash.
#[derive(serde::Serialize, serde::Deserialize, Default)]
struct Base { name: String, files: std::collections::BTreeMap<String, (String, u64)> }

fn base_path(plane: &str, root: &Path) -> Option<std::path::PathBuf> {
    let key = superci_core::crypto::sha256_hex(format!("{plane}\n{}", root.display()).as_bytes());
    Some(crate::store::root()?.join("trees").join(format!("{}.json", &key[..32])))
}

/// `owner/name` of the repository this checkout's `origin` is on GitHub (else the folder's name under `local/`).
fn repository(root: &Path) -> String {
    let url = git(root, &["remote", "get-url", "origin"]).unwrap_or_default();
    let path = url.trim_end_matches(".git").rsplit_once("github.com").map(|(_, p)| p.trim_start_matches([':', '/']).to_string()).unwrap_or_default();
    if path.split('/').count() == 2 { path } else { format!("local/{}", root.file_name().and_then(|n| n.to_str()).unwrap_or("repository")) }
}

/// How a run is followed: `quiet`, its jobs' lines are not printed (each step's result is, and the end of a failed
/// one's log); `detach`, not at all (its id is said, for `superci runs wait`).
#[derive(Clone, Copy, Default)]
pub struct Follow { pub quiet: bool, pub detach: bool, pub dry_run: bool }

pub fn create(plane: &str, key: &str, workflow: &str, job: Option<&str>, event: &str, secrets: &[(String, String)], vars: &[(String, String)], how: Follow, say: &mut dyn FnMut(&str)) -> Result<Done> {
    let here = std::env::current_dir().map_err(|e| e.to_string())?;
    let root = std::path::PathBuf::from(git(&here, &["rev-parse", "--show-toplevel"]).map_err(|_| "Run it in a git checkout: the run takes its working tree.".to_string())?);
    let file = here.join(workflow);
    let text = std::fs::read_to_string(&file).map_err(|e| format!("{workflow}: {e}"))?;
    let path = file.canonicalize().ok().and_then(|f| f.strip_prefix(root.canonicalize().ok()?).ok().map(|p| p.to_string_lossy().replace('\\', "/"))).unwrap_or_else(|| workflow.to_string());
    new_enough(crate::cloudflare::health(plane).as_ref().and_then(|h| h["version"].as_str()))?;
    if how.dry_run { return dry_run(&root, &path, &text, job) }
    let http = crate::aws::Blocking::new();
    let call = |method: &str, at: &str, body: Vec<u8>| -> Result<Value> {
        let r = Request::new(method, &format!("{plane}{at}")).with_header("authorization", &format!("Bearer {key}")).with_body(body);
        let res = block_on(http.send(r))?;
        if res.status >= 300 { return Err(format!("the control plane said {}: {}", res.status, res.body_text().trim())) }
        serde_json::from_slice(&res.body).map_err(|e| format!("the control plane's answer: {e}"))
    };
    let tree = format!("tree-{}", superci_core::crypto::random_token(18));
    let put = |link: &str, bytes: Vec<u8>| -> Result<()> {
        let res = block_on(http.send(Request::new("PUT", link).with_body(bytes)))?;
        if res.status >= 300 { return Err(format!("the bucket said {} to the working tree: {}", res.status, res.body_text().trim())) }
        Ok(())
    };
    // Only what changed since the base this computer kept for the repository, when the control plane still has it
    // and that is less than half of the tree: the machine puts the base in place, then the changes.
    let files = tree_files(&root)?;
    let now = hashes(&root, &files);
    let kept_at = base_path(plane, &root);
    let kept: Option<Base> = kept_at.as_ref().and_then(|p| std::fs::read(p).ok()).and_then(|b| serde_json::from_slice(&b).ok());
    let mut sent: Option<(u32, bool)> = None;
    // Into the bucket: at once, or while the run's machine starts when the control plane can have the machine wait
    // for it (`later`: it says so); then the base is kept, when it was one.
    let mut pending: Option<(String, Vec<u8>, bool)> = None;
    if let Some(base) = &kept {
        let changed: Vec<Vec<u8>> = files.iter().filter(|f| { let p = String::from_utf8_lossy(f); base.files.get(p.as_ref()) != now.get(p.as_ref()) }).cloned().collect();
        let removed: Vec<String> = base.files.keys().filter(|p| !now.contains_key(*p)).cloned().collect();
        let (some, all) = (changed.iter().map(|f| now.get(String::from_utf8_lossy(f).as_ref()).map(|h| h.1).unwrap_or(0)).sum::<u64>(), now.values().map(|h| h.1).sum::<u64>());
        if some * 2 < all.max(1) {
            let asked = call("POST", &format!("/runs/tree?id={tree}&part=1&link=1&later=1&base={}", base.name), vec![])?;
            if let (Some(link), true) = (asked["url"].as_str(), asked["base"] == true) {
                let packed = pack_files(&root, &changed, &removed)?;
                say(&format!("Sending what changed in the working tree ({} file{}, {} removed, {} KB packed)", changed.len(), if changed.len() == 1 { "" } else { "s" }, removed.len(), packed.len().div_ceil(1000)));
                if asked["later"] == true { pending = Some((link.to_string(), packed, false)) } else { put(link, packed)? }
                sent = Some((2, true));
            }
        }
    }
    // Else all of it, into the control plane's bucket in one when it has one (as a build service takes its source),
    // kept there as the base for the next runs'; else through the control plane, in parts (it takes a request of a
    // few MB).
    let packed = if sent.is_some() { vec![] } else { pack_files(&root, &files, &[])? };
    let name = format!("b{}", &superci_core::crypto::sha256_hex(serde_json::to_string(&now).unwrap_or_default().as_bytes())[..40]);
    let asked = if sent.is_some() { Value::Null } else { call("POST", &format!("/runs/tree?id={tree}&part=0&link=1&later=1&keep={name}"), vec![])? };
    let link = asked["url"].as_str().map(str::to_string);
    let most = if link.is_some() || sent.is_some() { MOST_IN_BUCKET } else { MOST };
    if packed.len() > most { return Err(format!("The working tree is {} MB packed; a run from a computer takes {} MB at most.", packed.len() / 1_000_000, most / 1_000_000)) }
    let keep_base = |name: &str, now: &std::collections::BTreeMap<String, (String, u64)>| if let Some(at) = &kept_at {
        let _ = std::fs::create_dir_all(at.parent().unwrap_or(Path::new(".")));
        let _ = std::fs::write(at, serde_json::to_vec(&Base { name: name.to_string(), files: now.clone() }).unwrap_or_default());
    };
    let (parts, layered) = match (&link, sent) {
        (_, Some(sent)) => sent,
        (Some(link), None) => {
            say(&format!("Sending the working tree ({} KB packed)", packed.len().div_ceil(1000)));
            if asked["later"] == true { pending = Some((link.clone(), packed, true)) } else { put(link, packed)?; keep_base(&name, &now) }
            (1, false)
        }
        (None, None) => {
            let parts = packed.chunks(PART).count() as u32;
            say(&format!("Sending the working tree ({} KB packed, {parts} part{})", packed.len().div_ceil(1000), if parts == 1 { "" } else { "s" }));
            for (i, part) in packed.chunks(PART).enumerate() { call("POST", &format!("/runs/tree?id={tree}&part={i}"), part.to_vec())?; }
            (parts, false)
        }
    };
    let reference = git(&root, &["symbolic-ref", "-q", "HEAD"]).unwrap_or_else(|_| "refs/heads/main".into());
    // The workflows its jobs call (`uses: ./.github/workflows/x.yml`), from the working tree as it is.
    let mut called = serde_json::Map::new();
    if let Ok(wf) = superci_core::workflow::parse(&text) {
        for (_, j) in wf.get("jobs").map(|j| j.pairs().to_vec()).unwrap_or_default() {
            let Some(p) = j.get("uses").and_then(|u| u.text()).and_then(|u| u.strip_prefix("./")).filter(|p| !p.contains('@')) else { continue };
            if called.contains_key(p) { continue }
            let file = root.join(p);
            let t = std::fs::read_to_string(&file).map_err(|e| format!("{p}, which a job calls: {e}"))?;
            called.insert(p.to_string(), t.into());
        }
    }
    let body = json!({ "workflow": text, "path": path, "job": job, "repository": repository(&root), "ref": reference, "sha": git(&root, &["rev-parse", "HEAD"]).unwrap_or_default(),
        "actor": git(&root, &["config", "user.name"]).unwrap_or_else(|_| "superci".into()), "event_name": event, "secrets": secrets, "vars": vars, "tree": tree, "parts": parts, "layered": layered, "called": called });
    let made = call("POST", "/runs", body.to_string().into_bytes())?;
    let run = made["run_id"].as_str().unwrap_or_default().to_string();
    // The tree still to send goes while the machine starts; the control plane is told once it is all there. Not
    // sent: the run is cancelled (its machine would wait for it).
    let sending = pending.map(|(link, bytes, base)| {
        let (plane, key, run, tree) = (plane.to_string(), key.to_string(), run.clone(), tree.clone());
        std::thread::spawn(move || -> Result<bool> {
            let http = crate::aws::Blocking::new();
            let ask = |method: &str, at: &str, body: Vec<u8>| block_on(http.send(Request::new(method, at).with_header("authorization", &format!("Bearer {key}")).with_body(body)));
            let sent = block_on(http.send(Request::new("PUT", &link).with_body(bytes))).and_then(|r| if r.status < 300 { Ok(()) } else { Err(format!("the bucket said {} to the working tree: {}", r.status, r.body_text().trim())) })
                .and_then(|_| ask("POST", &format!("{plane}/runs/tree?id={tree}&part=0&sent=1"), vec![]).and_then(|r| if r.status < 300 { Ok(()) } else { Err(format!("the control plane said {} to the working tree's end: {}", r.status, r.body_text().trim())) }));
            if sent.is_err() { let _ = ask("POST", &format!("{plane}/runs/{run}/cancel"), b"{}".to_vec()); }
            sent.map(|_| base)
        })
    });
    let sent = |sending: Option<std::thread::JoinHandle<Result<bool>>>| -> Result<()> {
        match sending.map(|t| t.join().unwrap_or_else(|_| Err("sending the working tree stopped".into()))) {
            Some(Ok(true)) => { keep_base(&name, &now); Ok(()) }
            Some(Err(e)) => Err(format!("The working tree was not sent ({e}); run {run} was cancelled.")),
            _ => Ok(()),
        }
    };
    if how.detach {
        sent(sending)?;
        return Ok(Done { said: vec![format!("Run {run} of {path} started. Follow it: superci runs wait {run}")], data: json!({ "run_id": run, "jobs": made["jobs"] }) })
    }
    say(&format!("Run {run} of {path}, on machines of this control plane's"));
    let followed = follow(plane, key, &run, how, std::time::Instant::now(), true, say);
    sent(sending)?;
    followed
}

/// `superci runs create --dry-run`: what would run, after what can be checked here was (the workflow, its job, the
/// control plane, the working tree), with nothing sent and no machine started.
fn dry_run(root: &Path, path: &str, text: &str, job: Option<&str>) -> Result<Done> {
    let wf = superci_core::workflow::parse(text).map_err(|e| format!("{path}: {e}"))?;
    let jobs: Vec<String> = wf.get("jobs").map(|j| j.pairs().iter().filter_map(|(k, _)| k.text().map(str::to_string)).collect()).unwrap_or_default();
    if jobs.is_empty() { return Err(format!("{path} has no jobs.")) }
    if let Some(j) = job.filter(|j| !jobs.iter().any(|k| k == j)) { return Err(format!("{path} has no job {j} (its jobs: {}).", jobs.join(", "))) }
    let files = tree_files(root)?;
    let bytes: u64 = files.iter().filter_map(|f| root.join(String::from_utf8_lossy(f).as_ref()).symlink_metadata().ok()).map(|m| m.len()).sum();
    let which = match job { Some(j) => format!("its job {j} (and the jobs it needs)"), None => format!("its job{} {}", if jobs.len() == 1 { "" } else { "s" }, jobs.join(", ")) };
    let would = format!("send the working tree ({} files, {} MB before packing) and run {path}: {which}, on this control plane's machines", files.len(), bytes.div_ceil(1_000_000));
    Ok(Done { said: vec![format!("Would {would}. Nothing was sent and no machine was started.")], data: json!({ "dry_run": true, "would": would, "jobs": jobs, "files": files.len(), "bytes": bytes }) })
}

/// `superci runs wait ID`: a run from a computer followed to its end, as `runs create` follows the one it starts.
pub fn wait(plane: &str, key: &str, run: &str, how: Follow, say: &mut dyn FnMut(&str)) -> Result<Done> {
    if run.is_empty() || !run.bytes().all(|b| b.is_ascii_digit()) { return Err("Say which run: `superci runs wait ID` (ids are in `superci runs list`).".into()) }
    follow(plane, key, run, how, std::time::Instant::now(), false, say)
}

fn follow(plane: &str, key: &str, run: &str, how: Follow, began: std::time::Instant, sent_here: bool, say: &mut dyn FnMut(&str)) -> Result<Done> {
    let http = crate::aws::Blocking::new();
    let call = |method: &str, at: &str| -> Result<Value> {
        let r = Request::new(method, &format!("{plane}{at}")).with_header("authorization", &format!("Bearer {key}"));
        let res = block_on(http.send(r))?;
        if res.status == 404 { return Err(format!("No run {run} among this control plane's runs from a computer (they are kept a day).")) }
        if res.status >= 300 { return Err(format!("the control plane said {}: {}", res.status, res.body_text().trim())) }
        serde_json::from_slice(&res.body).map_err(|e| format!("the control plane's answer: {e}"))
    };
    let (mut after, mut last): (Vec<u64>, Vec<String>) = (vec![], vec![]);
    let mut logs: Vec<Lines> = vec![];
    let mut ended_at: Vec<Option<std::time::Instant>> = vec![];
    let mut over_at: Option<std::time::Instant> = None;
    loop {
        std::thread::sleep(std::time::Duration::from_millis(1500));
        let cursor = after.iter().map(|n| n.to_string()).collect::<Vec<_>>().join(",");
        let st = call("GET", &format!("/runs/{run}?after={cursor}"))?;
        let jobs = st["jobs"].as_array().cloned().unwrap_or_default();
        after.resize(jobs.len(), 0);
        last.resize(jobs.len(), String::new());
        logs.resize_with(jobs.len(), Lines::default);
        ended_at.resize(jobs.len(), None);
        let several = jobs.len() > 1;
        for (i, j) in jobs.iter().enumerate() {
            let name = j["name"].as_str().unwrap_or_default();
            let mark = if several { format!("[{name}] ") } else { String::new() };
            // How each job stands, when that changes.
            let state = match (j["state"].as_str().unwrap_or_default(), j["result"].as_str()) {
                (_, Some("skipped")) => "skipped".to_string(),
                ("pending", _) => String::new(),
                ("launching", _) => "asking for its machine".to_string(),
                ("launched", _) => format!("its machine is starting ({})", j["machine"].as_str().unwrap_or("")),
                ("running", _) => "running".to_string(),
                // (With the control plane's word for why, when the job ended without its runner: no machine began it.)
                (_, Some(r)) => match j["error"].as_str().filter(|e| !e.is_empty()) { Some(e) => format!("ended: {r}: {e}"), None => format!("ended: {r}") },
                (other, _) => other.to_string(),
            };
            // (Its end is said after its lines: once each step's are out, or 15 seconds after it ended at most.)
            let ended = j["result"].is_string() && state != "skipped";
            if ended && ended_at[i].is_none() { ended_at[i] = Some(std::time::Instant::now()) }
            let give_up = ended_at[i].is_some_and(|t| t.elapsed().as_secs() > 15) || (st["over"] == true && over_at.is_some_and(|t| t.elapsed().as_secs() > 15));
            let said_state = |say: &mut dyn FnMut(&str), state: String, last: &mut String| if !state.is_empty() && state != *last {
                // Running: how its start went, in a line (where the time from sending it to its first step went).
                let start = if state == "running" { start_line(&j["start"]) } else { String::new() };
                say(&format!("{mark}{}{}{start}", &state[..1].to_uppercase(), &state[1..]));
                *last = state
            };
            if !ended { said_state(say, state.clone(), &mut last[i]) }
            // Its lines as GitHub shows them (a group's heading is a line, its end is nothing), each step's whole: what
            // the runner sent as it went while nothing is missing, then the rest from the step's own log once it ends
            // (the runner drops lines it cannot send as fast as they come).
            let fetch = |n: u64| -> Option<String> {
                let r = Request::new("GET", &format!("{plane}/runs/{run}/log?job={i}&log={n}")).with_header("authorization", &format!("Bearer {key}"));
                block_on(http.send(r)).ok().filter(|r| r.status < 300).map(|r| r.body_text())
            };
            let pages = |step: &str| -> Option<String> {
                let r = Request::new("GET", &format!("{plane}/runs/{run}/log?job={i}&step={step}")).with_header("authorization", &format!("Bearer {key}"));
                block_on(http.send(r)).ok().filter(|r| r.status < 300).map(|r| r.body_text())
            };
            for line in logs[i].take(j, give_up, &fetch, &pages) {
                if line.starts_with("##[endgroup]") || how.quiet { continue }
                say(&format!("{}{mark}{}", crate::commands::JOB_LINE, line.strip_prefix("##[group]").unwrap_or(&line)))
            }
            if ended && (logs[i].complete() || give_up) { said_state(say, state, &mut last[i]) }
            after[i] = j["next"].as_u64().unwrap_or(after[i]);
        }
        // Over: once every step's lines are out (its log may come a moment after), or a little later at most.
        if st["over"] == true && over_at.is_none() { over_at = Some(std::time::Instant::now()) }
        if st["over"] == true && logs.iter().any(|l| !l.complete()) && over_at.is_some_and(|t| t.elapsed().as_secs() <= 15) { continue }
        if st["over"] == true {
            let (result, took) = (st["result"].as_str().unwrap_or_default().to_string(), began.elapsed().as_secs());
            // Each job's steps, how each went and how long it took; a failed step's last lines when they were not
            // printed as they came.
            let usd: f64 = jobs.iter().filter_map(|j| j["usd"].as_f64()).sum::<f64>() + 0.0;
            for (i, j) in jobs.iter().enumerate() {
                say(&format!("{}{}{} ({}{}{}): {}", crate::commands::JOB_LINE, j["name"].as_str().unwrap_or("job"), j["job_id"].as_str().map(|id| format!(" · job {id}")).unwrap_or_default(), j["machine"].as_str().unwrap_or("no machine"),
                    j["took_ms"].as_u64().map(|ms| format!(", {}", span(ms / 1000))).unwrap_or_default(), j["usd"].as_f64().map(|u| format!(", ${u:.3}")).unwrap_or_default(),
                    match (j["result"].as_str().unwrap_or("?"), j["error"].as_str().filter(|e| !e.is_empty())) { (r, Some(e)) => format!("{r} ({e})"), (r, None) => r.to_string() }));
                let mut steps: Vec<&Value> = j["steps"].as_array().into_iter().flatten().collect();
                steps.sort_by_key(|s| s["order"].as_u64().unwrap_or(0));
                // Quiet, each step's whole log is read for its tests (its own log, else what its machine sent of it).
                let step_text = |s: &Value| -> String {
                    let at = match s["log"].as_u64() { Some(n) => format!("log={n}"), None => format!("step={}&lines=1", s["id"].as_str().unwrap_or_default()) };
                    let r = Request::new("GET", &format!("{plane}/runs/{run}/log?job={i}&{at}")).with_header("authorization", &format!("Bearer {key}"));
                    block_on(http.send(r)).ok().filter(|r| r.status < 300).map(|r| r.body_text()).unwrap_or_default()
                };
                let mut all = String::new();
                for s in steps {
                    let mark = match s["result"].as_str() { Some("succeeded") => "✓", Some("failed") => "✗", Some("skipped") | Some("cancelled") => "–", _ => "·" };
                    let time = match (s["start"].as_str().and_then(crate::log_view::seconds), s["finish"].as_str().and_then(crate::log_view::seconds)) { (Some(a), Some(b)) => format!(" {}", span(b.saturating_sub(a))), _ => String::new() };
                    say(&format!("{}  {mark} {}{time}", crate::commands::JOB_LINE, s["name"].as_str().unwrap_or("step")));
                    if !how.quiet || ["skipped", "cancelled"].contains(&s["result"].as_str().unwrap_or_default()) { continue }
                    let text = step_text(s);
                    if s["result"] == "failed" {
                        // The tests it failed, each with its whole error (its last lines may be another's, or a
                        // teardown's), then those lines.
                        for f in &crate::test_results::read(&text).failures {
                            say(&format!("{}    ✗ {}{}", crate::commands::JOB_LINE, f.name, f.file.as_deref().map(|p| format!(" ({p})")).unwrap_or_default()));
                            for l in f.error.lines() { say(&format!("{}        {l}", crate::commands::JOB_LINE)) }
                        }
                        let lines: Vec<&str> = text.lines().collect();
                        for l in &lines[lines.len().saturating_sub(40)..] { say(&format!("{}    {}", crate::commands::JOB_LINE, without_time(l))) }
                    }
                    all.push_str(&text);
                    all.push('\n');
                }
                if let (Some(id), true) = (j["job_id"].as_str(), j["result"] == "failure") { say(&format!("{}  Its whole log: superci jobs retrieve {id} --out=job.log", crate::commands::JOB_LINE)) }
                // What its tests came to, passed or not, and what a runner said of them as a whole.
                let tests = crate::test_results::read(&all);
                if !tests.runners.is_empty() {
                    say(&format!("{}  Tests: {} failed, {} passed{} ({})", crate::commands::JOB_LINE, tests.failed, tests.passed, if tests.skipped > 0 { format!(", {} skipped", tests.skipped) } else { String::new() }, tests.runners.join(", ")));
                    for l in crate::commands::ran_lines(&serde_json::to_value(&tests).unwrap_or_default(), "  ") { say(&format!("{}{l}", crate::commands::JOB_LINE)) }
                    for n in &tests.notes { say(&format!("{}  {n}", crate::commands::JOB_LINE)) }
                }
            }
            let said = format!("The run ended: {result}, {} after it was {}, ${usd:.3} of machines.", span(took), if sent_here { "sent" } else { "followed from here" });
            // (Not a success: the same data, as a failure's, with its job ids for a script.)
            let ok = result == "success";
            return Ok(Done { said: vec![said.clone()], data: json!({ "ok": ok, "error": if ok { Value::Null } else { said.into() }, "run_id": run, "result": result, "seconds": took, "usd": usd, "jobs": jobs.iter().map(|j| json!({ "name": j["name"], "job_id": j["job_id"], "result": j["result"], "machine": j["machine"], "usd": j["usd"], "took_ms": j["took_ms"],
                "steps": j["steps"].as_array().into_iter().flatten().map(|s| json!({ "name": s["name"], "result": s["result"], "start": s["start"], "finish": s["finish"] })).collect::<Vec<_>>() })).collect::<Vec<_>>() }) })
        }
        if began.elapsed().as_secs() > 6 * 3600 { return Err(format!("Still not over after six hours: `superci runs retrieve {run}`.")) }
    }
}

fn get(plane: &str, key: &str, at: &str) -> Result<Value> {
    let r = Request::new("GET", &format!("{plane}{at}")).with_header("authorization", &format!("Bearer {key}"));
    let res = block_on(crate::aws::Blocking::new().send(r))?;
    // Not found: the run (an answer of the plane's own, with no words), or an artifact of it (said in words).
    if res.status == 404 {
        let t = res.body_text().trim().to_string();
        return Err(if t.is_empty() || t.starts_with(['{', '<']) || t.contains("nothing here") { "No such run among this control plane's runs from a computer (their ids are in `superci runs list`).".into() } else { format!("{}{}.", &t[..1].to_uppercase(), &t[1..]) })
    }
    if res.status >= 300 { return Err(format!("the control plane said {}: {}", res.status, res.body_text().trim())) }
    serde_json::from_slice(&res.body).map_err(|e| format!("the control plane's answer: {e}"))
}

fn run_ok(run: &str) -> Result<()> {
    if run.is_empty() || !run.bytes().all(|b| b.is_ascii_digit()) { return Err("Say which run: --run=ID (ids are in `superci runs list`).".into()) }
    Ok(())
}

/// `superci artifacts list --run=ID`: what a run from a computer's jobs kept with `upload-artifact`.
pub fn artifacts(plane: &str, key: &str, run: &str) -> Result<Done> {
    run_ok(run)?;
    let got = get(plane, key, &format!("/runs/{run}/artifacts"))?;
    let all = got["artifacts"].as_array().cloned().unwrap_or_default();
    let said = if all.is_empty() { vec!["No artifacts: its jobs kept none, or the run is over a day old.".into()] }
        else { all.iter().map(|a| format!("{}  {}", a["name"].as_str().unwrap_or_default(), crate::dashboard::size_words(a["size"].as_u64().unwrap_or(0)))).collect() };
    Ok(Done { said, data: json!({ "run_id": run, "artifacts": all }) })
}

/// `superci artifacts retrieve NAME --run=ID [--out=DIR]`: fetches one and unpacks it into DIR (`./NAME` unless
/// given), as `download-artifact` would. One that is not a zip is kept as the file it is.
pub fn artifact(plane: &str, key: &str, run: &str, name: &str, out: Option<&str>) -> Result<Done> {
    run_ok(run)?;
    if name.is_empty() { return Err("Say which artifact: `superci artifacts retrieve NAME --run=ID` (names are in `superci artifacts list --run=ID`).".into()) }
    let at = format!("/runs/{run}/artifacts?name={}", url::form_urlencoded::byte_serialize(name.as_bytes()).collect::<String>());
    let got = get(plane, key, &at)?;
    let dir = std::path::PathBuf::from(out.unwrap_or(name));
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    // Its pieces, in order, into one file beside where it goes.
    let zip = dir.join(format!(".{}.superci-download", name.replace(['/', '\\'], "_")));
    let mut file = std::fs::File::create(&zip).map_err(|e| format!("{}: {e}", zip.display()))?;
    let http = crate::aws::Blocking::new();
    for p in got["pieces"].as_array().into_iter().flatten() {
        let res = block_on(http.send(Request::new("GET", p["url"].as_str().unwrap_or_default())))?;
        if res.status >= 300 { let _ = std::fs::remove_file(&zip); return Err(format!("the bucket said {} for a piece of {name}", res.status)) }
        file.write_all(&res.body).map_err(|e| e.to_string())?;
    }
    drop(file);
    let files = match unzip(&zip, &dir) {
        Ok(n) => { let _ = std::fs::remove_file(&zip); n }
        Err(Unpack::NotZip) => { let kept = dir.join(name); std::fs::rename(&zip, &kept).map_err(|e| e.to_string())?; 1 }
        Err(Unpack::Bad(e)) => { let _ = std::fs::remove_file(&zip); return Err(format!("{name} could not be unpacked: {e}")) }
    };
    let size = got["size"].as_u64().unwrap_or(0);
    Ok(Done { said: vec![format!("{name} ({}, {files} file{}) is in {}", crate::dashboard::size_words(size), if files == 1 { "" } else { "s" }, dir.display())],
        data: json!({ "run_id": run, "name": name, "size": size, "files": files, "out": dir.to_string_lossy() }) })
}

enum Unpack { NotZip, Bad(String) }

/// Unpacks a zip (as `upload-artifact` makes them: stored or deflated, its names relative) into `dir`; how many files.
fn unzip(path: &Path, dir: &Path) -> std::result::Result<usize, Unpack> {
    use std::io::{Read, Seek, SeekFrom};
    let bad = |e: &str| Unpack::Bad(e.to_string());
    let mut f = std::fs::File::open(path).map_err(|e| Unpack::Bad(e.to_string()))?;
    let len = f.metadata().map_err(|e| Unpack::Bad(e.to_string()))?.len();
    let mut head = [0u8; 4];
    if f.read_exact(&mut head).is_err() || head != [0x50, 0x4b, 0x03, 0x04] { return Err(Unpack::NotZip) }
    // Its end record: in the last 64 KB (and 22 bytes), the last that says so.
    let tail_len = len.min(65_557);
    let mut tail = vec![0u8; tail_len as usize];
    f.seek(SeekFrom::Start(len - tail_len)).and_then(|_| f.read_exact(&mut tail)).map_err(|e| Unpack::Bad(e.to_string()))?;
    let end = (0..tail.len().saturating_sub(21)).rev().find(|&i| tail[i..i + 4] == [0x50, 0x4b, 0x05, 0x06]).ok_or(bad("no end record"))?;
    let u16at = |b: &[u8], i: usize| u16::from_le_bytes([b[i], b[i + 1]]) as u64;
    let u32at = |b: &[u8], i: usize| u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]) as u64;
    let (count, cd_size, cd_at) = (u16at(&tail, end + 10), u32at(&tail, end + 12), u32at(&tail, end + 16));
    if count == 0xffff || cd_at == 0xffff_ffff { return Err(bad("it is over 4 GB (zip64), which this does not unpack yet")) }
    let mut cd = vec![0u8; cd_size as usize];
    f.seek(SeekFrom::Start(cd_at)).and_then(|_| f.read_exact(&mut cd)).map_err(|e| Unpack::Bad(e.to_string()))?;
    let (mut at, mut files) = (0usize, 0usize);
    for _ in 0..count {
        if cd.len() < at + 46 || cd[at..at + 4] != [0x50, 0x4b, 0x01, 0x02] { return Err(bad("a damaged directory")) }
        let (method, packed, size) = (u16at(&cd, at + 10), u32at(&cd, at + 20), u32at(&cd, at + 24));
        let (n, e, c, local) = (u16at(&cd, at + 28) as usize, u16at(&cd, at + 30) as usize, u16at(&cd, at + 32) as usize, u32at(&cd, at + 42));
        let name = String::from_utf8_lossy(&cd[at + 46..at + 46 + n]).replace('\\', "/");
        at += 46 + n + e + c;
        // Only names inside the folder.
        if name.starts_with('/') || name.split('/').any(|p| p == "..") || name.contains(':') { return Err(Unpack::Bad(format!("a name outside its folder: {name}"))) }
        let to = dir.join(&name);
        if name.ends_with('/') { std::fs::create_dir_all(&to).map_err(|e| Unpack::Bad(e.to_string()))?; continue }
        if packed == 0xffff_ffff || size == 0xffff_ffff { return Err(bad("a file over 4 GB (zip64), which this does not unpack yet")) }
        let mut lh = [0u8; 30];
        f.seek(SeekFrom::Start(local)).and_then(|_| f.read_exact(&mut lh)).map_err(|e| Unpack::Bad(e.to_string()))?;
        f.seek(SeekFrom::Current((u16at(&lh, 26) + u16at(&lh, 28)) as i64)).map_err(|e| Unpack::Bad(e.to_string()))?;
        let mut data = vec![0u8; packed as usize];
        f.read_exact(&mut data).map_err(|e| Unpack::Bad(e.to_string()))?;
        let data = match method {
            0 => data,
            8 => miniz_oxide::inflate::decompress_to_vec(&data).map_err(|e| Unpack::Bad(format!("{name}: {e:?}")))?,
            m => return Err(Unpack::Bad(format!("{name} is packed in a way this does not know ({m})"))),
        };
        if let Some(parent) = to.parent() { std::fs::create_dir_all(parent).map_err(|e| Unpack::Bad(e.to_string()))? }
        std::fs::write(&to, data).map_err(|e| Unpack::Bad(format!("{}: {e}", to.display())))?;
        files += 1;
    }
    Ok(files)
}

/// A job's lines as they are printed: each step in turn, its lines in order, none twice and none missing.
#[derive(Default)]
struct Lines {
    /// What came and is not printed yet: each post's step, where it begins in that step's log, and its lines.
    held: Vec<(Option<String>, Option<u64>, Vec<String>)>,
    /// The last line printed of each step, and the steps whose lines are all out.
    shown: std::collections::HashMap<String, u64>,
    /// Lines of a step not had yet when later ones came (first and last), printed once its whole log comes.
    missing: std::collections::HashMap<String, Vec<(u64, u64)>>,
    done: std::collections::HashSet<String>,
    /// Steps known (from the run's status): no steps, no order to keep (an older control plane).
    steps: usize,
}

impl Lines {
    /// What to print now from the job's latest status (`j`: its new posts and its steps). `give_up`: the run is over
    /// and a step's log has not come: what is held is printed as it is. `fetch`: a step's whole log by its number.
    fn take(&mut self, j: &Value, give_up: bool, fetch: &dyn Fn(u64) -> Option<String>, pages: &dyn Fn(&str) -> Option<String>) -> Vec<String> {
        for p in j["posts"].as_array().into_iter().flatten() {
            let lines = p["lines"].as_array().into_iter().flatten().filter_map(|l| l.as_str().map(str::to_string)).collect();
            self.held.push((p["step"].as_str().map(str::to_string), p["start"].as_u64(), lines));
        }
        // An older control plane: its lines as they come.
        if j["posts"].is_null() { return j["lines"].as_array().into_iter().flatten().filter_map(|l| l.as_str().map(str::to_string)).collect() }
        let mut steps: Vec<&Value> = j["steps"].as_array().into_iter().flatten().collect();
        steps.sort_by_key(|s| s["order"].as_u64().unwrap_or(0));
        self.steps = steps.len();
        let mut out = vec![];
        if steps.is_empty() { for (_, _, lines) in self.held.drain(..) { out.extend(lines) } return out }
        // (A step is over once one after it has begun, whatever its own record says.)
        let begun: Vec<bool> = steps.iter().map(|s| s["state"].as_str().is_some_and(|st| st != "pending")).collect();
        for (at, s) in steps.iter().enumerate() {
            let Some(id) = s["id"].as_str() else { continue };
            let s = *s;
            if self.done.contains(id) { continue }
            let shown = self.shown.entry(id.to_string()).or_insert(0);
            // What was sent of it as it went, while it follows on from what is out; past a gap (the runner drops lines
            // that come faster than it sends them), what its machine sent of the step's pages fills it, and on.
            let mut rest = vec![];
            let mut filled = false;
            for (step, start, lines) in self.held.drain(..) {
                if step.as_deref() != Some(id) { rest.push((step, start, lines)); continue }
                if let (Some(st), false) = (start, filled) {
                    if st > *shown + 1 {
                        filled = true;
                        if let Some(text) = pages(id) { for l in text.lines().skip(*shown as usize) { out.push(without_time(l).to_string()); *shown += 1 } }
                    }
                }
                match start {
                    Some(start) if start <= *shown + 1 => for (k, line) in lines.into_iter().enumerate() { if start + k as u64 > *shown { out.push(line); *shown = start + k as u64 } },
                    // Still a gap (the runner keeps the last of a page until it is full or the step ends): said, and
                    // on with what comes; what is missing follows once the step's whole log is here.
                    Some(start) => {
                        self.missing.entry(id.to_string()).or_default().push((*shown + 1, start - 1));
                        out.push(format!("··· {} of this step {} when it ends", which(*shown + 1, start - 1), if *shown + 1 == start - 1 { "follows" } else { "follow" }));
                        *shown = start + lines.len() as u64 - 1;
                        out.extend(lines);
                    }
                    None => { *shown += lines.len() as u64; out.extend(lines) }
                }
            }
            self.held = rest;
            // Over: the rest from its whole log, then the next step. (The run over and its log long in coming: what
            // came of it as it went, as it is.)
            let ended = s["state"] == "completed" || begun[at + 1..].iter().any(|b| *b) || give_up;
            if !ended { return out }
            match s["log"].as_u64().and_then(fetch) {
                Some(text) => {
                    let all: Vec<&str> = text.lines().collect();
                    for (a, b) in self.missing.remove(id).unwrap_or_default() {
                        out.push(format!("··· {} of this step, which came late:", which(a, b)));
                        out.extend(all.iter().skip(a as usize - 1).take((b + 1 - a) as usize).map(|l| without_time(l).to_string()));
                    }
                    out.extend(all.iter().skip(*shown as usize).map(|l| without_time(l).to_string()));
                }
                None if give_up => {
                    let (mine, rest): (Vec<_>, Vec<_>) = std::mem::take(&mut self.held).into_iter().partition(|(step, _, _)| step.as_deref() == Some(id));
                    self.held = rest;
                    for (_, start, lines) in mine { for (k, line) in lines.into_iter().enumerate() { if start.is_none_or(|st| st + k as u64 > *shown) { out.push(line) } } }
                }
                None => return out,
            }
            self.done.insert(id.to_string());
        }
        out
    }

    /// Whether every step's lines are out.
    fn complete(&self) -> bool { self.steps == 0 || self.done.len() >= self.steps }
}

/// A time in seconds as a person reads it: "42 s", "3m 5s".
/// A job's start in a line, from its record (see `dashboard::job_start`): " (its machine in 19.4 s: AWS gave a
/// machine 6.1 s, …)"; nothing when the control plane did not say.
fn start_line(record: &Value) -> String {
    if record.is_null() { return String::new() }
    let start = crate::dashboard::job_start(record);
    let steps: Vec<String> = start["steps"].as_array().into_iter().flatten().map(|s| format!("{} {:.1} s", s["what"].as_str().unwrap_or_default(), s["ms"].as_u64().unwrap_or(0) as f64 / 1000.0)).collect();
    match start["ms"].as_u64() { Some(ms) if !steps.is_empty() => format!(" (started in {:.1} s: {})", ms as f64 / 1000.0, steps.join(", ")), _ => String::new() }
}

fn span(s: u64) -> String { if s < 60 { format!("{s} s") } else { format!("{}m {}s", s / 60, s % 60) } }

/// Lines of a step by their numbers: "line 4", "lines 7–8".
fn which(a: u64, b: u64) -> String { if a == b { format!("line {a}") } else { format!("lines {a}–{b}") } }

/// A log's line without the time the runner put before it ("2026-10-14T21:09:53.1234567Z ").
fn without_time(line: &str) -> &str {
    // (A log's first line begins with a byte order mark.)
    let line = line.trim_start_matches('\u{feff}');
    let b = line.as_bytes();
    if b.len() > 20 && b[4] == b'-' && b[10] == b'T' && b[..4].iter().all(u8::is_ascii_digit) {
        if let Some(at) = line.find("Z ") { if at < 40 { return &line[at + 2..] } }
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_run_is_said_by_its_number() {
        assert!(run_ok("123").is_ok());
        for bad in ["", "12a", "-1"] { assert!(run_ok(bad).unwrap_err().starts_with("Say which run")) }
    }

    #[test]
    fn a_zip_as_upload_artifact_makes_it_is_unpacked_inside_its_folder() {
        let dir = std::env::temp_dir().join(format!("superci-unzip-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src/run")).unwrap();
        std::fs::write(dir.join("src/run/gaps.json"), "{\"gaps\":[1,2,3]}".repeat(50)).unwrap();
        std::fs::write(dir.join("src/lcov.info"), "TN:\nSF:a.rs\nend_of_record\n").unwrap();
        // The system's zip, deflated (as upload-artifact makes them).
        let ok = Command::new("zip").current_dir(dir.join("src")).args(["-q", "-r", "../a.zip", "."]).status();
        if !ok.is_ok_and(|s| s.success()) { eprintln!("no zip here"); return }
        assert_eq!(unzip(&dir.join("a.zip"), &dir.join("out")).ok(), Some(2));
        assert_eq!(std::fs::read_to_string(dir.join("out/run/gaps.json")).unwrap(), "{\"gaps\":[1,2,3]}".repeat(50));
        assert_eq!(std::fs::read_to_string(dir.join("out/lcov.info")).unwrap(), "TN:\nSF:a.rs\nend_of_record\n");
        std::fs::write(dir.join("plain.txt"), "not a zip").unwrap();
        assert!(matches!(unzip(&dir.join("plain.txt"), &dir.join("out2")), Err(Unpack::NotZip)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn each_steps_lines_are_printed_whole_and_in_order_what_the_runner_dropped_from_its_log() {
        let mut l = Lines::default();
        let step = |id: &str, order: u64, state: &str, log: Option<u64>| json!({ "id": id, "order": order, "state": state, "log": log });
        let post = |s: &str, start: u64, lines: &[&str]| json!({ "step": s, "start": start, "lines": lines });
        let logs = |n: u64| Some(match n { 1 => "2026-10-14T21:09:53.1234567Z a1\n2026-10-14T21:09:53.1234567Z a2\n2026-10-14T21:09:53.1234567Z a3\n2026-10-14T21:09:54.0000000Z a4".to_string(), _ => "b1\nb2".to_string() });
        let none = |_: u64| None;
        // Step a sends line 1, then 4 (2 and 3 were dropped); b's first line comes before a's log does.
        let j = json!({ "posts": [post("a", 1, &["a1"]), post("a", 4, &["a4"]), post("b", 1, &["b1"])], "steps": [step("a", 1, "inProgress", None), step("b", 2, "pending", None)] });
        assert_eq!(l.take(&j, false, &none, &|_| None), ["a1", "··· lines 2–3 of this step follow when it ends", "a4"], "past a gap with no pages, said and on; b waits for a");
        let j = json!({ "posts": [], "steps": [step("a", 1, "completed", Some(1)), step("b", 2, "inProgress", None)] });
        assert_eq!(l.take(&j, false, &logs, &|_| None), ["··· lines 2–3 of this step, which came late:", "a2", "a3", "b1"], "what a missed, from its log, its time taken off; then b");
        let j = json!({ "posts": [post("b", 2, &["b2"])], "steps": [step("a", 1, "completed", Some(1)), step("b", 2, "completed", Some(2))] });
        assert_eq!(l.take(&j, false, &logs, &|_| None), ["b2"]);
        assert!(l.complete());
        // An older control plane: its lines as they come.
        assert_eq!(Lines::default().take(&json!({ "lines": ["x", "y"] }), false, &none, &|_| None), ["x", "y"]);
        // Past a gap, its pages so far fill it, and the step's lines go on as they come.
        let mut l = Lines::default();
        let pages = |s: &str| (s == "a").then(|| "2026-10-15T11:00:49.1Z a1\n2026-10-15T11:00:49.2Z a2\n2026-10-15T11:00:49.3Z a3\n".to_string());
        let j = json!({ "posts": [post("a", 1, &["a1"]), post("a", 5, &["a5"])], "steps": [step("a", 1, "inProgress", None)] });
        assert_eq!(l.take(&j, false, &none, &pages), ["a1", "a2", "a3", "··· line 4 of this step follows when it ends", "a5"], "a gap filled from the pages as far as they go, then said");
        let j = json!({ "posts": [post("a", 4, &["a4", "a5", "a6"])], "steps": [step("a", 1, "inProgress", None)] });
        assert_eq!(l.take(&j, false, &none, &pages), ["a6"], "and on, none twice");
        // A gap the pages do not reach yet: said, and on; its lines once the step's whole log is here.
        let j = json!({ "posts": [post("a", 9, &["a9"])], "steps": [step("a", 1, "inProgress", None)] });
        assert_eq!(l.take(&j, false, &none, &pages), ["··· lines 7–8 of this step follow when it ends", "a9"]);
        let whole = |_: u64| Some((1..=10).map(|n| format!("2026-10-15T11:00:49.1Z a{n}")).collect::<Vec<_>>().join("\n"));
        let j = json!({ "posts": [], "steps": [step("a", 1, "completed", Some(1))] });
        assert_eq!(l.take(&j, false, &whole, &pages), ["··· line 4 of this step, which came late:", "a4", "··· lines 7–8 of this step, which came late:", "a7", "a8", "a10"]);
    }

    #[test]
    fn a_control_plane_before_runs_from_a_computer_is_said_to_need_updating() {
        assert!(new_enough(Some("0.16.9")).unwrap_err().contains("superci planes update"));
        assert!(new_enough(Some("0.18.0")).is_ok() && new_enough(Some("0.19.2")).is_ok() && new_enough(None).is_ok());
    }
}

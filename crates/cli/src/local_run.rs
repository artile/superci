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
/// The tree, packed, is at most this much.
const MOST: usize = 150_000_000;

fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git").current_dir(dir).args(args).output().map_err(|e| format!("git: {e}"))?;
    if !out.status.success() { return Err(format!("git {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim())) }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// The working tree as a gzipped tar: what git tracks and the new files it does not ignore (an ignored `.env`
/// stays here), as they are on disk now. By the system's own tar.
fn pack(root: &Path) -> Result<Vec<u8>> {
    let listed = Command::new("git").current_dir(root).args(["ls-files", "-z", "--cached", "--others", "--exclude-standard"]).output().map_err(|e| format!("git: {e}"))?;
    let files: Vec<&[u8]> = listed.stdout.split(|b| *b == 0).filter(|f| !f.is_empty()).filter(|f| root.join(String::from_utf8_lossy(f).as_ref()).symlink_metadata().is_ok_and(|m| !m.is_dir())).collect();
    let mut tar = Command::new("tar").current_dir(root).args(["-czf", "-", "--no-xattrs", "--null", "-T", "-"]).env("COPYFILE_DISABLE", "1")
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().map_err(|e| format!("tar: {e}"))?;
    let list: Vec<u8> = files.iter().flat_map(|f| f.iter().copied().chain([0])).collect();
    let mut stdin = tar.stdin.take().ok_or("tar took no input")?;
    let writer = std::thread::spawn(move || stdin.write_all(&list));
    let out = tar.wait_with_output().map_err(|e| format!("tar: {e}"))?;
    let _ = writer.join();
    if !out.status.success() { return Err(format!("tar: {}", String::from_utf8_lossy(&out.stderr).trim())) }
    Ok(out.stdout)
}

/// `owner/name` of the repository this checkout's `origin` is on GitHub (else the folder's name under `local/`).
fn repository(root: &Path) -> String {
    let url = git(root, &["remote", "get-url", "origin"]).unwrap_or_default();
    let path = url.trim_end_matches(".git").rsplit_once("github.com").map(|(_, p)| p.trim_start_matches([':', '/']).to_string()).unwrap_or_default();
    if path.split('/').count() == 2 { path } else { format!("local/{}", root.file_name().and_then(|n| n.to_str()).unwrap_or("repository")) }
}

pub fn create(plane: &str, key: &str, workflow: &str, job: Option<&str>, event: &str, secrets: &[(String, String)], vars: &[(String, String)], say: &mut dyn FnMut(&str)) -> Result<Done> {
    let here = std::env::current_dir().map_err(|e| e.to_string())?;
    let root = std::path::PathBuf::from(git(&here, &["rev-parse", "--show-toplevel"]).map_err(|_| "Run it in a git checkout: the run takes its working tree.".to_string())?);
    let file = here.join(workflow);
    let text = std::fs::read_to_string(&file).map_err(|e| format!("{workflow}: {e}"))?;
    let path = file.canonicalize().ok().and_then(|f| f.strip_prefix(root.canonicalize().ok()?).ok().map(|p| p.to_string_lossy().replace('\\', "/"))).unwrap_or_else(|| workflow.to_string());
    let http = crate::aws::Blocking::new();
    let call = |method: &str, at: &str, body: Vec<u8>| -> Result<Value> {
        let r = Request::new(method, &format!("{plane}{at}")).with_header("authorization", &format!("Bearer {key}")).with_body(body);
        let res = block_on(http.send(r))?;
        if res.status >= 300 { return Err(format!("the control plane said {}: {}", res.status, res.body_text().trim())) }
        serde_json::from_slice(&res.body).map_err(|e| format!("the control plane's answer: {e}"))
    };
    let packed = pack(&root)?;
    if packed.len() > MOST { return Err(format!("The working tree is {} MB packed; a run from a computer takes {} MB at most.", packed.len() / 1_000_000, MOST / 1_000_000)) }
    let tree = format!("tree-{}", superci_core::crypto::random_token(18));
    let parts = packed.chunks(PART).count() as u32;
    say(&format!("Sending the working tree ({} KB packed, {parts} part{})", packed.len().div_ceil(1000), if parts == 1 { "" } else { "s" }));
    for (i, part) in packed.chunks(PART).enumerate() { call("POST", &format!("/runs/tree?id={tree}&part={i}"), part.to_vec())?; }
    let reference = git(&root, &["symbolic-ref", "-q", "HEAD"]).unwrap_or_else(|_| "refs/heads/main".into());
    let body = json!({ "workflow": text, "path": path, "job": job, "repository": repository(&root), "ref": reference, "sha": git(&root, &["rev-parse", "HEAD"]).unwrap_or_default(),
        "actor": git(&root, &["config", "user.name"]).unwrap_or_else(|_| "superci".into()), "event_name": event, "secrets": secrets, "vars": vars, "tree": tree, "parts": parts });
    let made = call("POST", "/runs", body.to_string().into_bytes())?;
    let run = made["run_id"].as_str().unwrap_or_default().to_string();
    say(&format!("Run {run}: {} of {path}, on a machine of this control plane's", made["job"].as_str().unwrap_or_default()));
    let (began, mut after, mut last_state) = (std::time::Instant::now(), 0u64, String::new());
    loop {
        std::thread::sleep(std::time::Duration::from_millis(1500));
        let st = call("GET", &format!("/runs/{run}?after={after}"), vec![])?;
        let state = st["state"].as_str().unwrap_or_default().to_string();
        if state != last_state && !state.is_empty() {
            say(&match state.as_str() { "launching" => "Asking for its machine".to_string(), "launched" => format!("Its machine is starting ({})", st["machine"].as_str().unwrap_or("")), "running" => "Running".to_string(), other => other.to_string() });
            last_state = state.clone();
        }
        for line in st["lines"].as_array().into_iter().flatten() { say(line.as_str().unwrap_or_default()) }
        after = st["next"].as_u64().unwrap_or(after);
        if let Some(result) = st["result"].as_str() {
            let took = began.elapsed().as_secs();
            let said = format!("The job ended: {result}, {took} s after it was sent.");
            if result != "succeeded" { return Err(said) }
            return Ok(Done { said: vec![said], data: json!({ "run_id": run, "result": result, "seconds": took }) })
        }
        if ["failed", "cancelled", "swept"].contains(&state.as_str()) {
            return Err(format!("The job ended before its runner said how: {}", st["error"].as_str().unwrap_or(&state)))
        }
        if began.elapsed().as_secs() > 6 * 3600 { return Err(format!("Still not over after six hours: `superci runs retrieve {run}`.")) }
    }
}

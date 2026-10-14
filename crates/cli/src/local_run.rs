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
    new_enough(crate::cloudflare::health(plane).as_ref().and_then(|h| h["version"].as_str()))?;
    let http = crate::aws::Blocking::new();
    let call = |method: &str, at: &str, body: Vec<u8>| -> Result<Value> {
        let r = Request::new(method, &format!("{plane}{at}")).with_header("authorization", &format!("Bearer {key}")).with_body(body);
        let res = block_on(http.send(r))?;
        if res.status >= 300 { return Err(format!("the control plane said {}: {}", res.status, res.body_text().trim())) }
        serde_json::from_slice(&res.body).map_err(|e| format!("the control plane's answer: {e}"))
    };
    let packed = pack(&root)?;
    let tree = format!("tree-{}", superci_core::crypto::random_token(18));
    // Into the control plane's bucket, in one, when it has one (as a build service takes its source); else through
    // the control plane, in parts (it takes a request of a few MB).
    let link = call("POST", &format!("/runs/tree?id={tree}&part=0&link=1"), vec![])?["url"].as_str().map(str::to_string);
    let most = if link.is_some() { MOST_IN_BUCKET } else { MOST };
    if packed.len() > most { return Err(format!("The working tree is {} MB packed; a run from a computer takes {} MB at most.", packed.len() / 1_000_000, most / 1_000_000)) }
    let parts = match &link {
        Some(link) => {
            say(&format!("Sending the working tree ({} KB packed)", packed.len().div_ceil(1000)));
            let res = block_on(http.send(Request::new("PUT", link).with_body(packed.clone())))?;
            if res.status >= 300 { return Err(format!("the bucket said {} to the working tree: {}", res.status, res.body_text().trim())) }
            1
        }
        None => {
            let parts = packed.chunks(PART).count() as u32;
            say(&format!("Sending the working tree ({} KB packed, {parts} part{})", packed.len().div_ceil(1000), if parts == 1 { "" } else { "s" }));
            for (i, part) in packed.chunks(PART).enumerate() { call("POST", &format!("/runs/tree?id={tree}&part={i}"), part.to_vec())?; }
            parts
        }
    };
    let reference = git(&root, &["symbolic-ref", "-q", "HEAD"]).unwrap_or_else(|_| "refs/heads/main".into());
    let body = json!({ "workflow": text, "path": path, "job": job, "repository": repository(&root), "ref": reference, "sha": git(&root, &["rev-parse", "HEAD"]).unwrap_or_default(),
        "actor": git(&root, &["config", "user.name"]).unwrap_or_else(|_| "superci".into()), "event_name": event, "secrets": secrets, "vars": vars, "tree": tree, "parts": parts });
    let made = call("POST", "/runs", body.to_string().into_bytes())?;
    let run = made["run_id"].as_str().unwrap_or_default().to_string();
    say(&format!("Run {run} of {path}, on machines of this control plane's"));
    let (began, mut after, mut last): (std::time::Instant, Vec<u64>, Vec<String>) = (std::time::Instant::now(), vec![], vec![]);
    loop {
        std::thread::sleep(std::time::Duration::from_millis(1500));
        let cursor = after.iter().map(|n| n.to_string()).collect::<Vec<_>>().join(",");
        let st = call("GET", &format!("/runs/{run}?after={cursor}"), vec![])?;
        let jobs = st["jobs"].as_array().cloned().unwrap_or_default();
        after.resize(jobs.len(), 0);
        last.resize(jobs.len(), String::new());
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
                (_, Some(r)) => format!("ended: {r}"),
                (other, _) => other.to_string(),
            };
            if !state.is_empty() && state != last[i] { say(&format!("{mark}{}{}", &state[..1].to_uppercase(), &state[1..])); last[i] = state }
            // Its lines as GitHub shows them: a group's heading is a line, its end is nothing.
            for line in j["lines"].as_array().into_iter().flatten().filter_map(|l| l.as_str()) {
                if line.starts_with("##[endgroup]") { continue }
                say(&format!("{}{mark}{}", crate::commands::JOB_LINE, line.strip_prefix("##[group]").unwrap_or(line)))
            }
            after[i] = j["next"].as_u64().unwrap_or(after[i]);
        }
        if st["over"] == true {
            let (result, took) = (st["result"].as_str().unwrap_or_default().to_string(), began.elapsed().as_secs());
            let tally: Vec<String> = jobs.iter().map(|j| format!("{} {}", j["name"].as_str().unwrap_or_default(), j["result"].as_str().unwrap_or("?"))).collect();
            let said = format!("The run ended: {result}, {took} s after it was sent{}.", if several { format!(" ({})", tally.join(", ")) } else { String::new() });
            if result != "success" { return Err(said) }
            return Ok(Done { said: vec![said], data: json!({ "run_id": run, "result": result, "seconds": took, "jobs": jobs.iter().map(|j| json!({ "name": j["name"], "result": j["result"] })).collect::<Vec<_>>() }) })
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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn a_control_plane_before_runs_from_a_computer_is_said_to_need_updating() {
        assert!(new_enough(Some("0.16.9")).unwrap_err().contains("superci planes update"));
        assert!(new_enough(Some("0.18.0")).is_ok() && new_enough(Some("0.19.2")).is_ok() && new_enough(None).is_ok());
    }
}

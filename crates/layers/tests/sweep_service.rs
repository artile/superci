//! superci-layers with each of its steps failing in turn (`SUPERCI_LAYERS_FAULT`, see the program's `faults`), as
//! containerd and a machine's end use it: its content store, its snapshots, a layer from SuperCI's store, and `keep`.
//! Whatever fails, it does not panic, answers every request (an error said as one), and does not hang. (The clean
//! run alone unless `SUPERCI_SWEEP_FAULTS=1`, each `SUPERCI_SWEEP_SLICE` its share, as the other sweeps.)
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};
use superci_layers::containerd::services::content::v1 as capi;
use superci_layers::containerd::services::snapshots::v1 as sapi;

fn digest(b: &[u8]) -> String { format!("sha256:{}", Sha256::digest(b).iter().map(|x| format!("{x:02x}")).collect::<String>()) }

/// SuperCI's store and control plane, as the service and `keep` ask them: a layer's file by ranges, its links, an
/// offer answered with an upload, the upload, its word, what was kept.
fn plane(files: HashMap<String, Vec<u8>>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let at = format!("http://{}", listener.local_addr().unwrap());
    let base = at.clone();
    std::thread::spawn(move || for stream in listener.incoming().flatten() {
        let (files, base) = (files.clone(), base.clone());
        std::thread::spawn(move || {
            let mut r = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            if r.read_line(&mut line).is_err() { return }
            let mut parts = line.split_whitespace();
            let (method, path) = (parts.next().unwrap_or_default().to_string(), parts.next().unwrap_or_default().to_string());
            let mut headers = HashMap::new();
            loop { let mut h = String::new(); if r.read_line(&mut h).is_err() || h.trim().is_empty() { break } if let Some((k, v)) = h.split_once(':') { headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string()); } }
            let mut body = vec![0; headers.get("content-length").and_then(|l| l.parse().ok()).unwrap_or(0)];
            let _ = r.read_exact(&mut body);
            let mut out = stream;
            let mut send = |status: u16, extra: &str, b: &[u8]| { let _ = out.write_all(format!("HTTP/1.1 {status} X\r\ncontent-length: {}\r\n{extra}connection: close\r\n\r\n", b.len()).as_bytes()); let _ = out.write_all(b); };
            let json: serde_json::Value = serde_json::from_slice(&body).unwrap_or_default();
            match (method.as_str(), path.as_str()) {
                ("GET", f) if f.starts_with("/files/") => match (files.get(&f[7..]), headers.get("range").and_then(|r| r.strip_prefix("bytes=")).and_then(|r| r.split_once('-'))) {
                    (Some(b), Some((a, z))) => { let (a, z): (usize, usize) = (a.parse().unwrap(), z.parse::<usize>().unwrap().min(b.len() - 1)); send(206, &format!("content-range: bytes {a}-{z}/{}\r\n", b.len()), &b[a..=z]) }
                    (Some(b), None) => send(200, "", b),
                    (None, _) => send(404, "", b""),
                },
                ("POST", "/links") => {
                    let layers: serde_json::Map<String, serde_json::Value> = json["digests"].as_array().into_iter().flatten().filter_map(|d| d.as_str()).map(|d| {
                        let size = files.get(&format!("{d}.erofs")).map(|f| f.len()).unwrap_or(0);
                        (d.to_string(), serde_json::json!({ "size": 1, "erofs": { "url": format!("{base}/files/{d}.erofs"), "size": size }, "blob": { "url": format!("{base}/files/{d}.blob") } }))
                    }).collect();
                    send(200, "", serde_json::json!({ "layers": layers }).to_string().as_bytes())
                }
                ("POST", "/layers-propose") => {
                    let answers: serde_json::Map<String, serde_json::Value> = json["layers"].as_array().into_iter().flatten().filter_map(|l| l["digest"].as_str()).map(|d| (d.to_string(), serde_json::json!({ "do": "upload", "url": format!("{base}/up/{d}"), "id": d }))).collect();
                    send(200, "", serde_json::json!({ "layers": answers, "uploaded": format!("{base}/uploaded") }).to_string().as_bytes())
                }
                ("PUT", _) => send(200, "", b""),
                ("POST", "/uploaded") => send(200, "", br#"{"do": "converting"}"#),
                _ => send(200, "", b"{}"),
            }
        });
    });
    at
}

struct Run { child: Child, dir: std::path::PathBuf }

impl Run {
    fn log(&self) -> String { std::fs::read_to_string(self.dir.join("log")).unwrap_or_default() }
    /// Stopped as systemd stops it; what it said, and whether it ended by a panic.
    fn stop(mut self) -> (String, bool) {
        unsafe { libc::kill(self.child.id() as i32, libc::SIGTERM); }
        let until = Instant::now() + Duration::from_secs(5);
        let mut status = None;
        while Instant::now() < until { if let Ok(Some(s)) = self.child.try_wait() { status = Some(s); break } std::thread::sleep(Duration::from_millis(20)) }
        if status.is_none() { let _ = self.child.kill(); let _ = self.child.wait(); }
        let log = self.log();
        let _ = std::fs::remove_dir_all(&self.dir);
        (log.clone(), log.contains("panicked") || status.is_some_and(|s| s.code() == Some(101)))
    }
}

/// The service started with `fault` (none: the clean run, counting its steps into `count`), an index of one stored
/// layer at `at`.
fn start(fault: Option<usize>, count: &std::path::Path, at: &str, stored: &str) -> Option<(Run, std::path::PathBuf)> {
    static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!("superci-layers-sweep-{}-{}", std::process::id(), N.fetch_add(1, std::sync::atomic::Ordering::SeqCst)));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let socket = dir.join("layers.sock");
    std::fs::write(dir.join("index.json"), serde_json::json!({ "layers": { stored: { "size": 21, "erofs": { "size": 300_000 }, "blob": {} } }, "refresh": { "url": format!("{at}/links"), "token": "" } }).to_string()).unwrap();
    let mut c = Command::new(env!("CARGO_BIN_EXE_superci-layers"));
    c.args([format!("--root={}", dir.join("root").display()), format!("--content={}", dir.join("content").display()), format!("--socket={}", socket.display()),
        format!("--index={}", dir.join("index.json").display()), format!("--containerd={}", dir.join("no-containerd.sock").display()), "--parallel=2".into(), "--sync-remove".into()])
        .env("SUPERCI_LAYERS_FAULTS", count).stderr(std::fs::File::create(dir.join("log")).unwrap());
    if let Some(k) = fault { c.env("SUPERCI_LAYERS_FAULT", k.to_string()); }
    let mut child = c.spawn().unwrap();
    let until = Instant::now() + Duration::from_secs(20);
    while !socket.exists() {
        // (A fault as it starts may end it, saying why.)
        if let Ok(Some(_)) = child.try_wait() { let log = std::fs::read_to_string(dir.join("log")).unwrap_or_default(); assert!(!log.contains("panicked"), "{fault:?}: {log}"); return None }
        assert!(Instant::now() < until, "{fault:?}: it did not start");
        std::thread::sleep(Duration::from_millis(10));
    }
    Some((Run { child, dir }, socket))
}

/// One step of a scenario, its answer whatever it is, within a time: a hang is a failure.
async fn step<T>(what: &str, f: impl std::future::Future<Output = T>) -> Result<T, String> { tokio::time::timeout(Duration::from_secs(20), f).await.map_err(|_| format!("{what}: no answer within 20 s")) }

/// The scenario: what containerd asks of the content store and the snapshots, a stored layer used, `keep` after.
async fn scenario(socket: &std::path::Path, dir: &std::path::Path, at: &str, stored: &str, fault: Option<usize>) -> Result<(), String> {
    // (`fault`: keep's, counted from its start, into `dir/keep-count`.)
    use capi::WriteAction::*;
    let Ok(channel) = step("connect", superci_layers::client::channel(socket)).await? else { return Ok(()) };
    let mut c = capi::content_client::ContentClient::new(channel.clone());
    let mut s = sapi::snapshots_client::SnapshotsClient::new(channel);
    let msg = |action: capi::WriteAction, r: &str, total: i64, expected: &str, offset: i64, data: &[u8]| capi::WriteContentRequest { action: action as i32, r#ref: r.into(), total, expected: expected.into(), offset, data: data.to_vec(), labels: HashMap::new() };
    let blob = b"a blob's bytes".repeat(100);
    let d = digest(&blob);
    // (Each answer, success or error, is taken: only no answer fails the run.)
    let mut labelled = msg(Commit, "r1", blob.len() as i64, &d, 10, &blob[10..]);
    labelled.labels.insert("containerd.io/gc.root".into(), "now".into());
    let _ = step("write", async { let mut a = c.write(tokio_stream::iter(vec![msg(Stat, "r1", blob.len() as i64, "", 0, b""), msg(Write, "r1", 0, "", 0, &blob[..10]), labelled])).await?.into_inner(); while a.message().await?.is_some() {} Ok::<_, tonic::Status>(()) }).await?;
    // The same bytes again under another ref, not said whose they are: there already at the commit.
    let _ = step("again", async { let mut a = c.write(tokio_stream::iter(vec![msg(Write, "r1b", 0, "", 0, &blob), msg(Commit, "r1b", blob.len() as i64, "", blob.len() as i64, b"")])).await?.into_inner(); while a.message().await?.is_some() {} Ok::<_, tonic::Status>(()) }).await?;
    // Said only at its commit to be a blob there already: given up.
    let _ = step("late", async { let mut a = c.write(tokio_stream::iter(vec![msg(Write, "r1c", 0, "", 0, b"x"), msg(Commit, "r1c", blob.len() as i64, &d, 1, b"")])).await?.into_inner(); while a.message().await?.is_some() {} Ok::<_, tonic::Status>(()) }).await?;
    // A caller gone mid-write.
    let _ = step("gone", async {
        let (tx, rx) = tokio::sync::mpsc::channel(4);
        let _ = tx.send(msg(Write, "r1d", 20, "", 0, b"part")).await;
        let mut a = c.write(tokio_stream::wrappers::ReceiverStream::new(rx)).await?.into_inner();
        let _ = a.message().await;
        drop(a);
        tokio::time::sleep(Duration::from_millis(200)).await;
        let _ = tx.send(msg(Write, "r1d", 20, "", 4, b"more")).await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        Ok::<_, tonic::Status>(())
    }).await?;
    let _ = step("info", c.info(capi::InfoRequest { digest: d.clone() })).await?;
    let _ = step("read", async { let mut r = c.read(capi::ReadContentRequest { digest: d.clone(), offset: 0, size: 0 }).await?.into_inner(); while r.message().await?.is_some() {} Ok::<_, tonic::Status>(()) }).await?;
    let mut labels = HashMap::new();
    labels.insert("superci".to_string(), "yes".to_string());
    let _ = step("update", c.update(capi::UpdateRequest { info: Some(capi::Info { digest: d.clone(), labels, ..Default::default() }), update_mask: Some(prost_types::FieldMask { paths: vec!["labels.superci".into()] }) })).await?;
    let _ = step("list", async { let mut l = c.list(capi::ListContentRequest { filters: vec!["labels.superci==yes".into()] }).await?.into_inner(); while l.message().await?.is_some() {} Ok::<_, tonic::Status>(()) }).await?;
    let _ = step("write under way", async { let mut a = c.write(tokio_stream::iter(vec![msg(Write, "r2", 20, "", 0, b"part")])).await?.into_inner(); while a.message().await?.is_some() {} Ok::<_, tonic::Status>(()) }).await?;
    let _ = step("status", c.status(capi::StatusRequest { r#ref: "r2".into() })).await?;
    // Taken up again, then begun again from nothing; a write hashed again at its commit; labels set whole.
    let _ = step("resume", async { let mut a = c.write(tokio_stream::iter(vec![msg(Write, "r2", 20, "", 4, b"more")])).await?.into_inner(); while a.message().await?.is_some() {} Ok::<_, tonic::Status>(()) }).await?;
    let _ = step("truncate", async { let mut a = c.write(tokio_stream::iter(vec![msg(Write, "r2", 20, "", 0, b"anew")])).await?.into_inner(); while a.message().await?.is_some() {} Ok::<_, tonic::Status>(()) }).await?;
    let big = b"kept by its sha512".to_vec();
    let d512 = format!("sha512:{}", sha2::Sha512::digest(&big).iter().map(|x| format!("{x:02x}")).collect::<String>());
    let _ = step("rehash", async { let mut a = c.write(tokio_stream::iter(vec![msg(Write, "r5", 0, "", 0, &big), msg(Commit, "r5", big.len() as i64, &d512, big.len() as i64, b"")])).await?.into_inner(); while a.message().await?.is_some() {} Ok::<_, tonic::Status>(()) }).await?;
    let _ = step("labels whole", c.update(capi::UpdateRequest { info: Some(capi::Info { digest: d512.clone(), labels: [("a".to_string(), "1".to_string())].into_iter().collect(), ..Default::default() }), update_mask: None })).await?;
    let _ = step("labels none", c.update(capi::UpdateRequest { info: Some(capi::Info { digest: d512.clone(), labels: HashMap::new(), ..Default::default() }), update_mask: None })).await?;
    let _ = step("statuses", c.list_statuses(capi::ListStatusesRequest { filters: vec![] })).await?;
    let _ = step("abort", c.abort(capi::AbortRequest { r#ref: "r2".into() })).await?;
    let _ = step("delete", c.delete(capi::DeleteContentRequest { digest: d.clone() })).await?;
    // Snapshots: prepared, committed, one on it, a view, listed, changed, removed, cleaned up.
    if let Ok(Ok(m)) = step("prepare", s.prepare(sapi::PrepareSnapshotRequest { key: "ns/1/a".into(), ..Default::default() })).await {
        // (A file in it: what its usage counts.)
        if let Some(m) = m.into_inner().mounts.first() { let _ = std::fs::write(std::path::Path::new(&m.source).join("file"), b"in the layer"); }
    }
    let _ = step("mounts", s.mounts(sapi::MountsRequest { key: "ns/1/a".into(), ..Default::default() })).await?;
    let _ = step("usage", s.usage(sapi::UsageRequest { key: "ns/1/a".into(), ..Default::default() })).await?;
    let _ = step("commit", s.commit(sapi::CommitSnapshotRequest { name: "ns/1/base".into(), key: "ns/1/a".into(), ..Default::default() })).await?;
    let _ = step("prepare on it", s.prepare(sapi::PrepareSnapshotRequest { key: "ns/1/b".into(), parent: "ns/1/base".into(), ..Default::default() })).await?;
    let _ = step("view", s.view(sapi::ViewSnapshotRequest { key: "ns/1/v".into(), parent: "ns/1/base".into(), ..Default::default() })).await?;
    let _ = step("stat", s.stat(sapi::StatSnapshotRequest { key: "ns/1/base".into(), ..Default::default() })).await?;
    let mut labels = HashMap::new();
    labels.insert("superci".to_string(), "kept".to_string());
    let _ = step("update", s.update(sapi::UpdateSnapshotRequest { info: Some(sapi::Info { name: "ns/1/base".into(), labels, ..Default::default() }), update_mask: Some(prost_types::FieldMask { paths: vec!["labels.superci".into()] }), ..Default::default() })).await?;
    let _ = step("list", async { let mut l = s.list(sapi::ListSnapshotsRequest { filters: vec!["parent==ns/1/base".into()], ..Default::default() }).await?.into_inner(); while l.message().await?.is_some() {} Ok::<_, tonic::Status>(()) }).await?;
    for k in ["ns/1/b", "ns/1/v", "ns/1/base"] { let _ = step("remove", s.remove(sapi::RemoveSnapshotRequest { key: k.into(), ..Default::default() })).await?; }
    let _ = step("cleanup", s.cleanup(sapi::CleanupRequest::default())).await?;
    // A layer from the store, then one pulled; `keep` offers what was pulled.
    let layer = |key: &str, d: &str| sapi::PrepareSnapshotRequest { key: key.into(), labels: [("containerd.io/snapshot.ref".to_string(), format!("chain-{key}")), ("containerd.io/snapshot/cri.layer-digest".to_string(), d.to_string()), ("containerd.io/snapshot/cri.image-layers".to_string(), d.to_string())].into_iter().collect(), ..Default::default() };
    let _ = step("stored layer", s.prepare(layer("moby/1/extract-1", stored))).await?;
    let pulled = b"a pulled layer".repeat(50);
    let _ = step("pull", async { let mut a = c.write(tokio_stream::iter(vec![msg(Commit, "pull-1", pulled.len() as i64, &digest(&pulled), 0, &pulled)])).await?.into_inner(); while a.message().await?.is_some() {} Ok::<_, tonic::Status>(()) }).await?;
    let _ = step("pulled layer", s.prepare(layer("moby/2/extract-2", &digest(&pulled)))).await?;
    let mut keep = Command::new(env!("CARGO_BIN_EXE_superci-layers"));
    keep.args(["keep".to_string(), format!("--link={at}"), format!("--socket={}", socket.display()),
        format!("--content={}", dir.join("content").display()), format!("--index={}", dir.join("index.json").display()), "--most=20".into()]);
    // (Under the same fault, counted from its own start.)
    keep.env("SUPERCI_LAYERS_FAULTS", dir.join("keep-count"));
    if let Some(k) = fault { keep.env("SUPERCI_LAYERS_FAULT", k.to_string()); }
    let keep = keep.output().unwrap();
    if String::from_utf8_lossy(&keep.stderr).contains("panicked") { return Err(format!("keep panicked: {}", String::from_utf8_lossy(&keep.stderr))) }
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn the_service_holds_with_each_call_and_store_operation_failing() {
    let stored = digest(b"a stored layer's blob");
    let at = plane([(format!("{stored}.erofs"), vec![7u8; 300_000])].into_iter().collect());
    let count = std::env::temp_dir().join(format!("superci-layers-sweep-count-{}", std::process::id()));
    let (run, socket) = start(None, &count, &at, &stored).expect("the clean run starts");
    let dir = run.dir.clone();
    scenario(&socket, &dir, &at, &stored, None).await.unwrap();
    let keep_steps: usize = std::fs::read_to_string(dir.join("keep-count")).unwrap_or_default().trim().parse().unwrap_or(0);
    let (log, panicked) = run.stop();
    assert!(!panicked, "{log}");
    let steps: usize = std::fs::read_to_string(&count).unwrap_or_default().trim().parse().unwrap_or(0);
    assert!(steps > 0, "steps counted");
    if std::env::var("SUPERCI_SWEEP_FAULTS").ok().as_deref() != Some("1") { return }
    // (`k/n`: every n-th fault from the k-th, as the test kit's sweeps share theirs.)
    let said = std::env::var("SUPERCI_SWEEP_SLICE").unwrap_or_default();
    let (k0, n) = said.split_once('/').and_then(|(k, n)| Some((k.parse::<usize>().ok()?, n.parse::<usize>().ok()?))).filter(|(k, n)| *k >= 1 && k <= n).unwrap_or((1, 1));
    let mut broke = vec![];
    for k in (1..=steps).filter(|k| (k - 1) % n == k0 - 1) {
        let faulted = std::env::temp_dir().join(format!("superci-layers-sweep-count-{}-{k}", std::process::id()));
        let Some((run, socket)) = start(Some(k), &faulted, &at, &stored) else { continue };
        let dir = run.dir.clone();
        let done = scenario(&socket, &dir, &at, &stored, None).await;
        let (log, panicked) = run.stop();
        if panicked || done.is_err() { broke.push(format!("step {k}: {done:?}\n{}", log.lines().rev().take(8).collect::<Vec<_>>().join("\n"))) }
        let _ = std::fs::remove_file(&faulted);
    }
    // keep's own steps, each failing in turn, the service clean.
    for k in (1..=keep_steps).filter(|k| (k - 1) % n == k0 - 1) {
        let clean = std::env::temp_dir().join(format!("superci-layers-sweep-count-{}-keep{k}", std::process::id()));
        let (run, socket) = start(None, &clean, &at, &stored).unwrap();
        let dir = run.dir.clone();
        let done = scenario(&socket, &dir, &at, &stored, Some(k)).await;
        let (log, panicked) = run.stop();
        if panicked || done.is_err() { broke.push(format!("keep step {k}: {done:?}\n{}", log.lines().rev().take(8).collect::<Vec<_>>().join("\n"))) }
        let _ = std::fs::remove_file(&clean);
    }
    assert!(broke.is_empty(), "{} of {steps} faults broke it:\n{}", broke.len(), broke.join("\n\n"));
}

/// How what needs root runs: as it is, when this is root; through `sudo -n` where there is one without a password;
/// not at all elsewhere (Linux only).
fn as_root() -> Option<Vec<&'static str>> {
    if !cfg!(target_os = "linux") { return None }
    if unsafe { libc::geteuid() } == 0 { return Some(vec![]) }
    Command::new("sudo").args(["-n", "true"]).status().ok().filter(|s| s.success()).map(|_| vec!["sudo", "-n"])
}

/// `program` as root, with `env` (through sudo, on its command line, a coverage tool's settings too).
fn rooted(prefix: &[&str], program: &str, env: &[(String, String)]) -> Command {
    let mut c = Command::new(prefix.first().copied().unwrap_or("env"));
    if !prefix.is_empty() { c.args(&prefix[1..]).arg("env"); }
    c.args(std::env::vars().filter(|(k, _)| k.starts_with("SUPERCOV_")).chain(env.iter().cloned()).map(|(k, v)| format!("{k}={v}"))).arg(program);
    c
}

/// A layer's EROFS file made from its tar, where there is mkfs.erofs.
fn erofs_of(tar: &[u8]) -> Option<Vec<u8>> {
    let out = std::env::temp_dir().join(format!("superci-layers-sweep-erofs-{}", std::process::id()));
    let mut c = Command::new("mkfs.erofs").args(["--tar=f", "--aufs", "-zlz4hc", "-Enoinline_data", "-T0", "--mkfs-time"]).arg(&out).stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn().ok()?;
    c.stdin.take()?.write_all(tar).ok()?;
    if !c.wait().ok()?.success() { return None }
    let b = std::fs::read(&out).ok();
    let _ = std::fs::remove_file(&out);
    b
}

/// A tar of one file, as a layer's blob is.
fn tar_of(name: &str, body: &[u8]) -> Vec<u8> {
    let dir = std::env::temp_dir().join(format!("superci-layers-sweep-tar-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(name), body).unwrap();
    let out = Command::new("tar").args(["-C", dir.to_str().unwrap(), "--owner=0", "--group=0", "-cf", "-", name]).output().unwrap().stdout;
    let _ = std::fs::remove_dir_all(&dir);
    out
}

/// The served scenario's service as root on `dir` (`fault`, if any), its socket opened to the test; `None` when a fault
/// as it starts ended it (said, not by a panic).
fn start_rooted(prefix: &[&str], dir: &std::path::Path, fault: Option<usize>, count: &std::path::Path) -> Option<Child> {
    // (Each start counts its steps in a file of its own: a fault is counted from a program's start.)
    static STARTS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let count = &count.with_extension(STARTS.fetch_add(1, std::sync::atomic::Ordering::SeqCst).to_string());
    let socket = dir.join("layers.sock");
    let _ = Command::new(prefix.first().copied().unwrap_or("rm")).args(if prefix.is_empty() { vec![] } else { prefix[1..].iter().map(|s| s.to_string()).chain(["rm".to_string()]).collect() }).args(["-f".as_ref(), socket.as_os_str()]).status();
    let mut env = vec![("SUPERCI_LAYERS_FAULTS".to_string(), count.display().to_string())];
    if let Some(k) = fault { env.push(("SUPERCI_LAYERS_FAULT".into(), k.to_string())) }
    let log = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("log")).unwrap();
    let mut child = rooted(prefix, env!("CARGO_BIN_EXE_superci-layers"), &env)
        .args([format!("--root={}", dir.join("root").display()), format!("--content={}", dir.join("content").display()), format!("--socket={}", socket.display()),
            format!("--index={}", dir.join("index.json").display()), format!("--containerd={}", dir.join("c.sock").display()), "--parallel=2".into()])
        .stderr(log).spawn().unwrap();
    let until = Instant::now() + Duration::from_secs(20);
    while !socket.exists() {
        if let Ok(Some(_)) = child.try_wait() { return None }
        assert!(Instant::now() < until, "{fault:?}: it did not start");
        std::thread::sleep(Duration::from_millis(10));
    }
    let chmod = |p: &std::path::Path| { let mut c = Command::new(prefix.first().copied().unwrap_or("chmod")); if !prefix.is_empty() { c.args(&prefix[1..]).arg("chmod"); } c.arg("666").arg(p).status().unwrap(); };
    chmod(&socket);
    Some(child)
}

fn stop_rooted(prefix: &[&str], child: &mut Child) {
    let mut c = Command::new(prefix.first().copied().unwrap_or("kill"));
    if !prefix.is_empty() { c.args(&prefix[1..]).arg("kill"); }
    let _ = c.args(["-TERM", &child.id().to_string()]).status();
    let until = Instant::now() + Duration::from_secs(5);
    while Instant::now() < until { if let Ok(Some(_)) = child.try_wait() { return } std::thread::sleep(Duration::from_millis(20)) }
    // (Through sudo, what it started is killed too: killing sudo alone leaves it running.)
    if !prefix.is_empty() { let _ = Command::new(prefix[0]).args(&prefix[1..]).args(["pkill", "-KILL", "-P", &child.id().to_string()]).status(); }
    let _ = child.kill();
    let _ = child.wait();
}

/// The served scenario: a stored layer served (and asked again), one on it, its blob read; restarted; the layer asked
/// for under another name (its file there already); all removed, cleaned up.
async fn served_scenario(prefix: &[&str], dir: &std::path::Path, child: &mut Child, fault: Option<usize>, count: &std::path::Path, d: &str) -> Result<(), String> {
    // (`fault` is the restarted program's: the first one's is given as it starts.)
    let socket = dir.join("layers.sock");
    let layer = |key: &str, chain: &str| sapi::PrepareSnapshotRequest { key: key.into(), labels: [("containerd.io/snapshot.ref".to_string(), chain.to_string()),
        ("containerd.io/snapshot/cri.layer-digest".to_string(), d.to_string()), ("containerd.io/snapshot/cri.image-layers".to_string(), format!(",{d},sha256:{}", "e".repeat(64)))].into_iter().collect(), ..Default::default() };
    // (Its layer fetched ahead first, said in its log either way: each step then the same one in every run.)
    let until = Instant::now() + Duration::from_secs(3);
    while Instant::now() < until && !std::fs::read_to_string(dir.join("log")).unwrap_or_default().contains("fetched ahead") { tokio::time::sleep(Duration::from_millis(20)).await }
    for round in 0..2 {
        let Ok(ch) = step("connect", superci_layers::client::channel(&socket)).await? else { return Ok(()) };
        let mut s = sapi::snapshots_client::SnapshotsClient::new(ch.clone());
        let mut c = capi::content_client::ContentClient::new(ch);
        let _ = step("served", s.prepare(layer(&format!("moby/{round}/a"), &format!("chain-{round}")))).await?;
        let _ = step("again", s.prepare(layer(&format!("moby/{round}/b"), &format!("chain-{round}")))).await?;
        let _ = step("on it", s.prepare(sapi::PrepareSnapshotRequest { key: format!("moby/{round}/top"), parent: format!("moby/{round}/a/superci-layers"), ..Default::default() })).await?;
        // (The layer on a parent of its own, as a later layer of an image is.)
        let mut on_parent = layer(&format!("moby/{round}/c"), &format!("chain-c{round}"));
        on_parent.parent = format!("moby/{round}/a/superci-layers");
        let _ = step("on a parent", s.prepare(on_parent)).await?;
        // (A moment for containerd to be told.)
        tokio::time::sleep(Duration::from_millis(100)).await;
        let _ = step("blob", async { let mut r = c.read(capi::ReadContentRequest { digest: d.to_string(), offset: 0, size: 0 }).await?.into_inner(); while r.message().await?.is_some() {} Ok::<_, tonic::Status>(()) }).await?;
        let _ = step("info", c.info(capi::InfoRequest { digest: d.to_string() })).await?;
        if round == 0 {
            stop_rooted(prefix, child);
            let Some(next) = start_rooted(prefix, dir, fault, count) else { return Ok(()) };
            *child = next;
        }
    }
    let ch = step("connect", superci_layers::client::channel(&socket)).await?.map_err(|e| e.to_string())?;
    let mut s = sapi::snapshots_client::SnapshotsClient::new(ch);
    for k in ["moby/0/top", "moby/1/top", "moby/0/c/superci-layers", "moby/1/c/superci-layers", "moby/0/a/superci-layers", "moby/1/a/superci-layers"] { let _ = step("remove", s.remove(sapi::RemoveSnapshotRequest { key: k.into(), ..Default::default() })).await?; }
    // Restarted once more (the fault its own too): what a removal under one left is put right; then cleaned up, the
    // removed layers' files unmounted.
    stop_rooted(prefix, child);
    let Some(next) = start_rooted(prefix, dir, fault, count) else { return Ok(()) };
    *child = next;
    let ch = step("connect", superci_layers::client::channel(&socket)).await?.map_err(|e| e.to_string())?;
    let _ = step("cleanup", sapi::snapshots_client::SnapshotsClient::new(ch).cleanup(sapi::CleanupRequest::default())).await?;
    Ok(())
}

/// A containerd of the test's own as root on `dir`, the program its content store (as `setup` makes a machine's),
/// where there is one.
fn own_containerd(prefix: &[&str], dir: &std::path::Path) -> Option<Child> {
    if !Command::new("containerd").arg("--version").output().is_ok_and(|o| o.status.success()) { return None }
    // (Without CRI, which this containerd has no use for and whose fixed port one left running would hold.)
    let config = superci_layers::setup::containerd_config("version = 2\ndisabled_plugins = [\"io.containerd.grpc.v1.cri\"]\n").unwrap().replace("/run/superci-layers.sock", dir.join("layers.sock").to_str().unwrap());
    std::fs::write(dir.join("containerd.toml"), config).unwrap();
    let mut c = Command::new(prefix.first().copied().unwrap_or("containerd"));
    if !prefix.is_empty() { c.args(&prefix[1..]).arg("containerd"); }
    let child = c.args(["--config".as_ref(), dir.join("containerd.toml").as_os_str(), "--address".as_ref(), dir.join("c.sock").as_os_str(), "--root".as_ref(), dir.join("containerd-root").as_os_str(), "--state".as_ref(), dir.join("containerd-state").as_os_str()])
        .stdout(std::process::Stdio::null()).stderr(std::fs::File::create(dir.join("containerd.log")).unwrap()).spawn().ok()?;
    let until = Instant::now() + Duration::from_secs(20);
    while !dir.join("c.sock").exists() {
        assert!(Instant::now() < until, "containerd did not start:\n{}", std::fs::read_to_string(dir.join("containerd.log")).unwrap_or_default().lines().rev().take(20).collect::<Vec<_>>().join("\n"));
        std::thread::sleep(Duration::from_millis(20))
    }
    Some(child)
}

#[tokio::test(flavor = "multi_thread")]
async fn the_served_store_holds_with_each_call_and_store_operation_failing() {
    let Some(prefix) = as_root() else { eprintln!("not root on Linux (and no sudo): skipped"); return };
    let tar = tar_of("hello", b"from the store");
    let Some(erofs) = erofs_of(&tar) else { eprintln!("no mkfs.erofs here: skipped"); return };
    let d = digest(&tar);
    let at = plane([(format!("{d}.erofs"), erofs.clone()), (format!("{d}.blob"), tar.clone())].into_iter().collect());
    let index = serde_json::json!({ "layers": { &d: { "size": tar.len(), "erofs": { "url": format!("{at}/files/{d}.erofs"), "size": erofs.len(), "digest": digest(&erofs) }, "blob": { "url": format!("{at}/files/{d}.blob") }, "usage": { "size": 4096, "inodes": 3 }, "ahead": true } } });
    // (A fault for the first program, and one for it restarted: each step of each swept, the other one's run whole.)
    let run = |first: Option<usize>, fault: Option<usize>| { let (prefix, index, d) = (prefix.clone(), index.clone(), d.clone()); async move {
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        // (On disk: the kernel mounts an EROFS file from a filesystem that reads pages itself. Short names: the
        // folder may be deep, as a coverage tool's is, and containerd takes no socket path over 104 bytes.)
        let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("sl-s-{}-{}", std::process::id(), N.fetch_add(1, std::sync::atomic::Ordering::SeqCst)));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("index.json"), index.to_string()).unwrap();
        let count = dir.join("count");
        let mut containerd = own_containerd(&prefix, &dir);
        let done = match start_rooted(&prefix, &dir, first, &count) {
            None => Ok(()),
            Some(mut child) => { let r = served_scenario(&prefix, &dir, &mut child, fault, &count, &d).await; stop_rooted(&prefix, &mut child); r }
        };
        if let Some(c) = &mut containerd { stop_rooted(&prefix, c) }
        let log = std::fs::read_to_string(dir.join("log")).unwrap_or_default();
        let steps: usize = std::fs::read_dir(&dir).unwrap().flatten().filter(|e| e.file_name().to_string_lossy().starts_with("count.")).map(|e| std::fs::read_to_string(e.path()).unwrap_or_default().trim().parse().unwrap_or(0)).max().unwrap_or(0);
        let mut clean = Command::new(prefix.first().copied().unwrap_or("sh"));
        if !prefix.is_empty() { clean.args(&prefix[1..]).arg("sh"); }
        // (Each mount under the folder: one there is not unmounted with the folder, which is no mount itself.)
        let _ = clean.args(["-c", &format!("grep -o ' {0}[^ ]*' /proc/self/mounts | sort -r | xargs -r umount -l; rm -rf {0}", dir.display())]).status();
        (done, log, steps)
    } };
    let (done, log, steps) = run(None, None).await;
    assert!(done.is_ok() && !log.contains("panicked") && log.contains("from the store in"), "{done:?}\n{log}");
    if std::env::var("SUPERCI_SWEEP_FAULTS").ok().as_deref() != Some("1") { return }
    let said = std::env::var("SUPERCI_SWEEP_SLICE").unwrap_or_default();
    let (k0, n) = said.split_once('/').and_then(|(k, n)| Some((k.parse::<usize>().ok()?, n.parse::<usize>().ok()?))).filter(|(k, n)| *k >= 1 && k <= n).unwrap_or((1, 1));
    let mut broke = vec![];
    for k in (1..=steps).filter(|k| (k - 1) % n == k0 - 1) {
        for (first, restarted) in [(Some(k), Some(k)), (None, Some(k))] {
            let (done, log, _) = run(first, restarted).await;
            if done.is_err() || log.contains("panicked") { broke.push(format!("step {k} ({first:?}, {restarted:?}): {done:?}\n{}", log.lines().rev().take(8).collect::<Vec<_>>().join("\n"))) }
        }
    }
    assert!(broke.is_empty(), "{} of {steps} faults broke it:\n{}", broke.len(), broke.join("\n\n"));
}

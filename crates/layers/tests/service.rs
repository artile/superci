//! superci-layers as containerd reaches it: the real program on a socket, asked through containerd's own snapshot and
//! content APIs (the clients its .proto files make). Its content store (writes, reads, labels, writes under way, what
//! it refuses), its snapshots (made, committed, viewed, listed, counted, removed), then layers from SuperCI's store: an
//! index without links (asked for when a layer is used), a layer's file fetched (and, where it cannot be mounted, the
//! layer pulled as usual), the index read again; and after the job (`keep`), a layer pulled that the store lacks
//! offered and its blob uploaded, with what came from the store said.
//! Mounting needs root: run as root (as on a machine) a served layer is mounted; otherwise it falls back, as it would.
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::process::{Child, Command};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};
use superci_layers::containerd::services::content::v1 as capi;
use superci_layers::containerd::services::snapshots::v1 as sapi;
use tonic::Code;

fn digest(b: &[u8]) -> String { format!("sha256:{}", Sha256::digest(b).iter().map(|x| format!("{x:02x}")).collect::<String>()) }

struct Service { child: Child, dir: std::path::PathBuf, socket: std::path::PathBuf }
impl Drop for Service {
    // (Stopped as systemd stops it, SIGTERM, so a coverage tool's buffered evidence is written; killed if it stays.)
    fn drop(&mut self) {
        unsafe { libc::kill(self.child.id() as i32, libc::SIGTERM); }
        let until = Instant::now() + Duration::from_secs(3);
        while Instant::now() < until && !matches!(self.child.try_wait(), Ok(Some(_))) { std::thread::sleep(Duration::from_millis(20)) }
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn start(name: &str, index: Option<&serde_json::Value>) -> Service {
    let dir = std::env::temp_dir().join(format!("superci-layers-service-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let socket = dir.join("layers.sock");
    if let Some(i) = index { std::fs::write(dir.join("index.json"), i.to_string()).unwrap() }
    let child = Command::new(env!("CARGO_BIN_EXE_superci-layers"))
        .args([format!("--root={}", dir.join("root").display()), format!("--content={}", dir.join("content").display()), format!("--socket={}", socket.display()),
            format!("--index={}", dir.join("index.json").display()), format!("--containerd={}", dir.join("no-containerd.sock").display()), "--parallel=4".into(), "--sync-remove".into()])
        .stderr(std::fs::File::create(dir.join("log")).unwrap()).spawn().unwrap();
    let until = Instant::now() + Duration::from_secs(20);
    while !socket.exists() { assert!(Instant::now() < until, "it did not answer: {}", std::fs::read_to_string(dir.join("log")).unwrap_or_default()); std::thread::sleep(Duration::from_millis(20)) }
    Service { child, dir, socket }
}

impl Service {
    fn log(&self) -> String { std::fs::read_to_string(self.dir.join("log")).unwrap_or_default() }
    async fn content(&self) -> capi::content_client::ContentClient<tonic::transport::Channel> { capi::content_client::ContentClient::new(superci_layers::client::channel(&self.socket).await.unwrap()) }
    async fn snapshots(&self) -> sapi::snapshots_client::SnapshotsClient<tonic::transport::Channel> { sapi::snapshots_client::SnapshotsClient::new(superci_layers::client::channel(&self.socket).await.unwrap()) }
}

/// A write session of these messages: the answers, or how it was refused.
async fn write(c: &mut capi::content_client::ContentClient<tonic::transport::Channel>, msgs: Vec<capi::WriteContentRequest>) -> Result<Vec<capi::WriteContentResponse>, tonic::Status> {
    let mut answers = c.write(tokio_stream::iter(msgs)).await?.into_inner();
    let mut out = vec![];
    while let Some(m) = answers.message().await? { out.push(m) }
    Ok(out)
}

fn msg(action: capi::WriteAction, reference: &str, total: i64, expected: &str, offset: i64, data: &[u8]) -> capi::WriteContentRequest {
    capi::WriteContentRequest { action: action as i32, r#ref: reference.into(), total, expected: expected.into(), offset, data: data.to_vec(), labels: HashMap::new() }
}

async fn read_all(c: &mut capi::content_client::ContentClient<tonic::transport::Channel>, d: &str, offset: i64, size: i64) -> Result<Vec<u8>, tonic::Status> {
    let mut s = c.read(capi::ReadContentRequest { digest: d.into(), offset, size }).await?.into_inner();
    let mut out = vec![];
    while let Some(m) = s.message().await? { out.extend(m.data) }
    Ok(out)
}

#[tokio::test(flavor = "multi_thread")]
async fn its_content_store_is_containerds() {
    let s = start("content", None);
    let mut c = s.content().await;
    use capi::WriteAction::*;
    let blob = b"hello, the blob's bytes".repeat(1000);
    let d = digest(&blob);
    // Asked how it stands, written in two parts, committed with labels.
    let mut first = msg(Commit, "ref-1", blob.len() as i64, &d, blob.len() as i64 / 2, &blob[blob.len() / 2..]);
    first.labels.insert("containerd.io/gc.root".into(), "now".into());
    let answers = write(&mut c, vec![msg(Stat, "ref-1", blob.len() as i64, "", 0, b""), msg(Write, "ref-1", 0, "", 0, &blob[..blob.len() / 2]), first]).await.unwrap();
    assert_eq!((answers.first().unwrap().offset, answers.last().unwrap().digest.as_str()), (0, d.as_str()));
    let info = c.info(capi::InfoRequest { digest: d.clone() }).await.unwrap().into_inner().info.unwrap();
    assert_eq!((info.size, info.labels.get("containerd.io/gc.root").map(String::as_str)), (blob.len() as i64, Some("now")));
    assert_eq!(read_all(&mut c, &d, 0, 0).await.unwrap(), blob);
    assert_eq!(read_all(&mut c, &d, 5, 4).await.unwrap(), b", th");
    assert_eq!(read_all(&mut c, &d, blob.len() as i64 + 1, 0).await.unwrap_err().code(), Code::OutOfRange);
    // Its labels changed (the mask's), listed by them.
    let mut changed = info.clone();
    changed.labels = [("superci".to_string(), "yes".to_string())].into_iter().collect();
    let out = c.update(capi::UpdateRequest { info: Some(changed), update_mask: Some(prost_types::FieldMask { paths: vec!["labels.superci".into()] }) }).await.unwrap().into_inner().info.unwrap();
    assert_eq!((out.labels.get("superci").map(String::as_str), out.labels.get("containerd.io/gc.root").map(String::as_str)), (Some("yes"), Some("now")));
    let mut listed = c.list(capi::ListContentRequest { filters: vec!["labels.superci==yes".into()] }).await.unwrap().into_inner();
    let mut found = vec![];
    while let Some(m) = listed.message().await.unwrap() { found.extend(m.info.into_iter().map(|i| i.digest)) }
    assert_eq!(found, [d.clone()]);
    // Written again: it is there already.
    assert_eq!(write(&mut c, vec![msg(Commit, "ref-2", blob.len() as i64, &d, 0, &blob)]).await.unwrap_err().code(), Code::AlreadyExists);
    // A write under way: its status, listed, at the wrong offset refused, given up.
    let other = b"another blob".to_vec();
    write(&mut c, vec![msg(Write, "ref-3", other.len() as i64, &digest(&other), 0, &other[..5])]).await.unwrap();
    let st = c.status(capi::StatusRequest { r#ref: "ref-3".into() }).await.unwrap().into_inner().status.unwrap();
    assert_eq!((st.offset, st.total), (5, other.len() as i64));
    assert_eq!(c.list_statuses(capi::ListStatusesRequest { filters: vec!["ref==ref-3".into()] }).await.unwrap().into_inner().statuses.len(), 1);
    assert_eq!(write(&mut c, vec![msg(Write, "ref-3", 0, "", 9, b"x")]).await.unwrap_err().code(), Code::OutOfRange);
    c.abort(capi::AbortRequest { r#ref: "ref-3".into() }).await.unwrap();
    assert_eq!(c.status(capi::StatusRequest { r#ref: "ref-3".into() }).await.unwrap_err().code(), Code::NotFound);
    // A commit that is not what it said: refused, nothing kept.
    assert_eq!(write(&mut c, vec![msg(Commit, "ref-4", 3, &digest(b"abd"), 0, b"abc")]).await.unwrap_err().code(), Code::FailedPrecondition);
    // What it refuses to begin with.
    assert_eq!(write(&mut c, vec![msg(Stat, "", 0, "", 0, b"")]).await.unwrap_err().code(), Code::InvalidArgument);
    assert_eq!(c.info(capi::InfoRequest { digest: "sha256:nope".into() }).await.unwrap_err().code(), Code::InvalidArgument);
    // Deleted: gone.
    c.delete(capi::DeleteContentRequest { digest: d.clone() }).await.unwrap();
    assert_eq!(c.info(capi::InfoRequest { digest: d.clone() }).await.unwrap_err().code(), Code::NotFound);
    // (Deleting what is gone is no error: containerd's local store removes it with RemoveAll, which says nothing.)
    c.delete(capi::DeleteContentRequest { digest: d }).await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn its_snapshots_are_containerds_overlay() {
    let s = start("snapshots", None);
    let mut c = s.snapshots().await;
    let labels = |k: &str, v: &str| -> HashMap<String, String> { [(k.to_string(), v.to_string())].into_iter().collect() };
    // A first layer: a folder bound as it is; committed; a second on it, mounted over it; a view of the first.
    let m = c.prepare(sapi::PrepareSnapshotRequest { snapshotter: "superci".into(), key: "ns/1/a".into(), parent: String::new(), labels: labels("l", "1") }).await.unwrap().into_inner().mounts;
    assert_eq!(m[0].r#type, "bind");
    std::fs::write(std::path::Path::new(&m[0].source).join("file"), b"in the first layer").unwrap();
    assert_eq!(c.prepare(sapi::PrepareSnapshotRequest { key: "ns/1/a".into(), ..Default::default() }).await.unwrap_err().code(), Code::AlreadyExists);
    assert_eq!(c.mounts(sapi::MountsRequest { key: "ns/1/a".into(), ..Default::default() }).await.unwrap().into_inner().mounts[0].source, m[0].source);
    let usage = c.usage(sapi::UsageRequest { key: "ns/1/a".into(), ..Default::default() }).await.unwrap().into_inner();
    assert!(usage.size > 0 && usage.inodes >= 2, "{usage:?}");
    c.commit(sapi::CommitSnapshotRequest { name: "ns/1/base".into(), key: "ns/1/a".into(), ..Default::default() }).await.unwrap();
    assert_eq!(c.commit(sapi::CommitSnapshotRequest { name: "ns/1/x".into(), key: "ns/1/missing".into(), ..Default::default() }).await.unwrap_err().code(), Code::NotFound);
    let top = c.prepare(sapi::PrepareSnapshotRequest { key: "ns/1/b".into(), parent: "ns/1/base".into(), ..Default::default() }).await.unwrap().into_inner().mounts;
    assert!(top[0].r#type == "overlay" && top[0].options.iter().any(|o| o.starts_with("lowerdir=")), "{top:?}");
    let view = c.view(sapi::ViewSnapshotRequest { key: "ns/1/v".into(), parent: "ns/1/base".into(), ..Default::default() }).await.unwrap().into_inner().mounts;
    assert!(view[0].options.contains(&"ro".to_string()), "{view:?}");
    // Its info, changed, listed by what it says.
    let info = c.stat(sapi::StatSnapshotRequest { key: "ns/1/base".into(), ..Default::default() }).await.unwrap().into_inner().info.unwrap();
    assert_eq!((info.name.as_str(), info.kind), ("ns/1/base", sapi::Kind::Committed as i32));
    let mut changed = info.clone();
    changed.labels = labels("superci", "kept");
    let out = c.update(sapi::UpdateSnapshotRequest { info: Some(changed), update_mask: Some(prost_types::FieldMask { paths: vec!["labels.superci".into()] }), ..Default::default() }).await.unwrap().into_inner().info.unwrap();
    // (A committed snapshot has the labels its commit gave, not those of the active one it was: as containerd's.)
    assert_eq!(out.labels, labels("superci", "kept"));
    let mut listed = c.list(sapi::ListSnapshotsRequest { filters: vec!["parent==ns/1/base".into()], ..Default::default() }).await.unwrap().into_inner();
    let mut names = vec![];
    while let Some(m) = listed.message().await.unwrap() { names.extend(m.info.into_iter().map(|i| i.name)) }
    names.sort();
    assert_eq!(names, ["ns/1/b", "ns/1/v"]);
    // One with children is kept; removed once they are; then what is left behind is cleaned up.
    assert_eq!(c.remove(sapi::RemoveSnapshotRequest { key: "ns/1/base".into(), ..Default::default() }).await.unwrap_err().code(), Code::FailedPrecondition);
    for k in ["ns/1/b", "ns/1/v", "ns/1/base"] { c.remove(sapi::RemoveSnapshotRequest { key: k.into(), ..Default::default() }).await.unwrap(); }
    assert_eq!(c.stat(sapi::StatSnapshotRequest { key: "ns/1/base".into(), ..Default::default() }).await.unwrap_err().code(), Code::NotFound);
    c.cleanup(sapi::CleanupRequest::default()).await.unwrap();
}

/// SuperCI's store and control plane for the test, on one port: files by ranges (`/files/…`), links to a layer's
/// files (`POST /links`), offers (every layer asked to be uploaded, `POST /layers-propose`), uploads (`PUT /up/…`,
/// taken only with the SHA-256 they say), their word (`POST /uploaded`) and what was kept (`POST /layers-kept`).
/// What it was asked, kept.
#[derive(Default)]
struct Plane { files: HashMap<String, Vec<u8>>, asked: Vec<String>, uploaded: HashMap<String, Vec<u8>>, kept: Option<serde_json::Value> }

fn plane(files: HashMap<String, Vec<u8>>) -> (Arc<Mutex<Plane>>, String) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let at = format!("http://{}", listener.local_addr().unwrap());
    let p = Arc::new(Mutex::new(Plane { files, ..Default::default() }));
    let (pp, base) = (p.clone(), at.clone());
    std::thread::spawn(move || for stream in listener.incoming().flatten() {
        let (p, base) = (pp.clone(), base.clone());
        std::thread::spawn(move || {
            let mut r = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            if r.read_line(&mut line).is_err() { return }
            let mut parts = line.split_whitespace();
            let (method, path) = (parts.next().unwrap_or_default().to_string(), parts.next().unwrap_or_default().to_string());
            let mut headers = HashMap::new();
            loop {
                let mut h = String::new();
                if r.read_line(&mut h).is_err() || h.trim().is_empty() { break }
                if let Some((k, v)) = h.split_once(':') { headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string()); }
            }
            let mut body = vec![0; headers.get("content-length").and_then(|l| l.parse().ok()).unwrap_or(0)];
            let _ = r.read_exact(&mut body);
            p.lock().unwrap().asked.push(format!("{method} {path}"));
            let mut out = stream;
            let send = |out: &mut std::net::TcpStream, status: u16, extra: &str, body: &[u8]| { let _ = out.write_all(format!("HTTP/1.1 {status} X\r\ncontent-length: {}\r\n{extra}connection: close\r\n\r\n", body.len()).as_bytes()); let _ = out.write_all(body); };
            let json: serde_json::Value = serde_json::from_slice(&body).unwrap_or_default();
            match (method.as_str(), path.as_str()) {
                ("GET", f) if f.starts_with("/files/") => {
                    let Some(b) = p.lock().unwrap().files.get(&f[7..]).cloned() else { return send(&mut out, 404, "", b"") };
                    match headers.get("range").and_then(|r| r.strip_prefix("bytes=")).and_then(|r| r.split_once('-')) {
                        Some((a, z)) => { let (a, z): (usize, usize) = (a.parse().unwrap(), z.parse::<usize>().unwrap().min(b.len() - 1)); send(&mut out, 206, &format!("content-range: bytes {a}-{z}/{}\r\n", b.len()), &b[a..=z]) }
                        None => send(&mut out, 200, "", &b),
                    }
                }
                ("POST", "/links") => {
                    let layers: serde_json::Map<String, serde_json::Value> = json["digests"].as_array().unwrap().iter().map(|d| {
                        let d = d.as_str().unwrap();
                        let size = p.lock().unwrap().files.get(&format!("{d}.erofs")).map(|f| f.len()).unwrap_or(0);
                        (d.to_string(), serde_json::json!({ "size": 1, "erofs": { "url": format!("{base}/files/{d}.erofs"), "size": size }, "blob": { "url": format!("{base}/files/{d}.blob") } }))
                    }).collect();
                    send(&mut out, 200, "", serde_json::json!({ "layers": layers }).to_string().as_bytes())
                }
                ("POST", "/layers-propose") => {
                    let answers: serde_json::Map<String, serde_json::Value> = json["layers"].as_array().unwrap().iter().map(|l| {
                        let d = l["digest"].as_str().unwrap();
                        let sum = { use base64::Engine; base64::engine::general_purpose::STANDARD.encode(hex(&d[7..])) };
                        (d.to_string(), serde_json::json!({ "do": "upload", "url": format!("{base}/up/{d}"), "id": d, "headers": { "x-amz-checksum-sha256": sum } }))
                    }).collect();
                    send(&mut out, 200, "", serde_json::json!({ "layers": answers, "uploaded": format!("{base}/uploaded") }).to_string().as_bytes())
                }
                ("PUT", u) if u.starts_with("/up/") => {
                    let ok = headers.get("x-amz-checksum-sha256").is_some_and(|s| { use base64::Engine; base64::engine::general_purpose::STANDARD.encode(Sha256::digest(&body)) == *s });
                    if ok { p.lock().unwrap().uploaded.insert(u[4..].to_string(), body); send(&mut out, 200, "", b"") } else { send(&mut out, 400, "", b"BadDigest") }
                }
                ("POST", "/uploaded") => send(&mut out, 200, "", br#"{"do": "converting"}"#),
                ("POST", "/layers-kept") => { p.lock().unwrap().kept = Some(json); send(&mut out, 200, "", b"{}") }
                _ => send(&mut out, 404, "", b""),
            }
        });
    });
    (p, at)
}

fn hex(s: &str) -> Vec<u8> { (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect() }

#[tokio::test(flavor = "multi_thread")]
async fn layers_from_the_store_are_fetched_when_used_and_pulled_ones_kept_after_the_job() {
    // A layer the store has (its EROFS file a stand-in: mounting it is not what is tested here), and its index
    // without links.
    let (stored, erofs) = (digest(b"a stored layer's blob"), vec![7u8; 300_000]);
    let (p, at) = plane([(format!("{stored}.erofs"), erofs.clone())].into_iter().collect());
    let index = serde_json::json!({ "layers": { &stored: { "size": 21, "erofs": { "size": erofs.len() }, "blob": {} } }, "refresh": { "url": format!("{at}/links"), "token": "" } });
    let s = start("store", Some(&index));
    let mut snap = s.snapshots().await;
    let layer = |key: &str, d: &str, image: &str| sapi::PrepareSnapshotRequest { key: key.into(), parent: String::new(), labels: [("containerd.io/snapshot.ref".to_string(), format!("chain-{key}")),
        ("containerd.io/snapshot/cri.layer-digest".to_string(), d.to_string()), ("containerd.io/snapshot/cri.image-layers".to_string(), image.to_string())].into_iter().collect(), ..Default::default() };
    // The stored layer: links asked for (with its image's), its file fetched; served (as root) or pulled as usual.
    let served = snap.prepare(layer("moby/1/extract-1", &stored, &stored)).await;
    assert!(p.lock().unwrap().asked.iter().any(|a| a == "POST /links") && p.lock().unwrap().asked.iter().any(|a| a.starts_with(&format!("GET /files/{stored}.erofs"))), "{:?}", p.lock().unwrap().asked);
    match served {
        Err(e) => assert_eq!(e.code(), Code::AlreadyExists, "served: containerd told it exists"),
        Ok(m) => { assert_eq!(m.into_inner().mounts[0].r#type, "bind"); assert!(s.log().contains(&format!("{stored} pulled as usual")), "{}", s.log()) }
    }
    assert!(s.log().contains(&format!("{stored}'s file (0.3 MB) fetched in")), "{}", s.log());
    // A layer the store lacks: pulled as usual (its blob written to the content store as a pull does), with its
    // image's manifest naming both layers, as containerd's records do.
    let blob = b"a layer the store lacks".repeat(200);
    let lacked = digest(&blob);
    let mut content = s.content().await;
    let mut pull = msg(capi::WriteAction::Commit, "pull-1", blob.len() as i64, &lacked, 0, &blob);
    pull.labels.insert("containerd.io/distribution.source.ghcr.io".into(), "acme/app".into());
    write(&mut content, vec![pull]).await.unwrap();
    let manifest = b"{\"layers\": []}".to_vec();
    let mut m = msg(capi::WriteAction::Commit, "manifest-1", manifest.len() as i64, &digest(&manifest), 0, &manifest);
    m.labels.extend([("containerd.io/gc.ref.content.l.0".to_string(), stored.clone()), ("containerd.io/gc.ref.content.l.1".to_string(), lacked.clone())]);
    write(&mut content, vec![m]).await.unwrap();
    let mounts = snap.prepare(layer("moby/2/extract-2", &lacked, &lacked)).await.unwrap().into_inner().mounts;
    assert_eq!(mounts[0].r#type, "bind");
    assert!(!p.lock().unwrap().asked.iter().any(|a| a.contains("layers-propose")), "nothing offered while the job runs");
    // After the job: offered and uploaded at the link the control plane gives, with the SHA-256 it said; what came
    // from the store and what was pulled, said.
    let kept = Command::new(env!("CARGO_BIN_EXE_superci-layers")).args(["keep".to_string(), format!("--link={at}"), format!("--socket={}", s.socket.display()),
        format!("--content={}", s.dir.join("content").display()), format!("--index={}", s.dir.join("index.json").display()), "--most=20".into()]).output().unwrap();
    assert!(kept.status.success(), "{}", String::from_utf8_lossy(&kept.stderr));
    assert_eq!(p.lock().unwrap().uploaded.get(&lacked), Some(&blob), "{}", String::from_utf8_lossy(&kept.stderr));
    assert!(p.lock().unwrap().asked.iter().any(|a| a == "POST /uploaded"));
    let said = p.lock().unwrap().kept.clone().unwrap();
    assert_eq!((said["served"].as_u64(), said["pulled"].as_u64(), said["pulled_bytes"].as_u64(), said["uploaded"].as_u64()), (Some(1), Some(1), Some(blob.len() as u64), Some(1)), "{said}");
    // The index written again and SIGHUP: read again.
    std::fs::write(s.dir.join("index.json"), serde_json::json!({ "layers": {} }).to_string()).unwrap();
    unsafe { libc::kill(s.child.id() as i32, libc::SIGHUP); }
    let until = Instant::now() + Duration::from_secs(10);
    while !s.log().contains("0 layers in the store's index") { assert!(Instant::now() < until, "{}", s.log()); tokio::time::sleep(Duration::from_millis(50)).await }
}

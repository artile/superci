//! After a job (its runner has exited, the machine not yet off): the image layers Docker pulled that SuperCI's store
//! lacks, offered to the control plane and uploaded where it asks, so that the repository's next job has them; then
//! what the job had from the store and what it pulled, said for the job's record. The job's own time is never
//! touched, and nothing here can fail it: whatever goes wrong is logged, and the machine goes off as it would.
//!
//! The layers are read from containerd's own records (Docker's namespace, `moby`): each image manifest names its
//! layers in its labels (`containerd.io/gc.ref.content.l.<n>`), each layer's labels say where it was pulled from
//! (`containerd.io/distribution.source.<registry>`). That holds on any Docker that keeps its images in containerd,
//! with SuperCI's snapshotter or without it.

use crate::client::channel;
use crate::containerd::services::content::v1 as capi;
use crate::index::Index;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Docker's namespace in containerd.
pub const NAMESPACE: &str = "moby";
/// A manifest's label naming one of its layers: `containerd.io/gc.ref.content.l.<n>` = the layer's digest.
const LAYER_REF: &str = "containerd.io/gc.ref.content.l.";
/// containerd's label of where a blob came from: `containerd.io/distribution.source.<registry>` = `<repo>[,<repo>…]`.
const SOURCE: &str = "containerd.io/distribution.source.";
/// Uploads at once (the job is over: the machine's disk and network are its).
const AT_ONCE: usize = 4;

pub struct Options { pub link: String, pub socket: PathBuf, pub content: PathBuf, pub index: PathBuf, pub most: Duration,
    /// The blobs containerd had at the machine's start (one hex digest a line): its image's own, none of it offered.
    pub boot: PathBuf,
    /// Whether the store was switched in (see `setup::STATE`): what came from it counted only then.
    pub state: PathBuf }

/// A layer the job's images name: its digest, size, where it was pulled from.
#[derive(Debug, Clone, PartialEq)]
pub struct Pulled { pub digest: String, pub size: u64, pub sources: Vec<Value> }

/// The layers the images in containerd's records name (each once), from each blob's labels (digest → size, labels).
pub fn named(infos: &BTreeMap<String, (u64, HashMap<String, String>)>) -> Vec<String> {
    let mut named: Vec<String> = infos.values().flat_map(|(_, labels)| labels.iter().filter(|(k, _)| k.starts_with(LAYER_REF)).map(|(_, v)| v.clone())).collect();
    named.sort();
    named.dedup();
    named
}

/// Those of them containerd keeps, with their sizes and where they came from.
pub fn layers_of(infos: &BTreeMap<String, (u64, HashMap<String, String>)>) -> Vec<Pulled> {
    named(infos).into_iter().filter_map(|d| { let (size, labels) = infos.get(&d)?; Some(Pulled { digest: d, size: *size, sources: sources(labels) }) }).collect()
}

/// Where a blob was pulled from, by its labels (registries and repositories, as the control plane takes them).
pub fn sources<'a>(labels: impl IntoIterator<Item = (&'a String, &'a String)>) -> Vec<Value> {
    let mut out: Vec<Value> = vec![];
    let mut labels: Vec<(&String, &String)> = labels.into_iter().collect();
    labels.sort();
    for (k, v) in labels {
        let Some(registry) = k.strip_prefix(SOURCE) else { continue };
        for repo in v.split(',').filter(|r| !r.is_empty()) { out.push(json!({ "registry": registry, "repository": repo })) }
    }
    out
}

/// Every blob containerd keeps in Docker's namespace, with its size and labels.
async fn infos(socket: &Path) -> Result<BTreeMap<String, (u64, HashMap<String, String>)>, String> {
    let ch = channel(socket).await.and_then(crate::faults::said)?;
    let mut req = tonic::Request::new(capi::ListContentRequest { filters: vec![] });
    req.metadata_mut().insert("containerd-namespace", NAMESPACE.parse().map_err(|_| "namespace")?);
    let mut stream = capi::content_client::ContentClient::new(ch).list(req).await.and_then(crate::faults::pass).map_err(|e| e.message().to_string())?.into_inner();
    let mut out = BTreeMap::new();
    while let Some(r) = stream.message().await.and_then(crate::faults::pass).map_err(|e| e.message().to_string())? {
        for i in r.info { out.insert(i.digest, (i.size.max(0) as u64, i.labels.into_iter().collect())); }
    }
    Ok(out)
}

/// What a keep did: what came from the store, what was pulled, what was uploaded, and what of the pulled the store had
/// already (or has on its way: its next job gets it all the same).
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Done { pub served: u64, pub served_bytes: u64, pub pulled: u64, pub pulled_bytes: u64, pub uploaded: u64, pub uploaded_bytes: u64, pub had: u64, pub had_bytes: u64,
    /// Of the pulled and not kept, why: no whole file of it here, the control plane would not take it, its upload
    /// failed (the first failure's words in `note`).
    pub missing: u64, pub missing_bytes: u64, pub refused: u64, pub failed: u64, pub note: String,
    /// Why the store was not switched in on this machine, when its setup said (nothing came from it then).
    pub off: String }

/// The keep (see the module); 0 whatever happened (a machine's end never waits on it beyond `most`).
pub fn keep(o: &Options) -> i32 {
    let began = Instant::now();
    let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() { Ok(r) => r, Err(e) => { eprintln!("superci-layers keep: {e}"); return 0 } };
    let agent = crate::fetch::agent();
    let infos = match rt.block_on(infos(&o.socket)) {
        Ok(i) => i,
        Err(e) => {
            eprintln!("superci-layers keep: containerd: {e}");
            let _ = post(&agent, &format!("{}/layers-kept", o.link), &json!({ "error": format!("containerd: {e}") }));
            return 0
        }
    };
    let index = Index::load(&o.index).unwrap_or_default();
    let boot: std::collections::HashSet<String> = std::fs::read_to_string(&o.boot).unwrap_or_default().lines().map(|h| format!("sha256:{}", h.trim())).collect();
    let mut done = Done::default();
    // (Switched in, or not: then everything was pulled as Docker pulls, what the store has too.)
    let state = std::fs::read_to_string(&o.state).unwrap_or_default();
    let on = state.starts_with("on");
    done.off = state.strip_prefix("off: ").map(|w| w.trim().chars().take(200).collect()).unwrap_or_default();
    // (A layer from the store: counted as the index has it, whether or not containerd keeps its blob.)
    let mut served_digests = vec![];
    if on { for d in named(&infos).into_iter().filter(|d| !boot.contains(d)) { if let Some(l) = index.get(&d) { done.served += 1; done.served_bytes += l.size; served_digests.push(d) } } }
    // (Every layer the job's images have, but the machine's own: fetched ahead at its next run's start.)
    let used_digests: Vec<String> = layers_of(&infos).into_iter().map(|l| l.digest).filter(|d| !boot.contains(d)).take(500).collect();
    let mut offered = vec![];
    for l in layers_of(&infos).into_iter().filter(|l| !(on && index.get(&l.digest).is_some()) && !boot.contains(&l.digest)) {
        done.pulled += 1;
        done.pulled_bytes += l.size;
        // (Pulled though the store has it: it was not switched in. Kept already.)
        if index.get(&l.digest).is_some() { done.had += 1; done.had_bytes += l.size; continue }
        // (Only a blob kept here whole.)
        if blob_path(&o.content, &l.digest).and_then(|p| std::fs::metadata(p).ok()).is_some_and(|m| m.len() == l.size) { offered.push(l) } else { done.missing += 1; done.missing_bytes += l.size }
    }
    let sizes: std::collections::HashMap<String, u64> = offered.iter().map(|l| (l.digest.clone(), l.size)).collect();
    // Something to upload: what it had and pulled said first (the run's summary is printed as the job ends; the
    // uploads may take minutes).
    if !offered.is_empty() {
        let first = json!({ "served": done.served, "served_bytes": done.served_bytes, "pulled": done.pulled, "pulled_bytes": done.pulled_bytes, "off": done.off, "keeping": true });
        if let Err(e) = post(&agent, &format!("{}/layers-kept", o.link), &first) { eprintln!("superci-layers keep: saying what it had: {e}") }
    }
    let said = offer(&agent, &o.link, &o.content, offered, began + o.most);
    let (uploaded, bytes, had) = (said.uploaded, said.bytes, said.had);
    (done.uploaded, done.uploaded_bytes, done.refused, done.failed, done.note) = (uploaded, bytes, said.refused, said.failed, said.note);
    (done.had, done.had_bytes) = (done.had + had.len() as u64, done.had_bytes + had.iter().filter_map(|d| sizes.get(d)).sum::<u64>());
    let report = json!({ "served": done.served, "served_bytes": done.served_bytes, "pulled": done.pulled, "pulled_bytes": done.pulled_bytes,
        "uploaded": done.uploaded, "uploaded_bytes": done.uploaded_bytes, "had": done.had, "had_bytes": done.had_bytes,
        "missing": done.missing, "missing_bytes": done.missing_bytes, "refused": done.refused, "failed": done.failed, "note": done.note, "off": done.off, "served_digests": served_digests, "used": used_digests, "seconds": (began.elapsed().as_secs_f64() * 10.0).round() / 10.0 });
    eprintln!("superci-layers keep: {report}");
    if let Err(e) = post(&agent, &format!("{}/layers-kept", o.link), &report) { eprintln!("superci-layers keep: saying what was kept: {e}") }
    0
}

/// A blob's file in containerd's content folder.
fn blob_path(content: &Path, digest: &str) -> Option<PathBuf> { digest.strip_prefix("sha256:").filter(|h| h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit())).map(|h| content.join("blobs/sha256").join(h)) }

fn post(agent: &ureq::Agent, url: &str, body: &Value) -> Result<Value, String> {
    let res = agent.post(url).header("content-type", "application/json").send(body.to_string().as_bytes()).and_then(crate::faults::pass).map_err(|e| e.to_string())?;
    let status = res.status().as_u16();
    let text = res.into_body().read_to_string().unwrap_or_default();
    if status != 200 { return Err(format!("the control plane answered {status}: {}", text.chars().take(200).collect::<String>())) }
    serde_json::from_str(&text).map_err(|e| e.to_string())
}

/// What an offer came to: how many were uploaded and their bytes, the layers the store had already, how many it
/// would not take, how many uploads failed, and the first failure's words.
#[derive(Default)]
struct Offered { uploaded: u64, bytes: u64, had: Vec<String>, refused: u64, failed: u64, note: String }

/// The layers offered (fifty at a time), then those the control plane asks for uploaded, `AT_ONCE` at a time, until
/// `until`.
fn offer(agent: &ureq::Agent, link: &str, content: &Path, offered: Vec<Pulled>, until: Instant) -> Offered {
    let mut asked: Vec<(String, Value)> = vec![];
    let mut out = Offered::default();
    let total_offered = offered.len() as u64;
    let mut uploaded_at = String::new();
    for chunk in offered.chunks(50) {
        let body = json!({ "layers": chunk.iter().map(|l| json!({ "digest": l.digest, "size": l.size, "sources": l.sources })).collect::<Vec<_>>() });
        match post(agent, &format!("{link}/layers-propose"), &body) {
            Ok(answer) => {
                uploaded_at = answer["uploaded"].as_str().unwrap_or_default().to_string();
                for (d, a) in answer["layers"].as_object().into_iter().flatten() {
                    match a["do"].as_str() { Some("upload") => asked.push((d.clone(), a.clone())), Some("have") => out.had.push(d.clone()), _ => out.refused += 1 }
                }
            }
            Err(e) => { eprintln!("superci-layers keep: offering layers: {e}"); return Offered { failed: total_offered, note: format!("offering them: {e}"), ..out } }
        }
    }
    let asked_n = asked.len() as u64;
    let next = std::sync::Mutex::new(asked.into_iter());
    let total = std::sync::Mutex::new((0u64, 0u64));
    let first_error = std::sync::Mutex::new(String::new());
    std::thread::scope(|s| {
        (0..AT_ONCE).for_each(|_| { s.spawn(|| loop {
                if Instant::now() >= until { return }
                let Some((digest, a)) = next.lock().unwrap().next() else { return };
                let began = Instant::now();
                match upload(agent, content, &digest, &a, until).and_then(|n| post(agent, &uploaded_at, &json!({ "id": a["id"] })).map(|said| (n, said))) {
                    Ok((n, said)) => {
                        eprintln!("superci-layers keep: {digest} uploaded ({n} bytes in {:.1} s): {}", began.elapsed().as_secs_f64(), said["do"].as_str().unwrap_or("?"));
                        let mut t = total.lock().unwrap();
                        t.0 += 1;
                        t.1 += n;
                    }
                    Err(e) => { eprintln!("superci-layers keep: {digest} not uploaded: {e}"); let mut f = first_error.lock().unwrap(); if f.is_empty() { *f = e.chars().take(200).collect() } }
                }
            }); });
    });
    let (n, bytes) = total.into_inner().unwrap();
    // (Asked for and not uploaded: failed, or out of time.)
    let failed = asked_n - n;
    let note = first_error.into_inner().unwrap();
    Offered { uploaded: n, bytes, failed, note: if note.is_empty() && failed > 0 { "out of time".into() } else { note }, ..out }
}

/// The blob put at the link the control plane gave, with the headers it said (given up at `until`).
fn upload(agent: &ureq::Agent, content: &Path, digest: &str, a: &Value, until: Instant) -> Result<u64, String> {
    let path = blob_path(content, digest).ok_or("not a digest")?;
    let file = std::fs::File::open(&path).and_then(crate::faults::pass).map_err(|e| format!("{}: {e}", path.display()))?;
    let size = file.metadata().and_then(crate::faults::pass).map_err(|e| e.to_string())?.len();
    let mut req = agent.put(a["url"].as_str().ok_or("no link")?).header("content-length", size.to_string())
        .config().timeout_global(Some(until.saturating_duration_since(Instant::now()).max(Duration::from_secs(1)))).build();
    for (k, v) in a["headers"].as_object().into_iter().flatten() { if let Some(v) = v.as_str() { req = req.header(k.as_str(), v) } }
    let res = req.send(ureq::SendBody::from_reader(&mut std::io::BufReader::with_capacity(1 << 20, file))).and_then(crate::faults::pass).map_err(|e| e.to_string())?;
    if res.status().as_u16() >= 300 { return Err(format!("the bucket answered {}", res.status())) }
    Ok(size)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(pairs: &[(&str, &str)]) -> HashMap<String, String> { pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect() }

    #[test]
    fn the_layers_are_those_the_manifests_name_with_where_they_came_from() {
        let infos: BTreeMap<String, (u64, HashMap<String, String>)> = [
            ("sha256:index".to_string(), (300, labels(&[("containerd.io/gc.ref.content.m.0", "sha256:manifest")]))),
            ("sha256:manifest".to_string(), (900, labels(&[("containerd.io/gc.ref.content.config", "sha256:config"), ("containerd.io/gc.ref.content.l.0", "sha256:a"), ("containerd.io/gc.ref.content.l.1", "sha256:b")]))),
            ("sha256:other".to_string(), (800, labels(&[("containerd.io/gc.ref.content.l.0", "sha256:b"), ("containerd.io/gc.ref.content.l.1", "sha256:gone")]))),
            ("sha256:config".to_string(), (50, labels(&[]))),
            ("sha256:a".to_string(), (10, labels(&[("containerd.io/distribution.source.docker.io", "library/postgres,acme/pg"), ("containerd.io/uncompressed", "sha256:x")]))),
            ("sha256:b".to_string(), (20, labels(&[("containerd.io/distribution.source.ghcr.io", "acme/app")]))),
        ].into_iter().collect();
        assert_eq!(layers_of(&infos), [
            Pulled { digest: "sha256:a".into(), size: 10, sources: vec![json!({ "registry": "docker.io", "repository": "library/postgres" }), json!({ "registry": "docker.io", "repository": "acme/pg" })] },
            Pulled { digest: "sha256:b".into(), size: 20, sources: vec![json!({ "registry": "ghcr.io", "repository": "acme/app" })] },
        ], "one layer named by two images is one; one containerd no longer has is none");
        assert_eq!(blob_path(Path::new("/c"), &format!("sha256:{}", "a".repeat(64))), Some(PathBuf::from(format!("/c/blobs/sha256/{}", "a".repeat(64)))));
        assert_eq!(blob_path(Path::new("/c"), "sha256:../x"), None);
    }
}

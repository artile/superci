//! Layers this machine pulled that SuperCI's store lacks, proposed to the control plane once pulls have stopped
//! for a while (the job's containers are up by then), with where each was pulled from (containerd's
//! `distribution.source` labels). The control plane fetches a public one from its registry itself; for any other it
//! gives a link to upload the blob to, which takes only a body of the blob's SHA-256. Uploads go one at a time, at
//! a bounded rate, so the job keeps the machine's network. A layer answered "later" (the converter has enough at
//! once) is proposed again a little later, while the machine runs. Nothing here is needed by the job: a failure is
//! logged.

use crate::content::Digest;
use crate::store::Layers;
use serde_json::{json, Value};
use std::io::Read;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Pulls stopped this long: proposed.
pub const QUIET: Duration = Duration::from_secs(15);
/// Uploads go at this rate at most (bytes a second).
pub const RATE: u64 = 64 << 20;
/// containerd's label of where a blob came from: `containerd.io/distribution.source.<registry>` = `<repo>[,<repo>…]`.
const SOURCE: &str = "containerd.io/distribution.source.";

/// A layer answered "later" (others are being converted) is proposed again this long after, so many times at most.
pub const AGAIN: Duration = Duration::from_secs(30);
pub const AGAIN_MOST: u32 = 20;

/// The proposer's loop: every few seconds, what was pulled once pulls are quiet, and what is due again.
pub async fn run(layers: Arc<Layers>) {
    let mut again: Vec<(Instant, u32, (String, String))> = vec![];
    loop {
        tokio::time::sleep(Duration::from_secs(5)).await;
        let Some(st) = &layers.store else { return };
        let Some(url) = st.index.read().unwrap().propose.clone() else { continue };
        let mut pulled: Vec<(u32, (String, String))> = st.take_pulled(QUIET).into_iter().map(|p| (0, p)).collect();
        let (due, later): (Vec<_>, Vec<_>) = std::mem::take(&mut again).into_iter().partition(|(at, _, _)| at.elapsed() >= AGAIN);
        again = later;
        pulled.extend(due.into_iter().map(|(_, n, p)| (n, p)));
        if pulled.is_empty() { continue }
        let tries: std::collections::HashMap<String, (u32, String)> = pulled.iter().map(|(n, (ns, d))| (d.clone(), (*n, ns.clone()))).collect();
        let mut proposed = vec![];
        for (_, (ns, digest)) in pulled {
            let Ok(d) = Digest::parse(&digest) else { continue };
            // (Only a blob the content store has whole.)
            let Ok(info) = layers.content.info(&d) else { continue };
            let labels = match st.containerd() { Some(c) => c.labels(&ns, &digest).await.unwrap_or_default(), None => Default::default() };
            proposed.push(json!({ "digest": digest, "size": info.size, "sources": sources(&labels) }));
        }
        if proposed.is_empty() { continue }
        let l = layers.clone();
        let later = tokio::task::spawn_blocking(move || propose(&l, &url, proposed).unwrap_or_else(|e| { eprintln!("superci-layers: proposing to the store: {e}"); vec![] })).await.unwrap_or_default();
        for d in later {
            let Some((n, ns)) = tries.get(&d).cloned() else { continue };
            if n + 1 < AGAIN_MOST { again.push((Instant::now(), n + 1, (ns, d))) }
        }
    }
}

/// Where a blob was pulled from, by its labels (registries and repositories, as the control plane takes them).
pub fn sources<'a>(labels: impl IntoIterator<Item = (&'a String, &'a String)>) -> Vec<Value> {
    let mut out = vec![];
    for (k, v) in labels {
        let Some(registry) = k.strip_prefix(SOURCE) else { continue };
        for repo in v.split(',').filter(|r| !r.is_empty()) { out.push(json!({ "registry": registry, "repository": repo })) }
    }
    out
}

/// One proposal (fifty layers at a time), then the uploads asked for: the layers answered "later".
fn propose(layers: &Layers, url: &str, proposed: Vec<Value>) -> Result<Vec<String>, String> {
    let agent = crate::fetch::agent();
    let mut later = vec![];
    for chunk in proposed.chunks(50) {
        let body = json!({ "layers": chunk }).to_string();
        let res = agent.post(url).header("content-type", "application/json").send(body.as_bytes()).map_err(|e| e.to_string())?;
        if res.status().as_u16() != 200 { return Err(format!("the control plane answered {}", res.status())) }
        let answer: Value = serde_json::from_str(&res.into_body().read_to_string().map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        let said: Vec<String> = answer["layers"].as_object().into_iter().flatten().map(|(d, a)| format!("{d} {}", a["do"].as_str().unwrap_or("?"))).collect();
        eprintln!("superci-layers: proposed to the store: {}", said.join(", "));
        for (digest, a) in answer["layers"].as_object().into_iter().flatten() {
            if a["do"] == "later" { later.push(digest.clone()) }
            if a["do"] != "upload" { continue }
            let began = Instant::now();
            match upload(layers, &agent, digest, a).and_then(|n| uploaded(&agent, answer["uploaded"].as_str().unwrap_or_default(), a["id"].as_str().unwrap_or_default()).map(|said| (n, said))) {
                Ok((n, said)) => eprintln!("superci-layers: {digest} uploaded to the store ({n} bytes in {:.1} s): {said}", began.elapsed().as_secs_f64()),
                Err(e) => eprintln!("superci-layers: {digest} not uploaded to the store: {e}"),
            }
        }
    }
    Ok(later)
}

/// A blob read at no more than `RATE` bytes a second.
pub struct Paced<R> { pub inner: R, pub began: Instant, pub sent: u64, pub rate: u64 }

impl<R: Read> Read for Paced<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let due = Duration::from_secs_f64(self.sent as f64 / self.rate as f64);
        if let Some(wait) = due.checked_sub(self.began.elapsed()) { std::thread::sleep(wait) }
        let most = buf.len().min(1 << 20);
        let n = self.inner.read(&mut buf[..most])?;
        self.sent += n as u64;
        Ok(n)
    }
}

/// The blob put at the link the control plane gave, with the headers it said.
fn upload(layers: &Layers, agent: &ureq::Agent, digest: &str, a: &Value) -> Result<u64, String> {
    let d = Digest::parse(digest).map_err(|e| e.to_string())?;
    let (file, size) = layers.content.open_blob(&d).map_err(|e| e.to_string())?;
    let mut req = agent.put(a["url"].as_str().ok_or("no link")?).header("content-length", size.to_string());
    for (k, v) in a["headers"].as_object().into_iter().flatten() { if let Some(v) = v.as_str() { req = req.header(k.as_str(), v) } }
    let mut body = Paced { inner: file, began: Instant::now(), sent: 0, rate: RATE };
    let res = req.send(ureq::SendBody::from_reader(&mut body)).map_err(|e| e.to_string())?;
    if res.status().as_u16() >= 300 { return Err(format!("the bucket answered {}", res.status())) }
    Ok(size)
}

/// The control plane told the blob is there: what it does with it.
fn uploaded(agent: &ureq::Agent, url: &str, id: &str) -> Result<String, String> {
    let res = agent.post(url).header("content-type", "application/json").send(json!({ "id": id }).to_string().as_bytes()).map_err(|e| e.to_string())?;
    let status = res.status().as_u16();
    let text = res.into_body().read_to_string().unwrap_or_default();
    if status != 200 { return Err(format!("the control plane answered {status}: {text}")) }
    Ok(serde_json::from_str::<Value>(&text).ok().and_then(|v| v["do"].as_str().map(str::to_string)).unwrap_or(text))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn where_a_blob_came_from_is_read_from_its_labels() {
        let labels: std::collections::BTreeMap<String, String> = [
            ("containerd.io/distribution.source.docker.io".to_string(), "library/postgres,acme/pg".to_string()),
            ("containerd.io/distribution.source.ghcr.io".to_string(), "acme/app".to_string()),
            ("containerd.io/uncompressed".to_string(), "sha256:aa".to_string()),
        ].into_iter().collect();
        assert_eq!(sources(&labels), [json!({ "registry": "docker.io", "repository": "library/postgres" }), json!({ "registry": "docker.io", "repository": "acme/pg" }), json!({ "registry": "ghcr.io", "repository": "acme/app" })]);
    }

    #[test]
    fn an_upload_keeps_to_its_rate() {
        let data = vec![7u8; 3 << 20];
        let began = Instant::now();
        let mut p = Paced { inner: &data[..], began, sent: 0, rate: 8 << 20 };
        let mut out = vec![];
        p.read_to_end(&mut out).unwrap();
        assert_eq!(out, data);
        assert!(began.elapsed() >= Duration::from_millis(240), "3 MiB at 8 MiB a second: {:?}", began.elapsed());
    }
}

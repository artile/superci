//! The layer store: Docker's image layers kept in each region's cache bucket, each as its blob (the compressed tar a
//! registry serves) and as an EROFS file a machine mounts instead of unpacking it (see the `layers` crate, the
//! program on machines). The control plane fills it and decides who reads what.
//!
//! After its job, a machine offers the layers it pulled that the store lacks, and uploads each it is asked for to the
//! bucket's staging, at a link that takes only a body of the layer's SHA-256 (so the upload proves the repository has
//! it); the job's own time is never touched. A layer a public registry serves to anyone (asked as Docker asks, with
//! no sign-in) may be read by every repository; any other only by the repositories that uploaded it (granted). A
//! layer the store has is never shown to a repository that may not read it, and asking for one answers as for a layer
//! the store lacks.
//!
//! The converter (the AWS control plane's second function) writes what it made beside the files (`layers/done/`),
//! which the sweep reads. Records, in the store's kinds:
//!   `layer:<region>:<digest>`  a layer in a region's bucket (`Layer`)
//!   `lgrant:<ns>:<digest>`     a repository that showed it has a layer (until `cache::MAX_AGE_DAYS` later)
//!   `lconv:<region>:<digest>`  a conversion asked for and not heard of yet (`Converting`)
//!   `lmiss:<region>:<digest>`  a layer not converted (refused, too large), not asked for again for a while
//!   `lwant:<region>:<digest>`  a conversion waiting for room (`Wanted`)
//!   `lup:<id>`                 an upload link given (`Upload`)
//!   `limg:<repo>:<job>`        what a repository's last job of a workflow and name had and pulled (`Images`)
//! A layer goes when unused for the cache's days, or made `cache::MAX_AGE_DAYS` ago (the bucket's own rule removes
//! its files soon after: the next pull proposes it again), and the least recently used go first past a region's
//! `REGION_BYTES`.
use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::io::{get_json, Request, Response, Result};
use crate::plane::ControlPlane;
use crate::{aws, cache};

/// A blob larger than this is not converted (as the converter says).
pub const MOST_BYTES: u64 = 4 << 30;
/// Conversions under way at once in a region at most (more wait in the control plane's queue, `lwant:`, and start at
/// its sweeps, a minute apart, as these end): a stack's images (a hundred layers) all at once, so that its next run has
/// them. (Lambda queues what is past an account's concurrency, and runs it as room comes.)
pub const AT_ONCE: usize = 64;
/// How long a layer waits in the queue at most (a staged blob is kept a day).
pub const WANTED_MS: u64 = 86_400_000;
/// A conversion not heard of within this is taken as lost (the converter runs 15 minutes at most).
pub const CONVERT_MS: u64 = 20 * 60_000;
/// A registry's word that a layer is public, taken for this long; then asked again.
pub const PUBLIC_MS: u64 = 7 * 86_400_000;
/// A layer the converter refused is not asked for again within this.
pub const REFUSED_MS: u64 = 7 * 86_400_000;
/// An upload link lasts this long.
pub const UPLOAD_MS: u64 = 3_600_000;
/// What a region keeps at most, the least recently used going first.
pub const REGION_BYTES: u64 = 200 << 30;
/// Layers a machine proposes in one request at most.
pub const PROPOSED_MOST: usize = 50;
/// Where machines put blobs to be converted (the bucket removes what is left there after a day).
pub const STAGING: &str = "layers/staging/";

/// A registry that said a layer is public: where, and when it said so.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Public { pub registry: String, pub repository: String, pub checked: u64 }

/// A layer in a region's bucket.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Layer {
    pub size: u64,
    pub diff_id: String,
    pub erofs: cache::StoredFile,
    pub blob: cache::StoredFile,
    /// When it was made, and last read (a link signed for it), ms.
    pub made: u64,
    pub used: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub public: Option<Public>,
}

impl Layer {
    fn current(&self, now: u64) -> bool { now.saturating_sub(self.made) < cache::MAX_AGE_DAYS as u64 * 86_400_000 }
    fn is_public(&self, now: u64) -> bool { self.public.as_ref().is_some_and(|p| now.saturating_sub(p.checked) < PUBLIC_MS) }
    pub fn bytes(&self) -> u64 { self.blob.size.unwrap_or(self.size) + self.erofs.size.unwrap_or(0) }
}

/// A conversion asked for: when, its blob's size, and the registry it came from (a layer from staging has none).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Converting { pub at: u64, pub size: u64, #[serde(default)] pub public: Option<Public> }

/// A conversion waiting for room (see `AT_ONCE`): since when, its blob's size, where the converter reads it (a registry,
/// or a blob uploaded to staging), and its registry when public.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Wanted { pub at: u64, pub size: u64, pub source: Value, #[serde(default)] pub public: Option<Public> }

/// What a job had from the store and what it pulled, as its machine said after the job (layers and their blobs'
/// bytes): on the job's record, and the last of a job of a repository's (`images_key`, for its next machine, see `plan`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Images { #[serde(default)] pub served: u64, #[serde(default)] pub served_bytes: u64, #[serde(default)] pub pulled: u64, #[serde(default)] pub pulled_bytes: u64,
    #[serde(default)] pub uploaded: u64, #[serde(default)] pub uploaded_bytes: u64,
    /// Of the pulled, those the store had already (or had on their way): the next job has them too.
    #[serde(default)] pub had: u64, #[serde(default)] pub had_bytes: u64,
    /// Of the pulled and not kept, why: no whole file of it on the machine, the store would not take it, its upload
    /// failed (the first failure's words).
    #[serde(default)] pub missing: u64, #[serde(default)] pub missing_bytes: u64, #[serde(default)] pub refused: u64, #[serde(default)] pub failed: u64,
    #[serde(default, skip_serializing_if = "String::is_empty")] pub note: String,
    /// Why the store was not switched in on its machine (nothing came from it), when its setup said.
    #[serde(default, skip_serializing_if = "String::is_empty")] pub off: String,
    /// Every layer the job's images had (kept with the repository's record, not the job's): fetched ahead at its next
    /// machine's start.
    #[serde(default, skip_serializing_if = "Vec::is_empty")] pub used: Vec<String>,
    #[serde(default)] pub at: u64,
    /// Said as the job ended, before its machine kept what it pulled: what it had from the store and pulled (kept or
    /// not, and why, comes once it has).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")] pub keeping: bool,
    /// Why nothing could be said (containerd out of reach).
    #[serde(default, skip_serializing_if = "Option::is_none")] pub error: Option<String> }

/// The layers' files fetched ahead at a machine's start, at most (what its job's last run used).
pub const AHEAD_BYTES: u64 = 16 << 30;

/// How long a repository's last word counts for its next machine (the cache's days, at most).
pub const PLAN_MS: u64 = 7 * 86_400_000;

/// The images a job uses, at least, for its machine to have them from the store: smaller ones are pulled about as fast
/// (a pull of them is mostly asking the registry), and the store's switch at the machine's start would be waited for.
pub const PLAN_BYTES: u64 = 256 << 20;

/// What a machine of a job gets, from what the job's last run had and pulled (`Images`), or else the last of its
/// repository's jobs that used images (`repo_images_key`: a new job, a new workflow, has the layers its repository's
/// other jobs kept): the store set up at its start, on a disk of its own, only when that run used `PLAN_BYTES` of
/// images; else Docker exactly as it comes (nothing to wait for, nothing to pay for). (Kept in memory instead, a layer
/// that did not fit was pulled as usual after its fetch had begun: slower than no store.)
pub fn plan(last: Option<&Images>, now: u64) -> bool {
    last.filter(|l| now.saturating_sub(l.at) < PLAN_MS).is_some_and(|l| l.served_bytes + l.pulled_bytes >= PLAN_BYTES)
}

/// Where the last word of a job of a repository is kept: by its repository, workflow and name (each job of a workflow,
/// each combination of a matrix, uses images of its own; a lint job's says nothing of the tests').
pub fn images_key(repo: &str, workflow: &str, name: &str) -> String {
    format!("limg:{}:{}", repo.to_ascii_lowercase(), &crate::crypto::sha256_hex(format!("{workflow}\n{name}").as_bytes())[..16])
}

/// Where the last word of a repository's job that used images (`plan`'s bytes) is kept: for a job of it with none of
/// its own. (What its layers are, fetched ahead at a machine's start, is the job's own only: another job's would fill a
/// machine with layers its job may not use.)
pub fn repo_images_key(repo: &str) -> String { format!("limg:{}:repo", repo.to_ascii_lowercase()) }

/// An upload link given to a machine: for which layer, of which repository, to where.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Upload { pub region: String, pub digest: String, pub size: u64, pub ns: String, pub key: String, #[serde(default)] pub public: Option<Public> }

pub fn layer_key(region: &str, digest: &str) -> String { format!("layer:{region}:{digest}") }
fn grant_key(ns: &str, digest: &str) -> String { format!("lgrant:{ns}:{digest}") }
fn converting_key(region: &str, digest: &str) -> String { format!("lconv:{region}:{digest}") }
fn refused_key(region: &str, digest: &str) -> String { format!("lmiss:{region}:{digest}") }
fn wanted_key(region: &str, digest: &str) -> String { format!("lwant:{region}:{digest}") }

/// A blob's digest as registries give it (`sha256:` and 64 hex digits).
pub fn is_digest(d: &str) -> bool { d.len() == 71 && d.starts_with("sha256:") && d[7..].bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) }

/// A layer's files in the bucket: its EROFS file, its blob, and what the converter says of it.
pub fn files(digest: &str) -> (String, String, String) {
    let hex = &digest[7..];
    (format!("layers/{hex}.erofs"), format!("layers/{hex}.blob"), format!("layers/done/{hex}.json"))
}

/// The registry a public layer is fetched from: only the well-known public ones (a machine's word picks one, never
/// any address), by the host Docker reaches it at.
pub fn registry_host(registry: &str) -> Option<&str> {
    let plain = !registry.is_empty() && registry.len() < 100 && registry.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'-');
    match registry {
        "docker.io" | "index.docker.io" | "registry-1.docker.io" => Some("registry-1.docker.io"),
        "ghcr.io" | "public.ecr.aws" | "quay.io" | "mcr.microsoft.com" | "registry.gitlab.com" | "registry.k8s.io" | "gcr.io" => Some(registry),
        r if plain && (r.ends_with(".gcr.io") || r.ends_with("-docker.pkg.dev")) && !r.starts_with('.') && !r.starts_with('-') => Some(registry),
        _ => None,
    }
}

/// `key="value"` pairs of a `WWW-Authenticate: Bearer …` header.
fn bearer_challenge(header: &str) -> Option<Vec<(String, String)>> {
    let rest = header.trim().strip_prefix("Bearer ").or_else(|| header.trim().strip_prefix("bearer "))?;
    let mut out = vec![];
    let mut s = rest;
    while let Some(eq) = s.find('=') {
        let k = s[..eq].trim().trim_start_matches(',').trim().to_ascii_lowercase();
        let v = s[eq + 1..].trim_start();
        let (value, after) = match v.strip_prefix('"') { Some(q) => { let end = q.find('"')?; (&q[..end], &q[end + 1..]) } None => { let end = v.find(',').unwrap_or(v.len()); (&v[..end], &v[end..]) } };
        out.push((k, value.to_string()));
        s = after;
    }
    Some(out)
}

/// Whether a registry serves a layer to anyone: asked as Docker asks with no sign-in (a token for pulling, then the
/// blob's first byte). The blob's address and the headers it was asked with when it does; a refusal, a limit or a
/// fault all mean "not public".
pub async fn public_source(http: &dyn crate::io::Http, registry: &str, repository: &str, digest: &str) -> Option<Value> {
    let host = registry_host(registry)?;
    if !is_digest(digest) || repository.is_empty() || repository.len() > 255 || repository.starts_with('/') || repository.contains("..")
        || !repository.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-/".contains(&b)) { return None }
    let url = format!("https://{host}/v2/{repository}/blobs/{digest}");
    // (Its first byte, its redirect not followed: some registries answer no HEAD for a blob, ECR Public among them.)
    let head = |auth: Option<String>| {
        let mut r = Request::new("GET", &url).with_header("range", "bytes=0-0");
        r.no_follow = true;
        if let Some(a) = auth { r.headers.push(("authorization".into(), a)) }
        r
    };
    let found = |status: u16| status == 200 || status == 206 || (300..400).contains(&status);
    let first = http.send(head(None)).await.ok()?;
    if found(first.status) { return Some(json!({ "url": url, "headers": {} })) }
    if first.status != 401 { return None }
    let challenge = bearer_challenge(first.header("www-authenticate")?)?;
    let realm = challenge.iter().find(|(k, _)| k == "realm").map(|(_, v)| v.clone())?;
    if !realm.starts_with("https://") { return None }
    let mut token_url = url::Url::parse(&realm).ok()?;
    {
        let mut q = token_url.query_pairs_mut();
        if let Some((_, service)) = challenge.iter().find(|(k, _)| k == "service") { q.append_pair("service", service); }
        q.append_pair("scope", &format!("repository:{repository}:pull"));
    }
    let t = http.send(Request::new("GET", token_url.as_str())).await.ok()?;
    if t.status != 200 { return None }
    let body: Value = serde_json::from_slice(&t.body).ok()?;
    let token = body["token"].as_str().or(body["access_token"].as_str()).filter(|t| !t.is_empty())?.to_string();
    let second = http.send(head(Some(format!("Bearer {token}")))).await.ok()?;
    found(second.status).then(|| json!({ "url": url, "headers": { "authorization": format!("Bearer {token}") } }))
}

/// One proposed layer, as a machine says it.
#[derive(Debug, Clone, Deserialize)]
struct Proposed { digest: String, size: u64, #[serde(default)] sources: Vec<Source> }

#[derive(Debug, Clone, Deserialize)]
struct Source { registry: String, repository: String }

impl ControlPlane<'_> {
    /// The repository of a machine's job (none yet, or one SuperCI keeps no cache for: public layers only), and the
    /// layers it was granted.
    async fn layer_reader(&self, machine: &cache::Machine) -> Result<(Option<String>, HashSet<String>)> {
        let Ok(access) = self.machine_access(machine).await? else { return Ok((None, HashSet::new())) };
        let prefix = format!("lgrant:{}:", access.ns);
        let granted = self.store.list(&prefix).await?.into_iter().map(|(k, _)| k[prefix.len()..].to_string()).collect();
        Ok((Some(access.ns), granted))
    }

    /// A machine's index: the layers in its region's bucket its repository may read, without links (asked for when a
    /// layer is used, `layers-links`).
    /// Each with its links signed (`creds`: signing is a computation, nothing asked of AWS): the machine asks for
    /// none while Docker pulls (asked for one by one, a burst of them waited seconds on control planes starting).
    pub(crate) async fn layers_index(&self, machine: &cache::Machine, link: &str, creds: Option<&aws::Credentials>, now: u64) -> Result<Response> {
        let (_, granted) = self.layer_reader(machine).await?;
        let bucket = cache::bucket(&self.config.plane_id, &machine.region);
        let sign = |key: &str| -> Result<String> { match creds { Some(c) => aws::presign("GET", &aws::s3_url(&bucket, &machine.region, key), c, &machine.region, cache::LINK_SECONDS, now), None => Ok(String::new()) } };
        // What its job's last run used: fetched ahead as the machine's store starts (`AHEAD_BYTES` at most).
        let used: HashSet<String> = match self.job_of_machine(machine).await? {
            Some(j) if !j.repo.is_empty() => get_json::<Images>(self.store, &images_key(&j.repo, &j.workflow, &j.name)).await?.map(|i| i.used.into_iter().collect()).unwrap_or_default(),
            _ => HashSet::new(),
        };
        let mut ahead_bytes = 0u64;
        let mut layers = serde_json::Map::new();
        for (k, v) in self.store.list(&format!("layer:{}:", machine.region)).await? {
            let Ok(l) = serde_json::from_str::<Layer>(&v) else { continue };
            let digest = k.rsplit_once(":sha256:").map(|(_, h)| format!("sha256:{h}")).unwrap_or_default();
            if !l.current(now) || !(l.is_public(now) || granted.contains(&digest)) { continue }
            let mut erofs = json!({ "url": sign(&l.erofs.key)?, "size": l.erofs.size.unwrap_or(0) });
            if let Some(d) = &l.erofs.digest { erofs["digest"] = d.clone().into() }
            let ahead = used.contains(&digest) && ahead_bytes + l.erofs.size.unwrap_or(0) <= AHEAD_BYTES;
            if ahead { ahead_bytes += l.erofs.size.unwrap_or(0) }
            layers.insert(digest, json!({ "size": l.size, "erofs": erofs, "blob": { "url": sign(&l.blob.key)? }, "ahead": ahead }));
        }
        Ok(Response::json(&json!({ "layers": layers, "refresh": { "url": format!("{link}/layers-links"), "token": "" } })))
    }

    /// Links to layers' files (`{"digests": [...]}`), only for those the machine's repository may read; each layer's
    /// use noted (once a day at most).
    pub(crate) async fn layers_links(&self, machine: &cache::Machine, link: &str, creds: &aws::Credentials, body: &[u8], now: u64) -> Result<Response> {
        let wanted: Vec<String> = serde_json::from_slice::<Value>(body).ok().and_then(|b| b["digests"].as_array().map(|d| d.iter().filter_map(|x| x.as_str().filter(|d| is_digest(d)).map(str::to_string)).take(500).collect())).unwrap_or_default();
        let (_, granted) = self.layer_reader(machine).await?;
        let keys: Vec<String> = wanted.iter().map(|d| layer_key(&machine.region, d)).collect();
        let bucket = cache::bucket(&self.config.plane_id, &machine.region);
        let sign = |key: &str| aws::presign("GET", &aws::s3_url(&bucket, &machine.region, key), creds, &machine.region, cache::LINK_SECONDS, now);
        let mut layers = serde_json::Map::new();
        for ((d, key), got) in wanted.iter().zip(&keys).zip(self.store.get_many(&keys).await?) {
            let Some(mut l) = got.and_then(|v| serde_json::from_str::<Layer>(&v).ok()) else { continue };
            if !l.current(now) || !(l.is_public(now) || granted.contains(d)) { continue }
            let mut erofs = json!({ "url": sign(&l.erofs.key)?, "size": l.erofs.size.unwrap_or(0) });
            if let Some(digest) = &l.erofs.digest { erofs["digest"] = digest.clone().into() }
            layers.insert(d.clone(), json!({ "size": l.size, "erofs": erofs, "blob": { "url": sign(&l.blob.key)? } }));
            if now.saturating_sub(l.used) > 86_400_000 { l.used = now; self.store.put(key, crate::io::json(&l)).await? }
        }
        Ok(Response::json(&json!({ "layers": layers, "refresh": { "url": format!("{link}/layers-links"), "token": "" } })))
    }

    /// The converter given a layer (see `convert` in the AWS control plane): only a control plane on AWS has one.
    async fn convert(&self, region: &str, digest: &str, size: u64, source: Value, now: u64) -> Result<()> {
        let (Some((_, home)), Some(creds)) = (&self.config.aws_own, &self.config.aws_own_creds) else { return Err("only a control plane on AWS converts layers".into()) };
        let (erofs_key, blob_key, done_key) = files(digest);
        let job = json!({ "digest": digest, "size": size, "region": region, "bucket": cache::bucket(&self.config.plane_id, region), "source": source, "erofs_key": erofs_key, "blob_key": blob_key, "done_key": done_key });
        aws::invoke_later(self.http, home, &format!("superci-plane-{}-convert", self.config.plane_id), &job, creds, now).await
    }

    /// A layer converted now when the region has room (its record of being converted made), else queued; `converting`
    /// counts the region's conversions under way. Whether it is converting (else queued).
    async fn convert_or_queue(&self, region: &str, digest: &str, w: Wanted, converting: &mut usize, now: u64) -> Result<bool> {
        if *converting < AT_ONCE {
            // (Its record first: a converter given a layer with no record of it here would be heard of by none.)
            let c = Converting { at: now, size: w.size, public: w.public.clone() };
            self.store.put_until(&converting_key(region, digest), crate::io::json(&c), now + CONVERT_MS * 2).await?;
            // (A converter that refuses the call: queued, as when it has enough at once.)
            match self.convert(region, digest, w.size, w.source.clone(), now).await {
                Ok(()) => { *converting += 1; return Ok(true) }
                Err(e) => { let _ = self.store.delete(&converting_key(region, digest)).await; let _ = self.store.put("layers-convert-error", e).await; }
            }
        }
        self.store.put_until(&wanted_key(region, digest), crate::io::json(&w), now + WANTED_MS).await?;
        Ok(false)
    }

    /// An upload link for a layer (to staging, taking only a body of its SHA-256), noted for `layers-uploaded`.
    async fn upload_link(&self, machine: &cache::Machine, ns: &str, digest: &str, size: u64, public: Option<Public>, creds: &aws::Credentials, now: u64) -> Result<Value> {
        let id = crate::crypto::random_token(18);
        let key = format!("{STAGING}{}-{id}", &digest[7..]);
        let sum = crate::crypto::b64(&(0..32).map(|i| u8::from_str_radix(&digest[7 + 2 * i..9 + 2 * i], 16).unwrap_or(0)).collect::<Vec<u8>>());
        let url = aws::presign_with("PUT", &aws::s3_url(&cache::bucket(&self.config.plane_id, &machine.region), &machine.region, &key), creds, &machine.region, UPLOAD_MS / 1000, now, &[("x-amz-checksum-sha256", &sum)])?;
        let up = Upload { region: machine.region.clone(), digest: digest.to_string(), size, ns: ns.to_string(), key, public };
        self.store.put_until(&format!("lup:{id}"), crate::io::json(&up), now + UPLOAD_MS).await?;
        Ok(json!({ "do": "upload", "url": url, "headers": { "x-amz-checksum-sha256": sum }, "id": id }))
    }

    /// Layers a machine pulled that the store lacks, offered after its job (`{"layers": [{"digest", "size", "sources":
    /// [{"registry", "repository"}]}]}`), each answered: `have` (its repository may read it already, or it is public and
    /// on its way), `upload` (a link to put its blob at, then `layers-uploaded`) or `no`. Every layer comes from the
    /// machine that pulled it; whether its registry gives it to anyone decides only who may read it (every repository,
    /// or those that uploaded it).
    pub(crate) async fn layers_propose(&self, machine: &cache::Machine, link: &str, creds: &aws::Credentials, body: &[u8], now: u64) -> Result<Response> {
        let proposed: Vec<Proposed> = serde_json::from_slice::<Value>(body).ok().and_then(|b| serde_json::from_value(b["layers"].clone()).ok()).unwrap_or_default();
        let (ns, granted) = self.layer_reader(machine).await?;
        // (Only a control plane on AWS has a converter: elsewhere the store keeps what it has, and takes nothing new.
        // On AWS, its credentials are there: `creds` are those.)
        let converts = self.config.aws_own.is_some();
        let proposed: Vec<Proposed> = proposed.into_iter().filter(|p| is_digest(&p.digest)).take(PROPOSED_MOST).collect();
        let keys: Vec<String> = proposed.iter().map(|p| layer_key(&machine.region, &p.digest)).collect();
        let records = self.store.get_many(&keys).await?;
        let mut answers = serde_json::Map::new();
        for (p, record) in proposed.iter().zip(records) {
            let layer = record.and_then(|v| serde_json::from_str::<Layer>(&v).ok()).filter(|l| l.current(now));
            let answer = if p.size == 0 || p.size > MOST_BYTES || !converts || ns.is_none() {
                json!({ "do": "no" })
            } else if let Some(l) = layer {
                if l.is_public(now) || granted.contains(&p.digest) { json!({ "do": "have" }) }
                else { self.upload_link(machine, ns.as_deref().unwrap_or_default(), &p.digest, p.size, None, creds, now).await? }
            } else if self.store.get(&refused_key(&machine.region, &p.digest)).await?.is_some() {
                json!({ "do": "no" })
            } else {
                // On its way already (converting or queued): public, nothing more to do; else this repository's own
                // upload grants it.
                let under_way = match self.store.get(&converting_key(&machine.region, &p.digest)).await? {
                    Some(v) => serde_json::from_str::<Converting>(&v).ok().map(|c| c.public.is_some()),
                    None => self.store.get(&wanted_key(&machine.region, &p.digest)).await?.and_then(|v| serde_json::from_str::<Wanted>(&v).ok()).map(|w| w.public.is_some()),
                };
                if under_way == Some(true) { json!({ "do": "have" }) } else {
                    let mut public = None;
                    for s in p.sources.iter().take(4) {
                        if public_source(self.http, &s.registry, &s.repository, &p.digest).await.is_some() { public = Some(Public { registry: s.registry.clone(), repository: s.repository.clone(), checked: now }); break }
                    }
                    self.upload_link(machine, ns.as_deref().unwrap_or_default(), &p.digest, p.size, public, creds, now).await?
                }
            };
            answers.insert(p.digest.clone(), answer);
        }
        Ok(Response::json(&json!({ "layers": answers, "uploaded": format!("{link}/layers-uploaded") })))
    }

    /// A blob put at an upload link (`{"id"}`): its repository granted the layer, and the layer converted from it
    /// when the store lacks it (else the blob goes).
    pub(crate) async fn layers_uploaded(&self, machine: &cache::Machine, creds: &aws::Credentials, body: &[u8], now: u64) -> Result<Response> {
        let id = serde_json::from_slice::<Value>(body).ok().and_then(|b| b["id"].as_str().map(str::to_string)).unwrap_or_default();
        if id.len() < 16 || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') { return Ok(Response::text(400, "say which upload")) }
        let Some(up) = get_json::<Upload>(self.store, &format!("lup:{id}")).await? else { return Ok(Response::text(404, "no such upload")) };
        // (Another repository's link is not one this machine was given.)
        let (ns, _) = self.layer_reader(machine).await?;
        if up.region != machine.region || ns.as_deref() != Some(up.ns.as_str()) { return Ok(Response::text(404, "no such upload")) }
        let url = aws::s3_url(&cache::bucket(&self.config.plane_id, &up.region), &up.region, &up.key);
        // (S3 took it only with that SHA-256, as the link said; asked again all the same.)
        let head = aws::s3(self.http, "HEAD", &url, &up.region, creds, vec![], &[("x-amz-checksum-mode", "ENABLED")], now).await?;
        let sum = crate::crypto::b64(&(0..32).map(|i| u8::from_str_radix(&up.digest[7 + 2 * i..9 + 2 * i], 16).unwrap_or(0)).collect::<Vec<u8>>());
        if head.status >= 500 { return Err(format!("S3 did not say what was uploaded ({})", head.status)) }
        if head.status != 200 { return Ok(Response::text(409, "nothing was uploaded there")) }
        if head.header("content-length").and_then(|l| l.parse::<u64>().ok()) != Some(up.size) || head.header("x-amz-checksum-sha256") != Some(sum.as_str()) {
            let _ = aws::s3(self.http, "DELETE", &url, &up.region, creds, vec![], &[], now).await;
            return Ok(Response::text(409, "not that layer"))
        }
        self.store.put_until(&grant_key(&up.ns, &up.digest), now.to_string(), now + cache::MAX_AGE_DAYS as u64 * 86_400_000).await?;
        let has = get_json::<Layer>(self.store, &layer_key(&up.region, &up.digest)).await?.is_some_and(|l| l.current(now));
        let under_way = self.store.get(&converting_key(&up.region, &up.digest)).await?.is_some() || self.store.get(&wanted_key(&up.region, &up.digest)).await?.is_some();
        if has || under_way {
            let _ = self.store.delete(&format!("lup:{id}")).await;
            let _ = aws::s3(self.http, "DELETE", &url, &up.region, creds, vec![], &[], now).await;
            return Ok(Response::json(&json!({ "do": "granted" })))
        }
        // Converted now, or once there is room (the staged blob stays a day).
        let mut converting = self.store.list(&format!("lconv:{}:", up.region)).await?.len();
        self.convert_or_queue(&up.region, &up.digest, Wanted { at: now, size: up.size, source: json!({ "key": up.key }), public: up.public }, &mut converting, now).await?;
        // (Its record ends by itself too: one not taken out now is not asked again, the layer under way.)
        let _ = self.store.delete(&format!("lup:{id}")).await;
        Ok(Response::json(&json!({ "do": "converting" })))
    }

    /// What a machine's job had from the store and pulled, said after the job (`Images`): on the job's record, and
    /// the repository's for its next machine.
    pub(crate) async fn layers_kept(&self, machine: &cache::Machine, body: &[u8], now: u64) -> Result<Response> {
        let Ok(mut said) = serde_json::from_slice::<Images>(body) else { return Ok(Response::text(400, "not what was kept")) };
        said.at = now;
        // Said as the job ended, before keeping: on the job's record, for the run's summary, unless what was kept has
        // been said already (the rest is said with that).
        if said.keeping {
            let Some(mut job) = self.job_of_machine(machine).await? else { return Ok(Response::text(404, "no job of this machine")) };
            if job.images.as_ref().is_some_and(|i| !i.keeping) { return Ok(Response::json(&json!({}))) }
            job.images = Some(Images { used: vec![], ..said });
            self.keep_job(&job).await?;
            return Ok(Response::json(&json!({})))
        }
        // The layers it had from the store: used (once a day at most), so that they stay while jobs use them (their
        // links came with the index: nothing else says so).
        let served: Vec<String> = serde_json::from_slice::<Value>(body).ok().and_then(|b| b["served_digests"].as_array().map(|d| d.iter().filter_map(|x| x.as_str().filter(|d| is_digest(d)).map(str::to_string)).take(500).collect())).unwrap_or_default();
        let keys: Vec<String> = served.iter().map(|d| layer_key(&machine.region, d)).collect();
        for (key, got) in keys.iter().zip(self.store.get_many(&keys).await?) {
            let Some(mut l) = got.and_then(|v| serde_json::from_str::<Layer>(&v).ok()) else { continue };
            if now.saturating_sub(l.used) > 86_400_000 { l.used = now; self.store.put(key, crate::io::json(&l)).await? }
        }
        let Some(mut job) = self.job_of_machine(machine).await? else { return Ok(Response::text(404, "no job of this machine")) };
        said.used.retain(|d| is_digest(d));
        said.used.truncate(500);
        if !job.repo.is_empty() {
            self.store.put_until(&images_key(&job.repo, &job.workflow, &job.name), crate::io::json(&said), now + PLAN_MS).await?;
            if plan(Some(&said), now) { self.store.put_until(&repo_images_key(&job.repo), crate::io::json(&Images { used: vec![], ..said.clone() }), now + PLAN_MS).await?; }
        }
        job.images = Some(Images { used: vec![], ..said });
        self.keep_job(&job).await?;
        Ok(Response::json(&json!({})))
    }

    /// At each sweep while conversions are under way: what the converter made, recorded (or what it refused, not asked
    /// for again for a while); one not heard of in time, given up. Once an hour, the store's upkeep (see the module).
    pub(crate) async fn sweep_layers(&self, plane_url: &str, now: u64) -> Result<()> {
        let converting = self.store.list("lconv:").await?;
        let wanted = self.store.list("lwant:").await?;
        let hour = self.store.get("layers-swept").await?.and_then(|v| v.parse::<u64>().ok()).is_none_or(|at| now.saturating_sub(at) >= 3_600_000);
        if converting.is_empty() && wanted.is_empty() && !hour { return Ok(()) }
        let Some(a) = self.aws_connection().await?.filter(|a| a.connected) else { return Ok(()) };
        let creds = self.aws_creds(plane_url, &a).await?;
        for (k, v) in converting {
            let Some((region, digest)) = k.strip_prefix("lconv:").and_then(|r| r.split_once(':')) else { continue };
            let Ok(c) = serde_json::from_str::<Converting>(&v) else { self.store.delete(&k).await?; continue };
            let (_, _, done_key) = files(digest);
            let url = aws::s3_url(&cache::bucket(&self.config.plane_id, region), region, &done_key);
            let got = aws::s3(self.http, "GET", &url, region, &creds, vec![], &[], now).await?;
            if got.status == 404 {
                if now.saturating_sub(c.at) > CONVERT_MS { self.store.delete(&k).await?; self.store.put_until(&refused_key(region, digest), "not heard of in time".into(), now + 3_600_000).await? }
                continue
            }
            if got.status != 200 { continue }
            let done: Value = serde_json::from_slice(&got.body).unwrap_or_default();
            match done["error"].as_str() {
                Some(e) => self.store.put_until(&refused_key(region, digest), e.chars().take(300).collect(), now + REFUSED_MS).await?,
                None => {
                    let file = |f: &Value| cache::StoredFile { key: f["key"].as_str().unwrap_or_default().to_string(), size: f["size"].as_u64(), digest: f["digest"].as_str().map(str::to_string) };
                    let l = Layer { size: c.size, diff_id: done["diff_id"].as_str().unwrap_or_default().to_string(), erofs: file(&done["erofs"]), blob: file(&done["blob"]), made: now, used: now, public: c.public.clone() };
                    if done["digest"] == digest && !l.erofs.key.is_empty() && !l.blob.key.is_empty() {
                        self.store.put(&layer_key(region, digest), crate::io::json(&l)).await?;
                    }
                }
            }
            self.store.delete(&k).await?;
            let _ = aws::s3(self.http, "DELETE", &url, region, &creds, vec![], &[], now).await;
        }
        // The queue, oldest first, as far as each region has room.
        let mut queued: Vec<(String, String, Wanted)> = wanted.into_iter().filter_map(|(k, v)| {
            let (region, digest) = k["lwant:".len()..].split_once(':')?;
            Some((region.to_string(), digest.to_string(), serde_json::from_str::<Wanted>(&v).ok()?))
        }).collect();
        queued.sort_by_key(|(_, _, w)| w.at);
        let mut room: std::collections::HashMap<String, usize> = Default::default();
        for (region, digest, w) in queued {
            let converting = match room.get_mut(&region) { Some(n) => n, None => { let n = self.store.list(&format!("lconv:{region}:")).await?.len(); room.entry(region.clone()).or_insert(n) } };
            if *converting >= AT_ONCE { continue }
            if !self.convert_or_queue(&region, &digest, w, converting, now).await? { break }
            self.store.delete(&wanted_key(&region, &digest)).await?;
        }
        if hour { self.store.put("layers-swept", now.to_string()).await?; self.layers_upkeep(&creds, now).await?; }
        Ok(())
    }

    /// Layers that go (made too long ago, unused for the cache's days, past a region's room) taken out, their files
    /// deleted; registries asked again of public layers whose word is a week old (a few each time).
    async fn layers_upkeep(&self, creds: &aws::Credentials, now: u64) -> Result<()> {
        let unused = self.config.routing.cache.unused_ms();
        let mut by_region: std::collections::BTreeMap<String, Vec<(String, Layer)>> = Default::default();
        for (k, v) in self.store.list("layer:").await? {
            let region = k["layer:".len()..].split(':').next().unwrap_or_default();
            if let Ok(l) = serde_json::from_str::<Layer>(&v) { by_region.entry(region.to_string()).or_default().push((k.clone(), l)) }
        }
        let mut asked = 0;
        for (region, mut layers) in by_region {
            layers.sort_by_key(|(_, l)| std::cmp::Reverse(l.used));
            let (mut kept, mut layers) = (0u64, layers.into_iter());
            loop {
                let Some((k, mut l)) = layers.next() else { break };
                let goes = !l.current(now) || now.saturating_sub(l.used) > unused || kept + l.bytes() > REGION_BYTES;
                if goes {
                    let delete = async |key: &str| { let _ = aws::s3(self.http, "DELETE", &aws::s3_url(&cache::bucket(&self.config.plane_id, &region), &region, key), &region, creds, vec![], &[], now).await; };
                    delete(&l.erofs.key).await;
                    delete(&l.blob.key).await;
                    self.store.delete(&k).await?;
                    continue
                }
                kept += l.bytes();
                let stale = l.public.as_ref().filter(|p| now.saturating_sub(p.checked) >= PUBLIC_MS).cloned();
                if let Some(p) = stale.filter(|_| asked < 20) {
                    asked += 1;
                    let digest = k.rsplit_once(":sha256:").map(|(_, h)| format!("sha256:{h}")).unwrap_or_default();
                    l.public = public_source(self.http, &p.registry, &p.repository, &digest).await.map(|_| Public { checked: now, ..p });
                    self.store.put(&k, crate::io::json(&l)).await?;
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_well_known_public_registries_are_asked() {
        assert_eq!(registry_host("docker.io"), Some("registry-1.docker.io"));
        assert_eq!(registry_host("europe-west1-docker.pkg.dev"), Some("europe-west1-docker.pkg.dev"));
        assert_eq!(registry_host("eu.gcr.io"), Some("eu.gcr.io"));
        for refused in ["169.254.169.254", "localhost:5000", "registry.example.com", "evil.com/x.gcr.io", "", "-docker.pkg.dev"] { assert_eq!(registry_host(refused), None, "{refused}") }
        assert!(is_digest(&format!("sha256:{}", "a".repeat(64))) && !is_digest(&format!("sha256:{}", "A".repeat(64))) && !is_digest("sha256:aa"));
    }

    #[test]
    fn a_registry_s_challenge_is_read_as_docker_reads_it() {
        let c = bearer_challenge(r#"Bearer realm="https://auth.docker.io/token",service="registry.docker.io",scope="repository:library/alpine:pull""#).unwrap();
        assert_eq!(c, [("realm".to_string(), "https://auth.docker.io/token".to_string()), ("service".into(), "registry.docker.io".into()), ("scope".into(), "repository:library/alpine:pull".into())]);
        assert_eq!(bearer_challenge(r#"Bearer realm="https://ghcr.io/token",service=ghcr.io"#).unwrap()[1], ("service".to_string(), "ghcr.io".to_string()));
        assert!(bearer_challenge(r#"Basic realm="x""#).is_none());
        // A challenge with nothing in it, and one whose quote never ends.
        assert_eq!(bearer_challenge("Bearer nothing"), Some(vec![]));
        assert!(bearer_challenge(r#"Bearer realm="https://auth.docker.io/token"#).is_none());
    }

    /// A registry as a test has it answer each request: an answer, or a connection that failed.
    struct Registry(fn(&Request) -> crate::io::Result<Response>);
    #[async_trait::async_trait(?Send)]
    impl crate::io::Http for Registry {
        async fn send(&self, r: Request) -> crate::io::Result<Response> { (self.0)(&r) }
    }

    /// The registry's challenge to a request with no token, naming where tokens are had (`realm`).
    fn challenge(realm: &str) -> crate::io::Result<Response> {
        Ok(Response::new(401, "application/json", "").with_header("www-authenticate", &format!(r#"Bearer realm="{realm}",service="ghcr.io""#)))
    }

    #[test]
    fn a_layer_is_public_only_when_its_registry_gives_it_to_anyone_and_any_failure_on_the_way_says_not() {
        let digest = format!("sha256:{}", "a".repeat(64));
        let ask = |answers: fn(&Request) -> crate::io::Result<Response>| futures::executor::block_on(public_source(&Registry(answers), "ghcr.io", "acme/app", &digest));
        // Given at once; the registry not reached; a realm that is no address; no token, or one that is no JSON; the
        // blob not reached with the token.
        assert!(ask(|_| Ok(Response::new(206, "application/octet-stream", "x"))).is_some());
        assert!(ask(|_| Err("connection refused".into())).is_none());
        assert!(ask(|_| challenge("https://exa mple/token")).is_none());
        assert!(ask(|r| if r.url.contains("/v2/") { challenge("https://ghcr.io/token") } else { Err("connection refused".into()) }).is_none());
        assert!(ask(|r| if r.url.contains("/v2/") { challenge("https://ghcr.io/token") } else { Ok(Response::new(200, "application/json", "not json")) }).is_none());
        assert!(ask(|r| match (r.url.contains("/v2/"), r.header("authorization").is_some()) {
            (true, false) => challenge("https://ghcr.io/token"),
            (false, _) => Ok(Response::new(200, "application/json", r#"{"token":"t"}"#)),
            (true, true) => Err("connection reset".into()),
        }).is_none());
    }
}

//! GitHub's runner image (ghcr.io/actions/actions-runner, which GitHub builds and publishes) copied into your own
//! Cloudflare registry, since Cloudflare's containers cannot pull from GitHub's registry. Plain registry HTTP: the
//! linux/amd64 image's layers go from GitHub, through this machine, to Cloudflare, unchanged; nothing to install and
//! nothing published by us. Tagged by GitHub's digest, so a copy already made is reused.
use std::io::Read;
use std::time::Duration;

use serde_json::Value;

use crate::cloudflare::Cloudflare;
use crate::Result;

const SOURCE_REPO: &str = "actions/actions-runner";
const SOURCE_TAG: &str = "latest";
const TARGET_REPO: &str = "superci-runner";
const MANIFESTS: &str = "application/vnd.oci.image.index.v1+json, application/vnd.docker.distribution.manifest.list.v2+json, application/vnd.oci.image.manifest.v1+json, application/vnd.docker.distribution.manifest.v2+json";
const CHUNK: usize = 32 * 1024 * 1024;

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder().http_status_as_error(false).timeout_global(Some(Duration::from_secs(600))).build().into()
}

/// A registry's answer: status, headers we need, body.
struct Answer { status: u16, location: Option<String>, content_type: Option<String>, challenge: Option<String>, body: Vec<u8> }

/// One registry, with the authentication it asks for (a bearer token from its realm, or basic).
struct Registry { agent: ureq::Agent, base: String, basic: Option<String>, bearer: Option<String> }

impl Registry {
    fn send(&mut self, method: &str, url: &str, headers: &[(&str, &str)], body: Option<&[u8]>) -> Result<Answer> {
        // (Asked again once, after the token a 401 asks for; a second 401 is its answer.)
        let mut authenticated = false;
        loop {
            let url = if url.starts_with("http") { url.to_string() } else { format!("{}{url}", self.base) };
            let mut b = ureq::http::Request::builder().method(method).uri(crate::aws::stand_in(&url).as_str());
            for (k, v) in headers { b = b.header(*k, *v) }
            if let Some(t) = &self.bearer { b = b.header("authorization", format!("Bearer {t}")) } else if let Some(basic) = &self.basic { b = b.header("authorization", format!("Basic {basic}")) }
            let req = b.body(body.map(|b| b.to_vec()).unwrap_or_default()).map_err(|e| e.to_string())?;
            let mut r = self.agent.run(req).map_err(|e| format!("{method} {url}: {e}"))?;
            let h = |n: &str| r.headers().get(n).and_then(|v| v.to_str().ok()).map(str::to_string);
            let answer = Answer { status: r.status().as_u16(), location: h("location"), content_type: h("content-type"), challenge: h("www-authenticate"), body: vec![] };
            if answer.status == 401 && !authenticated {
                self.authenticate(answer.challenge.as_deref().unwrap_or_default())?;
                authenticated = true;
                continue;
            }
            let body = if method == "HEAD" { vec![] } else { r.body_mut().with_config().limit(64 * 1024 * 1024).read_to_vec().map_err(|e| e.to_string())? };
            // (A recording keeps no layer: a layer is many MB.)
            #[cfg(debug_assertions)]
            {
                let said: Vec<(String, String)> = [("location", &answer.location), ("content-type", &answer.content_type), ("www-authenticate", &answer.challenge)].into_iter().filter_map(|(k, v)| Some((k.to_string(), v.clone()?))).collect();
                crate::aws::record(&superci_core::io::Request::new(method, &url), answer.status, &said, if body.len() > 1_000_000 { b"(a layer)" } else { &body });
            }
            return Ok(Answer { body, ..answer });
        }
    }

    /// The registry's challenge: `Bearer realm="…",service="…",scope="…"` → a token from the realm.
    fn authenticate(&mut self, challenge: &str) -> Result<()> {
        let Some(params) = challenge.strip_prefix("Bearer ") else { return Ok(()) };
        let field = |k: &str| params.split(',').find_map(|p| p.trim().strip_prefix(&format!("{k}=")).map(|v| v.trim_matches('"').to_string()));
        let realm = field("realm").ok_or("registry challenge without a realm")?;
        let mut url = url::Url::parse(&realm).map_err(|e| e.to_string())?;
        if let Some(s) = field("service") { url.query_pairs_mut().append_pair("service", &s); }
        if let Some(s) = field("scope") { url.query_pairs_mut().append_pair("scope", &s); }
        let mut req = self.agent.get(crate::aws::stand_in(url.as_str()).as_str());
        if let Some(basic) = &self.basic { req = req.header("authorization", &format!("Basic {basic}")) }
        let mut r = req.call().map_err(|e| e.to_string())?;
        let text = r.body_mut().read_to_string().map_err(|e| e.to_string())?;
        #[cfg(debug_assertions)]
        crate::aws::record(&superci_core::io::Request::new("GET", url.as_str()), r.status().as_u16(), &[], text.as_bytes());
        let v: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
        self.bearer = Some(v["token"].as_str().or(v["access_token"].as_str()).ok_or("registry gave no token")?.to_string());
        Ok(())
    }
}

/// Copies GitHub's runner image into the account's Cloudflare registry (once per version) and returns its reference,
/// pinned by digest (`registry.cloudflare.com/<account>/<repo>@sha256:…`, as per-container scheduling requires).
pub fn copy_runner_image(cf: &Cloudflare, account_id: &str) -> Result<String> {
    let mut github = Registry { agent: agent(), base: "https://ghcr.io".into(), basic: None, bearer: None };
    let index = github.send("GET", &format!("/v2/{SOURCE_REPO}/manifests/{SOURCE_TAG}"), &[("accept", MANIFESTS)], None)?;
    if index.status != 200 { return Err(format!("GitHub's registry: {} for the runner image", index.status)) }
    let index_json: Value = serde_json::from_slice(&index.body).map_err(|e| e.to_string())?;
    // A multi-platform index: Cloudflare's containers run linux/amd64.
    let (manifest, media_type, digest) = match index_json["manifests"].as_array() {
        Some(list) => {
            let d = list.iter().find(|m| m["platform"]["os"] == "linux" && m["platform"]["architecture"] == "amd64").and_then(|m| m["digest"].as_str()).ok_or("no linux/amd64 runner image")?.to_string();
            let m = github.send("GET", &format!("/v2/{SOURCE_REPO}/manifests/{d}"), &[("accept", MANIFESTS)], None)?;
            (m.body, m.content_type.unwrap_or_default(), d)
        }
        None => (index.body.clone(), index.content_type.unwrap_or_default(), String::new()),
    };
    let tag = format!("gh-{}", digest.trim_start_matches("sha256:").get(..12).unwrap_or("latest"));
    use sha2::Digest;
    let pinned = format!("sha256:{}", sha2::Sha256::digest(&manifest).iter().map(|b| format!("{b:02x}")).collect::<String>());
    let image = format!("registry.cloudflare.com/{account_id}/{TARGET_REPO}@{pinned}");

    let (user, password) = cf.registry_push_credentials(account_id)?;
    use base64::Engine;
    let basic = base64::engine::general_purpose::STANDARD.encode(format!("{user}:{password}"));
    let mut cloudflare = Registry { agent: agent(), base: "https://registry.cloudflare.com".into(), basic: Some(basic), bearer: None };
    let repo = format!("{account_id}/{TARGET_REPO}");
    if cloudflare.send("HEAD", &format!("/v2/{repo}/manifests/{tag}"), &[("accept", MANIFESTS)], None)?.status == 200 { return Ok(image) }

    let m: Value = serde_json::from_slice(&manifest).map_err(|e| e.to_string())?;
    let blobs: Vec<(String, u64)> = std::iter::once(&m["config"]).chain(m["layers"].as_array().into_iter().flatten())
        .filter_map(|b| Some((b["digest"].as_str()?.to_string(), b["size"].as_u64().unwrap_or(0)))).collect();
    let total: u64 = blobs.iter().map(|b| b.1).sum();
    eprintln!("Copying GitHub's runner image into your Cloudflare registry ({} MB, once)…", total / 1_000_000);
    for (digest, size) in &blobs {
        let head = cloudflare.send("HEAD", &format!("/v2/{repo}/blobs/{digest}"), &[], None)?.status;
        eprintln!("  {digest} ({} MB): {}", size / 1_000_000, if head == 200 { "already there" } else { "copying" });
        if head == 200 { continue }
        copy_blob(&mut github, &mut cloudflare, &repo, digest, *size)?;
    }
    let put = cloudflare.send("PUT", &format!("/v2/{repo}/manifests/{tag}"), &[("content-type", &media_type)], Some(&manifest))?;
    if put.status >= 300 { return Err(format!("Cloudflare's registry refused the image manifest: {} {}", put.status, String::from_utf8_lossy(&put.body))) }
    eprintln!("Runner image ready: {image}");
    Ok(image)
}

/// One layer, streamed from GitHub and uploaded to Cloudflare in chunks.
fn copy_blob(github: &mut Registry, cloudflare: &mut Registry, repo: &str, digest: &str, size: u64) -> Result<()> {
    // GitHub answers with a redirect to its storage; the download itself needs no token.
    let mut source = {
        let bearer = github.bearer.clone().unwrap_or_default();
        github.agent.get(crate::aws::stand_in(&format!("https://ghcr.io/v2/{SOURCE_REPO}/blobs/{digest}")).as_str()).header("authorization", &format!("Bearer {bearer}")).call().map_err(|e| e.to_string())?
    };
    if source.status().as_u16() != 200 { return Err(format!("GitHub's registry: {} for layer {digest}", source.status())) }
    let mut reader = source.body_mut().with_config().limit(size + 1).reader();
    let start = cloudflare.send("POST", &format!("/v2/{repo}/blobs/uploads/"), &[("content-length", "0")], Some(&[]))?;
    let mut location = start.location.ok_or("Cloudflare's registry gave no upload location")?;
    let mut offset: u64 = 0;
    let mut chunk = Vec::with_capacity(CHUNK);
    loop {
        chunk.clear();
        (&mut reader).take(CHUNK as u64).read_to_end(&mut chunk).map_err(|e| e.to_string())?;
        let filled = chunk.len();
        if filled == 0 { break }
        let range = format!("{}-{}", offset, offset + filled as u64 - 1);
        let r = cloudflare.send("PATCH", &location, &[("content-type", "application/octet-stream"), ("content-range", &range)], Some(&chunk))?;
        if r.status >= 300 { return Err(format!("Cloudflare's registry refused a layer chunk: {} {}", r.status, String::from_utf8_lossy(&r.body))) }
        location = r.location.unwrap_or(location);
        offset += filled as u64;
        eprintln!("  {digest}: {} of {} MB", offset / 1_000_000, size / 1_000_000);
    }
    let separator = if location.contains('?') { '&' } else { '?' };
    let done = cloudflare.send("PUT", &format!("{location}{separator}digest={digest}"), &[("content-length", "0")], Some(&[]))?;
    if done.status >= 300 { return Err(format!("Cloudflare's registry refused layer {digest}: {} {}", done.status, String::from_utf8_lossy(&done.body))) }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aws::clouds::{self, answer, cf, cut, with};
    use serde_json::json;

    const INDEX: &str = r#"{"manifests":[{"platform":{"os":"linux","architecture":"arm64"},"digest":"sha256:arm"},{"platform":{"os":"linux","architecture":"amd64"},"digest":"sha256:amd"}]}"#;
    const MANIFEST: &str = r#"{"config":{"digest":"sha256:cfg","size":5},"layers":[]}"#;
    const INDEX_AT: &str = "GET /ghcr.io/v2/actions/actions-runner/manifests/latest";

    /// An answer with these headers too.
    fn answer_with(status: u16, headers: &str, body: &str) -> Vec<u8> {
        format!("HTTP/1.1 {status} X\r\n{headers}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len()).into_bytes()
    }

    /// GitHub's registry and the account's (`acc`) as a copy of the runner image meets them: GitHub's asks for a token
    /// first, the image has one layer, and Cloudflare's registry has none of it. What a request ("METHOD address") starts with in `given` is answered
    /// as given there instead (nothing: its connection closed unanswered).
    fn registries(given: Vec<(&'static str, Vec<u8>)>) -> String {
        let challenged = std::sync::atomic::AtomicBool::new(false);
        clouds::serve(move |method, path, _| {
            let asked = format!("{method} {path}");
            if let Some((_, a)) = given.iter().find(|(p, _)| asked.starts_with(p)) { return a.clone() }
            if asked == INDEX_AT && !challenged.swap(true, std::sync::atomic::Ordering::SeqCst) {
                return answer_with(401, r#"www-authenticate: Bearer realm="https://ghcr.io/token",service="ghcr.io",scope="repository:actions/actions-runner:pull""#, "")
            }
            match (method, path) {
                ("GET", "/ghcr.io/token?service=ghcr.io&scope=repository%3Aactions%2Factions-runner%3Apull") => answer(200, r#"{"token":"t"}"#),
                ("GET", "/ghcr.io/v2/actions/actions-runner/manifests/latest") => answer(200, INDEX),
                ("GET", "/ghcr.io/v2/actions/actions-runner/manifests/sha256:amd") => answer_with(200, "content-type: application/vnd.oci.image.manifest.v1+json", MANIFEST),
                ("GET", "/ghcr.io/v2/actions/actions-runner/blobs/sha256:cfg") => answer(200, "layer"),
                ("POST", p) if p.ends_with("/credentials") => cf(json!({ "username": "u", "password": "p" })),
                ("HEAD", _) => answer(404, ""),
                ("POST", "/registry.cloudflare.com/v2/acc/superci-runner/blobs/uploads/") => answer_with(202, "location: /v2/acc/superci-runner/blobs/uploads/u1", ""),
                ("PATCH", "/registry.cloudflare.com/v2/acc/superci-runner/blobs/uploads/u1") => answer_with(202, "location: /v2/acc/superci-runner/blobs/uploads/u1?part=2", ""),
                ("PUT", "/registry.cloudflare.com/v2/acc/superci-runner/blobs/uploads/u1?part=2&digest=sha256:cfg") => answer(201, ""),
                ("PUT", "/registry.cloudflare.com/v2/acc/superci-runner/manifests/gh-latest") => answer(201, ""),
                _ => answer(500, "the test has no answer for it"),
            }
        })
    }

    fn copy(given: Vec<(&'static str, Vec<u8>)>) -> Result<String> { with(&registries(given), || copy_runner_image(&Cloudflare::new("t"), "acc")) }

    #[test]
    fn the_runner_image_is_copied_layer_by_layer_and_pinned_by_its_manifest() {
        assert!(copy(vec![]).unwrap().starts_with("registry.cloudflare.com/acc/superci-runner@sha256:"));
        // A single image (no index) is the one copied; a manifest naming no layer leaves only itself to copy.
        assert!(copy(vec![(INDEX_AT, answer(200, "{}"))]).is_ok());
    }

    /// Whichever exchange of a copy gets no answer, or an answer cut off, the copy fails saying so.
    #[test]
    fn a_copy_fails_at_whichever_exchange_goes_unanswered_or_is_cut_off() {
        for at in ["HEAD /registry.cloudflare.com/v2/acc/superci-runner/manifests", "HEAD /registry.cloudflare.com/v2/acc/superci-runner/blobs", "GET /ghcr.io/v2/actions/actions-runner/blobs",
            "POST /registry.cloudflare.com/v2/acc/superci-runner/blobs/uploads", "PATCH", "PUT /registry.cloudflare.com/v2/acc/superci-runner/blobs", "PUT /registry.cloudflare.com/v2/acc/superci-runner/manifests"] {
            assert!(copy(vec![(at, vec![])]).is_err(), "{at}");
        }
        assert!(copy(vec![(INDEX_AT, cut(200))]).is_err());
        assert!(copy(vec![("GET /ghcr.io/v2/actions/actions-runner/blobs", cut(200))]).is_err());
        // An upload address that is no address.
        assert!(copy(vec![("POST /registry.cloudflare.com/v2/acc/superci-runner/blobs/uploads", answer_with(202, "location: /v2/x y", ""))]).is_err());
    }

    #[test]
    fn manifests_that_are_not_json_or_have_no_linux_amd64_image_are_not_copied() {
        assert!(copy(vec![(INDEX_AT, answer(200, "not json"))]).is_err());
        assert_eq!(copy(vec![(INDEX_AT, answer(200, r#"{"manifests":[]}"#))]).unwrap_err(), "no linux/amd64 runner image");
        assert!(copy(vec![("GET /ghcr.io/v2/actions/actions-runner/manifests/sha256:amd", answer(200, "not json"))]).is_err());
        // (A layer without its digest is none to copy.)
        assert!(copy(vec![("GET /ghcr.io/v2/actions/actions-runner/manifests/sha256:amd", answer(200, r#"{"config":{"size":1}}"#))]).is_ok());
    }

    /// GitHub's registry asking for a token from a realm that is no address, does not answer, answers cut off, not
    /// JSON or without a token: the copy fails.
    #[test]
    fn a_token_the_registrys_realm_does_not_give_fails_the_copy() {
        let challenge = |realm: &str| answer_with(401, &format!(r#"www-authenticate: Bearer realm="{realm}",service="ghcr.io""#), "");
        let closed = format!("{}/token", clouds::closed());
        // (A token that still leaves GitHub's registry refusing: its refusal is the answer.)
        assert_eq!(copy(vec![(INDEX_AT, challenge("https://ghcr.io/token")), ("GET /ghcr.io/token", answer(200, r#"{"token":"t"}"#))]).unwrap_err(), "GitHub's registry: 401 for the runner image");
        for (realm, token) in [("::no address", answer(200, "{}")), (closed.as_str(), answer(200, "{}")), ("https://ghcr.io/token", cut(200)), ("https://ghcr.io/token", answer(200, "not json")), ("https://ghcr.io/token", answer(200, "{}"))] {
            assert!(copy(vec![(INDEX_AT, challenge(realm)), ("GET /ghcr.io/token", token)]).is_err(), "{realm}");
        }
    }
}

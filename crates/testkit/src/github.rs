//! GitHub as the control plane finds it, for an organization `acme` with its repository `acme/app` and SuperCI's App
//! installed on it. Repositories, runs and jobs are answered in the shape GitHub answered for a public repository
//! (`fixtures/github/{repo,run,job}.json`, recorded with `gh api` and made Acme's); the App's own calls (its
//! installations, their tokens, a runner's registration, webhook deliveries), which only an App can make, as GitHub's
//! documentation gives them. The test plays GitHub's side of a job (`queue`, `start`, `finish`): its state here, and
//! the webhook GitHub sends of each change, signed with the App's secret.
use async_trait::async_trait;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use superci_core::io::{self, Http, Request, Response};

pub const OWNER: &str = "acme";
pub const REPO: &str = "acme/app";
pub const APP_ID: u64 = 1001;
pub const INSTALLATION: u64 = 42;
pub const WEBHOOK_SECRET: &str = "webhook-secret-for-tests";
/// Where GitHub's logs are (a link it gives for a minute, as to its blob store).
const LOGS_AT: &str = "https://productionresultssa0.blob.core.windows.net/actions-results/logs";

#[derive(Default)]
struct Hub {
    jobs: BTreeMap<u64, Value>,
    logs: BTreeMap<u64, String>,
    /// Each run's logs, once it has ended (GitHub's zip: a folder for each job, a file for each step).
    run_logs: BTreeMap<u64, Vec<u8>>,
    /// The runners registered (each machine's, by the control plane): id to name.
    runners: BTreeMap<u64, String>,
    next: u64,
    /// `acme/app` is public (anyone can open a pull request that runs code there).
    public: bool,
    /// How many times GitHub's key set for its jobs' tokens was asked for.
    jwks_asked: u64,
    /// The App was uninstalled.
    uninstalled: bool,
    /// S3's objects, by bucket and key (`bucket/key`), as the control plane's buckets keep them.
    objects: BTreeMap<String, Vec<u8>>,
}

#[derive(Clone, Default)]
pub struct GitHub { hub: Arc<Mutex<Hub>>, pub asked: Arc<Mutex<Vec<String>>>, pub unanswered: Arc<Mutex<Vec<String>>>, /** gitlab.com, beside it. */ pub gitlab: crate::GitLab }

fn fixture(name: &str) -> Value {
    serde_json::from_str(match name {
        "repo" => include_str!("../fixtures/github/repo.json"),
        "run" => include_str!("../fixtures/github/run.json"),
        "org" => include_str!("../fixtures/github/org.json"),
        _ => include_str!("../fixtures/github/job.json"),
    }).unwrap()
}

pub(crate) fn iso(ms: u64) -> String {
    let (secs, days) = (ms / 1000, ms / 86_400_000);
    // (Days since 1970 to a date: Howard Hinnant's civil_from_days.)
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let (d, m) = (doy - (153 * mp + 2) / 5 + 1, if mp < 10 { mp + 3 } else { mp - 9 });
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", secs / 3600 % 24, secs / 60 % 60, secs % 60)
}

impl GitHub {
    /// Makes `acme/app` public, or private again.
    pub fn set_public(&self, public: bool) { self.hub.lock().unwrap().public = public }

    /// An object put in a bucket (`bucket/key`), as the control plane's deploy leaves its program there.
    pub fn put_object(&self, key: &str, bytes: &[u8]) { self.hub.lock().unwrap().objects.insert(key.to_string(), bytes.to_vec()); }

    /// How many times GitHub's key set for its jobs' tokens was asked for.
    pub fn jwks_asked(&self) -> u64 { self.hub.lock().unwrap().jwks_asked }

    /// Whether the App was uninstalled.
    pub fn uninstalled(&self) -> bool { self.hub.lock().unwrap().uninstalled }

    /// The buckets' objects' keys (`bucket/key`) and sizes.
    pub fn objects(&self) -> Vec<(String, usize)> { self.hub.lock().unwrap().objects.iter().map(|(k, v)| (k.clone(), v.len())).collect() }

    fn repo(&self) -> Value {
        let mut r = fixture("repo");
        if self.hub.lock().unwrap().public { r["private"] = json!(false); r["visibility"] = json!("public") }
        r
    }

    /// Cancelled before any runner took it: its webhook.
    pub fn cancel(&self, id: u64, now_ms: u64) -> (Vec<u8>, String) {
        if let Some(j) = self.hub.lock().unwrap().jobs.get_mut(&id) { for (k, v) in [("status", json!("completed")), ("conclusion", json!("cancelled")), ("completed_at", json!(iso(now_ms)))] { j[k] = v } }
        self.event("completed", id)
    }

    /// Ended at GitHub without the webhook arriving (taken by another runner and done, as far as GitHub says).
    pub fn ended_untold(&self, id: u64) {
        if let Some(j) = self.hub.lock().unwrap().jobs.get_mut(&id) { for (k, v) in [("status", json!("completed")), ("conclusion", json!("success")), ("runner_name", json!("someone-else")), ("runner_id", json!(7))] { j[k] = v } }
    }

    /// The App as the dashboard keeps it in the control plane's settings (`GITHUB_APP`), with a key made for this
    /// test process (GitHub checks nothing of it here; the control plane signs with it).
    pub fn app() -> Value {
        static KEY: std::sync::OnceLock<String> = std::sync::OnceLock::new();
        let pem = KEY.get_or_init(|| {
            let out = std::process::Command::new("openssl").args(["genrsa", "2048"]).output().expect("openssl, to make the App's key");
            String::from_utf8(out.stdout).unwrap()
        });
        json!({ "id": APP_ID, "slug": "superci-acme", "pem": pem, "webhook_secret": WEBHOOK_SECRET, "owner": OWNER, "owner_is_org": true })
    }

    /// A token GitHub gives a job (`ACTIONS_RUNTIME_TOKEN`), with these claims, signed with the key GitHub's key set
    /// (`token.actions.githubusercontent.com/.well-known/jwks`, here) has.
    pub fn job_token(claims: &Value) -> String {
        let mut c = claims.clone();
        c["iss"] = json!("https://token.actions.githubusercontent.com");
        superci_core::crypto::rs256_jwt(&c, Self::app()["pem"].as_str().unwrap()).unwrap()
    }

    /// GitHub's key set for its jobs' tokens: the public half of the key above.
    fn job_keys() -> Value {
        let mut openssl = std::process::Command::new("openssl").args(["rsa", "-noout", "-modulus"]).stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::null()).spawn().expect("openssl");
        { use std::io::Write; openssl.stdin.take().unwrap().write_all(Self::app()["pem"].as_str().unwrap().as_bytes()).unwrap(); }
        let out = String::from_utf8(openssl.wait_with_output().unwrap().stdout).unwrap();
        let hex = out.trim().trim_start_matches("Modulus=");
        let n: Vec<u8> = (0..hex.len()).step_by(2).map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap()).collect();
        json!({ "keys": [{ "kty": "RSA", "alg": "RS256", "use": "sig", "n": superci_core::crypto::b64url(&n), "e": "AQAB" }] })
    }

    /// GitHub's record of job `id` as it stands.
    pub fn job(&self, id: u64) -> Value { self.hub.lock().unwrap().jobs.get(&id).cloned().unwrap_or_default() }

    /// The webhook GitHub sends of a job's change (`action`), as it sends it.
    fn event(&self, action: &str, id: u64) -> (Vec<u8>, String) {
        let payload = json!({ "action": action, "workflow_job": self.job(id), "repository": self.repo(), "organization": { "login": OWNER, "id": 9_919 },
            "installation": { "id": INSTALLATION, "node_id": "MDIzOkludGVncmF0aW9uSW5zdGFsbGF0aW9uNDI=" }, "sender": { "login": "octocat", "id": 1, "type": "User" } });
        let body = payload.to_string().into_bytes();
        let signature = format!("sha256={}", superci_core::crypto::hex(&superci_core::crypto::hmac_sha256(WEBHOOK_SECRET.as_bytes(), &body)));
        (body, signature)
    }

    /// A job queued in a run of `acme/app`, labelled `label`: its number, and the webhook to send.
    pub fn queue(&self, name: &str, label: &str, now_ms: u64) -> (u64, (Vec<u8>, String)) {
        let id = {
            let mut h = self.hub.lock().unwrap();
            h.next += 1;
            let (id, run) = (90_000_000_000 + h.next, 30_000_000_000 + h.next);
            let mut j = fixture("job");
            for (k, v) in [("id", json!(id)), ("run_id", json!(run)), ("name", json!(name)), ("labels", json!([label])), ("status", json!("queued")), ("conclusion", Value::Null),
                ("created_at", json!(iso(now_ms))), ("started_at", json!(iso(now_ms))), ("completed_at", Value::Null), ("runner_id", Value::Null), ("runner_name", Value::Null),
                ("runner_group_id", Value::Null), ("runner_group_name", Value::Null), ("steps", json!([])), ("workflow_name", json!("Build and Test")), ("run_attempt", json!(1)),
                ("url", json!(format!("https://api.github.com/repos/{REPO}/actions/jobs/{id}"))), ("html_url", json!(format!("https://github.com/{REPO}/actions/runs/{run}/job/{id}"))),
                ("run_url", json!(format!("https://api.github.com/repos/{REPO}/actions/runs/{run}")))] { j[k] = v }
            h.jobs.insert(id, j);
            id
        };
        (id, self.event("queued", id))
    }

    /// Its runner took it: the runner the control plane registered last (the machine started for it).
    pub fn start(&self, id: u64) -> (Vec<u8>, String) {
        {
            let mut h = self.hub.lock().unwrap();
            let (rid, name) = h.runners.iter().next_back().map(|(i, n)| (*i, n.clone())).expect("a runner registered for it");
            let j = h.jobs.get_mut(&id).unwrap();
            for (k, v) in [("status", json!("in_progress")), ("runner_id", json!(rid)), ("runner_name", json!(name)), ("runner_group_id", json!(1)), ("runner_group_name", json!("Default"))] { j[k] = v }
        }
        self.event("in_progress", id)
    }

    /// It ended (`success`, `failure`), with its whole log.
    pub fn finish(&self, id: u64, conclusion: &str, log: &str, now_ms: u64) -> (Vec<u8>, String) {
        {
            let mut h = self.hub.lock().unwrap();
            let j = h.jobs.get_mut(&id).unwrap();
            for (k, v) in [("status", json!("completed")), ("conclusion", json!(conclusion)), ("completed_at", json!(iso(now_ms))),
                ("steps", json!([{ "name": "Set up job", "status": "completed", "conclusion": "success", "number": 1, "started_at": j["started_at"], "completed_at": iso(now_ms) },
                    { "name": "Run make test", "status": "completed", "conclusion": conclusion, "number": 2, "started_at": j["started_at"], "completed_at": iso(now_ms) }]))] { j[k] = v }
            h.logs.insert(id, log.to_string());
        }
        self.event("completed", id)
    }

    /// Its steps as GitHub has them now (their names, numbers and times), for a job that ran some side by side.
    pub fn set_steps(&self, id: u64, steps: Value) { self.hub.lock().unwrap().jobs.get_mut(&id).unwrap()["steps"] = steps }

    /// Its run ended, with these logs (GitHub keeps a run's logs, each step's apart, once the whole run has ended).
    pub fn run_ended(&self, id: u64, zip: Vec<u8>) {
        let mut h = self.hub.lock().unwrap();
        let run = h.jobs[&id]["run_id"].as_u64().unwrap();
        h.run_logs.insert(run, zip);
    }

    fn answer(&self, r: &Request) -> Option<Response> {
        let url = url::Url::parse(&r.url).ok()?;
        let path = url.path().to_string();
        let ok = |status: u16, v: Value| Some(Response::new(status, "application/json; charset=utf-8", v.to_string()));
        if r.url.starts_with(LOGS_AT) {
            if let Some(run) = path.rsplit('/').next()?.strip_prefix("run-") { return self.hub.lock().unwrap().run_logs.get(&run.parse().ok()?).map(|z| Response::new(200, "application/zip", z.clone())) }
            let id: u64 = path.rsplit('/').next()?.parse().ok()?;
            return self.hub.lock().unwrap().logs.get(&id).map(|l| Response::new(200, "text/plain", l.clone()));
        }
        // S3, at `<bucket>.s3.<region>.amazonaws.com` (a link signed or a call signed alike): put, read, and asked of.
        if let Some(bucket) = url.host_str().and_then(|h| h.strip_suffix(".amazonaws.com")).and_then(|h| h.split_once(".s3.")).map(|(b, _)| b.to_string()) {
            let key = format!("{bucket}/{}", path.trim_start_matches('/'));
            let mut h = self.hub.lock().unwrap();
            let missing = || Response::new(404, "application/xml", format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<Error><Code>NoSuchKey</Code><Message>The specified key does not exist.</Message><Key>{}</Key></Error>", path.trim_start_matches('/')));
            return Some(match r.method.as_str() {
                "PUT" => { h.objects.insert(key, r.body.clone()); Response::new(200, "application/xml", "") }
                "HEAD" => if h.objects.contains_key(&key) { Response::new(200, "application/octet-stream", "") } else { Response::new(404, "application/xml", "") },
                "GET" => h.objects.get(&key).map(|b| Response::new(200, "application/octet-stream", b.clone())).unwrap_or_else(missing),
                "DELETE" => { h.objects.remove(&key); Response::new(204, "application/xml", "") }
                _ => return None,
            })
        }
        if url.host_str() == Some("token.actions.githubusercontent.com") && path == "/.well-known/jwks" {
            self.hub.lock().unwrap().jwks_asked += 1;
            return ok(200, Self::job_keys())
        }
        if url.host_str() != Some("api.github.com") { return None }
        let permissions = json!({ "actions": "write", "metadata": "read", "organization_self_hosted_runners": "write" });
        let account = json!({ "login": OWNER, "id": 9_919, "type": "Organization", "avatar_url": "https://avatars.githubusercontent.com/u/9919?v=4", "html_url": format!("https://github.com/{OWNER}") });
        let segs: Vec<&str> = path.trim_matches('/').split('/').collect();
        let mut h = self.hub.lock().unwrap();
        match (r.method.as_str(), segs.as_slice()) {
            // The App's own (docs.github.com/rest/apps).
            ("GET", ["app"]) => ok(200, json!({ "id": APP_ID, "slug": "superci-acme", "node_id": "A_kwDOAAAA", "owner": account, "name": "SuperCI acme", "description": "",
                "external_url": "https://superci.dev", "html_url": "https://github.com/apps/superci-acme", "created_at": "2026-10-01T10:00:00Z", "updated_at": "2026-10-01T10:00:00Z",
                "permissions": permissions, "events": ["workflow_job"], "installations_count": 1 })),
            ("GET", ["app", "installations"]) => ok(200, json!([{ "id": INSTALLATION, "account": account, "repository_selection": "all", "access_tokens_url": format!("https://api.github.com/app/installations/{INSTALLATION}/access_tokens"),
                "repositories_url": "https://api.github.com/installation/repositories", "html_url": format!("https://github.com/organizations/{OWNER}/settings/installations/{INSTALLATION}"),
                "app_id": APP_ID, "app_slug": "superci-acme", "target_id": 9_919, "target_type": "Organization", "permissions": permissions, "events": ["workflow_job"],
                "created_at": "2026-10-01T10:00:00Z", "updated_at": "2026-10-01T10:00:00Z", "single_file_name": null, "suspended_at": null }])),
            ("POST", ["app", "installations", id, "access_tokens"]) if *id == INSTALLATION.to_string() => ok(201, json!({ "token": "ghs_16C7e42F292c6912E7710c838347Ae178B4a", "expires_at": "2099-01-01T00:00:00Z",
                "permissions": permissions, "repository_selection": "all" })),
            // The App uninstalled (docs.github.com/rest/apps/apps#delete-an-installation-for-the-authenticated-app).
            ("DELETE", ["app", "installations", id]) if *id == INSTALLATION.to_string() => { h.uninstalled = true; Some(Response::new(204, "text/plain", "")) }
            ("GET", ["app", "hook", "deliveries"]) => ok(200, json!([])),
            // Who a name is, and the App made from the dashboard's manifest (docs.github.com/rest/apps/apps#create-a-github-app-from-a-manifest).
            ("GET", ["users", OWNER]) => ok(200, fixture("org")),
            ("GET", ["users", _]) => ok(404, json!({ "message": "Not Found", "documentation_url": "https://docs.github.com/rest/users/users#get-a-user", "status": "404" })),
            ("POST", ["app-manifests", _, "conversions"]) => {
                let app = Self::app();
                ok(201, json!({ "id": APP_ID, "slug": "superci-acme", "node_id": "A_kwDOAAAA", "owner": account, "name": "SuperCI acme", "description": null,
                    "external_url": "https://superci.dev", "html_url": "https://github.com/apps/superci-acme", "created_at": "2026-10-15T10:00:00Z", "updated_at": "2026-10-15T10:00:00Z",
                    "permissions": permissions, "events": ["workflow_job"], "client_id": "Iv1.8a61f9b3a7aba766", "client_secret": "1726be1638095a19edd134c77bde3aa2ece1e5d8",
                    "webhook_secret": WEBHOOK_SECRET, "pem": app["pem"] }))
            }
            ("PATCH", ["app", "hook", "config"]) => ok(200, json!({ "content_type": "json", "insecure_ssl": "0", "url": serde_json::from_slice::<Value>(&r.body).ok()?["url"] })),
            // A repository by its id (docs.github.com/rest/repos/repos#get-a-repository, as `/repositories/{id}`).
            ("GET", ["repositories", id]) => { drop(h); let r = self.repo(); if r["id"].to_string() == *id { ok(200, r) } else { ok(404, json!({ "message": "Not Found", "documentation_url": "https://docs.github.com/rest/repos/repos#get-a-repository", "status": "404" })) } }
            ("GET", ["installation", "repositories"]) => { drop(h); ok(200, json!({ "total_count": 1, "repository_selection": "all", "repositories": [self.repo()] })) }
            // A runner for one job (docs.github.com/rest/actions/self-hosted-runners).
            ("POST", ["orgs", OWNER, "actions", "runners", "generate-jitconfig"]) => {
                let body: Value = serde_json::from_slice(&r.body).ok()?;
                h.next += 1;
                let id = 20 + h.next;
                let name = body["name"].as_str()?.to_string();
                h.runners.insert(id, name.clone());
                let labels: Vec<Value> = body["labels"].as_array()?.iter().enumerate().map(|(i, l)| json!({ "id": i + 1, "name": l, "type": "custom" })).collect();
                ok(201, json!({ "runner": { "id": id, "name": name, "os": "unknown", "status": "offline", "busy": false, "labels": labels },
                    "encoded_jit_config": superci_core::crypto::b64(format!("{{\"runner\":{id}}}").as_bytes()) }))
            }
            ("GET", ["orgs", OWNER, "actions", "runners", id]) => {
                let id: u64 = id.parse().ok()?;
                let name = h.runners.get(&id)?.clone();
                let busy = h.jobs.values().any(|j| j["runner_id"] == id && j["status"] == "in_progress");
                ok(200, json!({ "id": id, "name": name, "os": "linux", "status": "online", "busy": busy, "labels": [] }))
            }
            ("DELETE", ["orgs", OWNER, "actions", "runners", id]) => { h.runners.remove(&id.parse().ok()?); Some(Response::new(204, "text/plain", "")) }
            // Its repository, runs and jobs (in the shape recorded).
            ("GET", ["repos", o, n]) if format!("{o}/{n}") == REPO => { drop(h); ok(200, self.repo()) }
            ("GET", ["repos", o, n, "actions", "jobs", id]) if format!("{o}/{n}") == REPO => h.jobs.get(&id.parse().ok()?).cloned().map(|j| Response::new(200, "application/json", j.to_string())),
            // A run cancelled, as its Cancel button does: GitHub accepts it (its jobs end as `cancel` then says).
            ("POST", ["repos", o, n, "actions", "runs", run, "cancel"]) if format!("{o}/{n}") == REPO => {
                let run: u64 = run.parse().ok()?;
                if !h.jobs.values().any(|j| j["run_id"] == run) { return ok(404, json!({ "message": "Not Found", "documentation_url": "https://docs.github.com/rest/actions/workflow-runs#cancel-a-workflow-run", "status": "404" })) }
                ok(202, json!({}))
            }
            ("GET", ["repos", o, n, "actions", "runs", run, "logs"] | ["repos", o, n, "actions", "runs", run, "attempts", _, "logs"]) if format!("{o}/{n}") == REPO => {
                let run: u64 = run.parse().ok()?;
                if !h.run_logs.contains_key(&run) { return ok(404, json!({ "message": "Not Found", "documentation_url": "https://docs.github.com/rest/actions/workflow-runs#download-workflow-run-attempt-logs", "status": "404" })) }
                Some(Response::new(302, "text/plain", "").with_header("location", &format!("{LOGS_AT}/run-{run}")))
            }
            ("GET", ["repos", o, n, "actions", "jobs", id, "logs"]) if format!("{o}/{n}") == REPO => {
                let id: u64 = id.parse().ok()?;
                if !h.logs.contains_key(&id) { return ok(404, json!({ "message": "Not Found", "documentation_url": "https://docs.github.com/rest/actions/workflow-jobs#download-job-logs-for-a-workflow-run", "status": "404" })) }
                Some(Response::new(302, "text/plain", "").with_header("location", &format!("{LOGS_AT}/{id}")))
            }
            ("GET", ["repos", o, n, "actions", "runs", id]) if format!("{o}/{n}") == REPO => {
                let id: u64 = id.parse().ok()?;
                let jobs: Vec<&Value> = h.jobs.values().filter(|j| j["run_id"] == id).collect();
                let first = jobs.first()?;
                let mut run = fixture("run");
                let status = if jobs.iter().all(|j| j["status"] == "completed") { "completed" } else if jobs.iter().any(|j| j["status"] == "in_progress") { "in_progress" } else { "queued" };
                for (k, v) in [("id", json!(id)), ("status", json!(status)), ("conclusion", if status == "completed" { first["conclusion"].clone() } else { Value::Null }),
                    ("run_attempt", json!(1)), ("created_at", first["created_at"].clone()), ("run_started_at", first["created_at"].clone()), ("updated_at", first["created_at"].clone())] { run[k] = v }
                ok(200, run)
            }
            ("GET", ["repos", o, n, "actions", "runs", id, "jobs"]) if format!("{o}/{n}") == REPO => {
                let id: u64 = id.parse().ok()?;
                let jobs: Vec<Value> = h.jobs.values().filter(|j| j["run_id"] == id).cloned().collect();
                ok(200, json!({ "total_count": jobs.len(), "jobs": jobs }))
            }
            ("GET", ["repos", o, n, "actions", "runs"]) if format!("{o}/{n}") == REPO => {
                let want = url.query_pairs().find(|(k, _)| k == "status").map(|(_, v)| v.into_owned()).unwrap_or_default();
                let runs: Vec<u64> = h.jobs.values().filter(|j| want.is_empty() || j["status"] == want.as_str()).filter_map(|j| j["run_id"].as_u64()).collect();
                drop(h);
                let list: Vec<Value> = runs.into_iter().filter_map(|id| self.answer(&Request::new("GET", &format!("https://api.github.com/repos/{REPO}/actions/runs/{id}")))).filter_map(|r| serde_json::from_slice(&r.body).ok()).collect();
                ok(200, json!({ "total_count": list.len(), "workflow_runs": list }))
            }
            ("POST", ["repos", o, n, "actions", "jobs", _, "rerun"]) | ("POST", ["repos", o, n, "actions", "runs", _, "rerun-failed-jobs"]) if format!("{o}/{n}") == REPO => ok(201, json!({})),
            _ => None,
        }
    }
}

#[async_trait(?Send)]
impl Http for GitHub {
    async fn send(&self, r: Request) -> io::Result<Response> {
        if let Some(answer) = self.gitlab.answer(&r) { return Ok(answer) }
        self.asked.lock().unwrap().push(format!("{} {}", r.method, r.url));
        Ok(self.answer(&r).unwrap_or_else(|| {
            self.unanswered.lock().unwrap().push(format!("{} {}", r.method, r.url));
            Response::new(404, "text/plain", format!("not here: {} {}", r.method, r.url))
        }))
    }
}

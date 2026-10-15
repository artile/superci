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
    /// The runners registered (each machine's, by the control plane): id to name.
    runners: BTreeMap<u64, String>,
    next: u64,
    /// `acme/app` is public (anyone can open a pull request that runs code there).
    public: bool,
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

    fn answer(&self, r: &Request) -> Option<Response> {
        let url = url::Url::parse(&r.url).ok()?;
        let path = url.path().to_string();
        let ok = |status: u16, v: Value| Some(Response::new(status, "application/json; charset=utf-8", v.to_string()));
        if r.url.starts_with(LOGS_AT) {
            let id: u64 = path.rsplit('/').next()?.parse().ok()?;
            return self.hub.lock().unwrap().logs.get(&id).map(|l| Response::new(200, "text/plain", l.clone()));
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

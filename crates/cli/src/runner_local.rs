//! In a debug build only: the control plane's service for GitHub's runner, run on this computer for one job, to see
//! a real runner take a job from SuperCI's own code (`superci debug runner-service`). The runner reaches it through
//! a tunnel (it listens on 127.0.0.1); it ends once the runner says how the job ended.
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::executor::block_on;
use superci_core::io::{self, Request, Store};
use superci_core::runner_service::{access_token, answer, jit_config, job_message, Given, HandedJob, Ids};

#[derive(Clone, Default)]
struct Memory(Arc<Mutex<BTreeMap<String, String>>>);

#[async_trait(?Send)]
impl Store for Memory {
    async fn get(&self, k: &str) -> io::Result<Option<String>> { Ok(self.0.lock().unwrap().get(k).cloned()) }
    async fn put(&self, k: &str, v: String) -> io::Result<()> { self.0.lock().unwrap().insert(k.into(), v); Ok(()) }
    async fn put_if_absent(&self, k: &str, v: String) -> io::Result<bool> { let mut m = self.0.lock().unwrap(); if m.contains_key(k) { return Ok(false) } m.insert(k.into(), v); Ok(true) }
    async fn swap(&self, k: &str, was: Option<&str>, v: String) -> io::Result<bool> { let mut m = self.0.lock().unwrap(); if m.get(k).map(String::as_str) != was { return Ok(false) } m.insert(k.into(), v); Ok(true) }
    async fn delete(&self, k: &str) -> io::Result<()> { self.0.lock().unwrap().remove(k); Ok(()) }
    async fn list(&self, p: &str) -> io::Result<Vec<(String, String)>> { Ok(self.0.lock().unwrap().iter().filter(|(k, _)| k.starts_with(p)).map(|(k, v)| (k.clone(), v.clone())).collect()) }
}

/// Serves one job of `workflow` (a file here) on `port`, for a runner whose calls come to `root` (its address as
/// the runner sees it, e.g. http://localhost/runner/rig). Writes the runner's configuration to `jit_file` first.
pub fn serve(workflow: &str, job: &str, port: u16, root: &str, secrets: &[(String, String)], vars: &[(String, String)], jit_file: &str) -> Result<String, String> {
    let text = std::fs::read_to_string(workflow).map_err(|e| format!("{workflow}: {e}"))?;
    let path = format!(".github/workflows/{}", std::path::Path::new(workflow).file_name().and_then(|n| n.to_str()).unwrap_or("ci.yml"));
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0);
    let github = serde_json::json!({ "event_name": "push", "event": {}, "repository": "acme/app", "repository_owner": "acme", "ref": "refs/heads/main", "ref_name": "main", "ref_type": "branch",
        "sha": "0000000000000000000000000000000000000000", "actor": "superci", "triggering_actor": "superci", "workflow": "local", "run_id": "1", "run_number": "1", "run_attempt": "1",
        "server_url": "https://github.com", "api_url": "https://api.github.com", "graphql_url": "https://api.github.com/graphql", "job": job, "retention_days": "1" });
    let service = format!("{root}/");
    let given = Given { workflow: &text, path: &path, job, secrets, vars, github, service: &service, access_token: &access_token(now), ids: Ids::new(1) };
    let message = job_message(&given)?;
    let store = Memory::default();
    block_on(store.put("rjob:local", serde_json::to_string(&HandedJob { message, ..Default::default() }).unwrap()))?;
    std::fs::write(jit_file, jit_config(root, "superci-local")?).map_err(|e| e.to_string())?;
    let prefix = url::Url::parse(root).map_err(|e| e.to_string())?.path().trim_end_matches('/').to_string();
    let listener = TcpListener::bind(("127.0.0.1", port)).map_err(|e| e.to_string())?;
    eprintln!("serving {job} of {path} on 127.0.0.1:{port} for {root}; the runner's configuration is in {jit_file}");
    for stream in listener.incoming() {
        let Ok(mut stream) = stream else { continue };
        let (here, prefix) = (store.clone(), prefix.clone());
        std::thread::spawn(move || {
            let store = here;
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut first = String::new();
            if reader.read_line(&mut first).is_err() { return }
            let mut parts = first.split_whitespace();
            let (method, target) = (parts.next().unwrap_or_default().to_string(), parts.next().unwrap_or_default().to_string());
            let mut length = 0usize;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).is_err() || line.trim().is_empty() { break }
                if let Some((k, v)) = line.split_once(':') { if k.eq_ignore_ascii_case("content-length") { length = v.trim().parse().unwrap_or(0) } }
            }
            let mut body = vec![0u8; length];
            if reader.read_exact(&mut body).is_err() { return }
            let path = target.strip_prefix(&prefix).unwrap_or(&target).to_string();
            let mut req = Request::new(&method, &format!("http://localhost{target}"));
            req.body = body;
            let res = block_on(answer(&store, "local", &req, &path));
            if res.as_ref().is_ok_and(|r| r.header("x-superci-hold").is_some()) { std::thread::sleep(std::time::Duration::from_millis(10_000)) }
            let res = res.unwrap_or_else(|e| io::Response::text(500, &e));
            eprintln!("{method} {} -> {}", path.split('?').next().unwrap_or_default(), res.status);
            let head = format!("HTTP/1.1 {} X\r\ncontent-type: {}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n", res.status, res.header("content-type").unwrap_or("application/json"), res.body.len());
            let _ = stream.write_all(head.as_bytes()).and_then(|_| stream.write_all(&res.body));
        });
        let job: Option<HandedJob> = block_on(store.get("rjob:local")).ok().flatten().and_then(|v| serde_json::from_str(&v).ok());
        if let Some(job) = job.filter(|j| j.result.is_some()) {
            std::thread::sleep(std::time::Duration::from_millis(500));
            let lines: Vec<String> = block_on(store.list("rline:local:")).unwrap_or_default().into_iter().flat_map(|(_, v)| serde_json::from_str::<Vec<String>>(&v).unwrap_or_default()).collect();
            let steps: Vec<String> = job.steps.iter().filter(|s| s["type"] == "Task").map(|s| format!("{} {}", s["name"].as_str().unwrap_or_default(), s["result"].as_str().unwrap_or("?"))).collect();
            return Ok(format!("the job ended: {}\nsteps: {}\noutputs: {}\n{}", job.result.unwrap_or_default(), steps.join("; "), job.outputs.unwrap_or_default(), lines.join("\n")))
        }
    }
    Err("the service stopped".into())
}

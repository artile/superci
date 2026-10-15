//! A SuperCI control plane served on this computer for tests (`serve`), its own code end to end: a store in memory
//! the test can look into, stand-ins for the clouds (a machine is a container that starts at once and does nothing
//! more), and a runner (`Runner`) that does what GitHub's runner does on a machine, by the same calls. So the CLI and
//! the dashboard are tested as they run, against the control plane as it runs, offline.
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::executor::block_on;
use serde_json::{json, Value};
use superci_core::io::{self, Clock, Containers, Http, Request, Response, Store, Timer, Work};
use superci_core::plane::{Cache, Config, ControlPlane};

/// A store in memory, shared with the test.
#[derive(Clone, Default)]
pub struct Shared(pub Arc<Mutex<BTreeMap<String, String>>>);

#[async_trait(?Send)]
impl Store for Shared {
    async fn get(&self, k: &str) -> io::Result<Option<String>> { Ok(self.0.lock().unwrap().get(k).cloned()) }
    async fn put(&self, k: &str, v: String) -> io::Result<()> { self.0.lock().unwrap().insert(k.into(), v); Ok(()) }
    async fn put_if_absent(&self, k: &str, v: String) -> io::Result<bool> { let mut m = self.0.lock().unwrap(); if m.contains_key(k) { return Ok(false) } m.insert(k.into(), v); Ok(true) }
    async fn swap(&self, k: &str, was: Option<&str>, v: String) -> io::Result<bool> { let mut m = self.0.lock().unwrap(); if m.get(k).map(String::as_str) != was { return Ok(false) } m.insert(k.into(), v); Ok(true) }
    async fn delete(&self, k: &str) -> io::Result<()> { self.0.lock().unwrap().remove(k); Ok(()) }
    async fn list(&self, p: &str) -> io::Result<Vec<(String, String)>> { Ok(self.0.lock().unwrap().iter().filter(|(k, _)| k.starts_with(p)).map(|(k, v)| (k.clone(), v.clone())).collect()) }
}

impl Shared {
    /// The records under `prefix`, as the test reads them.
    pub fn under(&self, prefix: &str) -> Vec<(String, Value)> {
        self.0.lock().unwrap().iter().filter(|(k, _)| k.starts_with(prefix)).map(|(k, v)| (k.clone(), serde_json::from_str(v).unwrap_or(Value::String(v.clone())))).collect()
    }
}

/// Every other service answers that it is not there (a test that needs one gives it its own answers).
struct Nowhere;
#[async_trait(?Send)]
impl Http for Nowhere {
    async fn send(&self, r: Request) -> io::Result<Response> { Ok(Response::new(404, "text/plain", format!("not here: {} {}", r.method, r.url))) }
}

/// Machines: each starts at once, its runner given to the test to play (see `Runner`).
#[derive(Clone, Default)]
pub struct Machines(pub Arc<Mutex<Vec<String>>>);
#[async_trait(?Send)]
impl Containers for Machines {
    async fn start(&self, name: &str, _work: &Work, _max_minutes: u32, _size: superci_core::spec::Size) -> io::Result<String> { self.0.lock().unwrap().push(name.to_string()); Ok(format!("machine-{name}")) }
    async fn stop(&self, _id: &str) -> io::Result<()> { Ok(()) }
}

struct Now;
impl Clock for Now { fn now_ms(&self) -> u64 { std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0) } }
struct NoTimer;
#[async_trait(?Send)]
impl Timer for NoTimer { async fn wake_in(&self, _ms: u64) -> io::Result<()> { Ok(()) } }

/// A control plane served on this computer: its address, its store, its machines, and a key with which the dashboard
/// and the CLI reach it (and one for coding agents, which may also start runs).
pub struct Served { pub url: String, pub store: Shared, pub machines: Machines, pub key: String, pub run_key: String }

/// Serves a control plane with machines of its own (`containers`), `configure`d further by the test.
pub fn serve(configure: impl FnOnce(&mut Config) + Send + 'static) -> Served {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    let (store, machines) = (Shared::default(), Machines::default());
    let (key, run_key) = ("dashboard-key-for-tests-0001".to_string(), "superci_run_0123456789abcdef0123456789abcdef".to_string());
    let (s, m, k, rk, u) = (store.clone(), machines.clone(), key.clone(), run_key.clone(), url.clone());
    std::thread::spawn(move || {
        let mut config = Config::new("testplane0001".into());
        config.containers = true;
        config.dashboard_keys = vec![(u64::MAX, k)];
        config.read_keys = vec![superci_core::plane::read_key("READ_KEY_4102444800_AGENT", &format!("{}:runs", superci_core::crypto::sha256_hex(rk.as_bytes()))).unwrap()];
        configure(&mut config);
        let cache = Cache::default();
        let plane = ControlPlane { store: &s, http: &Nowhere, clock: &Now, timer: &NoTimer, config: &config, cache: &cache, containers: Some(&m) };
        block_on(s.put("plane_url", format!("\"{u}\""))).unwrap();
        for stream in listener.incoming().flatten() { let _ = answer(&plane, &u, stream); }
    });
    Served { url, store, machines, key, run_key }
}

/// One request on its connection, answered by the control plane, then the connection closed.
fn answer(plane: &ControlPlane, base: &str, stream: TcpStream) -> std::io::Result<()> {
    let mut r = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    r.read_line(&mut line)?;
    let mut p = line.split_whitespace();
    let (method, target) = (p.next().unwrap_or_default().to_string(), p.next().unwrap_or_default().to_string());
    let (mut headers, mut len) = (vec![], 0usize);
    loop {
        let mut h = String::new();
        r.read_line(&mut h)?;
        if h.trim().is_empty() { break }
        if let Some((k, v)) = h.split_once(':') {
            if k.eq_ignore_ascii_case("content-length") { len = v.trim().parse().unwrap_or(0) }
            headers.push((k.trim().to_string(), v.trim().to_string()));
        }
    }
    let mut body = vec![0; len];
    r.read_exact(&mut body)?;
    let mut req = Request::new(&method, &format!("{base}{target}")).with_body(body);
    for (k, v) in headers { req = req.with_header(&k, &v) }
    let res = block_on(plane.handle(req));
    let mut out = stream;
    let mut head = format!("HTTP/1.1 {} X\r\ncontent-length: {}\r\nconnection: close\r\n", res.status, res.body.len());
    for (k, v) in &res.headers { if !k.eq_ignore_ascii_case("content-length") { head.push_str(&format!("{k}: {v}\r\n")) } }
    out.write_all(format!("{head}\r\n").as_bytes())?;
    out.write_all(&res.body)
}

/// GitHub's runner on a machine, played by the test: what it says of a job's steps, its lines as it writes them, each
/// step's whole log, and how the job ended, by the calls the runner makes (to the link its machine was given).
pub struct Runner { pub at: String }

impl Runner {
    /// The runner of the job handed out last for run `run` (its link, from the control plane's record of the run).
    pub fn of_run(served: &Served, run: &str) -> Option<Runner> {
        let r = served.store.under(&format!("run:{run}")).into_iter().next()?.1;
        let token = r["jobs"].as_array()?.iter().find_map(|j| j["token"].as_str())?.to_string();
        Some(Runner { at: format!("{}/runner/{token}/_apis/v1", served.url) })
    }
    fn post(&self, path: &str, body: Vec<u8>) -> u16 {
        let u = self.at.trim_start_matches("http://");
        let (host, rest) = u.split_once('/').unwrap();
        let mut s = TcpStream::connect(host).unwrap();
        s.write_all(format!("POST /{rest}{path} HTTP/1.1\r\nhost: {host}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n", body.len()).as_bytes()).unwrap();
        s.write_all(&body).unwrap();
        let mut out = String::new();
        let _ = s.read_to_string(&mut out);
        out.get(9..12).and_then(|c| c.parse().ok()).unwrap_or(0)
    }
    /// Each step's record: (id, name, order, state, result, its log's number).
    pub fn steps(&self, steps: &[(&str, &str, u64, &str, Option<&str>, Option<u64>)]) -> u16 {
        let value: Vec<Value> = steps.iter().map(|(id, name, order, state, result, log)| json!({ "id": id, "type": "Task", "name": name, "order": order, "state": state, "result": result,
            "log": log.map(|n| json!({ "id": n })), "startTime": "2026-10-15T10:00:00Z", "finishTime": if *state == "completed" { json!("2026-10-15T10:00:07Z") } else { Value::Null } })).collect();
        self.post("/Timeline/scope/hub/plan/timeline", json!({ "value": value }).to_string().into_bytes())
    }
    /// Lines of a step as it sends them while it runs, from line `start` of the step's log.
    pub fn lines(&self, step: &str, start: u64, lines: &[String]) -> u16 {
        self.post(&format!("/TimeLineWebConsoleLog/scope/hub/plan/timeline/{step}"), json!({ "value": lines, "stepId": step, "startLine": start }).to_string().into_bytes())
    }
    /// A step's whole log, as log `n`.
    pub fn log(&self, n: u64, text: &str) -> u16 { self.post(&format!("/Logfiles/scope/hub/plan/{n}"), text.as_bytes().to_vec()) }
    /// The job is over.
    pub fn finish(&self, result: &str) -> u16 { self.post("/FinishJob/scope/hub/plan", json!({ "result": result }).to_string().into_bytes()) }
}

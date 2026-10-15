//! A SuperCI control plane served on this computer for tests (`serve`), its own code end to end: a store in memory
//! the test can look into, stand-ins for the clouds (a machine is a container that starts at once and does nothing
//! more; AWS's SSM, where the dashboard keeps the control plane's settings, `Aws`), and a runner (`Runner`) that does
//! what GitHub's runner does on a machine, by the same calls. So the CLI and the dashboard are tested as they run,
//! against the control plane as it runs, offline.
#[cfg(unix)]
mod browser;
#[cfg(unix)]
pub use browser::Browser;

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

/// A control plane served on this computer: its address, its store, its machines, AWS as it finds it (its settings
/// are SSM parameters there, `/superci/testplane0001/…`, read on every request), and a key with which the dashboard
/// and the CLI reach it (and one for coding agents, which may also start runs).
pub struct Served { pub url: String, pub store: Shared, pub machines: Machines, pub aws: Aws, pub key: String, pub run_key: String }

/// The control plane's id, and the AWS account and region it is in.
pub const PLANE: &str = "testplane0001";
pub const ACCOUNT: &str = "111111111111";
pub const REGION: &str = "us-east-1";

/// Serves a control plane with machines of its own (`containers`), `configure`d further by the test.
pub fn serve(configure: impl Fn(&mut Config) + Send + 'static) -> Served {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    let (store, machines, aws) = (Shared::default(), Machines::default(), Aws::serve());
    *aws.plane_url.lock().unwrap() = url.clone();
    let (key, run_key) = ("dashboard-key-for-tests-0001".to_string(), "superci_run_0123456789abcdef0123456789abcdef".to_string());
    // Its keys, as a deploy and `superci keys create agent --runs` leave them.
    aws.put(&format!("/superci/{PLANE}/DASHBOARD_KEY_4102444800_TEST"), &key);
    aws.put(&format!("/superci/{PLANE}/READ_KEY_4102444800_AGENT"), &format!("{}:runs", superci_core::crypto::sha256_hex(run_key.as_bytes())));
    aws.put(&format!("/superci/{PLANE}/CONTAINERS"), "on");
    let (s, m, a, u) = (store.clone(), machines.clone(), aws.clone(), url.clone());
    std::thread::spawn(move || {
        let cache = Cache::default();
        block_on(s.put("plane_url", format!("\"{u}\""))).unwrap();
        for stream in listener.incoming().flatten() {
            let mut config = Config::new(PLANE.into());
            let settings = a.settings(PLANE);
            // (Its machines are its own containers, as a Cloudflare or Modal control plane's are: on until removed.)
            config.containers = settings.iter().any(|(k, v)| k == "CONTAINERS" && v == "on");
            config.read_settings(&settings);
            configure(&mut config);
            let plane = ControlPlane { store: &s, http: &Nowhere, clock: &Now, timer: &NoTimer, config: &config, cache: &cache, containers: Some(&m) };
            let _ = answer(&plane, &u, stream);
        }
    });
    Served { url, store, machines, aws, key, run_key }
}

/// A request as it arrived: method, target, headers, body.
fn read(stream: &TcpStream) -> std::io::Result<(String, String, Vec<(String, String)>, Vec<u8>)> {
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
    Ok((method, target, headers, body))
}

fn write(mut out: TcpStream, status: u16, headers: &[(String, String)], body: &[u8]) -> std::io::Result<()> {
    let mut head = format!("HTTP/1.1 {status} X\r\ncontent-length: {}\r\nconnection: close\r\n", body.len());
    for (k, v) in headers { if !k.eq_ignore_ascii_case("content-length") { head.push_str(&format!("{k}: {v}\r\n")) } }
    out.write_all(format!("{head}\r\n").as_bytes())?;
    out.write_all(body)
}

/// One request on its connection, answered by the control plane, then the connection closed.
fn answer(plane: &ControlPlane, base: &str, stream: TcpStream) -> std::io::Result<()> {
    let (method, target, headers, body) = read(&stream)?;
    let mut req = Request::new(&method, &format!("{base}{target}")).with_body(body);
    for (k, v) in headers { req = req.with_header(&k, &v) }
    let res = block_on(plane.handle(req));
    write(stream, res.status, &res.headers, &res.body)
}

/// AWS as SuperCI's calls find it (the CLI and the dashboard reach it with `SUPERCI_AWS_AT` set to `url`): SSM's
/// parameters, kept and answered for exactly as SSM does (its answers recorded from a real account are
/// `crates/cli/fixtures/aws/settings/ssm.jsonl`, and a test holds this to them). Each exchange is kept, in the shape
/// of a recording (`asked`); what it has no answer for is answered with an error that names it, and kept apart
/// (`unanswered`), so a test sees what it reached that is not stood in for yet.
#[derive(Clone, Default)]
pub struct Aws {
    pub url: String,
    /// Each parameter: its value, version, and when it was written last (milliseconds).
    pub params: Arc<Mutex<BTreeMap<String, (String, u64, u64)>>>,
    pub asked: Arc<Mutex<Vec<Value>>>,
    pub unanswered: Arc<Mutex<Vec<String>>>,
    /// What a real account answered for the rest (`fixtures/aws/settings/dashboard.jsonl`: what the dashboard asks of
    /// AWS when signed in), and the address its control plane's stands in for (`http://plane.test`).
    pub recorded: Arc<Vec<Value>>,
    pub plane_url: Arc<Mutex<String>>,
}

/// What a request asks AWS to do: its target (JSON APIs) or its Action (query APIs).
fn action(target: &str, asked: &str) -> String {
    if !target.is_empty() { return target.to_string() }
    asked.split('&').find_map(|kv| kv.strip_prefix("Action=")).unwrap_or_default().to_string()
}

impl Aws {
    pub fn serve() -> Aws {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let recorded = include_str!("../../cli/fixtures/aws/settings/dashboard.jsonl").lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
        let aws = Aws { url: format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port()), recorded: Arc::new(recorded), ..Default::default() };
        let a = aws.clone();
        std::thread::spawn(move || for stream in listener.incoming().flatten() {
            let Ok((method, target, headers, body)) = read(&stream) else { continue };
            let header = |n: &str| headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(n)).map(|(_, v)| v.clone()).unwrap_or_default();
            let (status, answer) = a.handle(&method, &format!("https:/{target}"), &header("x-amz-target"), &String::from_utf8_lossy(&body));
            let kind = if answer.starts_with('<') { "text/xml" } else { "application/x-amz-json-1.1" };
            let _ = write(stream, status, &[("content-type".into(), kind.into())], answer.as_bytes());
        });
        aws
    }

    /// A parameter written as the dashboard writes one.
    pub fn put(&self, name: &str, value: &str) {
        let mut p = self.params.lock().unwrap();
        let version = p.get(name).map(|(_, v, _)| v + 1).unwrap_or(1);
        p.insert(name.to_string(), (value.to_string(), version, Now.now_ms()));
    }

    /// A control plane's settings, by name, as it reads them (`/superci/<id>/<NAME>`).
    pub fn settings(&self, plane: &str) -> Vec<(String, String)> {
        let prefix = format!("/superci/{plane}/");
        self.params.lock().unwrap().iter().filter_map(|(k, (v, _, _))| Some((k.strip_prefix(&prefix)?.to_string(), v.clone()))).filter(|(k, _)| !k.contains('/')).collect()
    }

    /// One exchange: `url` is AWS's own (`https://ssm.us-east-1.amazonaws.com/`).
    pub fn handle(&self, method: &str, url: &str, target: &str, asked: &str) -> (u16, String) {
        let (status, answer) = match self.answer(url, target, asked) {
            (400, a) if a["__type"] == "SuperCITestkitUnknown" => self.replay(url, target, asked).unwrap_or((400, a.to_string())),
            (status, a) => (status, a.to_string()),
        };
        if status == 400 && answer.contains("SuperCITestkitUnknown") { self.unanswered.lock().unwrap().push(format!("{target} {url} {}", asked.chars().take(200).collect::<String>())) }
        self.asked.lock().unwrap().push(json!({ "method": method, "url": url, "target": target, "asked": asked, "status": status, "answer": answer }));
        (status, answer)
    }

    /// The recorded answer to the same request, else to the same action at the same address.
    fn replay(&self, url: &str, target: &str, asked: &str) -> Option<(u16, String)> {
        let at = |r: &&Value| r["url"] == url && r["target"] == target;
        let found = self.recorded.iter().find(|r| at(r) && r["asked"] == asked)
            .or_else(|| self.recorded.iter().find(|r| at(r) && action(r["target"].as_str().unwrap_or_default(), r["asked"].as_str().unwrap_or_default()) == action(target, asked)))?;
        let answer = found["answer"].as_str()?.replace("http://plane.test", &self.plane_url.lock().unwrap());
        Some((found["status"].as_u64()? as u16, answer))
    }

    fn answer(&self, url: &str, target: &str, asked: &str) -> (u16, Value) {
        let a: Value = serde_json::from_str(asked).unwrap_or_default();
        let region = url.split('.').nth(1).unwrap_or(REGION).to_string();
        let not_found = || (400, json!({ "__type": "ParameterNotFound" }));
        // (SSM's time: seconds, with milliseconds.)
        let at = |ms: u64| Value::from(ms as f64 / 1000.0);
        let one = |name: &str, (value, version, ms): &(String, u64, u64), decrypt: bool| json!({ "ARN": format!("arn:aws:ssm:{region}:{ACCOUNT}:parameter{name}"), "DataType": "text", "LastModifiedDate": at(*ms),
            "Name": name, "Type": "SecureString", "Value": if decrypt { value.clone() } else { "EXAMPLECIPHERTEXT".to_string() }, "Version": version });
        let name = a["Name"].as_str().unwrap_or_default().to_string();
        match target {
            "AmazonSSM.PutParameter" => {
                if self.params.lock().unwrap().contains_key(&name) && a["Overwrite"] != true { return (400, json!({ "__type": "ParameterAlreadyExists" })) }
                self.put(&name, a["Value"].as_str().unwrap_or_default());
                (200, json!({ "Tier": "Standard", "Version": self.params.lock().unwrap()[&name].1 }))
            }
            "AmazonSSM.GetParameter" => match self.params.lock().unwrap().get(&name) {
                Some(p) => (200, json!({ "Parameter": one(&name, p, a["WithDecryption"] == true) })),
                None => not_found(),
            },
            "AmazonSSM.DeleteParameter" => match self.params.lock().unwrap().remove(&name) { Some(_) => (200, json!({})), None => not_found() },
            "AmazonSSM.GetParametersByPath" => {
                let path = a["Path"].as_str().unwrap_or_default();
                let p = self.params.lock().unwrap();
                let under: Vec<Value> = p.iter().filter(|(k, _)| k.strip_prefix(path).is_some_and(|rest| a["Recursive"] == true || !rest.contains('/'))).map(|(k, v)| one(k, v, a["WithDecryption"] == true)).collect();
                let from: usize = a["NextToken"].as_str().and_then(|t| t.strip_prefix("next-")).and_then(|n| n.parse().ok()).unwrap_or(0);
                let max = a["MaxResults"].as_u64().unwrap_or(10) as usize;
                let page: Vec<Value> = under.iter().skip(from).take(max).cloned().collect();
                let mut out = json!({ "Parameters": page });
                if from + max < under.len() { out["NextToken"] = json!(format!("next-{}", from + max)) }
                (200, out)
            }
            _ => (400, json!({ "__type": "SuperCITestkitUnknown", "message": format!("the test kit's AWS has no answer for {target} at {url}") })),
        }
    }
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
    /// It takes the job (asks for its message, as a runner does once it listens).
    pub fn take(&self) -> u16 { self.post("/Message/1", Vec::new()) }
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

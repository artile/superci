//! A SuperCI control plane served on this computer for tests (`serve`), its own code end to end: a store in memory
//! the test can look into, stand-ins for the clouds (a machine is a container that starts at once and does nothing
//! more; AWS's SSM, where the dashboard keeps the control plane's settings, `Aws`), and a runner (`Runner`) that does
//! what GitHub's runner does on a machine, by the same calls. So the CLI and the dashboard are tested as they run,
//! against the control plane as it runs, offline.
pub mod github;
pub use github::GitHub;
pub mod gitlab;
pub use gitlab::GitLab;
#[cfg(unix)]
pub mod cloudflare;
#[cfg(unix)]
pub use cloudflare::{serve_cloudflare, CloudflarePlane};
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
use superci_core::io::{self, Clock, Containers, Request, Store, Timer, Work};
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


/// Machines: each starts at once, its runner given to the test to play (see `Runner`).
#[derive(Clone, Default)]
pub struct Machines(pub Arc<Mutex<Vec<String>>>);
#[async_trait(?Send)]
impl Containers for Machines {
    async fn start(&self, name: &str, _work: &Work, _max_minutes: u32, _size: superci_core::spec::Size) -> io::Result<String> { self.0.lock().unwrap().push(name.to_string()); Ok(format!("machine-{name}")) }
    async fn stop(&self, _id: &str) -> io::Result<()> { Ok(()) }
}

fn system_ms() -> u64 { std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0) }

/// The control plane's time: this computer's, moved on by the test (`Served::later`).
#[derive(Clone, Default)]
struct Now(Arc<std::sync::atomic::AtomicU64>);
impl Clock for Now { fn now_ms(&self) -> u64 { system_ms() + self.0.load(std::sync::atomic::Ordering::SeqCst) } }
struct NoTimer;
#[async_trait(?Send)]
impl Timer for NoTimer { async fn wake_in(&self, _ms: u64) -> io::Result<()> { Ok(()) } }

/// A control plane served on this computer: its address, its store, its machines, AWS as it finds it (its settings
/// are SSM parameters there, `/superci/testplane0001/…`, read on every request), and a key with which the dashboard
/// and the CLI reach it (and one for coding agents, which may also start runs).
pub struct Served { pub plane: String, pub url: String, pub store: Shared, pub machines: Machines, pub aws: Aws, pub github: GitHub, pub gitlab: GitLab, pub key: String, pub run_key: String, clock: Now }

impl Served {
    /// Moves the control plane's time on by `ms`.
    pub fn later(&self, ms: u64) { self.clock.0.fetch_add(ms, std::sync::atomic::Ordering::SeqCst); }

    /// The control plane's time now.
    pub fn now_ms(&self) -> u64 { self.clock.now_ms() }

    /// SuperCI's App for the organization `acme`, as the dashboard leaves it once made and installed (`GitHub`).
    pub fn connect_github(&self) { self.aws.put(&format!("/superci/{}/GITHUB_APP", self.plane), &GitHub::app().to_string()) }

    /// A webhook GitHub sends (`GitHub::queue`, `start`, `finish`), as it sends it: the control plane's answer.
    pub fn webhook(&self, (body, signature): (Vec<u8>, String)) -> u16 {
        let host = self.url.trim_start_matches("http://");
        let mut s = TcpStream::connect(host).unwrap();
        s.write_all(format!("POST /webhook HTTP/1.1\r\nhost: {host}\r\ncontent-type: application/json\r\nx-github-event: workflow_job\r\nx-hub-signature-256: {signature}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n", body.len()).as_bytes()).unwrap();
        s.write_all(&body).unwrap();
        let mut out = String::new();
        let _ = s.read_to_string(&mut out);
        out.get(9..12).and_then(|c| c.parse().ok()).unwrap_or(0)
    }

    /// A GitLab connection, as the dashboard leaves it (`GitLab`).
    pub fn connect_gitlab(&self) { self.aws.put(&format!("/superci/{}/GITLAB", self.plane), &GitLab::connection().to_string()) }

    /// A job event of GitLab's (`GitLab::pending`, `running`, `finish`), with the webhook's secret: the control
    /// plane's answer.
    pub fn gitlab_webhook(&self, body: Vec<u8>) -> u16 {
        let host = self.url.trim_start_matches("http://");
        let mut s = TcpStream::connect(host).unwrap();
        s.write_all(format!("POST /gitlab/webhook HTTP/1.1\r\nhost: {host}\r\ncontent-type: application/json\r\nx-gitlab-event: Job Hook\r\nx-gitlab-token: {}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n", gitlab::HOOK_SECRET, body.len()).as_bytes()).unwrap();
        s.write_all(&body).unwrap();
        let mut out = String::new();
        let _ = s.read_to_string(&mut out);
        out.get(9..12).and_then(|c| c.parse().ok()).unwrap_or(0)
    }

    /// Its upkeep, as its cloud runs it every minute (`ControlPlane::alarm`).
    pub fn upkeep(&self) {
        let host = self.url.trim_start_matches("http://");
        let mut s = TcpStream::connect(host).unwrap();
        s.write_all(format!("POST /__testkit/upkeep HTTP/1.1\r\nhost: {host}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n").as_bytes()).unwrap();
        let mut out = String::new();
        let _ = s.read_to_string(&mut out);
        assert!(out.starts_with("HTTP/1.1 200"), "{out}");
    }
}

/// The control plane's id, and the AWS account and region it is in.
pub const PLANE: &str = "testplane0001";
pub const ACCOUNT: &str = "111111111111";
pub const REGION: &str = "us-east-1";

/// Serves a control plane with machines of its own (`containers`), `configure`d further by the test.
pub fn serve(configure: impl Fn(&mut Config) + Send + 'static) -> Served { serve_as(PLANE, configure) }

/// The same, with this id (a second control plane beside the first).
pub fn serve_as(plane: &str, configure: impl Fn(&mut Config) + Send + 'static) -> Served {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    let (store, machines, aws) = (Shared::default(), Machines::default(), Aws::serve());
    *aws.plane_url.lock().unwrap() = url.clone();
    let github = GitHub::default();
    *aws.github.lock().unwrap() = Some(github.clone());
    let (key, run_key) = ("dashboard-key-for-tests-0001".to_string(), "superci_run_0123456789abcdef0123456789abcdef".to_string());
    // Its keys, as a deploy and `superci keys create agent --runs` leave them.
    aws.put(&format!("/superci/{plane}/DASHBOARD_KEY_4102444800_TEST"), &key);
    aws.put(&format!("/superci/{plane}/READ_KEY_4102444800_AGENT"), &format!("{}:runs", superci_core::crypto::sha256_hex(run_key.as_bytes())));
    aws.put(&format!("/superci/{plane}/CONTAINERS"), "on");
    let clock = Now::default();
    let (s, m, a, u, c, g, id) = (store.clone(), machines.clone(), aws.clone(), url.clone(), clock.clone(), github.clone(), plane.to_string());
    std::thread::spawn(move || {
        let cache = Cache::default();
        block_on(s.put("plane_url", format!("\"{u}\""))).unwrap();
        for stream in listener.incoming().flatten() {
            let mut config = Config::new(id.clone());
            let settings = a.settings(&id);
            // (Its machines are its own containers, as a Cloudflare or Modal control plane's are: on until removed.)
            config.containers = settings.iter().any(|(k, v)| k == "CONTAINERS" && v == "on");
            config.read_settings(&settings);
            configure(&mut config);
            let plane = ControlPlane { store: &s, http: &g, clock: &c, timer: &NoTimer, config: &config, cache: &cache, containers: Some(&m) };
            let _ = answer(&plane, &u, stream);
        }
    });
    let gitlab = github.gitlab.clone();
    Served { plane: plane.to_string(), url, store, machines, aws, github, gitlab, key, run_key, clock }
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
    if target == "/__testkit/upkeep" {
        return match block_on(plane.alarm()) { Ok(()) => write(stream, 200, &[], b"ok"), Err(e) => write(stream, 500, &[], e.as_bytes()) };
    }
    let mut req = Request::new(&method, &format!("{base}{target}")).with_body(body);
    for (k, v) in headers { req = req.with_header(&k, &v) }
    let res = block_on(plane.handle(req));
    write(stream, res.status, &res.headers, &res.body)
}

/// AWS as SuperCI's calls find it (the CLI and the dashboard reach it with `SUPERCI_STAND_IN` set to `url`; what they ask of other hosts there, GitHub's stand-in answers): SSM's
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
    pub recorded: Arc<Mutex<Vec<Value>>>,
    /// How many of each recorded call's answers were given (several of the same call are answered in their order).
    replayed: Arc<Mutex<BTreeMap<String, usize>>>,
    pub plane_url: Arc<Mutex<String>>,
    /// Where what is not AWS's goes (GitHub's logs).
    pub github: Arc<Mutex<Option<GitHub>>>,
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
        let aws = Aws { url: format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port()), recorded: Arc::new(Mutex::new(recorded)), ..Default::default() };
        let a = aws.clone();
        std::thread::spawn(move || for stream in listener.incoming().flatten() {
            let Ok((method, target, headers, body)) = read(&stream) else { continue };
            let header = |n: &str| headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(n)).map(|(_, v)| v.clone()).unwrap_or_default();
            let host = target.split('/').nth(1).unwrap_or_default().to_string();
            // (S3's objects are kept by GitHub's stand-in, which the control plane reaches AWS through.)
            if (!host.ends_with(".amazonaws.com") && !host.ends_with(".signin.aws.amazon.com")) || host.contains(".s3.") {
                let gh = a.github.lock().unwrap().clone().unwrap_or_default();
                let mut req = Request::new(&method, &format!("https:/{target}")).with_body(body);
                for (k, v) in &headers { if !k.eq_ignore_ascii_case("host") { req = req.with_header(k, v) } }
                let res = block_on(superci_core::io::Http::send(&gh, req)).unwrap();
                let _ = write(stream, res.status, &res.headers, &res.body);
                continue;
            }
            let (status, mut said, answer) = a.handle_full(&method, &format!("https:/{target}"), &header("x-amz-target"), &String::from_utf8_lossy(&body));
            if !said.iter().any(|(k, _)| k.eq_ignore_ascii_case("content-type")) { said.push(("content-type".into(), if answer.starts_with('<') { "text/xml" } else { "application/x-amz-json-1.1" }.into())) }
            let _ = write(stream, status, &said, answer.as_bytes());
        });
        aws
    }

    /// What a real account answered in a situation (`fixtures/aws/planes/update.jsonl`, …), answered first from now
    /// on, with what stood for something there replaced by what it is here (a control plane's id, its address).
    pub fn replaying(&self, recording: &str, replace: &[(&str, &str)]) {
        let mut all = self.recorded.lock().unwrap();
        // (Each answer marked with its recording's place: the newest recording's answers come first.)
        let newest = all.iter().filter_map(|r| r["recording"].as_u64()).max().unwrap_or(0) + 1;
        let mut r: Vec<Value> = recording.lines().filter_map(|l| serde_json::from_str::<Value>(&replace.iter().fold(l.to_string(), |l, (from, to)| l.replace(from, to))).ok())
            .map(|mut v| { v["recording"] = newest.into(); v }).collect();
        r.extend(all.drain(..));
        *all = r;
        self.replayed.lock().unwrap().clear();
    }

    /// A parameter written as the dashboard writes one.
    pub fn put(&self, name: &str, value: &str) {
        let mut p = self.params.lock().unwrap();
        let version = p.get(name).map(|(_, v, _)| v + 1).unwrap_or(1);
        p.insert(name.to_string(), (value.to_string(), version, system_ms()));
    }

    /// A control plane's settings, by name, as it reads them (`/superci/<id>/<NAME>`).
    pub fn settings(&self, plane: &str) -> Vec<(String, String)> {
        let prefix = format!("/superci/{plane}/");
        self.params.lock().unwrap().iter().filter_map(|(k, (v, _, _))| Some((k.strip_prefix(&prefix)?.to_string(), v.clone()))).filter(|(k, _)| !k.contains('/')).collect()
    }

    /// One exchange: `url` is AWS's own (`https://ssm.us-east-1.amazonaws.com/`).
    pub fn handle(&self, method: &str, url: &str, target: &str, asked: &str) -> (u16, String) { let (s, _, a) = self.handle_full(method, url, target, asked); (s, a) }

    /// The same, with the answer's headers.
    pub fn handle_full(&self, method: &str, url: &str, target: &str, asked: &str) -> (u16, Vec<(String, String)>, String) {
        let (status, headers, answer) = match self.answer(url, target, asked) {
            (400, a) if a["__type"] == "SuperCITestkitUnknown" => self.replay(method, url, target, asked).unwrap_or((400, vec![], a.to_string())),
            (status, a) => (status, vec![], a.to_string()),
        };
        if status == 400 && answer.contains("SuperCITestkitUnknown") { self.unanswered.lock().unwrap().push(format!("{target} {url} {}", asked.chars().take(200).collect::<String>())) }
        self.asked.lock().unwrap().push(json!({ "method": method, "url": url, "target": target, "asked": asked, "status": status, "answer": answer }));
        (status, headers, answer)
    }

    /// The recorded answer to the same request, else to the same action at the same address.
    fn replay(&self, method: &str, url: &str, target: &str, asked: &str) -> Option<(u16, Vec<(String, String)>, String)> {
        let recorded = self.recorded.lock().unwrap();
        let at = |r: &&Value| r["url"] == url && r["target"] == target && r["method"] == method;
        // The same call (exactly, else the same action at the same address), the next of its answers in their order.
        let same: Vec<&Value> = recorded.iter().filter(|r| at(r) && r["asked"] == asked).collect();
        let same: Vec<&Value> = if same.is_empty() { recorded.iter().filter(|r| at(r) && action(r["target"].as_str().unwrap_or_default(), r["asked"].as_str().unwrap_or_default()) == action(target, asked)).collect() } else { same };
        // Of the newest recording that has it.
        let newest = same.iter().filter_map(|r| r["recording"].as_u64()).max().unwrap_or(0);
        let same: Vec<&Value> = same.into_iter().filter(|r| r["recording"].as_u64().unwrap_or(0) == newest).collect();
        let key = format!("{method} {url} {target} {}", action(target, asked));
        let mut given = self.replayed.lock().unwrap();
        let n = given.entry(key).or_insert(0);
        let found = same.get(*n).or(same.last())?;
        *n += 1;
        let answer = found["answer"].as_str()?.replace("http://plane.test", &self.plane_url.lock().unwrap());
        let headers = found["headers"].as_array().into_iter().flatten().filter_map(|h| Some((h[0].as_str()?.to_string(), h[1].as_str()?.to_string()))).collect();
        Some((found["status"].as_u64()? as u16, headers, answer))
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
        // AWS's sign-in refusing its last step, as it answered a real one whose account was changed on its page
        // midway (15 October 2026).
        if url.contains(".signin.aws.amazon.com/v1/token") {
            return (400, json!({ "error": "INVALID_REQUEST", "message": "The request is missing a required parameter, includes an invalid parameter value, or is otherwise malformed." }))
        }
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
            "AmazonSSM.DeleteParameters" => {
                let mut p = self.params.lock().unwrap();
                let (mut gone, mut not) = (vec![], vec![]);
                for n in a["Names"].as_array().into_iter().flatten().filter_map(|n| n.as_str()) { if p.remove(n).is_some() { gone.push(n) } else { not.push(n) } }
                (200, json!({ "DeletedParameters": gone, "InvalidParameters": not }))
            }
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
    fn post(&self, path: &str, body: Vec<u8>) -> u16 { self.exchange(path, body).0 }
    /// A call and what it answered: its status and its body.
    fn exchange(&self, path: &str, body: Vec<u8>) -> (u16, String) {
        let u = self.at.trim_start_matches("http://");
        let (host, rest) = u.split_once('/').unwrap();
        let mut s = TcpStream::connect(host).unwrap();
        s.write_all(format!("POST /{rest}{path} HTTP/1.1\r\nhost: {host}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n", body.len()).as_bytes()).unwrap();
        s.write_all(&body).unwrap();
        let mut out = String::new();
        let _ = s.read_to_string(&mut out);
        (out.get(9..12).and_then(|c| c.parse().ok()).unwrap_or(0), out.split_once("\r\n\r\n").map(|(_, b)| b.to_string()).unwrap_or_default())
    }
    /// It takes the job (asks for its message, as a runner does once it listens).
    pub fn take(&self) -> u16 { self.post("/Message/1", Vec::new()) }
    /// It asks for its next message, as a runner keeps doing while it runs its job: none (204), or one, as it came
    /// (`messageType`, and its `body`).
    pub fn message(&self) -> (u16, Value) {
        let (status, body) = self.exchange("/Message/1", Vec::new());
        (status, serde_json::from_str(&body).unwrap_or(Value::Null))
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

    /// The job is over, with its outputs (as its `outputs:` made them).
    pub fn finish_with(&self, result: &str, outputs: &[(&str, &str)]) -> u16 {
        let outputs: serde_json::Map<String, Value> = outputs.iter().map(|(k, v)| (k.to_string(), json!({ "value": v }))).collect();
        self.post("/FinishJob/scope/hub/plan", json!({ "result": result, "outputs": outputs }).to_string().into_bytes())
    }

    /// The runner of the job of run `run` shown as `name` (`build / compile`, `test (2)`), once it is started.
    pub fn of_job(served: &Served, run: &str, name: &str) -> Option<Runner> {
        let r = served.store.under(&format!("run:{run}")).into_iter().next()?.1;
        let token = r["jobs"].as_array()?.iter().find(|j| j["name"] == name && j["token"].is_string())?["token"].as_str()?.to_string();
        Some(Runner { at: format!("{}/runner/{token}/_apis/v1", served.url) })
    }

    /// What the runner was handed for its job (its message: steps, contexts).
    pub fn handed(&self, served: &Served) -> Value {
        let token = self.at.split("/runner/").nth(1).and_then(|r| r.split('/').next()).unwrap_or_default();
        served.store.under(&format!("rjob:{token}")).into_iter().next().map(|(_, v)| v["message"].clone()).unwrap_or_default()
    }
}

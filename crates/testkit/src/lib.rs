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
pub struct Served { pub plane: String, pub url: String, pub store: Shared, pub machines: Machines, pub aws: Aws, pub github: GitHub, pub gitlab: GitLab, pub key: String, pub run_key: String, clock: Now,
    /// The address the control plane is told it answers at, when not this computer's (`https://…`, as behind a
    /// deployed one's URL): its machines' links are made with it.
    pub public: Arc<Mutex<Option<String>>>,
    /// It stopped answering (its connections closed at once), until this is set back.
    pub down: Arc<std::sync::atomic::AtomicBool> }

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
        let status = out.get(9..12).and_then(|c| c.parse().ok()).unwrap_or(0);
        // (Kept among the App's deliveries, as GitHub keeps each.)
        self.github.delivered(&body, status, out.split_once("\r\n\r\n").map(|(_, b)| b).unwrap_or_default(), &format!("{}/webhook", self.url), self.clock.now_ms());
        status
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
    *github.aws.lock().unwrap() = Some(aws.clone());
    let (key, run_key) = ("dashboard-key-for-tests-0001".to_string(), "superci_run_0123456789abcdef0123456789abcdef".to_string());
    // Its keys, as a deploy and `superci keys create agent --runs` leave them.
    aws.put(&format!("/superci/{plane}/DEVICE_KEY_4102444800_TEST"), &key);
    aws.put(&format!("/superci/{plane}/READ_KEY_4102444800_AGENT"), &format!("{}:runs", superci_core::crypto::sha256_hex(run_key.as_bytes())));
    aws.put(&format!("/superci/{plane}/CONTAINERS"), "on");
    let clock = Now::default();
    let down = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let public: Arc<Mutex<Option<String>>> = Arc::default();
    let (s, m, a, u, c, g, id, dn, pb) = (store.clone(), machines.clone(), aws.clone(), url.clone(), clock.clone(), github.clone(), plane.to_string(), down.clone(), public.clone());
    std::thread::spawn(move || {
        let cache = Cache::default();
        block_on(s.put("plane_url", format!("\"{u}\""))).unwrap();
        for stream in listener.incoming().flatten() {
            if dn.load(std::sync::atomic::Ordering::SeqCst) { drop(stream); continue }
            let mut config = Config::new(id.clone());
            let mut settings = a.settings(&id);
            // (One from before reads its launchers and its devices' keys under their old names, and not the new ones.)
            if a.old_version.lock().unwrap().is_some() {
                settings = settings.into_iter().filter(|(k, _)| k != "LAUNCHERS" && !k.starts_with("DEVICE_KEY_"))
                    .map(|(k, v)| (if k == "AGENTS" { "LAUNCHERS".into() } else { k.replacen("DASHBOARD_KEY_", "DEVICE_KEY_", 1) }, v)).collect();
            }
            // (Its machines are its own containers, as a Cloudflare or Modal control plane's are: on until removed.)
            config.containers = settings.iter().any(|(k, v)| k == "CONTAINERS" && v == "on");
            config.read_settings(&settings);
            configure(&mut config);
            let plane = ControlPlane { store: &s, http: &g, clock: &c, timer: &NoTimer, config: &config, cache: &cache, containers: Some(&m) };
            let old = a.old_version.lock().unwrap().clone();
            let base = pb.lock().unwrap().clone().unwrap_or_else(|| u.clone());
            let _ = answer(&plane, &base, stream, old);
        }
    });
    let gitlab = github.gitlab.clone();
    Served { plane: plane.to_string(), url, store, machines, aws, github, gitlab, key, run_key, clock, down, public }
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
/// (One said to be of an older version answers as that version did.)
fn answer(plane: &ControlPlane, base: &str, stream: TcpStream, old: Option<String>) -> std::io::Result<()> {
    let (method, target, headers, body) = read(&stream)?;
    if target == "/__testkit/upkeep" {
        return match block_on(plane.alarm()) { Ok(()) => write(stream, 200, &[], b"ok"), Err(e) => write(stream, 500, &[], e.as_bytes()) };
    }
    let mut req = Request::new(&method, &format!("{base}{target}")).with_body(body);
    for (k, v) in headers { req = req.with_header(&k, &v) }
    let res = block_on(plane.handle(req));
    let body = match (old, target.split('?').next().unwrap_or_default(), serde_json::from_slice::<Value>(&res.body)) {
        (Some(version), "/health", Ok(mut v)) => { v["version"] = json!(version); v.to_string().into_bytes() }
        (Some(version), "/status", Ok(mut v)) => {
            if let Some(o) = v.as_object_mut() {
                let l = o.remove("launchers").unwrap_or_default();
                o.insert("agents".into(), l);
                // (What came after it, it does not say: limits for the whole CI and an account came in 0.15.0, a day's in 0.16.0.)
                let older = |than: &str| { let n = |s: &str| s.split('.').map(|p| p.parse::<u32>().unwrap_or(0)).collect::<Vec<_>>(); n(&version) < n(than) };
                if older("0.15.0") { o.remove("account_limits"); }
                if older("0.16.0") { o.remove("daily_limits"); }
            }
            v.to_string().into_bytes()
        }
        _ => res.body,
    };
    write(stream, res.status, &res.headers, &body)
}

/// A DictUpdateRequest's entries (`updates`, field 2: each a key, field 1, and a value, field 2), each pickled as
/// Modal's client pickles a string (protocol 4), as text.
fn modal_entries(b: &[u8]) -> Vec<(String, String)> {
    fn varint(b: &[u8], i: &mut usize) -> Option<u64> { let (mut n, mut shift) = (0u64, 0); loop { let c = *b.get(*i)?; *i += 1; n |= ((c & 0x7f) as u64) << shift; shift += 7; if c < 0x80 { return Some(n) } } }
    fn fields(b: &[u8]) -> Vec<(u64, Vec<u8>)> {
        let (mut i, mut out) = (0, vec![]);
        while i < b.len() {
            let Some(key) = varint(b, &mut i) else { break };
            match key & 7 {
                0 => { if varint(b, &mut i).is_none() { break } }
                2 => { let Some(n) = varint(b, &mut i) else { break }; let end = (i + n as usize).min(b.len()); out.push((key >> 3, b[i..end].to_vec())); i = end }
                _ => break,
            }
        }
        out
    }
    let unpickle = |p: &[u8]| -> String {
        // 0x80 0x04 0x95, 8 bytes of length; then 0x8c and one byte, or `X` and four, of the text's length; the text.
        let body = p.get(11..).unwrap_or_default();
        let (at, len) = match body.first() { Some(0x8c) => (2, *body.get(1).unwrap_or(&0) as usize), Some(b'X') => (5, u32::from_le_bytes(body.get(1..5).and_then(|s| s.try_into().ok()).unwrap_or([0; 4])) as usize), _ => (0, 0) };
        String::from_utf8_lossy(body.get(at..at + len).unwrap_or_default()).into_owned()
    };
    fields(b).into_iter().filter(|(f, _)| *f == 2).map(|(_, entry)| {
        let e = fields(&entry);
        let get = |n: u64| e.iter().find(|(f, _)| *f == n).map(|(_, v)| unpickle(v)).unwrap_or_default();
        (get(1), get(2))
    }).collect()
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
    /// The secrets written to each Worker (by its script's name), as Cloudflare keeps them for it: its settings.
    pub worker_secrets: Arc<Mutex<BTreeMap<String, BTreeMap<String, String>>>>,
    /// What Modal's settings Dicts were given (`DictUpdate`), by key: a Modal control plane's settings.
    pub modal_settings: Arc<Mutex<BTreeMap<String, String>>>,
    /// Workers on workers.dev, by host (`superci.acme.workers.dev`), and the control plane on this computer each stands
    /// for (its address).
    pub workers: Arc<Mutex<BTreeMap<String, String>>>,
    /// DynamoDB's items, by table, partition and sort key (`p`, `k`: an AWS control plane's table), as its item calls
    /// keep them.
    pub items: Arc<Mutex<BTreeMap<(String, String, String), Value>>>,
    /// Lambda functions called without waiting (`Event`): each function's name and what it was given.
    pub invoked: Arc<Mutex<Vec<(String, Value)>>>,
    /// A control plane not updated yet: while set, it says this version and answers as one from before 0.19.13 did
    /// (its launchers as `agents`). Its function's code uploaded again (an update) ends it.
    pub old_version: Arc<Mutex<Option<String>>>,
    /// Calls answered with an error, by action (`RunInstances`, `AmazonSSM.PutParameter`, …, or `Action|address part`):
    /// its status and answer, and how many more times (none: every time). Written as AWS documents its errors, for what
    /// a test cannot have a real account answer (throttling, a fault of AWS's, an organization's policy).
    pub failing: Arc<Mutex<Vec<(String, u16, String, Option<u32>)>>>,
}

/// EC2's error answer (and IAM's, STS's: the query APIs'), as AWS documents it.
pub fn ec2_error(code: &str, message: &str) -> String {
    format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<Response><Errors><Error><Code>{code}</Code><Message>{message}</Message></Error></Errors><RequestID>00000000-0000-4000-8000-00000000e001</RequestID></Response>")
}

/// A JSON API's error answer (SSM, DynamoDB, Lambda, …), as AWS documents it.
pub fn json_error(kind: &str, message: &str) -> String { json!({ "__type": kind, "message": message }).to_string() }

/// What a request asks AWS to do: its target (JSON APIs) or its Action (query APIs).
fn action(target: &str, asked: &str) -> String {
    if !target.is_empty() { return target.to_string() }
    asked.split('&').find_map(|kv| kv.strip_prefix("Action=")).unwrap_or_default().to_string()
}

impl Aws {
    /// Modal's answer to one call (`method`, its request's message), as a recording has it: each answer's message, or
    /// Modal's refusal as a gRPC status (its code and message).
    fn modal_answer(&self, method: &str, message: &[u8]) -> (Vec<Vec<u8>>, Option<(u32, String)>) {
        use base64::Engine;
        let b64 = base64::engine::general_purpose::STANDARD;
        let url = format!("https://api.modal.com/modal.client.ModalClient/{method}");
        let asked = b64.encode(message);
        // (What a Modal settings Dict is given, kept: a Modal control plane served here reads it.)
        if method == "DictUpdate" { for (k, v) in modal_entries(message) { self.modal_settings.lock().unwrap().insert(k, v); } }
        let (status, _, answer) = self.replay("POST", &url, "", &asked).unwrap_or_else(|| {
            self.unanswered.lock().unwrap().push(format!("POST {url}"));
            (404, vec![], json!({ "error": format!("Modal {method}: the test kit's Modal has no answer for it") }).to_string())
        });
        self.asked.lock().unwrap().push(json!({ "method": "POST", "url": url, "target": "", "asked": asked, "status": status, "answer": answer }));
        let v: Value = serde_json::from_str(&answer).unwrap_or_default();
        // (A refusal is recorded as the CLI said it: "Modal METHOD: …", or a sign-in Modal no longer takes, code 16.)
        if let Some(e) = v["error"].as_str() {
            let said = format!("Modal {method}: ");
            return match e.find(&format!("(Modal {method}: ")) {
                Some(at) if e.contains("no longer accepted") => (vec![], Some((16, e[at + 1 + said.len()..].trim_end_matches(')').to_string()))),
                _ => (vec![], Some((2, e.strip_prefix(&said).unwrap_or(e).to_string()))),
            }
        }
        (v["answers"].as_array().into_iter().flatten().filter_map(|a| b64.decode(a.as_str()?).ok()).collect(), None)
    }

    /// One HTTP/2 connection, each request a gRPC call to Modal (`modal_answer`): its message out of gRPC's frame, each
    /// answer framed, then its status in the trailers.
    fn serve_grpc(&self, stream: TcpStream) {
        let Ok(rt) = tokio::runtime::Builder::new_current_thread().enable_all().build() else { return };
        let a = self.clone();
        rt.block_on(async move {
            if stream.set_nonblocking(true).is_err() { return }
            let Ok(s) = tokio::net::TcpStream::from_std(stream) else { return };
            let Ok(mut conn) = h2::server::handshake(s).await else { return };
            while let Some(Ok((req, mut respond))) = conn.accept().await {
                let a = a.clone();
                tokio::spawn(async move {
                    let method = req.uri().path().rsplit('/').next().unwrap_or_default().to_string();
                    let mut body = req.into_body();
                    let mut data = vec![];
                    while let Some(Ok(chunk)) = body.data().await { let _ = body.flow_control().release_capacity(chunk.len()); data.extend_from_slice(&chunk); }
                    // (gRPC's frame: a flag, four bytes of length, the message.)
                    let (answers, refused) = a.modal_answer(&method, data.get(5..).unwrap_or_default());
                    let Ok(mut send) = respond.send_response(http::Response::builder().status(200).header("content-type", "application/grpc").body(()).unwrap(), false) else { return };
                    for m in &answers {
                        let mut f = vec![0u8];
                        f.extend((m.len() as u32).to_be_bytes());
                        f.extend(m);
                        let _ = send.send_data(bytes::Bytes::from(f), false);
                    }
                    let mut trailers = http::HeaderMap::new();
                    let (code, message) = refused.unwrap_or((0, String::new()));
                    trailers.insert("grpc-status", code.to_string().parse().unwrap());
                    // (Its message percent-encoded where gRPC asks it to be.)
                    let encoded: String = message.bytes().map(|b| if (0x20..0x7f).contains(&b) && b != b'%' { (b as char).to_string() } else { format!("%{b:02X}") }).collect();
                    if code != 0 { if let Ok(v) = encoded.parse() { trailers.insert("grpc-message", v); } }
                    let _ = send.send_trailers(trailers);
                });
            }
        });
    }

    pub fn serve() -> Aws {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let recorded = include_str!("../../cli/fixtures/aws/settings/dashboard.jsonl").lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
        let aws = Aws { url: format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port()), recorded: Arc::new(Mutex::new(recorded)), ..Default::default() };
        let a = aws.clone();
        std::thread::spawn(move || for stream in listener.incoming().flatten() {
            // HTTP/2 (its preface first): Modal's API over gRPC, as the CLI calls it.
            let mut first = [0u8; 3];
            if stream.peek(&mut first).is_ok_and(|n| n == 3) && &first == b"PRI" { let a = a.clone(); std::thread::spawn(move || a.serve_grpc(stream)); continue }
            let Ok((method, target, headers, body)) = read(&stream) else { continue };
            let header = |n: &str| headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(n)).map(|(_, v)| v.clone()).unwrap_or_default();
            let host = target.split('/').nth(1).unwrap_or_default().to_string();
            // Cloudflare's API and its container registry, GitHub's (where its runner image is), and Modal's API (its
            // calls as the CLI sends them to a stand-in), as a recording has them (what is not there is unanswered).
            if ["api.cloudflare.com", "dash.cloudflare.com", "registry.cloudflare.com", "ghcr.io"].contains(&host.as_str()) {
                let url = format!("https:/{target}");
                let asked = String::from_utf8_lossy(&body).to_string();
                // (A Worker's secrets kept as written: what a control plane served here reads as its settings.)
                let path = url.split('?').next().unwrap_or_default().to_string();
                if let Some((script, rest)) = path.split_once("/workers/scripts/").map(|(_, r)| r.split_once('/').unwrap_or((r, ""))) {
                    let mut kept = a.worker_secrets.lock().unwrap();
                    match (method.as_str(), rest) {
                        ("PUT", "secrets") => if let Ok(v) = serde_json::from_str::<Value>(&asked) { if let (Some(n), Some(t)) = (v["name"].as_str(), v["text"].as_str()) { kept.entry(script.to_string()).or_default().insert(n.to_string(), t.to_string()); } },
                        ("DELETE", r) if r.starts_with("secrets/") => { if let Some(m) = kept.get_mut(script) { m.remove(&r["secrets/".len()..]); } }
                        ("DELETE", "") => { kept.remove(script); }
                        _ => {}
                    }
                }
                // (A Worker's secret written or taken out, with no recording of it: answered as Cloudflare answers.)
                let secret_op = path.contains("/workers/scripts/") && path.contains("/secrets") && ["PUT", "DELETE"].contains(&method.as_str());
                let (status, said, answer) = a.failed(&url, "", &asked).or_else(|| a.replay(&method, &url, "", &asked))
                    .or_else(|| secret_op.then(|| (200, vec![], json!({ "success": true, "errors": [], "messages": [], "result": null }).to_string()))).unwrap_or_else(|| {
                    a.unanswered.lock().unwrap().push(format!("{method} {url}"));
                    (404, vec![], json!({ "success": false, "errors": [{ "code": 0, "message": format!("the test kit's Cloudflare has no answer for {method} {url}") }] }).to_string())
                });
                a.asked.lock().unwrap().push(json!({ "method": method, "url": url, "target": "", "asked": asked, "status": status, "answer": answer }));
                let said = if said.iter().any(|(k, _)| k.eq_ignore_ascii_case("content-type")) { said } else { vec![("content-type".into(), "application/json".into())] };
                let _ = write(stream, status, &said, answer.as_bytes());
                continue;
            }
            // A Worker on workers.dev: the control plane on this computer it stands for.
            if let Some(to) = a.workers.lock().unwrap().get(&host).cloned() {
                let rest = target.splitn(3, '/').nth(2).unwrap_or_default();
                let u = to.trim_start_matches("http://");
                let (h, _) = u.split_once('/').unwrap_or((u, ""));
                let mut out = vec![];
                if let Ok(mut s) = TcpStream::connect(h) {
                    let extra: String = headers.iter().filter(|(k, _)| !["host", "content-length", "connection"].contains(&k.to_ascii_lowercase().as_str())).map(|(k, v)| format!("{k}: {v}\r\n")).collect();
                    let _ = s.write_all(format!("{method} /{rest} HTTP/1.1\r\nhost: {h}\r\n{extra}content-length: {}\r\nconnection: close\r\n\r\n", body.len()).as_bytes());
                    let _ = s.write_all(&body);
                    let _ = s.read_to_end(&mut out);
                }
                let mut stream = stream;
                let _ = stream.write_all(&out);
                continue;
            }
            // (S3's objects are kept by GitHub's stand-in, which the control plane reaches AWS through; a call to S3 a
            // recording has, at that very address, is answered as recorded.)
            let s3 = host.contains(".s3.");
            if s3 {
                let url = format!("https:/{target}");
                if let Some((status, said, answer)) = a.replay(&method, &url, "", &String::from_utf8_lossy(&body)).filter(|_| a.recorded.lock().unwrap().iter().any(|r| r["url"] == url.as_str() && r["method"] == method.as_str())) {
                    a.asked.lock().unwrap().push(json!({ "method": method, "url": url, "target": "", "asked": String::from_utf8_lossy(&body), "status": status, "answer": answer }));
                    let _ = write(stream, status, &said, answer.as_bytes());
                    continue;
                }
            }
            if (!host.ends_with(".amazonaws.com") && !host.ends_with(".signin.aws.amazon.com")) || s3 {
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

    /// What Modal's settings Dicts were given, as a Modal control plane reads them (its settings; an empty value is
    /// one taken out).
    pub fn modal_plane_settings(&self) -> Vec<(String, String)> { self.modal_settings.lock().unwrap().iter().filter(|(_, v)| !v.is_empty()).map(|(k, v)| (k.clone(), v.clone())).collect() }

    /// A Worker's secrets, as a control plane there reads them (its settings).
    pub fn worker_settings(&self, script: &str) -> Vec<(String, String)> { self.worker_secrets.lock().unwrap().get(script).map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect()).unwrap_or_default() }

    /// The answer `fail` set for this call, if one was (counted once more).
    fn failed(&self, url: &str, target: &str, asked: &str) -> Option<(u16, Vec<(String, String)>, String)> {
        let mut f = self.failing.lock().unwrap();
        let doing = action(target, asked);
        // (`Action|part of the address`: that action there only, `RunInstances|ec2.eu-west-1`; `DryRun=true` and the
        // like match what was asked.)
        let i = f.iter().position(|(a, ..)| a.split('|').all(|p| p == doing || url.contains(p) || asked.contains(p)))?;
        let got = (f[i].1, vec![], f[i].2.clone());
        match &mut f[i].3 { Some(1) => { f.remove(i); } Some(n) => *n -= 1, None => {} }
        Some(got)
    }

    /// `action` answered with this error from now on: `times` times, or every time.
    pub fn fail(&self, action: &str, status: u16, answer: &str, times: Option<u32>) { self.failing.lock().unwrap().push((action.to_string(), status, answer.to_string(), times)) }

    /// No call answered with an error any more (each answered as before `fail`).
    pub fn fail_none(&self) { self.failing.lock().unwrap().clear() }

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
        // (AWS's own public parameters, `/aws/service/…`, are not the account's: as a recording has them.)
        if let Some((status, headers, answer)) = self.failed(url, target, asked) {
            self.asked.lock().unwrap().push(json!({ "method": method, "url": url, "target": target, "asked": asked, "status": status, "answer": answer }));
            return (status, headers, answer)
        }
        let public = target == "AmazonSSM.GetParameter" && asked.contains("\"/aws/service/");
        let recorded = if public { self.replay(method, url, target, asked) } else { None };
        let (status, headers, answer) = match recorded {
            Some(r) => r,
            None => match self.answer(url, target, asked) {
                (400, a) if a["__type"] == "SuperCITestkitUnknown" => self.replay(method, url, target, asked).unwrap_or((400, vec![], a.to_string())),
                (status, a) => (status, vec![], a.to_string()),
            },
        };
        if method == "PUT" && url.contains("/functions/superci-plane-") && url.ends_with("/code") && status == 200 { *self.old_version.lock().unwrap() = None }
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
        // (Of those, the ones asked most alike, by the names of what was asked: a spot request's answer for a spot
        // request, an on-demand one's for an on-demand one.)
        let names = |a: &str| -> std::collections::BTreeSet<String> { a.split('&').filter_map(|kv| kv.split('=').next()).map(|k| k.chars().filter(|c| !c.is_ascii_digit()).collect()).collect() };
        let want = names(asked);
        let alike = |r: &&Value| names(r["asked"].as_str().unwrap_or_default()).intersection(&want).count() as i64 - names(r["asked"].as_str().unwrap_or_default()).symmetric_difference(&want).count() as i64;
        let best = same.iter().map(alike).max().unwrap_or(0);
        let same: Vec<&Value> = if asked.contains('&') { same.into_iter().filter(|r| alike(r) == best).collect() } else { same };
        // (Counted apart for each kind of the same call: a renewal's answers in their order, a first sign-in's in theirs.)
        let kind = same.first().map(|r| names(r["asked"].as_str().unwrap_or_default()).into_iter().collect::<Vec<_>>().join(",")).unwrap_or_default();
        let key = format!("{method} {url} {target} {} {kind}", action(target, asked));
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
        // (A recording of a sign-in that finished, and of its renewals, answers instead when one is played.)
        if url.contains(".signin.aws.amazon.com/v1/token") && !self.recorded.lock().unwrap().iter().any(|r| r["url"] == url) {
            return (400, json!({ "error": "INVALID_REQUEST", "message": "The request is missing a required parameter, includes an invalid parameter value, or is otherwise malformed." }))
        }
        // Lambda, a function called without waiting for it (the control plane's own sweep, its converter).
        if let Some(function) = url.split("/2015-03-31/functions/").nth(1).and_then(|f| f.strip_suffix("/invocations")) {
            self.invoked.lock().unwrap().push((function.to_string(), a));
            return (202, json!({}))
        }
        if let Some(op) = target.strip_prefix("DynamoDB_20120810.") { if let Some(answered) = self.dynamo(op, &a) { return answered } }
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
            "AmazonSSM.GetParameters" => {
                let p = self.params.lock().unwrap();
                let (mut found, mut not) = (vec![], vec![]);
                for n in a["Names"].as_array().into_iter().flatten().filter_map(|n| n.as_str()) { match p.get(n) { Some(v) => found.push(one(n, v, a["WithDecryption"] == true)), None => not.push(n) } }
                (200, json!({ "Parameters": found, "InvalidParameters": not }))
            }
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

impl Aws {
    /// DynamoDB's item calls, as DynamoDB answers them (a page of a query is 25 items at most, so callers page); none
    /// for its other calls (a table made or deleted: as recorded).
    fn dynamo(&self, op: &str, a: &Value) -> Option<(u16, Value)> {
        let table = a["TableName"].as_str().unwrap_or_default().to_string();
        let key = |table: &str, k: &Value| (table.to_string(), k["p"]["S"].as_str().unwrap_or_default().to_string(), k["k"]["S"].as_str().unwrap_or_default().to_string());
        let failed = |kind: &str| (400, json!({ "__type": format!("com.amazonaws.dynamodb.v20120810#{kind}"), "message": "The conditional request failed" }));
        let mut items = self.items.lock().unwrap();
        Some(match op {
            "GetItem" => (200, items.get(&key(&table, &a["Key"])).map(|i| json!({ "Item": i })).unwrap_or(json!({}))),
            "PutItem" => {
                let at = key(&table, &a["Item"]);
                let held = items.get(&at);
                let allowed = match a["ConditionExpression"].as_str() {
                    Some("attribute_not_exists(k)") => held.is_none(),
                    Some("v = :was") => held.is_some_and(|h| h["v"] == a["ExpressionAttributeValues"][":was"]),
                    _ => true,
                };
                if !allowed { return Some(failed("ConditionalCheckFailedException")) }
                items.insert(at, a["Item"].clone());
                (200, json!({}))
            }
            "DeleteItem" => { items.remove(&key(&table, &a["Key"])); (200, json!({})) }
            "Query" => {
                let vals = &a["ExpressionAttributeValues"];
                let (p, prefix) = (vals[":p"]["S"].as_str().unwrap_or_default(), vals[":k"]["S"].as_str().unwrap_or(""));
                let mut hits: Vec<&Value> = items.iter().filter(|((t, ip, ik), _)| *t == table && ip == p && ik.starts_with(prefix)).map(|(_, i)| i).collect();
                if a["ScanIndexForward"] == false { hits.reverse() }
                let start = a["ExclusiveStartKey"]["k"]["S"].as_str().map(|after| hits.iter().position(|i| i["k"]["S"] == after).map(|x| x + 1).unwrap_or(0)).unwrap_or(0);
                let page = a["Limit"].as_u64().unwrap_or(25).min(25) as usize;
                let shown: Vec<Value> = hits.iter().skip(start).take(page).map(|i| (*i).clone()).collect();
                let mut out = json!({ "Items": shown, "Count": shown.len() });
                if start + page < hits.len() { out["LastEvaluatedKey"] = json!({ "p": { "S": p }, "k": shown.last().map(|i| i["k"].clone()).unwrap_or_default() }) }
                (200, out)
            }
            "BatchGetItem" => {
                let mut responses = serde_json::Map::new();
                for (t, ask) in a["RequestItems"].as_object().into_iter().flatten() {
                    let found: Vec<Value> = ask["Keys"].as_array().into_iter().flatten().filter_map(|k| items.get(&key(t, k)).cloned()).collect();
                    responses.insert(t.clone(), Value::Array(found));
                }
                (200, json!({ "Responses": responses, "UnprocessedKeys": {} }))
            }
            _ => return None,
        })
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

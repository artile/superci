//! A SuperCI Worker as Cloudflare runs it: the module `./build.sh` makes, under `wrangler dev` (workerd, the runtime
//! Cloudflare runs it in, on this computer), its Durable Objects' storage and alarms as Cloudflare keeps them. What it
//! fetches from outside is answered by the test kit's stand-ins, and each runner's container by the test
//! (`worker-shim.mjs`: nothing of the Worker's own is changed). Where Node is and the Worker is built.
use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::sweep::{Fault, Faults};
use crate::{Aws, GitHub};

/// The wrangler that runs the tests (its workerd: Cloudflare's runtime, as of this version).
const WRANGLER: &str = "wrangler@4.149.0";

fn free_port() -> u16 { TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port() }

/// One HTTP request, its connection closed after: the status and the body.
pub fn http(url: &str, method: &str, path: &str, headers: &[(&str, &str)], body: &[u8]) -> (u16, String) {
    let host = url.trim_start_matches("http://");
    let mut s = TcpStream::connect(host).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(60))).unwrap();
    let extra: String = headers.iter().map(|(k, v)| format!("{k}: {v}\r\n")).collect();
    s.write_all(format!("{method} {path} HTTP/1.1\r\nhost: {host}\r\n{extra}content-length: {}\r\nconnection: close\r\n\r\n", body.len()).as_bytes()).unwrap();
    s.write_all(body).unwrap();
    let mut out = Vec::new();
    let _ = s.read_to_end(&mut out);
    let out = String::from_utf8_lossy(&out).into_owned();
    let status = out.get(9..12).and_then(|c| c.parse().ok()).unwrap_or(0);
    let (head, body) = out.split_once("\r\n\r\n").unwrap_or((&out, ""));
    // (Chunked, as workerd sends a body it streams: its chunks put together.)
    let body = if head.to_ascii_lowercase().contains("transfer-encoding: chunked") { unchunk(body) } else { body.to_string() };
    (status, body)
}

fn unchunk(mut s: &str) -> String {
    let mut out = String::new();
    while let Some((size, rest)) = s.split_once("\r\n") {
        let n = usize::from_str_radix(size.trim(), 16).unwrap_or(0);
        if n == 0 { break }
        out.push_str(&rest[..n.min(rest.len())]);
        s = rest.get(n + 2..).unwrap_or_default();
    }
    out
}

/// The runners' containers, as the test has them: each operation the Worker asked (`start`, `keep`, `destroy`) with
/// what it gave, and how each goes (done, unless an answer for it was queued: `{"throw": "why"}`). Each counted as a
/// call among the stand-ins' (`Faults`): a sweep fails it (Cloudflare's container service failing) or drops it.
#[derive(Clone)]
pub struct Containers { pub url: String, ops: Arc<Mutex<Vec<(String, Value)>>>, answers: Arc<Mutex<VecDeque<(String, Value)>>> }

impl Containers {
    fn serve(faults: Faults) -> Containers {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let c = Containers { url: format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port()), ops: Arc::default(), answers: Arc::default() };
        let me = c.clone();
        std::thread::spawn(move || for stream in listener.incoming().flatten() {
            let mut r = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            if r.read_line(&mut line).is_err() { continue }
            let op = line.split_whitespace().nth(1).unwrap_or_default().trim_start_matches('/').to_string();
            let mut len = 0;
            loop {
                let mut h = String::new();
                if r.read_line(&mut h).is_err() || h.trim().is_empty() { break }
                if let Some((k, v)) = h.split_once(':') { if k.eq_ignore_ascii_case("content-length") { len = v.trim().parse().unwrap_or(0) } }
            }
            let mut body = vec![0; len];
            let _ = r.read_exact(&mut body);
            me.ops.lock().unwrap().push((op.clone(), serde_json::from_slice(&body).unwrap_or_default()));
            let answer = match faults.call(format!("container {op}")) {
                Some(true) => continue,
                Some(false) => json!({ "throw": "Cloudflare's container service failed" }),
                None => { let mut a = me.answers.lock().unwrap(); a.iter().position(|(o, _)| *o == op).and_then(|i| a.remove(i)).map(|(_, v)| v).unwrap_or(json!({})) }
            };
            let answer = answer.to_string();
            let mut stream = stream;
            let _ = stream.write_all(format!("HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{answer}", answer.len()).as_bytes());
        });
        c
    }

    /// The next `op` goes as `answer` says.
    pub fn answer(&self, op: &str, answer: Value) { self.answers.lock().unwrap().push_back((op.to_string(), answer)) }

    pub fn ops(&self, op: &str) -> Vec<Value> { self.ops.lock().unwrap().iter().filter(|(o, _)| o == op).map(|(_, v)| v.clone()).collect() }
}

/// What a Worker is: the module worker-build made (`build/index.js`), its Durable Objects' bindings and migrations
/// (`wrangler.toml` lines), its plain settings (`[vars]` lines).
pub struct Setup<'a> { pub built: PathBuf, pub bindings: &'a str, pub vars: &'a str }

/// A Worker running under `wrangler dev`, with its settings as its secrets; the stand-ins it reaches.
pub struct Worker { child: Child, pub url: String, dir: PathBuf, pub aws: Aws, pub github: GitHub, pub containers: Containers }

impl Drop for Worker {
    fn drop(&mut self) {
        // (wrangler and the workerd it started, as one group.)
        unsafe { libc::kill(-(self.child.id() as i32), libc::SIGTERM); }
        let until = Instant::now() + Duration::from_secs(5);
        while Instant::now() < until { if let Ok(Some(_)) = self.child.try_wait() { break } std::thread::sleep(Duration::from_millis(50)) }
        unsafe { libc::kill(-(self.child.id() as i32), libc::SIGKILL); }
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

impl Worker {
    /// Started with these settings (as its secrets), where Node and the built Worker are (`None`, said, elsewhere).
    pub fn start(setup: &Setup, settings: &[(String, String)]) -> Option<Worker> {
        if !setup.built.exists() { eprintln!("the Worker is not built (./build.sh): skipped"); return None }
        if !Command::new("npx").arg("--version").output().is_ok_and(|o| o.status.success()) { eprintln!("no Node here: skipped"); return None }
        let aws = Aws::serve();
        let github = GitHub::default();
        *aws.github.lock().unwrap() = Some(github.clone());
        *github.aws.lock().unwrap() = Some(aws.clone());
        let containers = Containers::serve(aws.faults.clone());
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!("superci-worker-{}-{}", std::process::id(), N.fetch_add(1, std::sync::atomic::Ordering::SeqCst)));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut shim = include_str!("worker-shim.mjs").replace("\"BUILT\"", &format!("{:?}", setup.built.display().to_string()));
        // (The control plane's Durable Object only for a Worker that has it: an export that is none is refused.)
        if !std::fs::read_to_string(&setup.built).unwrap_or_default().contains(" as PlaneObject") {
            let (before, rest) = shim.split_once("// PLANE {").unwrap();
            shim = format!("{before}{}", rest.split_once("// } PLANE").unwrap().1);
        }
        std::fs::write(dir.join("shim.mjs"), shim).unwrap();
        std::fs::write(dir.join("wrangler.toml"), format!("name = \"superci-test\"\nmain = \"shim.mjs\"\ncompatibility_date = \"2026-09-01\"\n\n{}\n[vars]\nSUPERCI_STAND_IN = \"{}\"\nSUPERCI_CONTAINERS = \"{}\"\n{}\n",
            setup.bindings, aws.url, containers.url, setup.vars)).unwrap();
        // (Its secrets, as `wrangler dev` reads them beside its settings.)
        let vars: String = settings.iter().map(|(k, v)| { assert!(!v.contains('\'')); format!("{k}='{v}'\n") }).collect();
        std::fs::write(dir.join(".dev.vars"), vars).unwrap();
        // (Its ports picked free, then taken by wrangler: one taken meanwhile by another test's, started again on others.)
        let mut tries = 0;
        let w = loop {
            let port = free_port();
            let log = std::fs::File::create(dir.join("log")).unwrap();
            use std::os::unix::process::CommandExt;
            let child = Command::new("npx").args(["-y", WRANGLER, "dev", "--local", "--ip", "127.0.0.1", "--port", &port.to_string(), "--inspector-port", &free_port().to_string(), "--persist-to"])
                .arg(dir.join("state")).current_dir(&dir).env("WRANGLER_SEND_METRICS", "false").env("NO_COLOR", "1").env("CI", "1")
                .stdin(Stdio::null()).stdout(log.try_clone().unwrap()).stderr(log).process_group(0).spawn().unwrap();
            let url = format!("http://127.0.0.1:{port}");
            let w = Worker { child, url, dir: dir.clone(), aws: aws.clone(), github: github.clone(), containers: containers.clone() };
            let until = Instant::now() + Duration::from_secs(120);
            while !w.log().contains("Ready on") && !w.log().contains("Address already in use") { assert!(Instant::now() < until, "wrangler did not start:\n{}", w.log()); std::thread::sleep(Duration::from_millis(200)) }
            if !w.log().contains("Address already in use") { break w }
            tries += 1;
            assert!(tries < 5, "wrangler found no port free:\n{}", w.log());
            // (Ended without its folder going: the next start keeps it.)
            let mut w = w;
            w.dir = PathBuf::new();
        };
        *w.aws.plane_url.lock().unwrap() = w.url.clone();
        Some(w)
    }

    /// What wrangler and the Worker said.
    pub fn log(&self) -> String { std::fs::read_to_string(self.dir.join("log")).unwrap_or_default() }

    pub fn get(&self, path: &str) -> (u16, String) { http(&self.url, "GET", path, &[], b"") }

    /// A request with these headers.
    pub fn call(&self, method: &str, path: &str, headers: &[(&str, &str)], body: &[u8]) -> (u16, String) { http(&self.url, method, path, headers, body) }

    /// How Cloudflare's containers are from now on (see the shim's `mode`): `{}` as they should be.
    pub fn containers_are(&self, mode: Value) { assert_eq!(http(&self.url, "POST", "/__test/containers", &[], mode.to_string().as_bytes()).0, 200) }

    /// A runner asked directly, as the Worker asks it (`start`, `stop`): its answer.
    pub fn runner(&self, name: &str, what: &str, body: Value) -> (u16, String) { http(&self.url, "POST", &format!("/__test/runner?name={name}&what={what}"), &[], body.to_string().as_bytes()) }

    /// A Durable Object's alarm, run now: a runner's (its time bound), or the control plane's (its upkeep).
    pub fn alarm(&self, runner: Option<&str>) -> (u16, String) {
        match runner { Some(name) => self.get(&format!("/__test/runner-alarm?name={name}")), None => self.get("/__test/plane-alarm") }
    }

    /// Waits for `what` to hold, saying the Worker's log when it does not.
    pub fn until(&self, what: &str, f: impl Fn() -> bool) {
        let until = Instant::now() + Duration::from_secs(30);
        while !f() { assert!(Instant::now() < until, "{what}:\n{}", self.log()); std::thread::sleep(Duration::from_millis(100)) }
    }
}

/// What a run broke that must hold whatever fails: the Worker panicking (wasm's `unreachable`), the runtime saying
/// what it did not catch.
fn broke(w: &Worker) -> Vec<String> {
    w.log().lines().filter(|l| l.contains("panicked at") || l.contains("RuntimeError") || l.contains("Uncaught")).map(str::to_string).collect()
}

/// A Worker's scenario run clean, then once for each call it made (to the stand-ins, and to its runners' containers),
/// that one failing (answered with its service's error, and dropped), a Worker of its own each time: held to what must
/// hold whatever fails (nothing panics; and the scenario's own checks: what it returns). The clean run alone unless
/// `SUPERCI_SWEEP_FAULTS=1`, and this run's slice of the faults (`SUPERCI_SWEEP_SLICE`), as every sweep.
pub fn sweep(name: &str, start: impl Fn() -> Option<Worker>, scenario: impl Fn(&Worker) -> Vec<String>) {
    let run = |fault: Fault| -> Option<(Vec<String>, usize)> {
        let w = start()?;
        w.aws.faults.arm(fault);
        let mut problems = scenario(&w);
        problems.extend(broke(&w));
        Some((problems, w.aws.faults.calls().len()))
    };
    let Some((problems, calls)) = run(Fault::None) else { return };
    assert!(problems.is_empty(), "{name}, clean: {}", problems.join("\n  "));
    if std::env::var("SUPERCI_SWEEP_FAULTS").ok().as_deref() != Some("1") { return }
    let (k0, n) = crate::sweep::slice();
    let faults: Vec<Fault> = (1..=calls).flat_map(|k| [Fault::Call(k), Fault::Dropped(k)]).enumerate().filter(|(i, _)| i % n == k0 - 1).map(|(_, f)| f).collect();
    let mut broken = vec![];
    for f in faults {
        let Some((problems, _)) = run(f) else { return };
        if !problems.is_empty() { broken.push(format!("{f}: {}", problems.join("\n    "))) }
    }
    assert!(broken.is_empty(), "{name}: {} of the faults broke it:\n  {}", broken.len(), broken.join("\n  "));
}

/// `build/index.js` of a crate (`CARGO_MANIFEST_DIR`).
pub fn built(crate_dir: &str) -> PathBuf { Path::new(crate_dir).join("build/index.js") }

/// A runner's container, each way Cloudflare's may go (the same runner in the control plane's Worker and in the
/// launcher's), asked as its Worker asks it: nothing to run; started, sized, kept running, started again, ended by its
/// time bound and when stopped; GitLab's runner; a start Cloudflare refuses however it says so; kept running only on
/// the fourth try, or not at all; answered at once rather than as a promise; no runner image; no container at all;
/// the class kept from before 0.5.0.
pub fn each_way_a_container_goes(w: &Worker) {
    let job = |extra: Value| { let mut b = json!({ "jit": "eyJqaXQiOnRydWV9", "max_minutes": 30, "cpu": 2, "ram_gb": 8, "disk_gb": 20 }); for (k, v) in extra.as_object().unwrap() { b[k] = v.clone() } b };
    // Nothing to run: refused.
    assert_eq!(w.runner("r0", "start", json!({})).0, 400);
    // Started: sized, its full image and the reason it is to fail in its environment; kept running for its time (at
    // most Cloudflare's six hours); started again, the one there first ended.
    let (status, said) = w.runner("r1", "start", job(json!({ "image_url": "https://images.example.com/full/0123456789abcdef", "fail": "a GPU is not here", "max_minutes": 500 })));
    assert_eq!(status, 200, "{said}");
    let start = w.containers.ops("start").pop().unwrap();
    assert!(start["options"]["env"]["SUPERCI_IMAGE"].as_str().is_some() && start["options"]["env"]["SUPERCI_FAIL"] == "a GPU is not here" && start["options"]["instance"]["memoryMib"] == 8192, "{start}");
    assert_eq!(w.containers.ops("keep").pop().unwrap()["ms"], 360 * 60_000);
    w.containers.answer("destroy", json!({ "throw": "already gone" }));
    assert_eq!(w.runner("r1", "start", job(json!({}))).0, 200);
    assert_eq!(w.containers.ops("destroy").pop().unwrap()["reason"], "starting again");
    // Its time bound: what runs ends; stopped: ended once, then nothing more to end.
    assert_eq!(w.alarm(Some("r1")).0, 200);
    assert_eq!(w.containers.ops("destroy").pop().unwrap()["reason"], "time bound");
    assert_eq!(w.alarm(Some("r1")).0, 200);
    assert_eq!(w.runner("r1", "start", job(json!({}))).0, 200);
    assert_eq!(w.runner("r1", "stop", json!({})).0, 200);
    let ends = w.containers.ops("destroy").len();
    assert_eq!(w.runner("r1", "stop", json!({})).0, 200);
    assert_eq!(w.containers.ops("destroy").len(), ends, "nothing more to end");
    // A GitLab job's runner: its own script and settings.
    assert_eq!(w.runner("r2", "start", json!({ "gitlab": { "url": "https://gitlab.com", "token": "glrt-0123" }, "max_minutes": 30 })).0, 200);
    let start = w.containers.ops("start").pop().unwrap();
    assert!(start["options"]["env"]["GL_TOKEN"] == "glrt-0123" && start["options"]["env"]["GL_RUNNER_FLAGS"].as_str().is_some(), "{start}");
    // A start Cloudflare refuses, however it says so: said, with what it said.
    for (answer, said) in [(json!({ "throw": "image not found" }), "image not found"), (json!({ "throw": "quota", "as": "string" }), "quota"),
        (json!({ "throw": "x", "as": "object" }), "\"said\""), (json!({ "throw": "x", "as": "nothing" }), "an error without a message")] {
        w.containers.answer("start", answer);
        let (status, text) = w.runner("r3", "start", job(json!({})));
        assert!(status == 502 && text.contains("the container did not start") && text.contains(said), "{status} {text}");
    }
    // Kept running only on the fourth try (Cloudflare busy right after a start); not at all; refused at once.
    for _ in 0..3 { w.containers.answer("keep", json!({ "throw": "temporarily unavailable" })) }
    assert_eq!(w.runner("r4", "start", job(json!({}))).0, 200);
    for _ in 0..4 { w.containers.answer("keep", json!({ "throw": "temporarily unavailable" })) }
    let (status, text) = w.runner("r4", "start", job(json!({})));
    assert!(status == 502 && text.contains("cannot be kept running: temporarily unavailable"), "{status} {text}");
    // Answered at once rather than as a promise; no runner image named; no container at all.
    w.containers_are(json!({ "syncStart": true, "syncKeep": true }));
    assert_eq!(w.runner("r5", "start", job(json!({}))).0, 200);
    w.containers_are(json!({ "noImage": true }));
    let (status, text) = w.runner("r5", "start", job(json!({})));
    assert!(status == 502 && text.contains("names no runner image"), "{status} {text}");
    w.containers_are(json!({ "noContainer": true }));
    assert_ne!(w.runner("r6", "start", job(json!({}))).0, 200);
    assert_eq!(w.alarm(Some("r6")).0, 200);
    w.containers_are(json!({}));
    // The class kept from before 0.5.0: gone, said.
    assert_eq!(w.get("/__test/legacy").0, 410);
}


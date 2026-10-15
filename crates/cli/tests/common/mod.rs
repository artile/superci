//! What the CLI's end-to-end tests share: a workspace on this computer that keeps a control plane served here
//! (`superci_testkit`), a run played to its end on it, the CLI and the dashboard as they run.
#![allow(dead_code)]
use std::io::{BufRead, BufReader};
use std::process::{Command, Output, Stdio};
use superci_testkit::{Runner, Served};

/// A folder of its own for one test.
pub fn temp(what: &str) -> std::path::PathBuf {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let dir = std::env::temp_dir().join(format!("superci-{what}-{}-{}-{nanos}", std::process::id(), N.fetch_add(1, std::sync::atomic::Ordering::SeqCst)));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A workspace on this computer that keeps the served control plane and a key it takes, as one signed in before
/// would (AWS's sign-in since ended).
pub fn home(served: &Served) -> std::path::PathBuf {
    let home = temp("home");
    let ws = home.join("workspaces/default");
    std::fs::create_dir_all(&ws).unwrap();
    std::fs::write(ws.join("sign-ins.json"), serde_json::json!({
        "key": { "name": "DASHBOARD_KEY_4102444800_TEST", "value": served.key, "planes": ["testplane0001"] },
        "plane": { "Aws": { "account_id": "111111111111", "region": "us-east-1", "url": served.url, "plane_id": "testplane0001", "label": "superci" } },
    }).to_string()).unwrap();
    home
}

/// The same, signed in to the control plane's AWS account too (so it may change it), its AWS being the test kit's
/// stand-in.
pub fn signed_in(served: &Served) -> std::path::PathBuf {
    let home = home(served);
    let file = home.join("workspaces/default/sign-ins.json");
    let mut kept: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    kept["aws"] = serde_json::json!({ "region": "us-east-1", "account_id": "111111111111", "who": "arn:aws:sts::111111111111:assumed-role/Admin/someone",
        "creds": { "access_key_id": "ASIAEXAMPLE", "secret_access_key": "example", "session_token": "example", "expires_at_ms": 4102444800000u64 },
        "refresh_token": "", "dpop": { "private_b64": "", "kid": "" } });
    std::fs::write(&file, kept.to_string()).unwrap();
    std::fs::write(home.join("aws-at"), &served.aws.url).unwrap();
    home
}

/// The CLI or the dashboard, in `home`'s workspace (with its stand-in for AWS, if it has one).
fn command(home: &std::path::Path) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_superci"));
    c.env("SUPERCI_HOME", home).env_remove("SUPERCI_PLANE").env_remove("SUPERCI_KEY").env("SUPERCI_STAND_IN", std::fs::read_to_string(home.join("aws-at")).unwrap_or_default());
    c
}

/// The CLI run in `home`'s workspace.
pub fn superci(home: &std::path::Path, args: &[&str]) -> Output { command(home).args(args).output().unwrap() }

/// The CLI's answer with `--json`, and how it ended.
pub fn json(home: &std::path::Path, args: &[&str]) -> (i32, serde_json::Value) {
    let o = superci(home, &[args, &["--json"]].concat());
    let text = String::from_utf8_lossy(&o.stdout);
    (o.status.code().unwrap_or(-1), serde_json::from_str(&text).unwrap_or_else(|_| panic!("{args:?}: {text}{}", String::from_utf8_lossy(&o.stderr))))
}

/// A run of one job played to its end in a checkout under `dir`: its step, its 12 lines (in colour) and log, passed.
pub fn played(served: &Served, dir: &std::path::Path) {
    std::fs::create_dir_all(dir.join(".github/workflows")).unwrap();
    std::fs::write(dir.join(".github/workflows/ci.yml"), "jobs:\n  tests:\n    runs-on: superci\n    steps:\n      - run: make test\n").unwrap();
    for args in [&["init", "-q"][..], &["add", "-A"], &["-c", "user.name=t", "-c", "user.email=t@example.com", "commit", "-qm", "t"]] { assert!(Command::new("git").current_dir(dir).args(args).status().unwrap().success()) }
    let cli = Command::new(env!("CARGO_BIN_EXE_superci")).current_dir(dir).args(["runs", "create", "--workflow=.github/workflows/ci.yml", "--quiet"])
        .env("SUPERCI_HOME", dir.join(".cli")).env("SUPERCI_PLANE", &served.url).env("SUPERCI_KEY", &served.run_key).stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();
    let run = loop { if let Some((k, _)) = served.store.under("run:").into_iter().next() { break k.trim_start_matches("run:").to_string() } std::thread::sleep(std::time::Duration::from_millis(50)) };
    let runner = loop { if let Some(r) = Runner::of_run(served, &run) { break r } std::thread::sleep(std::time::Duration::from_millis(50)) };
    let step = "dddddddd-0000-0000-0000-000000000004";
    runner.take();
    runner.steps(&[(step, "Run make test", 1, "inProgress", None, None)]);
    let lines: Vec<String> = (1..=12).map(|n| format!("\u{1b}[32mtest {n} passed\u{1b}[0m")).collect();
    runner.lines(step, 1, &lines);
    runner.log(1, &lines.iter().map(|l| format!("2026-10-15T10:00:01.0000000Z {l}")).collect::<Vec<_>>().join("\n"));
    runner.steps(&[(step, "Run make test", 1, "completed", Some("succeeded"), Some(1))]);
    runner.finish("succeeded");
    let _ = cli.wait_with_output();
}

/// A dashboard on this computer for `home`'s workspace, and its link (with the key that opens it).
pub fn dashboard(home: &std::path::Path) -> (std::process::Child, String) {
    let port = { let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap(); l.local_addr().unwrap().port() };
    let mut child = command(home).args(["dashboard", "--no-browser", "--workspace=default"]).env("SUPERCI_PORT", port.to_string())
        .stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines().map_while(Result::ok);
    let link = lines.find_map(|l| l.split_whitespace().find(|w| w.starts_with("http://localhost")).map(str::to_string)).expect("the dashboard's link");
    // (What it prints later is read too: a closed pipe would end it.)
    std::thread::spawn(move || lines.for_each(drop));
    (child, link)
}

/// What the dashboard answers for `path`, as its own page asks (with the cookie its link set): status and body.
pub fn part(link: &str, path: &str) -> (u16, String) {
    let (base, key) = link.split_once("/?k=").unwrap();
    let port = base.rsplit(':').next().unwrap();
    let agent: ureq::Agent = ureq::Agent::config_builder().http_status_as_error(false).max_redirects(0).build().into();
    let mut r = agent.get(&format!("{base}{path}")).header("cookie", &format!("superci_local_{port}={key}")).header("sec-fetch-site", "same-origin").call().unwrap();
    (r.status().as_u16(), r.body_mut().read_to_string().unwrap_or_default())
}

/// A page's words, without its markup.
pub fn words(html: &str) -> String {
    // (Without its styles and scripts.)
    let mut html = html.to_string();
    for (open, close) in [("<style", "</style>"), ("<script", "</script>")] {
        while let Some(a) = html.find(open) { let b = html[a..].find(close).map(|b| a + b + close.len()).unwrap_or(html.len()); html.replace_range(a..b, " "); }
    }
    let mut out = String::new();
    let mut tag = false;
    for c in html.chars() {
        match c { '<' => { tag = true; out.push(' ') } '>' => tag = false, c if !tag => out.push(c), _ => {} }
    }
    out.replace("&#39;", "'").replace("&amp;", "&").split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A form of the dashboard's own pages sent (`fields`), as a browser sends it: status, where it leads, and the body.
pub fn post(link: &str, path: &str, fields: &[(&str, &str)]) -> (u16, String, String) {
    let (base, key) = link.split_once("/?k=").unwrap();
    let port = base.rsplit(':').next().unwrap();
    let agent: ureq::Agent = ureq::Agent::config_builder().http_status_as_error(false).max_redirects(0).build().into();
    let body = url::form_urlencoded::Serializer::new(String::new()).extend_pairs(fields).finish();
    let mut r = agent.post(&format!("{base}{path}")).header("cookie", &format!("superci_local_{port}={key}")).header("sec-fetch-site", "same-origin")
        .header("content-type", "application/x-www-form-urlencoded").send(body).unwrap();
    let to = r.headers().get("location").and_then(|v| v.to_str().ok()).unwrap_or_default().to_string();
    (r.status().as_u16(), to, r.body_mut().read_to_string().unwrap_or_default())
}

/// The same for a page reached with a GET, with where it leads.
pub fn get(link: &str, path: &str) -> (u16, String, String) {
    let (base, key) = link.split_once("/?k=").unwrap();
    let port = base.rsplit(':').next().unwrap();
    let agent: ureq::Agent = ureq::Agent::config_builder().http_status_as_error(false).max_redirects(0).build().into();
    let mut r = agent.get(&format!("{base}{path}")).header("cookie", &format!("superci_local_{port}={key}")).header("sec-fetch-site", "same-origin").call().unwrap();
    let to = r.headers().get("location").and_then(|v| v.to_str().ok()).unwrap_or_default().to_string();
    (r.status().as_u16(), to, r.body_mut().read_to_string().unwrap_or_default())
}

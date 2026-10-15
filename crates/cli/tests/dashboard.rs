//! The dashboard as a browser shows it, against a control plane served on this computer (`superci_testkit`): its
//! pages drawn by headless Chrome after their scripts have run (what a person sees), with a run played to its end
//! first. Skipped where no Chrome is installed.
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use superci_testkit::{serve, Runner, Served};

fn chrome() -> Option<String> {
    ["/Applications/Google Chrome.app/Contents/MacOS/Google Chrome", "/usr/bin/google-chrome", "/usr/bin/google-chrome-stable", "/usr/bin/chromium", "/usr/bin/chromium-browser"]
        .iter().find(|p| std::path::Path::new(p).exists()).map(|p| p.to_string())
}

/// A workspace on this computer that keeps the served control plane and a key it takes, as one signed in before would.
fn home(served: &Served) -> std::path::PathBuf {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let home = std::env::temp_dir().join(format!("superci-dash-{}-{}", std::process::id(), N.fetch_add(1, std::sync::atomic::Ordering::SeqCst)));
    let ws = home.join("workspaces/default");
    std::fs::create_dir_all(&ws).unwrap();
    std::fs::write(ws.join("sign-ins.json"), serde_json::json!({
        "key": { "name": "DASHBOARD_KEY_4102444800_TEST", "value": served.key, "planes": ["testplane0001"] },
        "plane": { "Aws": { "account_id": "111111111111", "region": "us-east-1", "url": served.url, "plane_id": "testplane0001", "label": "superci" } },
    }).to_string()).unwrap();
    home
}

/// A run of one job played to its end: its steps, lines and log, and how it ended.
fn played(served: &Served, dir: &std::path::Path) {
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

/// A dashboard on this computer for `home`'s workspace: its link (with the key that opens it).
fn dashboard(home: &std::path::Path) -> (std::process::Child, String) {
    let port = { let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap(); l.local_addr().unwrap().port() };
    let mut child = Command::new(env!("CARGO_BIN_EXE_superci")).args(["dashboard", "--no-browser", "--workspace=default"]).env("SUPERCI_HOME", home).env("SUPERCI_PORT", port.to_string())
        .stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    let out = child.stdout.take().unwrap();
    let mut lines = BufReader::new(out).lines().map_while(Result::ok);
    let link = lines.find_map(|l| l.split_whitespace().find(|w| w.starts_with("http://localhost")).map(str::to_string)).expect("the dashboard's link");
    // (What it prints later is read too: a closed pipe would end it.)
    std::thread::spawn(move || lines.for_each(drop));
    (child, link)
}

/// A page as a browser draws it, its scripts given time to run.
fn page(chrome: &str, link: &str, home: &std::path::Path) -> String {
    let out = Command::new(chrome).args(["--headless=new", "--disable-gpu", "--no-first-run", "--virtual-time-budget=8000", &format!("--user-data-dir={}", home.join("chrome").display()), "--dump-dom", link]).output().unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn the_overview_and_a_jobs_page_show_what_ran_and_its_log_in_steps() {
    let Some(chrome) = chrome() else { eprintln!("no Chrome here: skipped"); return };
    let served = serve(|_| {});
    let home = home(&served);
    played(&served, &home.join("checkout"));
    let (mut child, link) = dashboard(&home);
    // The Overview: the usage of the last day, its period switch, and the job.
    let overview = page(&chrome, &link, &home);
    let main_of = |p: &str| p.split("<main").nth(1).unwrap_or(p).chars().take(6000).collect::<String>();
    assert!(overview.contains("<h2>Usage</h2>") && overview.contains("24 hours") && overview.contains("30 days") && overview.contains("Passed"), "{}", main_of(&overview));
    // The job's page: its log read by the page itself, in steps, its lines in colour.
    let job = served.store.under("job:").into_iter().next().unwrap().1["job_id"].as_u64().unwrap();
    // (Each Chrome run starts without the last one's cookie: the link carries the key, and names the page.)
    let shown = page(&chrome, &format!("{link}&p=job&id={job}"), &home);
    assert!(shown.contains("Its log") && shown.contains("lstep success") && shown.contains("Run make test"), "{}", main_of(&shown));
    assert!(shown.contains(r#"<span class="a32">test 12 passed</span>"#), "its lines in colour");
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

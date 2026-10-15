//! The dashboard against a control plane served on this computer (`superci_testkit`), a run played to its end first:
//! each page as the dashboard's own page fetches it, and the pages headless Chrome draws once their scripts have run
//! (what a person sees; skipped where no Chrome is installed).
mod common;
use common::{dashboard, home, part, played, words};
use std::process::Command;
use superci_testkit::serve;

fn chrome() -> Option<String> {
    ["/Applications/Google Chrome.app/Contents/MacOS/Google Chrome", "/usr/bin/google-chrome", "/usr/bin/google-chrome-stable", "/usr/bin/chromium", "/usr/bin/chromium-browser"]
        .iter().find(|p| std::path::Path::new(p).exists()).map(|p| p.to_string())
}

/// A page as a browser draws it, its scripts given time to run.
fn page(chrome: &str, link: &str, home: &std::path::Path) -> String {
    let out = Command::new(chrome).args(["--headless=new", "--disable-gpu", "--no-first-run", "--virtual-time-budget=8000", &format!("--user-data-dir={}", home.join("chrome").display()), "--dump-dom", link]).output().unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn each_page_shows_what_the_control_plane_has() {
    let served = serve(|_| {});
    let home = home(&served);
    played(&served, &home.join("checkout"));
    let (mut child, link) = dashboard(&home);
    let page = |p: &str| { let (status, body) = part(&link, &format!("/?p={p}&fragment=1")); assert_eq!(status, 200, "{p}"); words(&body) };
    let has = |p: &str, all: &[&str]| { let w = page(p); for s in all { assert!(w.contains(s), "{p} lacks {s:?}: {w}") } w };
    let job = served.store.under("job:").into_iter().next().unwrap().1;
    let id = job["job_id"].as_u64().unwrap();

    has("overview", &["Usage", "24 hours", "7 days", "30 days", "1 job", "Control plane", "Running"]);
    has("jobs", &["Recent jobs", "tests", "checkout · .github/workflows/ci.yml", "4 CPU · 12 GB", "Passed"]);
    // A run, and its page by the link the list gives.
    let (_, runs) = part(&link, "/?p=runs&fragment=1");
    assert!(words(&runs).contains(".github/workflows/ci.yml checkout 1 job"), "{}", words(&runs));
    let run_link = runs.split("href=\"").filter_map(|h| h.split('"').next()).find(|h| h.starts_with("/?p=run&")).expect("a run's link").replace("&amp;", "&");
    has(run_link.trim_start_matches("/?p="), &["Run from a computer: it has no page at GitHub.", "Passed", "tests"]);
    let shown = has(&format!("job&id={id}"), &["tests", "Run from a computer", "Status Passed", "Its start", "Its log", "Raw log"]);
    assert!(shown.contains("from queued to running"), "{shown}");
    has("runners", &["Runner providers", "Jobs at once", "Monthly budget"]);
    has("add-runners", &["Add a provider", "AWS", "Modal", "Cloudflare"]);
    has("workflows", &["Default machine", "Connect GitHub or GitLab first"]);
    has("repos", &["Repositories", "GitHub", "Create its App"]);
    has("gitlab", &["GitLab is not connected"]);
    has("planes", &["Control plane", "AWS · 111111111111 · us-east-1", env!("CARGO_PKG_VERSION"), "In use"]);
    has("settings", &["Name", "SuperCI", "Labels", "superci", "Largest machine", "Workspace", "default"]);
    has("changes", &["Changelog", env!("CARGO_PKG_VERSION")]);

    // Its log as the page reads it: in steps, its lines in colour, their times.
    let (status, view) = part(&link, &format!("/job/view?id={id}"));
    let view: serde_json::Value = serde_json::from_str(&view).unwrap();
    assert_eq!((status, view["going"].as_bool(), view["errors"].as_u64()), (200, Some(false), Some(0)));
    let step = &view["steps"][0];
    assert_eq!((step["name"].as_str(), step["state"].as_str(), step["lines"].as_array().map(Vec::len)), (Some("Run make test"), Some("success"), Some(12)));
    assert_eq!(step["lines"][11], serde_json::json!(["10:00:01", "", r#"<span class="a32">test 12 passed</span>"#]));
    // And as it is, to save.
    let (status, raw) = part(&link, &format!("/job/raw?id={id}"));
    assert!(status == 200 && raw.lines().count() == 12 && raw.trim_end().ends_with("test 12 passed\u{1b}[0m"), "{raw}");
    // A job not asked well.
    assert_eq!(part(&link, "/job/view?id=x1").0, 400);
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn the_overview_and_a_jobs_page_as_chrome_draws_them() {
    let Some(chrome) = chrome() else { eprintln!("no Chrome here: skipped"); return };
    let served = serve(|_| {});
    let home = home(&served);
    played(&served, &home.join("checkout"));
    let (mut child, link) = dashboard(&home);
    let main_of = |p: &str| p.split("<main").nth(1).unwrap_or(p).chars().take(6000).collect::<String>();
    // The Overview: the usage of the last day, its period switch, and the job.
    let overview = page(&chrome, &link, &home);
    assert!(overview.contains("<h2>Usage</h2>") && overview.contains("24 hours") && overview.contains("30 days") && overview.contains("Passed"), "{}", main_of(&overview));
    // The job's page: its log read by the page itself, in steps, its lines in colour. (Each Chrome run starts without
    // the last one's cookie: the link carries the key, and names the page.)
    let job = served.store.under("job:").into_iter().next().unwrap().1["job_id"].as_u64().unwrap();
    let shown = page(&chrome, &format!("{link}&p=job&id={job}"), &home);
    assert!(shown.contains("Its log") && shown.contains("lstep success") && shown.contains("Run make test"), "{}", main_of(&shown));
    assert!(shown.contains(r#"<span class="a32">test 12 passed</span>"#), "its lines in colour");
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

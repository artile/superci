//! The dashboard against a control plane served on this computer (`superci_testkit`), a run played to its end first:
//! each page as the dashboard's own page fetches it (`browser.rs` uses them in Chrome).
mod common;
use common::{dashboard, home, json, part, played, signed_in, words};
use superci_testkit::serve;

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
fn an_aws_sign_in_that_aws_does_not_finish_says_so_and_its_button_signs_in_again() {
    let served = serve(|_| {});
    let home = home(&served);
    std::fs::write(home.join("aws-at"), &served.aws.url).unwrap();
    let (mut child, link) = dashboard(&home);
    // Signing in: AWS's page, with the state it comes back with.
    let (_, to, page) = common::post(&link, "/aws/signin", &[("next", "?p=overview")]);
    let at = if to.is_empty() { page } else { to };
    let state = at.split("state=").nth(1).and_then(|r| r.split(['&', '"']).next()).expect("a state").to_string();
    // AWS comes back, and then refuses its last step.
    let (status, _, refused) = common::get(&link, &format!("/oauth/callback?code=a-code-from-aws&state={state}"));
    let said = words(&refused);
    assert!(status == 400 && said.contains("AWS did not finish the sign-in") && said.contains("is otherwise malformed") && said.contains("Sign in again") && !said.contains("Something went wrong"), "{status} {said}");
    // Its button: on the dashboard's own address, signing in again (signed out of AWS first), then back where it was.
    let again = refused.split("class=\"button primary\" href=\"").nth(1).and_then(|r| r.split('"').next()).unwrap().replace("&amp;", "&");
    assert!(again.starts_with("http://localhost:") && again.contains("/aws/again?next=") && again.contains("p%3Doverview"), "{again}");
    let (status, to, _) = common::get(&link, &again[again.find("/aws/again").unwrap()..]);
    assert!(status == 303 && to.contains("logout") && to.contains("state%3D"), "{status} {to}");
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn what_the_dashboard_is_given_is_checked_each_part_on_its_own_and_a_refusal_changes_nothing() {
    // With Cloudflare's own containers: a GitHub job running there (for removing a provider that runs one).
    let served = serve(|c| c.containers = true);
    served.connect_github();
    let home = signed_in(&served);
    let (id, queued) = served.github.queue("test", "superci", served.now_ms());
    assert_eq!(served.webhook(queued), 202);
    assert_eq!(served.webhook(served.github.start(id)), 202);
    let (mut child, link) = dashboard(&home);
    let said = |r: (u16, String)| (r.0, words(&r.1));

    // An artifact: a run's id in digits, and a name.
    for q in ["/run/artifact?run=&name=a", "/run/artifact?run=12a&name=a", "/run/artifact?run=12&name="] {
        let (status, page) = said(part(&link, q));
        assert!(status == 400 && page.contains("Which artifact?"), "{q}: {status} {page}");
    }
    // A job's log: its id in digits, and a GitLab connection's name as one is written.
    for q in ["/job/view?id=", "/job/view?id=1x", "/job/view?id=1&gl=Not-This"] {
        let (status, page) = said(part(&link, q));
        assert!(status == 400 && page.contains("Which job?"), "{q}: {status} {page}");
    }
    assert_eq!(part(&link, &format!("/job/view?id={id}")).0, 200, "its own");

    // GitLab's address: https, and no spaces; a connection named that is not here.
    for (url, which, refused) in [("http://gitlab.com", "new", "GitLab's address starts with https://"), ("https://git lab.com", "new", "GitLab's address starts with https://"), ("https://gitlab.com", "gnope", "That GitLab is not connected here")] {
        let (status, _, page) = common::post(&link, "/gitlab/connect", &[("which", which), ("url", url), ("token", superci_testkit::gitlab::TOKEN)]);
        assert!(status == 400 && words(&page).contains(refused), "{url} {which}: {status} {}", words(&page));
    }
    assert!(served.aws.settings(superci_testkit::PLANE).iter().all(|(k, _)| k != "GITLAB"), "nothing connected");

    // Removing a provider: for the control plane it was shown for; one of the known; and not under a running job
    // unless that job is to be stopped too.
    let (status, _, page) = common::post(&link, "/runners/remove", &[("plane", "otherplane01"), ("cloud", "cloudflare")]);
    assert!(status == 409 && words(&page).contains("Another control plane is in use now"), "{status} {}", words(&page));
    let (status, _, page) = common::post(&link, "/runners/remove", &[("plane", superci_testkit::PLANE), ("cloud", "nowhere")]);
    assert!(status == 400 && words(&page).contains("Unknown provider"), "{status} {}", words(&page));
    let (status, _, page) = common::post(&link, "/runners/remove", &[("plane", superci_testkit::PLANE), ("cloud", "cloudflare")]);
    assert!(status == 409 && words(&page).contains("1 job is running on Cloudflare"), "{status} {}", words(&page));
    assert!(served.aws.settings(superci_testkit::PLANE).iter().any(|(k, v)| k == "CONTAINERS" && v == "on"), "still there");

    // A key's name: letters and digits, 24 at most; and its days, 1 to 366.
    let keys = || json(&home, &["keys", "list"]).1["keys"].as_array().map(Vec::len);
    let before = keys();
    for (args, refused) in [(&["keys", "create", "agent-1"][..], "letters and digits"), (&["keys", "create", "abcdefghijklmnopqrstuvwxy"], "letters and digits"), (&["keys", "create", "agent", "--days=0"], "1 to 366 days"), (&["keys", "create", "agent", "--days=367"], "1 to 366 days")] {
        let (code, v) = json(&home, args);
        assert!(code != 0 && v["error"].as_str().unwrap_or_default().contains(refused), "{args:?}: {v}");
    }
    assert_eq!(keys(), before, "no key made");
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_page_asked_for_often_still_shows_what_happened_a_few_seconds_ago() {
    let served = serve(|_| {});
    served.connect_github();
    let home = signed_in(&served);
    let (mut child, link) = dashboard(&home);
    // Read once, with no jobs yet.
    assert!(!words(&part(&link, "/?p=jobs&fragment=1").1).contains("build"));
    // A job starts; the page is asked for every half second (several tabs, or a page that looks often).
    let (id, queued) = served.github.queue("build", "superci", served.now_ms());
    assert_eq!(served.webhook(queued), 202);
    assert_eq!(served.webhook(served.github.start(id)), 202);
    let (began, mut shown) = (std::time::Instant::now(), false);
    while began.elapsed() < std::time::Duration::from_secs(15) {
        if words(&part(&link, "/?p=jobs&fragment=1").1).contains("build") { shown = true; break }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    assert!(shown, "the new job not shown after 15 s of asking every half second");
    assert!(began.elapsed() < std::time::Duration::from_secs(8), "shown only after {:?}", began.elapsed());
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

/// What the Runners and Settings pages' forms change, each as its page posts it: where jobs go (the default cloud, a
/// rule for repositories, taken out again), a read key taken out, a repository's cache entries taken out; and what is
/// refused, with nothing changed (a repository that is no repository, an unknown change, a budget choice AWS has not).
#[test]
fn the_routing_keys_and_cache_forms_change_the_control_plane() {
    let served = superci_testkit::serve(|_| {});
    let home = common::signed_in(&served);
    let (mut child, link) = common::dashboard(&home);
    let routing = || served.aws.settings(superci_testkit::PLANE).into_iter().find(|(k, _)| k == "ROUTING").and_then(|(_, v)| serde_json::from_str::<serde_json::Value>(&v).ok()).unwrap_or_default();
    let post = |path: &str, fields: &[(&str, &str)]| common::post(&link, path, fields);
    assert!(post("/routing", &[("action", "default"), ("cloud", "aws")]).0 < 400);
    assert_eq!(routing()["default"], "aws");
    assert!(post("/routing", &[("action", "add"), ("repo", "acme/*"), ("cloud", "cloudflare")]).0 < 400);
    assert_eq!(routing()["rules"], serde_json::json!([{ "repo": "acme/*", "cloud": "cloudflare" }]));
    let (status, _, said) = post("/routing", &[("action", "add"), ("repo", "not a repository"), ("cloud", "aws")]);
    assert!(status == 400 && common::words(&said).contains("A repository is owner/name, owner/* or *"), "{status} {}", common::words(&said));
    assert!(post("/routing", &[("action", "remove"), ("repo", "acme/*")]).0 < 400);
    assert_eq!(routing()["rules"], serde_json::json!([]));
    assert_eq!(post("/routing", &[("action", "nothing-like-it")]).0, 400);
    let (status, _, said) = post("/routing", &[("action", "account_usd"), ("account", "aws"), ("usd", "40"), ("budget", "maybe")]);
    assert!(status == 400 && common::words(&said).contains("What AWS does at the limit is none, emails or stops"), "{status} {}", common::words(&said));
    // A read key taken out from Settings → Keys.
    let read = |n: &str| served.aws.settings(superci_testkit::PLANE).iter().any(|(k, _)| k.starts_with("READ_KEY_") && k.ends_with(n));
    assert!(read("_AGENT"));
    assert!(post("/keys/remove", &[("read", "agent")]).0 < 400);
    assert!(!read("_AGENT"));
    // A repository's cache entries taken out (from the control plane's own store).
    let (status, to, _) = post("/caches/delete", &[("repo", "acme/app")]);
    assert!(status == 303 && to.contains("p=settings"), "{status} {to}");
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

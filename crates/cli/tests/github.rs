//! GitHub's jobs end to end: the test plays GitHub (`superci_testkit::GitHub`: a job queued in `acme/app`, taken by
//! its runner, ended, each a signed webhook) against a control plane served on this computer with SuperCI's App
//! connected; the job is then followed in the CLI and in the dashboard, in Chrome where it is installed.
mod common;
use common::{dashboard, json, part, signed_in, superci, words};
use superci_testkit::{serve, Browser, Served};

/// GitHub's log of a job, as it keeps it (each line timed; a step's lines in a group).
fn github_log() -> String {
    ["2026-10-15T10:00:00.1000000Z ##[group]Set up job", "2026-10-15T10:00:00.2000000Z Current runner version: '2.329.0'", "2026-10-15T10:00:00.3000000Z ##[endgroup]",
        "2026-10-15T10:00:01.0000000Z ##[group]Run make test", "2026-10-15T10:00:01.1000000Z \u{1b}[36;1mmake test\u{1b}[0m", "2026-10-15T10:00:01.2000000Z ##[endgroup]",
        "2026-10-15T10:00:02.0000000Z test 1 passed", "2026-10-15T10:00:03.0000000Z \u{1b}[32mall 2 passed\u{1b}[0m", "2026-10-15T10:00:04.0000000Z Cleaning up orphan processes"].join("\n")
}

/// A job of `acme/app` labelled `superci`, from queued to ended as GitHub tells it: its number.
fn ran(served: &Served, conclusion: &str) -> u64 {
    let (id, queued) = served.github.queue("test", "superci", served.now_ms());
    assert_eq!(served.webhook(queued), 202);
    // A machine for it, its runner registered with GitHub for exactly this job.
    assert_eq!(served.machines.0.lock().unwrap().len(), 1, "its machine");
    let job = served.store.under(&format!("job:{id}")).into_iter().next().expect("the job kept").1;
    assert_eq!((job["state"].as_str(), job["repo"].as_str()), (Some("launched"), Some("acme/app")), "{job}");
    assert_eq!(served.webhook(served.github.start(id)), 202);
    assert_eq!(served.store.under(&format!("job:{id}"))[0].1["state"], "running");
    assert_eq!(served.webhook(served.github.finish(id, conclusion, &github_log(), served.now_ms())), 202);
    id
}

fn unanswered(served: &Served) -> Vec<String> { served.github.unanswered.lock().unwrap().iter().filter(|u| u.contains("github")).cloned().collect() }

#[test]
fn a_github_job_runs_on_its_machine_and_is_read_with_its_log_in_the_cli() {
    let served = serve(|_| {});
    served.connect_github();
    let home = signed_in(&served);
    let id = ran(&served, "success");
    let job = served.store.under(&format!("job:{id}"))[0].1.clone();
    assert_eq!((job["state"].as_str(), job["conclusion"].as_str()), (Some("done"), Some("success")), "{job}");

    // Listed, with its repository and its page at GitHub.
    let jobs = json(&home, &["jobs", "list"]).1;
    let listed = &jobs["jobs"][0];
    assert_eq!((listed["job"].as_str(), listed["ended"].as_str()), (Some("test"), Some("passed")), "{jobs}");
    assert!(listed["link"].as_str().unwrap().starts_with("https://github.com/acme/app/actions/runs/"), "{jobs}");
    // Its log, from GitHub.
    let o = superci(&home, &["jobs", "retrieve", &id.to_string()]);
    let said = String::from_utf8_lossy(&o.stdout);
    assert!(o.status.success() && said.contains("Run make test") && said.contains("all 2 passed"), "{said}{}", String::from_utf8_lossy(&o.stderr));
    // The whole of it into a file, from GitHub's own link.
    let out = home.join("job.log");
    let (code, saved) = json(&home, &["jobs", "retrieve", &id.to_string(), &format!("--out={}", out.display())]);
    assert_eq!(code, 0, "{saved}");
    assert_eq!(std::fs::read_to_string(&out).unwrap(), github_log());
    // The App, its installation and what it may do.
    let connections = json(&home, &["connections", "list"]).1;
    assert_eq!(connections["github"][0]["owner"].as_str().or(connections["installations"][0]["account"].as_str()), Some("acme"), "{connections}");
    assert!(unanswered(&served).is_empty(), "{:?}", unanswered(&served));
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_github_jobs_page_shows_its_log_in_steps_and_leads_to_github() {
    let served = serve(|_| {});
    served.connect_github();
    let home = signed_in(&served);
    let id = ran(&served, "failure");
    let (mut child, link) = dashboard(&home);
    // Its page as the dashboard's page fetches it, and its log in steps.
    let (_, page) = part(&link, &format!("/?p=job&id={id}&fragment=1"));
    let shown = words(&page);
    assert!(shown.contains("app · Build and Test") && shown.contains("Status Failed") && !shown.contains("It did not start"), "{shown}");
    assert!(page.contains(&format!("/actions/runs/")) && page.contains("On GitHub"), "{shown}");
    let (_, view) = part(&link, &format!("/job/view?id={id}"));
    let view: serde_json::Value = serde_json::from_str(&view).unwrap();
    let steps: Vec<&str> = view["steps"].as_array().unwrap().iter().filter_map(|s| s["name"].as_str()).collect();
    assert!(steps.contains(&"Set up job") && steps.contains(&"Run make test"), "{view}");
    // Its raw log is GitHub's own link.
    let (status, _) = part(&link, &format!("/job/raw?id={id}"));
    assert_eq!(status, 302);
    // Repositories: the App connected in acme, its repository.
    let (_, repos) = part(&link, "/?p=repos&fragment=1");
    assert!(words(&repos).contains("acme"), "{}", words(&repos));

    // In Chrome: from Jobs to the job, its failed step open with its lines.
    if let Some(mut b) = Browser::open(1300, 900) {
        b.goto(&link);
        b.wait("document.querySelector('main') && document.querySelector('main').innerText.includes('Usage')");
        b.click("nav a", "Jobs");
        b.wait("document.querySelector('main').innerText.includes('Recent jobs')");
        b.click("main a", "test");
        b.wait("document.querySelector('details.lstep')");
        b.wait("[...document.querySelectorAll('details.lstep')].some(d => d.innerText.includes('Run make test'))");
        let on_github = b.eval("document.querySelector('a[href^=\"https://github.com/acme/app/actions/runs/\"]').innerText");
        assert!(on_github.as_str().unwrap_or_default().contains("On GitHub"), "{on_github}");
    }
    assert!(unanswered(&served).is_empty(), "{:?}", unanswered(&served));
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_public_repositorys_jobs_wait_until_it_is_allowed_then_run() {
    let served = serve(|_| {});
    served.connect_github();
    let home = signed_in(&served);
    served.github.set_public(true);
    // Refused: anyone could open a pull request that runs code on these machines.
    let (first, queued) = served.github.queue("test", "superci", served.now_ms());
    assert_eq!(served.webhook(queued), 202);
    let job = served.store.under(&format!("job:{first}"))[0].1.clone();
    assert!(job["state"] == "failed" && job["error"].as_str().unwrap().contains("a public repository"), "{job}");
    assert!(served.machines.0.lock().unwrap().is_empty());
    let jobs = json(&home, &["jobs", "list"]).1;
    assert!(jobs["jobs"][0]["why"].as_str().unwrap_or_default().to_lowercase().contains("public"), "{jobs}");
    // Allowed in Settings: its next job runs.
    assert_eq!(json(&home, &["public_repos", "create", "acme/app"]).0, 0);
    assert_eq!(json(&home, &["public_repos", "list"]).1["public_repos"], serde_json::json!(["acme/app"]));
    let (_, queued) = served.github.queue("test", "superci", served.now_ms());
    assert_eq!(served.webhook(queued), 202);
    assert_eq!(served.machines.0.lock().unwrap().len(), 1);
    assert_eq!(json(&home, &["public_repos", "delete", "acme/app"]).0, 0);
    assert!(unanswered(&served).is_empty(), "{:?}", unanswered(&served));
}

#[test]
fn a_job_cancelled_before_its_runner_took_it_ends_its_machine() {
    let served = serve(|_| {});
    served.connect_github();
    let (id, queued) = served.github.queue("test", "superci", served.now_ms());
    served.webhook(queued);
    assert_eq!(served.webhook(served.github.cancel(id, served.now_ms())), 202);
    let job = served.store.under(&format!("job:{id}"))[0].1.clone();
    assert!(["cancelled", "done"].contains(&job["state"].as_str().unwrap()) && job["ended_ms"].is_u64(), "{job}");
    // Its runner is taken back from GitHub.
    assert!(served.github.asked.lock().unwrap().iter().any(|a| a.starts_with("DELETE https://api.github.com/orgs/acme/actions/runners/")), "{:?}", served.github.asked.lock().unwrap());
}

#[test]
fn a_job_github_no_longer_waits_for_is_noticed_by_the_upkeep_and_its_machine_ended() {
    let served = serve(|_| {});
    served.connect_github();
    let (id, queued) = served.github.queue("test", "superci", served.now_ms());
    served.webhook(queued);
    // Another runner took it and it ended; GitHub's word of that never came.
    served.github.ended_untold(id);
    served.later(30_000);
    served.upkeep();
    assert_eq!(served.store.under(&format!("job:{id}"))[0].1["state"], "launched", "not asked before it has had time to come up");
    served.later(60_000);
    served.upkeep();
    let job = served.store.under(&format!("job:{id}"))[0].1.clone();
    assert!(["swept", "done"].contains(&job["state"].as_str().unwrap()), "{job}");
    assert!(served.github.asked.lock().unwrap().iter().any(|a| a == &format!("GET https://api.github.com/repos/acme/app/actions/jobs/{id}")));
}

#[test]
fn a_webhook_not_signed_by_the_app_is_refused() {
    let served = serve(|_| {});
    served.connect_github();
    let (_, (body, _)) = served.github.queue("test", "superci", served.now_ms());
    assert_eq!(served.webhook((body, format!("sha256={}", "0".repeat(64)))), 401);
    assert!(served.machines.0.lock().unwrap().is_empty() && served.store.under("job:").is_empty());
}

#[test]
fn github_is_connected_from_repositories_and_its_jobs_then_run() {
    let served = serve(|_| {});
    let home = signed_in(&served);
    let (mut child, link) = dashboard(&home);
    // Repositories → Create its App: a form that takes the App's manifest to GitHub, for the organization named.
    let (status, _, form) = common::post(&link, "/github/start", &[("gh-where", "public"), ("host", ""), ("login", "acme")]);
    assert_eq!(status, 200, "{}", words(&form));
    assert!(form.contains("https://github.com/organizations/acme/settings/apps/new?state="), "{form}");
    let state = form.split("state=").nth(1).unwrap().split(|c: char| !c.is_ascii_alphanumeric() && c != '-' && c != '_').next().unwrap().to_string();
    let manifest = form.split("name=\"manifest\" value=\"").nth(1).unwrap().split('"').next().unwrap().replace("&quot;", "\"").replace("&amp;", "&");
    let manifest: serde_json::Value = serde_json::from_str(&manifest).unwrap();
    assert_eq!((manifest["hook_attributes"]["url"].as_str(), manifest["default_events"][0].as_str()), (Some(format!("{}/webhook", served.url).as_str()), Some("workflow_job")), "{manifest}");
    // GitHub made it and comes back with its code: the App goes into the control plane's settings, then GitHub's
    // page to install it.
    let (status, to, _) = common::get(&link, &format!("/github/callback?code=a1b2c3d4e5f6&state={state}"));
    assert_eq!((status, to.as_str()), (303, "https://github.com/apps/superci-acme/installations/new"));
    let kept: serde_json::Value = serde_json::from_str(&served.aws.settings(superci_testkit::PLANE).into_iter().find(|(k, _)| k == "GITHUB_APP").unwrap().1).unwrap();
    assert_eq!((kept["id"].as_u64(), kept["owner"].as_str(), kept["owner_is_org"].as_bool()), (Some(1001), Some("acme"), Some(true)));
    // A code that is not this tab's is refused.
    assert_eq!(common::get(&link, "/github/callback?code=a1b2c3d4e5f6&state=other").0, 400);
    // Installed: back here, and Repositories shows acme.
    assert_eq!(common::get(&link, "/github/installed").0, 200);
    let (_, repos) = part(&link, "/?p=repos&fragment=1");
    assert!(words(&repos).contains("acme"), "{}", words(&repos));
    // Its jobs run.
    ran(&served, "success");
    assert!(unanswered(&served).is_empty(), "{:?}", unanswered(&served));
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_finished_jobs_page_shows_its_whole_log_however_large() {
    let served = serve(|_| {});
    served.connect_github();
    let home = signed_in(&served);
    let (id, queued) = served.github.queue("test", "superci", served.now_ms());
    served.webhook(queued);
    served.webhook(served.github.start(id));
    // 2.4 MB: more than the last part a running job's page reads.
    // (Its lines in the second its steps began, as GitHub says of them.)
    let at = served.github.job(id)["started_at"].as_str().unwrap().trim_end_matches('Z').to_string();
    let mut log = vec![format!("{at}.1000000Z ##[group]Set up job"), format!("{at}.2000000Z ##[endgroup]"), format!("{at}.3000000Z ##[group]Run make test")];
    log.extend((1..=40_000).map(|n| format!("{at}.4000000Z test {n} of the suite passed in good time")));
    let log = log.join("\n");
    assert!(log.len() > 2_000_000);
    served.webhook(served.github.finish(id, "success", &log, served.now_ms()));
    let (mut child, link) = dashboard(&home);
    let (_, view) = part(&link, &format!("/job/view?id={id}"));
    let view: serde_json::Value = serde_json::from_str(&view).unwrap();
    assert_eq!(view["truncated"], false);
    let step = view["steps"].as_array().unwrap().iter().find(|s| s["name"] == "Run make test").unwrap();
    let lines = step["lines"].as_array().unwrap();
    assert_eq!((lines.len(), lines[1][2].as_str(), lines[40_000][2].as_str()), (40_001, Some("test 1 of the suite passed in good time"), Some("test 40000 of the suite passed in good time")));
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

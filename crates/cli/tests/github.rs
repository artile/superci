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
    // What its log says of tests (none of the runners' that are read), and where its artifacts are: at GitHub.
    let said = String::from_utf8_lossy(&superci(&home, &["jobs", "retrieve", &id.to_string(), "--tests"]).stdout).into_owned();
    assert!(said.contains("No test results in its log") && said.contains(&format!("Its artifacts: https://github.com/acme/app/actions/runs/{}#artifacts", job["run_id"])), "{said}");
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
        b.click("nav a", "Runs");
        b.wait("document.querySelector('main').innerText.includes('Recent runs')");
        b.click(".tabs a", "Jobs");
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
    // A code that is no code GitHub gives (too short, too long, with what codes have not) is not taken to GitHub.
    for code in ["abc", &"a".repeat(101), "a1b2c3d4%21%21"] {
        let (status, _, said) = common::get(&link, &format!("/github/callback?code={code}&state={state}"));
        assert!(status >= 400 && words(&said).contains("invalid manifest code"), "{code}: {status} {}", words(&said));
    }
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
    // In Chrome: nothing of it drawn until a step is opened, then its first 2,000 lines and a way to more; search
    // finds a line far down without drawing the step whole.
    if let Some(mut b) = Browser::open(1300, 900) {
        b.goto(&format!("{link}&p=job&id={id}"));
        b.wait("document.querySelector('details.lstep')");
        assert_eq!(b.eval("document.querySelectorAll('.ll').length"), 0);
        b.click("details.lstep summary", "Run make test");
        b.wait("document.querySelectorAll('.ll').length === 2000");
        assert_eq!(b.text(".ll-more"), "Show 4,000 more of 38,001 lines");
        b.click(".ll-more", "");
        b.wait("document.querySelectorAll('.ll').length === 6000");
        assert_eq!(b.eval("document.querySelectorAll('.ll')[5999].querySelector('.ln').textContent"), "6000", "each line its number");
        b.fill("[data-log-search]", "test 39999 of");
        b.wait("document.querySelectorAll('.ll.hit').length === 1");
        assert_eq!((b.text(".ll.hit .lx"), b.eval("document.querySelectorAll('.ll').length")), ("test 39999 of the suite passed in good time".to_string(), serde_json::json!(1)));
        // Cleared: the step as it was opened (6,000 lines shown).
        b.fill("[data-log-search]", "");
        b.wait("document.querySelectorAll('.ll').length === 6000");
    }
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_jobs_steps_run_side_by_side_show_their_own_lines_once_its_run_has_ended() {
    let served = serve(|_| {});
    served.connect_github();
    let home = signed_in(&served);
    let id = ran(&served, "success");
    // GitHub's steps and run logs for a job with background steps and a parallel group (a probe on GitHub). Its own
    // log has their lines mixed: "two" after the next step's heading.
    served.github.set_steps(id, serde_json::from_str(include_str!("../fixtures/github/parallel-steps.json")).unwrap());
    let (mut child, link) = dashboard(&home);
    let lines = |view: &serde_json::Value, name: &str| -> Vec<String> {
        let s = view["steps"].as_array().unwrap().iter().find(|s| s["name"] == name).unwrap_or_else(|| panic!("{name}: {view}"));
        s["lines"].as_array().unwrap().iter().map(|l| l[2].as_str().unwrap().to_string()).collect()
    };
    // While its run goes on (GitHub keeps a run's logs once it has ended): its lines as they came, and said so.
    let (_, view) = part(&link, &format!("/job/view?id={id}"));
    let view: serde_json::Value = serde_json::from_str(&view).unwrap();
    assert_eq!(view["mixed"], true, "{view}");
    // Its run ended: each step's own lines, from its run's logs (not the view kept from before).
    served.github.run_ended(id, include_bytes!("../fixtures/github/run-logs.zip").to_vec());
    let (_, view) = part(&link, &format!("/job/view?id={id}"));
    let view: serde_json::Value = serde_json::from_str(&view).unwrap();
    assert_eq!(view.get("mixed"), None, "{view}");
    assert_eq!(lines(&view, "Run echo two; sleep 2").last().map(String::as_str), Some("two"));
    assert_eq!(lines(&view, "Run echo while they run").last().map(String::as_str), Some("while they run"));
    assert!(lines(&view, "Parallel group").iter().any(|l| l.contains("First: Succeeded")), "{view}");
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_github_run_is_cancelled_from_the_cli_by_github_and_its_running_jobs_machine_ends() {
    let served = serve(|_| {});
    served.connect_github();
    let home = signed_in(&served);
    let (id, queued) = served.github.queue("test", "superci", served.now_ms());
    assert_eq!(served.webhook(queued), 202);
    assert_eq!(served.webhook(served.github.start(id)), 202);
    let run = served.github.job(id)["run_id"].as_u64().unwrap().to_string();
    // Cancelled from the CLI: GitHub is asked, with the App's token for its repository.
    let o = superci(&home, &["runs", "cancel", &run]);
    let said = String::from_utf8_lossy(&o.stdout).to_string() + &String::from_utf8_lossy(&o.stderr);
    assert!(o.status.success() && said.contains(&format!("GitHub cancels run {run}")), "{said}");
    assert!(served.github.asked.lock().unwrap().iter().any(|a| a == &format!("POST https://api.github.com/repos/acme/app/actions/runs/{run}/cancel")), "{:?}", served.github.asked.lock().unwrap());
    // GitHub stops its runner's job and says so: its machine is ended.
    assert_eq!(served.webhook(served.github.cancel(id, served.now_ms())), 202);
    let job = served.store.under(&format!("job:{id}"))[0].1.clone();
    assert!(job["ended_ms"].is_u64() && job["conclusion"] == "cancelled", "{job}");
    // A run nobody here knows: said so.
    let o = superci(&home, &["runs", "cancel", "12345"]);
    assert!(!o.status.success() && String::from_utf8_lossy(&o.stderr).contains("No run 12345 here"), "{}", String::from_utf8_lossy(&o.stderr));
    // Another run, cancelled from the dashboard (its page's Cancel run): GitHub is asked the same way, and the page
    // is shown again.
    let (other, queued) = served.github.queue("test", "superci", served.now_ms());
    assert_eq!(served.webhook(queued), 202);
    assert_eq!(served.webhook(served.github.start(other)), 202);
    let run = served.github.job(other)["run_id"].as_u64().unwrap().to_string();
    let (mut child, link) = dashboard(&home);
    let (status, to, said) = common::post(&link, "/run/cancel", &[("id", &run), ("repo", "acme/app"), ("attempt", "1")]);
    assert!(status == 303 && to.starts_with(&format!("/?p=run&repo=acme%2Fapp&id={run}&attempt=1")), "{status} {to} {}", words(&said));
    assert!(served.github.asked.lock().unwrap().iter().any(|a| a == &format!("POST https://api.github.com/repos/acme/app/actions/runs/{run}/cancel")));
    let (status, _, said) = common::post(&link, "/run/cancel", &[("id", "not-a-run")]);
    assert!(status == 400 && words(&said).contains("Which run?"), "{status}");
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_job_asking_for_more_than_allowed_fails_at_once_saying_why_and_the_dashboard_shows_why() {
    let served = serve(|_| {});
    served.connect_github();
    let home = signed_in(&served);
    // More than any machine (a label that says no machine), and more than this CI allows (32 CPUs unless set).
    for (label, why) in [("superci/cpu=999", "cpu is a number of CPUs from 1 to 192"), ("superci/cpu=64", "the most allowed is 32")] {
        let (id, queued) = served.github.queue("test", label, served.now_ms());
        assert_eq!(served.webhook(queued), 202);
        let job = served.store.under(&format!("job:{id}"))[0].1.clone();
        assert!(job["state"] == "failed" && job["failed_fast"] == true && job["error"].as_str().unwrap_or_default().contains(why), "{label}: {job}");
    }
    assert!(served.machines.0.lock().unwrap().iter().all(|m| m.ends_with("-x")), "no machine for it, only one that fails it on GitHub: {:?}", served.machines.0.lock().unwrap());
    let (mut child, link) = dashboard(&home);
    let (_, page) = part(&link, "/?p=jobs&fragment=1");
    assert!(words(&page).contains("the most allowed is 32"), "{}", words(&page));
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn each_jobs_row_says_where_it_ran_and_why_in_plain_words() {
    let served = serve(|_| {});
    served.connect_github();
    let home = signed_in(&served);
    // Jobs as the control plane keeps them after what AWS and Cloudflare said (each a real job of acme/app, its
    // record then given what that situation writes).
    let cases: Vec<(serde_json::Value, &str)> = vec![
        (serde_json::json!({ "state": "done", "cloud": "aws", "machine_type": "c8a.xlarge", "cpu": 4, "on_demand": true, "spot_refused": "us-east-1: MaxSpotInstanceCountExceeded: Max spot instance count exceeded", "spot_ending": true }), "On-demand: your AWS spot limit in N. Virginia was reached (machines AWS was still ending count too)"),
        (serde_json::json!({ "state": "done", "cloud": "aws", "machine_type": "c8a.xlarge", "cpu": 4, "on_demand": true, "spot_refused": "us-east-1: InsufficientInstanceCapacity" }), "On-demand: AWS had no spot machine for it"),
        (serde_json::json!({ "state": "done", "cloud": "aws", "machine_type": "c8a.xlarge", "cpu": 4, "on_demand": true, "spot_refused": format!("us-east-1: {}", superci_core::plane::SPOT_NOT_ALLOWED) }), "On-demand: your AWS organization does not allow spot machines"),
        (serde_json::json!({ "state": "done", "cloud": "aws", "machine_type": "c8a.xlarge", "cpu": 4, "on_demand": true, "spot_refused": "us-east-1: RequestLimitExceeded" }), "On-demand: AWS refused the spot request for its rate"),
        (serde_json::json!({ "state": "done", "cloud": "aws", "machine_type": "c8a.xlarge", "cpu": 4, "on_demand": true, "after_reclaim": true }), "On-demand: run again after AWS took back its spot machine"),
        (serde_json::json!({ "state": "running", "cloud": "aws", "machine_type": "c8a.2xlarge", "cpu": 8, "spot_again": true }), "On a spot machine again: an on-demand one does not fit"),
        (serde_json::json!({ "state": "done", "cloud": "aws", "machine_type": "m8a.xlarge", "cpu": 4, "local_disk": true, "disk_gb": 120, "disk_mibs": 500 }), "4 CPU · m8a.xlarge · 120 GB disk at 500 MiB/s · local disk"),
        (serde_json::json!({ "state": "failed", "cloud": "cloudflare", "error": "runner container: 502 the container did not start in time" }), "Cloudflare did not start its container"),
        (serde_json::json!({ "state": "failed", "cloud": "cloudflare", "error": "the container cannot be kept running" }), "Cloudflare could not keep its container running"),
        (serde_json::json!({ "state": "failed", "cloud": "", "error": format!("waiting: {}", "x".repeat(130)) }), &"…"),
        (serde_json::json!({ "state": "swept", "cloud": "aws", "error": null }), "Its machine never started the job"),
        (serde_json::json!({ "state": "orphan", "cloud": "aws", "error": null }), "Its machine was lost"),
        // Kept off on-demand before its cloud is said; a RAM disk at more than AWS's own speed; spot again, but on
        // an on-demand machine after all, or on Cloudflare (neither said to be on spot again).
        (serde_json::json!({ "state": "launching", "cloud": "", "spot_again": true }), "On a spot machine again"),
        (serde_json::json!({ "state": "done", "cloud": "aws", "machine_type": "r8a.4xlarge", "cpu": 16, "ramdisk": true, "disk_mibs": 1000 }), "16 CPU · r8a.4xlarge · 1000 MiB/s disk · RAM disk"),
        (serde_json::json!({ "state": "done", "cloud": "aws", "machine_type": "c8a.xlarge", "cpu": 4, "on_demand": true, "spot_again": true, "after_reclaim": true }), "4 CPU · c8a.xlarge · on-demand"),
        (serde_json::json!({ "state": "done", "cloud": "cloudflare", "machine_type": "4cpu-12gb", "spot_again": true }), "4 CPU · 12 GB"),
        (serde_json::json!({ "state": "done", "cloud": "cloudflare", "machine_type": "2cpu-8gb", "on_demand": true }), "2 CPU · 8 GB"),
        (serde_json::json!({ "state": "done", "cloud": "aws", "machine_type": "c8a.large", "cpu": 2, "on_demand": true }), "2 CPU · c8a.large · on-demand"),
        (serde_json::json!({ "state": "waiting", "cloud": "", "error": "waiting: Cloudflare runs 2 at once" }), "Cloudflare runs 2 at once"),
    ];
    let mut ids = vec![];
    for (fields, _) in &cases {
        let (id, queued) = served.github.queue("test", "superci", served.now_ms());
        ids.push(id);
        assert_eq!(served.webhook(queued), 202);
        let key = format!("job:{id}");
        let mut m = served.store.0.lock().unwrap();
        let mut j: serde_json::Value = serde_json::from_str(&m[&key]).unwrap();
        for (k, v) in fields.as_object().unwrap() { j[k] = v.clone() }
        m.insert(key, j.to_string());
    }
    let (mut child, link) = dashboard(&home);
    let (_, page) = part(&link, "/?p=jobs&fragment=1");
    let said = words(&page);
    for (_, want) in &cases { assert!(said.contains(want), "no {want:?} in: {said}") }
    assert_eq!(said.matches("On a spot machine again").count(), 2, "{said}");
    assert_eq!(said.matches("run again after AWS took back its spot machine").count(), 2, "{said}");
    // A job's own page says why it failed, or why it waits.
    for (i, want) in [(7, "Cloudflare did not start its container"), (cases.len() - 1, "Cloudflare runs 2 at once")] {
        let (_, page) = part(&link, &format!("/?p=job&id={}&fragment=1", ids[i]));
        assert!(words(&page).contains(want), "{want}: {}", words(&page));
    }
    // The spot limit reached in the last day: said once above the jobs, with where to ask AWS for more.
    assert!(said.contains("in the last day ran on on-demand machines, at about twice the price: your AWS account's spot limit in N. Virginia was reached") && page.contains("servicequotas"), "{said}");
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn github_jobs_reach_the_cache_with_githubs_own_token_each_branch_in_its_own_scope_reading_the_default_branchs() {
    // A control plane with its own AWS account; a machine's link to its cache; GitHub's App connected.
    let served = serve(|c| {
        (c.aws_own, c.aws_own_creds) = (Some(("111111111111".into(), "us-east-1".into())), Some(superci_core::aws::Credentials { access_key_id: "ASIAX".into(), secret_access_key: "secret".into(), session_token: Some("tok".into()), expires_at_ms: u64::MAX }));
    });
    served.connect_github();
    let link = "machine-cache-link-000000000002";
    served.store.0.lock().unwrap().insert(format!("cachem:{link}"), serde_json::json!({ "region": "us-east-1", "arch": "x86_64", "until": u64::MAX / 4, "runner": "a-runner" }).to_string());
    let post = |what: &str, body: serde_json::Value| -> (u16, serde_json::Value) {
        let agent: ureq::Agent = ureq::Agent::config_builder().http_status_as_error(false).build().into();
        let mut r = agent.post(&format!("{}/cache/{link}/{what}", served.url)).send(body.to_string()).unwrap();
        (r.status().as_u16(), serde_json::from_str(&r.body_mut().read_to_string().unwrap_or_default()).unwrap_or_default())
    };
    let put = |url: &str, bytes: &[u8]| {
        let agent: ureq::Agent = ureq::Agent::config_builder().http_status_as_error(false).build().into();
        agent.put(&format!("{}/{}", served.aws.url, url.trim_start_matches("https://"))).send(bytes).unwrap().status().as_u16()
    };
    // GitHub's tokens: a push to main (writes main's), a pull request's branch (writes its own, reads main's), a job
    // that only reads, another repository's.
    let exp = 4_102_444_800u64;
    let token = |repo: u64, ac: serde_json::Value| superci_testkit::GitHub::job_token(&serde_json::json!({ "repository_id": repo.to_string(), "ac": ac.to_string(), "exp": exp }));
    let main = token(197814629, serde_json::json!([{ "Scope": "refs/heads/main", "Permission": 3 }]));
    let branch = token(197814629, serde_json::json!([{ "Scope": "refs/heads/fix", "Permission": 3 }, { "Scope": "refs/heads/main", "Permission": 1 }]));
    let reads = token(197814629, serde_json::json!([{ "Scope": "refs/heads/main", "Permission": 1 }]));
    let other = token(5, serde_json::json!([{ "Scope": "refs/heads/main", "Permission": 3 }]));
    let version = "c".repeat(64);
    let save = |t: &str, key: &str, bytes: &[u8]| {
        assert_eq!(post("begin", serde_json::json!({ "token": t, "key": key, "version": version })).1["ok"], true, "{key}");
        let (_, signed) = post("sign", serde_json::json!({ "token": t, "key": key, "version": version, "names": ["p00000"] }));
        assert_eq!(put(signed["urls"][0].as_str().unwrap(), bytes), 200);
        assert_eq!(post("finish", serde_json::json!({ "token": t, "key": key, "version": version, "pieces": [["p00000", bytes.len()]] })).1["ok"], true);
    };
    let found = |t: &str, key: &str| post("lookup", serde_json::json!({ "token": t, "key": key, "restore_keys": ["deps-"], "version": version })).1;
    save(&main, "deps-main", b"main's");
    // The branch reads main's; saves its own, which it then finds first; main does not see the branch's.
    assert_eq!(found(&branch, "deps-x")["key"], "deps-main");
    save(&branch, "deps-fix", b"fix's");
    assert_eq!(found(&branch, "deps-x")["key"], "deps-fix");
    assert_eq!(found(&main, "deps-fix")["key"], "deps-main", "(main reads only its own)");
    // One that only reads: finds, may not save. Another repository: finds nothing.
    assert_eq!(found(&reads, "deps-main")["hit"], true);
    let (status, said) = post("begin", serde_json::json!({ "token": reads, "key": "deps-new", "version": version }));
    assert_eq!((status, said["why"].as_str()), (403, Some("this job may only read the cache")), "{said}");
    assert_eq!(found(&other, "deps-main")["hit"], false);
    // Its artifacts are GitHub's to keep.
    let (status, said) = post("artifact-begin", serde_json::json!({ "token": main, "name": "report" }));
    assert_eq!((status, said["why"].as_str()), (403, Some("a job of GitHub's keeps its artifacts at GitHub")), "{said}");
    // Refused: a token past its time, one with no repository, one not GitHub's (signed by a key GitHub has not).
    let late = superci_testkit::GitHub::job_token(&serde_json::json!({ "repository_id": "197814629", "ac": "[]", "exp": 1 }));
    let nameless = superci_testkit::GitHub::job_token(&serde_json::json!({ "ac": "[]", "exp": exp }));
    let mut forged = main.clone();
    forged.truncate(forged.len() - 4);
    forged.push_str("AAAA");
    for (t, why) in [(late.as_str(), "expired"), (nameless.as_str(), "a token without its repository"), (forged.as_str(), "bad signature"), ("not.a.token", "not a job's token")] {
        let (status, said) = post("lookup", serde_json::json!({ "token": t, "key": "deps-main", "version": version }));
        assert_eq!((status, said["why"].as_str()), (403, Some(why)), "{said}");
    }
    // Versions GitHub's client would not send, a token whose repository is no number, a save finished with no
    // pieces, a machine's link too short: refused.
    for v in ["".to_string(), "a-b".to_string(), "c".repeat(129)] {
        assert_eq!(post("begin", serde_json::json!({ "token": main, "key": "deps-v", "version": v })).0, 400, "{v:?}");
    }
    let odd = superci_testkit::GitHub::job_token(&serde_json::json!({ "repository_id": "abc", "ac": "[]", "exp": exp }));
    assert_eq!(post("lookup", serde_json::json!({ "token": odd, "key": "deps-main", "version": version })).1["why"], "a token without its repository");
    assert_eq!(post("begin", serde_json::json!({ "token": main, "key": "deps-empty", "version": version })).1["ok"], true);
    assert!(post("finish", serde_json::json!({ "token": main, "key": "deps-empty", "version": version, "pieces": [] })).0 >= 400);
    let agent: ureq::Agent = ureq::Agent::config_builder().http_status_as_error(false).build().into();
    assert_eq!(agent.post(&format!("{}/cache/short/lookup", served.url)).send("{}").unwrap().status().as_u16(), 404);
    // GitHub's keys were asked for once, and kept.
    assert_eq!(served.github.jwks_asked(), 1);
    // The cache's list: the repository by its name, as GitHub has it; its entries by scope; a branch's taken out.
    let home = signed_in(&served);
    let (_, listed) = json(&home, &["caches", "list"]);
    assert_eq!((listed["repos"][0]["name"].as_str(), listed["repos"][0]["entries"].as_u64()), (Some("acme/app"), Some(2)), "{listed}");
    let (_, entries) = json(&home, &["caches", "list", "--repo=acme/app"]);
    let scopes: Vec<&str> = entries["entries"].as_array().unwrap().iter().filter_map(|e| e["scope"].as_str()).collect();
    assert!(scopes.contains(&"refs/heads/main") && scopes.contains(&"refs/heads/fix"), "{entries}");
    let o = superci(&home, &["caches", "delete", "--repo=acme/app", "deps-fix", "--scope=refs/heads/fix", "--confirm"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(found(&branch, "deps-x")["key"], "deps-main");
    // By the repository's id too.
    let o = superci(&home, &["caches", "delete", "--repo=gh197814629", "deps-none", "--confirm"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let o = superci(&home, &["caches", "delete", "--repo=acme/app", "--confirm"]);
    assert!(o.status.success() && String::from_utf8_lossy(&o.stdout).contains("acme/app's cache entries are out."), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(found(&main, "deps-main")["hit"], false);
    let (_, listed) = json(&home, &["caches", "list"]);
    assert_eq!(listed["repos"], serde_json::json!([]), "{listed}");
}

#[test]
fn the_runs_page_says_how_each_run_stands_how_long_it_took_and_the_job_it_waited_for() {
    let served = serve(|_| {});
    served.connect_github();
    let home = signed_in(&served);
    let now = served.now_ms();
    // Runs of acme/app as the control plane keeps their jobs: (run, attempt, workflow, each job's name, state,
    // conclusion, minutes from queued to started and to ended).
    type Job<'a> = (&'a str, &'a str, Option<&'a str>, u64, u64);
    let runs: Vec<(u64, u64, &str, Vec<Job>)> = vec![
        (9001, 1, "Build", vec![("lint", "done", Some("success"), 0, 1), ("e2e", "done", Some("success"), 2, 7)]),
        (9002, 1, "Tests", vec![("unit", "done", Some("failure"), 0, 2), ("types", "done", Some("success"), 0, 1)]),
        (9003, 1, "Deploy", vec![("ship", "done", Some("cancelled"), 0, 1)]),
        (9004, 1, "Docs", vec![("pages", "done", Some("skipped"), 0, 0), ("site", "done", Some("success"), 0, 1)]),
        (9005, 1, "Nightly", vec![("sweep", "done", None, 0, 3)]),
        (9006, 2, "Retry", vec![("flaky", "done", Some("success"), 0, 1)]),
        (9007, 1, "", vec![("old", "done", Some("success"), 0, 1)]),
        (9008, 1, "Live", vec![("long", "running", None, 0, 0)]),
    ];
    for (i, (run, attempt, workflow, jobs)) in runs.iter().enumerate() {
        for (name, state, conclusion, started, ended) in jobs {
            let (id, queued) = served.github.queue(name, "superci", now);
            assert_eq!(served.webhook(queued), 202);
            let key = format!("job:{id}");
            let mut m = served.store.0.lock().unwrap();
            let mut j: serde_json::Value = serde_json::from_str(&m[&key]).unwrap();
            let at = now - (10 - i as u64) * 3_600_000;
            for (k, v) in [("run_id", serde_json::json!(run)), ("attempt", serde_json::json!(attempt)), ("workflow", serde_json::json!(workflow)), ("name", serde_json::json!(name)), ("state", serde_json::json!(state)),
                ("conclusion", serde_json::json!(conclusion)), ("at_ms", serde_json::json!(at)), ("started_ms", serde_json::json!(at + started * 60_000))] { j[k] = v }
            if *state == "done" { j["ended_ms"] = serde_json::json!(at + ended * 60_000) }
            m.insert(key, j.to_string());
        }
    }
    let (mut child, link) = dashboard(&home);
    let (_, page) = part(&link, "/?p=runs&fragment=1");
    let said = words(&page);
    let row = |run: u64| { let at = page.find(&format!("id={run}&amp;")).unwrap_or_else(|| panic!("no run {run}: {said}")); let end = page[at..].find("</a>").map(|e| at + e).unwrap_or(page.len()); words(&page[at..end]) };
    // How each stands: passed (skipped jobs too), failed, cancelled, done without a word of how, running.
    for (run, word) in [(9001, "Passed"), (9002, "Failed"), (9003, "Cancelled"), (9004, "Passed"), (9005, "Done"), (9008, "Running")] { assert!(row(run).contains(word), "{run} {word}: {}", row(run)) }
    // The job a run of several waited for, and how long it waited for its machine; a run of one says none.
    assert!(row(9001).contains("Last to end: e2e, 5m after 2m waiting for a machine") && row(9001).contains("7m"), "{}", row(9001));
    assert!(row(9002).contains("Last to end: unit, 2m") && !row(9002).contains("waiting for a machine"), "{}", row(9002));
    assert!(!row(9003).contains("Last to end"), "{}", row(9003));
    // A second attempt says so; a run with no workflow's name goes by its repository.
    assert!(row(9006).contains("Retry") && row(9006).contains("attempt 2"), "{}", row(9006));
    assert!(row(9007).contains("app acme"), "{}", row(9007));
    // A run's own page: its jobs, and GitHub's page of that attempt.
    let (_, run) = part(&link, "/?p=run&repo=acme%2Fapp&id=9006&attempt=2&fragment=1");
    assert!(run.contains("https://github.com/acme/app/actions/runs/9006/attempts/2") && words(&run).contains("flaky"), "{}", words(&run));
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_github_job_cancelled_while_it_waits_for_room_or_before_its_first_word_came_starts_nothing() {
    // Room for one job at a time on its containers.
    let served = serve(|c| { c.routing.order = vec![superci_core::plane::Pool { max_jobs: Some(1), ..superci_core::plane::Pool::of("cloudflare") }]; });
    served.connect_github();
    let state = |id: u64| served.store.under(&format!("job:{id}")).into_iter().next().map(|(_, j)| j).unwrap_or_default();
    let (a, queued) = served.github.queue("lint", "superci", served.now_ms());
    assert_eq!(served.webhook(queued), 202);
    // A second waits for the room the first has; cancelled, it ends here, and the upkeep starts nothing for it.
    let (b, queued) = served.github.queue("docs", "superci", served.now_ms());
    assert_eq!(served.webhook(queued), 202);
    assert_eq!(state(b)["state"], "waiting", "{}", state(b));
    assert_eq!(served.webhook(served.github.cancel(b, served.now_ms())), 202);
    assert_eq!(state(b)["state"], "cancelled", "{}", state(b));
    assert_eq!(served.webhook(served.github.start(a)), 202);
    assert_eq!(served.webhook(served.github.finish(a, "success", &github_log(), served.now_ms())), 202);
    served.upkeep();
    assert_eq!(served.machines.0.lock().unwrap().len(), 1, "only the first's machine");
    // Its end came before its queued event (GitHub sends a job's events side by side): the queued one starts nothing.
    let (c, late) = served.github.queue("deploy", "superci", served.now_ms());
    assert_eq!(served.webhook(served.github.cancel(c, served.now_ms())), 202);
    served.webhook(late);
    assert!(!["launching", "launched", "running"].contains(&state(c)["state"].as_str().unwrap_or_default()), "{}", state(c));
    assert_eq!(served.machines.0.lock().unwrap().len(), 1, "no machine for it");
}

#[test]
fn a_job_past_a_spend_limit_fails_saying_which_limit_and_one_under_them_starts() {
    // $80 spent today on Cloudflare's containers (the only provider); a job queued past each limit.
    let refused = |set: fn(&mut superci_core::plane::Routing)| -> serde_json::Value {
        let served = serve(move |c| { c.routing.order = vec![superci_core::plane::Pool::of("cloudflare")]; set(&mut c.routing) });
        served.connect_github();
        let now = served.now_ms();
        {
            let mut m = served.store.0.lock().unwrap();
            m.insert("job:999".into(), serde_json::json!({ "job_id": 999, "run_id": 1, "repo": "acme/app", "state": "done", "cloud": "cloudflare", "at_ms": now - 60_000, "started_ms": now - 50_000, "ended_ms": now - 10_000, "cost_usd": 80.0, "installation_id": 1, "seen_in_progress": true }).to_string());
            m.insert(format!("day:{:06}", now / 86_400_000), serde_json::json!({ "spend": { "cloudflare": 80.0 }, "hours": [] }).to_string());
        }
        let (id, queued) = served.github.queue("test", "superci", now);
        served.webhook(queued);
        // (It fails at GitHub with the reason: a small machine's runner takes it and says why.)
        let j = served.store.under(&format!("job:{id}")).into_iter().next().map(|(_, j)| j).unwrap_or_default();
        assert!(j["state"] == "failed" && j["failed_fast"] == true, "{j}");
        j
    };
    let said = |j: &serde_json::Value| format!("{} {}", j["state"], j["error"]);
    let j = refused(|r| r.monthly_usd = Some(60.0));
    assert!(said(&j).contains("reached its limit of $60 a month ($80.00 spent on machines)"), "{}", said(&j));
    let j = refused(|r| r.daily_usd = Some(50.0));
    assert!(said(&j).contains("reached its limit of $50 a day ($80.00 spent on machines today)"), "{}", said(&j));
    let j = refused(|r| r.order[0].monthly_usd = Some(40.0));
    assert!(said(&j).contains("Cloudflare reached its $40 a month"), "{}", said(&j));
    let j = refused(|r| r.order[0].off = true);
    assert!(said(&j).contains("Cloudflare is turned off"), "{}", said(&j));
    // Under each: it starts.
    let served = serve(|c| { c.routing.order = vec![superci_core::plane::Pool { monthly_usd: Some(500.0), ..superci_core::plane::Pool::of("cloudflare") }]; c.routing.monthly_usd = Some(1000.0); c.routing.daily_usd = Some(900.0); });
    served.connect_github();
    let (id, queued) = served.github.queue("test", "superci", served.now_ms());
    served.webhook(queued);
    let j = served.store.under(&format!("job:{id}")).into_iter().next().map(|(_, j)| j).unwrap_or_default();
    assert!(j["state"] == "launched" && j["failed_fast"].is_null(), "{j}");
}

#[test]
fn a_further_organization_is_removed_from_the_dashboard_its_app_uninstalled_and_forgotten() {
    let served = serve(|_| {});
    served.connect_github();
    // A second organization's App beside acme's (as the dashboard keeps a further one).
    let mut globex = superci_testkit::GitHub::app();
    for (k, v) in [("id", serde_json::json!(2002)), ("slug", serde_json::json!("superci-globex")), ("owner", serde_json::json!("globex"))] { globex[k] = v }
    served.aws.put(&format!("/superci/{}/GITHUB_APP_2002", superci_testkit::PLANE), &globex.to_string());
    let home = signed_in(&served);
    let (mut child, link) = dashboard(&home);
    // (The control plane has both.)
    let connections = json(&home, &["connections", "list"]).1;
    assert!(connections["github"].as_array().unwrap().iter().any(|a| a["id"] == 2002 && a["owner"] == "globex"), "{connections}");
    // The first organization is not one to remove here (that is stopping SuperCI); one not here neither.
    for app in ["1001", "3003"] {
        let (status, _, said) = common::post(&link, "/github/remove", &[("app", app)]);
        assert!(status >= 400 && words(&said).contains("That organization is not one added here"), "{app}: {status} {}", words(&said));
    }
    // Removed: its App uninstalled at GitHub, its secret gone, and what to do on GitHub said.
    let (status, to, _) = common::post(&link, "/github/remove", &[("app", "2002")]);
    assert!(status == 303 && to.contains("p=repos"), "{status} {to}");
    assert!(served.github.uninstalled());
    assert!(served.aws.settings(superci_testkit::PLANE).iter().all(|(k, _)| k != "GITHUB_APP_2002"));
    let (_, page) = part(&link, "/?p=repos&fragment=1");
    assert!(words(&page).contains("Removed globex. Its App") && words(&page).contains("is uninstalled; delete the App itself on GitHub"), "{}", words(&page));
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_running_github_jobs_lines_as_its_machine_sends_them_are_read_before_github_has_its_log() {
    // Without a bucket (kept in the store), and with one (kept there).
    for bucket in [false, true] {
        let served = serve(move |c| {
            if bucket { (c.aws_own, c.aws_own_creds) = (Some(("111111111111".into(), "us-east-1".into())), Some(superci_core::aws::Credentials { access_key_id: "ASIAX".into(), secret_access_key: "secret".into(), session_token: Some("tok".into()), expires_at_ms: u64::MAX })); }
            c.routing.order = vec![superci_core::plane::Pool::of("cloudflare")];
        });
        if bucket { served.github.put_object(&format!("{}/{}", superci_core::cache::bucket(superci_testkit::PLANE, "us-east-1"), superci_core::cache::program_key("x86_64")), b"program") }
        served.connect_github();
        let home = signed_in(&served);
        let (id, queued) = served.github.queue("test", "superci", served.now_ms());
        assert_eq!(served.webhook(queued), 202);
        assert_eq!(served.webhook(served.github.start(id)), 202);
        // (Its machine's own link back, as the control plane writes one into an AWS machine's start-up script.)
        let hello = "machine-link-0123456789ab".to_string();
        let runner = {
            let key = format!("job:{id}");
            let mut m = served.store.0.lock().unwrap();
            let mut j: serde_json::Value = serde_json::from_str(&m[&key]).unwrap();
            j["hello"] = serde_json::json!(hello);
            m.insert(key, j.to_string());
            j["runner"].as_str().unwrap().to_string()
        };
        // Its runner's pages of a step's log, as its machine sends them (the second from where the first ended).
        let step = "aaaaaaaa-0000-0000-0000-000000000001";
        let first = "2026-10-15T10:00:01.0000000Z ##[group]Run make test\n2026-10-15T10:00:02.0000000Z test 1 passed\n";
        let second = "2026-10-15T10:00:03.0000000Z test 2 passed\n";
        let agent: ureq::Agent = ureq::Agent::config_builder().http_status_as_error(false).build().into();
        for (at, page) in [(0, first), (first.len(), second)] {
            let r = agent.post(&format!("{}/hello?runner={runner}&t={hello}&live=timeline_{step}_1.log&at={at}", served.url)).send(page).unwrap();
            assert_eq!(r.status().as_u16(), 200);
        }
        // An empty page is nothing to keep; a link not its own is not taken.
        assert_eq!(agent.post(&format!("{}/hello?runner={runner}&t={hello}&live=timeline_{step}_2.log&at=0", served.url)).send("").unwrap().status().as_u16(), 200);
        assert_eq!(agent.post(&format!("{}/hello?runner={runner}&t=not-its-link-at-all&live=x_1.log", served.url)).send("x").unwrap().status().as_u16(), 404);
        assert_eq!(served.github.objects().iter().filter(|(k, _)| k.contains("/live/")).count(), if bucket { 2 } else { 0 }, "{:?}", served.github.objects());
        let o = superci(&home, &["jobs", "retrieve", &id.to_string()]);
        let said = String::from_utf8_lossy(&o.stdout);
        assert!(o.status.success() && said.contains("test 1 passed") && said.contains("test 2 passed"), "bucket {bucket}: {said}{}", String::from_utf8_lossy(&o.stderr));
        let _ = std::fs::remove_dir_all(&home);
    }
}

#[test]
fn what_github_says_of_the_events_it_sent_is_read_and_one_it_could_not_deliver_is_sent_again_once() {
    let served = serve(|_| {});
    served.connect_github();
    let home = signed_in(&served);
    let id = ran(&served, "success");
    // Listed, newest first: each event, its action, and what the control plane answered.
    let o = superci(&home, &["deliveries", "list"]);
    let said = String::from_utf8_lossy(&o.stdout);
    assert!(o.status.success() && said.contains("workflow_job") && said.contains("queued") && said.contains("completed") && said.contains("202"), "{said}");
    // One whole: where it was sent, the job it was of, and the answer.
    let (_, listed) = json(&home, &["deliveries", "list"]);
    let first = listed[0]["deliveries"].as_array().unwrap().last().unwrap()["id"].as_u64().unwrap();
    let o = superci(&home, &["deliveries", "retrieve", &first.to_string()]);
    let said = String::from_utf8_lossy(&o.stdout);
    assert!(o.status.success() && said.contains(&format!("sent to {}/webhook", served.url)) && said.contains(&format!("job {id} test")) && said.contains("answer:"), "{said}");
    let o = superci(&home, &["deliveries", "retrieve", "1"]);
    assert!(String::from_utf8_lossy(&o.stdout).contains("GitHub would not say"), "{}", String::from_utf8_lossy(&o.stdout));
    // One GitHub could not deliver (the control plane was restarting): asked to be sent again, once.
    let (other, queued) = served.github.queue("lint", "superci", served.now_ms());
    served.github.delivered(&queued.0, 0, "", &format!("{}/webhook", served.url), served.now_ms());
    let failed = served.github.redelivered().len();
    served.upkeep();
    served.later(120_000);
    served.upkeep();
    let asked = served.github.redelivered();
    assert_eq!(asked.len(), failed + 1, "{asked:?} (job {other})");
}

#[test]
fn usage_says_what_jobs_cost_beside_githubs_runners_how_many_did_not_run_and_the_wait_for_a_machine() {
    let served = serve(|_| {});
    served.connect_github();
    let home = signed_in(&served);
    // A job that waited half a minute for its machine and ran ten minutes; one asking for what nothing here runs.
    let (id, queued) = served.github.queue("test", "superci", served.now_ms());
    assert_eq!(served.webhook(queued), 202);
    served.later(30_000);
    assert_eq!(served.webhook(served.github.start(id)), 202);
    served.later(600_000);
    assert_eq!(served.webhook(served.github.finish(id, "success", &github_log(), served.now_ms())), 202);
    let (_, refused) = served.github.queue("gpu", "superci/gpu=h100", served.now_ms());
    assert_eq!(served.webhook(refused), 202);
    let (code, v) = json(&home, &["usage", "retrieve", "--period=day"]);
    assert_eq!(code, 0, "{v}");
    let o = superci(&home, &["usage", "retrieve"]);
    let said = String::from_utf8_lossy(&o.stdout);
    // (Half a minute, and the moment the test itself took.)
    assert!(said.contains("2 jobs, 1 ran, 1 did not") && said.contains("saved") && (said.contains("Median wait for a machine: 30 s") || said.contains("Median wait for a machine: 31 s")), "{said}");
}

#[test]
fn a_personal_accounts_app_registers_its_runners_in_the_repository_and_its_manifest_goes_to_the_persons_settings() {
    // The App of a person's own account (not an organization's).
    let served = serve(|_| {});
    let mut app = superci_testkit::GitHub::app();
    app["owner_is_org"] = serde_json::json!(false);
    served.aws.put(&format!("/superci/{}/GITHUB_APP", superci_testkit::PLANE), &app.to_string());
    let (id, queued) = served.github.queue("test", "superci", served.now_ms());
    assert_eq!(served.webhook(queued), 202);
    let asked = || served.github.asked.lock().unwrap().clone();
    assert!(asked().iter().any(|a| a == "POST https://api.github.com/repos/acme/app/actions/runners/generate-jitconfig"), "{:?}", asked());
    // Cancelled before its runner took it: its runner taken back from the repository.
    assert_eq!(served.webhook(served.github.cancel(id, served.now_ms())), 202);
    assert!(asked().iter().any(|a| a.starts_with("DELETE https://api.github.com/repos/acme/app/actions/runners/")), "{:?}", asked());
    // Made from Repositories for a person: GitHub's page for an App of their own.
    let home = signed_in(&served);
    let (_child, link) = dashboard(&home);
    let (status, _, form) = common::post(&link, "/github/start", &[("gh-where", "public"), ("host", ""), ("login", "octocat")]);
    assert!(status == 200 && form.contains("https://github.com/settings/apps/new?state="), "{status} {}", words(&form));
    let (status, _, said) = common::post(&link, "/github/start", &[("gh-where", "public"), ("host", ""), ("login", "nobody-here")]);
    assert!(status == 400 && words(&said).contains("There is no GitHub organization or user named nobody-here"), "{status} {}", words(&said));
}

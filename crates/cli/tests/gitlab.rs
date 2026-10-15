//! GitLab's jobs end to end: the test plays GitLab (`superci_testkit::GitLab`: a job of `acme/app` pending, running,
//! ended, each its project's job event) against a control plane served on this computer with a GitLab connection;
//! the job is followed in the CLI and the dashboard. And GitLab connected from the dashboard, a project switched on.
mod common;
use common::{dashboard, json, part, signed_in, superci, words};
use superci_testkit::{gitlab, serve, Served};

/// GitLab's log of a job, as its trace has it (sections are its steps).
fn gitlab_log() -> String {
    ["\u{1b}[0KRunning with gitlab-runner 19.4.1", "\u{1b}[0Ksection_start:1760455360:get_sources\r\u{1b}[0K\u{1b}[36;1mGetting source from Git repository\u{1b}[0;m", "Fetching changes...",
        "\u{1b}[0Ksection_end:1760455363:get_sources\r\u{1b}[0K", "\u{1b}[0Ksection_start:1760455363:step_script\r\u{1b}[0K\u{1b}[36;1mExecuting \"step_script\" stage\u{1b}[0;m",
        "$ make test", "\u{1b}[32mall 2 passed\u{1b}[0m", "\u{1b}[0Ksection_end:1760455370:step_script\r\u{1b}[0K", "\u{1b}[32;1mJob succeeded\u{1b}[0;m"].join("\n")
}

/// A job of `acme/app` tagged `superci`, from pending to ended as GitLab tells it: its id.
fn ran(served: &Served, status: &str) -> u64 {
    let (id, pending) = served.gitlab.pending("test", &["superci"], served.now_ms());
    assert!(served.gitlab_webhook(pending) < 300);
    assert_eq!(served.machines.0.lock().unwrap().len(), 1, "its machine");
    let job = served.store.under(&format!("job:gl{id}")).into_iter().next().expect("the job kept").1;
    assert_eq!((job["provider"].as_str(), job["state"].as_str()), (Some("gitlab"), Some("launched")), "{job}");
    assert!(served.gitlab.asked.lock().unwrap().iter().any(|a| a == "POST https://gitlab.com/api/v4/user/runners"), "a runner for it");
    assert!(served.gitlab_webhook(served.gitlab.running(id, served.now_ms())) < 300);
    assert_eq!(served.store.under(&format!("job:gl{id}"))[0].1["state"], "running");
    assert!(served.gitlab_webhook(served.gitlab.finish(id, status, &gitlab_log(), served.now_ms())) < 300);
    id
}

#[test]
fn a_gitlab_job_runs_on_its_machine_and_is_read_in_the_cli_and_the_dashboard() {
    let served = serve(|_| {});
    served.connect_gitlab();
    let home = signed_in(&served);
    let id = ran(&served, "success");
    let job = served.store.under(&format!("job:gl{id}"))[0].1.clone();
    assert_eq!(job["state"], "done", "{job}");
    // Its runner is taken back once its job is over.
    assert!(served.gitlab.asked.lock().unwrap().iter().any(|a| a.starts_with("DELETE https://gitlab.com/api/v4/runners/") || a.starts_with("PUT https://gitlab.com/api/v4/runners/")), "{:?}", served.gitlab.asked.lock().unwrap());

    let jobs = json(&home, &["jobs", "list"]).1;
    let listed = &jobs["jobs"][0];
    assert_eq!((listed["job"].as_str(), listed["ended"].as_str()), (Some("test"), Some("passed")), "{jobs}");
    assert_eq!(listed["link"].as_str(), Some(format!("https://gitlab.com/acme/app/-/jobs/{id}").as_str()), "{jobs}");
    let o = superci(&home, &["jobs", "retrieve", &id.to_string()]);
    let said = String::from_utf8_lossy(&o.stdout);
    assert!(o.status.success() && said.contains("all 2 passed"), "{said}{}", String::from_utf8_lossy(&o.stderr));

    let (mut child, link) = dashboard(&home);
    let (_, page) = part(&link, &format!("/?p=job&id={id}&gl=&fragment=1"));
    assert!(words(&page).contains("On GitLab") && page.contains(&format!("https://gitlab.com/acme/app/-/jobs/{id}")), "{}", words(&page));
    let (_, view) = part(&link, &format!("/job/view?id={id}"));
    let view: serde_json::Value = serde_json::from_str(&view).unwrap();
    let steps: Vec<&str> = view["steps"].as_array().unwrap().iter().filter_map(|s| s["name"].as_str()).collect();
    assert!(steps.contains(&"Getting source from Git repository") && steps.contains(&"Executing \"step_script\" stage"), "{view}");
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn gitlab_is_connected_from_the_dashboard_and_a_project_switched_on_and_off() {
    let served = serve(|_| {});
    let home = signed_in(&served);
    let (mut child, link) = dashboard(&home);
    // A token that is not valid is said as that.
    let (status, _, refused) = common::post(&link, "/gitlab/connect", &[("which", "new"), ("url", "https://gitlab.com"), ("token", "wrong")]);
    assert!(status == 400 && words(&refused).contains("the token is not valid"), "{status} {}", words(&refused));
    let (status, _, said) = common::post(&link, "/gitlab/connect", &[("which", "new"), ("url", "https://gitlab.com"), ("token", gitlab::TOKEN)]);
    assert!(status < 400, "{status} {}", words(&said));
    let kept: serde_json::Value = serde_json::from_str(&served.aws.settings(superci_testkit::PLANE).into_iter().find(|(k, _)| k == "GITLAB").unwrap().1).unwrap();
    assert_eq!((kept["url"].as_str(), kept["token"].as_str()), (Some("https://gitlab.com"), Some(gitlab::TOKEN)));
    // Its projects, each off until switched on: a webhook to the control plane, with the connection's secret.
    let (_, page) = part(&link, "/?p=gitlab&fragment=1");
    assert!(words(&page).contains("acme/app"), "{}", words(&page));
    let (status, _, _) = common::post(&link, "/gitlab/project", &[("gitlab", ""), ("id", &gitlab::PROJECT.to_string()), ("enabled", "true")]);
    assert!(status < 400);
    assert!(served.gitlab.asked.lock().unwrap().iter().any(|a| a == &format!("POST https://gitlab.com/api/v4/projects/{}/hooks", gitlab::PROJECT)));
    let projects = json(&home, &["projects", "list"]).1;
    assert_eq!(projects["projects"][0]["enabled"], true, "{projects}");
    common::post(&link, "/gitlab/project", &[("gitlab", ""), ("id", &gitlab::PROJECT.to_string()), ("enabled", "false")]);
    assert_eq!(json(&home, &["projects", "list"]).1["projects"][0]["enabled"], false);
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_job_with_other_tags_is_left_alone_and_an_event_without_the_secret_refused() {
    let served = serve(|_| {});
    served.connect_gitlab();
    let (_, pending) = served.gitlab.pending("lint", &["docker"], served.now_ms());
    assert!(served.gitlab_webhook(pending.clone()) < 300);
    assert!(served.machines.0.lock().unwrap().is_empty());
    // (Sent with another secret.)
    let host = served.url.trim_start_matches("http://").to_string();
    let r = ureq::post(&format!("http://{host}/gitlab/webhook")).header("x-gitlab-token", "not-the-secret-at-all").config().http_status_as_error(false).build().send(&pending[..]).unwrap();
    assert_eq!(r.status().as_u16(), 401);
}

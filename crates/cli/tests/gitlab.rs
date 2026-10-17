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
    // Disconnected: its projects all off, and the connection kept as off.
    common::post(&link, "/gitlab/project", &[("gitlab", ""), ("id", &gitlab::PROJECT.to_string()), ("enabled", "true")]);
    let (status, to, _) = common::post(&link, "/gitlab/disconnect", &[("gitlab", "")]);
    assert!(status == 303 && to.contains("p=repos"), "{status} {to}");
    assert_eq!(served.aws.settings(superci_testkit::PLANE).into_iter().find(|(k, _)| k == "GITLAB").map(|(_, v)| v).as_deref(), Some("off"));
    assert!(served.gitlab.asked.lock().unwrap().iter().any(|a| a.starts_with(&format!("DELETE https://gitlab.com/api/v4/projects/{}/hooks/", gitlab::PROJECT))), "its webhook removed");
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

#[test]
fn gitlab_is_connected_and_a_project_switched_on_and_off_in_the_browser() {
    let Some(mut b) = superci_testkit::Browser::open(1300, 900) else { eprintln!("no Chrome here: skipped"); return };
    let served = serve(|_| {});
    let home = signed_in(&served);
    let (mut child, link) = dashboard(&home);
    let shows = |words: &str| format!("document.querySelector('main') && document.querySelector('main').innerText.includes({})", serde_json::json!(words));
    let enabled = || json(&home, &["projects", "list"]).1["projects"][0]["enabled"].as_bool();
    b.goto(&link);
    b.wait(&shows("Usage"));
    // Repositories: GitLab's Connect, its token pasted in the dialog (GitLab.com).
    b.click("nav a", "Repositories");
    b.wait(r#"[...document.querySelectorAll('main button')].some(x => x.textContent === 'Connect' && (x.getAttribute('onclick') || '').includes('gl-add'))"#);
    b.eval(r#"[...document.querySelectorAll('main button')].find(x => x.textContent === 'Connect' && (x.getAttribute('onclick') || '').includes('gl-add')).click()"#);
    b.wait("document.getElementById('gl-add').open");
    b.fill("#gl-add input[name=token]", gitlab::TOKEN);
    b.eval("document.querySelector('#gl-add input[name=token]').form.requestSubmit()");
    // Its page: its projects, each off until turned on (a webhook for its job events), and off again.
    b.wait(&shows("acme/app"));
    b.wait(r#"[...document.querySelectorAll('main form[action="/gitlab/project"] button')].some(x => x.textContent === 'Turn on')"#);
    b.eval(r#"[...document.querySelectorAll('main form[action="/gitlab/project"] button')].find(x => x.textContent === 'Turn on').click()"#);
    let until = |what: &str, holds: &dyn Fn() -> bool| { let end = std::time::Instant::now() + std::time::Duration::from_secs(15); while !holds() { assert!(std::time::Instant::now() < end, "{what}"); std::thread::sleep(std::time::Duration::from_millis(200)) } };
    until("the project on", &|| enabled() == Some(true));
    assert!(served.gitlab.asked.lock().unwrap().iter().any(|a| a == &format!("POST https://gitlab.com/api/v4/projects/{}/hooks", gitlab::PROJECT)));
    b.wait(r#"[...document.querySelectorAll('main form[action="/gitlab/project"] button')].some(x => x.textContent === 'Turn off') && !document.body.classList.contains('busy')"#);
    b.eval(r#"[...document.querySelectorAll('main form[action="/gitlab/project"] button')].find(x => x.textContent === 'Turn off').click()"#);
    until("the project off", &|| enabled() == Some(false));
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_gitlab_job_asking_for_windows_or_only_modal_is_said_to_have_nowhere_to_run() {
    // Modal's sandboxes beside Cloudflare's containers: neither runs Docker for GitLab on Windows, and Modal runs none.
    let served = serve(|c| { c.containers = true; c.launchers = vec![superci_core::plane::Launcher { cloud: "modal".into(), url: "https://acme--superci-runners.modal.run".into() }]; });
    served.connect_gitlab();
    for (tags, why) in [(&["superci/os=windows"][..], "runs no GitLab jobs on Windows"), (&["superci/provider=modal"], "Modal runs no Docker")] {
        let (id, pending) = served.gitlab.pending("test", tags, served.now_ms());
        assert!(served.gitlab_webhook(pending) < 300);
        let job = served.store.under(&format!("job:gl{id}")).into_iter().next().map(|(_, j)| j).unwrap_or_default();
        let error = job["error"].as_str().unwrap_or_default().to_string();
        assert!(error.contains(why), "{tags:?}: {job}");
    }
    assert!(served.machines.0.lock().unwrap().is_empty(), "no machine started");
}

#[test]
fn github_and_gitlab_jobs_side_by_side_are_each_shown_with_their_own_host_and_runs() {
    let served = serve(|_| {});
    served.connect_github();
    served.connect_gitlab();
    let home = signed_in(&served);
    // A GitLab job that failed, a GitHub job that passed.
    let gl = ran(&served, "failed");
    let (gh, queued) = served.github.queue("build", "superci", served.now_ms());
    assert_eq!(served.webhook(queued), 202);
    assert_eq!(served.webhook(served.github.start(gh)), 202);
    assert_eq!(served.webhook(served.github.finish(gh, "success", "2026-10-15T10:00:00.1000000Z ok", served.now_ms())), 202);
    let (mut child, link) = dashboard(&home);
    // Overview: the minutes as both code hosts bill them.
    let (_, page) = part(&link, "/?p=overview&fragment=1");
    assert!(words(&page).contains("as GitHub and GitLab bill them"), "{}", words(&page));
    // Runs: GitHub's run and GitLab's pipeline, each linked to its own host, one passed and one failed.
    let (_, page) = part(&link, "/?p=runs&fragment=1");
    let said = words(&page);
    assert!(said.contains("Pipeline") && said.contains("Passed") && said.contains("Failed"), "{said}");
    assert!(page.contains("/-/pipelines/") || page.contains("p=run&"), "{page}");
    // Another job of the pipeline, running: its page says it runs, and has no Cancel (a GitLab pipeline is not
    // cancelled from here yet).
    let (more, pending) = served.gitlab.pending("test 2", &["superci"], served.now_ms());
    assert!(served.gitlab_webhook(pending) < 300);
    assert!(served.gitlab_webhook(served.gitlab.running(more, served.now_ms())) < 300);
    let pipeline = served.store.under(&format!("job:gl{gl}"))[0].1["run_id"].as_u64().unwrap();
    assert_eq!(served.store.under(&format!("job:gl{more}"))[0].1["run_id"].as_u64(), Some(pipeline), "the same pipeline");
    // (The dashboard reads the control plane again every few seconds.)
    // (Asked as the page asks, every few seconds.)
    let end = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        std::thread::sleep(std::time::Duration::from_secs(4));
        if words(&part(&link, "/?p=jobs&fragment=1").1).contains("test 2") { break }
        assert!(std::time::Instant::now() < end, "the running job not shown");
    }
    let (_, page) = part(&link, &format!("/?p=run&repo=acme/app&id={pipeline}&attempt=0&fragment=1"));
    assert!(words(&page).contains("Running") && words(&page).contains("Jobs here 2") && !page.contains("Cancel run"), "{}", words(&page));
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn gitlab_jobs_cancelled_at_each_point_end_here_too_and_one_on_a_spot_machine_aws_took_back_is_run_again() {
    // Room for one job at a time on its containers, so a second one waits.
    let served = serve(|c| { c.routing.order = vec![superci_core::plane::Pool { max_jobs: Some(1), ..superci_core::plane::Pool::of("cloudflare") }]; });
    served.connect_gitlab();
    let state = |id: u64| served.store.under(&format!("job:gl{id}")).into_iter().next().map(|(_, j)| j).unwrap_or_default();
    let now = || served.now_ms();
    // Cancelled while its runner runs it: over here, its runner removed.
    let (a, pending) = served.gitlab.pending("lint", &["superci"], now());
    assert!(served.gitlab_webhook(pending) < 300);
    // A second while the first has the only room: it waits; cancelled, it ends here without a machine.
    let (b, pending) = served.gitlab.pending("docs", &["superci"], now());
    assert!(served.gitlab_webhook(pending) < 300);
    assert_eq!(state(b)["state"], "waiting", "{}", state(b));
    assert!(served.gitlab_webhook(served.gitlab.finish(b, "canceled", "", now())) < 300);
    assert_eq!(state(b)["state"], "cancelled", "{}", state(b));
    assert!(served.gitlab_webhook(served.gitlab.running(a, now())) < 300);
    let runners_before = served.gitlab.asked.lock().unwrap().iter().filter(|x| x.starts_with("DELETE https://gitlab.com/api/v4/runners/")).count();
    assert!(served.gitlab_webhook(served.gitlab.finish(a, "canceled", "", now())) < 300);
    assert_eq!((state(a)["state"].as_str(), state(a)["conclusion"].as_str()), (Some("cancelled"), Some("cancelled")), "{}", state(a));
    assert_eq!(served.gitlab.asked.lock().unwrap().iter().filter(|x| x.starts_with("DELETE https://gitlab.com/api/v4/runners/")).count(), runners_before + 1);
    // Cancelled before its runner took it: its machine is left to end (no job on it).
    let (c, pending) = served.gitlab.pending("build", &["superci"], now());
    assert!(served.gitlab_webhook(pending) < 300);
    assert_eq!(state(c)["state"], "launched", "{}", state(c));
    assert!(served.gitlab_webhook(served.gitlab.finish(c, "canceled", "", now())) < 300);
    assert_eq!(state(c)["state"], "orphan", "{}", state(c));
    // Cancelled before its first word came here (GitLab's events arrive side by side): its start, behind, starts nothing.
    let machines = served.machines.0.lock().unwrap().len();
    let (d, late) = served.gitlab.pending("deploy", &["superci"], now());
    assert!(served.gitlab_webhook(served.gitlab.finish(d, "canceled", "", now())) < 300);
    assert!(served.gitlab_webhook(late) < 300);
    assert_eq!(served.machines.0.lock().unwrap().len(), machines, "no machine for it");
    // On a spot machine AWS took back, it failed: run again (GitLab makes a new job of it), off spot machines.
    served.upkeep();
    let (e, pending) = served.gitlab.pending("test", &["superci"], now());
    assert!(served.gitlab_webhook(pending) < 300);
    assert!(served.gitlab_webhook(served.gitlab.running(e, now())) < 300);
    {
        let key = format!("job:gl{e}");
        let mut m = served.store.0.lock().unwrap();
        let mut j: serde_json::Value = serde_json::from_str(&m[&key]).unwrap();
        j["interrupted"] = serde_json::json!(true);
        m.insert(key, j.to_string());
    }
    assert!(served.gitlab_webhook(served.gitlab.finish(e, "failed", "", now())) < 300);
    let j = state(e);
    assert_eq!((j["state"].as_str(), j["error"].as_str()), (Some("failed"), Some("AWS took back its spot machine; SuperCI ran the job again by itself, off spot machines")), "{j}");
    assert!(served.gitlab.asked.lock().unwrap().iter().any(|x| x.ends_with(&format!("/jobs/{e}/retry"))), "{:?}", served.gitlab.asked.lock().unwrap());
}

#[test]
fn a_failed_gitlab_jobs_log_with_times_streams_and_nested_sections_is_shown_in_its_steps() {
    let served = serve(|_| {});
    served.connect_gitlab();
    let home = signed_in(&served);
    let (id, pending) = served.gitlab.pending("e2e", &["superci"], served.now_ms());
    assert!(served.gitlab_webhook(pending) < 300);
    assert!(served.gitlab_webhook(served.gitlab.running(id, served.now_ms())) < 300);
    // A runner with times on (each line its time and stream, "00O" out, "01E" error, "+" a line's rest): a section
    // whose heading comes on the next line, one inside another, and the script's, after which the job failed.
    let t = |n: u32, stream: &str, text: &str| format!("2026-10-15T10:00:{n:02}.123456Z {stream} {text}");
    let log = [
        t(0, "00O", "\u{1b}[0KRunning with gitlab-runner 19.4.1"),
        t(1, "00O", "\u{1b}[0Ksection_start:1760522401:prepare_executor\r\u{1b}[0K"),
        t(1, "00O+", "\u{1b}[0K\u{1b}[36;1mPreparing the \"docker\" executor\u{1b}[0;m"),
        t(2, "00O", "Using Docker executor with image node:22 ..."),
        t(3, "00O", "\u{1b}[0Ksection_end:1760522403:prepare_executor\r\u{1b}[0K"),
        t(4, "00O", "\u{1b}[0Ksection_start:1760522404:step_script\r\u{1b}[0K\u{1b}[36;1mExecuting \"step_script\" stage\u{1b}[0;m"),
        t(5, "00O", "\u{1b}[0Ksection_start:1760522405:install[collapsed=true]\r\u{1b}[0K\u{1b}[1mnpm ci\u{1b}[0m"),
        t(6, "00O", "added 312 packages"),
        t(7, "00O", "\u{1b}[0Ksection_end:1760522407:install\r\u{1b}[0K"),
        t(8, "01E", "WARNING: a deprecated option"),
        t(9, "01E", "\u{1b}[31m2 tests failed\u{1b}[0m\u{7}"),
        t(9, "00O", "\u{1b}[2Kprogress\u{1b}[1Gdone"),
        t(10, "00O", "\u{1b}[0Ksection_end:1760522410:step_script\r\u{1b}[0K"),
        t(10, "01E", "\u{1b}[31;1mERROR: Job failed: exit code 1\u{1b}[0;m"),
    ].join("\n");
    assert!(served.gitlab_webhook(served.gitlab.finish(id, "failed", &log, served.now_ms())) < 300);
    let (mut child, link) = dashboard(&home);
    let (_, view) = part(&link, &format!("/job/view?id={id}"));
    let view: serde_json::Value = serde_json::from_str(&view).unwrap_or_else(|_| panic!("{view}"));
    let steps: Vec<(String, String)> = view["steps"].as_array().unwrap().iter().map(|s| (s["name"].as_str().unwrap_or_default().to_string(), s["state"].as_str().unwrap_or_default().to_string())).collect();
    let named = |n: &str| steps.iter().find(|(name, _)| name == n).map(|(_, s)| s.clone()).unwrap_or_else(|| panic!("no step {n}: {steps:?}"));
    assert_eq!(named("Preparing the \"docker\" executor"), "success", "the heading on the next line names it");
    assert_eq!(named("Executing \"step_script\" stage"), "failure", "{steps:?}");
    let text = view.to_string();
    assert!(text.contains("npm ci") && text.contains("added 312 packages") && text.contains("2 tests failed") && !text.contains("00O") && !text.contains("01E"), "{text}");
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

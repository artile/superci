//! The CLI's commands that read, against a control plane served on this computer (`superci_testkit`), from a
//! workspace that keeps it and this computer's key (AWS's sign-in since ended), a run played to its end first; and
//! what changes say when there is no sign-in to make them with.
mod common;
use common::{home, json, played, superci};
use superci_testkit::serve;

#[test]
fn what_the_control_plane_has_is_read_with_the_kept_key() {
    let served = serve(|_| {});
    let home = home(&served);
    played(&served, &home.join("checkout"));

    let (code, status) = json(&home, &["status"]);
    assert_eq!(code, 0);
    assert_eq!((status["control_plane"]["version"].as_str(), status["control_plane"]["online"].as_bool(), status["control_plane"]["where"].as_str()),
        (Some(env!("CARGO_PKG_VERSION")), Some(true), Some("AWS · 111111111111 · us-east-1")));
    assert!(status["signed_in"].as_str().unwrap().contains("this machine's key"), "{status}");

    let (_, jobs) = json(&home, &["jobs", "list"]);
    let jobs = jobs["jobs"].as_array().unwrap();
    assert_eq!(jobs.len(), 1, "{jobs:?}");
    let (_, runs) = json(&home, &["runs", "list"]);
    assert_eq!(runs["runs"].as_array().map(Vec::len), Some(1), "{runs}");

    let (_, usage) = json(&home, &["usage", "retrieve"]);
    assert_eq!((usage["period"].as_str(), usage["usage"]["jobs"].as_u64(), usage["usage"]["ran"].as_u64()), (Some("day"), Some(1), Some(1)), "{usage}");
    assert_eq!(usage["usage"]["steps"].as_array().map(Vec::len), Some(24), "an hour each");
    let (_, month) = json(&home, &["usage", "retrieve", "--period=month"]);
    assert_eq!(month["usage"]["jobs"].as_u64(), Some(1), "{month}");

    let (_, caches) = json(&home, &["caches", "list"]);
    assert_eq!((caches["on"].as_bool(), caches["size_gb"].as_u64(), caches["days"].as_u64()), (Some(true), Some(25), Some(7)), "{caches}");
    let (_, keys) = json(&home, &["keys", "list"]);
    assert_eq!(keys["keys"], serde_json::json!([{ "name": "agent", "runs": true, "until": "2100-01-01" }]));
    let (_, labels) = json(&home, &["labels", "list"]);
    assert_eq!(labels["labels"], serde_json::json!(["superci"]));
    let (_, name) = json(&home, &["name", "retrieve"]);
    assert_eq!(name["name"], "SuperCI");
    let (_, runners) = json(&home, &["runners", "list"]);
    assert_eq!(runners["order"].as_array().map(Vec::len), Some(1), "{runners}");
    let (_, connections) = json(&home, &["connections", "list"]);
    assert_eq!(connections, serde_json::json!({ "github": [], "gitlab": [], "installations": [] }));

    // Limits, and nothing spent: 0, not -0 (an empty sum of floats is -0.0).
    let limits = superci(&home, &["limits", "retrieve", "--json"]);
    let text = String::from_utf8_lossy(&limits.stdout);
    assert!(!text.contains("-0.0"), "{text}");
    let limits: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!((limits["max_cpu"].as_u64(), limits["max_hours"].as_u64()), (Some(32), Some(6)));

    // Said to a person too.
    let said = String::from_utf8_lossy(&superci(&home, &["jobs", "list"]).stdout).into_owned();
    assert!(said.contains("tests") && said.contains("passed"), "{said}");

    // What cannot be done as things stand is said as it is.
    let (code, projects) = json(&home, &["projects", "list"]);
    assert_eq!((code, projects["error"].as_str()), (1, Some("GitLab is not connected.")), "{projects}");
    let (code, wiped) = json(&home, &["caches", "delete", "--repo=acme/app"]);
    assert!(code == 2 && wiped["error"].as_str().unwrap().contains("Add --confirm"), "{wiped}");
    // A change needs a sign-in to the control plane's own cloud: asked for, and nothing changed.
    for change in [&["name", "update", "Acme"][..], &["labels", "create", "gpu"], &["limits", "update", "--max-cpu=16"], &["keys", "create", "agent", "--runs"]] {
        let (code, refused) = json(&home, change);
        assert_eq!((code, refused["needs"].as_str()), (3, Some("superci login")), "{change:?}: {refused}");
    }
    assert_eq!(json(&home, &["name", "retrieve"]).1["name"], "SuperCI");
    let _ = std::fs::remove_dir_all(&home);
}

/// What a change asked of AWS, as the dashboard asks it (`fixtures/aws/settings`: recorded from a real account).
fn recorded(name: &str) -> serde_json::Value {
    let line: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(format!("{}/fixtures/aws/settings/{name}.jsonl", env!("CARGO_MANIFEST_DIR"))).unwrap()).unwrap();
    serde_json::from_str(line["asked"].as_str().unwrap()).unwrap()
}

#[test]
fn changes_are_written_where_the_control_plane_reads_them_and_it_answers_with_them() {
    let served = serve(|_| {});
    let home = common::signed_in(&served);
    let asked = |n: usize| { let a = served.aws.asked.lock().unwrap(); serde_json::from_str::<serde_json::Value>(a[a.len() - n]["asked"].as_str().unwrap()).unwrap() };
    let ok = |args: &[&str]| { let (code, v) = json(&home, &[args, &["--confirm"]].concat()); assert_eq!(code, 0, "{args:?}: {v}"); v };

    // Its name: asked of AWS exactly as the real change was, and read back from the control plane.
    ok(&["name", "update", "Recorded"]);
    assert_eq!(asked(1), recorded("01-name"));
    assert_eq!(json(&home, &["name", "retrieve"]).1["name"], "Recorded");
    // A label more, then taken away.
    ok(&["labels", "create", "recorded"]);
    let (labels, real) = (asked(1), recorded("03-label"));
    assert_eq!((&labels["Name"], &labels["Type"], &labels["Overwrite"]), (&real["Name"], &real["Type"], &real["Overwrite"]));
    assert_eq!(labels["Value"], r#"["recorded","superci"]"#);
    assert_eq!(json(&home, &["labels", "list"]).1["labels"], serde_json::json!(["recorded", "superci"]));
    ok(&["labels", "delete", "recorded"]);
    assert_eq!(json(&home, &["labels", "list"]).1["labels"], serde_json::json!(["superci"]));
    // The largest machine a label may ask for.
    ok(&["limits", "update", "--max-cpu=16"]);
    let (routing, real) = (asked(1), recorded("05-limits"));
    assert_eq!((&routing["Name"], &routing["Type"]), (&real["Name"], &real["Type"]));
    assert_eq!(serde_json::from_str::<serde_json::Value>(routing["Value"].as_str().unwrap()).unwrap()["max_cpu"], 16);
    assert_eq!(json(&home, &["limits", "retrieve"]).1["max_cpu"], 16);
    // The default machine.
    ok(&["machine", "update", "--cpu=8"]);
    assert_eq!(asked(1), recorded("07-machine"));
    assert_eq!(json(&home, &["machine", "retrieve"]).1["machine"], serde_json::json!({ "cpu": 8 }));
    ok(&["machine", "update", "--cpu=default"]);
    assert_eq!(json(&home, &["machine", "retrieve"]).1["machine"], serde_json::json!({}));
    // No Docker image cache for any job unless its label says so.
    ok(&["machine", "update", "--docker-cache=false"]);
    assert_eq!(json(&home, &["machine", "retrieve"]).1["machine"], serde_json::json!({ "docker_cache": false }));
    ok(&["machine", "update", "--docker-cache=default"]);
    assert_eq!(json(&home, &["machine", "retrieve"]).1["machine"], serde_json::json!({}));
    // With --dry-run, what it would do, and nothing done.
    let before = served.aws.asked.lock().unwrap().len();
    let (code, would) = json(&home, &["name", "update", "Other", "--dry-run"]);
    assert_eq!((code, would["dry_run"].as_bool(), would["would"].as_str(), would["form"]["to"].as_str()), (0, Some(true), Some("call it Other"), Some("/name")), "{would}");
    assert_eq!((served.aws.asked.lock().unwrap().len(), json(&home, &["name", "retrieve"]).1["name"].as_str()), (before, Some("Recorded")));
    assert!(served.aws.unanswered.lock().unwrap().is_empty(), "{:?}", served.aws.unanswered.lock().unwrap());
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn runners_limits_the_cache_and_keys_are_set_and_read_back() {
    let served = serve(|_| {});
    let home = common::signed_in(&served);
    let ok = |args: &[&str]| { let (code, v) = json(&home, args); assert_eq!(code, 0, "{args:?}: {v}"); v };
    let routing = || serde_json::from_str::<serde_json::Value>(&served.aws.settings(superci_testkit::PLANE).into_iter().find(|(k, _)| k == "ROUTING").map(|(_, v)| v).unwrap_or("{}".into())).unwrap();

    // A provider's jobs at once, its money a month, where its machines run; then back as they were.
    let runners = ok(&["runners", "update", "cloudflare", "--max-jobs=20", "--monthly-usd=50", "--location=weur"]);
    assert_eq!((runners["order"][0]["max_jobs"].as_u64(), runners["order"][0]["monthly_usd"].as_f64(), runners["cloudflare_location"].as_str()), (Some(20), Some(50.0), Some("weur")), "{runners}");
    assert_eq!(routing()["order"][0], serde_json::json!({ "cloud": "cloudflare", "max_jobs": 20, "monthly_usd": 50.0 }));
    // (What is used is the order's to say, for every provider alike.)
    let (code, off) = json(&home, &["runners", "update", "cloudflare", "--enabled=false"]);
    assert!(code == 2 && off["error"].as_str().unwrap().contains("superci runners order"), "{off}");
    let (code, regions) = json(&home, &["runners", "update", "cloudflare", "--regions=us-east-1"]);
    assert!(code == 2 && regions["error"].as_str().unwrap().contains("aws and aws-on-demand"), "{regions}");
    let back = ok(&["runners", "update", "cloudflare", "--max-jobs=none", "--monthly-usd=none"]);
    assert_eq!((back["order"][0]["taken_out"].as_bool(), back["order"][0]["max_jobs"].as_u64()), (Some(false), None), "{back}");
    let (code, wrong) = json(&home, &["runners", "update", "nowhere", "--max-jobs=2"]);
    assert!(code == 2 && wrong["error"].as_str().unwrap().contains("one of cloudflare"), "{wrong}");

    // The longest job, and what the CI may spend in a month and a day.
    assert_eq!(ok(&["limits", "update", "--max-hours=12"])["max_hours"], 12);
    let money = ok(&["limits", "update", "--monthly-usd=100", "--daily-usd=10"]);
    assert_eq!((money["monthly_usd"].as_f64(), money["daily_usd"].as_f64()), (Some(100.0), Some(10.0)), "{money}");
    assert_eq!((routing()["monthly_usd"].as_f64(), routing()["daily_usd"].as_f64()), (Some(100.0), Some(10.0)));
    let (code, refused) = json(&home, &["limits", "update", "--max-hours=500"]);
    assert!(code != 0 && refused["error"].as_str().unwrap().contains("120 hours"), "{refused}");
    // The AWS account's own limit (no budget in AWS itself: nothing asked of AWS for it).
    let account = ok(&["accounts", "update", "aws", "--monthly-usd=80", "--daily-usd=8"]);
    assert_eq!((account["monthly_usd"].as_f64(), account["daily_usd"].as_f64(), account["aws_budget"].as_str()), (Some(80.0), Some(8.0), None), "{account}");

    // The cache: what a repository keeps, how long an unused entry stays, and off.
    let cache = ok(&["caches", "update", "--size-gb=50", "--days=3"]);
    assert_eq!((cache["size_gb"].as_u64(), cache["days"].as_u64()), (Some(50), Some(3)), "{cache}");
    assert_eq!(ok(&["caches", "update", "--enabled=false"])["on"], false);
    let (code, wiped) = json(&home, &["caches", "delete", "--repo=acme/app", "--confirm"]);
    assert_eq!(code, 0, "{wiped}");

    // A run key: made (kept by the control plane as its hash), listed, ended.
    let key = ok(&["keys", "create", "builder", "--runs", "--days=7"]);
    assert_eq!((key["name"].as_str(), key["runs"].as_bool()), (Some("builder"), Some(true)), "{key}");
    let secret = key["env"]["SUPERCI_KEY"].as_str().unwrap().to_string();
    let kept: Vec<(String, String)> = served.aws.settings(superci_testkit::PLANE).into_iter().filter(|(k, _)| k.starts_with("READ_KEY_") && k.ends_with("_BUILDER")).collect();
    assert!(kept.len() == 1 && !kept[0].1.contains(&secret) && kept[0].1.ends_with(":runs"), "{kept:?}");
    assert!(ok(&["keys", "list"])["keys"].as_array().unwrap().iter().any(|k| k["name"] == "builder"));
    // It reads with no sign-in.
    let o = std::process::Command::new(env!("CARGO_BIN_EXE_superci")).args(["jobs", "list", "--json"]).env("SUPERCI_HOME", common::temp("bare")).env("SUPERCI_PLANE", &served.url).env("SUPERCI_KEY", &secret).output().unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    // A read key beside it, said as each is.
    ok(&["keys", "create", "reader"]);
    let listed = String::from_utf8_lossy(&superci(&home, &["keys", "list"]).stdout).into_owned();
    assert!(listed.lines().any(|l| l.starts_with("builder") && l.contains("run key: reads and starts runs")) && listed.lines().any(|l| l.starts_with("reader") && l.contains("read key, until")), "{listed}");
    ok(&["keys", "delete", "reader"]);
    ok(&["keys", "delete", "builder"]);
    assert!(!ok(&["keys", "list"])["keys"].as_array().unwrap().iter().any(|k| k["name"] == "builder"));
    assert!(served.aws.unanswered.lock().unwrap().is_empty(), "{:?}", served.aws.unanswered.lock().unwrap());
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_run_its_job_and_the_control_plane_are_read_signed_in() {
    let served = serve(|_| {});
    let home = common::signed_in(&served);
    common::played(&served, &home.join("checkout"));
    let text = |args: &[&str]| { let o = superci(&home, args); assert!(o.status.success(), "{args:?}: {}", String::from_utf8_lossy(&o.stderr)); String::from_utf8_lossy(&o.stdout).into_owned() };
    let run = json(&home, &["runs", "list"]).1["runs"][0].clone();
    let (run_id, job_id) = (run["id"].to_string(), run["job_ids"][0].to_string());

    // The run, with its jobs.
    let got = json(&home, &["runs", "retrieve", &run_id]).1;
    let one = &got["runs"][0];
    assert_eq!((one["state"].as_str(), one["run"].as_str(), one["job_list"][0]["job"].as_str(), one["job_list"][0]["ended"].as_str()), (Some("passed"), Some(".github/workflows/ci.yml"), Some("tests"), Some("passed")), "{got}");
    let said = text(&["runs", "retrieve", &run_id]);
    assert!(said.contains(".github/workflows/ci.yml") && said.contains("tests") && said.contains("passed"), "{said}");
    // Its jobs, and none of another run's (`--run`).
    let listed = json(&home, &["jobs", "list", &format!("--run={run_id}")]).1;
    assert_eq!(listed["jobs"].as_array().map(|j| j.iter().map(|j| j["id"].to_string()).collect::<Vec<_>>()), Some(vec![job_id.clone()]), "{listed}");
    assert!(json(&home, &["jobs", "list", "--run=12345"]).1["jobs"].as_array().is_some_and(|j| j.is_empty()));
    assert!(text(&["jobs", "list", "--run=12345"]).contains("No jobs of run 12345 among the latest"));
    assert_eq!(json(&home, &["jobs", "list", "--run=latest"]).0, 2);
    // Its job, with how its start went and its log (its last 1,000 bytes at least: here all of it).
    let job = json(&home, &["jobs", "retrieve", &job_id, "--bytes=200"]).1;
    assert_eq!(job["job"]["start"]["steps"].as_array().map(Vec::len), Some(2), "{job}");
    let log = job["log"].as_str().unwrap_or_default();
    assert!(job["truncated"] == false && log.contains("test 1 passed") && log.contains("test 12 passed"), "{job}");
    let said = text(&["jobs", "retrieve", &job_id]);
    assert!(said.contains("its machine was asked for") && said.contains("test 1 passed") && said.contains("test 12 passed"), "{said}");

    // Signed in: the control planes of the account, the images jobs run on in each region, the order of providers.
    let planes = json(&home, &["planes", "list"]).1;
    assert_eq!((planes["control_planes"][0]["id"].as_str(), planes["control_planes"][0]["in_use"].as_bool()), (Some("testplane0001"), Some(true)), "{planes}");
    assert!(text(&["planes", "list"]).contains("AWS · 111111111111 · us-east-1"));
    let images = json(&home, &["images", "list"]).1;
    let published: Vec<&str> = images["images"].as_array().unwrap().iter().filter(|i| i["published"] == true).filter_map(|i| i["region"].as_str()).collect();
    assert!(published.contains(&"us-east-1") && !published.contains(&"ca-central-1"), "{images}");
    assert!(text(&["images", "list"]).contains("us-east-1"));
    assert_eq!(json(&home, &["runners", "order", "cloudflare"]).1["order"], serde_json::json!(["cloudflare"]));
    let status = json(&home, &["status"]).1;
    assert_eq!(status["signed_in"], "AWS account 111111111111");
    assert!(text(&["status"]).contains("AWS account 111111111111"));
    // Usage by week, said to a person.
    assert_eq!(json(&home, &["usage", "retrieve", "--period=week"]).1["usage"]["jobs"], 1);
    let said = text(&["usage", "retrieve", "--period=week"]);
    assert!(said.contains('1'), "{said}");
    assert!(served.aws.unanswered.lock().unwrap().is_empty(), "{:?}", served.aws.unanswered.lock().unwrap());
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn the_control_plane_is_updated_as_aws_answered_a_real_account() {
    let served = serve(|_| {});
    let home = common::signed_in(&served);
    // (The recording's control plane is this one.)
    served.aws.replaying(include_str!("../fixtures/aws/planes/update.jsonl"), &[("testplane0002", superci_testkit::PLANE), ("http://new-plane.test", &served.url)]);
    let o = superci(&home, &["planes", "update"]);
    let said = String::from_utf8_lossy(&o.stdout);
    assert!(o.status.success() && said.contains(&format!("Updated to {}", env!("CARGO_PKG_VERSION"))), "{said}{}", String::from_utf8_lossy(&o.stderr));
    // Its new function went up; its role, table, address and schedule were checked.
    let asked: Vec<String> = served.aws.asked.lock().unwrap().iter().map(|a| format!("{} {}", a["method"].as_str().unwrap(), a["url"].as_str().unwrap())).collect();
    assert!(asked.iter().any(|a| a.starts_with("PUT https://lambda.us-east-1.amazonaws.com/2015-03-31/functions/superci-plane-testplane0001/code")), "{asked:?}");
    // The layer store's converter beside it, made (new in this version), not tried again when it fails.
    assert!(asked.iter().any(|a| a == "PUT https://lambda.us-east-1.amazonaws.com/2019-09-25/functions/superci-plane-testplane0001-convert/event-invoke-config"), "{asked:?}");
    assert!(served.aws.unanswered.lock().unwrap().is_empty(), "{:?}", served.aws.unanswered.lock().unwrap());
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn an_update_waits_for_jobs_on_cloudflare_says_a_launcher_it_could_not_update_and_what_aws_refused() {
    // Two jobs running on its Cloudflare containers: the update waits for them.
    let served = serve(|_| {});
    served.connect_github();
    for name in ["one", "two"] { let (id, q) = served.github.queue(name, "superci/provider=cloudflare", served.now_ms()); served.webhook(q); served.webhook(served.github.start(id)); }
    let home = common::signed_in(&served);
    let o = superci(&home, &["planes", "update"]);
    let said = String::from_utf8_lossy(&o.stderr).to_string() + &String::from_utf8_lossy(&o.stdout);
    assert!(!o.status.success() && said.contains("2 jobs are running on Cloudflare"), "{said}");
    let _ = std::fs::remove_dir_all(&home);
    // A launcher in Cloudflare, which is not signed in to here: the control plane is updated, the launcher said as not.
    let served = serve(|_| {});
    served.aws.put(&format!("/superci/{}/LAUNCHERS", superci_testkit::PLANE), r#"[{"cloud":"cloudflare","url":"https://superci-runners.acme.workers.dev"}]"#);
    served.aws.put(&format!("/superci/{}/CONTAINERS", superci_testkit::PLANE), "off");
    let home = common::signed_in(&served);
    served.aws.replaying(include_str!("../fixtures/aws/planes/update.jsonl"), &[("testplane0002", superci_testkit::PLANE), ("http://new-plane.test", &served.url)]);
    let o = superci(&home, &["planes", "update"]);
    let said = String::from_utf8_lossy(&o.stdout).to_string() + &String::from_utf8_lossy(&o.stderr);
    assert!(o.status.success() && said.contains("Its Cloudflare launcher was not updated: Cloudflare is not signed in to here."), "{said}");
    let _ = std::fs::remove_dir_all(&home);
    // AWS refusing its new code (as AWS words a throttled call): said, as AWS said it.
    let served = serve(|_| {});
    let home = common::signed_in(&served);
    served.aws.replaying(include_str!("../fixtures/aws/planes/update.jsonl"), &[("testplane0002", superci_testkit::PLANE), ("http://new-plane.test", &served.url)]);
    served.aws.fail(&format!("functions/superci-plane-{}/code", superci_testkit::PLANE), 429, &superci_testkit::json_error("TooManyRequestsException", "Rate exceeded"), None);
    let o = superci(&home, &["planes", "update"]);
    let said = String::from_utf8_lossy(&o.stdout).to_string() + &String::from_utf8_lossy(&o.stderr);
    assert!(!o.status.success() && said.contains("Rate exceeded"), "{said}");
    let _ = std::fs::remove_dir_all(&home);
}

/// A control plane from before 0.19.13 reads its launchers as AGENTS and its devices' keys as DASHBOARD_KEY_…: its
/// update writes LAUNCHERS before the new version goes up, and takes the old names out once that answers.
#[test]
fn a_control_plane_from_before_the_new_names_has_its_settings_moved_by_its_update() {
    let served = serve(|_| {});
    let home = common::signed_in(&served);
    served.aws.replaying(include_str!("../fixtures/aws/planes/update.jsonl"), &[("testplane0002", superci_testkit::PLANE), ("http://new-plane.test", &served.url)]);
    let launchers = r#"[{"cloud":"modal","url":"https://acme--superci-runners.modal.run"}]"#;
    let plane = superci_testkit::PLANE;
    served.aws.put(&format!("/superci/{plane}/AGENTS"), launchers);
    served.aws.put(&format!("/superci/{plane}/DASHBOARD_KEY_4102444800_TEST"), &served.key);
    served.aws.put(&format!("/superci/{plane}/DASHBOARD_KEY_1700000000_GONE"), "a-key-of-another-device");
    *served.aws.old_version.lock().unwrap() = Some("0.19.12".into());
    let o = superci(&home, &["planes", "update"]);
    let said = String::from_utf8_lossy(&o.stdout);
    assert!(o.status.success() && said.contains(&format!("Updated to {}", env!("CARGO_PKG_VERSION"))), "{said}{}", String::from_utf8_lossy(&o.stderr));
    let names: Vec<(String, String)> = served.aws.settings(plane);
    assert!(names.iter().any(|(k, v)| k == "LAUNCHERS" && v == launchers), "{names:?}");
    assert!(!names.iter().any(|(k, _)| k == "AGENTS" || k.starts_with("DASHBOARD_KEY_")), "{names:?}");
    assert!(names.iter().any(|(k, _)| k.starts_with("DEVICE_KEY_")), "{names:?}");
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn another_control_plane_is_deleted_as_aws_answered_a_real_account_and_the_one_in_use_is_not() {
    // The one in use, and another beside it in the same account (as the recording had them).
    let served = serve(|_| {});
    let other = superci_testkit::serve_as("testplane0002", |_| {});
    let home = common::signed_in(&served);
    served.aws.replaying(include_str!("../fixtures/aws/planes/delete.jsonl"), &[("http://new-plane.test", &other.url)]);
    for (k, v) in other.aws.settings("testplane0002") { served.aws.put(&format!("/superci/testplane0002/{k}"), &v) }
    let listed = json(&home, &["planes", "list"]).1;
    let ids: Vec<&str> = listed["control_planes"].as_array().unwrap().iter().filter_map(|p| p["id"].as_str()).collect();
    assert!(ids.contains(&"testplane0001") && ids.contains(&"testplane0002"), "{listed}");
    // The one in use is not deleted: that is stopping SuperCI.
    let o = superci(&home, &["planes", "delete", "testplane0001", "--confirm"]);
    assert!(!o.status.success() && String::from_utf8_lossy(&o.stderr).contains("in use"), "{}", String::from_utf8_lossy(&o.stderr));
    let (code, refused) = json(&home, &["planes", "delete", "testplane0002"]);
    assert!(code != 0 && refused["error"].as_str().unwrap().contains("--confirm"), "{refused}");
    let o = superci(&home, &["planes", "delete", "testplane0002", "--confirm"]);
    let said = String::from_utf8_lossy(&o.stdout);
    assert!(o.status.success() && said.contains("Deleted"), "{said}{}", String::from_utf8_lossy(&o.stderr));
    // Its settings are gone from SSM, its function, table and role from AWS; the one in use keeps its own.
    assert!(served.aws.settings("testplane0002").is_empty(), "{:?}", served.aws.settings("testplane0002"));
    assert!(!served.aws.settings(superci_testkit::PLANE).is_empty());
    let asked: Vec<String> = served.aws.asked.lock().unwrap().iter().map(|a| format!("{} {} {}", a["method"].as_str().unwrap(), a["url"].as_str().unwrap(), a["asked"].as_str().unwrap().chars().take(80).collect::<String>())).collect();
    for gone in ["DELETE https://lambda.us-east-1.amazonaws.com/2015-03-31/functions/superci-plane-testplane0002", "superci-plane-testplane0002\"}", "Action=DeleteRole&Version=2010-05-08&RoleName=superci-plane-testplane0002",
        "DELETE https://lambda.us-east-1.amazonaws.com/2015-03-31/functions/superci-plane-testplane0002-convert", "/aws/lambda/superci-plane-testplane0002-convert",
        "DELETE https://superci-plane-testplane0002-us-east-1.s3.us-east-1.amazonaws.com/ "] {
        assert!(asked.iter().any(|a| a.contains(gone)), "{gone}: {asked:?}");
    }
    assert!(served.aws.unanswered.lock().unwrap().is_empty(), "{:?}", served.aws.unanswered.lock().unwrap());
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn each_command_says_what_it_needs_and_what_it_would_do_and_changes_nothing() {
    let served = serve(|_| {});
    served.connect_github();
    let home = common::signed_in(&served);
    // (Its settings, without this computer's own keys: a command with a cloud sign-in hands the control plane one
    // of its own the first time it reaches it, as signing in does.)
    let settings = || served.aws.settings(superci_testkit::PLANE).into_iter().filter(|(k, _)| !k.starts_with("DEVICE_KEY_")).collect::<Vec<_>>();
    let before = settings();
    // What a command needs and is not given: said, as a usage error (exit 2).
    for (args, said) in [
        (&["limits", "update"][..], "Say what to change: `superci limits update"),
        (&["caches", "update"], "Say what to change: `superci caches update"),
        (&["caches", "update", "--enabled=maybe"], "--enabled is true or false"),
        (&["caches", "delete"], "Say whose"),
        (&["keys", "create"], "Give it a name"),
        (&["keys", "create", "agent", "--days=many"], "--days is a number of days"),
        (&["keys", "create", "agent", "--runs", "--max-cpu=8"], "A key has no limits of its own"),
        (&["keys", "delete"], "Say which: `superci keys delete NAME`"),
        (&["accounts", "update", "gcp", "--monthly-usd=5"], "Say which cloud's"),
        (&["accounts", "update", "aws"], "Say what to change: `superci accounts update aws"),
        (&["accounts", "update", "aws", "--aws-budget=maybe"], "--aws-budget is none, emails, stops or lift"),
        (&["deliveries", "retrieve", "x1"], "Say which: `superci deliveries retrieve ID`"),
        (&["runs", "cancel", "x1"], "Say which: `superci runs cancel ID`"),
        (&["runs", "retrieve", "x1"], "Say which: `superci runs retrieve ID`"),
        (&["artifacts", "list"], "Say which run: --run=ID"),
        (&["artifacts", "list", "--run=x1"], "Say which run: --run=ID"),
        (&["artifacts", "retrieve", "--run=12"], "Say which artifact"),
        (&["usage", "retrieve", "--period=year"], "--period is day, week or month"),
        (&["jobs", "list", "-x"], "-x is not a flag of superci"),
        (&["images", "create", "mars-1"], "Say where: `superci images create REGION`"),
        (&["images", "create", "ca-central-1", "--from=nope"], "--from is"),
        (&["images", "update", "mars-1"], "Say where: `superci images update REGION`"),
        (&["images", "delete", "mars-1"], "Say where: `superci images delete REGION --confirm`"),
        (&["planes", "delete"], "Say which: `superci planes delete ID --confirm`"),
        (&["planes", "create", "gcp"], "Say where: `superci planes create aws --region=us-east-1`"),
        (&["planes", "create", "aws"], "Say where: --region=us-east-1"),
        (&["runners", "create", "gcp"], "Say which: `superci runners create aws`"),
        (&["runners", "delete", "gcp"], "Say which: `superci runners delete aws --confirm`"),
        (&["jobs", "retrieve", "x1"], "Say which: `superci jobs retrieve ID`"),
        (&["login", "gcp"], "Say which: `superci login aws`"),
    ] {
        let (code, v) = json(&home, args);
        assert!(code == 2 && v["error"].as_str().unwrap_or_default().contains(said), "{args:?}: {code} {v}");
    }
    // Signed in already: said, and no sign-in begun (a further AWS account is `--add`).
    for args in [&["login", "aws"][..], &["login"]] {
        let o = superci(&home, args);
        assert!(o.status.success() && String::from_utf8_lossy(&o.stdout).contains("SuperCI is signed in on this computer"), "{args:?}: {}", String::from_utf8_lossy(&o.stdout));
    }
    // What deletes needs --confirm; with --dry-run, what it would do, and nothing done.
    let (code, v) = json(&home, &["caches", "delete", "--repo=acme/app"]);
    assert!(code != 0 && v["error"].as_str().unwrap_or_default().contains("--confirm"), "{v}");
    for (args, would) in [
        (&["caches", "delete", "--repo=acme/app", "--confirm", "--dry-run"][..], "take every cache entry of acme/app out"),
        (&["caches", "delete", "node-modules-1", "--repo=acme/app", "--scope=refs/heads/main", "--confirm", "--dry-run"], "take the cache entry node-modules-1 of acme/app out"),
        (&["keys", "create", "bot", "--dry-run"], "make a key named bot (a read key: it only reads), good for 30 days"),
        (&["keys", "create", "bot", "--runs", "--days=7", "--dry-run"], "make a key named bot (a run key: it reads and starts runs from a computer), good for 7 days"),
        (&["keys", "delete", "agent", "--dry-run"], "end the key named agent"),
        (&["accounts", "update", "aws", "--monthly-usd=50", "--aws-budget=none", "--dry-run"], "no budget in AWS itself"),
        (&["runs", "cancel", "12", "--dry-run"], "cancel run 12"),
        (&["images", "delete", "ca-central-1", "--confirm", "--dry-run"], "remove your copy of the image jobs run on from ca-central-1"),
        (&["runners", "delete", "aws", "--only-here", "--confirm", "--dry-run"], "leaving what SuperCI made for it there"),
        (&["runners", "create", "aws", "--dry-run"], "let aws run jobs for the control plane in use"),
        (&["planes", "delete", "testplane0002", "--confirm", "--dry-run"], "delete the control plane testplane0002"),
        (&["planes", "allow", "--dry-run"], "The AWS role of the control plane testplane0001 has what this version asks for: nothing would change."),
        (&["planes", "update", "--dry-run"], &format!("deploy the control plane in use (AWS · 111111111111 · us-east-1) again: it is on {} already", env!("CARGO_PKG_VERSION"))),
        (&["leave", "--confirm", "--dry-run"], "stop SuperCI"),
        (&["leave", "--plane=testplane0001", "--confirm", "--dry-run"], "stop the control plane testplane0001"),
    ] {
        let (code, v) = json(&home, args);
        assert!(code == 0 && v["dry_run"] == true && v["would"].as_str().unwrap_or_default().contains(would), "{args:?}: {code} {v}");
    }
    // A run not among those kept; deliveries GitHub has (none here).
    let (code, v) = json(&home, &["runs", "retrieve", "999"]);
    assert!(code != 0 && v["error"].as_str().unwrap_or_default().contains("not among the latest"), "{v}");
    let (code, v) = json(&home, &["deliveries", "list"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(settings(), before, "nothing changed");

    // With a read key (a coding agent's, elsewhere): what looks works, what changes is refused.
    let made = json(&home, &["keys", "create", "reader"]).1;
    // A key's name, how long it lasts, one there already, one not there: each said, nothing written.
    for (args, said) in [(&["keys", "create", "ci-2"][..], "A key's name is letters and digits"), (&["keys", "create", "abcdefghijklmnopqrstuvwxyz"], "A key's name is letters and digits"),
        (&["keys", "create", "bot", "--days=400"], "from 1 to 366 days"), (&["keys", "create", "reader"], "There is a key named reader already"), (&["keys", "delete", "nobody", "--confirm"], "There is no key named nobody")] {
        let (code, v) = json(&home, args);
        assert!(code != 0 && v["error"].as_str().unwrap_or_default().contains(said), "{args:?}: {code} {v}");
    }
    let (plane, key) = (made["env"]["SUPERCI_PLANE"].as_str().unwrap().to_string(), made["env"]["SUPERCI_KEY"].as_str().unwrap().to_string());
    let bare = common::temp("reader-home");
    let as_reader = |args: &[&str]| {
        let o = std::process::Command::new(env!("CARGO_BIN_EXE_superci")).args(args).arg("--json").env("SUPERCI_HOME", &bare).env("SUPERCI_PLANE", &plane).env("SUPERCI_KEY", &key).output().unwrap();
        (o.status.code().unwrap_or(-1), serde_json::from_slice::<serde_json::Value>(&o.stdout).unwrap_or_default())
    };
    let (code, v) = as_reader(&["jobs", "list"]);
    assert_eq!(code, 0, "{v}");
    let (code, v) = as_reader(&["caches", "delete", "--repo=acme/app", "--confirm"]);
    assert!(code != 0 && v["error"].as_str().unwrap_or_default().contains("only reads"), "{v}");
    let _ = std::fs::remove_dir_all(&bare);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn the_aws_accounts_own_budget_is_made_raised_made_to_stop_lifted_and_removed_as_aws_answered() {
    let served = serve(|_| {});
    let home = common::signed_in(&served);
    // Each step answered as a real account answered it (`fixtures/aws/budget`, its control plane's id this one's).
    let step = |file: &str| served.aws.replaying(&std::fs::read_to_string(format!("{}/fixtures/aws/budget/{file}", env!("CARGO_MANIFEST_DIR"))).unwrap(), &[("abcdefghijkl", superci_testkit::PLANE)]);
    let ok = |args: &[&str]| { let (code, v) = json(&home, args); assert_eq!(code, 0, "{args:?}: {v}"); v };
    let refused = |args: &[&str]| { let (code, v) = json(&home, args); assert_ne!(code, 0, "{args:?}: {v}"); v["error"].as_str().unwrap_or_default().to_string() };
    // Refused before AWS is asked: a budget with no monthly limit, an address that is none, nothing to lift.
    assert!(refused(&["accounts", "update", "aws", "--aws-budget=emails", "--email=someone@example.com"]).contains("A budget in AWS needs the monthly limit"));
    assert!(refused(&["accounts", "update", "aws", "--monthly-usd=100", "--aws-budget=emails", "--email=not an address"]).contains("That is no email address"));
    assert!(refused(&["accounts", "update", "aws", "--monthly-usd=100", "--aws-budget=lift"]).contains("There is no budget in AWS to lift"));
    // Made, with AWS's emails at 80% and 100%.
    step("02-emails.jsonl");
    let made = ok(&["accounts", "update", "aws", "--monthly-usd=100", "--aws-budget=emails", "--email=someone@example.com"]);
    assert_eq!((made["monthly_usd"].as_f64(), made["aws_budget"].as_str()), (Some(100.0), Some("emails")), "{made}");
    // The limit raised: the budget kept in step, unasked.
    step("04-raise.jsonl");
    assert_eq!(ok(&["accounts", "update", "aws", "--monthly-usd=120"])["aws_budget"], "emails");
    // Made to stop new machines at the limit (AWS knew the new role only after some seconds).
    step("06-stop.jsonl");
    assert_eq!(ok(&["accounts", "update", "aws", "--aws-budget=stops"])["aws_budget"], "stops");
    // The rule taken off the control plane's role; then all of it removed.
    step("08-lift.jsonl");
    ok(&["accounts", "update", "aws", "--aws-budget=lift"]);
    step("09-remove.jsonl");
    let gone = ok(&["accounts", "update", "aws", "--aws-budget=none"]);
    assert_eq!((gone["aws_budget"].as_str(), gone["monthly_usd"].as_f64()), (None, Some(120.0)), "{gone}");
    let asked: Vec<String> = served.aws.asked.lock().unwrap().iter().filter_map(|a| a["target"].as_str().map(str::to_string)).filter(|t| t.starts_with("AWSBudget")).collect();
    assert!(asked.iter().any(|t| t.ends_with("CreateBudget")) && asked.iter().any(|t| t.ends_with("DeleteBudget")), "{asked:?}");
    assert!(served.aws.unanswered.lock().unwrap().is_empty(), "{:?}", served.aws.unanswered.lock().unwrap());
}

#[test]
fn workspaces_are_listed_made_renamed_and_forgotten_from_the_cli() {
    let served = serve(|_| {});
    let home = common::signed_in(&served);
    let text = |args: &[&str]| { let o = superci(&home, args); (o.status.success(), String::from_utf8_lossy(&o.stdout).to_string() + &String::from_utf8_lossy(&o.stderr)) };
    let (ok, said) = text(&["workspaces", "list"]);
    assert!(ok && said.contains("default") && said.contains("AWS · 111111111111 · us-east-1"), "{said}");
    // Made (as a dry run first): empty, its id from its name; a second of that id refused.
    let (code, v) = json(&home, &["workspaces", "create", "Acme", "CI", "--dry-run"]);
    assert!(code == 0 && v["would"].as_str().unwrap_or_default().contains("make an empty workspace called Acme CI, with the id acme-ci"), "{v}");
    assert!(!home.join("workspaces/acme-ci").exists());
    let (ok, said) = text(&["workspaces", "create", "Acme", "CI"]);
    assert!(ok && said.contains("The workspace Acme CI is made, with nothing in it yet. Its id is acme-ci."), "{said}");
    let (ok, said) = text(&["workspaces", "create", "Acme", "CI"]);
    assert!(!ok && said.contains("There is a workspace with the id acme-ci already"), "{said}");
    let (_, said) = text(&["workspaces", "list"]);
    assert!(said.contains("acme-ci") && said.contains("nothing set up yet"), "{said}");
    // With several, status says which one it is of.
    let (code, status) = json(&home, &["status", "--workspace=default"]);
    assert!(code == 0 && status["workspace"] == "default" && status["workspaces"] == 2, "{status}");
    // Given another id (a dry run first); one not here refused.
    let (code, v) = json(&home, &["workspaces", "update", "acme-ci", "--id=acme", "--dry-run"]);
    assert!(code == 0 && v["would"].as_str().unwrap_or_default().contains("give the workspace acme-ci the id acme"), "{v}");
    let (ok, said) = text(&["workspaces", "update", "acme-ci", "--id=acme"]);
    assert!(ok && said.contains("The workspace acme-ci has the id acme now.") && home.join("workspaces/acme").exists(), "{said}");
    let (ok, _) = text(&["workspaces", "update", "nowhere", "--id=x2"]);
    assert!(!ok);
    // Forgotten (a dry run first, and only with --confirm): signed out, nothing in its clouds deleted.
    let (code, v) = json(&home, &["workspaces", "delete", "acme", "--confirm", "--dry-run"]);
    assert!(code == 0 && v["would"].as_str().unwrap_or_default().contains("sign the workspace acme out and forget it on this machine"), "{v}");
    let (ok, said) = text(&["workspaces", "delete", "acme"]);
    assert!(!ok && said.contains("--confirm"), "{said}");
    let (ok, said) = text(&["workspaces", "delete", "acme", "--confirm"]);
    assert!(ok && said.contains("The workspace acme is forgotten on this machine.") && !home.join("workspaces/acme").exists(), "{said}");
    // The one with a control plane: its key leaves the control plane as it is signed out.
    let keys = || served.aws.settings(superci_testkit::PLANE).into_iter().filter(|(k, _)| k.starts_with("DEVICE_KEY_")).count();
    let before = keys();
    let (ok, said) = text(&["workspaces", "delete", "default", "--confirm"]);
    assert!(ok && said.contains("The workspace default is forgotten"), "{said}");
    assert!(keys() < before, "its key gone from the control plane");
    let (ok, said) = text(&["workspaces", "list"]);
    assert!(ok && said.contains("None yet"), "{said}");
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn secrets_are_kept_for_runs_from_a_computer_from_stdin_or_a_variable_listed_without_values_and_taken_out() {
    let served = serve(|_| {});
    let home = common::signed_in(&served);
    let given = |args: &[&str], stdin: &str, env: &[(&str, &str)]| -> (i32, serde_json::Value) {
        use std::io::Write;
        let mut c = common::command(&home);
        c.args([args, &["--json"]].concat()).stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped());
        for (k, v) in env { c.env(k, v); }
        let mut child = c.spawn().unwrap();
        child.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
        let o = child.wait_with_output().unwrap();
        let text = String::from_utf8_lossy(&o.stdout);
        (o.status.code().unwrap_or(-1), serde_json::from_str(&text).unwrap_or_else(|_| panic!("{args:?}: {text}{}", String::from_utf8_lossy(&o.stderr))))
    };
    let kept = || served.aws.settings(superci_testkit::PLANE).into_iter().filter(|(k, _)| k.starts_with("SECRET_")).collect::<Vec<_>>();
    // Nothing given: refused, with how.
    let (code, v) = given(&["secrets", "create", "NPM_TOKEN"], "", &[]);
    assert!(code != 0 && v["error"].as_str().unwrap_or_default().contains("empty"), "{v}");
    let (code, v) = given(&["secrets", "create", "GITHUB_THING"], "x", &[]);
    assert!(code != 0 && v["error"].as_str().unwrap_or_default().contains("GitHub's own"), "{v}");
    // From stdin (its last line's end left out), and from a variable; each scope a setting of its own.
    let (code, v) = given(&["secrets", "create", "npm_token"], "npm-value-0001\n", &[]);
    assert!(code == 0 && v["name"] == "NPM_TOKEN", "{v}");
    let (code, v) = given(&["secrets", "create", "NPM_TOKEN"], "again", &[]);
    assert!(code != 0 && v["error"].as_str().unwrap_or_default().contains("already"), "{v}");
    let (code, v) = given(&["secrets", "create", "NPM_TOKEN", "--repo=acme/app", "--branch=release/*", "--value-env=MY_NPM"], "", &[("MY_NPM", "npm-value-0002")]);
    assert!(code == 0, "{v}");
    let (code, v) = given(&["secrets", "create", "OTHER", "--value-env=NOT_SET_HERE"], "", &[]);
    assert!(code != 0 && v["error"] == "NOT_SET_HERE is not set.", "{v}");
    let settings = kept();
    assert_eq!(settings.iter().map(|(k, _)| k.as_str()).filter(|k| *k == "SECRET_NPM_TOKEN_ALL").count(), 1, "{settings:?}");
    assert!(settings.iter().any(|(_, v)| v.contains("\"value\":\"npm-value-0001\"")) && settings.iter().any(|(_, v)| v.contains("npm-value-0002") && v.contains("release/*")), "{settings:?}");
    // Listed: names and scopes, never a value.
    let (code, v) = given(&["secrets", "list"], "", &[]);
    assert!(code == 0 && !v.to_string().contains("npm-value"), "{v}");
    assert_eq!(v["secrets"].as_array().unwrap().iter().map(|s| (s["name"].as_str().unwrap(), s["scope"]["repo"].as_str())).collect::<Vec<_>>(), [("NPM_TOKEN", None), ("NPM_TOKEN", Some("acme/app"))]);
    let o = superci(&home, &["secrets", "list"]);
    let text = String::from_utf8_lossy(&o.stdout);
    assert!(text.contains("repo acme/app, branch release/*") && text.contains("every run") && !text.contains("npm-value"), "{text}");
    // A new value for one kept; none for one that is not.
    let (code, v) = given(&["secrets", "update", "NPM_TOKEN"], "npm-value-0003", &[]);
    assert!(code == 0, "{v}");
    assert!(kept().iter().any(|(_, v)| v.contains("npm-value-0003")) && !kept().iter().any(|(_, v)| v.contains("npm-value-0001")));
    let (code, v) = given(&["secrets", "update", "OTHER"], "x", &[]);
    assert!(code != 0 && v["error"].as_str().unwrap_or_default().contains("not kept"), "{v}");
    // Taken out by name and scope, when confirmed.
    let (code, _) = given(&["secrets", "delete", "NPM_TOKEN"], "", &[]);
    assert_ne!(code, 0);
    let (code, v) = given(&["secrets", "delete", "NPM_TOKEN", "--repo=acme/web", "--confirm"], "", &[]);
    assert!(code != 0 && v["error"].as_str().unwrap_or_default().contains("repo acme/app, branch release/*"), "{v}");
    let (code, v) = given(&["secrets", "delete", "NPM_TOKEN", "--confirm"], "", &[]);
    assert!(code == 0, "{v}");
    assert_eq!(kept().len(), 1);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn the_devices_that_look_after_a_ci_are_listed_with_their_names_and_one_is_taken_out() {
    let served = serve(|_| {});
    let home = common::signed_in(&served);
    // Another computer's dashboard, which said what it is called.
    let other = "dashboard-key-of-another-computer-0002";
    served.aws.put(&format!("/superci/{}/DEVICE_KEY_4102444800_OTHER2", superci_testkit::PLANE), other);
    let hello = ureq::post(&format!("{}/keys/hello", served.url)).header("authorization", &format!("Bearer {other}")).send_json(serde_json::json!({ "device": "Ada's laptop" })).unwrap();
    assert_eq!(hello.status(), 200);
    // Listed: this one (its own key) and the other, by name.
    let (code, v) = json(&home, &["keys", "list"]);
    let devices = v["devices"].as_array().unwrap_or_else(|| panic!("{v}"));
    assert!(code == 0 && devices.iter().any(|c| c["name"] == "DEVICE_KEY_4102444800_TEST" && c["this_device"] == true), "{v}");
    assert!(devices.iter().any(|c| c["name"] == "DEVICE_KEY_4102444800_OTHER2" && c["device"] == "Ada's laptop" && c["last_used_ms"].is_u64()), "{v}");
    let o = superci(&home, &["keys", "list"]);
    assert!(String::from_utf8_lossy(&o.stdout).contains("Ada's laptop"), "{}", String::from_utf8_lossy(&o.stdout));
    // This computer's own is not taken out here (logout does that); the other one is.
    let (code, v) = json(&home, &["keys", "delete", "DEVICE_KEY_4102444800_TEST"]);
    assert!(code != 0 && v["error"].as_str().unwrap_or_default().contains("this computer's own key"), "{v}");
    let (code, v) = json(&home, &["keys", "delete", "DEVICE_KEY_4102444800_OTHER2"]);
    assert!(code == 0, "{v}");
    assert!(!served.aws.settings(superci_testkit::PLANE).iter().any(|(k, _)| k == "DEVICE_KEY_4102444800_OTHER2"), "gone from the CI");
    let gone = ureq::get(&format!("{}/keys", served.url)).header("authorization", &format!("Bearer {other}")).config().http_status_as_error(false).build().call().unwrap();
    assert_ne!(gone.status(), 200, "its key no longer reaches the CI");
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn what_a_job_had_from_the_caches_docker_images_is_said_and_keeping_them_is_turned_off_and_on() {
    let served = serve(|_| {});
    let home = common::signed_in(&served);
    common::played(&served, &home.join("checkout"));
    let job_id = json(&home, &["runs", "list"]).1["runs"][0]["job_ids"][0].to_string();
    // What its machine said after the job (see `layers-kept`), on its record.
    {
        let mut store = served.store.0.lock().unwrap();
        let key = format!("job:{job_id}");
        let mut j: serde_json::Value = serde_json::from_str(&store[&key]).unwrap();
        j["images"] = serde_json::json!({ "served": 103, "served_bytes": 2_700_000_000u64, "pulled": 12, "pulled_bytes": 300_000_000u64, "uploaded": 12, "uploaded_bytes": 300_000_000u64, "at": 1 });
        store.insert(key, j.to_string());
    }
    let (code, v) = json(&home, &["jobs", "retrieve", &job_id, "--bytes=100"]);
    assert_eq!((code, v["job"]["images"].as_str()), (0, Some("103 layers from the cache (2.7 GB); 12 layers pulled (300 MB), kept for the next job")), "{v}");
    let said = String::from_utf8_lossy(&superci(&home, &["jobs", "retrieve", &job_id, "--bytes=100"]).stdout).into_owned();
    assert!(said.contains("Docker images: 103 layers from the cache (2.7 GB); 12 layers pulled (300 MB), kept for the next job."), "{said}");
    // Pulled and not uploaded (or only some): said as that, never as kept.
    // (Kept: what was uploaded, and what the cache had already.)
    for (uploaded, had, said) in [(0, 0, "12 layers pulled (300 MB), none kept for the next job"), (5, 0, "12 layers pulled (300 MB), 5 of them kept for the next job (120 MB)"), (2, 10, "12 layers pulled (300 MB), kept for the next job")] {
        {
            let mut store = served.store.0.lock().unwrap();
            let key = format!("job:{job_id}");
            let mut j: serde_json::Value = serde_json::from_str(&store[&key]).unwrap();
            j["images"] = serde_json::json!({ "pulled": 12, "pulled_bytes": 300_000_000u64, "uploaded": uploaded, "uploaded_bytes": 120_000_000u64 * (uploaded > 0) as u64, "had": had, "had_bytes": 25_000_000u64 * had, "at": 1 });
            store.insert(key, j.to_string());
        }
        assert_eq!(json(&home, &["jobs", "retrieve", &job_id, "--bytes=100"]).1["job"]["images"].as_str(), Some(said));
    }
    // Why the rest was not kept, as its machine said.
    {
        let mut store = served.store.0.lock().unwrap();
        let key = format!("job:{job_id}");
        let mut j: serde_json::Value = serde_json::from_str(&store[&key]).unwrap();
        j["images"] = serde_json::json!({ "pulled": 12, "pulled_bytes": 900_000_000u64, "uploaded": 8, "uploaded_bytes": 180_000_000u64, "missing": 3, "missing_bytes": 716_000_000u64, "failed": 1, "note": "the bucket answered 403 Forbidden", "at": 1 });
        store.insert(key, j.to_string());
    }
    assert_eq!(json(&home, &["jobs", "retrieve", &job_id, "--bytes=100"]).1["job"]["images"].as_str(), Some("12 layers pulled (900 MB), 8 of them kept for the next job (180 MB): 3 layers with no whole file on the machine (716 MB), 1 layer not uploaded (the bucket answered 403 Forbidden)"));
    // The cache not set up on its machine: everything pulled, and why.
    {
        let mut store = served.store.0.lock().unwrap();
        let key = format!("job:{job_id}");
        let mut j: serde_json::Value = serde_json::from_str(&store[&key]).unwrap();
        j["images"] = serde_json::json!({ "pulled": 97, "pulled_bytes": 1_900_000_000u64, "had": 94, "had_bytes": 1_150_000_000u64, "uploaded": 3, "uploaded_bytes": 716_000_000u64, "off": "the kernel does not mount an EROFS file: Block device required (os error 15)", "at": 1 });
        store.insert(key, j.to_string());
    }
    assert_eq!(json(&home, &["jobs", "retrieve", &job_id, "--bytes=100"]).1["job"]["images"].as_str(), Some("97 layers pulled (1.9 GB), kept for the next job; the cache was not set up on its machine (the kernel does not mount an EROFS file: Block device required (os error 15))"));
    // One whose keeping failed says why.
    {
        let mut store = served.store.0.lock().unwrap();
        let key = format!("job:{job_id}");
        let mut j: serde_json::Value = serde_json::from_str(&store[&key]).unwrap();
        j["images"] = serde_json::json!({ "error": "containerd: connection refused", "at": 1 });
        store.insert(key, j.to_string());
    }
    assert_eq!(json(&home, &["jobs", "retrieve", &job_id, "--bytes=100"]).1["job"]["images"], "not kept (containerd: connection refused)");
    // Kept unless turned off; what is kept, said.
    let said = |args: &[&str]| String::from_utf8_lossy(&superci(&home, args).stdout).into_owned();
    assert!(said(&["caches", "retrieve"]).contains("Docker images: kept for each job's next run (Ubuntu 26.04 machines); none yet."), "{}", said(&["caches", "retrieve"]));
    let (code, v) = json(&home, &["caches", "update", "--images=false"]);
    assert_eq!(code, 0, "{v}");
    let routing = served.aws.settings(superci_testkit::PLANE).into_iter().find(|(k, _)| k == "ROUTING").map(|(_, v)| v).unwrap_or_default();
    assert!(routing.contains(r#""images_off":true"#), "{routing}");
    let (code, v) = json(&home, &["caches", "update", "--images=maybe"]);
    assert!(code == 2 && v["error"].as_str().unwrap_or_default().contains("--images is true or false"), "{v}");
    json(&home, &["caches", "update", "--images=true"]);
    let routing = served.aws.settings(superci_testkit::PLANE).into_iter().find(|(k, _)| k == "ROUTING").map(|(_, v)| v).unwrap_or_default();
    assert!(!routing.contains("images_off"), "{routing}");
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_control_plane_deleted_in_its_cloud_is_dropped_from_the_workspace() {
    let served = serve(|_| {});
    let home = common::signed_in(&served);
    assert!(String::from_utf8_lossy(&superci(&home, &["planes", "list"]).stdout).contains(superci_testkit::PLANE));
    // Deleted elsewhere (by someone else, or in AWS's console): the account has no role of it any more.
    let none = serde_json::json!({ "method": "POST", "url": "https://iam.amazonaws.com/", "target": "", "asked": "Action=ListRoles&Version=2010-05-08&MaxItems=1000", "status": 200,
        "answer": "<ListRolesResponse xmlns=\"https://iam.amazonaws.com/doc/2010-05-08/\"><ListRolesResult><IsTruncated>false</IsTruncated><Roles></Roles></ListRolesResult></ListRolesResponse>" });
    served.aws.replaying(&format!("{none}\n{none}\n{none}\n{none}\n"), &[]);
    let said = String::from_utf8_lossy(&superci(&home, &["planes", "list"]).stdout).into_owned();
    assert!(!said.contains(superci_testkit::PLANE), "{said}");
    let kept = std::fs::read_to_string(home.join("workspaces/default/sign-ins.json")).unwrap();
    assert!(!kept.contains(&format!("\"plane_id\":\"{}\"", superci_testkit::PLANE)), "{kept}");
    let _ = std::fs::remove_dir_all(&home);
}

/// The control plane in use no longer takes this computer's key (removed there, by another device's tidy or by
/// hand): it is found again with the sign-in, handed a new key, and read as before.
#[test]
fn a_control_plane_that_lost_this_computers_key_is_found_again_and_handed_a_new_one() {
    let served = serve(|_| {});
    let home = common::signed_in(&served);
    let plane = superci_testkit::PLANE;
    served.aws.params.lock().unwrap().remove(&format!("/superci/{plane}/DEVICE_KEY_4102444800_TEST"));
    let (code, v) = json(&home, &["status"]);
    assert_eq!(code, 0, "{v}");
    let keys: Vec<String> = served.aws.settings(plane).into_iter().map(|(k, _)| k).filter(|k| k.starts_with("DEVICE_KEY_")).collect();
    assert!(keys.len() == 1 && keys[0] != "DEVICE_KEY_4102444800_TEST", "{keys:?}");
    let kept = std::fs::read_to_string(home.join("workspaces/default/sign-ins.json")).unwrap();
    assert!(kept.contains(&keys[0]), "{kept}");
    // It said which device it is on, as a dashboard's key does (the CI's Keys name it).
    let noted = served.store.under(&format!("dkey:{}", keys[0]));
    assert!(noted.first().is_some_and(|(_, v)| v["device"].as_str().is_some_and(|d| !d.is_empty())), "{noted:?}");
    let _ = std::fs::remove_dir_all(&home);
}

/// A key a SuperCI from before 0.19.13 kept (as DASHBOARD_KEY_…, which control planes no longer read): this version
/// keeps it under the new name and hands it under it, instead of taking it for one that expired.
#[test]
fn a_key_an_older_superci_kept_is_kept_and_handed_under_the_new_name() {
    let served = serve(|_| {});
    let home = common::signed_in(&served);
    let plane = superci_testkit::PLANE;
    served.aws.params.lock().unwrap().remove(&format!("/superci/{plane}/DEVICE_KEY_4102444800_TEST"));
    served.aws.put(&format!("/superci/{plane}/DASHBOARD_KEY_4102444800_TEST"), &served.key);
    let file = home.join("workspaces/default/sign-ins.json");
    let kept = std::fs::read_to_string(&file).unwrap().replace("DEVICE_KEY_4102444800_TEST", "DASHBOARD_KEY_4102444800_TEST");
    std::fs::write(&file, kept).unwrap();
    let (code, v) = json(&home, &["runs", "list"]);
    assert_eq!(code, 0, "{v}");
    // (Kept under the new name, and handed under it: the test kit's key, not a folder's, is replaced as well.)
    let kept: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    let name = kept["key"]["name"].as_str().unwrap_or_default();
    assert!(name.starts_with("DEVICE_KEY_"), "{kept}");
    assert!(served.aws.settings(plane).iter().any(|(k, _)| k == name));
    let _ = std::fs::remove_dir_all(&home);
}

/// A control plane that answers and does not take this computer's key is said as that, with what to do: never as
/// "no runs yet" or a run not there.
#[test]
fn a_control_plane_that_does_not_take_this_computers_key_says_why() {
    let served = serve(|_| {});
    let home = common::home(&served);
    let plane = superci_testkit::PLANE;
    served.aws.params.lock().unwrap().remove(&format!("/superci/{plane}/DEVICE_KEY_4102444800_TEST"));
    let said = |args: &[&str]| { let (code, v) = json(&home, args); assert_ne!(code, 0, "{args:?}: {v}"); v["error"].as_str().unwrap_or_default().to_string() };
    // Replaced or expired (no sign-in here to hand a new one).
    for args in [&["runs", "list"][..], &["runs", "retrieve", "12345"]] {
        let e = said(args);
        assert!(e.contains("does not take this computer's key") && e.contains("superci login"), "{args:?}: {e}");
    }
    // Of a version from before the new names: it reads the key under its old name.
    *served.aws.old_version.lock().unwrap() = Some("0.19.11".into());
    let e = said(&["runs", "list"]);
    assert!(e.contains("runs SuperCI 0.19.11, which reads this computer's key under its old name"), "{e}");
    // Newer than this SuperCI: update it.
    *served.aws.old_version.lock().unwrap() = Some("99.0.0".into());
    let e = said(&["runs", "list"]);
    assert!(e.contains("newer than this SuperCI") && e.contains("npx @superci/cli@latest"), "{e}");
    let _ = std::fs::remove_dir_all(&home);
}

/// Commands started side by side on a computer with no key yet (a coding agent's, several at once) hand the control
/// plane one key between them, not one each: the first makes it, the others take it.
#[test]
fn commands_side_by_side_hand_one_key_between_them() {
    let served = serve(|_| {});
    let home = common::signed_in(&served);
    let file = home.join("workspaces/default/sign-ins.json");
    let mut kept: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    kept.as_object_mut().unwrap().remove("key");
    std::fs::write(&file, kept.to_string()).unwrap();
    let before = served.aws.settings(superci_testkit::PLANE).iter().filter(|(k, _)| k.starts_with("DEVICE_KEY_")).count();
    let children: Vec<_> = (0..6).map(|_| common::command(&home).args(["runs", "list", "--json"]).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).spawn().unwrap()).collect();
    for c in children { let o = c.wait_with_output().unwrap(); assert!(o.status.success(), "{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)); }
    let keys: Vec<String> = served.aws.settings(superci_testkit::PLANE).into_iter().map(|(k, _)| k).filter(|k| k.starts_with("DEVICE_KEY_")).collect();
    assert_eq!(keys.len(), before + 1, "{keys:?}");
    let _ = std::fs::remove_dir_all(&home);
}

/// GitHub disconnected from the CLI: --confirm needed; one not connected, and the control plane's own first App, said
/// as such.
#[test]
fn github_is_disconnected_from_the_cli() {
    let served = serve(|_| {});
    served.connect_github();
    let home = common::signed_in(&served);
    let owner = superci_testkit::github::OWNER;
    let (code, v) = json(&home, &["connections", "delete", "github", owner]);
    assert!(code != 0 && v["error"].as_str().unwrap_or_default().contains("--confirm"), "{v}");
    let (code, v) = json(&home, &["connections", "delete", "github", "globex", "--confirm"]);
    assert!(code != 0 && v["error"].as_str().unwrap_or_default().contains("globex is not connected here"), "{v}");
    // The first App connected is the control plane's own: it stays with it, and that is said.
    let (code, v) = json(&home, &["connections", "delete", "github", owner, "--confirm"]);
    assert!(code != 0 && v["error"].as_str().unwrap_or_default().contains("is the first this control plane was connected with"), "{v}");
    let _ = std::fs::remove_dir_all(&home);
}

/// Every limit spending counts against is listed, each with the command that changes it: the CI's, its AWS account's
/// (for a month and a day, with whether AWS itself emails), and a provider's own.
#[test]
fn limits_retrieve_lists_every_limit_with_the_command_that_changes_it() {
    let served = serve(|_| {});
    served.aws.put(&format!("/superci/{}/ROUTING", superci_testkit::PLANE), &serde_json::json!({ "monthly_usd": 200.0, "account_usd": { "aws": 100.0 }, "account_daily_usd": { "aws": 10.0 },
        "order": [{ "cloud": "aws" }, { "cloud": "cloudflare", "monthly_usd": 20.0 }] }).to_string());
    let home = common::signed_in(&served);
    let o = superci(&home, &["limits", "retrieve"]);
    let said = String::from_utf8_lossy(&o.stdout).into_owned();
    for line in ["The CI spent $0.00 on machines this month, of $200 allowed (`superci limits update --monthly-usd=N --daily-usd=N`)",
        "The AWS account spent $0.00 on machines this month, of $100 allowed (`superci accounts update aws --monthly-usd=N --daily-usd=N`)", "Today (UTC): $0.00 of $10 allowed.",
        "No budget in AWS itself, so AWS sends no email at 80% or 100%", "Cloudflare spent $0.00 this month, of $20 allowed (`superci runners update cloudflare --monthly-usd=N`)."] {
        assert!(said.contains(line), "{line}\n\n{said}");
    }
    let (_, v) = json(&home, &["limits", "retrieve"]);
    assert_eq!((v["aws_account"]["monthly_usd"].as_f64(), v["aws_account"]["daily_usd"].as_f64(), v["providers"][0]["cloud"].as_str()), (Some(100.0), Some(10.0), Some("cloudflare")), "{v}");
    let _ = std::fs::remove_dir_all(&home);
}

/// The image jobs run on, copied into a region that lacks it, as AWS answered a real copy into ca-central-1: made,
/// on its way (an update says so), listed while AWS makes it and once it is there, the newest build already, removed.
#[test]
fn the_runner_image_is_copied_in_listed_and_removed_as_aws_answered_a_real_copy() {
    let served = serve(|_| {});
    let home = common::signed_in(&served);
    let step = |recording: &str, args: &[&str]| -> String {
        served.aws.replaying(recording, &[]);
        let o = superci(&home, args);
        let said = format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
        said
    };
    let said = step(include_str!("../fixtures/aws/images/01-create.jsonl"), &["images", "create", "ca-central-1"]);
    assert!(said.contains("Copying the image jobs run on into ca-central-1, from us-east-1") && said.contains("`superci images delete ca-central-1 --confirm` removes it"), "{said}");
    let said = step(include_str!("../fixtures/aws/images/02-coming.jsonl"), &["images", "update", "ca-central-1"]);
    assert!(said.contains("A copy is on its way into ca-central-1 already"), "{said}");
    let said = step(include_str!("../fixtures/aws/images/03-list-pending.jsonl"), &["images", "list"]);
    assert!(said.lines().any(|l| l.starts_with("ca-central-1") && l.contains("is being made (pending)")), "{said}");
    let said = step(include_str!("../fixtures/aws/images/04-list-there.jsonl"), &["images", "list"]);
    assert!(said.lines().any(|l| l.starts_with("ca-central-1") && l.contains("your copy ami-") && l.contains("runs-on-v2.2-ubuntu24-full-x64-")), "{said}");
    let said = step(include_str!("../fixtures/aws/images/05-update-newest.jsonl"), &["images", "update", "ca-central-1"]);
    assert!(said.contains("Your copy in ca-central-1 is of the newest build"), "{said}");
    let said = step(include_str!("../fixtures/aws/images/06-delete.jsonl"), &["images", "delete", "ca-central-1", "--confirm"]);
    assert!(said.contains("Removed your copy of the image from ca-central-1, with its storage."), "{said}");
    let _ = std::fs::remove_dir_all(&home);
}

/// A limit an older control plane does not keep is refused, saying to update it: a day's before 0.16.0, the whole CI's
/// before 0.15.0. Nothing is written.
#[test]
fn a_limit_an_older_control_plane_does_not_keep_is_refused_saying_to_update_it() {
    let served = serve(|_| {});
    let home = common::signed_in(&served);
    let routing = || served.aws.settings(superci_testkit::PLANE).into_iter().find(|(k, _)| k == "ROUTING").map(|(_, v)| v);
    let before = routing();
    for (version, args, said) in [("0.15.3", &["limits", "update", "--monthly-usd=50", "--daily-usd=5"][..], "A limit for a day, and a budget in AWS itself, need a newer control plane"),
        ("0.14.0", &["limits", "update", "--monthly-usd=50"], "A limit for the whole CI or for a cloud account needs a newer control plane")] {
        *served.aws.old_version.lock().unwrap() = Some(version.into());
        let o = superci(&home, args);
        let e = String::from_utf8_lossy(&o.stderr).to_string() + &String::from_utf8_lossy(&o.stdout);
        assert!(!o.status.success() && e.contains(said) && e.contains("superci planes update"), "{version}: {e}");
        assert_eq!(routing(), before, "nothing written");
    }
    let _ = std::fs::remove_dir_all(&home);
}

/// A command that needs a cloud whose sign-in ended (noted by an earlier run) says so, with what to do, and changes
/// nothing: AWS's, Cloudflare's and Modal's.
#[test]
fn a_command_needing_a_sign_in_that_ended_says_so() {
    let served = serve(|_| {});
    for (flag, args, said) in [
        ("aws_ended", &["limits", "update", "--max-cpu=8"][..], "SuperCI's AWS sign-in has ended"),
        ("cloudflare_ended", &["runners", "create", "cloudflare"], "Cloudflare sign-in has expired"),
        ("modal_ended", &["runners", "create", "modal"], "Modal no longer accepts SuperCI's sign-in"),
    ] {
        let home = common::home(&served);
        let file = home.join("workspaces/default/sign-ins.json");
        let mut kept: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        kept[flag] = true.into();
        std::fs::write(&file, kept.to_string()).unwrap();
        let o = superci(&home, args);
        let e = String::from_utf8_lossy(&o.stderr).to_string() + &String::from_utf8_lossy(&o.stdout);
        assert!(!o.status.success() && e.contains(said), "{flag}: {e}");
        let _ = std::fs::remove_dir_all(&home);
    }
}

/// A key for a coding agent: a name that cannot be one refused; a control plane too old for keys, or for keys that
/// start runs, refused with what to do. Nothing is kept.
#[test]
fn a_key_that_cannot_be_made_is_refused_saying_why() {
    let served = serve(|_| {});
    let home = common::signed_in(&served);
    let keys = || served.aws.settings(superci_testkit::PLANE).into_iter().filter(|(k, _)| k.starts_with("READ_KEY_")).count();
    let before = keys();
    let refused = |args: &[&str], said: &str| {
        let o = superci(&home, args);
        let e = String::from_utf8_lossy(&o.stderr).to_string() + &String::from_utf8_lossy(&o.stdout);
        assert!(!o.status.success() && e.contains(said), "{args:?}: {e}");
    };
    refused(&["keys", "create", "not-a-name"], "A key's name is letters and digits, 24 at most");
    refused(&["keys", "create", "abcdefghijklmnopqrstuvwxy"], "A key's name is letters and digits, 24 at most");
    *served.aws.old_version.lock().unwrap() = Some("0.10.0".into());
    refused(&["keys", "create", "agent"], "read keys need control plane 0.11.0 or newer");
    *served.aws.old_version.lock().unwrap() = Some("0.17.0".into());
    refused(&["keys", "create", "builder", "--runs"], "keys that start runs need control plane 0.18.0 or newer");
    assert_eq!(keys(), before, "nothing kept");
    let _ = std::fs::remove_dir_all(&home);
}

/// What a command says when a cloud's sign-in is over: one noted as ended (it asks to sign in there), one the cloud
/// refuses as the command goes (Cloudflare's 9109, Modal's Unauthenticated, as each answered): said as ended, with what to
/// do. A token given by name for the run, refused, is said as the cloud said it (signing in would not replace it).
#[test]
fn a_command_whose_cloud_sign_in_is_over_or_refused_says_which_and_what_to_do() {
    let served = serve(|_| {});
    let run = |kept_extra: &[(&str, serde_json::Value)], env: &[(&str, &str)], recording: &str, args: &[&str]| -> (bool, String) {
        let home = common::signed_in(&served);
        let file = home.join("workspaces/default/sign-ins.json");
        let mut kept: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        for (k, v) in kept_extra { kept[*k] = v.clone() }
        std::fs::write(&file, kept.to_string()).unwrap();
        if !recording.is_empty() { served.aws.replaying(recording, &[]) }
        let o = common::command(&home).args(args).envs(env.iter().copied()).output().unwrap();
        let _ = std::fs::remove_dir_all(&home);
        (o.status.success(), String::from_utf8_lossy(&o.stderr).to_string() + &String::from_utf8_lossy(&o.stdout))
    };
    let cf = serde_json::json!({ "access": "example-access_token-ended", "expires_at_ms": 4102444800000u64, "refresh": "example-refresh_token-ended" });
    let modal = serde_json::json!({ "token_id": "ak-superciendedrecording00", "token_secret": "as-superciendedrecording00", "workspace": "acmeci" });
    let cf_ended = include_str!("../fixtures/cloudflare/sign-in/ended.jsonl");
    let modal_ended = include_str!("../fixtures/modal/sign-in/ended.jsonl");
    // Noted as ended, and the command needs that cloud: said as ended.
    let (ok, said) = run(&[("cloudflare_ended", true.into())], &[], "", &["planes", "create", "cloudflare"]);
    assert!(!ok && said.contains("Cloudflare sign-in has expired"), "{said}");
    let (ok, said) = run(&[("modal_ended", true.into())], &[], "", &["planes", "create", "modal"]);
    assert!(!ok && said.contains("Modal no longer accepts"), "{said}");
    // Refused by the cloud as the command goes: said as ended.
    let (ok, said) = run(&[("cloudflare", cf.clone())], &[], cf_ended, &["planes", "create", "cloudflare"]);
    assert!(!ok && said.contains("Cloudflare sign-in has expired"), "{said}");
    let (ok, said) = run(&[("modal", modal.clone())], &[], modal_ended, &["planes", "create", "modal"]);
    assert!(!ok && said.contains("Modal no longer accepts"), "{said}");
    // A token given for the run, refused: as the cloud said it, not as a sign-in that ended.
    let (ok, said) = run(&[], &[("SUPERCI_CLOUDFLARE_TOKEN", "a-token-cloudflare-refuses")], cf_ended, &["planes", "create", "cloudflare"]);
    assert!(!ok && !said.contains("sign-in has expired") && (said.contains("9109") || said.contains("Invalid access token")), "{said}");
    let (ok, said) = run(&[], &[("SUPERCI_MODAL_TOKEN_ID", "ak-superciendedrecording00"), ("SUPERCI_MODAL_TOKEN_SECRET", "as-superciendedrecording00")], modal_ended, &["planes", "create", "modal"]);
    assert!(!ok && said.contains("Token not found"), "{said}");
}

#[test]
fn a_region_is_added_with_its_network_and_its_network_deleted_as_aws_answered_a_real_account() {
    let served = serve(|c| {
        (c.aws_own, c.aws_own_creds) = (Some(("111111111111".into(), "us-east-1".into())), Some(superci_core::aws::Credentials { access_key_id: "ASIAX".into(), secret_access_key: "secret".into(), session_token: Some("tok".into()), expires_at_ms: u64::MAX }));
    });
    let home = common::signed_in(&served);
    let plane = superci_testkit::PLANE;
    // Stockholm beside N. Virginia: its network made in the account first, then the control plane told.
    served.aws.replaying(include_str!("../fixtures/aws/regions/add.jsonl"), &[("mixplane0001", plane)]);
    let o = superci(&home, &["runners", "update", "aws", "--regions=us-east-1,eu-north-1"]);
    assert!(o.status.success(), "{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
    let regions = served.aws.settings(plane).into_iter().find(|(k, _)| k == "AWS_REGIONS").map(|(_, v)| v).unwrap_or_default();
    assert_eq!(regions, r#"["us-east-1","eu-north-1"]"#);
    let asked = |action: &str| served.aws.asked.lock().unwrap().iter().any(|a| a["url"].as_str().unwrap_or_default().contains("eu-north-1") && a["asked"].as_str().unwrap_or_default().contains(&format!("Action={action}")));
    assert!(asked("CreateVpc") && asked("CreateSubnet") && asked("CreateVpcEndpoint"), "its network made there");
    // Its network there deleted (with its cache bucket), as removing AWS deletes it.
    served.aws.replaying(include_str!("../fixtures/aws/regions/delete.jsonl"), &[("mixplane0001", plane)]);
    let o = superci(&home, &["debug", "networks", "eu-north-1"]);
    assert!(o.status.success() && String::from_utf8_lossy(&o.stdout).contains("deleted in eu-north-1"), "{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
    assert!(asked("DeleteVpc"), "its network deleted");
    let o = superci(&home, &["debug", "networks", "mars-1"]);
    assert!(!o.status.success() && String::from_utf8_lossy(&o.stderr).contains("Say which region"));
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn the_control_plane_is_updated_with_its_cloudflare_and_modal_launchers_as_a_real_account_answered() {
    let served = serve(|_| {});
    let plane = superci_testkit::PLANE;
    served.aws.put(&format!("/superci/{plane}/LAUNCHERS"), r#"[{"cloud":"cloudflare","url":"https://superci-runners-testplane0001.acme.workers.dev"},{"cloud":"modal","url":"https://acme--superci-runners-testplane0001.modal.run"}]"#);
    served.aws.put(&format!("/superci/{plane}/CONTAINERS"), "off");
    let home = common::signed_in(&served);
    let file = home.join("workspaces/default/sign-ins.json");
    let mut kept: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    kept["cloudflare"] = serde_json::json!({ "access": "cloudflare-access-for-tests", "expires_at_ms": 4102444800000u64, "refresh": "cloudflare-refresh-for-tests" });
    kept["modal"] = serde_json::json!({ "token_id": "ak-modal-for-tests", "token_secret": "as-modal-for-tests", "workspace": "acmeorg" });
    std::fs::write(&file, kept.to_string()).unwrap();
    served.aws.replaying(include_str!("../fixtures/aws/planes/update-with-launchers.jsonl"), &[("mixplane0001", plane), ("http://new-plane.test", &served.url)]);
    // (The launchers' health, which the update waits for: a server here answers for each.)
    { let mut w = served.aws.workers.lock().unwrap(); for host in [format!("superci-runners-{plane}.acme.workers.dev"), format!("acmeorg--superci-runners-{plane}.modal.run")] { w.insert(host, served.url.clone()); } }
    let o = superci(&home, &["planes", "update"]);
    let said = String::from_utf8_lossy(&o.stdout).to_string() + &String::from_utf8_lossy(&o.stderr);
    assert!(o.status.success() && said.contains("Updated to") && !said.contains("was not updated"), "{said}");
    // Its Cloudflare launcher went up with it, on the image of GitHub's runner its containers run.
    let asked: Vec<String> = served.aws.asked.lock().unwrap().iter().map(|a| format!("{} {}", a["method"].as_str().unwrap_or_default(), a["url"].as_str().unwrap_or_default())).collect();
    assert!(asked.iter().any(|a| a.starts_with("PUT https://api.cloudflare.com/") && a.contains(&format!("/workers/scripts/superci-runners-{plane}"))), "{asked:?}");
    let _ = std::fs::remove_dir_all(&home);
}

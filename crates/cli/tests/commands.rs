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

    // A key for coding agents: made (kept by the control plane as its hash), listed, ended.
    let key = ok(&["keys", "create", "builder", "--runs", "--days=7"]);
    assert_eq!((key["name"].as_str(), key["runs"].as_bool()), (Some("builder"), Some(true)), "{key}");
    let secret = key["env"]["SUPERCI_KEY"].as_str().unwrap().to_string();
    let kept: Vec<(String, String)> = served.aws.settings(superci_testkit::PLANE).into_iter().filter(|(k, _)| k.starts_with("READ_KEY_") && k.ends_with("_BUILDER")).collect();
    assert!(kept.len() == 1 && !kept[0].1.contains(&secret) && kept[0].1.ends_with(":runs"), "{kept:?}");
    assert!(ok(&["keys", "list"])["keys"].as_array().unwrap().iter().any(|k| k["name"] == "builder"));
    // It reads with no sign-in.
    let o = std::process::Command::new(env!("CARGO_BIN_EXE_superci")).args(["jobs", "list", "--json"]).env("SUPERCI_HOME", common::temp("bare")).env("SUPERCI_PLANE", &served.url).env("SUPERCI_KEY", &secret).output().unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
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
    assert!(served.aws.unanswered.lock().unwrap().is_empty(), "{:?}", served.aws.unanswered.lock().unwrap());
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
    for gone in ["DELETE https://lambda.us-east-1.amazonaws.com/2015-03-31/functions/superci-plane-testplane0002", "superci-plane-testplane0002\"}", "Action=DeleteRole&Version=2010-05-08&RoleName=superci-plane-testplane0002"] {
        assert!(asked.iter().any(|a| a.contains(gone)), "{gone}: {asked:?}");
    }
    assert!(served.aws.unanswered.lock().unwrap().is_empty(), "{:?}", served.aws.unanswered.lock().unwrap());
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn the_order_takes_providers_out_and_puts_them_back_and_each_of_awss_has_its_own_regions() {
    // A control plane whose own AWS account runs jobs (its spot and on-demand machines), with Cloudflare's containers.
    let served = serve(|c| {
        c.containers = true;
        (c.aws_own, c.aws_own_creds) = (Some(("111111111111".into(), "us-east-1".into())), Some(superci_core::aws::Credentials { access_key_id: "ASIAX".into(), secret_access_key: "secret".into(), session_token: Some("tok".into()), expires_at_ms: u64::MAX }));
    });
    let home = common::signed_in(&served);
    let text = |args: &[&str]| { let o = superci(&home, args); String::from_utf8_lossy(&o.stdout).to_string() };
    // The order: the providers named, in that order; the others taken out (kept, and put back by naming them).
    let order = json(&home, &["runners", "order", "cloudflare"]).1;
    assert_eq!((order["order"].clone(), order["taken_out"].clone()), (serde_json::json!(["cloudflare"]), serde_json::json!(["aws", "aws-on-demand"])), "{order}");
    assert!(text(&["runners", "list"]).contains("Taken out of the order: aws, aws-on-demand"));
    let order = json(&home, &["runners", "order", "aws", "cloudflare", "aws-on-demand"]).1;
    assert_eq!((order["order"].clone(), order["taken_out"].clone()), (serde_json::json!(["aws", "cloudflare", "aws-on-demand"]), serde_json::json!([])), "{order}");
    // AWS's on-demand machines in regions of their own (here: N. Virginia, where the control plane is set up already).
    let listed = json(&home, &["runners", "update", "aws-on-demand", "--regions=us-east-1"]).1;
    let on_demand = listed["order"].as_array().unwrap().iter().find(|p| p["cloud"] == "aws-on-demand").cloned().unwrap_or_default();
    assert_eq!((on_demand["regions"].clone(), on_demand["taken_out"].as_bool()), (serde_json::json!(["us-east-1"]), Some(false)), "{listed}");
    assert!(text(&["runners", "list"]).contains("aws-on-demand  in us-east-1"));
    // Not one left in the order: refused (no job could start).
    let (code, none) = json(&home, &["runners", "order", "nowhere"]);
    assert_eq!(code, 2, "{none}");
    // In the dashboard: its rows, AWS's two alike; each one's Take out, and Put back (to the end of the order).
    let (mut child, link) = common::dashboard(&home);
    let routing = || serde_json::from_str::<serde_json::Value>(&served.aws.settings(superci_testkit::PLANE).into_iter().find(|(k, _)| k == "ROUTING").map(|(_, v)| v).unwrap_or("{}".into())).unwrap();
    let (_, page) = common::part(&link, "/?p=runners&fragment=1");
    assert!(page.contains("<strong>AWS spot</strong>") && page.contains("<strong>AWS on-demand</strong>") && page.contains("<h2>Clouds</h2>") && page.matches("data-regions").count() == 2, "{}", common::words(&page));
    let current = "aws,cloudflare,aws-on-demand";
    let (status, to, said) = common::post(&link, "/routing", &[("action", "take_out"), ("cloud", "aws"), ("current", current)]);
    assert!(status == 303 && to.contains("p=runners"), "{status} {said}");
    assert_eq!(routing()["order"], serde_json::json!([{ "cloud": "cloudflare" }, { "cloud": "aws-on-demand", "regions": ["us-east-1"] }, { "cloud": "aws", "off": true, "regions": ["us-east-1"] }]), "(spot's regions kept as they were when on-demand's were set)");
    let (_, page) = common::part(&link, "/?p=runners&fragment=1");
    assert!(common::words(&page).contains("AWS spot Taken out of the order") && !page.contains(r#"data-cloud="aws""#), "{}", common::words(&page));
    common::post(&link, "/routing", &[("action", "put_back"), ("cloud", "aws"), ("current", "cloudflare,aws-on-demand,aws")]);
    assert_eq!(routing()["order"], serde_json::json!([{ "cloud": "cloudflare" }, { "cloud": "aws-on-demand", "regions": ["us-east-1"] }, { "cloud": "aws", "regions": ["us-east-1"] }]));
    // The last one in the order is not taken out.
    common::post(&link, "/routing", &[("action", "take_out"), ("cloud", "aws"), ("current", "cloudflare,aws-on-demand,aws")]);
    common::post(&link, "/routing", &[("action", "take_out"), ("cloud", "aws-on-demand"), ("current", "cloudflare,aws-on-demand,aws")]);
    let (status, _, said) = common::post(&link, "/routing", &[("action", "take_out"), ("cloud", "cloudflare"), ("current", "cloudflare,aws-on-demand,aws")]);
    assert!(status == 400 && said.contains("Keep one provider in the order"), "{status} {said}");
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn each_command_says_what_it_needs_and_what_it_would_do_and_changes_nothing() {
    let served = serve(|_| {});
    served.connect_github();
    let home = common::signed_in(&served);
    let settings = || served.aws.settings(superci_testkit::PLANE);
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
    ] {
        let (code, v) = json(&home, args);
        assert!(code == 2 && v["error"].as_str().unwrap_or_default().contains(said), "{args:?}: {code} {v}");
    }
    // What deletes needs --confirm; with --dry-run, what it would do, and nothing done.
    let (code, v) = json(&home, &["caches", "delete", "--repo=acme/app"]);
    assert!(code != 0 && v["error"].as_str().unwrap_or_default().contains("--confirm"), "{v}");
    for (args, would) in [
        (&["caches", "delete", "--repo=acme/app", "--confirm", "--dry-run"][..], "take every cache entry of acme/app out"),
        (&["caches", "delete", "node-modules-1", "--repo=acme/app", "--scope=refs/heads/main", "--confirm", "--dry-run"], "take the cache entry node-modules-1 of acme/app out"),
        (&["keys", "create", "bot", "--dry-run"], "make a key named bot that only reads, good for 30 days"),
        (&["keys", "create", "bot", "--runs", "--days=7", "--dry-run"], "make a key named bot that reads and starts runs from a computer, good for 7 days"),
        (&["keys", "delete", "agent", "--dry-run"], "end the key named agent"),
        (&["accounts", "update", "aws", "--monthly-usd=50", "--aws-budget=none", "--dry-run"], "no budget in AWS itself"),
        (&["runs", "cancel", "12", "--dry-run"], "cancel run 12"),
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

    // With a key that only reads (an agent's, elsewhere): what looks works, what changes is refused.
    let made = json(&home, &["keys", "create", "reader"]).1;
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

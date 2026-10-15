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
    // (Only AWS's on-demand machines are turned off: another provider is removed.)
    let (code, off) = json(&home, &["runners", "update", "cloudflare", "--enabled=false"]);
    assert!(code == 2 && off["error"].as_str().unwrap().contains("superci runners delete cloudflare"), "{off}");
    let back = ok(&["runners", "update", "cloudflare", "--max-jobs=none", "--monthly-usd=none"]);
    assert_eq!((back["order"][0]["off"].as_bool(), back["order"][0]["max_jobs"].as_u64()), (Some(false), None), "{back}");
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

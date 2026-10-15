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

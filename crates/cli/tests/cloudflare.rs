//! The Cloudflare control plane as Cloudflare runs it (its Worker and Durable Object, built, in Cloudflare's own
//! runtime on this computer: `superci_testkit::serve_cloudflare`), read and used by the CLI. Skipped where `wrangler`
//! is not installed or the Worker is not built.
#![cfg(unix)]
mod common;
use common::temp;
use std::process::Command;
use superci_testkit::{serve_cloudflare, CloudflarePlane};

/// A workspace that keeps it and the key it takes.
fn home(p: &CloudflarePlane) -> std::path::PathBuf {
    let home = temp("cf-home");
    let ws = home.join("workspaces/default");
    std::fs::create_dir_all(&ws).unwrap();
    std::fs::write(ws.join("sign-ins.json"), serde_json::json!({
        "key": { "name": "DASHBOARD_KEY_4102444800_TEST", "value": p.key, "planes": [p.plane] },
        "plane": { "Cloudflare": { "account_id": "1111111111111111111111111111111a", "account_name": "Acme", "script": format!("superci-plane-{}", p.plane), "url": p.url, "plane_id": p.plane, "label": "superci" } },
    }).to_string()).unwrap();
    home
}

fn json(home: &std::path::Path, env: &[(&str, &str)], args: &[&str]) -> (i32, serde_json::Value) {
    let mut c = Command::new(env!("CARGO_BIN_EXE_superci"));
    c.args(args).arg("--json").env("SUPERCI_HOME", home).env_remove("SUPERCI_PLANE").env_remove("SUPERCI_KEY");
    for (k, v) in env { c.env(k, v); }
    let o = c.output().unwrap();
    let text = String::from_utf8_lossy(&o.stdout);
    (o.status.code().unwrap_or(-1), serde_json::from_str(&text).unwrap_or_else(|_| panic!("{args:?}: {text}{}", String::from_utf8_lossy(&o.stderr))))
}

#[test]
fn the_cloudflare_control_plane_answers_with_its_keys_and_keeps_a_run_from_a_computer() {
    let Some(plane) = serve_cloudflare() else { eprintln!("no wrangler or no built Worker here: skipped"); return };
    let home = home(&plane);
    // Read with this computer's key.
    let (code, status) = json(&home, &[], &["status"]);
    assert_eq!(code, 0, "{status}");
    assert_eq!((status["control_plane"]["cloud"].as_str(), status["control_plane"]["version"].as_str(), status["control_plane"]["online"].as_bool()), (Some("cloudflare"), Some(env!("CARGO_PKG_VERSION")), Some(true)), "{status}");
    assert_eq!(json(&home, &[], &["jobs", "list"]).1["jobs"], serde_json::json!([]));
    assert_eq!(json(&home, &[], &["keys", "list"]).1["keys"][0]["name"], "agent");
    assert_eq!(json(&home, &[], &["usage", "retrieve"]).1["usage"]["jobs"], 0);
    // With a key that may start runs (and only read otherwise), and with one it does not know.
    let bare = temp("cf-bare");
    let agent = [("SUPERCI_PLANE", plane.url.as_str()), ("SUPERCI_KEY", plane.run_key.as_str())];
    assert_eq!(json(&bare, &agent, &["runs", "list"]).1["runs"], serde_json::json!([]));
    let (code, refused) = json(&bare, &[("SUPERCI_PLANE", plane.url.as_str()), ("SUPERCI_KEY", "superci_run_ffffffffffffffffffffffffffffffff")], &["runs", "list"]);
    assert!(code != 0, "{refused}");

    // A run from a computer: its working tree kept in the Durable Object, the run recorded. (No machines here: it
    // waits for a runner provider.)
    let dir = temp("cf-checkout");
    std::fs::create_dir_all(dir.join(".github/workflows")).unwrap();
    std::fs::write(dir.join(".github/workflows/ci.yml"), "jobs:\n  tests:\n    runs-on: superci\n    steps:\n      - run: make test\n").unwrap();
    for args in [&["init", "-q"][..], &["add", "-A"], &["-c", "user.name=t", "-c", "user.email=t@example.com", "commit", "-qm", "t"]] { assert!(Command::new("git").current_dir(&dir).args(args).status().unwrap().success()) }
    let o = Command::new(env!("CARGO_BIN_EXE_superci")).current_dir(&dir).args(["runs", "create", "--workflow=.github/workflows/ci.yml", "--detach", "--json"])
        .env("SUPERCI_HOME", dir.join(".cli")).env("SUPERCI_PLANE", &plane.url).env("SUPERCI_KEY", &plane.run_key).output().unwrap();
    let said = format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
    assert!(o.status.success(), "{said}");
    let runs = json(&bare, &agent, &["runs", "list"]).1;
    assert_eq!(runs["runs"].as_array().map(Vec::len), Some(1), "{runs} {said}");
    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&dir);
}

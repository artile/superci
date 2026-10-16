//! A Cloudflare control plane from the CLI and the dashboard, Cloudflare's API answering as it answered a real account
//! (`fixtures/cloudflare`, recorded 2026-10-16 with a control plane made for it and deleted after), and the control
//! plane itself one served on this computer, reading the secrets its Worker was given as its settings.
mod common;
use common::{part, words};
use superci_testkit::{serve, serve_as};

/// The workspace signed in to Cloudflare too.
fn with_cloudflare(home: &std::path::Path) {
    let file = home.join("workspaces/default/sign-ins.json");
    let mut kept: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    kept["cloudflare"] = serde_json::json!({ "access": "cloudflare-access-for-tests", "expires_at_ms": 4102444800000u64, "refresh": "cloudflare-refresh-for-tests" });
    std::fs::write(&file, kept.to_string()).unwrap();
}

#[test]
fn a_cloudflare_control_plane_is_deployed_given_its_containers_shown_and_deleted_as_cloudflare_answered() {
    // The workspace's control plane in AWS, serving GitHub (the one in use); the Cloudflare one comes beside it.
    let served = serve(|_| {});
    served.connect_github();
    let aws = served.aws.clone();
    // (The Worker's secrets are its settings.)
    let cf = serve_as("cfplane00001", move |c| {
        let settings = aws.worker_settings("superci");
        c.read_settings(&settings);
        if let Some((_, on)) = settings.iter().find(|(k, _)| k == "CONTAINERS") { c.containers = on == "on" }
    });
    cf.aws.params.lock().unwrap().remove("/superci/cfplane00001/CONTAINERS");
    served.aws.workers.lock().unwrap().insert("superci.acme.workers.dev".into(), cf.url.clone());
    let home = common::signed_in(&served);
    with_cloudflare(&home);
    let step = |name: &str| served.aws.replaying(&std::fs::read_to_string(format!("{}/fixtures/cloudflare/{name}.jsonl", env!("CARGO_MANIFEST_DIR"))).unwrap(), &[]);
    let run = |args: &[&str]| {
        let o = common::command(&home).args(args).env("SUPERCI_NEW_PLANE_ID", "cfplane00001").output().unwrap();
        (o.status.success(), String::from_utf8_lossy(&o.stdout).to_string() + &String::from_utf8_lossy(&o.stderr))
    };
    // Deployed: the Worker uploaded, given its address, answering.
    step("02-deploy");
    let (ok, said) = run(&["planes", "create", "cloudflare"]);
    assert!(ok && said.contains("The control plane is running: https://superci.acme.workers.dev (Cloudflare · Acme)"), "{said}");
    // Found in the account, and read.
    step("03-use");
    let (ok, said) = run(&["planes", "list"]);
    assert!(ok && said.contains("cfplane00001") && said.contains("Cloudflare · Acme"), "{said}");
    let (ok, said) = run(&["status", "--plane=cfplane00001"]);
    assert!(ok && said.contains("Control plane Cloudflare · Acme") && said.contains("https://superci.acme.workers.dev"), "{said}");
    // Its own containers: the image prepared, the container application made, switched on.
    step("04-containers");
    let (ok, said) = run(&["runners", "create", "cloudflare", "--plane=cfplane00001"]);
    assert!(ok && said.contains("cloudflare runs jobs now."), "{said}");
    assert_eq!(served.aws.worker_settings("superci").into_iter().find(|(k, _)| k == "CONTAINERS").map(|(_, v)| v).as_deref(), Some("on"));
    let (ok, said) = run(&["runners", "list", "--plane=cfplane00001"]);
    assert!(ok && said.contains("1. cloudflare"), "{said}");
    // The dashboard: its Runners and Add runners pages, Cloudflare's spending read.
    step("05-dashboard");
    let (_child, link) = common::dashboard(&home);
    let page = (0..40).map(|_| { std::thread::sleep(std::time::Duration::from_millis(250)); words(&part(&link, "/?p=runners&c=cfplane00001&fragment=1").1) }).find(|p| p.contains("Cloudflare") && !p.contains("Reading your control plane")).unwrap_or_default();
    assert!(page.contains("Cloudflare Containers"), "{page}");
    drop(_child);
    // Taken out, then the control plane deleted with its Worker and container application.
    step("06-remove");
    let (ok, said) = run(&["runners", "delete", "cloudflare", "--plane=cfplane00001", "--confirm"]);
    assert!(ok && said.contains("Removed Cloudflare."), "{said}");
    step("07-delete");
    let (ok, said) = run(&["planes", "delete", "cfplane00001", "--confirm"]);
    assert!(ok && said.contains("Deleted Cloudflare · Acme."), "{said}");
    // (Every call answered as recorded, but the tidy of expired keys after a key is handed over, which the real
    // account did not need: it gives up quietly.)
    assert!(served.aws.unanswered.lock().unwrap().iter().all(|u| !u.contains("cloudflare.com") || u.ends_with("/workers/scripts/superci/secrets")), "{:?}", served.aws.unanswered.lock().unwrap());
    let _ = std::fs::remove_dir_all(&home);
}

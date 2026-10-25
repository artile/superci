//! The dashboard's rarer ways, end to end: a sign-in its cloud does not renew, a cloud that refuses one call, a
//! control plane that does not answer as asked, and moves and updates whose every step can stop. Whatever stops, the
//! dashboard says why and nothing is left half done that it says is done.
mod common;
use superci_testkit::{serve, Served};

fn sign_ins(home: &std::path::Path) -> std::path::PathBuf { home.join("workspaces/default/sign-ins.json") }

/// The workspace's sign-ins changed as `change` says.
fn kept_as(home: &std::path::Path, change: impl Fn(&mut serde_json::Value)) {
    let file = sign_ins(home);
    let mut k: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    change(&mut k);
    std::fs::write(&file, k.to_string()).unwrap();
}

/// Another control plane in the workspace beside the one in use: in the same AWS account, set up and never used (it
/// answers nowhere).
fn beside(home: &std::path::Path, planes: serde_json::Value) { kept_as(home, |k| k["planes_more"] = planes.clone()) }

/// The workspace's AWS sign-in with its credentials over, and AWS's sign-in failing as it renews them (not ending
/// it: AWS did not answer).
fn aws_renewal_fails(served: &Served, home: &std::path::Path) {
    kept_as(home, |k| {
        k["aws"]["creds"]["expires_at_ms"] = 0.into();
        k["aws"]["refresh_token"] = "superci-refresh".into();
        k["aws"]["dpop"] = serde_json::to_value(superci_core::crypto::SigningKeyStore::generate()).unwrap();
    });
    served.aws.fail("signin.aws.amazon.com/v1/token", 500, r#"{"error":"server_error","message":"We encountered an internal error."}"#, None);
}

/// A form the dashboard's pages send, and its answer: status and words.
fn sent(link: &str, path: &str, fields: &[(&str, &str)]) -> (u16, String) {
    let (status, _, body) = common::post(link, path, fields);
    (status, common::words(&body))
}

/// Every change that needs AWS's sign-in, while AWS does not renew it: each says so, and none goes on without it.
#[test]
fn every_change_that_needs_aws_says_so_while_aws_does_not_renew_its_sign_in() {
    let served = serve(|_| {});
    served.connect_github();
    let home = common::signed_in(&served);
    beside(&home, serde_json::json!([{ "Aws": { "account_id": "111111111111", "region": "us-east-1", "url": "http://127.0.0.1:9", "plane_id": "otherplane01", "label": "superci" } }]));
    aws_renewal_fails(&served, &home);
    // (Signed in to Cloudflare too: the control plane's launcher there comes along with a move.)
    kept_as(&home, |k| k["cloudflare"] = serde_json::json!({ "access": "cloudflare-access-for-tests", "expires_at_ms": 4102444800000u64, "refresh": "cloudflare-refresh-for-tests" }));
    let (_child, link) = common::dashboard_with(&home, &[("SUPERCI_VIEWS_S", "0"), ("SUPERCI_PAUSE_MS", "1")]);
    let networks = "us-east-1 subnet-0123456789abcdef0 sg-0123456789abcdef0";
    for (path, fields) in [
        ("/runners/aws-own", &[][..]),
        ("/runners/remove", &[("cloud", "aws"), ("stop", "on")]),
        ("/plane/aws", &[("region", "us-east-1")]),
        ("/aws/image", &[("region", "us-east-1")]),
        ("/plane/update", &[("plane", superci_testkit::PLANE)]),
        ("/permissions/aws", &[("plane", superci_testkit::PLANE)]),
        ("/routing", &[("action", "account_usd"), ("account", "aws"), ("usd", "100"), ("budget", "emails")]),
        ("/routing", &[("action", "account"), ("cloud", "aws"), ("networks", networks)]),
        ("/routing", &[("action", "pool"), ("cloud", "aws"), ("region_0", "eu-north-1")]),
        ("/aws/connect", &[("region", "us-east-1")]),
        ("/plane/move", &[("plane", "otherplane01")]),
        ("/plane/delete", &[("plane", "otherplane01")]),
    ] {
        let (status, said) = sent(&link, path, fields);
        assert!(status >= 400 && said.contains("AWS"), "{path} {fields:?}: {status} {said}");
    }
    // The commands that read the account say so too.
    for args in [&["accounts", "retrieve", "aws"][..], &["planes", "allow", superci_testkit::PLANE, "--dry-run"]] {
        let (ok, said) = common::superci_in_time(&home, args, &[("SUPERCI_PAUSE_MS", "1")], 60);
        assert!(!ok && said.contains("AWS"), "{args:?}: {said}");
    }
    let _ = std::fs::remove_dir_all(&home);
}

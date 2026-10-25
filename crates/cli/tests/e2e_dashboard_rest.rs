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

/// A Cloudflare sign-in that takes, good for years.
fn cloudflare_signed_in() -> serde_json::Value { serde_json::json!({ "access": "cloudflare-access-for-tests", "expires_at_ms": 4102444800000u64, "refresh": "cloudflare-refresh-for-tests" }) }

/// The served control plane kept as a Cloudflare one: what is written for it goes to its Worker's secrets, which it
/// does not read (a change written there shows nowhere).
fn as_cloudflare(home: &std::path::Path, served: &Served) {
    let url = served.url.clone();
    kept_as(home, |k| k["plane"] = serde_json::json!({ "Cloudflare": { "account_id": "1111111111111111111111111111111a", "account_name": "Acme", "script": "superci", "url": url, "plane_id": superci_testkit::PLANE, "label": "superci" } }));
}

/// Cloudflare's answer of an internal error.
const CF_FAILED: &str = r#"{"success":false,"errors":[{"code":10000,"message":"Internal server error"}],"messages":[],"result":null}"#;

/// Waits up to 15 seconds for `holds`, else fails saying `what`.
fn until(what: &str, holds: impl Fn() -> bool) {
    let end = std::time::Instant::now() + std::time::Duration::from_secs(15);
    while !holds() { assert!(std::time::Instant::now() < end, "waited for {what}"); std::thread::sleep(std::time::Duration::from_millis(100)) }
}

/// Every change that needs Cloudflare's sign-in, while Cloudflare does not renew it (not ending it: Cloudflare did not
/// answer): each says so; an update started says so where it shows.
#[test]
fn every_change_that_needs_cloudflare_says_so_while_cloudflare_does_not_renew_its_sign_in() {
    let served = serve(|_| {});
    served.connect_github();
    let home = common::signed_in(&served);
    as_cloudflare(&home, &served);
    beside(&home, serde_json::json!([{ "Cloudflare": { "account_id": "1111111111111111111111111111111a", "account_name": "Acme", "script": "superci-other", "url": "http://127.0.0.1:9", "plane_id": "cfplane00009", "label": "superci" } }]));
    kept_as(&home, |k| k["cloudflare"] = serde_json::json!({ "access": "cloudflare-access-for-tests", "expires_at_ms": 0, "refresh": "cloudflare-refresh-for-tests" }));
    served.aws.fail("dash.cloudflare.com/oauth2/token", 500, r#"{"error":"server_error"}"#, None);
    let (_child, link) = common::dashboard_with(&home, &[("SUPERCI_VIEWS_S", "0"), ("SUPERCI_PAUSE_MS", "1")]);
    for (path, fields) in [
        ("/name", &[("name", "Acme CI")][..]),
        ("/keys/remove", &[("name", "DEVICE_KEY_4102444800_OTHER")]),
        ("/runners/remove", &[("cloud", "cloudflare"), ("stop", "on")]),
        ("/plane/delete", &[("plane", "cfplane00009")]),
    ] {
        let (status, said) = sent(&link, path, fields);
        assert!(status >= 400 && (said.contains("Cloudflare") || said.contains("500")), "{path} {fields:?}: {status} {said}");
    }
    let (status, said) = sent(&link, "/plane/update", &[("plane", superci_testkit::PLANE)]);
    assert!(status == 303, "{status} {said}");
    until("the update said to stop", || common::words(&common::part(&link, "/?p=planes&fragment=1").1).contains("The update stopped: Sign in with Cloudflare first."));
    let _ = std::fs::remove_dir_all(&home);
}

/// Setting a control plane up in Cloudflare while its accounts are not read, and in an account the sign-in does not
/// reach: each said, nothing deployed.
#[test]
fn a_cloudflare_control_plane_is_set_up_only_in_an_account_the_sign_in_reaches() {
    let served = serve(|_| {});
    let home = common::signed_in(&served);
    kept_as(&home, |k| k["cloudflare"] = cloudflare_signed_in());
    served.aws.fail("/client/v4/accounts?per_page", 500, CF_FAILED, None);
    let (_child, link) = common::dashboard_with(&home, &[("SUPERCI_VIEWS_S", "0"), ("SUPERCI_PAUSE_MS", "1")]);
    let (status, said) = sent(&link, "/plane/cloudflare", &[("account", "1111111111111111111111111111111a")]);
    assert!(status >= 400 && said.contains("Internal server error"), "{status} {said}");
    served.aws.fail_none();
    served.aws.fail("/client/v4/accounts?per_page", 200, r#"{"success":true,"errors":[],"messages":[],"result":[{"id":"2222222222222222222222222222222b","name":"Globex"}]}"#, None);
    let (status, said) = sent(&link, "/plane/cloudflare", &[("account", "1111111111111111111111111111111a")]);
    assert!(status >= 400 && said.contains("the sign-in cannot reach that account"), "{status} {said}");
    let _ = std::fs::remove_dir_all(&home);
}

/// Runners added to a control plane that does not answer: nothing is deployed, and the page says to try again.
#[test]
fn runners_are_not_added_to_a_control_plane_that_does_not_answer() {
    let served = serve(|_| {});
    let home = common::signed_in(&served);
    common::kept_at(&home, "http://127.0.0.1:9");
    kept_as(&home, |k| {
        k["cloudflare"] = cloudflare_signed_in();
        k["modal"] = serde_json::json!({ "token_id": "ak-modal-for-tests", "token_secret": "as-modal-for-tests", "workspace": "acmeorg" });
    });
    let (_child, link) = common::dashboard_with(&home, &[("SUPERCI_VIEWS_S", "0"), ("SUPERCI_PAUSE_MS", "1")]);
    for path in ["/runners/modal", "/runners/cloudflare"] {
        let (status, said) = sent(&link, path, &[]);
        assert!(status == 503 && said.contains("The control plane did not answer yet"), "{path}: {status} {said}");
    }
    let _ = std::fs::remove_dir_all(&home);
}

/// Signed in to an AWS account with no control plane in it: a job's log and a run's artifact are of none; and with AWS
/// not answering at all, `status` says so.
#[test]
fn a_job_or_an_artifact_asked_for_where_no_control_plane_is_found_is_of_none() {
    let served = serve(|_| {});
    let home = common::aws_only(&served.aws);
    let (_child, link) = common::dashboard_with(&home, &[("SUPERCI_VIEWS_S", "0"), ("SUPERCI_PAUSE_MS", "1")]);
    for path in ["/job/view?id=1", "/run/artifact?run=1&name=coverage"] {
        let (status, _, said) = common::get(&link, path);
        assert!(status == 404 && common::words(&said).contains("No control plane"), "{path}: {status} {}", common::words(&said));
    }
    served.arm(superci_testkit::Fault::Outage(1));
    let (ok, said) = common::superci_in_time(&home, &["status"], &[("SUPERCI_PAUSE_MS", "1")], 60);
    assert!(!ok && said.contains("AWS"), "{said}");
    let _ = std::fs::remove_dir_all(&home);
}

/// A sign-in with Modal that Modal does not start, and one with Cloudflare whose last step Cloudflare refuses: each said.
#[test]
fn sign_ins_that_modal_does_not_start_or_cloudflare_does_not_finish_say_why() {
    let served = serve(|_| {});
    let home = common::signed_in(&served);
    // (On Cloudflare's own port: its sign-in comes back to no other.)
    let port = { let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap(); l.local_addr().unwrap().port() }.to_string();
    let (_child, link) = common::dashboard_with(&home, &[("SUPERCI_VIEWS_S", "0"), ("SUPERCI_PAUSE_MS", "1"), ("SUPERCI_OAUTH_PORT", port.as_str()), ("SUPERCI_PORT", port.as_str())]);
    // (Its key's cookie on that port is the usual one.)
    let key = link.split_once("/?k=").unwrap().1.to_string();
    let agent: ureq::Agent = ureq::Agent::config_builder().http_status_as_error(false).max_redirects(0).build().into();
    let ask = |method: &str, path: &str| {
        let url = format!("http://127.0.0.1:{port}{path}");
        let mut r = if method == "POST" { agent.post(&url).header("cookie", &format!("superci_local={key}")).header("sec-fetch-site", "same-origin").header("content-type", "application/x-www-form-urlencoded").send("next=").unwrap() }
            else { agent.get(&url).header("cookie", &format!("superci_local={key}")).header("sec-fetch-site", "same-origin").call().unwrap() };
        let to = r.headers().get("location").and_then(|v| v.to_str().ok()).unwrap_or_default().to_string();
        (r.status().as_u16(), to, r.body_mut().read_to_string().unwrap_or_default())
    };
    served.arm(superci_testkit::Fault::Outage(1));
    for (method, path) in [("GET", "/connect/modal"), ("POST", "/modal/signin")] {
        let (status, _, said) = ask(method, path);
        assert!(status >= 400, "{path}: {status} {}", common::words(&said));
    }
    served.faults().calm();
    let (_, to, page) = ask("GET", "/connect/cloudflare");
    let at = if to.is_empty() { page } else { to };
    let state = at.split("state=").nth(1).and_then(|r| r.split(['&', '"', '\'']).next()).expect("a state").to_string();
    served.aws.fail("dash.cloudflare.com/oauth2/token", 500, r#"{"error":"server_error"}"#, None);
    let (status, _, said) = ask("GET", &format!("/oauth/callback?code=a-code-from-cloudflare&state={state}"));
    assert!(status >= 400 && common::words(&said).contains("server_error"), "{status} {}", common::words(&said));
    let _ = std::fs::remove_dir_all(&home);
}

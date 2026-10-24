//! Edges of what the control plane and the CLI do that only a request at the wrong moment, or an address or a folder
//! not as SuperCI makes them, reaches: each against a control plane served here (`superci_testkit`).
mod common;
use common::{dashboard, json, signed_in, superci};
use superci_testkit::{serve, PLANE};

/// A control plane of AWS's machines, its GitHub connected, served at https://plane.acme.test, AWS answering as it
/// answered Test.
fn aws_plane() -> superci_testkit::Served {
    let served = serve(|c| {
        (c.aws_own, c.aws_own_creds) = (Some(("111111111111".into(), "us-east-1".into())), Some(superci_core::aws::Credentials { access_key_id: "ASIAX".into(), secret_access_key: "secret".into(), session_token: Some("tok".into()), expires_at_ms: u64::MAX }));
    });
    served.connect_github();
    *served.public.lock().unwrap() = Some("https://plane.acme.test".into());
    served.aws.put(&format!("/superci/{PLANE}/ROUTING"), r#"{"order":[{"cloud":"aws","max_jobs":100},{"cloud":"aws-on-demand"},{"cloud":"cloudflare","off":true}]}"#);
    served.aws.replaying(include_str!("../fixtures/aws/launch/spot.jsonl"), &[]);
    served
}

/// AWS's answer to a machine started (as it answered Test).
fn aws_started() -> String {
    include_str!("../fixtures/aws/launch/on-demand.jsonl").lines().filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .find(|v| v["asked"].as_str().unwrap_or_default().contains("Action=RunInstances") && v["status"] == 200).map(|v| v["answer"].as_str().unwrap_or_default().to_string()).unwrap()
}

/// A GitHub job queued with `label`, as the control plane keeps it once GitHub said so.
fn aws_job(served: &superci_testkit::Served, name: &str, label: &str) -> serde_json::Value {
    served.store.0.lock().unwrap().retain(|k, _| !k.starts_with("aws:pool:"));
    served.store.0.lock().unwrap().insert("aws:no-fleet".into(), served.now_ms().to_string());
    let (id, q) = served.github.queue(name, label, served.now_ms());
    assert!(served.webhook(q) < 300, "{label}");
    served.store.under(&format!("job:{id}"))[0].1.clone()
}

/// The start-up script of the machine AWS was asked for last.
fn last_script(served: &superci_testkit::Served) -> String {
    let asked = served.aws.asked.lock().unwrap().iter().rev().find(|a| a["asked"].as_str().unwrap_or_default().contains("Action=RunInstances")).map(|a| a["asked"].as_str().unwrap().to_string()).expect("a machine asked for");
    let data: String = url::form_urlencoded::parse(asked.as_bytes()).find(|(k, _)| k == "UserData").map(|(_, v)| v.to_string()).unwrap();
    use base64::Engine;
    String::from_utf8_lossy(&base64::engine::general_purpose::STANDARD.decode(data).unwrap()).to_string()
}

/// Control planes found through their roles in AWS: one at an https address and the tests' own on this computer are
/// listed; one whose role names a plain http address elsewhere is not offered at all.
#[test]
fn a_control_plane_found_in_aws_at_a_plain_http_address_is_not_offered_while_https_and_this_computers_are() {
    let role = |name: &str, description: &str| format!("<member><Path>/</Path><RoleName>{name}</RoleName><RoleId>AROAEXAMPLE{:09}</RoleId><Arn>arn:aws:iam::111111111111:role/{name}</Arn><CreateDate>2026-10-18T10:00:00Z</CreateDate><Description>{description}</Description><MaxSessionDuration>3600</MaxSessionDuration></member>", name.len());
    let served = serve(|_| {});
    served.connect_github();
    for id in ["otherci000002", "otherci000003"] {
        served.aws.fail(&format!("AmazonSSM.GetParameters|{id}"), 400, &superci_testkit::json_error("AccessDeniedException", "not authorized to perform: ssm:GetParameters"), None);
    }
    let roles = [role(&format!("superci-plane-{PLANE}"), &format!("Runs the SuperCI control plane {} (Lambda, us-east-1, runs-on superci).", served.url)),
        role("superci-plane-otherci000002", "Runs the SuperCI control plane https://otherci2.example (Lambda, us-east-1, runs-on initech)."),
        role("superci-plane-otherci000003", "Runs the SuperCI control plane http://otherci3.example (Lambda, us-east-1, runs-on globex).")];
    served.aws.fail("ListRoles", 200, &format!("<ListRolesResponse xmlns=\"https://iam.amazonaws.com/doc/2010-05-08/\"><ListRolesResult><IsTruncated>false</IsTruncated><Roles>{}</Roles></ListRolesResult><ResponseMetadata><RequestId>00000000-0000-4000-8000-000000000001</RequestId></ResponseMetadata></ListRolesResponse>", roles.concat()), None);
    let home = signed_in(&served);
    let (_child, link) = dashboard(&home);
    let mut page = String::new();
    for _ in 0..40 {
        page = common::words(&common::part(&link, "/?p=planes&w=default&fragment=1").1);
        if page.contains("otherci000002") { break }
        std::thread::sleep(std::time::Duration::from_millis(300));
    }
    assert!(page.contains("A CI (otherci000002)") && !page.contains("otherci000003"), "{page}");
    let _ = std::fs::remove_dir_all(&home);
}

/// A control plane told it moved away takes an https address, or in a test the address of one on this computer; a
/// plain http address elsewhere is refused and nothing is kept.
#[test]
fn a_move_away_to_a_plain_http_address_is_refused_and_one_to_https_or_this_computer_is_kept() {
    let away = |served: &superci_testkit::Served, to: &str| -> u16 {
        ureq::post(&format!("{}/move/away", served.url)).header("authorization", &format!("Bearer {}", served.key)).config().http_status_as_error(false).build()
            .send(serde_json::json!({ "to": to }).to_string()).unwrap().status().as_u16()
    };
    let served = serve(|_| {});
    assert_eq!(away(&served, "http://next.example"), 400);
    assert!(served.store.under("moved_to").is_empty());
    let next = serve(|_| {});
    assert_eq!(away(&served, &next.url), 200);
    assert_eq!(served.store.under("moved_to")[0].1, next.url.as_str());
    let other = serve(|_| {});
    assert_eq!(away(&other, "https://next.example"), 200);
    assert_eq!(other.store.under("moved_to")[0].1, "https://next.example");
}

/// A spot machine is given a link of its own to say AWS is taking it back: from a control plane at an https address,
/// or a test's on this computer. One answering at a plain http address elsewhere gives no such link: no spot machine
/// is started, and the job has an on-demand one, which needs none.
#[test]
fn a_control_plane_at_a_plain_http_address_starts_no_spot_machine_without_a_safe_notice_link_and_goes_on_demand() {
    let served = aws_plane();
    served.aws.fail("RunInstances", 200, &aws_started(), None);
    let j = aws_job(&served, "secure", "superci");
    assert!(j["state"] == "launched" && j["on_demand"] != true && last_script(&served).contains("https://plane.acme.test/interrupted?runner="), "{j}");
    *served.public.lock().unwrap() = None;
    let j = aws_job(&served, "here", "superci");
    assert!(j["state"] == "launched" && j["on_demand"] != true && last_script(&served).contains(&format!("{}/interrupted?runner=", served.url)), "{j}");
    *served.public.lock().unwrap() = Some("http://plane.acme.test".into());
    let asked = served.aws.asked.lock().unwrap().len();
    let j = aws_job(&served, "plain", "superci");
    let spot = served.aws.asked.lock().unwrap().iter().skip(asked).filter(|a| a["asked"].as_str().is_some_and(|a| a.contains("Action=RunInstances") && a.contains("InstanceMarketOptions"))).count();
    assert!(j["state"] == "launched" && j["on_demand"] == true && spot == 0 && !last_script(&served).contains("/interrupted?"), "{j}");
}

/// A job on Ubuntu 26.04 whose own last run lately pulled enough of Docker's images gets the layer store set up at its
/// machine's start, though its repository's other jobs used none.
#[test]
fn a_jobs_own_last_run_that_used_enough_images_sets_up_its_layer_store() {
    let served = aws_plane();
    served.aws.fail("RunInstances", 200, &aws_started(), None);
    let bucket = superci_core::cache::bucket(PLANE, "us-east-1");
    served.github.put_object(&format!("{bucket}/{}", superci_core::cache::program_key("x86_64")), b"program");
    served.github.put_object(&format!("{bucket}/{}", superci_core::cache::layers_key("x86_64")), b"program");
    let now = served.now_ms();
    let images = |bytes: u64| serde_json::json!({ "served": 3, "served_bytes": bytes, "pulled": 0, "pulled_bytes": 0, "at": now }).to_string();
    {
        let mut m = served.store.0.lock().unwrap();
        m.insert(superci_core::layers::repo_images_key("acme/app"), images(0));
        m.insert(superci_core::layers::images_key("acme/app", "Build and Test", "e2e"), images(2 << 30));
    }
    let e2e = aws_job(&served, "e2e", "superci/image=ubuntu26");
    assert!(e2e["state"] == "launched" && e2e["layers"] == true, "its own last run used images: {e2e}");
    let lint = aws_job(&served, "lint", "superci/image=ubuntu26");
    assert!(lint["state"] == "launched" && lint["layers"] != true, "neither it nor its repository did: {lint}");
}

/// A machine whose own job ran elsewhere offers itself to the other jobs still waiting for one, GitHub asked of each:
/// one GitHub no longer has queued is noted as begun, and one whose record another request took out as GitHub was
/// asked is not written again.
#[test]
fn a_job_offered_a_free_machine_whose_record_is_taken_out_meanwhile_is_not_written_again() {
    let served = serve(|_| {});
    served.connect_github();
    let now = served.now_ms();
    let queued = |name: &str| { let (id, _) = served.github.queue(name, "superci", now); served.github.start_elsewhere(id, "another-ci-runner"); id };
    let (own, begun, gone) = (queued("its own"), queued("begun elsewhere"), queued("taken out"));
    {
        let mut m = served.store.0.lock().unwrap();
        let job = |id: u64, state: &str, ago: u64| serde_json::json!({ "job_id": id, "run_id": 41, "repo": "acme/app", "state": state, "label": "superci", "cloud": "cloudflare", "at_ms": now - ago,
            "installation_id": 42, "seen_in_progress": false, "began": false });
        let mut machine = job(own, "launched", 600_000);
        for (k, v) in [("launched_ms", serde_json::json!(now - 600_000)), ("runner", format!("superci-{own}").into()), ("runner_id", 7.into()), ("machine_id", format!("machine-{own}").into())] { machine[k] = v }
        m.insert(format!("job:{own}"), machine.to_string());
        for id in [begun, gone] { m.insert(format!("job:{id}"), job(id, "done", 60_000).to_string()); }
    }
    let (store, key) = (served.store.clone(), format!("job:{gone}"));
    served.github.during("GET", &format!("/actions/jobs/{gone}"), move || { store.0.lock().unwrap().remove(&key); });
    served.upkeep();
    let asked = |id: u64| served.github.asked.lock().unwrap().iter().filter(|a| a.ends_with(&format!("/actions/jobs/{id}"))).count();
    assert!(asked(begun) == 1 && asked(gone) == 1, "{:?}", served.github.asked.lock().unwrap());
    assert_eq!(served.store.under(&format!("job:{begun}"))[0].1["began"], true);
    assert!(served.store.under(&format!("job:{gone}")).is_empty(), "{:?}", served.store.under(&format!("job:{gone}")));
}

/// A run from a computer moved on while another request writes its record: written again over what is there then
/// (the store reads, then writes, as a Durable Object's does), what the other wrote kept.
#[test]
fn a_run_written_meanwhile_is_moved_on_over_what_another_request_wrote() {
    let served = serve(|_| {});
    served.written_meanwhile("run:", 1, |v| v.map(|v| { let mut r: serde_json::Value = serde_json::from_str(v).unwrap(); r["meanwhile"] = true.into(); r.to_string() }));
    let body = serde_json::json!({ "workflow": "jobs:\n  a:\n    runs-on: superci\n    steps:\n      - run: echo a\n", "path": ".github/workflows/ci.yml", "repository": "acme/app", "ref": "refs/heads/main" });
    let mut r = ureq::post(&format!("{}/runs", served.url)).header("authorization", &format!("Bearer {}", served.run_key)).send(body.to_string()).unwrap();
    let said: serde_json::Value = r.body_mut().read_json().unwrap();
    let run = said["run_id"].as_str().unwrap_or_else(|| panic!("{said}")).to_string();
    let record = served.store.under(&format!("run:{run}"))[0].1.clone();
    assert!(record["meanwhile"] == true && record["jobs"][0]["state"] == "started" && served.store.under("job:").len() == 1, "{record}");
}

/// A workspace whose folder holds what this computer will not let SuperCI remove is said so, and kept.
#[test]
#[cfg(unix)]
fn a_workspace_whose_folder_cannot_be_removed_is_said_so_and_kept() {
    use std::os::unix::fs::PermissionsExt;
    let served = serve(|_| {});
    let home = signed_in(&served);
    let (code, made) = json(&home, &["workspaces", "create", "Acme"]);
    assert_eq!(code, 0, "{made}");
    let stuck = home.join("workspaces/acme/stuck");
    std::fs::create_dir_all(&stuck).unwrap();
    std::fs::write(stuck.join("kept"), "x").unwrap();
    std::fs::set_permissions(&stuck, std::fs::Permissions::from_mode(0o500)).unwrap();
    // (Where permissions are not enforced, as for root, there is nothing to see.)
    if std::fs::write(stuck.join("probe"), "").is_ok() { eprintln!("folders not kept read-only here: skipped"); let _ = std::fs::remove_dir_all(&home); return }
    let o = superci(&home, &["workspaces", "delete", "acme", "--confirm"]);
    let said = String::from_utf8_lossy(&o.stderr).to_string();
    std::fs::set_permissions(&stuck, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(!o.status.success() && said.contains("could not remove") && home.join("workspaces/acme").exists(), "{said}");
    let _ = std::fs::remove_dir_all(&home);
}

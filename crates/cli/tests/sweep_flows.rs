//! The CLI's cloud work with each of its calls and the control planes' store operations failing in turn
//! (`superci_testkit::sweep`), as the clouds answered real ones: a control plane set up in AWS, each provider's runners
//! added to it, a Cloudflare one set up and everything moved there, AWS's runners taken out, both deleted; Cloudflare
//! and Modal control planes set up, runners added, moved, updated and deleted. Whatever fails, the CLI does not panic,
//! a command that fails says why, the workspace's sign-ins stay readable, `planes list` and `status` still answer,
//! and the same command run again once the cloud answers goes through.
mod common;
use std::path::PathBuf;
use superci_testkit::{serve, serve_as, sweep, Fault, Outcome, Served};

/// A flow's control planes besides the workspace's own (served, the scenario's), kept answering while it runs; where
/// a recording's new control plane is (`http://new-plane.test` there); and the workspace, signed in to AWS,
/// Cloudflare and Modal.
struct World { home: PathBuf, new_plane: String, _planes: Vec<Served> }

/// A flow: its control planes, where its recordings are, and its steps in their order.
struct Flow { world: fn() -> (Served, World), recordings: &'static str, steps: &'static [Step] }

/// The workspace's control plane in AWS (serving GitHub), the one set up beside it in the same account, and the
/// Cloudflare one set up later; as the test of every provider's runners has them.
fn every_provider() -> (Served, World) {
    let served = serve(|_| {});
    served.connect_github();
    let aws = served.aws.clone();
    let creds = superci_core::aws::Credentials { access_key_id: "ASIAX".into(), secret_access_key: "secret".into(), session_token: Some("tok".into()), expires_at_ms: u64::MAX };
    let second = serve_as("awsplane0001", move |c| {
        c.read_settings(&aws.settings("awsplane0001"));
        (c.aws_own, c.aws_own_creds) = (Some(("111111111111".into(), "us-east-1".into())), Some(creds.clone()));
        c.aws_runners_off = aws.settings("awsplane0001").iter().any(|(k, v)| k == "AWS_RUNNERS" && v == "off");
    });
    second.aws.params.lock().unwrap().remove("/superci/awsplane0001/CONTAINERS");
    let aws = served.aws.clone();
    let cf = serve_as("cfplane00004", move |c| {
        let settings = aws.worker_settings("superci");
        c.read_settings(&settings);
        c.containers = settings.iter().any(|(k, v)| k == "CONTAINERS" && v == "on");
        if let Some(v) = settings.iter().find(|(k, _)| k == "AWS_CONNECT").and_then(|(_, v)| serde_json::from_str::<serde_json::Value>(v).ok()) {
            c.aws_region = v["region"].as_str().map(str::to_string);
            c.aws_connect_token = v["token"].as_str().map(str::to_string);
        }
    });
    cf.aws.params.lock().unwrap().remove("/superci/cfplane00004/CONTAINERS");
    {
        let mut w = served.aws.workers.lock().unwrap();
        w.insert("superci.acme.workers.dev".into(), cf.url.clone());
        for launcher in ["acmeorg--superci-runners-awsplane0001.modal.run", "acmeorg--superci-runners-cfplane00004.modal.run", "superci-runners-awsplane0001.acme.workers.dev"] { w.insert(launcher.into(), cf.url.clone()); }
    }
    let home = common::signed_in(&served);
    let file = home.join("workspaces/default/sign-ins.json");
    let mut kept: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    kept["plane"]["Aws"]["plane_id"] = "otherplane01".into();
    kept["key"]["planes"] = serde_json::json!(["otherplane01"]);
    kept["cloudflare"] = serde_json::json!({ "access": "cloudflare-access-for-tests", "expires_at_ms": 4102444800000u64, "refresh": "cloudflare-refresh-for-tests" });
    kept["modal"] = serde_json::json!({ "token_id": "ak-modal-for-tests", "token_secret": "as-modal-for-tests", "workspace": "acmeorg" });
    std::fs::write(&file, kept.to_string()).unwrap();
    let new_plane = second.url.clone();
    (served, World { home, new_plane, _planes: vec![second, cf] })
}

/// The workspace signed in to Cloudflare and Modal too, with its control plane in AWS; the Cloudflare and Modal ones
/// it sets up served here, each reading its settings where its cloud keeps them (`plane`: the id each is given).
fn cloudflare_and_modal(cf_id: &'static str, md_id: &'static str) -> (Served, World) {
    let served = serve(|_| {});
    served.connect_github();
    let aws = served.aws.clone();
    let cf = serve_as(cf_id, move |c| {
        let settings = aws.worker_settings("superci");
        c.read_settings(&settings);
        if let Some((_, on)) = settings.iter().find(|(k, _)| k == "CONTAINERS") { c.containers = on == "on" }
    });
    cf.aws.params.lock().unwrap().remove(&format!("/superci/{cf_id}/CONTAINERS"));
    let aws = served.aws.clone();
    let md = serve_as(md_id, move |c| {
        let settings = aws.modal_plane_settings();
        c.read_settings(&settings);
        c.own_cloud = "modal".into();
        c.containers = settings.iter().any(|(k, v)| k == "CONTAINERS" && v == "on");
    });
    md.aws.params.lock().unwrap().remove(&format!("/superci/{md_id}/CONTAINERS"));
    {
        let mut w = served.aws.workers.lock().unwrap();
        w.insert("superci.acme.workers.dev".into(), cf.url.clone());
        w.insert(format!("acmeorg--superci-plane-{md_id}.modal.run"), md.url.clone());
        // (The Modal launcher's health, waited for once it is deployed: answered by a server here.)
        w.insert(format!("acmeorg--superci-runners-{cf_id}.modal.run"), md.url.clone());
    }
    let home = common::signed_in(&served);
    let file = home.join("workspaces/default/sign-ins.json");
    let mut kept: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    kept["cloudflare"] = serde_json::json!({ "access": "cloudflare-access-for-tests", "expires_at_ms": 4102444800000u64, "refresh": "cloudflare-refresh-for-tests" });
    kept["modal"] = serde_json::json!({ "token_id": "ak-modal-for-tests", "token_secret": "as-modal-for-tests", "workspace": "acmeorg" });
    std::fs::write(&file, kept.to_string()).unwrap();
    (served, World { home, new_plane: String::new(), _planes: vec![cf, md] })
}

/// One step of the flow: its recording, the id a control plane it makes is given, the command, and what it says when
/// it goes through.
struct Step { recording: &'static str, id: &'static str, args: &'static [&'static str], done: &'static str }

/// Every provider's runners on an AWS control plane, then a move to Cloudflare (`fixtures/runners`).
const EVERY_PROVIDER: Flow = Flow { world: every_provider, recordings: "runners", steps: &[
    Step { recording: "01-deploy-aws", id: "awsplane0001", args: &["planes", "create", "aws", "--region=us-east-1"], done: "The control plane is running" },
    Step { recording: "02-aws-runners", id: "", args: &["runners", "create", "aws", "--plane=awsplane0001"], done: "aws runs jobs now." },
    Step { recording: "03-cf-agent", id: "", args: &["runners", "create", "cloudflare", "--plane=awsplane0001"], done: "cloudflare runs jobs now." },
    Step { recording: "04-modal-agent", id: "", args: &["runners", "create", "modal", "--plane=awsplane0001"], done: "modal runs jobs now." },
    Step { recording: "07-deploy-cf", id: "cfplane00004", args: &["planes", "create", "cloudflare"], done: "The control plane is running" },
    Step { recording: "08-move", id: "", args: &["planes", "move", "cfplane00004", "--plane=awsplane0001"], done: "Moved." },
    Step { recording: "10-cf-remove-aws", id: "", args: &["runners", "delete", "aws", "--plane=cfplane00004", "--confirm"], done: "Removed AWS." },
    Step { recording: "11-delete-cf", id: "", args: &["planes", "delete", "cfplane00004", "--confirm"], done: "Deleted Cloudflare · Acme." },
    Step { recording: "12-delete-aws", id: "", args: &["planes", "delete", "awsplane0001", "--confirm"], done: "Deleted AWS" },
] };

/// Cloudflare and Modal control planes: Cloudflare's own containers and a Modal launcher on the first, everything
/// moved to the Modal one, the launcher taken out, both deleted (`fixtures/move`).
const MOVE_TO_MODAL: Flow = Flow { world: || cloudflare_and_modal("cfplane00002", "mdplane00002"), recordings: "move", steps: &[
    Step { recording: "01-deploy-cf", id: "cfplane00002", args: &["planes", "create", "cloudflare"], done: "The control plane is running: https://superci.acme.workers.dev" },
    Step { recording: "02-deploy-modal", id: "mdplane00002", args: &["planes", "create", "modal"], done: "The control plane is running: https://acmeorg--superci-plane-mdplane00002.modal.run" },
    Step { recording: "03-cf-containers", id: "", args: &["runners", "create", "cloudflare", "--plane=cfplane00002"], done: "cloudflare runs jobs now." },
    Step { recording: "04-cf-modal-agent", id: "", args: &["runners", "create", "modal", "--plane=cfplane00002"], done: "modal runs jobs now." },
    Step { recording: "07-move", id: "", args: &["planes", "move", "mdplane00002", "--plane=cfplane00002"], done: "Moved." },
    Step { recording: "09-md-remove-cf", id: "", args: &["runners", "delete", "cloudflare", "--plane=mdplane00002", "--confirm"], done: "Removed Cloudflare." },
    Step { recording: "10-delete-cf", id: "", args: &["planes", "delete", "cfplane00002", "--confirm"], done: "Deleted Cloudflare · Acme." },
    Step { recording: "11-delete-md", id: "", args: &["planes", "delete", "mdplane00002", "--confirm"], done: "Deleted Modal · acmeorg." },
] };

/// Cloudflare and Modal control planes set up, the Cloudflare one's containers set, both updated, both deleted
/// (`fixtures/update`).
const UPDATE: Flow = Flow { world: || cloudflare_and_modal("cfplane00003", "mdplane00003"), recordings: "update", steps: &[
    Step { recording: "01-deploy-cf", id: "cfplane00003", args: &["planes", "create", "cloudflare"], done: "The control plane is running" },
    Step { recording: "02-deploy-modal", id: "mdplane00003", args: &["planes", "create", "modal"], done: "The control plane is running" },
    Step { recording: "03-cf-containers", id: "", args: &["runners", "create", "cloudflare", "--plane=cfplane00003"], done: "cloudflare runs jobs now." },
    Step { recording: "04-cf-settings", id: "", args: &["runners", "update", "cloudflare", "--location=weur", "--max-jobs=3", "--image=https://images.example.com/full", "--plane=cfplane00003"], done: "up to 3 jobs at once" },
    Step { recording: "05-update-cf", id: "", args: &["planes", "update", "cfplane00003", "--plane=cfplane00003"], done: "Updated to" },
    Step { recording: "06-update-md", id: "", args: &["planes", "update", "mdplane00003", "--plane=mdplane00003"], done: "Updated to" },
    Step { recording: "07-delete-cf", id: "", args: &["planes", "delete", "cfplane00003", "--confirm"], done: "Deleted Cloudflare" },
    Step { recording: "08-delete-md", id: "", args: &["planes", "delete", "mdplane00003", "--confirm"], done: "Deleted Modal" },
] };

impl World {
    /// Step `i` of `flow`: its recording answered from now on, then its command run; whether it went through, and
    /// what it said.
    fn step(&self, served: &Served, flow: &Flow, i: usize) -> (bool, String) {
        let s = &flow.steps[i];
        let recording = std::fs::read_to_string(format!("{}/fixtures/{}/{}.jsonl", env!("CARGO_MANIFEST_DIR"), flow.recordings, s.recording)).unwrap();
        served.aws.replaying(&recording, &[("http://new-plane.test", &self.new_plane)]);
        // (The runner image there already when Cloudflare's runners are added: its copy is swept on its own.)
        if flow.recordings == "runners" && s.recording == "03-cf-agent" { image_there(served) }
        let o = common::command(&self.home).args(s.args).env("SUPERCI_NEW_PLANE_ID", s.id).env("SUPERCI_PAUSE_MS", "1").env("SUPERCI_MOVE_PAUSE_MS", "1").output().unwrap();
        served.aws.fail_none();
        (o.status.success(), String::from_utf8_lossy(&o.stdout).to_string() + &String::from_utf8_lossy(&o.stderr))
    }

    /// The CLI run, its counting held: whether it went through, and what it said.
    fn cli(&self, args: &[&str]) -> (bool, String) {
        let o = common::command(&self.home).args(args).env("SUPERCI_PAUSE_MS", "1").env("SUPERCI_MOVE_PAUSE_MS", "1").output().unwrap();
        (o.status.success(), String::from_utf8_lossy(&o.stdout).to_string() + &String::from_utf8_lossy(&o.stderr))
    }
}

/// GitHub's runner image in the Cloudflare account's registry already, ready.
fn image_there(served: &Served) {
    let reg = "registry.cloudflare.com/v2/1111111111111111111111111111111a/superci-runner";
    served.aws.fail_with_headers(&format!("{reg}/manifests/gh-"), 200, &[], "", None);
}

/// What a command that failed must have said: why, and not as a panic.
fn said_why(out: &Outcome, what: &str, ok: bool, said: &str) {
    out.check(!said.contains("panicked"), || format!("{what} panicked: {said}"));
    out.check(ok || said.trim().len() > 10, || format!("{what} failed saying nothing: {said:?}"));
}

/// Step `i` of `flow`, the steps before it gone through; then what it left checked.
fn flow_step(flow: &Flow, i: usize, fault: Fault) -> Outcome {
    let (served, w) = (flow.world)();
    for before in 0..i {
        let (ok, said) = w.step(&served, flow, before);
        assert!(ok && said.contains(flow.steps[before].done), "{}: {said}", flow.steps[before].recording);
    }
    served.arm(fault);
    let (ok, said) = w.step(&served, flow, i);
    served.faults().stop();
    let out = Outcome::new(served);
    let step = flow.steps[i].args.join(" ");
    // (Said in full when one fault is looked into.)
    if std::env::var("SUPERCI_SWEEP_ONLY").is_ok() { eprintln!("{step}: {ok}\n{said}") }
    said_why(&out, &step, ok, &said);
    out.check(!ok || said.contains(flow.steps[i].done), || format!("{step} went through without saying so: {said}"));
    // The workspace's sign-ins still readable.
    let kept = std::fs::read_to_string(w.home.join("workspaces/default/sign-ins.json")).unwrap_or_default();
    out.check(serde_json::from_str::<serde_json::Value>(&kept).is_ok(), || format!("after {step}, sign-ins.json is not JSON: {kept}"));
    // What the workspace keeps still answers.
    for look in [&["planes", "list"][..], &["status"]] {
        let (_, seen) = w.cli(look);
        if std::env::var("SUPERCI_SWEEP_ONLY").is_ok() { eprintln!("{}:\n{seen}", look.join(" ")) }
        said_why(&out, &look.join(" "), true, &seen);
        out.check(!seen.trim().is_empty(), || format!("after {step}, {} said nothing", look.join(" ")));
    }
    // Run again, with the cloud answering: nothing it left half-made is in its way. (The clean run's too: a step
    // done is done again, or said to be, after the looks above.)
    if !ok || fault == Fault::None {
        let (again, said_again) = w.step(&out.served, flow, i);
        out.check(again && said_again.contains(flow.steps[i].done), || format!("{step} failed ({said}), and run again: {said_again}"));
    }
    let _ = std::fs::remove_dir_all(&w.home);
    out
}

#[test]
fn an_aws_control_plane_set_up_from_the_cli_holds_with_each_call_and_store_operation_failing() {
    sweep("an aws control plane set up from the cli", |f| flow_step(&EVERY_PROVIDER, 0, f));
}

#[test]
fn aws_runners_added_from_the_cli_holds_with_each_call_and_store_operation_failing() {
    sweep("aws runners added from the cli", |f| flow_step(&EVERY_PROVIDER, 1, f));
}

#[test]
fn cloudflare_runners_added_from_the_cli_holds_with_each_call_and_store_operation_failing() {
    sweep("cloudflare runners added from the cli", |f| flow_step(&EVERY_PROVIDER, 2, f));
}

#[test]
fn modal_runners_added_from_the_cli_holds_with_each_call_and_store_operation_failing() {
    sweep("modal runners added from the cli", |f| flow_step(&EVERY_PROVIDER, 3, f));
}

#[test]
fn a_cloudflare_control_plane_set_up_from_the_cli_holds_with_each_call_and_store_operation_failing() {
    sweep("a cloudflare control plane set up from the cli", |f| flow_step(&EVERY_PROVIDER, 4, f));
}

#[test]
fn a_move_from_aws_to_cloudflare_from_the_cli_holds_with_each_call_and_store_operation_failing() {
    sweep("a move from aws to cloudflare from the cli", |f| flow_step(&EVERY_PROVIDER, 5, f));
}

#[test]
fn aws_runners_taken_out_of_the_cloudflare_control_plane_holds_with_each_call_and_store_operation_failing() {
    sweep("aws runners taken out of the cloudflare control plane", |f| flow_step(&EVERY_PROVIDER, 6, f));
}

#[test]
fn the_cloudflare_control_plane_deleted_from_the_cli_holds_with_each_call_and_store_operation_failing() {
    sweep("the cloudflare control plane deleted from the cli", |f| flow_step(&EVERY_PROVIDER, 7, f));
}

#[test]
fn the_aws_control_plane_deleted_from_the_cli_holds_with_each_call_and_store_operation_failing() {
    sweep("the aws control plane deleted from the cli", |f| flow_step(&EVERY_PROVIDER, 8, f));
}

#[test]
fn a_cloudflare_control_plane_set_up_beside_another_from_the_cli_holds_with_each_call_and_store_operation_failing() {
    sweep("a cloudflare control plane set up beside another from the cli", |f| flow_step(&MOVE_TO_MODAL, 0, f));
}

#[test]
fn a_modal_control_plane_set_up_from_the_cli_holds_with_each_call_and_store_operation_failing() {
    sweep("a modal control plane set up from the cli", |f| flow_step(&MOVE_TO_MODAL, 1, f));
}

#[test]
fn cloudflare_containers_added_to_a_cloudflare_control_plane_holds_with_each_call_and_store_operation_failing() {
    sweep("cloudflare containers added to a cloudflare control plane", |f| flow_step(&MOVE_TO_MODAL, 2, f));
}

#[test]
fn a_modal_launcher_added_to_a_cloudflare_control_plane_holds_with_each_call_and_store_operation_failing() {
    sweep("a modal launcher added to a cloudflare control plane", |f| flow_step(&MOVE_TO_MODAL, 3, f));
}

#[test]
fn a_move_from_cloudflare_to_modal_from_the_cli_holds_with_each_call_and_store_operation_failing() {
    sweep("a move from cloudflare to modal from the cli", |f| flow_step(&MOVE_TO_MODAL, 4, f));
}

#[test]
fn the_cloudflare_launcher_taken_out_of_the_modal_control_plane_holds_with_each_call_and_store_operation_failing() {
    sweep("the cloudflare launcher taken out of the modal control plane", |f| flow_step(&MOVE_TO_MODAL, 5, f));
}

#[test]
fn a_cloudflare_control_plane_that_moved_away_deleted_holds_with_each_call_and_store_operation_failing() {
    sweep("a cloudflare control plane that moved away deleted", |f| flow_step(&MOVE_TO_MODAL, 6, f));
}

#[test]
fn a_modal_control_plane_deleted_from_the_cli_holds_with_each_call_and_store_operation_failing() {
    sweep("a modal control plane deleted from the cli", |f| flow_step(&MOVE_TO_MODAL, 7, f));
}

#[test]
fn cloudflare_containers_settings_changed_from_the_cli_holds_with_each_call_and_store_operation_failing() {
    sweep("cloudflare containers settings changed from the cli", |f| flow_step(&UPDATE, 3, f));
}

#[test]
fn a_cloudflare_control_plane_updated_from_the_cli_holds_with_each_call_and_store_operation_failing() {
    sweep("a cloudflare control plane updated from the cli", |f| flow_step(&UPDATE, 4, f));
}

#[test]
fn a_modal_control_plane_updated_from_the_cli_holds_with_each_call_and_store_operation_failing() {
    sweep("a modal control plane updated from the cli", |f| flow_step(&UPDATE, 5, f));
}

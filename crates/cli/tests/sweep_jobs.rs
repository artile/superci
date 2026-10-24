//! Jobs' lives with each of their calls and store operations failing in turn (`superci_testkit::sweep`): a GitHub job
//! on an AWS spot machine, and a run from a computer of two jobs, one after the other. Whatever fails, nothing panics,
//! no error is answered without saying what failed, and the job ends as GitHub (or the computer) can see: done, or
//! failed saying why; never left launching or running with nothing to move it on.
use serde_json::Value;
use superci_testkit::{serve, sweep, Fault, Outcome, Runner, Served, PLANE};

/// A control plane of AWS's machines, its GitHub connected, served at https://plane.acme.test, AWS answering as it
/// answered Test.
fn aws_plane() -> Served {
    let served = serve(|c| {
        (c.aws_own, c.aws_own_creds) = (Some(("111111111111".into(), "us-east-1".into())), Some(superci_core::aws::Credentials { access_key_id: "ASIAX".into(), secret_access_key: "secret".into(), session_token: Some("tok".into()), expires_at_ms: u64::MAX }));
    });
    served.connect_github();
    *served.public.lock().unwrap() = Some("https://plane.acme.test".into());
    served.aws.put(&format!("/superci/{PLANE}/ROUTING"), r#"{"order":[{"cloud":"aws","max_jobs":100},{"cloud":"aws-on-demand"},{"cloud":"cloudflare","off":true}]}"#);
    served.aws.replaying(include_str!("../fixtures/aws/launch/spot.jsonl"), &[]);
    served.store.0.lock().unwrap().insert("aws:no-fleet".into(), served.now_ms().to_string());
    served
}

/// The control plane's record of job `id`, if it has one.
fn job(served: &Served, id: u64) -> Option<Value> { served.store.under(&format!("job:{id}")).into_iter().next().map(|(_, j)| j) }

/// A few minutes on, its upkeep, and each delivery it asked GitHub to send again sent again, as GitHub does.
fn settle(served: &Served) {
    for _ in 0..2 {
        served.later(5 * 60_000 + 1_000);
        served.try_upkeep();
        for again in served.github.to_send_again() { served.webhook(again); }
    }
}

/// A GitHub job's life: queued, its spot machine started on AWS, its runner registered and taking it, finished;
/// upkeep after.
fn github_job(fault: Fault) -> Outcome {
    let served = aws_plane();
    served.arm(fault);
    let (id, queued) = served.github.queue("tests", "superci", served.now_ms());
    served.webhook(queued);
    // Its runner takes it, once its machine is up with the runner GitHub has (a start that failed is tried again by
    // the upkeep, a spot machine AWS had none of a few minutes on).
    let up = || job(&served, id).filter(|j| j["state"] == "launched" && j["machine_id"].is_string()).and_then(|j| j["runner"].as_str().map(str::to_string)).filter(|r| served.github.runner_names().contains(r));
    let mut runner = None;
    for _ in 0..5 { settle(&served); runner = up(); if runner.is_some() { break } }
    if let Some(runner) = &runner {
        served.github.ready(runner);
        served.webhook(served.github.start_on(id, runner));
        settle(&served);
    }
    served.webhook(served.github.finish(id, if runner.is_some() { "success" } else { "cancelled" }, "ok\n", served.now_ms()));
    settle(&served);
    served.later(3_600_000);
    served.try_upkeep();
    let out = Outcome::new(served);
    let served = &out.served;
    let j = job(served, id);
    let state = j.as_ref().map(|j| j["state"].as_str().unwrap_or_default().to_string()).unwrap_or_default();
    // Its record kept, and over: done, or failed saying why.
    let said = j.as_ref().map(|j| j["error"].as_str().unwrap_or_default().to_string()).unwrap_or_default();
    out.check(j.is_some(), || "no record of the job: lost".into());
    out.check(j.is_none() || ["done", "swept", "cancelled"].contains(&state.as_str()) || (state == "failed" && !said.is_empty()), || format!("the job is left {state}: {}", j.clone().unwrap_or_default()));
    // Done as GitHub says it ended.
    let ended = served.github.job(id)["conclusion"].clone();
    out.check(state != "done" || j.as_ref().is_some_and(|j| j["conclusion"] == ended), || format!("done, but not as GitHub says it ended ({ended}): {}", j.clone().unwrap_or_default()));
    // No machine AWS started left on: each is the job's own (its runner ends it, and it stops itself), or was ended.
    let machine = j.as_ref().and_then(|j| j["machine_id"].as_str().map(str::to_string));
    let asked = served.aws.asked.lock().unwrap().clone();
    let said = |action: &str| { let a = format!("Action={action}&"); asked.iter().filter(|r| r["asked"].as_str().unwrap_or_default().contains(&a) && r["status"] == 200).cloned().collect::<Vec<Value>>() };
    let ids = |text: &str| text.split("<instanceId>").skip(1).filter_map(|s| s.split('<').next()).map(str::to_string).collect::<Vec<_>>();
    let ended: Vec<String> = said("TerminateInstances").iter().flat_map(|a| url::form_urlencoded::parse(a["asked"].as_str().unwrap_or_default().as_bytes()).filter(|(k, _)| k.starts_with("InstanceId")).map(|(_, v)| v.to_string()).collect::<Vec<_>>()).collect();
    for started in said("RunInstances").iter().flat_map(|a| ids(a["answer"].as_str().unwrap_or_default())) {
        out.check(machine.as_deref() == Some(started.as_str()) || ended.contains(&started), || format!("machine {started} was started and left on, the job's being {machine:?}"));
    }
    out
}

#[test]
fn a_github_jobs_life_on_an_aws_spot_machine_holds_with_each_call_and_store_operation_failing() {
    sweep("a GitHub job's life on AWS", github_job);
}

/// The runner of the job of run `run` shown as `name`, when its machine is up (started, and not stopped since): a
/// machine that was stopped has no runner to take the job.
fn on_a_machine(served: &Served, run: &str, name: &str) -> Option<Runner> {
    let runner = Runner::of_job(served, run, name)?;
    let token = runner.at.split("/runner/").nth(1)?.split('/').next()?.to_string();
    let j = served.store.under("job:").into_iter().map(|(_, j)| j).find(|j| j["local"] == token.as_str())?;
    let machine = j["machine_id"].as_str()?;
    let started = served.machines.0.lock().unwrap().iter().filter(|n| machine == format!("machine-{n}")).count();
    let stopped = served.machines.3.lock().unwrap().iter().filter(|m| *m == machine).count();
    (["launched", "running"].contains(&j["state"].as_str().unwrap_or_default()) && started > stopped).then_some(runner)
}

/// A run from a computer of two jobs, the second needing the first: each taken by its runner and finished; upkeep after.
fn run_of_two(fault: Fault) -> Outcome {
    let served = serve(|_| {});
    served.arm(fault);
    let wf = "jobs:\n  a:\n    runs-on: superci\n    steps:\n      - run: echo a\n  b:\n    needs: a\n    runs-on: superci\n    steps:\n      - run: echo b\n";
    let body = serde_json::json!({ "workflow": wf, "path": ".github/workflows/ci.yml", "repository": "acme/app", "ref": "refs/heads/main" });
    let answered = ureq::post(&format!("{}/runs", served.url)).header("authorization", &format!("Bearer {}", served.run_key)).config().http_status_as_error(false).build().send(body.to_string());
    let (status, said) = match answered { Ok(mut r) => (r.status().as_u16(), r.body_mut().read_to_string().unwrap_or_default()), Err(e) => (0, e.to_string()) };
    let run = serde_json::from_str::<Value>(&said).ok().and_then(|v| v["run_id"].as_str().map(str::to_string));
    let mut finished = vec![];
    if let Some(run) = &run {
        for name in ["a", "b"] {
            // (Its machine started late, when it was refused at first: the upkeep starts it.)
            let mut runner = on_a_machine(&served, run, name);
            for _ in 0..3 { if runner.is_some() { break } served.later(61_000); served.try_upkeep(); runner = on_a_machine(&served, run, name) }
            let Some(runner) = runner else { break };
            // (A runner asks again for what was not answered, as GitHub's runner does.)
            for _ in 0..3 { if runner.take() == 200 { break } served.later(61_000); served.try_upkeep(); }
            for _ in 0..3 { if runner.finish("succeeded") < 300 { break } served.later(61_000); served.try_upkeep(); }
            finished.push(name);
        }
    }
    served.later(10 * 60_000);
    served.try_upkeep();
    served.later(10 * 60_000);
    served.try_upkeep();
    let out = Outcome::new(served);
    let served = &out.served;
    // Refused: said why, nothing left of it.
    out.check(run.is_some() || (status >= 400 && !said.trim().is_empty()), || format!("POST /runs answered {status} without a run or a reason: {said}"));
    // Nothing of a refused run is started later: the computer was told it did not start.
    let Some(run) = run else {
        let left: Vec<Value> = served.store.under("run:").into_iter().map(|(_, r)| r).collect();
        out.check(left.is_empty() && served.store.under("job:").is_empty(), || format!("refused ({status} {said}), yet the run goes on: {left:?}"));
        return out
    };
    let record = served.store.under(&format!("run:{run}")).into_iter().next().map(|(_, r)| r);
    out.check(record.is_some(), || "the run's record is gone".into());
    let jobs: Vec<Value> = record.as_ref().and_then(|r| r["jobs"].as_array().cloned()).unwrap_or_default();
    // Each job over: done, or failed saying why.
    for j in &jobs {
        let state = j["state"].as_str().unwrap_or_default();
        let ok = state == "done" || (state == "failed" && !j["error"].as_str().unwrap_or_default().is_empty());
        out.check(ok, || format!("job {} of the run is left {state} (finished: {finished:?}): {j}", j["name"]));
    }
    // And the control plane's own records of them, too.
    for (k, j) in served.store.under("job:") {
        let state = j["state"].as_str().unwrap_or_default();
        out.check(!["launching", "launched", "running", "waiting"].contains(&state), || format!("{k} is left {state}: {j}"));
    }
    out
}

#[test]
fn a_run_of_two_jobs_from_a_computer_holds_with_each_call_and_store_operation_failing() {
    sweep("a run of two jobs from a computer", run_of_two);
}

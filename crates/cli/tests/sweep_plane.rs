//! The control plane's other flows with each of their calls and store operations failing in turn
//! (`superci_testkit::sweep`): a GitLab job's life, a machine's cache and a run's artifacts, the layer store, the
//! upkeep of a busy control plane, its settings, keys and secrets, and what machines report (hello, start, a spot
//! machine taken back). Whatever fails, nothing panics, no error is answered without saying what failed, and once
//! time passes and the upkeep runs clean nothing is left stuck.
use serde_json::{json, Value};
use superci_testkit::{serve, sweep, Fault, Outcome, Served};

/// One sweep at a time: each runs eight side by side already, and more would use up this computer's ports.
fn one_at_a_time() -> std::sync::MutexGuard<'static, ()> {
    static ONE: std::sync::Mutex<()> = std::sync::Mutex::new(());
    ONE.lock().unwrap_or_else(|e| e.into_inner())
}

/// The control plane's record under `key` (`job:gl…`), if it has one.
fn record(served: &Served, key: &str) -> Option<Value> { served.store.under(key).into_iter().find(|(k, _)| k == key).map(|(_, j)| j) }

/// A few minutes on, and its upkeep, twice.
fn settle(served: &Served) {
    for _ in 0..2 {
        served.later(5 * 60_000 + 1_000);
        served.try_upkeep();
    }
}

/// A request to the control plane, whatever it answers: its status and body.
fn ask(served: &Served, method: &str, path: &str, auth: Option<&str>, body: Option<Value>) -> (u16, String) {
    let agent: ureq::Agent = ureq::Agent::config_builder().http_status_as_error(false).build().into();
    let url = format!("{}{path}", served.url);
    let answered = match (method, body) {
        ("GET", _) => { let mut r = agent.get(&url); if let Some(a) = auth { r = r.header("authorization", &format!("Bearer {a}")) } r.call() }
        (m, body) => { let _ = m; let mut r = agent.post(&url); if let Some(a) = auth { r = r.header("authorization", &format!("Bearer {a}")) } r.send(body.unwrap_or(json!({})).to_string()) }
    };
    match answered { Ok(mut r) => (r.status().as_u16(), r.body_mut().read_to_string().unwrap_or_default()), Err(e) => (0, e.to_string()) }
}

/// Over: done, cancelled, swept, or failed saying why.
fn over(j: &Value) -> bool {
    let state = j["state"].as_str().unwrap_or_default();
    ["done", "swept", "cancelled"].contains(&state) || (state == "failed" && !j["error"].as_str().unwrap_or_default().is_empty())
}

/// A GitLab job's life on the control plane's own machines: pending, its runner made and its machine started, taken by
/// that runner, finished; upkeep after.
fn gitlab_job(fault: Fault) -> Outcome {
    let served = serve(|_| {});
    served.connect_gitlab();
    // (Its project's jobs sent here: the upkeep asks GitLab of those its events did not tell.)
    assert_eq!(ask(&served, "POST", "/gitlab/projects", Some(&served.key), Some(json!({ "all_on": true }))).0, 200);
    served.arm(fault);
    let (id, pending) = served.gitlab.pending("tests", &["superci"], served.now_ms());
    served.gitlab_webhook(pending);
    let key = format!("job:gl{id}");
    // Its runner takes it once its machine is up (a start that failed, or an event that did not come, is taken up by
    // the upkeep a few minutes on).
    let up = || record(&served, &key).filter(|j| j["state"] == "launched" && j["machine_id"].is_string()).and_then(|j| j["runner_id"].as_u64());
    let mut runner = up();
    for _ in 0..5 { if runner.is_some() { break } settle(&served); runner = up() }
    if let Some(r) = runner {
        served.gitlab_webhook(served.gitlab.running_on(id, r, served.now_ms()));
        settle(&served);
    }
    served.gitlab_webhook(served.gitlab.finish(id, if runner.is_some() { "success" } else { "canceled" }, "ok\n", served.now_ms()));
    settle(&served);
    // Long past any machine's time: what is left of it, removed by a clean upkeep.
    served.later(6 * 86_400_000);
    served.try_upkeep();
    served.try_upkeep();
    let out = Outcome::new(served);
    let served = &out.served;
    let j = record(served, &key);
    out.check(j.is_some(), || "no record of the job: lost".into());
    out.check(j.as_ref().is_none_or(over), || format!("the job is left as it was: {}", j.clone().unwrap_or_default()));
    let ended = served.gitlab.job(id)["status"].clone();
    out.check(j.as_ref().is_none_or(|j| j["state"] != "done" || j["conclusion"] == ended), || format!("done, but not as GitLab says it ended ({ended}): {}", j.clone().unwrap_or_default()));
    let runners = served.store.under("glrunner:");
    out.check(runners.is_empty(), || format!("GitLab runners kept long past their machine's time: {runners:?}"));
    out
}

#[test]
fn a_gitlab_jobs_life_holds_with_each_call_and_store_operation_failing() {
    let _one = one_at_a_time();
    sweep("a GitLab job's life", gitlab_job);
}

/// Each answer of the control plane's that refused or failed says why.
fn refusals_say_why(out: &Outcome) {
    for (asked, status, body) in out.served.faults().answers() {
        out.check(status < 400 || !body.trim().is_empty(), || format!("{asked} was answered {status} saying nothing"));
    }
}

/// A control plane connected to AWS by its role (AWS's credentials asked of STS) and a machine of its own with the
/// cache's link, a GitHub job on it: the link.
fn cache_plane() -> (Served, String) {
    let served = serve(|_| {});
    served.connect_github();
    let now = served.now_ms();
    let link = "machine-cache-link-000000000021".to_string();
    {
        let mut m = served.store.0.lock().unwrap();
        m.insert("aws".into(), json!({ "account_id": "111111111111", "region": "us-east-1", "role_arn": "arn:aws:iam::111111111111:role/superci-testplane0001", "connected": true }).to_string());
        m.insert(format!("cachem:{link}"), json!({ "region": "us-east-1", "arch": "x86_64", "until": u64::MAX / 4, "runner": "a-runner" }).to_string());
        m.insert("runner:a-runner".into(), "5001".into());
        m.insert("job:5001".into(), json!({ "job_id": 5001, "run_id": 1, "repo": "acme/app", "repo_id": 197814629, "branch": "main", "default_branch": "main", "state": "running", "at_ms": now,
            "installation_id": 42, "cloud": "aws", "seen_in_progress": true, "name": "test" }).to_string());
    }
    (served, link)
}

/// `superci-cache`'s call `what` on the machine of `link`: its status and what it said.
fn cache_call(served: &Served, link: &str, what: &str, body: Value) -> (u16, Value) {
    let (status, said) = ask(served, "POST", &format!("/cache/{link}/{what}"), None, Some(body));
    (status, serde_json::from_str(&said).unwrap_or(Value::Null))
}

/// The same, asked once more when it failed (as `superci-cache` does).
fn cache_call_again(served: &Served, link: &str, what: &str, body: Value) -> (u16, Value) {
    let first = cache_call(served, link, what, body.clone());
    if first.0 >= 500 || first.0 == 0 { cache_call(served, link, what, body) } else { first }
}

/// A month on, the upkeep (and an hour later again): what is past its time goes.
fn a_month_on(served: &Served) {
    served.later(31 * 86_400_000);
    served.try_upkeep();
    served.later(3_600_001);
    served.try_upkeep();
}

/// The index points only at pieces that were said to be written: a finished entry has the pieces a finish gave.
fn index_holds(out: &Outcome, given: &[&str]) {
    for (k, e) in out.served.store.under("cache:").into_iter().chain(out.served.store.under("artifact:")) {
        if e["done"] != true { continue }
        let pieces = e["pieces"].to_string();
        out.check(given.contains(&pieces.as_str()), || format!("{k} is finished with pieces never said written: {e}"));
    }
}

/// What `superci-cache` asks on a machine: a GitHub job's entry looked up, saved and found; a tool's artifact (Turbo's)
/// saved and found; what the dashboard reads and takes out of the cache; a month on, the cache's upkeep.
fn machine_cache(fault: Fault) -> Outcome {
    let (served, link) = cache_plane();
    let github = superci_testkit::GitHub::job_token(&json!({ "repository_id": "197814629", "ac": json!([{ "Scope": "refs/heads/main", "Permission": 3 }]).to_string(), "exp": 4_102_444_800u64 }));
    served.arm(fault);
    let version = "c".repeat(64);
    let (call, again) = (|what: &str, body: Value| cache_call(&served, &link, what, body), |what: &str, body: Value| cache_call_again(&served, &link, what, body));
    let mut problems = vec![];
    // A GitHub job's entry: missed, saved, found.
    let lookup = || call("lookup", json!({ "token": github, "key": "deps-1", "restore_keys": ["deps-"], "version": version }));
    let _ = lookup();
    let _ = again("begin", json!({ "token": github, "key": "deps-1", "version": version }));
    let _ = again("sign", json!({ "token": github, "key": "deps-1", "version": version, "names": ["p-1", "p-2"] }));
    let finished = again("finish", json!({ "token": github, "key": "deps-1", "version": version, "pieces": [["p-1", 10], ["p-2", 20]] }));
    // (A lookup the store did not answer is a miss: the job goes on without the cache. Asked again, it is found.)
    let found = match lookup() { f if f.1["hit"] == true => f, _ => lookup() };
    if finished.1["ok"] == true && found.0 == 200 && found.1["hit"] != true { problems.push(format!("saved ({finished:?}), then not found: {found:?}")) }
    if found.1["hit"] == true && found.1["pieces"].as_array().map_or(0, |p| p.len()) != 2 { problems.push(format!("found without its two pieces: {found:?}")) }
    // Found by its restore key, from a job of another key.
    let _ = call("lookup", json!({ "token": github, "key": "deps-2", "restore_keys": ["deps-"], "version": version }));
    // A tool's artifact, asked by the machine with no token.
    let _ = call("tool-get", json!({ "tool": "turbo", "hash": "0123456789abcdef" }));
    let _ = again("tool-begin", json!({ "tool": "turbo", "hash": "0123456789abcdef" }));
    let _ = again("tool-sign", json!({ "tool": "turbo", "hash": "0123456789abcdef", "names": ["p00001"] }));
    let tool_done = again("tool-finish", json!({ "tool": "turbo", "hash": "0123456789abcdef", "size": 5, "meta": "{\"duration\":7}" }));
    let tool_get = || call("tool-get", json!({ "tool": "turbo", "hash": "0123456789abcdef" }));
    let tool_found = match tool_get() { f if f.1["hit"] == true => f, _ => tool_get() };
    if tool_done.1["ok"] == true && tool_found.0 == 200 && tool_found.1["hit"] != true { problems.push(format!("a tool's artifact saved ({tool_done:?}), then not found: {tool_found:?}")) }
    // What the dashboard reads and takes out.
    let _ = ask(&served, "GET", "/caches", Some(&served.key), None);
    let _ = ask(&served, "GET", "/caches?repo=gh197814629", Some(&served.key), None);
    let _ = ask(&served, "GET", "/caches?repo=acme/app", Some(&served.key), None);
    let _ = ask(&served, "POST", "/caches/delete", Some(&served.key), Some(json!({ "repo": "gh197814629", "key": "0123456789abcdef" })));
    a_month_on(&served);
    let out = Outcome::new(served);
    for p in problems { out.check(false, || p) }
    refusals_say_why(&out);
    index_holds(&out, &["[[\"p-1\",10],[\"p-2\",20]]", "[[\"p00000\",5]]"]);
    // A month on, nothing is left: each entry taken out once too old, none begun and never finished kept.
    let left: Vec<String> = out.served.store.under("cache:").into_iter().map(|(k, _)| k).collect();
    out.check(left.is_empty(), || format!("cache entries kept past their time: {left:?}"));
    if fault == Fault::None { out.check(found.1["hit"] == true && tool_found.1["hit"] == true, || format!("the clean run did not go through: {found:?} {tool_found:?}")) }
    out
}

#[test]
fn a_machines_cache_holds_with_each_call_and_store_operation_failing() {
    let _one = one_at_a_time();
    sweep("a machine's cache", machine_cache);
}

/// A run from a computer of one job, its artifacts kept by that job: kept, listed, fetched, one deleted; read from the
/// dashboard; a month on, the upkeep.
fn run_artifacts(fault: Fault) -> Outcome {
    let (served, link) = cache_plane();
    let wf = "jobs:\n  a:\n    runs-on: superci\n    steps:\n      - run: echo a\n";
    let (status, said) = ask(&served, "POST", "/runs", Some(&served.run_key), Some(json!({ "workflow": wf, "path": ".github/workflows/ci.yml", "repository": "acme/app", "ref": "refs/heads/main" })));
    assert_eq!(status, 200, "{said}");
    let run = serde_json::from_str::<Value>(&said).unwrap()["run_id"].as_str().unwrap().to_string();
    let runner = superci_testkit::Runner::of_job(&served, &run, "a").expect("its job started");
    let token = runner.handed(&served)["resources"]["endpoints"].as_array().into_iter().flatten().find_map(|e| e["authorization"]["parameters"]["AccessToken"].as_str().map(str::to_string)).unwrap();
    assert_eq!(runner.take(), 200);
    served.arm(fault);
    let (call, again) = (|what: &str, body: Value| cache_call(&served, &link, what, body), |what: &str, body: Value| cache_call_again(&served, &link, what, body));
    let mut problems = vec![];
    // The run's artifacts: one kept, one kept and deleted.
    let mut kept = vec![];
    for name in ["report", "scratch"] {
        let _ = again("artifact-begin", json!({ "token": token, "name": name }));
        let _ = again("artifact-sign", json!({ "token": token, "name": name, "names": ["p-1"] }));
        kept.push(again("artifact-finish", json!({ "token": token, "name": name, "pieces": [["p-1", 3]] })));
    }
    let _ = call("artifacts", json!({ "token": token }));
    let got = call("artifact-get", json!({ "token": token, "name": "report" }));
    if kept[0].1["ok"] == true && got.0 < 500 && got.1["pieces"].as_array().map_or(0, |p| p.len()) != 1 { problems.push(format!("kept ({:?}), then not fetched: {got:?}", kept[0])) }
    let deleted = again("artifact-delete", json!({ "token": token, "name": "scratch" }));
    let listed = call("artifacts", json!({ "token": token }));
    let names: Vec<String> = listed.1["artifacts"].as_array().into_iter().flatten().filter_map(|a| a["name"].as_str().map(str::to_string)).collect();
    if deleted.1["ok"] == true && names.iter().any(|n| n == "scratch") { problems.push(format!("deleted ({deleted:?}), yet listed: {listed:?}")) }
    // The job over (its runner asks again what was not answered).
    for _ in 0..3 { if runner.finish("succeeded") < 300 { break } }
    // What the dashboard reads of them.
    let _ = ask(&served, "GET", &format!("/runs/{run}/artifacts"), Some(&served.key), None);
    let _ = ask(&served, "GET", &format!("/runs/{run}/artifacts?name=report"), Some(&served.key), None);
    a_month_on(&served);
    let out = Outcome::new(served);
    for p in problems { out.check(false, || p) }
    refusals_say_why(&out);
    index_holds(&out, &["[[\"p-1\",3]]"]);
    // A month on, the run is no longer kept, nor its artifacts.
    let left: Vec<String> = out.served.store.under("artifact:").into_iter().map(|(k, _)| k).collect();
    out.check(left.is_empty(), || format!("artifacts kept past their run: {left:?}"));
    if fault == Fault::None { out.check(names == ["report"], || format!("the clean run did not go through: {names:?}")) }
    out
}

#[test]
fn a_runs_artifacts_hold_with_each_call_and_store_operation_failing() {
    let _one = one_at_a_time();
    sweep("a run's artifacts", run_artifacts);
}

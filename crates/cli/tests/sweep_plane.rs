//! The control plane's other flows with each of their calls and store operations failing in turn
//! (`superci_testkit::sweep`): a GitLab job's life, a machine's cache and a run's artifacts, the layer store, the
//! upkeep of a busy control plane, its settings, keys and secrets, and what machines report (hello, start, a spot
//! machine taken back). Whatever fails, nothing panics, no error is answered without saying what failed, and once
//! time passes and the upkeep runs clean nothing is left stuck.
use serde_json::{json, Value};
use superci_testkit::{serve, sweep, Fault, Outcome, Served, PLANE};

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
    let agent: ureq::Agent = ureq::Agent::config_builder().http_status_as_error(false).max_redirects(0).build().into();
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

/// A control plane on AWS (its converter there, its own credentials), its programs in its bucket, and a machine of its
/// own with the cache's link, a GitHub job on it that keeps Docker's images: the control plane and the link.
fn layers_plane() -> (Served, String) {
    let served = serve(|c| {
        (c.aws_own, c.aws_own_creds) = (Some(("111111111111".into(), "us-east-1".into())), Some(superci_core::aws::Credentials { access_key_id: "ASIAX".into(), secret_access_key: "secret".into(), session_token: Some("tok".into()), expires_at_ms: u64::MAX }));
    });
    served.connect_github();
    let bucket = superci_core::cache::bucket(PLANE, "us-east-1");
    served.github.put_object(&format!("{bucket}/{}", superci_core::cache::program_key("x86_64")), b"program");
    served.github.put_object(&format!("{bucket}/{}", superci_core::cache::layers_key("x86_64")), b"program");
    let link = "machine-cache-link-000000000031".to_string();
    let now = served.now_ms();
    {
        let mut m = served.store.0.lock().unwrap();
        m.insert(format!("cachem:{link}"), json!({ "region": "us-east-1", "arch": "x86_64", "until": u64::MAX / 4, "runner": "a-runner", "job": "job:5002" }).to_string());
        m.insert("job:5002".into(), json!({ "job_id": 5002, "run_id": 1, "repo": "acme/app", "repo_id": 197814629, "branch": "main", "default_branch": "main", "state": "running", "at_ms": now,
            "installation_id": 42, "cloud": "aws", "seen_in_progress": true, "name": "test", "workflow": "CI", "label": "superci/docker-cache=true" }).to_string());
    }
    (served, link)
}

/// The layer store as a machine uses it: its index and links, a layer it pulled proposed, uploaded and converted, what
/// its job had said after it, its programs; the upkeep, until the converter's word is read, and a month on.
fn layer_store(fault: Fault) -> Outcome {
    let (served, link) = layers_plane();
    let digest = |c: char| format!("sha256:{}", c.to_string().repeat(64));
    let now = served.now_ms();
    // A public layer the store has.
    served.store.0.lock().unwrap().insert(format!("layer:us-east-1:{}", digest('1')), json!({ "size": 100, "diff_id": "", "erofs": { "key": format!("layers/{}.erofs", "1".repeat(64)), "size": 1 },
        "blob": { "key": format!("layers/{}.blob", "1".repeat(64)), "size": 100 }, "made": now, "used": now - 2 * 86_400_000, "public": { "registry": "docker.io", "repository": "library/node", "checked": now } }).to_string());
    // What the converter will say of the layer the machine uploads (read once it has been given it).
    let bucket = superci_core::cache::bucket(PLANE, "us-east-1");
    let hex = "6".repeat(64);
    served.github.put_object(&format!("{bucket}/layers/done/{hex}.json"), json!({ "digest": digest('6'), "diff_id": "sha256:diff", "erofs": { "key": format!("layers/{hex}.erofs"), "size": 1 }, "blob": { "key": format!("layers/{hex}.blob"), "size": 500 } }).to_string().as_bytes());
    served.arm(fault);
    let (call, again) = (|what: &str, body: Value| cache_call(&served, &link, what, body), |what: &str, body: Value| cache_call_again(&served, &link, what, body));
    for what in ["layers", "layers.json", "layers-program", "program", "cache"] { let _ = ask(&served, "GET", &format!("/cache/{link}/{what}"), None, None); }
    let _ = again("layers-links", json!({ "digests": [digest('1')] }));
    // A layer it pulled that the store lacks: proposed, put at the link given, said uploaded.
    let proposed = again("layers-propose", json!({ "layers": [{ "digest": digest('6'), "size": 500 }, { "digest": digest('1'), "size": 100 }] }));
    let a = proposed.1["layers"][digest('6')].clone();
    let mut uploaded = None;
    if let (Some(url), Some(sum)) = (a["url"].as_str(), a["headers"]["x-amz-checksum-sha256"].as_str()) {
        served.github.put_at(url, &[0u8; 500], &[("x-amz-checksum-sha256", sum)]);
        uploaded = Some(again("layers-uploaded", json!({ "id": a["id"] })));
    }
    // What its job had, said after it.
    let _ = again("layers-kept", json!({ "pulled": 2, "pulled_bytes": 600, "served_digests": [digest('1')], "used": [digest('1'), digest('6')] }));
    // The upkeep reads what the converter said (an upkeep that failed is followed by others).
    for _ in 0..3 { served.later(61_000); served.try_upkeep(); }
    let had = record(&served, &format!("layer:us-east-1:{}", digest('6')));
    let (converting, queued) = (served.store.under("lconv:"), served.store.under("lwant:"));
    a_month_on(&served);
    let out = Outcome::new(served);
    refusals_say_why(&out);
    // Each layer kept has both its files, as the converter made them.
    out.check(had.as_ref().is_none_or(|l| l["erofs"]["key"].as_str().is_some_and(|k| !k.is_empty()) && l["blob"]["key"].as_str().is_some_and(|k| !k.is_empty())), || format!("a layer kept without its files: {had:?}"));
    // Said uploaded and taken (converting): kept once the converter's word is read, nothing left under way.
    let taken = uploaded.as_ref().is_some_and(|u| u.0 == 200);
    out.check(!taken || (had.is_some() && converting.is_empty() && queued.is_empty()), || format!("uploaded ({uploaded:?}), and not kept ({had:?}) or left under way ({converting:?} {queued:?})"));
    // Uploaded, yet refused for good: a layer a machine gave is lost.
    out.check(uploaded.as_ref().is_none_or(|u| u.0 == 200 || u.0 >= 500), || format!("an upload refused: {uploaded:?}"));
    // A month on, both layers are past their time.
    let left: Vec<String> = out.served.store.under("layer:").into_iter().map(|(k, _)| k).collect();
    out.check(left.is_empty(), || format!("layers kept past their time: {left:?}"));
    if fault == Fault::None { out.check(taken && had.is_some(), || format!("the clean run did not go through: {proposed:?} {uploaded:?}")) }
    out
}

#[test]
fn the_layer_store_holds_with_each_call_and_store_operation_failing() {
    let _one = one_at_a_time();
    sweep("the layer store", layer_store);
}

/// Every job on the control plane's records is over once time has passed: none left launching, launched, running or
/// waiting with nothing to move it on.
fn nothing_stuck(out: &Outcome) {
    for (k, j) in out.served.store.under("job:") {
        let state = j["state"].as_str().unwrap_or_default();
        out.check(!["launching", "launched", "running", "waiting", "orphan"].contains(&state), || format!("{k} is left {state}: {j}"));
    }
}

/// A busy control plane of its own machines, GitHub and GitLab connected: jobs launched, running, refused a machine
/// (waiting), whose start was cut off, past their machine's time, over with their indexes left, machines of jobs over
/// (orphans); and what the upkeep forgets: launch templates, working trees, runs and their logs, machines' cache links
/// and GitLab runners past their time, spot notes and reruns of days ago. Its upkeep run every few minutes with
/// GitHub ending the jobs meanwhile; hours and days on, nothing is left stuck.
fn busy_upkeep(fault: Fault) -> Outcome {
    let served = serve(|_| {});
    served.connect_github();
    served.connect_gitlab();
    let now = served.now_ms();
    let (launched, q) = served.github.queue("launched", "superci", now);
    served.webhook(q);
    let (running, q) = served.github.queue("running", "superci", now);
    served.webhook(q);
    served.webhook(served.github.start(running));
    // Refused a machine: it waits.
    served.machines_refuse(Some("no room for another container"));
    let (waiting, q) = served.github.queue("waiting", "superci", now);
    served.webhook(q);
    served.machines_refuse(None);
    {
        let mut m = served.store.0.lock().unwrap();
        let job = |id: u64, state: &str, at: u64, extra: Value| {
            let mut j = json!({ "job_id": id, "run_id": 1, "repo": "acme/app", "state": state, "label": "superci", "at_ms": at, "installation_id": 42, "app_id": 1001, "cloud": "cloudflare", "seen_in_progress": false, "name": "test", "workflow": "CI" });
            for (k, v) in extra.as_object().into_iter().flatten() { j[k] = v.clone() }
            j.to_string()
        };
        // Its start cut off a while ago; running past its machine's time; machines of jobs over; over days ago with its
        // indexes left.
        for (id, state, at, extra) in [(701u64, "launching", now - 10 * 60_000, json!({})), (702, "running", now - 3 * 86_400_000, json!({ "machine_id": "machine-702", "seen_in_progress": true, "began": true, "started_ms": now - 3 * 86_400_000 })),
            (703, "orphan", now - 60_000, json!({ "machine_id": "machine-703" })), (704, "done", now - 3 * 86_400_000, json!({ "ended_ms": now - 3 * 86_400_000 + 60_000, "conclusion": "success", "seen_in_progress": true, "began": true }))] {
            m.insert(format!("job:{id}"), job(id, state, at, extra));
            m.insert(format!("live:{id}"), String::new());
        }
        let old = now - 8 * 86_400_000;
        m.insert("aws:template:us-east-1:superci-lt-old".into(), old.to_string());
        m.insert("rtree-at:tree0001".into(), old.to_string());
        m.insert("rtree:tree0001:00000:000".into(), "\"cGFydA==\"".into());
        m.insert("run:111".into(), json!({ "id": 111, "at_ms": old, "jobs": [{ "token": "oldtoken0000000000000001", "state": "done" }] }).to_string());
        m.insert("rjob:oldtoken0000000000000001".into(), json!({ "message": {}, "job_id": 1 }).to_string());
        m.insert("rline:oldtoken0000000000000001:0000000000001000".into(), "line".into());
        m.insert("rlog:oldtoken0000000000000001:0001:0000".into(), "\"bG9n\"".into());
        m.insert("cachem:machine-cache-link-000000000041".into(), json!({ "region": "us-east-1", "arch": "x86_64", "until": old, "runner": "gone" }).to_string());
        m.insert("glrunner:61".into(), json!({ "id": 61, "gitlab": "", "at_ms": old }).to_string());
        m.insert(format!("spot:interrupted:{old}:1"), old.to_string());
        m.insert(format!("rerun:acme/app:1:test"), old.to_string());
        m.insert("artifact:111:report".into(), json!({ "id": "report", "region": "us-east-1", "size": 3, "made": old, "done": true, "pieces": [["p-1", 3]] }).to_string());
    }
    served.arm(fault);
    // Its upkeep every few minutes, GitHub ending each job meanwhile.
    for round in 0..4 {
        served.later(4 * 60_000 + 1_000);
        served.try_upkeep();
        if round == 1 {
            for id in [launched, running, waiting] { served.webhook(served.github.finish(id, "success", "ok\n", served.now_ms())); }
        }
        for again in served.github.to_send_again() { served.webhook(again); }
    }
    // Hours, then days on.
    for ms in [3_600_000, 6 * 3_600_000, 6 * 86_400_000] { served.later(ms); served.try_upkeep(); served.try_upkeep(); }
    let out = Outcome::new(served);
    refusals_say_why(&out);
    nothing_stuck(&out);
    let served = &out.served;
    // What is past its time is forgotten.
    for prefix in ["aws:template:", "rtree", "run:111", "rjob:oldtoken", "rline:oldtoken", "rlog:oldtoken", "cachem:", "glrunner:", "spot:interrupted:", "rerun:", "artifact:"] {
        let left = served.store.under(prefix);
        out.check(left.is_empty(), || format!("kept past its time: {left:?}"));
    }
    // The jobs over are no longer cared for.
    let live: Vec<String> = served.store.under("live:").into_iter().map(|(k, _)| k).collect();
    out.check(live.is_empty(), || format!("jobs over still cared for: {live:?}"));
    out
}

#[test]
fn the_upkeep_of_a_busy_control_plane_holds_with_each_call_and_store_operation_failing() {
    let _one = one_at_a_time();
    sweep("the upkeep of a busy control plane", busy_upkeep);
}

/// GitHub jobs through a Cloudflare and a Modal launcher (the control plane's own containers off): each
/// started, taken, finished; the upkeep after.
fn launchers(fault: Fault) -> Outcome {
    let served = serve(|_| {});
    served.connect_github();
    let launchers = r#"[{"cloud":"cloudflare","url":"https://superci-runners.acme.workers.dev"},{"cloud":"modal","url":"https://acme--superci-runners.modal.run"}]"#;
    for (k, v) in [("CONTAINERS", "off"), ("LAUNCHERS", launchers), ("CF_IMAGE", "https://images.example.com/full:1")] { served.aws.put(&format!("/superci/{PLANE}/{k}"), v) }
    served.arm(fault);
    let now = served.now_ms();
    let (cf, q) = served.github.queue("on cloudflare", "superci", now);
    served.webhook(q);
    let (modal, q) = served.github.queue("on modal", "superci/provider=modal/gpu=t4", now);
    served.webhook(q);
    settle(&served);
    for id in [cf, modal] {
        served.webhook(served.github.start(id));
        served.webhook(served.github.finish(id, "success", "ok\n", served.now_ms()));
    }
    settle(&served);
    for again in served.github.to_send_again() { served.webhook(again); }
    for ms in [3_600_000, 6 * 86_400_000] { served.later(ms); served.try_upkeep(); served.try_upkeep(); }
    let out = Outcome::new(served);
    refusals_say_why(&out);
    nothing_stuck(&out);
    if fault == Fault::None {
        let clouds: Vec<String> = [cf, modal].iter().map(|id| record(&out.served, &format!("job:{id}")).map(|j| format!("{} {}", j["cloud"], j["state"])).unwrap_or_default()).collect();
        out.check(clouds == ["\"cloudflare\" \"done\"", "\"modal\" \"done\""], || format!("the clean run did not go through: {clouds:?}"));
    }
    out
}

#[test]
fn jobs_through_cloudflare_and_modal_launchers_hold_with_each_call_and_store_operation_failing() {
    let _one = one_at_a_time();
    sweep("jobs through launchers", launchers);
}

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

/// What a spot machine reports of its job: how its start went (its times, its runner's log, the job message, a page of
/// a step's log as it runs), then AWS taking it back; GitHub ends the job, and it is run again off spot machines; its
/// page and log read; the upkeep after.
fn machine_reports(fault: Fault) -> Outcome {
    let served = aws_plane();
    let (id, q) = served.github.queue("tests", "superci", served.now_ms());
    served.webhook(q);
    // (AWS's first pools full: a machine from another a few minutes on.)
    for _ in 0..5 {
        if record(&served, &format!("job:{id}")).is_some_and(|j| j["machine_id"].is_string() && j["runner"].as_str().is_some_and(|r| served.github.runner_names().iter().any(|n| n == r))) { break }
        served.later(5 * 60_000 + 1_000);
        served.upkeep();
    }
    let j = record(&served, &format!("job:{id}")).expect("its record");
    let (runner, hello, notice) = (j["runner"].as_str().unwrap_or_default().to_string(), j["hello"].as_str().unwrap_or_default().to_string(), j["notice"].as_str().unwrap_or_default().to_string());
    assert!(!runner.is_empty() && !hello.is_empty() && !notice.is_empty() && served.github.runner_names().contains(&runner), "{j} {:?}", served.github.runner_names());
    served.arm(fault);
    let now = served.now_ms();
    let say = |q: &str, body: &str| -> (u16, String) {
        let agent: ureq::Agent = ureq::Agent::config_builder().http_status_as_error(false).build().into();
        match agent.post(&format!("{}{q}", served.url)).send(body.to_string()) { Ok(mut r) => (r.status().as_u16(), r.body_mut().read_to_string().unwrap_or_default()), Err(e) => (0, e.to_string()) }
    };
    let _ = say(&format!("/hello?runner={runner}&t={hello}&script={}&boot=12.5&read={}&ready={}&listening={}&gh=3", now - 5_000, now - 4_000, now - 3_000, now - 2_000), "");
    let _ = say(&format!("/hello?runner={runner}&t={hello}&log=runner"), "[RUNNER 2026-10-15 10:00:00Z INFO] Listening for jobs\n");
    let _ = say(&format!("/hello?runner={runner}&t={hello}&worker=1"), "[WORKER 2026-10-15 10:00:01Z INFO] Job message:\n{\"steps\":[{\"id\":\"s1\",\"displayName\":\"Run tests\",\"reference\":{\"type\":\"script\"}}]}\n");
    served.github.ready(&runner);
    served.webhook(served.github.start_on(id, &runner));
    let _ = say(&format!("/hello?runner={runner}&t={hello}&live=step_1.log&at=0"), "2026-10-15T10:00:02.0000000Z running\n");
    // AWS takes it back (the machine says so twice, as two of its checks may).
    let (taken, said) = say(&format!("/interrupted?runner={runner}&t={notice}"), "");
    let _ = say(&format!("/interrupted?runner={runner}&t={notice}"), "");
    served.webhook(served.github.finish(id, "failure", "The runner has received a shutdown signal.\n", served.now_ms()));
    for again in served.github.to_send_again() { served.webhook(again); }
    for what in [format!("/job/log?id={id}"), "/status".to_string(), format!("/job/view?id={id}")] { let _ = ask(&served, "GET", &what, Some(&served.key), None); }
    settle(&served);
    for ms in [3_600_000, 6 * 86_400_000] { served.later(ms); served.try_upkeep(); served.try_upkeep(); }
    let out = Outcome::new(served);
    refusals_say_why(&out);
    nothing_stuck(&out);
    let j = record(&out.served, &format!("job:{id}")).unwrap_or_default();
    // Taken back as the machine said: the job knows it.
    out.check(taken != 200 || j["interrupted"] == true, || format!("AWS took its machine back ({said}), and the job does not say so: {j}"));
    if fault == Fault::None { out.check(taken == 200 && j["interrupted"] == true && j["rerun"].is_object(), || format!("the clean run did not go through: {taken} {j}")) }
    out
}

#[test]
fn what_a_spot_machine_reports_holds_with_each_call_and_store_operation_failing() {
    let _one = one_at_a_time();
    sweep("what a spot machine reports", machine_reports);
}

/// What the dashboard and the CLI ask of the control plane with a device's key and a coding agent's: a device said
/// hello, keys and secrets listed, a run that uses a secret (its use noted), GitLab's projects read and switched off and
/// on, costs measured elsewhere kept, permissions given, GitHub's deliveries and public repositories, AWS forgotten.
fn dashboard_calls(fault: Fault) -> Outcome {
    let served = serve(|_| {});
    served.connect_github();
    served.connect_gitlab();
    let scope = superci_core::secrets::Scope::default();
    let kept = superci_core::secrets::Stored { name: "TOKEN".into(), value: "v".into(), scope: scope.clone(), updated_ms: 1 };
    served.aws.put(&format!("/superci/{PLANE}/{}", superci_core::secrets::setting("TOKEN", &scope)), &serde_json::to_string(&kept).unwrap());
    let now = served.now_ms();
    served.store.0.lock().unwrap().insert("job:801".into(), json!({ "job_id": 801, "run_id": 1, "repo": "acme/app", "state": "done", "at_ms": now - 60_000, "ended_ms": now - 1_000, "installation_id": 42, "cloud": "cloudflare", "seen_in_progress": true }).to_string());
    served.store.0.lock().unwrap().insert("aws".into(), json!({ "account_id": "111111111111", "region": "us-east-1", "role_arn": "arn:aws:iam::111111111111:role/superci-testplane0001", "connected": true }).to_string());
    served.store.0.lock().unwrap().insert("denied:aws:ec2:RunInstances".into(), now.to_string());
    served.arm(fault);
    let key = served.key.clone();
    let said: Vec<(u16, String)> = vec![
        ask(&served, "POST", "/keys/hello", Some(&key), Some(json!({ "device": "a laptop" }))),
        ask(&served, "GET", "/keys", Some(&key), None),
        ask(&served, "GET", "/secrets", Some(&key), None),
        ask(&served, "POST", "/runs", Some(&served.run_key), Some(json!({ "workflow": "jobs:\n  t:\n    if: false\n    runs-on: superci\n    steps:\n      - run: echo ${{ secrets.TOKEN }}\n", "path": ".github/workflows/ci.yml", "repository": "acme/app", "ref": "refs/heads/main" }))),
        ask(&served, "GET", "/secrets", Some(&key), None),
        ask(&served, "GET", "/gitlab/projects", Some(&key), None),
        ask(&served, "POST", "/gitlab/projects", Some(&key), Some(json!({ "all_on": true }))),
        ask(&served, "POST", "/gitlab/projects", Some(&key), Some(json!({ "refresh": true }))),
        ask(&served, "POST", "/gitlab/projects", Some(&key), Some(json!({ "all_off": true }))),
        ask(&served, "POST", "/costs", Some(&key), Some(json!({ "measured": { "job:801": 0.25 } }))),
        ask(&served, "POST", "/permissions/given", Some(&key), Some(json!({ "cloud": "aws" }))),
        ask(&served, "GET", "/github/deliveries", Some(&key), None),
        ask(&served, "GET", "/github/public-repos", Some(&key), None),
        ask(&served, "GET", "/status", Some(&key), None),
        ask(&served, "GET", "/health", None, None),
        ask(&served, "POST", "/aws/forget", Some(&key), None),
    ];
    let out = Outcome::new(served);
    refusals_say_why(&out);
    let served = &out.served;
    // Each change answered as made is made.
    let made = |i: usize| said[i].0 == 200;
    out.check(!made(0) || served.store.under("dkey:").iter().any(|(_, v)| v.to_string().contains("a laptop")), || format!("hello answered {:?}, and the device not noted: {:?}", said[0], served.store.under("dkey:")));
    out.check(!made(3) || !served.store.under("sused:").is_empty(), || format!("a run that used a secret ({:?}), its use not noted", said[3]));
    out.check(!made(9) || record(served, "job:801").is_some_and(|j| j["cost_usd"] == 0.25), || format!("costs kept ({:?}), not on the job", said[9]));
    out.check(!made(10) || served.store.under("denied:").is_empty(), || format!("permissions given ({:?}), what was denied still kept", said[10]));
    out.check(!made(15) || served.store.under("aws").iter().all(|(k, _)| k != "aws"), || format!("AWS forgotten ({:?}), still kept", said[15]));
    if fault == Fault::None { out.check(said.iter().all(|(s, _)| *s == 200), || format!("the clean run did not go through: {said:?}")) }
    out
}

#[test]
fn what_the_dashboard_asks_holds_with_each_call_and_store_operation_failing() {
    let _one = one_at_a_time();
    sweep("what the dashboard asks", dashboard_calls);
}

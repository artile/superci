//! What GitHub's runner asks of its service, answered by the control plane for a job it handed the runner itself
//! (a run that does not go through GitHub). About ten calls, recorded from a real runner (see
//! `tests/fixtures/runner/runner-requests.jsonl`): where each service is, a session, the job, a lock it renews,
//! where each action's code is, the job's lines and step results as they come, its logs, and how it ended.
//!
//! The runner is told no secret of the service's: the address it is given (a path with a token only that machine
//! has) is what lets it in. Its session is not encrypted (the runner reads a message as it is when its session has
//! no key), and the token it asks for is one the runner only carries back.
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::io::{get_json, put_json, Request, Response, Result, Store};

/// Where each of the service's calls is, as the runner asks for it (`connectionData`): its id and its path.
const LOCATIONS: &str = include_str!("runner_locations.json");

/// A job handed to a runner, as the control plane keeps it (`rjob:<id>`).
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct HandedJob {
    /// The message the runner is handed (see `job_message`).
    pub message: Value,
    /// Whether the runner has taken it.
    #[serde(default)] pub taken: bool,
    /// How it ended, as the runner said ("succeeded", "failed", "cancelled"), and its outputs.
    #[serde(default, skip_serializing_if = "Option::is_none")] pub result: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub outputs: Option<Value>,
    /// Each step's record as the runner last said it (name, state, result), by its id.
    #[serde(default, skip_serializing_if = "Vec::is_empty")] pub steps: Vec<Value>,
    /// How many lines and logs were kept (`rline:<id>:<n>`, `rlog:<id>:<n>`).
    #[serde(default)] pub lines: u64,
    #[serde(default)] pub logs: u64,
}

/// What a job's message needs beyond the workflow file: the job's own ids, what a run was given on the command
/// line (secrets, variables), the `github` context (as plain JSON: the event, the repository, the commit), and
/// where the runner's service is for this job (an address ending in `/`) with the token it carries.
pub struct Given<'a> {
    pub workflow: &'a str,
    pub path: &'a str,
    pub job: &'a str,
    pub secrets: &'a [(String, String)],
    pub vars: &'a [(String, String)],
    pub github: Value,
    pub service: &'a str,
    pub access_token: &'a str,
    pub ids: Ids,
}

/// The ids a job's message carries (GUIDs; the runner puts some of them in the paths of its calls).
#[derive(Clone, Debug, Default)]
pub struct Ids { pub plan: String, pub scope: String, pub timeline: String, pub job: String, pub request: u64 }

impl Ids {
    pub fn new(request: u64) -> Ids { Ids { plan: guid(), scope: guid(), timeline: guid(), job: guid(), request } }
}

/// A random GUID, as the runner expects ids to look.
pub fn guid() -> String {
    let b = crate::crypto::random_bytes(16);
    let h = crate::crypto::hex(&b);
    format!("{}-{}-4{}-{}{}-{}", &h[0..8], &h[8..12], &h[13..16], ["8", "9", "a", "b"][(b[8] & 3) as usize], &h[17..20], &h[20..32])
}

/// Plain JSON as the runner's context data: text and numbers as they are, a list `{t: 1, a: […]}`, a mapping
/// `{t: 2, d: [{k, v}]}` (an empty one `{t: 2}`).
pub fn context(v: &Value) -> Value {
    match v {
        Value::Array(a) => json!({ "t": 1, "a": a.iter().map(context).collect::<Vec<_>>() }),
        Value::Object(m) if m.is_empty() => json!({ "t": 2 }),
        Value::Object(m) => json!({ "t": 2, "d": m.iter().map(|(k, v)| json!({ "k": k, "v": context(v) })).collect::<Vec<_>>() }),
        Value::Number(n) => json!(n.as_f64().unwrap_or_default()),
        other => other.clone(),
    }
}

/// The runner's switches a job's message carries, as GitHub's own service sets them for its runners (taken from a
/// recorded message: what changes how steps run, not GitHub's own telemetry).
const SWITCHES: &[&str] = &["DistributedTask.NewActionMetadata", "DistributedTask.EnableCompositeActions", "DistributedTask.EnhancedAnnotations", "DistributedTask.UploadStepSummary",
    "DistributedTask.AddWarningToNode12Action", "DistributedTask.AllowRunnerContainerHooks", "DistributedTask.DeprecateStepOutputCommands", "DistributedTask.ForceGithubJavascriptActionsToNode20",
    "DistributedTask.MarkJobAsFailedOnWorkerCrash", "actions.runner.usenode24bydefault"];

/// The whole message a runner is handed for a job (see `workflow::job_parts` for what the workflow file decides).
pub fn job_message(g: &Given) -> Result<Value> {
    let workflow = crate::workflow::parse(g.workflow)?;
    let mut m = crate::workflow::job_parts(&workflow, g.path, g.job, &mut guid)?;
    let job = workflow.get("jobs").and_then(|j| j.get(g.job));
    let display = job.and_then(|j| j.get("name")).and_then(|n| n.text()).filter(|n| !n.contains("${{")).unwrap_or(g.job).to_string();
    let mut variables = serde_json::Map::new();
    for s in SWITCHES { variables.insert(s.to_string(), json!({ "value": "true" })); }
    for (k, v) in [("system.github.job", g.job), ("system.phaseDisplayName", g.job), ("system.runnerGroupName", "Default"), ("system.runner.lowdiskspacethreshold", "100"), ("system.workflowFilePath", g.path)] {
        variables.insert(k.into(), json!({ "value": v }));
    }
    // A run's secrets: the runner makes its `secrets` context of them, and hides each wherever it would be printed.
    for (k, v) in g.secrets { variables.insert(k.clone(), json!({ "value": v, "isSecret": true })); }
    let vars: serde_json::Map<String, Value> = g.vars.iter().map(|(k, v)| (k.clone(), Value::String(v.clone()))).collect();
    m["variables"] = variables.into();
    m["mask"] = g.secrets.iter().filter(|(_, v)| !v.is_empty()).map(|(_, v)| json!({ "type": "regex", "value": regex_escape(v) })).collect();
    m["messageType"] = "PipelineAgentJobRequest".into();
    m["plan"] = json!({ "scopeIdentifier": g.ids.scope, "planType": "free", "version": 12, "planId": g.ids.plan, "planGroup": "free", "artifactUri": null, "artifactLocation": null, "definition": null, "owner": null });
    m["timeline"] = json!({ "id": g.ids.timeline, "changeId": 1, "location": null });
    m["jobId"] = g.ids.job.clone().into();
    m["jobDisplayName"] = display.into();
    m["jobName"] = g.job.into();
    m["requestId"] = g.ids.request.into();
    m["lockedUntil"] = "0001-01-01T00:00:00".into();
    let feed = format!("{}_apis/v1/TimeLineWebConsoleLog/feedstream/{}/ws", g.service.replacen("https://", "wss://", 1).replacen("http://", "ws://", 1), g.ids.timeline);
    m["resources"] = json!({ "endpoints": [{ "data": { "CacheServerUrl": g.service, "FeedStreamUrl": feed, "ResultsServiceUrl": g.service }, "name": "SystemVssConnection", "url": g.service,
        "authorization": { "parameters": { "AccessToken": g.access_token }, "scheme": "OAuth" }, "isShared": false, "isReady": true }] });
    m["contextData"] = json!({
        "github": context(&g.github), "inputs": { "t": 2 }, "vars": context(&Value::Object(vars)), "needs": { "t": 2 }, "matrix": null,
        "strategy": context(&json!({ "fail-fast": true, "job-index": 0, "job-total": 1, "max-parallel": 1 })),
    });
    m["workspace"] = json!({});
    Ok(m)
}

/// A text as a regular expression that matches only it.
fn regex_escape(s: &str) -> String {
    s.chars().map(|c| if "\\^$.|?*+()[]{}".contains(c) { format!("\\{c}") } else { c.to_string() }).collect()
}

/// A token shaped as the runner reads one (header.payload.signature); the runner carries it back and reads nothing
/// in it but its time.
pub fn access_token(now_ms: u64) -> String {
    let b = |v: Value| crate::crypto::b64url(v.to_string().as_bytes());
    let secs = now_ms / 1000;
    format!("{}.{}.{}", b(json!({ "alg": "RS256", "typ": "JWT" })), b(json!({ "nbf": secs, "iat": secs, "exp": secs + 6 * 3600, "iss": "superci", "aud": "superci" })), crate::crypto::b64url(&crate::crypto::random_bytes(32)))
}

/// The configuration a runner starts with (`run.sh --jitconfig …`), as GitHub hands one out for a just-in-time
/// runner, here pointing at this service: `root` is where its calls go (no trailing slash), `name` what it is
/// called. It runs one job and ends. Its key is made for it (the runner signs with it when it asks for a token;
/// this service does not need what it signs: the address it is given is what lets it in).
pub fn jit_config(root: &str, name: &str) -> Result<String> {
    use rsa::traits::{PrivateKeyParts, PublicKeyParts};
    let key = rsa::RsaPrivateKey::new(&mut rsa::rand_core::OsRng, 2048).map_err(|e| e.to_string())?;
    let n = |b: &rsa::BigUint| crate::crypto::b64(&b.to_bytes_be());
    let primes = key.primes();
    let (p, q) = (&primes[0], &primes[1]);
    let one = rsa::BigUint::from(1u8);
    let (dp, dq) = (key.d() % (p - &one), key.d() % (q - &one));
    let inverse_q = key.crt_coefficient().ok_or("no CRT coefficient")?;
    let rsaparams = json!({ "d": n(key.d()), "dp": n(&dp), "dq": n(&dq), "exponent": n(key.e()), "inverseQ": n(&inverse_q), "modulus": n(key.n()), "p": n(p), "q": n(q) });
    let runner = json!({ "AgentId": "1", "AgentName": name, "DisableUpdate": "True", "Ephemeral": "True", "PoolId": "1", "PoolName": "Default", "ServerUrl": root, "WorkFolder": "_work", "GitHubUrl": root });
    let credentials = json!({ "Scheme": "OAuth", "Data": { "ClientId": guid(), "AuthorizationUrl": format!("{root}/_apis/v1/auth/"), "RequireFipsCryptography": "False" } });
    let b = |v: &Value| crate::crypto::b64(v.to_string().as_bytes());
    Ok(b(&json!({ ".runner": b(&runner), ".credentials": b(&credentials), ".credentials_rsaparams": b(&rsaparams) })))
}

fn ok(body: Value) -> Response { Response::json(&body) }
fn empty() -> Response { Response::new(200, "application/json", "") }

/// Answers one of the runner's calls for the job kept at `rjob:<id>`. `path`: the call's path after the job's own
/// prefix. `hold`: waits a moment in place (a runner asking for a message when there is none is held, then told so).
pub async fn answer(store: &dyn Store, id: &str, req: &Request, path: &str, hold: &dyn Fn(u64)) -> Result<Response> {
    let key = format!("rjob:{id}");
    let Some(mut job) = get_json::<HandedJob>(store, &key).await? else { return Ok(Response::text(404, "no such job")) };
    // (The runner starts some bodies with a byte-order mark, as .NET writes UTF-8.)
    let raw = req.body.strip_prefix("\u{feff}".as_bytes()).unwrap_or(&req.body);
    let body: Value = serde_json::from_slice(raw).unwrap_or(Value::Null);
    let p = path.split('?').next().unwrap_or_default();
    let m = req.method.as_str();
    Ok(if p.ends_with("/connectionData") {
        Response::new(200, "application/json", LOCATIONS)
    } else if p.contains("/auth/") {
        ok(json!({ "access_token": job.message["resources"]["endpoints"][0]["authorization"]["parameters"]["AccessToken"], "expires_in": 21600, "scope": "/", "token_type": "access_token" }))
    } else if p.contains("/AgentSession/") {
        if m == "DELETE" { return Ok(empty()) }
        // (No `encryptionKey`: the runner reads its messages as they are.)
        let mut s = body; s["sessionId"] = guid().into(); s.as_object_mut().map(|o| o.remove("encryptionKey")); ok(s)
    } else if p.contains("/Message/") {
        if m == "DELETE" { return Ok(empty()) }
        if !job.taken {
            job.taken = true;
            put_json(store, &key, &job).await?;
            return Ok(ok(json!({ "messageId": 1, "messageType": "PipelineAgentJobRequest", "body": job.message.to_string() })))
        }
        hold(20_000);
        Response::new(204, "application/json", "")
    } else if p.contains("/AgentRequest/") {
        let mut r = body; r["lockedUntil"] = "2099-01-01T00:00:00Z".into(); ok(r)
    } else if p.contains("/feedstream/") {
        Response::new(400, "text/plain", "")
    } else if p.contains("/ActionDownloadInfo/") {
        // Where each action's code is: GitHub's public tarball of it at the version named.
        let mut out = serde_json::Map::new();
        for a in body["actions"].as_array().into_iter().flatten() {
            let (name, r) = (a["nameWithOwner"].as_str().unwrap_or_default(), a["ref"].as_str().unwrap_or_default());
            out.insert(format!("{name}@{r}"), json!({ "nameWithOwner": name, "resolvedNameWithOwner": name, "resolvedSha": r, "ref": r,
                "tarballUrl": format!("https://api.github.com/repos/{name}/tarball/{r}"), "zipballUrl": format!("https://api.github.com/repos/{name}/zipball/{r}") }));
        }
        ok(json!({ "actions": out }))
    } else if p.contains("/TimeLineWebConsoleLog/") {
        // The job's lines as they are written: kept in order, each post as one entry.
        let lines: Vec<String> = body["value"].as_array().into_iter().flatten().filter_map(|l| l.as_str().map(str::to_string)).collect();
        if !lines.is_empty() {
            job.lines += 1;
            put_json(store, &format!("rline:{id}:{:06}", job.lines), &lines).await?;
            put_json(store, &key, &job).await?;
        }
        empty()
    } else if p.contains("/Timeline/") {
        // Each step's record: the last word of each kept.
        for r in body["value"].as_array().into_iter().flatten() {
            let slim = json!({ "id": r["id"], "type": r["type"], "name": r["name"], "state": r["state"], "result": r["result"], "order": r["order"] });
            match job.steps.iter_mut().find(|s| s["id"] == r["id"]) { Some(s) => *s = slim, None => job.steps.push(slim) }
        }
        put_json(store, &key, &job).await?;
        ok(body)
    } else if p.contains("/Logfiles/") {
        if p.rsplit('/').next().is_some_and(|last| last.parse::<u64>().is_ok()) {
            let n = p.rsplit('/').next().unwrap_or_default();
            store.put(&format!("rlog:{id}:{n}"), String::from_utf8_lossy(&req.body).chars().take(200_000).collect()).await?;
            return Ok(empty())
        }
        job.logs += 1;
        put_json(store, &key, &job).await?;
        let mut l = body; l["id"] = job.logs.into(); ok(l)
    } else if p.contains("/FinishJob/") {
        // (A body cut short still says how it ended.)
        let said = String::from_utf8_lossy(&req.body);
        job.result = body["result"].as_str().map(str::to_string).or_else(|| said.split("\"result\":\"").nth(1).and_then(|r| r.split('"').next()).map(str::to_string));
        job.outputs = body.get("outputs").cloned().filter(|o| !o.is_null());
        put_json(store, &key, &job).await?;
        empty()
    } else {
        Response::text(404, "not a call of the runner's")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_data_is_written_as_the_runner_reads_it() {
        assert_eq!(context(&json!({ "a": "x", "n": 3, "l": [true, null], "e": {} })), json!({ "t": 2, "d": [{ "k": "a", "v": "x" }, { "k": "e", "v": { "t": 2 } }, { "k": "l", "v": { "t": 1, "a": [true, null] } }, { "k": "n", "v": 3.0 }] }));
        assert_eq!(regex_escape("a.b(c)*"), "a\\.b\\(c\\)\\*");
        let g = guid();
        assert!(g.len() == 36 && g.as_bytes()[14] == b'4', "{g}");
        assert_eq!(access_token(1_790_000_000_000).split('.').count(), 3);
    }
}

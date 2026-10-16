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
    /// The working tree the run was started with, as uploaded (`rtree:<tree>:…`, in `parts`), for its checkout.
    #[serde(default, skip_serializing_if = "Option::is_none")] pub tree: Option<String>,
    #[serde(default)] pub parts: u32,
    /// Whether its parts are archives one on another (a base kept from before, then what changed since, with the
    /// paths removed since in `.superci-removed`), not pieces of one.
    #[serde(default)] pub layered: bool,
    /// When it was handed out (ms): what is kept of it goes after a day.
    #[serde(default)] pub at_ms: u64,
    /// The control plane's record of the job (`job:<id>`) and its runner's name.
    #[serde(default)] pub job_id: u64,
    #[serde(default, skip_serializing_if = "String::is_empty")] pub runner: String,
    /// Asked to stop (its run was cancelled), and whether its runner has been told: GitHub's message for it, once.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")] pub cancel: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")] pub cancel_told: bool,
}

/// How long a cancelled job's steps have to stop (as GitHub gives them), before its runner ends them.
pub const CANCEL_GRACE: &str = "00:05:00";

/// How long what a run's job left (its lines, its logs) is kept.
pub const KEPT_MS: u64 = 2 * 86_400_000;
/// A piece of a log as the store keeps it (DynamoDB takes 400 KB a record; this, as base64, stays under).
pub const LOG_PIECE: usize = 280_000;

/// What a post of lines was kept as (`rline:<id>:<n>`): its step, the number of its first line in that step's log,
/// and its lines. (Kept as the lines alone before 0.19.0.)
pub fn feed(v: &str) -> (Option<String>, Option<u64>, Vec<String>) {
    match serde_json::from_str::<Value>(v).unwrap_or_default() {
        Value::Array(lines) => (None, None, lines.into_iter().filter_map(|l| l.as_str().map(str::to_string)).collect()),
        o => (o["step"].as_str().map(str::to_string), o["start"].as_u64(), o["lines"].as_array().into_iter().flatten().filter_map(|l| l.as_str().map(str::to_string)).collect()),
    }
}

/// How long a runner asking for a job when there is none is held before it is told so (it asks again at once):
/// said with the answer, in `x-superci-hold`, for whoever answers to wait that long first.
pub const HOLD_MS: u64 = 20_000;

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
    /// This job's place in its matrix (its combination, or null), its strategy (`job-index`, `job-total`), what it
    /// needs (`{name: {result, outputs}}`), and its name as shown when a matrix or an expression makes it.
    pub matrix: Value,
    pub strategy: Value,
    pub needs: Value,
    pub display: Option<String>,
    /// A called workflow's inputs (`{name: value}`; null for a job of the workflow run itself).
    pub inputs: Value,
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
    let display = g.display.clone().unwrap_or_else(|| job.and_then(|j| j.get("name")).and_then(|n| n.text()).filter(|n| !n.contains("${{")).unwrap_or(g.job).to_string());
    let mut variables = serde_json::Map::new();
    for s in SWITCHES { variables.insert(s.to_string(), json!({ "value": "true" })); }
    for (k, v) in [("system.github.job", g.job), ("system.phaseDisplayName", g.job), ("system.runnerGroupName", "Default"), ("system.runner.lowdiskspacethreshold", "100"), ("system.workflowFilePath", g.path)] {
        variables.insert(k.into(), json!({ "value": v }));
    }
    // A run's secrets: the runner makes its `secrets` context of them, and hides each wherever it would be printed.
    for (k, v) in g.secrets { variables.insert(k.clone(), json!({ "value": v, "isSecret": true })); }
    // GitHub's cache as GitHub's jobs have it now (version 2: `ACTIONS_CACHE_SERVICE_V2` for the job's actions).
    variables.insert("actions_uses_cache_service_v2".into(), json!({ "value": "true" }));
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
    let needs = if g.needs.is_object() { g.needs.clone() } else { json!({}) };
    let strategy = if g.strategy.is_object() { g.strategy.clone() } else { json!({ "fail-fast": true, "job-index": 0, "job-total": 1, "max-parallel": 1 }) };
    m["contextData"] = json!({
        "github": context(&g.github), "inputs": if g.inputs.is_object() { context(&g.inputs) } else { json!({ "t": 2 }) }, "vars": context(&Value::Object(vars)),
        "needs": context(&needs), "matrix": if g.matrix.is_null() { Value::Null } else { context(&g.matrix) }, "strategy": context(&strategy),
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

/// What runs in place of `actions/checkout` for a run from a computer: the working tree the run was started with,
/// fetched in its parts from `tree` (an address this job's machine alone has), unpacked where the checkout would
/// have put the repository, and made one commit, as a checkout of one commit leaves it. It takes every input
/// `actions/checkout` takes (so none is said to be unexpected); another repository than the run's own is refused.
pub fn checkout_action(tree: &str, parts: u32, layered: bool) -> String {
    const INPUTS: [&str; 20] = ["repository", "ref", "token", "ssh-key", "ssh-known-hosts", "ssh-strict", "ssh-user", "persist-credentials", "path", "clean", "filter", "sparse-checkout",
        "sparse-checkout-cone-mode", "fetch-depth", "fetch-tags", "show-progress", "lfs", "submodules", "set-safe-directory", "github-server-url"];
    let inputs: String = INPUTS.iter().map(|i| format!("  {i}:\n    required: false\n    default: ''\n")).collect();
    // Each part into a file of its own, all at once; a connection that stalls (nothing for 10 s) is dropped and the
    // part asked for again from its start (the control plane gives a fresh link each time), five times at most. A part
    // still on its way from the computer (425: the run was started while it was sent) is waited for, ten minutes at
    // most. How long each took is said: the control plane's answer, then the bucket's.
    let get = format!(r#"t=$(mktemp -d "${{RUNNER_TEMP:-/tmp}}/superci-tree.XXXXXX"); began=$SECONDS
        fetch() {{ local tries=0 waited=0 rc said code size total asked; while :; do rc=0; said=$(curl -q -sSL --connect-timeout 10 --speed-limit 20000 --speed-time 10 -o "$t/$1" -w "%{{http_code}} %{{size_download}} %{{time_total}} %{{time_redirect}}" "{tree}?part=$1") || rc=$?; read -r code size total asked <<< "$said"; [ $rc = 0 ] || code="curl error $rc"; case "$code" in 200) echo "Part $1: $size bytes in $total s ($asked s of it the control plane's answer)"; return 0;; 425) [ $waited = 0 ] && echo "Part $1: waiting for the computer to finish sending it"; waited=$((waited + 1)); [ $waited -ge 600 ] && {{ echo "::error::The computer did not finish sending the working tree in ten minutes."; return 1; }}; sleep 1; continue;; esac; tries=$((tries + 1)); [ $tries -ge 5 ] && {{ echo "::error::Part $1 of the working tree could not be fetched (last: $code)."; return 1; }}; echo "Part $1: asked again ($tries; $code)"; sleep 1; done; }}
        pids=(); for part in $(seq 0 {last}); do fetch "$part" & pids+=($!); done; for p in "${{pids[@]}}"; do wait "$p"; done; fetched=$SECONDS"#, last = parts.saturating_sub(1));
    let unpack = if layered {
        // Each archive on the one before, and what was removed by it, in turn.
        format!("for part in $(seq 0 {}); do tar -xz --ignore-zeros -f \"$t/$part\"; if [ -f .superci-removed ]; then tr '\\n' '\\0' < .superci-removed | xargs -0 -r rm -f --; rm -f .superci-removed; fi; done", parts.saturating_sub(1))
    } else {
        format!("for part in $(seq 0 {}); do cat \"$t/$part\"; done | tar -xz", parts.saturating_sub(1))
    };
    let fetch = if parts == 0 { "true".to_string() } else {
        format!("{get}\n        {unpack}; rm -rf \"$t\"\n        echo \"The working tree: fetched in $((fetched - began)) s, unpacked in $((SECONDS - fetched)) s\"")
    };
    format!(r#"name: Check out (SuperCI)
description: The working tree a run from a computer was started with, in place of a checkout from GitHub.
inputs:
{inputs}runs:
  using: composite
  steps:
    - shell: bash
      env:
        SUPERCI_PATH: ${{{{ inputs.path }}}}
        SUPERCI_REPOSITORY: ${{{{ inputs.repository }}}}
      run: |
        set -Eeuo pipefail
        trap 'echo "::error::The checkout stopped at its line $LINENO ($BASH_COMMAND), with status $?."' ERR
        if [ -n "$SUPERCI_REPOSITORY" ] && [ "$SUPERCI_REPOSITORY" != "$GITHUB_REPOSITORY" ]; then echo "::error::A run from a computer has its own working tree only; $SUPERCI_REPOSITORY is not checked out here."; exit 1; fi
        dir="$GITHUB_WORKSPACE/$SUPERCI_PATH"; mkdir -p "$dir"; cd "$dir"
        {fetch}
        git init -q . && git add -A && git -c user.name=superci -c user.email=superci@users.noreply.github.com commit -q --no-verify --allow-empty -m "The working tree of a run from a computer"
        echo "The working tree as it was on the computer: $(git ls-files | wc -l | tr -d ' ') files"
"#)
}

/// The checkout above as GitHub serves an action: a gzipped tar with the action in one folder.
pub fn checkout_tarball(tree: &str, parts: u32, layered: bool) -> Vec<u8> { gzip_stored(&tar(&[("checkout/action.yml", checkout_action(tree, parts, layered).as_bytes())])) }

/// A tar of these files (each in a folder the runner takes off as it unpacks).
fn tar(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut out = vec![];
    let header = |name: &str, size: usize, kind: u8, mode: &str| {
        let mut h = [0u8; 512];
        h[..name.len()].copy_from_slice(name.as_bytes());
        let octal = |h: &mut [u8; 512], at: usize, width: usize, n: u64| { let s = format!("{n:0w$o}", w = width - 1); h[at..at + width - 1].copy_from_slice(s.as_bytes()) };
        h[100..100 + mode.len()].copy_from_slice(mode.as_bytes());
        octal(&mut h, 108, 8, 0); octal(&mut h, 116, 8, 0); octal(&mut h, 124, 12, size as u64); octal(&mut h, 136, 12, 0);
        h[156] = kind;
        h[257..263].copy_from_slice(b"ustar\0"); h[263..265].copy_from_slice(b"00");
        h[148..156].copy_from_slice(b"        ");
        let sum: u32 = h.iter().map(|b| *b as u32).sum();
        let s = format!("{sum:06o}\0 ");
        h[148..156].copy_from_slice(s.as_bytes());
        h
    };
    let folder = files.first().and_then(|(n, _)| n.split_once('/')).map(|(f, _)| format!("{f}/")).unwrap_or_default();
    if !folder.is_empty() { out.extend_from_slice(&header(&folder, 0, b'5', "0000755")) }
    for (name, data) in files {
        out.extend_from_slice(&header(name, data.len(), b'0', "0000644"));
        out.extend_from_slice(data);
        out.resize(out.len().div_ceil(512) * 512, 0);
    }
    out.resize(out.len() + 1024, 0);
    out
}

/// Gzip of these bytes, not compressed (stored blocks): what an action of a few kilobytes needs.
fn gzip_stored(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 255];
    let chunks: Vec<&[u8]> = if data.is_empty() { vec![&[][..]] } else { data.chunks(65_535).collect() };
    for (i, c) in chunks.iter().enumerate() {
        out.push(if i + 1 == chunks.len() { 1 } else { 0 });
        let len = c.len() as u16;
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&(!len).to_le_bytes());
        out.extend_from_slice(c);
    }
    out.extend_from_slice(&crc32(data).to_le_bytes());
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for b in data {
        crc ^= *b as u32;
        for _ in 0..8 { crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 } }
    }
    !crc
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
    // (Its work folder where GitHub's own runners have theirs, as SuperCI's machines expect.)
    let runner = json!({ "AgentId": "1", "AgentName": name, "DisableUpdate": "True", "Ephemeral": "True", "PoolId": "1", "PoolName": "Default", "ServerUrl": root, "WorkFolder": "/home/runner/work", "GitHubUrl": root });
    let credentials = json!({ "Scheme": "OAuth", "Data": { "ClientId": guid(), "AuthorizationUrl": format!("{root}/_apis/v1/auth/"), "RequireFipsCryptography": "False" } });
    let b = |v: &Value| crate::crypto::b64(v.to_string().as_bytes());
    Ok(b(&json!({ ".runner": b(&runner), ".credentials": b(&credentials), ".credentials_rsaparams": b(&rsaparams) })))
}

fn ok(body: Value) -> Response { Response::json(&body) }
fn empty() -> Response { Response::new(200, "application/json", "") }

/// Answers one of the runner's calls for the job kept at `rjob:<id>`. `path`: the call's path after the job's own
/// prefix. A runner asking for a message when there is none is told so with `x-superci-hold` (see `HOLD_MS`).
pub async fn answer(store: &dyn Store, id: &str, req: &Request, path: &str) -> Result<Response> {
    let key = format!("rjob:{id}");
    let Some(mut job) = get_json::<HandedJob>(store, &key).await? else { return Ok(Response::text(404, "no such job")) };
    // (The runner starts some bodies with a byte-order mark, as .NET writes UTF-8.)
    let raw = req.body.strip_prefix("\u{feff}".as_bytes()).unwrap_or(&req.body);
    let body: Value = serde_json::from_slice(raw).unwrap_or(Value::Null);
    let p = path.split('?').next().unwrap_or_default();
    let m = req.method.as_str();
    // GitHub's results service, reached here only from a machine without SuperCI's cache beside its runner (Modal's,
    // Cloudflare's; on AWS `superci-cache` answers it): its cache has no entry and takes none, said as GitHub's own
    // service says it (a miss, at once: an answer that is not JSON is asked again five times, each way); artifacts are
    // refused, saying why.
    if let Some(call) = p.split_once("/twirp/github.actions.results.api.v1.").map(|(_, c)| c) {
        return Ok(match call.split_once('/') {
            Some(("CacheService", "GetCacheEntryDownloadURL" | "CreateCacheEntry" | "FinalizeCacheEntryUpload")) => ok(json!({ "ok": false })),
            Some(("CacheService", _)) => Response::json(&json!({ "code": "not_found", "msg": "no cache entries for this job's machine" })).with_status(404),
            _ => Response::json(&json!({ "code": "unimplemented", "msg": "artifacts of a run from a computer are kept from SuperCI's AWS machines only: this job ran on another provider's" })).with_status(501),
        })
    }
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
        // Its run cancelled: the runner is told as GitHub tells it, and it stops the job's steps (the job ends cancelled).
        if job.cancel && !job.cancel_told {
            job.cancel_told = true;
            put_json(store, &key, &job).await?;
            return Ok(ok(json!({ "messageId": 2, "messageType": "JobCancellation", "body": json!({ "jobId": job.message["jobId"], "timeout": CANCEL_GRACE }).to_string() })))
        }
        Response::new(204, "application/json", "").with_header("x-superci-hold", &HOLD_MS.to_string())
    } else if p.contains("/AgentRequest/") {
        let mut r = body; r["lockedUntil"] = "2099-01-01T00:00:00Z".into(); ok(r)
    } else if p.contains("/feedstream/") {
        Response::new(400, "text/plain", "")
    } else if p.contains("/ActionDownloadInfo/") {
        // Where each action's code is: GitHub's public tarball of it at the version named; `actions/checkout`
        // is this service's own, which puts the run's working tree in place (see `checkout_tarball`).
        let service = job.message["resources"]["endpoints"][0]["url"].as_str().unwrap_or_default().to_string();
        let mut out = serde_json::Map::new();
        for a in body["actions"].as_array().into_iter().flatten() {
            let (name, r) = (a["nameWithOwner"].as_str().unwrap_or_default(), a["ref"].as_str().unwrap_or_default());
            let tarball = if name.eq_ignore_ascii_case("actions/checkout") { format!("{service}superci/checkout.tar.gz") } else { format!("https://api.github.com/repos/{name}/tarball/{r}") };
            out.insert(format!("{name}@{r}"), json!({ "nameWithOwner": name, "resolvedNameWithOwner": name, "resolvedSha": r, "ref": r,
                "tarballUrl": tarball, "zipballUrl": format!("https://api.github.com/repos/{name}/zipball/{r}") }));
        }
        ok(json!({ "actions": out }))
    } else if p.contains("/TimeLineWebConsoleLog/") {
        // The job's lines as they are written, each post as one entry, with its step and the number of its first line
        // in that step's log: the runner drops lines when it writes faster than it sends them (it holds 1,024 at
        // most), so a gap is seen, and the step's whole log (`/Logfiles/`) fills it.
        let lines: Vec<String> = body["value"].as_array().into_iter().flatten().filter_map(|l| l.as_str().map(str::to_string)).collect();
        if !lines.is_empty() {
            job.lines += 1;
            let step = body["stepId"].as_str().map(str::to_string).or_else(|| p.trim_end_matches('/').rsplit('/').next().map(str::to_string));
            store.put_until(&format!("rline:{id}:{:06}", job.lines), json!({ "step": step, "start": body["startLine"], "lines": lines }).to_string(), job.at_ms + KEPT_MS).await?;
            put_json(store, &key, &job).await?;
        }
        empty()
    } else if p.contains("/Timeline/") {
        // Each step's record: the last word of each kept.
        for r in body["value"].as_array().into_iter().flatten() {
            let log = r["log"]["id"].clone();
            let mut slim = json!({ "id": r["id"], "type": r["type"], "name": r["name"], "state": r["state"], "result": r["result"], "order": r["order"], "log": log, "start": r["startTime"], "finish": r["finishTime"] });
            match job.steps.iter_mut().find(|s| s["id"] == r["id"]) {
                // (A later word that leaves something out keeps what was said of it before.)
                Some(s) => { for (k, v) in slim.as_object_mut().into_iter().flatten() { if v.is_null() { *v = s[k.as_str()].clone() } } *s = slim }
                None => job.steps.push(slim),
            }
        }
        put_json(store, &key, &job).await?;
        ok(body)
    } else if p.contains("/Logfiles/") {
        if let Some(n) = p.rsplit('/').next().and_then(|last| last.parse::<u64>().ok()) {
            // A log's content, whole (each upload after the first adds to it), in pieces the store takes. (Where the
            // control plane has a bucket, it keeps them there instead, see `ControlPlane::for_runner`.)
            let seq = store.list(&format!("rlog:{id}:{n:04}:")).await?.len();
            for (i, piece) in req.body.chunks(LOG_PIECE).enumerate() {
                store.put_until(&format!("rlog:{id}:{n:04}:{:04}", seq + i), crate::crypto::b64(piece), job.at_ms + KEPT_MS).await?;
            }
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
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn the_checkout_is_an_action_the_runner_can_unpack() {
        let gz = checkout_tarball("https://plane.example/runner/abc/superci/tree", 3, false);
        let layered = checkout_action("https://plane.example/runner/abc/superci/tree", 2, true);
        assert!(layered.contains(r#"for part in $(seq 0 1); do tar -xz --ignore-zeros -f "$t/$part"; if [ -f .superci-removed ]"#) && layered.contains("xargs -0 -r rm -f --") && layered.contains(r"tr '\n' '\0' < .superci-removed"), "{layered}");
        assert_eq!(&gz[..3], &[0x1f, 0x8b, 8]);
        // (Unpacked by the system's own tar, as the runner does.)
        let dir = std::env::temp_dir().join(format!("superci-checkout-{}", guid()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.tar.gz"), &gz).unwrap();
        let ok = std::process::Command::new("tar").args(["xzf", "a.tar.gz", "--strip-components", "1"]).current_dir(&dir).status().unwrap().success();
        let action = std::fs::read_to_string(dir.join("action.yml")).unwrap_or_default();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(ok && action.contains("using: composite") && action.contains("for part in $(seq 0 2); do fetch \"$part\" &") && action.contains("  fetch-depth:\n    required: false"), "{action}");
        assert!(action.contains("SUPERCI_PATH: ${{ inputs.path }}"));
    }

    /// A layered checkout run by bash: a base, then each run's changes in turn, each with what it removed; a file
    /// removed by one layer and back in the next is there, as on the computer.
    #[test]
    fn layers_are_laid_down_in_turn_each_with_what_it_removed() {
        use std::io::{Read, Write};
        let dir = std::env::temp_dir().join(format!("superci-layers-{}", guid()));
        let (work, tmp) = (dir.join("work"), dir.join("tmp"));
        for d in [&work, &tmp] { std::fs::create_dir_all(d).unwrap() }
        // An archive of these files (and a list of removed paths after it, as the computer packs a layer).
        let pack = |files: &[(&str, &str)], removed: &[&str]| {
            let src = dir.join(format!("src-{}", guid()));
            std::fs::create_dir_all(&src).unwrap();
            for (p, t) in files { std::fs::write(src.join(p), t).unwrap() }
            let mut names: Vec<String> = files.iter().map(|(p, _)| p.to_string()).collect();
            if !removed.is_empty() { std::fs::write(src.join(".superci-removed"), removed.join("\n") + "\n").unwrap(); names.push(".superci-removed".into()) }
            let raw = std::process::Command::new("tar").arg("-cf").arg("-").args(&names).current_dir(&src).output().unwrap().stdout;
            let mut gz = std::process::Command::new("gzip").arg("-c").stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).spawn().unwrap();
            gz.stdin.take().unwrap().write_all(&raw).unwrap();
            gz.wait_with_output().unwrap().stdout
        };
        let parts = vec![pack(&[("a.txt", "one\n"), ("gone.txt", "first\n")], &[]), pack(&[("a.txt", "two\n")], &["gone.txt"]), pack(&[("gone.txt", "back\n")], &[])];
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let served = parts.clone();
        std::thread::spawn(move || for stream in listener.incoming() {
            let (mut stream, parts) = (stream.unwrap(), served.clone());
            std::thread::spawn(move || {
                let mut asked = vec![0u8; 4096];
                let n = stream.read(&mut asked).unwrap_or(0);
                let path = String::from_utf8_lossy(&asked[..n]).lines().next().unwrap_or_default().split(' ').nth(1).unwrap_or_default().to_string();
                let body = path.strip_prefix("/tree?part=").and_then(|p| p.parse::<usize>().ok()).and_then(|p| parts.get(p).cloned()).unwrap_or_default();
                let _ = write!(stream, "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n", body.len());
                let _ = stream.write_all(&body);
            });
        });
        let action = checkout_action(&format!("http://127.0.0.1:{port}/tree"), parts.len() as u32, true);
        let script: String = action.split("run: |\n").nth(1).unwrap().lines().map(|l| l.strip_prefix("        ").unwrap_or(l)).collect::<Vec<_>>().join("\n");
        let out = std::process::Command::new("bash").args(["-c", &script]).env("GITHUB_WORKSPACE", &work).env("RUNNER_TEMP", &tmp).env("SUPERCI_PATH", "").env("SUPERCI_REPOSITORY", "").env("GITHUB_REPOSITORY", "acme/app").output().unwrap();
        let said = String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
        let read = |p: &str| std::fs::read_to_string(work.join(p)).ok();
        let (a, gone, marker) = (read("a.txt"), read("gone.txt"), work.join(".superci-removed").exists());
        let _ = std::fs::remove_dir_all(&dir);
        assert!(out.status.success(), "{said}");
        assert_eq!((a.as_deref(), gone.as_deref(), marker), (Some("two\n"), Some("back\n"), false), "{said}");
    }

    /// The checkout's script run by bash, as the runner runs it, against a control plane that sends each part on to
    /// the bucket and a bucket that drops the first answer halfway: that part is asked for again from its start, one the
    /// computer is still sending is waited for, and the tree is unpacked whole.
    #[test]
    fn the_checkout_asks_again_for_a_part_cut_off_and_unpacks_the_tree_whole() {
        use std::io::{Read, Write};
        let dir = std::env::temp_dir().join(format!("superci-fetch-{}", guid()));
        let (src, work, tmp) = (dir.join("src"), dir.join("work"), dir.join("tmp"));
        for d in [&src, &work, &tmp] { std::fs::create_dir_all(d).unwrap() }
        for n in 0..40 { std::fs::write(src.join(format!("file-{n}.txt")), format!("{n} ").repeat(2000)).unwrap() }
        let packed = std::process::Command::new("tar").args(["-czf", "-", "."]).current_dir(&src).output().unwrap().stdout;
        let pieces: Vec<Vec<u8>> = packed.chunks(packed.len().div_ceil(3)).map(<[u8]>::to_vec).collect();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let cut = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let early = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let served = pieces.clone();
        std::thread::spawn(move || for stream in listener.incoming() {
            let (mut stream, pieces, cut, early) = (stream.unwrap(), served.clone(), cut.clone(), early.clone());
            std::thread::spawn(move || {
                let mut asked = vec![0u8; 4096];
                let n = stream.read(&mut asked).unwrap_or(0);
                let line = String::from_utf8_lossy(&asked[..n]).lines().next().unwrap_or_default().to_string();
                let path = line.split(' ').nth(1).unwrap_or_default().to_string();
                // (The last part is still on its way from the computer, the first two times it is asked for.)
                if path == "/tree?part=2" && early.fetch_add(1, std::sync::atomic::Ordering::SeqCst) < 2 {
                    let _ = write!(stream, "HTTP/1.1 425 Too Early\r\ncontent-length: 5\r\nconnection: close\r\n\r\nlater");
                } else if let Some(part) = path.strip_prefix("/tree?part=") {
                    let _ = write!(stream, "HTTP/1.1 302 Found\r\nlocation: http://127.0.0.1:{port}/bucket/{part}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n");
                } else if let Some(part) = path.strip_prefix("/bucket/").and_then(|p| p.parse::<usize>().ok()) {
                    let body = &pieces[part];
                    let _ = write!(stream, "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n", body.len());
                    // (The first answer for the second part ends halfway.)
                    let whole = !(part == 1 && !cut.swap(true, std::sync::atomic::Ordering::SeqCst));
                    let _ = stream.write_all(if whole { body } else { &body[..body.len() / 2] });
                }
            });
        });
        let action = checkout_action(&format!("http://127.0.0.1:{port}/tree"), pieces.len() as u32, false);
        let script: String = action.split("run: |\n").nth(1).unwrap().lines().map(|l| l.strip_prefix("        ").unwrap_or(l)).collect::<Vec<_>>().join("\n");
        let out = std::process::Command::new("bash").args(["-c", &script]).env("GITHUB_WORKSPACE", &work).env("RUNNER_TEMP", &tmp).env("SUPERCI_PATH", "").env("SUPERCI_REPOSITORY", "").env("GITHUB_REPOSITORY", "acme/app").output().unwrap();
        let said = String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
        let whole = (0..40).all(|n| std::fs::read_to_string(work.join(format!("file-{n}.txt"))).is_ok_and(|t| t == format!("{n} ").repeat(2000)));
        let left = std::fs::read_dir(&tmp).map(|d| d.count()).unwrap_or(0);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(out.status.success() && whole, "{:?}, whole: {whole}\n{said}", out.status);
        assert!(!said.contains("The checkout stopped"), "{said}");
        assert!(said.contains("Part 1: asked again (1; curl error 18)") && said.contains("Part 2: waiting for the computer to finish sending it") && !said.contains("Part 2: asked again") && said.contains("Part 0: ") && said.contains("The working tree: fetched in ") && said.contains("The working tree as it was on the computer: 40 files"), "{said}");
        assert_eq!(left, 0, "the parts are gone once unpacked");
    }
}

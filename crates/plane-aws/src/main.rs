//! A SuperCI control plane on AWS Lambda (provided.al2023, arm64): GitHub's webhooks arrive through the function's
//! URL; state is one DynamoDB table; secrets (the GitHub App, runner agents, dashboard keys) are SecureString parameters
//! under /superci/<id>/ in SSM Parameter Store, written by the dashboard; a schedule calls the sweep every minute,
//! and the function calls itself for a sweep sooner than that (see `Scheduled`). Machines in this account are
//! started with the function's own role, so AWS needs no OpenID Connect here.
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use futures::executor::block_on;
use serde_json::{json, Value};

use superci_core::aws::{self, Credentials};
use superci_core::io::{self, Clock, Http, Request, Response, Store, Timer};
use superci_core::plane::{Agent, Cache, Config, ControlPlane};

fn now_ms() -> u64 { SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0) }

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder().http_status_as_error(false).timeout_global(Some(Duration::from_secs(25))).build().into()
}

struct Blocking(ureq::Agent);

#[async_trait(?Send)]
impl Http for Blocking {
    async fn send(&self, r: Request) -> io::Result<Response> {
        let mut builder = ureq::http::Request::builder().method(r.method.as_str()).uri(r.url.as_str());
        for (k, v) in &r.headers { builder = builder.header(k.as_str(), v.as_str()) }
        let request = builder.body(r.body.clone()).map_err(|e| e.to_string())?;
        let mut response = self.0.run(request).map_err(|e| format!("{} {}: {e}", r.method, r.url))?;
        let headers = response.headers().iter().filter_map(|(k, v)| Some((k.as_str().to_string(), v.to_str().ok()?.to_string()))).collect();
        Ok(Response { status: response.status().as_u16(), headers, body: response.body_mut().read_to_vec().map_err(|e| e.to_string())? })
    }
}

/// The function's own credentials (from its role, in the environment).
fn own_credentials() -> Credentials {
    let var = |n: &str| std::env::var(n).unwrap_or_default();
    Credentials { access_key_id: var("AWS_ACCESS_KEY_ID"), secret_access_key: var("AWS_SECRET_ACCESS_KEY"), session_token: std::env::var("AWS_SESSION_TOKEN").ok(), expires_at_ms: u64::MAX }
}

/// What DynamoDB says when a call may be made again: too much at once, or a passing fault of its own.
const AGAIN: [&str; 6] = ["ThrottlingException", "ProvisionedThroughputExceededException", "RequestLimitExceeded", "InternalServerError", "ServiceUnavailable", "HTTP 5"];

/// State in one DynamoDB table: each kind of record in a partition of its own (`p`: the key up to its first colon,
/// `job` for `job:1717`), the key itself as the sort key (`k`), the record as text (`v`), and its end in seconds (`x`,
/// the table's time to live) when it has one. Before 0.19.0 every record was in the one partition `s`: until those
/// are moved (`migrate`, a little at each sweep; then `layout` says so), what is not found in its own is looked for
/// there too.
struct Dynamo<'a> { http: &'a Blocking, table: String, region: String, old: std::cell::Cell<Option<bool>> }

/// The partition a key is kept in: its kind.
fn part(key: &str) -> &str { key.split(':').next().unwrap_or(key) }

/// Where the layout is said to be this one (once nothing is left in the old partition).
const LAYOUT: &str = "layout";

impl Dynamo<'_> {
    /// One call, asked again when DynamoDB says to (throttled, or a passing error of its own), as AWS's own SDKs do:
    /// up to ten times, each wait twice the last at most (from 50 ms to 5 s), a random part of it.
    async fn call(&self, op: &str, body: Value) -> io::Result<Value> {
        let mut most_ms = 50u64;
        for _ in 0..9 {
            match aws::json_call(self.http, "POST", &format!("https://dynamodb.{}.amazonaws.com/", self.region), &self.region, "dynamodb", Some(&format!("DynamoDB_20120810.{op}")),
                "application/x-amz-json-1.0", Some(&body), &own_credentials(), now_ms()).await {
                Err(e) if AGAIN.iter().any(|kind| e.starts_with(kind)) => {
                    let r = superci_core::crypto::random_bytes(2);
                    std::thread::sleep(std::time::Duration::from_millis(u16::from_le_bytes([r[0], r[1]]) as u64 % most_ms + 1));
                    most_ms = (most_ms * 2).min(5000);
                }
                done => return done,
            }
        }
        aws::json_call(self.http, "POST", &format!("https://dynamodb.{}.amazonaws.com/", self.region), &self.region, "dynamodb", Some(&format!("DynamoDB_20120810.{op}")),
            "application/x-amz-json-1.0", Some(&body), &own_credentials(), now_ms()).await
    }
    fn key(&self, k: &str) -> Value { json!({ "p": { "S": part(k) }, "k": { "S": k } }) }
    fn old_key(&self, k: &str) -> Value { json!({ "p": { "S": "s" }, "k": { "S": k } }) }
    fn item(&self, k: &str, value: String, until_ms: Option<u64>) -> Value {
        let mut item = json!({ "p": { "S": part(k) }, "k": { "S": k }, "v": { "S": value } });
        if let Some(until) = until_ms { item["x"] = json!({ "N": (until / 1000).to_string() }) }
        item
    }
    /// Whether records may still be in the old partition (asked once per request).
    async fn old(&self) -> io::Result<bool> {
        if let Some(old) = self.old.get() { return Ok(old) }
        let v = self.call("GetItem", json!({ "TableName": self.table, "Key": self.key(LAYOUT), "ConsistentRead": true })).await?;
        let old = v["Item"]["v"]["S"].as_str() != Some("kinds");
        self.old.set(Some(old));
        Ok(old)
    }
    async fn get_old(&self, key: &str) -> io::Result<Option<String>> {
        if !self.old().await? { return Ok(None) }
        let v = self.call("GetItem", json!({ "TableName": self.table, "Key": self.old_key(key), "ConsistentRead": true })).await?;
        Ok(v["Item"]["v"]["S"].as_str().map(str::to_string))
    }
    async fn query(&self, partition: &str, prefix: &str, last: Option<usize>) -> io::Result<Vec<(String, String)>> {
        let (mut out, mut start) = (vec![], Value::Null);
        loop {
            let mut q = json!({ "TableName": self.table, "KeyConditionExpression": "p = :p AND begins_with(k, :k)", "ExpressionAttributeValues": { ":p": { "S": partition }, ":k": { "S": prefix } }, "ConsistentRead": true });
            if let Some(n) = last { q["ScanIndexForward"] = false.into(); q["Limit"] = (n - out.len()).into() }
            if !start.is_null() { q["ExclusiveStartKey"] = start }
            let v = self.call("Query", q).await?;
            for item in v["Items"].as_array().into_iter().flatten() {
                if let (Some(k), Some(val)) = (item["k"]["S"].as_str(), item["v"]["S"].as_str()) { out.push((k.to_string(), val.to_string())) }
            }
            start = v["LastEvaluatedKey"].clone();
            if start.is_null() || last.is_some_and(|n| out.len() >= n) { break }
        }
        if last.is_some() { out.reverse() }
        Ok(out)
    }
    /// A conditional write: whether it was made.
    async fn put_when(&self, put: Value) -> io::Result<bool> {
        match self.call("PutItem", put).await { Ok(_) => Ok(true), Err(e) if e.starts_with("ConditionalCheckFailedException") => Ok(false), Err(e) => Err(e) }
    }
}

#[async_trait(?Send)]
impl Store for Dynamo<'_> {
    async fn get(&self, key: &str) -> io::Result<Option<String>> {
        let v = self.call("GetItem", json!({ "TableName": self.table, "Key": self.key(key), "ConsistentRead": true })).await?;
        match v["Item"]["v"]["S"].as_str() { Some(s) => Ok(Some(s.to_string())), None => self.get_old(key).await }
    }
    async fn put(&self, key: &str, value: String) -> io::Result<()> {
        self.call("PutItem", json!({ "TableName": self.table, "Item": self.item(key, value, None) })).await.map(|_| ())
    }
    /// Requests run in parallel here: DynamoDB's condition makes the claim atomic.
    async fn put_if_absent(&self, key: &str, value: String) -> io::Result<bool> {
        if self.get_old(key).await?.is_some() { return Ok(false) }
        self.put_when(json!({ "TableName": self.table, "Item": self.item(key, value, None), "ConditionExpression": "attribute_not_exists(k)" })).await
    }
    async fn swap(&self, key: &str, was: Option<&str>, value: String) -> io::Result<bool> { self.swap_until(key, was, value, None).await }
    async fn delete(&self, key: &str) -> io::Result<()> {
        self.call("DeleteItem", json!({ "TableName": self.table, "Key": self.key(key) })).await?;
        if self.old().await? { self.call("DeleteItem", json!({ "TableName": self.table, "Key": self.old_key(key) })).await?; }
        Ok(())
    }
    async fn list(&self, prefix: &str) -> io::Result<Vec<(String, String)>> {
        let mut out = self.query(part(prefix), prefix, None).await?;
        if self.old().await? {
            let have: std::collections::HashSet<String> = out.iter().map(|(k, _)| k.clone()).collect();
            out.extend(self.query("s", prefix, None).await?.into_iter().filter(|(k, _)| !have.contains(k)));
            out.sort_by(|a, b| a.0.cmp(&b.0));
        }
        Ok(out)
    }
    async fn put_until(&self, key: &str, value: String, until_ms: u64) -> io::Result<()> {
        self.call("PutItem", json!({ "TableName": self.table, "Item": self.item(key, value, Some(until_ms)) })).await.map(|_| ())
    }
    /// The same for a write over what was read: only while the value is still that one. (A record still in the old
    /// partition is written in its own, only while that one is not there yet.)
    async fn swap_until(&self, key: &str, was: Option<&str>, value: String, until_ms: Option<u64>) -> io::Result<bool> {
        let mut put = json!({ "TableName": self.table, "Item": self.item(key, value, until_ms) });
        let in_old = match was { Some(was) if self.old().await? => {
            let here = self.call("GetItem", json!({ "TableName": self.table, "Key": self.key(key), "ConsistentRead": true })).await?;
            here["Item"].is_null() && self.get_old(key).await?.as_deref() == Some(was)
        } _ => false };
        match was {
            Some(was) if !in_old => { put["ConditionExpression"] = "v = :was".into(); put["ExpressionAttributeValues"] = json!({ ":was": { "S": was } }) }
            _ => put["ConditionExpression"] = "attribute_not_exists(k)".into(),
        }
        let made = self.put_when(put).await?;
        if made && in_old { self.call("DeleteItem", json!({ "TableName": self.table, "Key": self.old_key(key) })).await?; }
        Ok(made)
    }
    async fn list_last(&self, prefix: &str, n: usize) -> io::Result<Vec<(String, String)>> {
        if n == 0 { return Ok(vec![]) }
        if self.old().await? {
            let mut all = self.list(prefix).await?;
            return Ok(all.split_off(all.len().saturating_sub(n)))
        }
        self.query(part(prefix), prefix, Some(n)).await
    }
    /// A hundred at a time (DynamoDB's most), asked again for what it leaves for later.
    async fn get_many(&self, keys: &[String]) -> io::Result<Vec<Option<String>>> {
        let mut found: std::collections::HashMap<String, String> = std::collections::HashMap::new();
        for chunk in keys.chunks(100) {
            let mut want: Vec<Value> = chunk.iter().collect::<std::collections::BTreeSet<_>>().into_iter().map(|k| self.key(k)).collect();
            for _ in 0..8 {
                if want.is_empty() { break }
                let v = self.call("BatchGetItem", json!({ "RequestItems": { &self.table: { "Keys": want, "ConsistentRead": true } } })).await?;
                for item in v["Responses"][&self.table].as_array().into_iter().flatten() {
                    if let (Some(k), Some(val)) = (item["k"]["S"].as_str(), item["v"]["S"].as_str()) { found.insert(k.to_string(), val.to_string()); }
                }
                want = v["UnprocessedKeys"][&self.table]["Keys"].as_array().cloned().unwrap_or_default();
            }
        }
        let mut out = Vec::with_capacity(keys.len());
        for k in keys { out.push(match found.get(k) { Some(v) => Some(v.clone()), None => self.get_old(k).await? }) }
        Ok(out)
    }
    /// Moves what is left in the old partition into each record's own (not over one written there since), then says
    /// the layout is this one.
    async fn migrate(&self, budget: usize) -> io::Result<bool> {
        if !self.old().await? { return Ok(true) }
        let v = self.call("Query", json!({ "TableName": self.table, "KeyConditionExpression": "p = :p", "ExpressionAttributeValues": { ":p": { "S": "s" } }, "Limit": budget.max(1), "ConsistentRead": true })).await?;
        let items = v["Items"].as_array().cloned().unwrap_or_default();
        for item in &items {
            let (Some(k), Some(val)) = (item["k"]["S"].as_str(), item["v"]["S"].as_str()) else { continue };
            self.put_when(json!({ "TableName": self.table, "Item": self.item(k, val.to_string(), None), "ConditionExpression": "attribute_not_exists(k)" })).await?;
            self.call("DeleteItem", json!({ "TableName": self.table, "Key": self.old_key(k) })).await?;
        }
        if !items.is_empty() || !v["LastEvaluatedKey"].is_null() { return Ok(false) }
        self.put(LAYOUT, "kinds".into()).await?;
        self.old.set(Some(false));
        Ok(true)
    }
}

struct SystemClock;
impl Clock for SystemClock { fn now_ms(&self) -> u64 { now_ms() } }

/// A wake-up asked for within this long is the function's own doing; a later one is left to the schedule.
const SOON_MS: u64 = 30_000;

/// The schedule calls the sweep every minute, which is late for what is asked for sooner: a start that failed and
/// is tried again, jobs waiting for room (twenty seconds). For those the function calls itself, not waiting for the
/// answer; that call waits out the time and sweeps (see `main`). One such call is on its way at a time, however
/// many jobs ask (claimed in the table): a sweep is for every job, and asks for the next itself while any waits.
/// Where it may not call itself (a role from before), the schedule does it, as it did.
struct Scheduled<'a> { http: &'a Blocking, store: &'a Dynamo<'a>, region: &'a str, function: &'a str,
    /// What is to be done once the request at hand is answered (see `main`).
    later: &'a std::cell::RefCell<Vec<String>> }

/// Where a sweep that is going notes itself (when it began, ms).
const SWEEPING: &str = "aws:sweeping";

/// Where a sweep that is on its way is noted (when it is for, ms). One still there well after its time never came.
const WAKING: &str = "aws:waking";

#[async_trait(?Send)]
impl Timer for Scheduled<'_> {
    async fn wake_in(&self, ms: u64) -> io::Result<()> {
        if ms > SOON_MS || self.function.is_empty() { return Ok(()) }
        let at = now_ms() + ms;
        // (Nothing here fails what asked: the schedule's sweep comes within the minute whatever happens.)
        if !self.store.put_if_absent(WAKING, at.to_string()).await.unwrap_or(false) {
            let coming = self.store.get(WAKING).await.ok().flatten().and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
            if now_ms() < coming + SOON_MS { return Ok(()) }
            let _ = self.store.put(WAKING, at.to_string()).await;
        }
        let body = json!({ "superci": "wake", "at": at }).to_string();
        let url = format!("https://lambda.{}.amazonaws.com/2015-03-31/functions/{}/invocations", self.region, self.function);
        let headers = vec![("content-type".to_string(), "application/json".to_string()), ("x-amz-invocation-type".to_string(), "Event".to_string())];
        let sent = match aws::sigv4("POST", &url, &headers, body.as_bytes(), &own_credentials(), self.region, "lambda", &aws::amz_date(now_ms())) {
            Ok(signed) => { let mut r = Request::new("POST", &url).with_body(body); r.headers = signed; self.http.send(r).await.is_ok_and(|r| r.status < 300) }
            Err(_) => false,
        };
        if !sent { let _ = self.store.delete(WAKING).await; }
        Ok(())
    }
    /// A Lambda answers one request at a time: it simply waits.
    async fn pause(&self, ms: u64) { std::thread::sleep(Duration::from_millis(ms)) }
    /// Done by `main` once the answer is given: the function goes on running until it asks for its next request.
    fn later(&self, what: &str) -> bool { self.later.borrow_mut().push(what.to_string()); true }
}

/// The secrets under /superci/<id>/, read at most every 15 seconds.
struct Secrets { at: Option<Instant>, values: Vec<(String, String)> }

impl Secrets {
    /// `fresh`: a dashboard key not seen yet arrived (a new session just set it): read again now, at most every second.
    fn refresh(&mut self, http: &Blocking, region: &str, plane_id: &str, fresh: bool) -> io::Result<()> {
        let within = if fresh { Duration::from_secs(1) } else { Duration::from_secs(15) };
        if self.at.is_some_and(|t| t.elapsed() < within) { return Ok(()) }
        let (mut values, mut next) = (vec![], None::<String>);
        loop {
            let mut body = json!({ "Path": format!("/superci/{plane_id}/"), "Recursive": false, "WithDecryption": true, "MaxResults": 10 });
            if let Some(n) = &next { body["NextToken"] = json!(n) }
            let v = block_on(aws::json_call(http, "POST", &format!("https://ssm.{region}.amazonaws.com/"), region, "ssm", Some("AmazonSSM.GetParametersByPath"), "application/x-amz-json-1.1", Some(&body), &own_credentials(), now_ms()))?;
            for p in v["Parameters"].as_array().into_iter().flatten() {
                if let (Some(n), Some(val)) = (p["Name"].as_str(), p["Value"].as_str()) { values.push((n.rsplit('/').next().unwrap_or(n).to_string(), val.to_string())) }
            }
            next = v["NextToken"].as_str().map(str::to_string);
            if next.is_none() { break }
        }
        self.values = values;
        self.at = Some(Instant::now());
        Ok(())
    }
    fn get(&self, name: &str) -> Option<&str> { self.values.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_str()) }
}

fn config(secrets: &Secrets, account_id: &str) -> Config {
    let var = |n: &str| std::env::var(n).ok().filter(|v| !v.is_empty());
    let mut c = Config::new(var("PLANE_ID").unwrap_or_default());
    if let Some(v) = var("LABEL") { c.label = v.to_ascii_lowercase() }
    if let Some(v) = secrets.get("LABELS") { c.set_labels(v) }
    if let Some(v) = secrets.get("NAME") { c.set_name(v) }
    if let Some(v) = var("INSTANCE_TYPES") { c.instance_types = v.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect() }
    c.app = secrets.get("GITHUB_APP").and_then(|v| serde_json::from_str(v).ok());
    // One parameter for each further organization's GitHub App: GITHUB_APP_<its id>.
    c.more_apps = secrets.values.iter().filter(|(n, _)| n.starts_with("GITHUB_APP_")).filter_map(|(_, v)| serde_json::from_str::<superci_core::github::App>(v).ok())
        .filter(|a| c.app.as_ref().is_none_or(|f| f.id != a.id)).collect();
    c.agents = secrets.get("AGENTS").and_then(|v| serde_json::from_str::<Vec<Agent>>(v).ok()).unwrap_or_default();
    c.routing = secrets.get("ROUTING").and_then(|v| serde_json::from_str(v).ok()).unwrap_or_default();
    c.gitlab = secrets.get("GITLAB").and_then(|v| serde_json::from_str(v).ok());
    // One parameter for each further GitLab connection: GITLAB_<its name>.
    c.more_gitlabs = secrets.values.iter().filter(|(n, _)| n.starts_with("GITLAB_")).filter_map(|(_, v)| serde_json::from_str::<superci_core::gitlab::GitLab>(v).ok())
        .filter(|g| superci_core::gitlab::valid_id(&g.id)).collect();
    c.machine = secrets.get("MACHINE").and_then(|v| serde_json::from_str(v).ok()).unwrap_or_default();
    // One parameter per dashboard session: DASHBOARD_KEY_<expires, unix seconds>_<random>.
    c.dashboard_keys = secrets.values.iter().filter_map(|(n, v)| Some((n.strip_prefix("DASHBOARD_KEY_")?.split('_').next()?.parse::<u64>().ok()? * 1000, v.clone()))).collect();
    // One parameter per key that only reads: READ_KEY_<expires, unix seconds>_<NAME>, holding the key's SHA-256.
    c.read_keys = secrets.values.iter().filter_map(|(n, v)| superci_core::plane::read_key(n, v)).collect();
    c.move_token = secrets.get("MOVE_TOKEN").map(str::to_string);
    if let Some(l) = secrets.get("CF_LOCATION") { c.cloudflare_location = l.to_string() }
    c.cloudflare_image = secrets.get("CF_IMAGE").and_then(superci_core::plane::image_address);
    c.aws_regions = secrets.get("AWS_REGIONS").and_then(|v| serde_json::from_str(v).ok()).unwrap_or_default();
    c.aws_networks = secrets.get("AWS_NETWORKS").and_then(|v| serde_json::from_str(v).ok()).unwrap_or_default();
    c.aws_own = Some((account_id.to_string(), var("RUNNER_REGION").or(var("AWS_REGION")).unwrap_or_default()));
    c.aws_own_creds = Some(own_credentials());
    c.aws_runners_off = secrets.get("AWS_RUNNERS") == Some("off");
    c
}

/// A function URL event (payload 2.0) as a request.
fn request(event: &Value) -> Request {
    let ctx = &event["requestContext"];
    let query = event["rawQueryString"].as_str().filter(|q| !q.is_empty()).map(|q| format!("?{q}")).unwrap_or_default();
    let url = format!("https://{}{}{query}", ctx["domainName"].as_str().unwrap_or_default(), event["rawPath"].as_str().unwrap_or("/"));
    let body = event["body"].as_str().unwrap_or_default();
    let body = if event["isBase64Encoded"] == true { STANDARD.decode(body).unwrap_or_default() } else { body.as_bytes().to_vec() };
    let mut r = Request::new(ctx["http"]["method"].as_str().unwrap_or("GET"), &url).with_body(body);
    for (k, v) in event["headers"].as_object().into_iter().flatten() { if let Some(v) = v.as_str() { r.headers.push((k.to_ascii_lowercase(), v.to_string())) } }
    r
}

fn response(r: Response) -> Value {
    let headers: serde_json::Map<String, Value> = r.headers.into_iter().map(|(k, v)| (k, Value::String(v))).collect();
    json!({ "statusCode": r.status, "headers": headers, "body": STANDARD.encode(&r.body), "isBase64Encoded": true })
}

fn main() {
    let api = std::env::var("AWS_LAMBDA_RUNTIME_API").expect("runs in AWS Lambda");
    let region = std::env::var("AWS_REGION").unwrap_or_default();
    let (http, runtime) = (Blocking(agent()), ureq::Agent::from(ureq::Agent::config_builder().http_status_as_error(false).build()));
    let table = std::env::var("TABLE").unwrap_or_default();
    let plane_id = std::env::var("PLANE_ID").unwrap_or_default();
    let function = std::env::var("AWS_LAMBDA_FUNCTION_NAME").unwrap_or_default();
    let cache = Cache::default();
    let mut secrets = Secrets { at: None, values: vec![] };
    loop {
        let Ok(mut next) = runtime.get(&format!("http://{api}/2018-06-01/runtime/invocation/next")).call() else { continue };
        let id = next.headers().get("lambda-runtime-aws-request-id").and_then(|v| v.to_str().ok()).unwrap_or_default().to_string();
        let account_id = next.headers().get("lambda-runtime-invoked-function-arn").and_then(|v| v.to_str().ok()).and_then(|a| a.split(':').nth(4)).unwrap_or_default().to_string();
        let event: Value = next.body_mut().read_json().unwrap_or_default();
        // Whoever comes with a key (the dashboard, a command) gets the settings as they are now, not as they
        // were up to fifteen seconds ago: a change is seen at once, and the next one starts from it. A key not
        // known yet is a new session's, set a moment ago. Webhooks and the sweep carry none, and use what was read.
        let bearer = event["headers"]["authorization"].as_str().and_then(|a| a.strip_prefix("Bearer ")).unwrap_or_default();
        let unknown = bearer.len() >= 16;
        // A move's steps need what the dashboard wrote a moment ago (the App, GitLab): read fresh for them too.
        let moving = event["rawPath"].as_str().is_some_and(|p| p.starts_with("/move/"));
        let refreshed = secrets.refresh(&http, &region, &plane_id, unknown || moving);
        let config = config(&secrets, &account_id);
        let store = Dynamo { http: &http, table: table.clone(), region: region.clone(), old: std::cell::Cell::new(None) };
        let later = std::cell::RefCell::new(vec![]);
        let timer = Scheduled { http: &http, store: &store, region: &region, function: &function, later: &later };
        let plane = ControlPlane { store: &store, http: &http, clock: &SystemClock, timer: &timer, config: &config, cache: &cache, containers: None };
        let outcome: io::Result<Value> = (|| {
            refreshed?;
            // One sweep at a time: the schedule's and the function's own could otherwise each start a machine for
            // the same waiting job. One that finds another going leaves it to that one (which asks for the next
            // itself); a note older than a minute and a half is of a sweep that was cut off.
            let sweep = || -> io::Result<Value> {
                let now = now_ms();
                if !block_on(store.put_if_absent(SWEEPING, now.to_string())).unwrap_or(true) {
                    let since = block_on(store.get(SWEEPING)).ok().flatten().and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
                    if now.saturating_sub(since) < 90_000 { return Ok(json!({ "swept": false })) }
                    let _ = block_on(store.put(SWEEPING, now.to_string()));
                }
                let done = block_on(plane.alarm()).map(|_| json!({ "swept": true }));
                let _ = block_on(store.delete(SWEEPING));
                done
            };
            if event["superci"] == "sweep" { return sweep() }
            // Its own call for a sweep sooner than the schedule's: the time is waited out here, then the sweep.
            if event["superci"] == "wake" {
                let at = event["at"].as_u64().unwrap_or(0);
                std::thread::sleep(Duration::from_millis(at.saturating_sub(now_ms()).min(SOON_MS)));
                let _ = block_on(store.delete(WAKING));
                return sweep()
            }
            Ok(response(block_on(plane.handle(request(&event)))))
        })();
        let _ = match outcome {
            Ok(v) => runtime.post(&format!("http://{api}/2018-06-01/runtime/invocation/{id}/response")).send_json(v),
            Err(e) => { eprintln!("superci: {e}"); runtime.post(&format!("http://{api}/2018-06-01/runtime/invocation/{id}/error")).send_json(json!({ "errorMessage": e, "errorType": "superci" })) }
        };
        // What was left for after the answer (a queued job's machine: GitHub has its answer, and AWS is asked now).
        // The function runs on until it asks for its next request, and takes no other meanwhile.
        let url = request(&event).url;
        // (One thing done may leave another: each in its turn, until none is left.)
        loop {
            let next = { let mut left = later.borrow_mut(); if left.is_empty() { None } else { Some(left.remove(0)) } };
            let Some(what) = next else { break };
            if let Err(e) = block_on(plane.later(&url, &what)) { eprintln!("superci: {what}: {e}") }
        }
    }
}

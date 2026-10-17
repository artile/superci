//! The AWS control plane's program as Lambda runs it: the real `bootstrap`, given its requests by a stand-in for
//! Lambda's runtime API here, its calls to AWS answered by the test kit's stand-in (DynamoDB's items, SSM's
//! parameters, S3's objects, Lambda's calls). Once as the control plane (a request through its URL, the sweep, its own
//! wake-up), once as the layer store's converter (with a stand-in for mkfs.erofs).
use std::collections::{BTreeMap, VecDeque};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Child, Command};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::Engine;
use serde_json::{json, Value};
use superci_testkit::{Aws, GitHub};

const PLANE: &str = "lambdaplane1";
const TABLE: &str = "superci-plane-lambdaplane1";

fn now_ms() -> u64 { SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as u64 }

/// Lambda's runtime API, as a function's program sees it: its next request (waited for), and its answer to each.
#[derive(Default)]
struct Runtime { queue: Mutex<VecDeque<(String, Value)>>, queued: Condvar, answers: Mutex<BTreeMap<String, (String, Value)>>, answered: Condvar }

impl Runtime {
    fn serve() -> (Arc<Runtime>, String) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let at = listener.local_addr().unwrap().to_string();
        let rt = Arc::new(Runtime::default());
        let r = rt.clone();
        std::thread::spawn(move || for stream in listener.incoming().flatten() { let r = r.clone(); std::thread::spawn(move || r.exchange(stream)); });
        (rt, at)
    }

    fn exchange(&self, mut stream: TcpStream) {
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut line = String::new();
        if reader.read_line(&mut line).is_err() { return }
        let path = line.split(' ').nth(1).unwrap_or_default().to_string();
        let mut length = 0;
        loop {
            let mut h = String::new();
            if reader.read_line(&mut h).is_err() || h.trim().is_empty() { break }
            if let Some(v) = h.to_ascii_lowercase().strip_prefix("content-length:") { length = v.trim().parse().unwrap_or(0) }
        }
        let mut body = vec![0; length];
        let _ = reader.read_exact(&mut body);
        let answer = |stream: &mut TcpStream, extra: &str, body: &str| { let _ = stream.write_all(format!("HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n{extra}content-length: {}\r\nconnection: close\r\n\r\n{body}", body.len()).as_bytes()); };
        if path.ends_with("/invocation/next") {
            let mut q = self.queue.lock().unwrap();
            while q.is_empty() { q = self.queued.wait(q).unwrap() }
            let (id, event) = q.pop_front().unwrap();
            drop(q);
            answer(&mut stream, &format!("lambda-runtime-aws-request-id: {id}\r\nlambda-runtime-invoked-function-arn: arn:aws:lambda:us-east-1:111111111111:function:{TABLE}\r\n"), &event.to_string());
        } else if let Some(rest) = path.strip_prefix("/2018-06-01/runtime/invocation/") {
            let (id, kind) = rest.split_once('/').unwrap_or_default();
            self.answers.lock().unwrap().insert(id.to_string(), (kind.to_string(), serde_json::from_slice(&body).unwrap_or(Value::Null)));
            self.answered.notify_all();
            answer(&mut stream, "", "{}");
        }
    }

    /// A request given to the function, and how it answered (`response` or `error`, and what it said).
    fn invoke(&self, event: Value) -> (String, Value) {
        let id = format!("request-{}", now_ms() % 1_000_000_007 + self.answers.lock().unwrap().len() as u64);
        self.queue.lock().unwrap().push_back((id.clone(), event));
        self.queued.notify_all();
        let until = Instant::now() + Duration::from_secs(60);
        let mut answers = self.answers.lock().unwrap();
        loop {
            if let Some(a) = answers.remove(&id) { return a }
            assert!(Instant::now() < until, "the function did not answer");
            answers = self.answered.wait_timeout(answers, Duration::from_millis(200)).unwrap().0;
        }
    }
}

/// The program, started as Lambda starts it (`env`: what this function's are).
struct Function(Child);
impl Drop for Function { fn drop(&mut self) { let _ = self.0.kill(); let _ = self.0.wait(); } }

fn start(runtime: &str, stand_in: &str, env: &[(&str, &str)]) -> Function {
    let mut c = Command::new(env!("CARGO_BIN_EXE_bootstrap"));
    // (Only what the program reads is set or taken away: the rest, a coverage tool's among it, stays.)
    for k in ["PLANE_ID", "TABLE", "LABEL", "RUNNER_REGION", "INSTANCE_TYPES", "AWS_LAMBDA_FUNCTION_NAME", "SUPERCI_CONVERTER", "LAMBDA_TASK_ROOT", "AWS_PROFILE"] { c.env_remove(k); }
    c.env("AWS_LAMBDA_RUNTIME_API", runtime).env("AWS_REGION", "us-east-1").env("SUPERCI_STAND_IN", stand_in)
        .env("AWS_ACCESS_KEY_ID", "ASIAEXAMPLEFUNCTION1").env("AWS_SECRET_ACCESS_KEY", "example-secret").env("AWS_SESSION_TOKEN", "example-token");
    for (k, v) in env { c.env(k, v); }
    Function(c.spawn().unwrap())
}

/// A request through the function's URL, as Lambda gives it (payload 2.0).
fn url_event(method: &str, path: &str, headers: Value, body: &str) -> Value {
    json!({ "version": "2.0", "rawPath": path, "rawQueryString": "", "headers": headers, "isBase64Encoded": true, "body": base64::engine::general_purpose::STANDARD.encode(body),
        "requestContext": { "domainName": "lambdaplane1.lambda-url.us-east-1.on.aws", "http": { "method": method } } })
}

fn item(aws: &Aws, key: &str) -> Option<String> {
    let p = key.split(':').next().unwrap();
    aws.items.lock().unwrap().get(&(TABLE.to_string(), p.to_string(), key.to_string())).and_then(|i| i["v"]["S"].as_str().map(str::to_string))
}

fn aws() -> Aws {
    let aws = Aws::serve();
    *aws.github.lock().unwrap() = Some(GitHub::default());
    aws
}

#[test]
fn the_control_plane_answers_its_url_sweeps_and_wakes_itself_as_lambda_runs_it() {
    let aws = aws();
    aws.put(&format!("/superci/{PLANE}/MAX_CPU"), "16");
    let (runtime, at) = Runtime::serve();
    let _f = start(&at, &aws.url, &[("PLANE_ID", PLANE), ("TABLE", TABLE), ("LABEL", "AcmeCI"), ("RUNNER_REGION", "us-east-1"), ("AWS_LAMBDA_FUNCTION_NAME", TABLE)]);
    // Its URL: what it answers, its settings read from SSM, its state in its table.
    let (kind, answer) = runtime.invoke(url_event("GET", "/health", json!({}), ""));
    assert_eq!((kind.as_str(), answer["statusCode"].as_u64()), ("response", Some(200)), "{answer}");
    let health: Value = serde_json::from_slice(&base64::engine::general_purpose::STANDARD.decode(answer["body"].as_str().unwrap()).unwrap()).unwrap();
    assert_eq!((health["plane"].as_str(), health["label"].as_str(), health["version"].as_str()), (Some(PLANE), Some("acmeci"), Some(env!("CARGO_PKG_VERSION"))), "{health}");
    assert_eq!(item(&aws, "plane_url").as_deref(), Some("\"https://lambdaplane1.lambda-url.us-east-1.on.aws\""));
    let reads = || aws.asked.lock().unwrap().iter().filter(|a| a["target"] == "AmazonSSM.GetParametersByPath").count();
    // (Settings read at most every 15 seconds; at once for a key it has not seen, a new session's.)
    let before = reads();
    runtime.invoke(url_event("GET", "/health", json!({}), ""));
    assert_eq!(reads(), before, "read a moment ago");
    std::thread::sleep(Duration::from_millis(1100));
    runtime.invoke(url_event("GET", "/status", json!({ "authorization": "Bearer a-new-sessions-key-0123456789" }), ""));
    assert_eq!(reads(), before + 1, "a key it has not seen: read again at once");
    // The schedule's sweep; one while another goes is left to that one.
    let (kind, swept) = runtime.invoke(json!({ "superci": "sweep" }));
    assert_eq!((kind.as_str(), &swept), ("response", &json!({ "swept": true })));
    assert_eq!(item(&aws, "aws:sweeping"), None, "its note taken away once done");
    aws.items.lock().unwrap().insert((TABLE.into(), "aws".into(), "aws:sweeping".into()), json!({ "p": { "S": "aws" }, "k": { "S": "aws:sweeping" }, "v": { "S": now_ms().to_string() } }));
    assert_eq!(runtime.invoke(json!({ "superci": "sweep" })).1, json!({ "swept": false }));
    // One left by a sweep cut off long ago is not waited for.
    aws.items.lock().unwrap().insert((TABLE.into(), "aws".into(), "aws:sweeping".into()), json!({ "p": { "S": "aws" }, "k": { "S": "aws:sweeping" }, "v": { "S": (now_ms() - 600_000).to_string() } }));
    assert_eq!(runtime.invoke(json!({ "superci": "sweep" })).1, json!({ "swept": true }));
    // Its own wake-up: the time waited out, then a sweep, its note gone.
    aws.items.lock().unwrap().insert((TABLE.into(), "aws".into(), "aws:waking".into()), json!({ "p": { "S": "aws" }, "k": { "S": "aws:waking" }, "v": { "S": now_ms().to_string() } }));
    let began = Instant::now();
    assert_eq!(runtime.invoke(json!({ "superci": "wake", "at": now_ms() + 1500 })).1, json!({ "swept": true }));
    assert!(began.elapsed() >= Duration::from_millis(1200), "waited for its time: {:?}", began.elapsed());
    assert_eq!(item(&aws, "aws:waking"), None);
    assert!(aws.unanswered.lock().unwrap().iter().all(|u| !u.contains("dynamodb")), "{:?}", aws.unanswered.lock().unwrap());
}

/// A stand-in for mkfs.erofs: the tar it is given, written where it is told (its last argument); a tar that says
/// "refuse" refused.
fn mkfs_dir() -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("superci-lambda-converter-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    let p = d.join("mkfs.erofs");
    std::fs::write(&p, "#!/bin/sh\nfor a; do out=$a; done\ncat > \"$out\"\nif grep -q refuse \"$out\"; then echo 'unusual entry' >&2; exit 1; fi\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    d
}

/// A registry's blob at an address here: its bytes, whatever is asked.
fn registry(blob: Vec<u8>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let at = format!("http://{}/v2/acme/app/blobs/x", listener.local_addr().unwrap());
    std::thread::spawn(move || for mut s in listener.incoming().flatten() {
        let mut buf = [0u8; 4096];
        let _ = s.read(&mut buf);
        assert!(String::from_utf8_lossy(&buf).to_ascii_lowercase().contains("authorization: bearer registry-token"), "fetched with the token it was given");
        let _ = s.write_all(format!("HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n", blob.len()).as_bytes());
        let _ = s.write_all(&blob);
    });
    at
}

fn gzip(b: &[u8]) -> Vec<u8> {
    let mut out = Command::new("gzip").arg("-c").stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).spawn().unwrap();
    out.stdin.take().unwrap().write_all(b).unwrap();
    out.wait_with_output().unwrap().stdout
}

fn sha(b: &[u8]) -> String {
    let out = Command::new("shasum").args(["-a", "256"]).stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).spawn().unwrap();
    let mut out = out;
    out.stdin.take().unwrap().write_all(b).unwrap();
    format!("sha256:{}", String::from_utf8(out.wait_with_output().unwrap().stdout).unwrap().split(' ').next().unwrap())
}

#[test]
fn the_converter_makes_a_layers_file_from_a_registry_or_from_staging_as_lambda_runs_it() {
    let aws = aws();
    let gh = aws.github.lock().unwrap().clone().unwrap();
    let (runtime, at) = Runtime::serve();
    let dir = mkfs_dir();
    let _f = start(&at, &aws.url, &[("SUPERCI_CONVERTER", "1"), ("LAMBDA_TASK_ROOT", dir.to_str().unwrap())]);
    let bucket = "superci-plane-lambdaplane1-us-east-1";
    let job = |digest: &str, size: usize, source: Value| {
        let hex = &digest[7..];
        json!({ "digest": digest, "size": size, "region": "us-east-1", "bucket": bucket, "source": source,
            "erofs_key": format!("layers/{hex}.erofs"), "blob_key": format!("layers/{hex}.blob"), "done_key": format!("layers/done/{hex}.json") })
    };
    let object = |key: &str| gh.objects().into_iter().find(|(k, _)| k == &format!("{bucket}/{key}")).map(|(_, n)| n);
    // From a registry, with its token: the blob kept as it came, its tar made a file, what was made said beside them.
    let tar = b"a layer's tar".repeat(500);
    let blob = gzip(&tar);
    let d = sha(&blob);
    let (kind, done) = runtime.invoke(job(&d, blob.len(), json!({ "url": registry(blob.clone()), "headers": { "authorization": "Bearer registry-token" } })));
    assert_eq!(kind, "response");
    assert_eq!((done["digest"].as_str(), done["diff_id"].as_str(), done["erofs"]["size"].as_u64(), done["error"].as_str()), (Some(d.as_str()), Some(sha(&tar).as_str()), Some(tar.len() as u64), None), "{done}");
    assert_eq!((object(&format!("layers/{}.erofs", &d[7..])), object(&format!("layers/{}.blob", &d[7..]))), (Some(tar.len()), Some(blob.len())));
    assert!(object(&format!("layers/done/{}.json", &d[7..])).is_some());
    // From staging (a machine's upload): the blob copied in S3 and taken from staging.
    let tar2 = b"another layer".repeat(300);
    let blob2 = gzip(&tar2);
    let d2 = sha(&blob2);
    let staged = format!("layers/staging/{}-abc", &d2[7..]);
    gh.put_object(&format!("{bucket}/{staged}"), &blob2);
    let (_, done) = runtime.invoke(job(&d2, blob2.len(), json!({ "key": staged })));
    assert_eq!((done["error"].as_str(), done["blob"]["key"].as_str()), (None, Some(format!("layers/{}.blob", &d2[7..]).as_str())), "{done}");
    assert_eq!((object(&format!("layers/{}.blob", &d2[7..])), object(&staged)), (Some(blob2.len()), None));
    // What it refuses, said as such: a tar mkfs.erofs takes not, a blob not of its digest, a digest of nothing.
    let odd = gzip(b"refuse this");
    let (_, done) = runtime.invoke(job(&sha(&odd), odd.len(), json!({ "url": registry(odd.clone()), "headers": { "authorization": "Bearer registry-token" } })));
    assert!(done["error"].as_str().is_some_and(|e| e.starts_with("mkfs.erofs refused the layer") && e.ends_with("unusual entry")), "{done}");
    let wrong = format!("sha256:{}", "c".repeat(64));
    let (_, done) = runtime.invoke(job(&wrong, blob.len(), json!({ "url": registry(blob.clone()), "headers": { "authorization": "Bearer registry-token" } })));
    assert!(done["error"].as_str().unwrap().contains("not sha256:cccc"), "{done}");
    assert!(object(&format!("layers/{}.erofs", "c".repeat(64))).is_none(), "nothing made of it");
    let (_, done) = runtime.invoke(job("sha256:nope", 1, json!({ "key": "x" })));
    assert_eq!(done["error"], "not a sha256 digest");
    let (_, done) = runtime.invoke(job(&wrong, 5 << 30, json!({ "key": "x" })));
    assert!(done["error"].as_str().unwrap().contains("converted up to"), "{done}");
    let (kind, done) = runtime.invoke(json!({ "something": "else" }));
    assert!(kind == "response" && done["error"].as_str().unwrap().starts_with("not a layer to convert"), "{done}");
}

#[test]
fn a_request_it_cannot_answer_is_an_error_to_lambda_and_the_next_one_is_answered() {
    // AWS out of reach (its stand-in's address taken away): its settings cannot be read.
    let aws = aws();
    let (runtime, at) = Runtime::serve();
    let f = start(&at, "http://127.0.0.1:9", &[("PLANE_ID", PLANE), ("TABLE", TABLE), ("AWS_LAMBDA_FUNCTION_NAME", TABLE)]);
    let (kind, said) = runtime.invoke(json!({ "superci": "sweep" }));
    assert_eq!((kind.as_str(), said["errorType"].as_str()), ("error", Some("superci")), "{said}");
    assert!(said["errorMessage"].as_str().is_some_and(|m| m.contains("ssm.us-east-1.amazonaws.com")), "{said}");
    drop(f);
    // Back: the same program answers. (A runtime of its own: the one before may still have the last one's wait.)
    let (runtime, at) = Runtime::serve();
    let f = start(&at, &aws.url, &[("PLANE_ID", PLANE), ("TABLE", TABLE), ("AWS_LAMBDA_FUNCTION_NAME", TABLE)]);
    assert_eq!(runtime.invoke(json!({ "superci": "sweep" })), ("response".to_string(), json!({ "swept": true })));
    drop(f);
}

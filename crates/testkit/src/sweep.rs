//! Faults, one at a time, as SQLite tests itself: a scenario run once clean, counting every call the stand-ins answer
//! and every operation of the control plane's store, then again for each of those, that one failing (a call answered
//! with its service's own error, or its connection dropped; a store operation not answered). Each faulted run is
//! held to what SuperCI must do whatever fails: nothing panics, every answer of the control plane's that is an error
//! says what failed, and the state it leaves is one the scenario's own checks take (a job that runs, waits saying
//! why, or failed saying why).
use std::sync::{Arc, Mutex};

use serde_json::json;

use crate::Served;

/// Which fault a run of a scenario is given, armed once the scenario's setup is done (`Served::arm`). Counted from
/// there, from 1.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    /// None: the clean run that counts.
    None,
    /// The k-th call answered with its service's error (AWS's InternalError, a 500 of GitHub's, …).
    Call(usize),
    /// The k-th call's connection dropped before any answer.
    Dropped(usize),
    /// The k-th store operation not answered.
    Store(usize),
}

impl std::fmt::Display for Fault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Fault::None => write!(f, "no fault"),
            Fault::Call(k) => write!(f, "call {k} failing"),
            Fault::Dropped(k) => write!(f, "call {k} dropped"),
            Fault::Store(k) => write!(f, "store operation {k} failing"),
        }
    }
}

/// What the stand-ins and the store counted, the fault armed, and what the control plane answered and where it
/// panicked: shared by a `Served`'s store, machines and stand-ins.
#[derive(Clone, Default)]
pub struct Faults(Arc<Mutex<Faulting>>);

#[derive(Default)]
struct Faulting {
    /// Each call answered and each store operation, in their order (what was asked, as one line).
    calls: Vec<String>,
    ops: Vec<String>,
    /// Where counting started (`arm`): the calls, operations and answers before it.
    marks: (usize, usize, usize),
    /// The call (its place among `calls`) to fail, and whether by dropping it; the operation to fail.
    call: Option<(usize, bool)>,
    op: Option<usize>,
    /// What was failed.
    fired: Vec<String>,
    /// The control plane's panics: what it was answering, and what the panic said.
    panics: Vec<String>,
    /// The request the control plane is answering now, and each answer it gave: request, status, body.
    asking: String,
    answers: Vec<(String, u16, String)>,
}

impl Faults {
    /// Counting starts now, with `fault` armed.
    pub fn arm(&self, fault: Fault) {
        let mut f = self.0.lock().unwrap();
        f.marks = (f.calls.len(), f.ops.len(), f.answers.len());
        let (calls, ops) = (f.calls.len(), f.ops.len());
        (f.call, f.op) = match fault {
            Fault::None => (None, None),
            Fault::Call(k) => (Some((calls + k - 1, false)), None),
            Fault::Dropped(k) => (Some((calls + k - 1, true)), None),
            Fault::Store(k) => (None, Some(ops + k - 1)),
        };
    }

    /// The k-th call from now answered with its service's error.
    pub fn fail_call(&self, k: usize) { let mut f = self.0.lock().unwrap(); f.call = Some((f.calls.len() + k - 1, false)) }
    /// The k-th call from now dropped before any answer.
    pub fn drop_call(&self, k: usize) { let mut f = self.0.lock().unwrap(); f.call = Some((f.calls.len() + k - 1, true)) }
    /// The k-th store operation from now not answered.
    pub fn fail_store_op(&self, k: usize) { let mut f = self.0.lock().unwrap(); f.op = Some(f.ops.len() + k - 1) }

    /// The calls answered since counting started (`arm`), each as one line (`POST https://ec2…/ RunInstances`).
    pub fn calls(&self) -> Vec<String> { let f = self.0.lock().unwrap(); f.calls[f.marks.0..].to_vec() }
    /// The store's operations since then (`put job:90000000001`).
    pub fn store_ops(&self) -> Vec<String> { let f = self.0.lock().unwrap(); f.ops[f.marks.1..].to_vec() }
    /// What was failed.
    pub fn fired(&self) -> Vec<String> { self.0.lock().unwrap().fired.clone() }
    /// Where the control plane panicked.
    pub fn panics(&self) -> Vec<String> { self.0.lock().unwrap().panics.clone() }
    /// The control plane's answers since counting started: request, status, body.
    pub fn answers(&self) -> Vec<(String, u16, String)> { let f = self.0.lock().unwrap(); f.answers[f.marks.2..].to_vec() }

    /// A call counted: whether it is the one to fail (`Some(true)`: dropped).
    pub(crate) fn call(&self, what: String) -> Option<bool> {
        let mut f = self.0.lock().unwrap();
        let at = f.calls.len();
        f.calls.push(what.clone());
        let hit = f.call.filter(|(k, _)| *k == at).map(|(_, dropped)| dropped);
        if let Some(dropped) = hit { f.fired.push(format!("{} {what}", if dropped { "dropped" } else { "failed" })) }
        hit
    }

    /// A store operation counted: its refusal, when it is the one to fail.
    pub(crate) fn op(&self, what: String) -> superci_core::io::Result<()> {
        let mut f = self.0.lock().unwrap();
        let at = f.ops.len();
        f.ops.push(what.clone());
        if f.op != Some(at) { return Ok(()) }
        f.fired.push(format!("failed {what}"));
        Err(format!("the store did not answer ({what})"))
    }

    pub(crate) fn asking(&self, request: String) { self.0.lock().unwrap().asking = request }
    pub(crate) fn answered(&self, status: u16, body: &[u8]) {
        let mut f = self.0.lock().unwrap();
        let asking = f.asking.clone();
        f.answers.push((asking, status, String::from_utf8_lossy(body).chars().take(400).collect()));
    }
    pub(crate) fn panicked(&self, panic: Box<dyn std::any::Any + Send>) {
        let mut f = self.0.lock().unwrap();
        let said = panic.downcast_ref::<String>().cloned().or_else(|| panic.downcast_ref::<&str>().map(|s| s.to_string())).unwrap_or_default();
        let asking = f.asking.clone();
        f.panics.push(format!("answering {asking}: {said}"));
    }
}

/// A failed call's answer, in its service's own words: AWS's (its JSON APIs', its query APIs', S3's), GitHub's,
/// GitLab's, Cloudflare's, Modal's.
pub(crate) fn failed_answer(url: &str, target: &str) -> (u16, Vec<(String, String)>, String) {
    let host = url::Url::parse(url).ok().and_then(|u| u.host_str().map(str::to_string)).unwrap_or_default();
    let (status, kind, body) = if host.contains(".s3.") {
        (500, "application/xml", "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<Error><Code>InternalError</Code><Message>We encountered an internal error. Please try again.</Message></Error>".to_string())
    } else if host.ends_with(".amazonaws.com") && !target.is_empty() {
        (500, "application/x-amz-json-1.1", crate::json_error("InternalServerError", "An internal error occurred. Try again."))
    } else if host.starts_with("lambda.") {
        (500, "application/json", json!({ "Type": "Service", "message": "An internal error occurred. Try again." }).to_string())
    } else if host.ends_with(".amazonaws.com") || host.ends_with(".amazon.com") {
        (500, "text/xml", crate::ec2_error("InternalError", "An internal error has occurred"))
    } else if host.contains("gitlab") {
        (500, "application/json", json!({ "message": "500 Internal Server Error" }).to_string())
    } else if host.ends_with("cloudflare.com") || host.ends_with(".workers.dev") {
        (500, "application/json", json!({ "success": false, "errors": [{ "code": 10000, "message": "Internal server error" }], "messages": [], "result": null }).to_string())
    } else if host.ends_with(".modal.run") || host.contains("modal.com") {
        (500, "application/json", json!({ "detail": "Internal Server Error" }).to_string())
    } else {
        (500, "application/json", json!({ "message": "Server Error" }).to_string())
    };
    (status, vec![("content-type".into(), kind.into())], body)
}

/// One run of a scenario: its control plane, and what the scenario's own checks found wrong (none: all held).
pub struct Outcome { pub served: Served, problems: std::cell::RefCell<Vec<String>> }

impl Outcome {
    pub fn new(served: Served) -> Outcome { Outcome { served, problems: Default::default() } }
    /// A check of the state the run left: `what` said when it does not hold.
    pub fn check(&self, holds: bool, what: impl FnOnce() -> String) { if !holds { self.problems.borrow_mut().push(what()) } }
    /// What did not hold.
    pub fn problems(&self) -> Vec<String> { self.problems.borrow().clone() }
}

/// How many faulted runs a sweep did, and how long it took.
#[derive(Debug)]
pub struct Swept { pub calls: usize, pub store_ops: usize, pub runs: usize, pub ms: u128 }

/// `scenario` run clean, then once for each call and each store operation it made, that one failing (a call twice:
/// answered with an error, and dropped). It builds its own control plane each time and arms the fault it is given
/// once set up (`Served::arm`). Panics, naming each fault and what it broke, when any run broke what must hold.
/// (`SUPERCI_SWEEP_CLEAN=1` runs the clean run alone: what the faults add to coverage is measured against it.)
pub fn sweep(name: &str, scenario: impl Fn(Fault) -> Outcome + Sync) -> Swept {
    let started = std::time::Instant::now();
    let clean = run(&scenario, Fault::None);
    let (calls, ops) = match clean {
        Ok((ref o, ref problems)) if problems.is_empty() => (o.served.faults().calls(), o.served.faults().store_ops()),
        Ok((_, problems)) => panic!("{name}, clean: {}", problems.join("\n  ")),
        Err(e) => panic!("{name}, clean: {e}"),
    };
    let mut faults: Vec<Fault> = vec![];
    if std::env::var("SUPERCI_SWEEP_CLEAN").ok().as_deref() != Some("1") {
        for k in 1..=calls.len() { faults.push(Fault::Call(k)); faults.push(Fault::Dropped(k)) }
        for k in 1..=ops.len() { faults.push(Fault::Store(k)) }
    }
    // (One of them alone, said in full, to look into it: `SUPERCI_SWEEP_ONLY=store:109`, `call:41`, `dropped:41`.)
    let only = std::env::var("SUPERCI_SWEEP_ONLY").ok();
    if let Some((kind, k)) = only.as_deref().and_then(|o| o.split_once(':')) {
        let k: usize = k.parse().unwrap_or(1).max(1);
        faults = vec![match kind { "call" => Fault::Call(k), "dropped" => Fault::Dropped(k), _ => Fault::Store(k) }];
    }
    // (Each run on a control plane of its own: side by side.)
    let next = Mutex::new(0usize);
    let found: Mutex<Vec<String>> = Mutex::default();
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).min(8);
    std::thread::scope(|s| for _ in 0..threads {
        s.spawn(|| loop {
            let i = { let mut n = next.lock().unwrap(); *n += 1; *n - 1 };
            let Some(fault) = faults.get(i).copied() else { break };
            let planned = match fault { Fault::Call(k) | Fault::Dropped(k) => calls.get(k - 1).cloned(), Fault::Store(k) => ops.get(k - 1).cloned(), Fault::None => None }.unwrap_or_default();
            let problems = match run(&scenario, fault) {
                Ok((o, p)) => {
                    let f = o.served.faults();
                    if only.is_some() { eprintln!("{fault}: {:?}\ncalls: {:#?}\nanswers: {:#?}", f.fired(), f.calls(), f.answers()) }
                    p
                }
                Err(e) => vec![e],
            };
            if !problems.is_empty() { found.lock().unwrap().push(format!("{fault} ({planned}):\n    {}", problems.join("\n    "))) }
        });
    });
    let swept = Swept { calls: calls.len(), store_ops: ops.len(), runs: faults.len(), ms: started.elapsed().as_millis() };
    eprintln!("{name}: {} calls, {} store operations, {} faulted runs in {} ms", swept.calls, swept.store_ops, swept.runs, swept.ms);
    let mut found = found.into_inner().unwrap();
    found.sort();
    assert!(found.is_empty(), "{name}: {} of {} faulted runs broke what must hold:\n  {}", found.len(), swept.runs, found.join("\n  "));
    swept
}

/// One run, held to what must hold whatever fails; what did not.
fn run(scenario: &(impl Fn(Fault) -> Outcome + Sync), fault: Fault) -> Result<(Outcome, Vec<String>), String> {
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| scenario(fault))).map_err(|p| {
        let said = p.downcast_ref::<String>().cloned().or_else(|| p.downcast_ref::<&str>().map(|s| s.to_string())).unwrap_or_default();
        format!("the scenario panicked: {said}")
    })?;
    let mut problems = outcome.problems();
    let faults = outcome.served.faults();
    for p in faults.panics() { problems.push(format!("the control plane panicked {p}")) }
    for (asked, status, body) in faults.answers() {
        if status >= 500 && body.trim().is_empty() { problems.push(format!("{asked} was answered {status} saying nothing")) }
    }
    Ok((outcome, problems))
}

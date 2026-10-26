//! One job's runner, in a container of its own: GitHub's runner image (or GitLab's runner) with a configuration that
//! runs exactly one job; it ends when the job ends, when asked, or at its time bound. The same in the launcher's Worker
//! and in the control plane's (which starts its jobs' containers itself), compiled into each.
use js_sys::{Array, Function, Object, Reflect};
use wasm_bindgen::{JsCast, JsValue};
use worker::*;

/// A Durable Object's id (64 hex digits), not a name.
pub fn is_object_id(s: &str) -> bool { s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()) }

#[durable_object]
pub struct JobRunner { state: State }

impl DurableObject for JobRunner {
    fn new(state: State, _env: Env) -> Self { JobRunner { state } }

    async fn fetch(&self, mut req: Request) -> Result<Response> {
        let container = self.state.container().ok_or("no container for this runner")?;
        let body: serde_json::Value = req.json().await?;
        if req.path() == "/stop" {
            // (A stop Cloudflare fails: said, for the one who asked to try again.)
            if container.running() { if let Err(e) = container.destroy(None).await { return Response::error(format!("the container did not stop: {e}"), 502) } }
            self.state.storage().delete_alarm().await?;
            return Response::from_json(&serde_json::json!({ "stopped": true }))
        }
        let jit = body["jit"].as_str().unwrap_or_default();
        if jit.is_empty() && !body["gitlab"].is_object() { return Response::error("no runner configuration", 400) }
        let name = body["name"].as_str().unwrap_or_default();
        // A start is always a fresh runner: one left from a start that failed half-way goes first.
        if container.running() { let _ = container.destroy(Some("starting again")).await; }
        if let Err(e) = start_runner(&container, jit, &body).await { console_error!("{e}"); return Response::error(e.to_string(), 502) }
        let minutes = body["max_minutes"].as_u64().unwrap_or(70);
        self.state.storage().set_alarm(std::time::Duration::from_secs(minutes * 60)).await?;
        Response::from_json(&serde_json::json!({ "id": name, "kind": "container" }))
    }

    /// The time bound: whatever still runs ends.
    async fn alarm(&self) -> Result<Response> {
        if let Some(c) = self.state.container() { if c.running() { c.destroy(Some("time bound")).await? } }
        Response::ok("ended")
    }
}

/// One field of an object made here (a plain object's field is always set).
fn set(o: &Object, k: &str, v: &JsValue) { Reflect::set(o, &k.into(), v).expect("a field of a plain object"); }

/// One of the container's own operations, as Cloudflare's runtime has them.
fn operation(container: &Container, name: &str) -> Function {
    Reflect::get(container.as_ref(), &name.into()).ok().and_then(|f| f.dyn_into().ok()).expect("an operation of Cloudflare's containers")
}

/// Starts GitHub's runner: `/home/runner/run.sh` with a just-in-time configuration runs exactly one job. The container
/// is sized for the job, from the runner image the Worker names (Cloudflare's per-container scheduling).
async fn start_runner(container: &Container, jit: &str, body: &serde_json::Value) -> Result<()> {
    let options = Object::new();
    if body["gitlab"].is_object() { gitlab_options(&options, body) }
    else {
        // GitHub's runner, with Docker that works here (see superci_core::docker).
        set(&options, "entrypoint", &Array::of3(&"bash".into(), &"-c".into(), &superci_core::docker::start().into()));
        let env = Object::new();
        set(&env, "SUPERCI_JIT", &jit.into());
        set(&env, "SUPERCI_DOCKER", &superci_core::docker::WRAPPER.into());
        // A failing runner (a job nothing here can run): why, for its job-started hook.
        if let Some(why) = body["fail"].as_str() { set(&env, "SUPERCI_FAIL", &why.into()); }
        // GitHub's full image, loaded as it is read (checked again here: it goes into the start script's environment).
        if let Some(i) = body["image_url"].as_str().and_then(superci_core::plane::image_address) { set(&env, "SUPERCI_IMAGE", &i.into()); }
        set(&options, "env", &env);
    }
    set(&options, "enableInternet", &JsValue::TRUE);
    let image = Reflect::get(container.as_ref(), &"images".into()).ok().filter(|i| i.is_object()).and_then(|i| Reflect::get(&i, &"runner".into()).ok()).filter(|i| !i.is_undefined());
    let image = image.ok_or("this Worker names no runner image: update the control plane from the dashboard")?;
    set(&options, "image", &image);
    let n = |k: &str, d: u64| body[k].as_u64().unwrap_or(d) as f64;
    let instance = Object::new();
    set(&instance, "vcpu", &n("cpu", 4).into());
    set(&instance, "memoryMib", &(n("ram_gb", 12) * 1024.0).into());
    set(&instance, "diskMb", &(n("disk_gb", 20) * 1000.0).into());
    set(&options, "instance", &instance);
    // A start that fails (an image or size Cloudflare refuses) says so, rather than leaving the job waiting.
    let started = operation(container, "start").call1(container.as_ref(), &options).map_err(|e| Error::RustError(format!("the container did not start: {}", js_message(&e))))?;
    if let Ok(promise) = started.dyn_into::<js_sys::Promise>() {
        wasm_bindgen_futures::JsFuture::from(promise).await.map_err(|e| Error::RustError(format!("the container did not start: {}", js_message(&e))))?;
    }
    // Cloudflare stops a container soon after its Durable Object goes idle unless told otherwise: it runs until the job
    // ends (or its time bound; Cloudflare's longest is 6 hours).
    let minutes = body["max_minutes"].as_u64().unwrap_or(70).min(360);
    // Cloudflare can answer "temporarily unavailable" right after a start: asked again, three times, a second apart.
    let keep = operation(container, "setInactivityTimeout");
    let mut tries = 0;
    loop {
        let last = match keep.call1(container.as_ref(), &((minutes * 60_000) as f64).into()) {
            Err(e) => js_message(&e),
            Ok(kept) => match kept.dyn_into::<js_sys::Promise>() {
                Ok(promise) => match wasm_bindgen_futures::JsFuture::from(promise).await { Ok(_) => return Ok(()), Err(e) => js_message(&e) },
                Err(_) => return Ok(()),
            },
        };
        tries += 1;
        if tries == 4 { return Err(Error::RustError(format!("the container cannot be kept running: {last}"))) }
        Delay::from(std::time::Duration::from_secs(1)).await
    }
}

/// What a JavaScript error says (its message), not how it is built.
fn js_message(e: &JsValue) -> String {
    match e.dyn_ref::<js_sys::Error>() {
        Some(err) => String::from(err.message()),
        None => e.as_string().unwrap_or_else(|| js_sys::JSON::stringify(e).map(String::from).unwrap_or_else(|_| "an error without a message".into())),
    }
}

/// The containers of one size from before 0.5.0: the class stays (Cloudflare keeps its namespace), with nothing in it.
#[durable_object]
pub struct Runner { _state: State }

impl DurableObject for Runner {
    fn new(state: State, _env: Env) -> Self { Runner { _state: state } }
    async fn fetch(&self, _req: Request) -> Result<Response> { Response::error("superseded by JobRunner", 410) }
}

/// GitLab's runner for one job, in Docker on the container's own network (no bridge networks here). The token and URL go
/// in the environment, not the command line.
fn gitlab_options(options: &Object, body: &serde_json::Value) {
    let gl = &body["gitlab"];
    set(options, "entrypoint", &Array::of3(&"bash".into(), &"-c".into(), &superci_core::gitlab::RUNNER_SCRIPT.into()));
    let env = Object::new();
    set(&env, "GL_URL", &gl["url"].as_str().unwrap_or_default().into());
    set(&env, "GL_TOKEN", &gl["token"].as_str().unwrap_or_default().into());
    set(&env, "DOCKERD_FLAGS", &"--iptables=false --ip6tables=false --ip-forward=false".into());
    set(&env, "GL_RUNNER_FLAGS", &"--docker-network-mode host".into());
    set(options, "env", &env);
}

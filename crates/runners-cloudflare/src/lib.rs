//! A SuperCI launcher on Cloudflare: one container per job (GitHub's runner, registered just in time for that
//! job), started when a control plane asks. It trusts one control plane the way AWS does: a token that control plane
//! signed (ES256; its public keys pinned here when the dashboard set this agent up), for this agent's URL, from its
//! subject, unexpired.
//! Each runner is a Durable Object with a container, sized for its job and placed in eastern North America (near GitHub);
//! it ends when the job ends, when asked, or at its time bound.
use superci_core::crypto::verify_es256_jwt;
use superci_core::plane::CLOUDFLARE_LOCATIONS;
use wasm_bindgen::JsValue;
use worker::*;

mod job_runner;
pub use job_runner::{JobRunner, Runner};
use job_runner::is_object_id;

fn now_secs() -> u64 { (Date::now().as_millis() / 1000) as u64 }

/// The caller is the trusted control plane, or an error saying why not.
fn verify(req: &Request, env: &Env) -> std::result::Result<(), String> {
    let issuer = env.var("TRUSTED_ISSUER").map(|v| v.to_string()).map_err(|_| "no trusted control plane")?;
    let subject = env.var("TRUSTED_SUBJECT").map(|v| v.to_string()).map_err(|_| "no trusted control plane")?;
    let token = req.headers().get("authorization").ok().flatten().and_then(|v| v.strip_prefix("Bearer ").map(str::to_string)).ok_or("no token")?;
    let keys: serde_json::Value = serde_json::from_str(&env.var("TRUSTED_KEYS").map(|v| v.to_string()).map_err(|_| "no trusted keys")?).map_err(|e| e.to_string())?;
    let audience = req.url().expect("a request's own URL").origin().ascii_serialization();
    for key in keys["keys"].as_array().into_iter().flatten() {
        if let Ok(claims) = verify_es256_jwt(&token, key, now_secs()) {
            if claims["iss"] == issuer.as_str() && claims["aud"] == audience.as_str() && claims["sub"] == subject.as_str() { return Ok(()) }
            return Err("token for another agent or control plane".into());
        }
    }
    Err("bad token".into())
}

#[event(fetch)]
async fn fetch(mut req: Request, env: Env, _ctx: Context) -> Result<Response> {
    let path = req.path();
    if req.method() == Method::Get && path == "/health" { return Response::from_json(&serde_json::json!({ "agent": "cloudflare" })) }
    if req.method() != Method::Post || !["/launch", "/stop"].contains(&path.as_str()) { return Response::error("SuperCI launcher: nothing here", 404) }
    if let Err(e) = verify(&req, &env) { return Response::error(format!("not allowed: {e}"), 401) }
    let body: serde_json::Value = req.json().await?;
    let id = if path == "/launch" { body["name"].as_str() } else { body["id"].as_str() }.ok_or("no runner name")?.to_string();
    // Where its container starts, as the control plane says (Cloudflare chooses for "auto"); the hint counts when made.
    let location = body["location"].as_str().unwrap_or("enam").to_string();
    if !CLOUDFLARE_LOCATIONS.iter().any(|(l, _)| *l == location) { return Response::error(format!("Cloudflare has no location {location}"), 400) }
    let namespace = env.durable_object("RUNNER")?;
    // A runner is named at its start; then known by its Durable Object's id, which Cloudflare's usage analytics count
    // its container by (`instanceId`), so its cost can be read as billed.
    let by_id = is_object_id(&id);
    let object = if by_id { namespace.id_from_string(&id)? } else { namespace.id_from_name(&id).expect("a Durable Object named") };
    let stub = if location == "auto" || by_id { object.get_stub().expect("a Durable Object's stub") } else { object.get_stub_with_location_hint(&location).expect("a stub where Cloudflare has a location") };
    let mut init = RequestInit::new();
    init.with_method(Method::Post).with_body(Some(JsValue::from_str(&body.to_string())));
    let mut r = stub.fetch_with_request(Request::new_with_init(&format!("https://runner{path}"), &init).expect("a request to a runner")).await?;
    if path != "/launch" || r.status_code() != 200 { return Ok(r) }
    let mut started: serde_json::Value = r.json().await.expect("a runner's start, said in JSON");
    started["id"] = object.to_string().into();
    Response::from_json(&started)
}

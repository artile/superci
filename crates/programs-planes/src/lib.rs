//! The control planes SuperCI puts in a cloud (AWS, Modal, Cloudflare) and its runner agent for Cloudflare, built, for the superci program to carry.

/// Cloudflare's control plane (a Worker): its script.
pub const WORKER_JS: &[u8] = include_bytes!(env!("WORKER_JS"));
/// and its WebAssembly.
pub const WORKER_WASM: &[u8] = include_bytes!(env!("WORKER_WASM"));
/// Cloudflare's runner agent: its script.
pub const RUNNERS_JS: &[u8] = include_bytes!(env!("RUNNERS_JS"));
/// and its WebAssembly.
pub const RUNNERS_WASM: &[u8] = include_bytes!(env!("RUNNERS_WASM"));
/// AWS's control plane, as Lambda takes it.
pub const PLANE_AWS: &[u8] = include_bytes!(env!("PLANE_AWS"));
/// Modal's control plane.
pub const PLANE_MODAL: &[u8] = include_bytes!(env!("PLANE_MODAL"));

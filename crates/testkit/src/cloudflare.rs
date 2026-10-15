//! The Cloudflare control plane as Cloudflare runs it, on this computer: its Worker and Durable Object, built
//! (`crates/plane-cloudflare/build`, by `./build.sh`), in Cloudflare's own runtime (`wrangler dev --local`), with
//! storage of its own for the test. `None` where `wrangler` is not installed or the Worker is not built.
use std::io::Read;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};

pub struct CloudflarePlane { pub url: String, pub plane: String, pub key: String, pub run_key: String, child: Child, dir: std::path::PathBuf }

impl Drop for CloudflarePlane {
    // (The whole of it: `wrangler` starts Cloudflare's runtime as processes of its own, in its group.)
    fn drop(&mut self) { unsafe { libc::kill(-(self.child.id() as i32), libc::SIGTERM); } let _ = self.child.kill(); let _ = self.child.wait(); let _ = std::fs::remove_dir_all(&self.dir); }
}

pub fn serve_cloudflare() -> Option<CloudflarePlane> {
    let worker = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../plane-cloudflare");
    if !worker.join("build/index.js").exists() { return None }
    let wrangler = std::env::var_os("PATH").and_then(|p| std::env::split_paths(&p).map(|d| d.join("wrangler")).find(|w| w.exists()))?;
    let port = { let l = std::net::TcpListener::bind("127.0.0.1:0").ok()?; l.local_addr().ok()?.port() };
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!("superci-workerd-{}-{}", std::process::id(), N.fetch_add(1, std::sync::atomic::Ordering::SeqCst)));
    let (plane, key, run_key) = ("cftest000001".to_string(), "dashboard-key-for-tests-0001".to_string(), "superci_run_0123456789abcdef0123456789abcdef".to_string());
    let read = format!("{}:runs", superci_core::crypto::sha256_hex(run_key.as_bytes()));
    let child = Command::new(wrangler).current_dir(&worker)
        .args(["dev", "--local", "--ip", "127.0.0.1", "--port", &port.to_string(), "--show-interactive-dev-session=false", "--persist-to", &dir.display().to_string(),
            "--var", &format!("PLANE_ID:{plane}"), "--var", &format!("DASHBOARD_KEY_4102444800_TEST:{key}"), "--var", &format!("READ_KEY_4102444800_AGENT:{read}")])
        .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped()).process_group(0).spawn().ok()?;
    let p = CloudflarePlane { url: format!("http://127.0.0.1:{port}"), plane, key, run_key, child, dir };
    // Up once it answers (its first answer builds the Worker in the runtime: a few seconds).
    let healthy = || -> bool {
        use std::io::Write;
        let Ok(mut s) = std::net::TcpStream::connect(("127.0.0.1", port)) else { return false };
        let _ = s.set_read_timeout(Some(std::time::Duration::from_secs(10)));
        let _ = s.write_all(b"GET /health HTTP/1.1\r\nhost: 127.0.0.1\r\nconnection: close\r\n\r\n");
        let mut out = String::new();
        let _ = s.read_to_string(&mut out);
        out.starts_with("HTTP/1.1 200")
    };
    for _ in 0..120 {
        if healthy() { return Some(p) }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    let mut p = p;
    let mut said = String::new();
    if let Some(mut e) = p.child.stderr.take() { let _ = p.child.kill(); let _ = e.read_to_string(&mut said); }
    panic!("the Cloudflare control plane did not answer in Cloudflare's runtime: {said}");
}

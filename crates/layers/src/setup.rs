//! A machine's switch to SuperCI's layers, at its start and before its runner: checked first, and on anything
//! that fails, put back as it was (Docker as it comes) before the job runs. It never fails the start.
//!
//! 1. Docker keeps its images in containerd already (Docker 29 does by default; turning that on for an older Docker
//!    would change what Docker is like, so this does not).
//! 2. The kernel mounts an EROFS file (a small one carried here).
//! 3. The store's disk, when the machine has one, made and mounted for the layer files; the index fetched.
//! 4. This service started under systemd, and answering.
//! 5. containerd's and Docker's settings kept, then changed: containerd's own content store off, this service as
//!    its snapshotter and content store, Docker on it; both started again.
//! 6. Docker says it uses this service, and containerd reaches both of its parts.

use std::path::{Path, PathBuf};
use std::process::Command;

const CHECK_EROFS: &[u8] = include_bytes!("../assets/check.erofs");
const CONTAINERD_CONFIG: &str = "/etc/containerd/config.toml";
const DOCKER_CONFIG: &str = "/etc/docker/daemon.json";
const KEPT: &str = ".superci-stock";
const UNIT: &str = "/etc/systemd/system/superci-layers.service";
const BIN: &str = "/usr/local/bin/superci-layers";
const SOCKET: &str = "/run/superci-layers.sock";
const INDEX: &str = "/etc/superci/layers.json";

pub struct Options {
    /// Where the index is fetched (the machine's own link to its control plane).
    pub index_url: String,
    /// The store's own disk: a device, `auto` (an EBS disk that is not the root's), or `none`.
    pub disk: String,
    pub root: PathBuf,
}

/// The switch, or the reason it was not made (and everything put back).
pub fn setup(o: &Options) -> i32 {
    let began = std::time::Instant::now();
    match switch(o) {
        Ok(said) => println!("superci-layers: on in {:.1} s: {said}", began.elapsed().as_secs_f64()),
        Err(e) => {
            println!("superci-layers: off: {e}");
            if let Err(e) = put_back() { println!("superci-layers: putting Docker back as it was: {e}") }
        }
    }
    0
}

/// Docker as it comes again (by hand, or after a failed switch).
pub fn off() -> i32 {
    match put_back() { Ok(()) => { println!("superci-layers: off; Docker as it comes"); 0 } Err(e) => { println!("superci-layers: {e}"); 1 } }
}

fn run(cmd: &str, args: &[&str]) -> Result<String, String> {
    let out = Command::new(cmd).args(args).output().map_err(|e| format!("{cmd}: {e}"))?;
    if out.status.success() { Ok(String::from_utf8_lossy(&out.stdout).trim().to_string()) } else {
        Err(format!("{cmd} {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim().lines().last().unwrap_or("failed")))
    }
}

fn switch(o: &Options) -> Result<String, String> {
    // 1. Docker's images in containerd already.
    let status = run("docker", &["info", "--format", "{{json .DriverStatus}}"])?;
    if !status.contains("io.containerd.snapshotter.v1") { return Err("Docker keeps its images in its own driver, not containerd (Docker before 29)".into()) }
    // 2. EROFS from a file.
    check_erofs(&o.root)?;
    // 3. The files' disk, and the index.
    let files = o.root.join("files");
    std::fs::create_dir_all(&files).map_err(|e| format!("{}: {e}", files.display()))?;
    let disk = store_disk(&o.disk, &files)?;
    let index = fetch_index(&o.index_url)?;
    // 4. The service.
    std::fs::write(UNIT, unit(&o.root, &files)).map_err(|e| format!("{UNIT}: {e}"))?;
    run("systemctl", &["daemon-reload"])?;
    run("systemctl", &["start", "superci-layers"])?;
    // 5. containerd and Docker on it, their own settings kept for putting back.
    keep(CONTAINERD_CONFIG)?;
    keep(DOCKER_CONFIG)?;
    let config = std::fs::read_to_string(CONTAINERD_CONFIG).unwrap_or_default();
    std::fs::write(CONTAINERD_CONFIG, containerd_config(&config)?).map_err(|e| format!("{CONTAINERD_CONFIG}: {e}"))?;
    let daemon = std::fs::read_to_string(DOCKER_CONFIG).unwrap_or_else(|_| "{}".into());
    std::fs::write(DOCKER_CONFIG, docker_config(&daemon)?).map_err(|e| format!("{DOCKER_CONFIG}: {e}"))?;
    run("systemctl", &["restart", "containerd"])?;
    run("systemctl", &["restart", "docker"])?;
    wait_for_docker()?;
    // 6. Checks.
    let driver = run("docker", &["info", "--format", "{{.Driver}}"])?;
    if driver != "superci" { return Err(format!("Docker's driver is {driver:?} after the switch")) }
    check_containerd()?;
    Ok(format!("{index} layers in the index; files on {disk}"))
}

/// The small EROFS file mounted and read.
fn check_erofs(root: &Path) -> Result<(), String> {
    let dir = root.join("check");
    std::fs::create_dir_all(dir.join("mnt")).map_err(|e| e.to_string())?;
    let file = dir.join("check.erofs");
    std::fs::write(&file, CHECK_EROFS).map_err(|e| e.to_string())?;
    let mnt = dir.join("mnt");
    crate::mount::mount_erofs(&file, &mnt).map_err(|e| format!("the kernel does not mount an EROFS file: {e}"))?;
    let read = std::fs::read_to_string(mnt.join("ok"));
    let _ = crate::mount::unmount(&mnt);
    let _ = std::fs::remove_dir_all(&dir);
    match read { Ok(t) if t == "superci-layers\n" => Ok(()), Ok(_) => Err("an EROFS file read wrong".into()), Err(e) => Err(format!("an EROFS file could not be read: {e}")) }
}

/// The store's own disk for the layer files: made (ext4, no journal: it holds what can be fetched again) and
/// mounted; with none, the files are kept on the machine's own disk.
fn store_disk(disk: &str, files: &Path) -> Result<String, String> {
    let dev = match disk {
        "none" => return Ok("the machine's disk".into()),
        "auto" => match unused_ebs()? { Some(d) => d, None => return Ok("the machine's disk".into()) },
        d => d.to_string(),
    };
    run("mkfs.ext4", &["-q", "-F", "-E", "lazy_itable_init=1,lazy_journal_init=1", "-O", "^has_journal", &dev])?;
    run("mount", &["-o", "noatime", &dev, &files.to_string_lossy()])?;
    Ok(dev)
}

/// An EBS disk that is not the root's and has nothing on it (found by its model, not its name: NVMe names it).
fn unused_ebs() -> Result<Option<String>, String> {
    let root_src = run("findmnt", &["-no", "SOURCE", "/"])?;
    let root = run("lsblk", &["-no", "PKNAME", &root_src]).unwrap_or_default();
    let list = run("lsblk", &["-dnpo", "NAME,MODEL"])?;
    for line in list.lines() {
        let (name, model) = line.split_once(' ').unwrap_or((line, ""));
        if !model.contains("Amazon Elastic Block Store") || name.ends_with(&format!("/{}", root.lines().next().unwrap_or("-"))) || root_src.starts_with(name) { continue }
        // (Nothing mounted from it, no partitions.)
        let children = run("lsblk", &["-nro", "NAME,MOUNTPOINT", name]).unwrap_or_default();
        if children.lines().count() == 1 && children.split_whitespace().nth(1).is_none() { return Ok(Some(name.to_string())) }
    }
    Ok(None)
}

fn fetch_index(url: &str) -> Result<usize, String> {
    let agent = crate::fetch::agent();
    let res = agent.get(url).call().map_err(|e| format!("the index: {e}"))?;
    if res.status().as_u16() != 200 { return Err(format!("the index: the control plane answered {}", res.status())) }
    let text = res.into_body().with_config().limit(64 << 20).read_to_string().map_err(|e| format!("the index: {e}"))?;
    let index: crate::index::Index = serde_json::from_str(&text).map_err(|e| format!("the index: {e}"))?;
    if index.layers.is_empty() { return Err("the store has no layers for this machine yet".into()) }
    std::fs::create_dir_all(Path::new(INDEX).parent().unwrap()).map_err(|e| e.to_string())?;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(INDEX).map_err(|e| format!("{INDEX}: {e}"))?;
    std::io::Write::write_all(&mut f, text.as_bytes()).map_err(|e| format!("{INDEX}: {e}"))?;
    Ok(index.layers.len())
}

fn unit(root: &Path, files: &Path) -> String {
    format!("[Unit]\nDescription=SuperCI's image layers (containerd's snapshotter and content store)\nBefore=containerd.service\n\n[Service]\nType=notify\nExecStart={BIN} --root={} --files={} --index={INDEX} --socket={SOCKET}\nRestart=always\nRestartSec=1\n\n[Install]\nWantedBy=multi-user.target\n", root.display(), files.display())
}

/// A setting file kept beside it as it was (once: a second switch keeps the first one's), or that there was none.
fn keep(path: &str) -> Result<(), String> {
    let kept = format!("{path}{KEPT}");
    if Path::new(&kept).exists() || Path::new(&format!("{kept}-none")).exists() { return Ok(()) }
    match std::fs::copy(path, &kept) {
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => std::fs::write(format!("{kept}-none"), b"").map_err(|e| e.to_string()),
        Err(e) => Err(format!("{path}: {e}")),
    }
}

/// containerd's settings with its own content store off and this service as its snapshotter and content store:
/// added to what is there, its other settings kept.
pub fn containerd_config(config: &str) -> Result<String, String> {
    if config.contains("[proxy_plugins") { return Err("containerd's settings have proxy plugins of their own".into()) }
    let entry = "\"io.containerd.content.v1.content\"";
    let mut out = String::new();
    let mut done = false;
    let mut in_table = false;
    for line in config.lines() {
        let t = line.trim_start();
        if t.starts_with('[') { in_table = true }
        if !in_table && !done && t.starts_with("disabled_plugins") {
            let Some((head, list)) = line.split_once('[') else { return Err("containerd's disabled_plugins is not a list on one line".into()) };
            let inner = list.trim_end().strip_suffix(']').ok_or("containerd's disabled_plugins is not a list on one line")?.trim();
            let joined = if inner.is_empty() { entry.to_string() } else if inner.contains(entry) { inner.to_string() } else { format!("{inner}, {entry}") };
            out.push_str(&format!("{head}[{joined}]\n"));
            done = true;
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    // (None there: said before the first table, where a top-level key belongs.)
    if !done {
        let first_table = out.lines().position(|l| l.trim_start().starts_with('['));
        let line = format!("disabled_plugins = [{entry}]");
        out = match first_table {
            Some(i) => { let mut ls: Vec<&str> = out.lines().collect(); ls.insert(i, &line); ls.join("\n") + "\n" }
            None => format!("{line}\n{out}"),
        };
    }
    out.push_str(&format!("\n[proxy_plugins]\n  [proxy_plugins.superci]\n    type = \"snapshot\"\n    address = \"{SOCKET}\"\n  [proxy_plugins.superci-content]\n    type = \"content\"\n    address = \"{SOCKET}\"\n"));
    Ok(out)
}

/// Docker's settings with this service as its snapshotter (its other settings kept).
pub fn docker_config(daemon: &str) -> Result<String, String> {
    let mut v: serde_json::Value = serde_json::from_str(if daemon.trim().is_empty() { "{}" } else { daemon }).map_err(|e| format!("{DOCKER_CONFIG}: {e}"))?;
    let o = v.as_object_mut().ok_or(format!("{DOCKER_CONFIG} is not an object"))?;
    o.insert("storage-driver".into(), "superci".into());
    let features = o.entry("features").or_insert_with(|| serde_json::json!({}));
    features.as_object_mut().ok_or("Docker's features are not an object")?.insert("containerd-snapshotter".into(), true.into());
    serde_json::to_string_pretty(&v).map_err(|e| e.to_string())
}

fn wait_for_docker() -> Result<(), String> {
    for _ in 0..120 {
        if run("docker", &["info", "--format", "{{.Driver}}"]).is_ok() { return Ok(()) }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    Err("Docker did not answer after the switch".into())
}

/// containerd reaches this service for snapshots (a list) and for content (a blob that is not there: not found,
/// not unreachable).
fn check_containerd() -> Result<(), String> {
    use crate::containerd::services::{content::v1 as capi, snapshots::v1 as sapi};
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|e| e.to_string())?;
    rt.block_on(async {
        let ch = crate::client::channel(Path::new("/run/containerd/containerd.sock")).await?;
        fn ns<T>(m: T) -> tonic::Request<T> { let mut r = tonic::Request::new(m); r.metadata_mut().insert("containerd-namespace", tonic::metadata::MetadataValue::from_static("moby")); r }
        let mut list = sapi::snapshots_client::SnapshotsClient::new(ch.clone()).list(ns(sapi::ListSnapshotsRequest { snapshotter: "superci".into(), filters: vec![] })).await.map_err(|e| format!("containerd's snapshots from superci-layers: {}", e.message()))?.into_inner();
        while list.message().await.map_err(|e| format!("containerd's snapshots from superci-layers: {}", e.message()))?.is_some() {}
        let empty = crate::content::Digest::of(crate::content::Algorithm::Sha256, b"").to_string();
        match capi::content_client::ContentClient::new(ch).info(ns(capi::InfoRequest { digest: empty })).await {
            Ok(_) => Ok(()),
            Err(e) if e.code() == tonic::Code::NotFound => Ok(()),
            Err(e) => Err(format!("containerd's content from superci-layers: {}", e.message())),
        }
    })
}

/// containerd's and Docker's settings as they were, this service stopped, both started again, Docker answering.
fn put_back() -> Result<(), String> {
    for path in [CONTAINERD_CONFIG, DOCKER_CONFIG] {
        let kept = format!("{path}{KEPT}");
        if Path::new(&kept).exists() { std::fs::rename(&kept, path).map_err(|e| format!("{path}: {e}"))? }
        else if Path::new(&format!("{kept}-none")).exists() { let _ = std::fs::remove_file(path); let _ = std::fs::remove_file(format!("{kept}-none")); }
    }
    let _ = run("systemctl", &["stop", "superci-layers"]);
    let _ = std::fs::remove_file(UNIT);
    let _ = run("systemctl", &["daemon-reload"]);
    run("systemctl", &["restart", "containerd"])?;
    run("systemctl", &["restart", "docker"])?;
    wait_for_docker()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn containerds_settings_keep_theirs_and_add_the_proxies() {
        let got = containerd_config("disabled_plugins = [\"cri\"]\n").unwrap();
        assert!(got.starts_with("disabled_plugins = [\"cri\", \"io.containerd.content.v1.content\"]\n"), "{got}");
        assert!(got.contains("[proxy_plugins.superci]\n    type = \"snapshot\"\n    address = \"/run/superci-layers.sock\"") && got.contains("[proxy_plugins.superci-content]\n    type = \"content\""));
        // A list with none, and a version line before; a key of a table named the same is not touched.
        let got = containerd_config("version = 3\ndisabled_plugins = []\n[plugins.x]\n  disabled_plugins = [\"y\"]\n").unwrap();
        assert!(got.contains("disabled_plugins = [\"io.containerd.content.v1.content\"]\n[plugins.x]\n  disabled_plugins = [\"y\"]"), "{got}");
        // None there: before the first table.
        let got = containerd_config("version = 3\n[plugins.x]\n  a = 1\n").unwrap();
        assert!(got.starts_with("version = 3\ndisabled_plugins = [\"io.containerd.content.v1.content\"]\n[plugins.x]"), "{got}");
        assert!(containerd_config("").unwrap().starts_with("disabled_plugins = [\"io.containerd.content.v1.content\"]\n"));
        // Twice is the same as once; proxy plugins of its own are left alone (the switch is not made).
        assert_eq!(containerd_config("disabled_plugins = [\"cri\", \"io.containerd.content.v1.content\"]\n").unwrap().matches("io.containerd.content.v1.content").count(), 1);
        assert!(containerd_config("[proxy_plugins.other]\n").is_err());
    }

    #[test]
    fn dockers_settings_keep_theirs() {
        let got: serde_json::Value = serde_json::from_str(&docker_config(r#"{"max-concurrent-downloads": 16, "features": {"x": true}}"#).unwrap()).unwrap();
        assert_eq!(got, serde_json::json!({"max-concurrent-downloads": 16, "storage-driver": "superci", "features": {"x": true, "containerd-snapshotter": true}}));
        assert_eq!(serde_json::from_str::<serde_json::Value>(&docker_config("").unwrap()).unwrap()["storage-driver"], "superci");
        assert!(docker_config("[]").is_err());
    }

    #[test]
    fn the_check_file_is_an_erofs_image() {
        // (EROFS's magic number at 1024.)
        assert_eq!(&CHECK_EROFS[1024..1028], &0xE0F5E1E2u32.to_le_bytes());
    }
}

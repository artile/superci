//! A machine's switch to SuperCI's layers, at its start, in the start script's place for starting Docker again: what
//! comes before the runner takes a fraction of a second, and Docker starts once, beside the runner, as it would have
//! anyway (a job's first use of it waits on its socket, as it does without this). On anything that fails, Docker as
//! it comes; it never fails the start.
//!
//! Before the runner (`setup`), at once where they can be:
//! 1. Docker keeps its images in containerd already (Docker 29 does by default: its installed package says, not the
//!    daemon, which is starting; turning that on for an older Docker would change what Docker is like).
//! 2. The kernel mounts an EROFS file (a small one carried here).
//! 3. The store's disk, when the machine has one, made and mounted for the layer files; the index fetched.
//! 4. This service started under systemd (a unit of its own while the machine runs), and answering.
//! 5. containerd's and Docker's settings kept, then changed: containerd's own content store off, this service as
//!    its snapshotter and content store, Docker on it.
//!
//! Beside the runner (`finish`, on its own): containerd and Docker started again, then Docker says it uses this
//! service and containerd reaches both of its parts; if not, their settings put back and both started again.

use std::path::{Path, PathBuf};
use std::process::Command;

const CHECK_EROFS: &[u8] = include_bytes!("../assets/check.erofs");
const CONTAINERD_CONFIG: &str = "/etc/containerd/config.toml";
const DOCKER_CONFIG: &str = "/etc/docker/daemon.json";
const KEPT: &str = ".superci-stock";
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
        Ok(said) => {
            println!("superci-layers: switched in {:.2} s: {said}; containerd and Docker start again beside the runner", began.elapsed().as_secs_f64());
            if let Err(e) = detached(&[std::env::current_exe().map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|_| BIN.into()).as_str(), "setup-finish"]) {
                println!("superci-layers: off: {e}");
                let _ = put_back_before_restart();
            }
        }
        Err(e) => {
            println!("superci-layers: off: {e}");
            if let Err(e) = put_back_before_restart() { println!("superci-layers: putting Docker back as it was: {e}") }
        }
    }
    0
}

/// After the switch, beside the runner: containerd and Docker started again on this service (once, as the start
/// script would have started Docker), then checked; Docker as it comes again if it did not take.
pub fn finish() -> i32 {
    let began = std::time::Instant::now();
    let done = (|| -> Result<(), String> {
        run("systemctl", &["restart", "containerd", "docker"])?;
        wait_for_docker()?;
        let driver = docker_info()?["Driver"].as_str().unwrap_or_default().to_string();
        if driver != "superci" { return Err(format!("Docker's driver is {driver:?} after the switch")) }
        check_containerd()
    })();
    match done {
        Ok(()) => println!("superci-layers: on: Docker answered on it {:.1} s after the switch", began.elapsed().as_secs_f64()),
        Err(e) => {
            println!("superci-layers: off: {e}");
            if let Err(e) = put_back() { println!("superci-layers: putting Docker back as it was: {e}") }
        }
    }
    0
}

/// A program started on its own (its own session, its output in the log), not waited for.
fn detached(argv: &[&str]) -> Result<(), String> {
    use std::os::unix::process::CommandExt;
    let log = std::fs::OpenOptions::new().create(true).append(true).open("/var/log/superci-layers.log").map_err(|e| e.to_string())?;
    let mut c = Command::new(argv[0]);
    c.args(&argv[1..]).stdin(std::process::Stdio::null()).stdout(log.try_clone().map_err(|e| e.to_string())?).stderr(log);
    unsafe { c.pre_exec(|| { libc::setsid(); Ok(()) }); }
    c.spawn().map(|_| ()).map_err(|e| format!("{}: {e}", argv[0]))
}

/// Before containerd and Docker were started again: the settings as they were, this service stopped, and Docker
/// started again beside the runner as the start script starts it.
fn put_back_before_restart() -> Result<(), String> {
    restore_settings()?;
    let _ = run("systemctl", &["stop", "superci-layers"]);
    detached(&["systemctl", "restart", "docker"])
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
    // (Each step's time, said with the outcome: the switch is before the runner, so its time is the job's.)
    let mut took: Vec<(&str, f64)> = vec![];
    let mut clock = std::time::Instant::now();
    let mut step = |name: &'static str, took: &mut Vec<(&str, f64)>| { took.push((name, clock.elapsed().as_secs_f64())); clock = std::time::Instant::now(); };
    // 1-3 at once, each timed: Docker keeps its images in containerd already; an EROFS file mounts (its module
    // loaded, the first time); the files' disk made; the index fetched.
    let files = o.root.join("files");
    std::fs::create_dir_all(&files).map_err(|e| format!("{}: {e}", files.display()))?;
    let timed = |f: &dyn Fn() -> Result<String, String>| { let t = std::time::Instant::now(); (f(), t.elapsed().as_secs_f64()) };
    let (docker, erofs, disk, index) = std::thread::scope(|s| {
        let docker = s.spawn(|| timed(&|| docker_keeps_images_in_containerd().map(|_| String::new())));
        let erofs = s.spawn(|| timed(&|| check_erofs(&o.root).map(|_| String::new())));
        let index = s.spawn(|| timed(&|| fetch_index(&o.index_url).map(|n| n.to_string())));
        let disk = timed(&|| store_disk(&o.disk, &files));
        let stopped = |what: &str| (Err(format!("{what} stopped")), 0.0);
        (docker.join().unwrap_or_else(|_| stopped("Docker's check")), erofs.join().unwrap_or_else(|_| stopped("the EROFS check")), disk, index.join().unwrap_or_else(|_| stopped("fetching the index")))
    });
    docker.0?;
    erofs.0?;
    let (disk_t, index_t) = (disk.1, index.1);
    let (disk, index) = (disk.0?, index.0?);
    step("checks, disk and index at once", &mut took);
    // (Each of the four, said after the time they took together.)
    took.extend([("of which Docker's check", docker.1), ("EROFS", erofs.1), ("disk", disk_t), ("index", index_t)]);
    // 4. The service: a unit of its own while the machine runs (no unit file, so no reload of systemd's).
    let (root, files_arg) = (format!("--root={}", o.root.display()), format!("--files={}", files.display()));
    run("systemd-run", &["--unit=superci-layers", "--service-type=notify", "--property=Restart=always", "--property=RestartSec=1", "--property=TimeoutStartSec=30",
        "--description=SuperCI's image layers (containerd's snapshotter and content store)", BIN, &root, &files_arg, &format!("--index={INDEX}"), &format!("--socket={SOCKET}")])?;
    step("service", &mut took);
    // 5. containerd and Docker on it, their own settings kept for putting back; both started again at once
    // (systemd starts Docker after containerd).
    keep(CONTAINERD_CONFIG)?;
    keep(DOCKER_CONFIG)?;
    let config = std::fs::read_to_string(CONTAINERD_CONFIG).unwrap_or_default();
    std::fs::write(CONTAINERD_CONFIG, containerd_config(&config)?).map_err(|e| format!("{CONTAINERD_CONFIG}: {e}"))?;
    let daemon = std::fs::read_to_string(DOCKER_CONFIG).unwrap_or_else(|_| "{}".into());
    std::fs::write(DOCKER_CONFIG, docker_config(&daemon)?).map_err(|e| format!("{DOCKER_CONFIG}: {e}"))?;
    step("settings", &mut took);
    // 6. (Started again and checked beside the runner: `finish`.)
    let times = took.iter().map(|(n, t)| format!("{n} {t:.2} s")).collect::<Vec<_>>().join(", ");
    Ok(format!("{index} layers in the index; files on {disk} ({times})"))
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
/// mounted; with none (`auto`: a repository whose images fit), memory, a quarter of it at most (a file that would
/// not fit leaves its layer to be pulled as usual); `none`, the machine's own disk.
fn store_disk(disk: &str, files: &Path) -> Result<String, String> {
    let dev = match disk {
        "none" => return Ok("the machine's disk".into()),
        "auto" => match unused_ebs()? {
            Some(d) => d,
            None => { run("mount", &["-t", "tmpfs", "-o", "size=25%,mode=0700", "tmpfs", &files.to_string_lossy()])?; return Ok("memory".into()) }
        },
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

/// Whether Docker keeps its images in containerd: Docker 29 or later (its installed package says) with no setting
/// that turns that off. Not asked of the daemon: at a machine's start it is starting (the script starts it again,
/// beside the runner), and waiting for it would be the job's time. A Docker installed some other way is asked.
fn docker_keeps_images_in_containerd() -> Result<(), String> {
    let refused = || Err("Docker keeps its images in its own driver, not containerd (Docker before 29)".to_string());
    let daemon: serde_json::Value = std::fs::read_to_string(DOCKER_CONFIG).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    if daemon["features"]["containerd-snapshotter"] == false || daemon["storage-driver"].as_str().is_some_and(|d| d != "overlayfs") { return refused() }
    match docker_version(&std::fs::read_to_string("/var/lib/dpkg/status").unwrap_or_default()) {
        Some(major) if major >= 29 => Ok(()),
        Some(_) => refused(),
        None => if docker_info()?["DriverStatus"].to_string().contains("io.containerd.snapshotter.v1") { Ok(()) } else { refused() },
    }
}

/// Docker's major version from dpkg's record of installed packages (`docker-ce`, else `docker.io`).
fn docker_version(status: &str) -> Option<u32> {
    for package in ["docker-ce", "docker.io"] {
        for block in status.split("\n\n") {
            if !block.lines().any(|l| l == format!("Package: {package}")) || !block.lines().any(|l| l.starts_with("Status:") && l.ends_with(" installed")) { continue }
            let v = block.lines().find_map(|l| l.strip_prefix("Version: "))?;
            // ("5:29.4.2-1~ubuntu.26.04~resolute": the epoch, then the version.)
            let v = v.split_once(':').map(|(_, v)| v).unwrap_or(v);
            return v.split('.').next()?.parse().ok();
        }
    }
    None
}

/// What Docker's daemon says of itself (`GET /info` on its socket): asked of the daemon, not of the `docker`
/// command, which a machine just started reads from a disk still coming from its snapshot (seconds, the first time).
fn docker_info() -> Result<serde_json::Value, String> {
    use std::io::{Read, Write};
    let mut s = std::os::unix::net::UnixStream::connect("/var/run/docker.sock").map_err(|e| format!("Docker's socket: {e}"))?;
    s.set_read_timeout(Some(std::time::Duration::from_secs(30))).map_err(|e| e.to_string())?;
    // (HTTP/1.0: the answer whole, then the connection closed; no chunks.)
    s.write_all(b"GET /info HTTP/1.0\r\nHost: docker\r\n\r\n").map_err(|e| format!("Docker's socket: {e}"))?;
    let mut answer = Vec::new();
    s.read_to_end(&mut answer).map_err(|e| format!("Docker's socket: {e}"))?;
    let text = String::from_utf8_lossy(&answer);
    let (head, body) = text.split_once("\r\n\r\n").ok_or("Docker's daemon answered no HTTP")?;
    if !head.starts_with("HTTP/1.0 200") && !head.starts_with("HTTP/1.1 200") { return Err(format!("Docker's daemon answered {}", head.lines().next().unwrap_or_default())) }
    serde_json::from_str(body).map_err(|e| format!("Docker's daemon answered: {e}"))
}

fn wait_for_docker() -> Result<(), String> {
    for _ in 0..120 {
        if docker_info().is_ok() { return Ok(()) }
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

/// containerd's and Docker's settings as they were (kept by `keep`).
fn restore_settings() -> Result<(), String> {
    for path in [CONTAINERD_CONFIG, DOCKER_CONFIG] {
        let kept = format!("{path}{KEPT}");
        if Path::new(&kept).exists() { std::fs::rename(&kept, path).map_err(|e| format!("{path}: {e}"))? }
        else if Path::new(&format!("{kept}-none")).exists() { let _ = std::fs::remove_file(path); let _ = std::fs::remove_file(format!("{kept}-none")); }
    }
    Ok(())
}

/// containerd's and Docker's settings as they were, this service stopped, both started again, Docker answering.
fn put_back() -> Result<(), String> {
    restore_settings()?;
    // (Its unit is the machine's while it runs: stopped, it is gone.)
    let _ = run("systemctl", &["stop", "superci-layers"]);
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
    fn dockers_version_is_read_from_dpkgs_record() {
        let status = "Package: containerd.io\nStatus: install ok installed\nVersion: 2.3.6-1\n\nPackage: docker-ce\nStatus: install ok installed\nVersion: 5:29.4.2-1~ubuntu.26.04~resolute\n\nPackage: x\nVersion: 1\n";
        assert_eq!(docker_version(status), Some(29));
        assert_eq!(docker_version("Package: docker.io\nStatus: install ok installed\nVersion: 28.2.2-0ubuntu1\n"), Some(28));
        assert_eq!(docker_version("Package: docker-ce\nStatus: deinstall ok config-files\nVersion: 5:29.0.0-1\n"), None, "removed is not installed");
        assert_eq!(docker_version(""), None);
    }

    #[test]
    fn dockers_daemon_says_its_driver_on_its_socket() {
        // (Where Docker runs: a developer's machine, CI.)
        if !Path::new("/var/run/docker.sock").exists() { return }
        let info = docker_info().unwrap();
        assert!(info["Driver"].as_str().is_some_and(|d| !d.is_empty()) && info["DriverStatus"].is_array(), "{info}");
    }

    #[test]
    fn the_check_file_is_an_erofs_image() {
        // (EROFS's magic number at 1024.)
        assert_eq!(&CHECK_EROFS[1024..1028], &0xE0F5E1E2u32.to_le_bytes());
    }
}

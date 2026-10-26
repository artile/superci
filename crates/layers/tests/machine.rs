//! superci-layers where it runs: on a machine as SuperCI's are, recreated here as a privileged container (Ubuntu 26.04,
//! systemd as its first process, Docker from Docker's packages), the program built here mounted in. `setup` switches
//! the machine's Docker to it (systemd starting it as a service), Docker then runs as it does on it, `off` puts it back;
//! and the end-to-end suite (`e2e/docker.sh`: Docker on it against Docker as it comes) runs too. What the program does in
//! there is measured as any test's, where something measures it (its settings carried into the container).
//! Only where Docker runs Linux containers of this computer's own kind (a Linux computer: the program built here is
//! what runs in them); skipped elsewhere.
use std::process::{Command, Output};

fn docker(args: &[&str]) -> Output { Command::new("docker").args(args).output().unwrap_or_else(|e| panic!("docker {args:?}: {e}")) }

fn ok(args: &[&str]) -> String {
    let o = docker(args);
    assert!(o.status.success(), "docker {args:?}: {}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
    String::from_utf8_lossy(&o.stdout).into_owned()
}

/// The program the containers run: the one built here, or a Linux build of it named by `SUPERCI_LAYERS_LINUX_BIN` (to
/// run these on a computer whose Docker runs Linux containers in a VM).
fn program() -> String { std::env::var("SUPERCI_LAYERS_LINUX_BIN").unwrap_or_else(|_| env!("CARGO_BIN_EXE_superci-layers").to_string()) }

/// Whether the containers can run that program.
fn here() -> bool {
    if !cfg!(target_os = "linux") && std::env::var_os("SUPERCI_LAYERS_LINUX_BIN").is_none() { eprintln!("not a Linux computer: skipped"); return false }
    if !Command::new("docker").args(["info", "-f", "{{.OSType}}"]).output().is_ok_and(|o| o.status.success()) { eprintln!("no Docker here: skipped"); return false }
    true
}

/// What measures the tests, carried into a container: its settings, and (`mounts`, for starting one) its folder
/// mounted at the same path.
fn measured_with(mounts: bool) -> Vec<String> {
    let mut args = vec![];
    for (k, v) in std::env::vars().filter(|(k, _)| k.starts_with("SUPERCOV_")) {
        if k.ends_with("_DIR") && mounts { std::fs::create_dir_all(&v).ok(); args.extend(["-v".to_string(), format!("{v}:{v}")]) }
        args.extend(["-e".to_string(), format!("{k}={v}")]);
    }
    args
}
fn measured() -> Vec<String> { measured_with(true) }
/// The same for a command in a container already started with them (`docker exec` takes no mounts).
fn measured_env() -> Vec<String> { measured_with(false) }

fn image(file: &str, tag: &str) {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/e2e");
    ok(&["build", "-q", "-f", &format!("{dir}/{file}"), "-t", tag, dir]);
}

/// The store's folder on this computer, removed when dropped: through a container, as what the machine wrote there
/// as root this user may not remove (nor read: Cargo's folder is cached as a whole).
struct Store(std::path::PathBuf);
impl Drop for Store {
    fn drop(&mut self) {
        let _ = docker(&["run", "--rm", "-v", &format!("{}:/store", self.0.display()), "--entrypoint", "sh", "superci-layers-machine", "-c", "umount -R /store/* 2>/dev/null; rm -rf /store/* /store/.[!.]*"]);
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A machine, running until dropped.
struct Machine(String);
impl Drop for Machine { fn drop(&mut self) { let _ = docker(&["rm", "-f", &self.0]); } }
impl Machine {
    fn exec(&self, env: &[String], argv: &[&str]) -> Output {
        let mut args: Vec<String> = vec!["exec".into()];
        args.extend(env.iter().cloned());
        args.push(self.0.clone());
        args.extend(argv.iter().map(|s| s.to_string()));
        docker(&args.iter().map(String::as_str).collect::<Vec<_>>())
    }
    fn sh(&self, script: &str) -> String {
        let o = self.exec(&[], &["sh", "-c", script]);
        assert!(o.status.success(), "{script}: {}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
        String::from_utf8_lossy(&o.stdout).trim().to_string()
    }
}

impl Machine {
    /// Docker answering with `driver`.
    fn docker_on(&self, driver: &str) {
        self.sh(&format!("for i in $(seq 240); do [ \"$(docker info -f '{{{{.Driver}}}}' 2>/dev/null)\" = '{driver}' ] && exit 0; sleep 0.5; done; cat /var/log/superci-layers.log; docker info -f '{{{{.Driver}}}}'; exit 1"));
    }

    /// `setup` as a machine's start runs it (`env` besides what measures it), then what it left said: switched (by
    /// `setup-finish`, which it starts on its own), or not. What it printed, and that state.
    fn switch(&self, env: &[String], args: &[&str]) -> (String, String) {
        self.sh("rm -f /run/superci-layers.state");
        let mut e = measured_env();
        e.extend(env.iter().cloned());
        let o = self.exec(&e, &[&["/usr/local/bin/superci-layers", "setup"][..], args].concat());
        let said = String::from_utf8_lossy(&o.stdout).into_owned() + &String::from_utf8_lossy(&o.stderr);
        assert!(o.status.success() && !said.contains("panicked"), "setup {args:?}: {said}");
        self.sh("for i in $(seq 240); do [ -s /run/superci-layers.state ] && exit 0; sleep 0.5; done; cat /var/log/superci-layers.log; exit 1");
        (said, self.sh("cat /run/superci-layers.state"))
    }

    /// Docker as it comes again (`off`), answering on its own driver.
    fn back(&self, stock: &str) {
        let o = self.exec(&measured_env(), &["/usr/local/bin/superci-layers", "off"]);
        assert!(o.status.success(), "off: {}", String::from_utf8_lossy(&o.stdout));
        self.docker_on(stock);
    }
}

/// A machine as SuperCI's are, its store on a disk of this computer: up, with Docker as it comes (its driver said),
/// and the store's index served on it (a layer it has; its file is not asked for here) at :8000/layers.json, with
/// answers that are not one beside it.
fn machine() -> Option<(Machine, Store, String)> {
    if !here() { return None }
    image("machine.Dockerfile", "superci-layers-machine");
    let bin = &program();
    static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let name = format!("superci-layers-machine-{}-{n}", std::process::id());
    // (The store on a disk, as on a machine: the kernel mounts an EROFS file from a filesystem that reads pages itself,
    // which neither a container's own overlay nor tmpfs is: /tmp is tmpfs on Ubuntu 26.04. Cargo's folder is on disk.)
    let store = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("superci-layers-store-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&store).unwrap();
    let store = Store(store);
    let mut args: Vec<String> = ["run", "-d", "--privileged", "--cgroupns=host", "-v", "/sys/fs/cgroup:/sys/fs/cgroup:rw", "--tmpfs", "/run", "--tmpfs", "/run/lock", "--name", &name,
        "-v", &format!("{bin}:/usr/local/bin/superci-layers:ro"), "-v", &format!("{}:/var/lib/superci-layers", store.0.display())].iter().map(|s| s.to_string()).collect();
    args.extend(measured());
    args.push("superci-layers-machine".into());
    ok(&args.iter().map(String::as_str).collect::<Vec<_>>());
    let m = Machine(name);
    m.sh("for i in $(seq 120); do docker info >/dev/null 2>&1 && exit 0; sleep 0.5; done; exit 1");
    let stock = m.sh("docker info -f '{{.Driver}}'");
    assert_ne!(stock, "superci");
    m.sh(r#"mkdir -p /srv && echo '{"layers": {"sha256:0000000000000000000000000000000000000000000000000000000000000001": {"size": 1, "erofs": {"size": 1}, "blob": {}}}}' > /srv/layers.json && echo nope > /srv/bad.json && echo '{"layers": {}}' > /srv/empty.json && (setsid python3 -m http.server 8000 -d /srv >/dev/null 2>&1 &) && sleep 1"#);
    Some((m, store, stock))
}

/// Whether the machine's kernel mounts an EROFS file (6.12 and later): where it does not, `setup` says so and leaves
/// Docker as it comes, as on such a machine.
fn mounts_erofs(m: &Machine, stock: &str) -> bool {
    let (said, state) = m.switch(&[], &["--index-url=http://127.0.0.1:8000/layers.json", "--disk=none"]);
    if !state.contains("the kernel does not mount an EROFS file") { m.back(stock); return true }
    let kernel = m.sh("uname -r");
    let (major, minor) = { let mut v = kernel.split(['.', '-']).map(|n| n.parse::<u32>().unwrap_or(0)); (v.next().unwrap_or(0), v.next().unwrap_or(0)) };
    assert!((major, minor) < (6, 12), "kernel {kernel} mounts EROFS files, yet: {said}");
    m.docker_on(stock);
    false
}

/// A loop device of the machine's (on a file in it), taken away when dropped: a disk for the store.
struct Loop<'a>(&'a Machine, String);
impl Drop for Loop<'_> { fn drop(&mut self) { let _ = self.0.exec(&[], &["sh", "-c", &format!("umount /var/lib/superci-layers/files /run/sl/files 2>/dev/null; rm -f /dev/ram0; losetup -d {}", self.1)]); } }

/// lsblk, findmnt and modprobe as an AWS machine's would answer, for `--disk=auto` and a disk in memory: EBS disks of
/// which only `disk` (the loop device) is not the root's and has no partitions and nothing mounted; brd's /dev/ram0
/// being that loop device. `SHIM_MODE=none`: no EBS disk at all.
const SHIMS: &str = r#"mkdir -p /shims && cat > /shims/findmnt <<'END'
#!/bin/sh
echo /dev/srcdisk1
END
cat > /shims/lsblk <<'END'
#!/bin/sh
case "$*" in
  "-no PKNAME /dev/srcdisk1") echo rootdisk ;;
  "-dnpo NAME,MODEL") if [ "$SHIM_MODE" = none ]; then echo "/dev/other Virtual Disk"; else printf '/dev/other Virtual Disk\n/dev/rootdisk Amazon Elastic Block Store\n/dev/srcdisk Amazon Elastic Block Store\n/dev/parted Amazon Elastic Block Store\n/dev/used Amazon Elastic Block Store\nDISK Amazon Elastic Block Store\n'; fi ;;
  "-nro NAME,MOUNTPOINT /dev/parted") printf 'parted\nparted1 /x\n' ;;
  "-nro NAME,MOUNTPOINT /dev/used") echo 'used /mnt' ;;
  "-nro NAME,MOUNTPOINT DISK") basename DISK ;;
  *) exit 1 ;;
esac
END
cat > /shims/modprobe <<'END'
#!/bin/sh
ln -sf DISK /dev/ram0
END
chmod +x /shims/*"#;

#[test]
fn setup_switches_a_machines_docker_to_the_layers_service_and_off_puts_it_back() {
    let Some((m, _store, stock)) = machine() else { return };
    let index = "--index-url=http://127.0.0.1:8000/layers.json";
    // Switched: the service under systemd, containerd and Docker on it (started again beside, as beside a runner).
    let (said, state) = m.switch(&[], &[index, "--disk=none"]);
    // (A kernel before 6.12, as GitHub's runners had, mounts no EROFS file: said, and Docker left as it comes.)
    if state.contains("the kernel does not mount an EROFS file") { m.docker_on(&stock); assert!(!mounts_erofs(&m, &stock)); return }
    assert!(said.contains("superci-layers: switched in") && state.starts_with("on: Docker answered on it"), "{said}\n{state}");
    m.docker_on("superci");
    assert_eq!(m.sh("systemctl is-active superci-layers"), "active");
    // Off: Docker as it came.
    m.back(&stock);
    // An index that cannot be had, is not there, is not one, or has no layers: nothing switched, Docker as it comes,
    // and the start not failed.
    for (url, why) in [("http://127.0.0.1:9/none.json", "the index: "), ("http://127.0.0.1:8000/missing.json", "the control plane answered 404"),
        ("http://127.0.0.1:8000/bad.json", "the index: "), ("http://127.0.0.1:8000/empty.json", "no layers for this machine yet")] {
        let (_, state) = m.switch(&[], &[&format!("--index-url={url}"), "--disk=none"]);
        assert!(state.starts_with("off: ") && state.contains(why), "{url}: {state}");
        m.docker_on(&stock);
    }
    // Docker with no settings file of its own: none kept, none left after.
    m.sh("cp -a /etc/docker/daemon.json /var/tmp/daemon.json 2>/dev/null; rm -f /etc/docker/daemon.json");
    assert!(m.switch(&[], &[index, "--disk=none"]).1.starts_with("on: "));
    m.back(&stock);
    assert_eq!(m.sh("ls /etc/docker/daemon.json 2>/dev/null | wc -l"), "0");
    m.sh("[ ! -e /var/tmp/daemon.json ] || cp -a /var/tmp/daemon.json /etc/docker/daemon.json");
    // A disk of its own, given: made and mounted for the files.
    let disk = Loop(&m, m.sh("truncate -s 256M /var/tmp/disk.img && losetup -f --show /var/tmp/disk.img"));
    let (said, state) = m.switch(&[], &[index, &format!("--disk={}", disk.1)]);
    assert!(said.contains(&format!("files on {}", disk.1)) && state.starts_with("on: "), "{said}\n{state}");
    m.back(&stock);
    m.sh("umount /var/lib/superci-layers/files");
    // Found (`auto`): the EBS disk that is not the root's, has no partitions and nothing mounted, as lsblk and findmnt
    // say on an AWS machine (here their stand-ins); with none found, the machine's own disk.
    m.sh(&SHIMS.replace("DISK", &disk.1));
    let shims = || vec!["-e".to_string(), "PATH=/shims:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin".to_string()];
    let (said, state) = m.switch(&shims(), &[index, "--disk=auto"]);
    assert!(said.contains(&format!("files on {}", disk.1)) && state.starts_with("on: "), "{said}\n{state}");
    m.back(&stock);
    m.sh("umount /var/lib/superci-layers/files");
    let (said, _) = m.switch(&[shims(), vec!["-e".into(), "SHIM_MODE=none".into()]].concat(), &[index, "--disk=auto"]);
    assert!(said.contains("files on the machine's disk"), "{said}");
    m.back(&stock);
    // Its folder in memory (tmpfs, which the kernel mounts no EROFS file from): a disk in memory (brd, whose stand-in
    // gives the loop device as /dev/ram0); with too little memory for one, said, and not switched.
    let (said, state) = m.switch(&shims(), &[index, "--disk=none", "--root=/run/sl"]);
    assert!(said.contains("files on memory (/dev/ram0)") && state.starts_with("on: "), "{said}\n{state}");
    m.back(&stock);
    m.sh("umount /run/sl/files");
    m.sh("echo 'MemTotal: 1000 kB' > /var/tmp/meminfo && mount --bind /var/tmp/meminfo /proc/meminfo");
    let (_, state) = m.switch(&shims(), &[index, "--disk=none", "--root=/run/sl2"]);
    m.sh("umount /proc/meminfo");
    assert!(state.contains("too little memory"), "{state}");
    m.docker_on(&stock);
}

/// `setup` (and `setup-finish`, which it starts), `setup-finish` by itself after a switch, and `off`, each with each of
/// its steps failing in turn (`SUPERCI_LAYERS_FAULT`): the machine's start never fails, and Docker always comes back as
/// it came.
#[test]
fn setup_holds_with_each_call_and_store_operation_failing() {
    let Some((m, _store, stock)) = machine() else { return };
    if !mounts_erofs(&m, &stock) { return }
    let index = "--index-url=http://127.0.0.1:8000/layers.json";
    let fault = |k: Option<usize>, count: &str| { let mut e = vec!["-e".to_string(), format!("SUPERCI_LAYERS_FAULTS=/var/tmp/{count}")]; if let Some(k) = k { e.extend(["-e".into(), format!("SUPERCI_LAYERS_FAULT={k}")]) } e };
    // How many steps each takes. (`setup-finish` counts in the same file once it is started, a restart later: what
    // `setup` counted is read as it ends.)
    m.sh("rm -f /run/superci-layers.state");
    let mut e = measured_env();
    e.extend(fault(None, "count"));
    let o = m.exec(&e, &["sh", "-c", &format!("/usr/local/bin/superci-layers setup {index} --disk=none >/dev/null && cat /var/tmp/count")]);
    let setup: usize = String::from_utf8_lossy(&o.stdout).trim().parse().unwrap();
    m.sh("for i in $(seq 240); do [ -s /run/superci-layers.state ] && exit 0; sleep 0.5; done; exit 1");
    let finish: usize = m.sh("cat /var/tmp/count").parse().unwrap();
    let mut e = measured_env();
    e.extend(fault(None, "count-off"));
    assert!(m.exec(&e, &["/usr/local/bin/superci-layers", "off"]).status.success());
    m.docker_on(&stock);
    let off: usize = m.sh("cat /var/tmp/count-off").parse().unwrap();
    let run = |what: &str, k: usize| -> String {
        if what == "setup" {
            // (The same step of `setup-finish` fails too, where `setup` gets that far.)
            let (said, state) = m.switch(&fault(Some(k), "c"), &[index, "--disk=none"]);
            return format!("{said}{state}")
        }
        assert!(m.switch(&[], &[index, "--disk=none"]).1.starts_with("on: "));
        let mut e = measured_env();
        e.extend(fault(Some(k), "c"));
        let o = m.exec(&e, &["/usr/local/bin/superci-layers", what]);
        String::from_utf8_lossy(&o.stdout).into_owned() + &String::from_utf8_lossy(&o.stderr)
    };
    if std::env::var("SUPERCI_SWEEP_FAULTS").ok().as_deref() != Some("1") { return }
    let said = std::env::var("SUPERCI_SWEEP_SLICE").unwrap_or_default();
    let (k0, n) = said.split_once('/').and_then(|(k, n)| Some((k.parse::<usize>().ok()?, n.parse::<usize>().ok()?))).filter(|(k, n)| *k >= 1 && k <= n).unwrap_or((1, 1));
    let mut broke = vec![];
    for (what, steps) in [("setup", setup.max(finish)), ("setup-finish", finish), ("off", off)] {
        for k in (1..=steps).filter(|k| (k - 1) % n == k0 - 1) {
            let said = run(what, k);
            if said.contains("panicked") { broke.push(format!("{what} step {k}: {said}")) }
            // (Docker as it came, whatever failed: put back by the program, or by `off` once more.)
            m.back(&stock);
        }
    }
    assert!(broke.is_empty(), "{} faults broke it:\n{}", broke.len(), broke.join("\n\n"));
}

#[test]
fn the_end_to_end_suite_runs_docker_on_the_layers_service_against_docker_as_it_comes() {
    // (CI runs the suite in a job of its own, on each Docker version machines have; here only where it is measured.)
    if std::env::var("SUPERCI_LAYERS_E2E").as_deref() != Ok("1") { eprintln!("SUPERCI_LAYERS_E2E is not 1: skipped"); return }
    if !here() { return }
    image("Dockerfile", "superci-layers-e2e");
    let bin = &program();
    let volume = format!("superci-layers-e2e-{}", std::process::id());
    let mut args: Vec<String> = ["run", "--rm", "--privileged", "-e", &format!("IMAGES={}", std::env::var("IMAGES").unwrap_or_else(|_| "mirror.gcr.io/library".into())),
        "-v", &format!("{bin}:/superci-layers:ro"), "-v", &format!("{volume}:/var/lib")].iter().map(|s| s.to_string()).collect();
    args.extend(measured());
    args.push("superci-layers-e2e".into());
    let o = docker(&args.iter().map(String::as_str).collect::<Vec<_>>());
    let _ = docker(&["volume", "rm", "-f", &volume]);
    let said = String::from_utf8_lossy(&o.stdout).into_owned() + &String::from_utf8_lossy(&o.stderr);
    assert!(o.status.success(), "{}", said.chars().rev().take(6000).collect::<String>().chars().rev().collect::<String>());
}

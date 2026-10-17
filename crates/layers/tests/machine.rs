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

/// What measures the tests, carried into a container: its settings, and its folder mounted at the same path.
fn measured() -> Vec<String> {
    let mut args = vec![];
    for (k, v) in std::env::vars().filter(|(k, _)| k.starts_with("SUPERCOV_")) {
        if k.ends_with("_DIR") { std::fs::create_dir_all(&v).ok(); args.extend(["-v".to_string(), format!("{v}:{v}")]) }
        args.extend(["-e".to_string(), format!("{k}={v}")]);
    }
    args
}

fn image(file: &str, tag: &str) {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/e2e");
    ok(&["build", "-q", "-f", &format!("{dir}/{file}"), "-t", tag, dir]);
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

#[test]
fn setup_switches_a_machines_docker_to_the_layers_service_and_off_puts_it_back() {
    if !here() { return }
    image("machine.Dockerfile", "superci-layers-machine");
    let bin = &program();
    let name = format!("superci-layers-machine-{}", std::process::id());
    let mut args: Vec<String> = ["run", "-d", "--privileged", "--cgroupns=host", "-v", "/sys/fs/cgroup:/sys/fs/cgroup:rw", "--tmpfs", "/run", "--tmpfs", "/run/lock", "--name", &name,
        "-v", &format!("{bin}:/usr/local/bin/superci-layers:ro")].iter().map(|s| s.to_string()).collect();
    args.extend(measured());
    args.push("superci-layers-machine".into());
    ok(&args.iter().map(String::as_str).collect::<Vec<_>>());
    let m = Machine(name);
    // Up, with Docker as it comes (on containerd's own snapshotter).
    m.sh("for i in $(seq 120); do docker info >/dev/null 2>&1 && exit 0; sleep 0.5; done; exit 1");
    let stock = m.sh("docker info -f '{{.Driver}}'");
    assert_ne!(stock, "superci");
    // The store's index, served on the machine (a layer it has; its file is not asked for here).
    m.sh(r#"mkdir -p /srv && echo '{"layers": {"sha256:0000000000000000000000000000000000000000000000000000000000000001": {"size": 1, "erofs": {"size": 1}, "blob": {}}}}' > /srv/layers.json && (setsid python3 -m http.server 8000 -d /srv >/dev/null 2>&1 &) && sleep 1"#);
    // Switched: the service under systemd, containerd and Docker on it (started again beside, as beside a runner).
    let o = m.exec(&measured(), &["/usr/local/bin/superci-layers", "setup", "--index-url=http://127.0.0.1:8000/layers.json", "--disk=none"]);
    let said = String::from_utf8_lossy(&o.stdout).into_owned();
    assert!(o.status.success() && said.contains("superci-layers: switched in"), "{said}{}", String::from_utf8_lossy(&o.stderr));
    m.sh("for i in $(seq 120); do [ \"$(docker info -f '{{.Driver}}' 2>/dev/null)\" = superci ] && exit 0; sleep 0.5; done; cat /var/log/superci-layers.log; exit 1");
    assert_eq!(m.sh("systemctl is-active superci-layers"), "active");
    assert!(m.sh("cat /var/log/superci-layers.log").contains("Docker answered on it"), "{}", m.sh("cat /var/log/superci-layers.log"));
    // Off: Docker as it came.
    let o = m.exec(&measured(), &["/usr/local/bin/superci-layers", "off"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stdout));
    m.sh(&format!("for i in $(seq 120); do [ \"$(docker info -f '{{{{.Driver}}}}' 2>/dev/null)\" = '{stock}' ] && exit 0; sleep 0.5; done; exit 1"));
    // An index that cannot be had: nothing switched, Docker as it comes, and the start not failed.
    let o = m.exec(&measured(), &["/usr/local/bin/superci-layers", "setup", "--index-url=http://127.0.0.1:9/none.json", "--disk=none"]);
    assert!(o.status.success() && String::from_utf8_lossy(&o.stdout).contains("superci-layers: off: "), "{}", String::from_utf8_lossy(&o.stdout));
    assert_eq!(m.sh("docker info -f '{{.Driver}}'"), stock);
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

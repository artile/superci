//! `superci-cache wrap`, as a machine's start-up runs it: the runner's Node becomes this program, which points an
//! action's results service at superci-cache once it is up and runs the real Node with the rest as it was.
use std::process::Command;

#[test]
fn the_runners_node_points_actions_here_and_keeps_every_variable() {
    let dir = std::env::temp_dir().join(format!("superci-cache-wrap-{}", std::process::id()));
    let bin = dir.join("node20/bin");
    std::fs::create_dir_all(&bin).unwrap();
    // The "real Node" here prints what it was given: its environment (not through a shell, which drops a variable with
    // a dash in its name; not env itself, which on Ubuntu 26.04 is one program for all of coreutils, telling which by
    // the name it is run as, here "node").
    std::fs::write(bin.join("node"), "#!/usr/bin/env python3\nimport os\nfor k, v in os.environ.items(): print(f'{k}={v}')\n").unwrap();
    std::fs::set_permissions(bin.join("node"), std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let program = env!("CARGO_BIN_EXE_superci-cache");
    for _ in 0..2 { assert!(Command::new(program).args(["wrap", dir.to_str().unwrap()]).status().unwrap().success()) }
    assert!(bin.join("node.real").exists());
    let run = |results: Option<&str>| {
        let mut c = Command::new(bin.join("node"));
        // (What else the test runs with stays: a coverage tool's, among it.)
        c.env_remove("ACTIONS_RESULTS_URL").env("INPUT_IF-NO-FILES-FOUND", "warn").env("PATH", "/usr/bin:/bin");
        if let Some(r) = results { c.env("ACTIONS_RESULTS_URL", r); }
        let out = String::from_utf8(c.output().unwrap().stdout).unwrap();
        (out.lines().find_map(|l| l.strip_prefix("ACTIONS_RESULTS_URL=")).map(str::to_string), out.contains("INPUT_IF-NO-FILES-FOUND=warn"))
    };
    let github = "https://results-receiver.actions.githubusercontent.com/";
    // Not up yet: as it was.
    assert_eq!(run(Some(github)), (Some(github.to_string()), true));
    std::fs::write(dir.join("superci-cache.address"), "http://10.0.0.5:4000\n").unwrap();
    assert_eq!(run(Some(github)), (Some("http://10.0.0.5:4000/".to_string()), true), "pointed here, and an input with a dash in its name kept");
    // Already here (a Node an action started itself), or none: left alone.
    assert_eq!(run(Some("http://10.0.0.5:4000/")), (Some("http://10.0.0.5:4000/".to_string()), true));
    assert_eq!(run(None), (None, true));
    assert_eq!(run(Some("unix:///run/results")), (Some("unix:///run/results".to_string()), true), "not an address of the web: as it was");
    std::fs::remove_dir_all(&dir).unwrap();
}

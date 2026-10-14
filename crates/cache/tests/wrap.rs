//! `superci-cache wrap`, as a machine's start-up runs it: the runner's Node becomes this program, which points an
//! action's results service at superci-cache once it is up and runs the real Node with the rest as it was.
use std::process::Command;

#[test]
fn the_runners_node_points_actions_here_and_keeps_every_variable() {
    let dir = std::env::temp_dir().join(format!("superci-cache-wrap-{}", std::process::id()));
    let bin = dir.join("node20/bin");
    std::fs::create_dir_all(&bin).unwrap();
    // The "real Node" here prints what it was given: its environment (env itself, no shell in between; a link, as
    // macOS stops a copy of its own programs).
    std::os::unix::fs::symlink("/usr/bin/env", bin.join("node")).unwrap();
    let program = env!("CARGO_BIN_EXE_superci-cache");
    for _ in 0..2 { assert!(Command::new(program).args(["wrap", dir.to_str().unwrap()]).status().unwrap().success()) }
    assert!(bin.join("node.real").exists());
    let run = |results: Option<&str>| {
        let mut c = Command::new(bin.join("node"));
        c.env_clear().env("INPUT_IF-NO-FILES-FOUND", "warn").env("PATH", "/usr/bin:/bin");
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
    std::fs::remove_dir_all(&dir).unwrap();
}

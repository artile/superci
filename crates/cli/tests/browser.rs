//! The dashboard in a browser, used as a person uses it: headless Chrome (`superci_testkit::Browser`) clicks, types
//! and saves, the dashboard's own scripts running, against a control plane served on this computer and the test
//! kit's stand-in for AWS (where the dashboard writes the control plane's settings, and the control plane reads them).
//! Skipped where no Chrome is installed.
mod common;
use common::{dashboard, home, played, signed_in};
use superci_testkit::{serve, Browser, PLANE};

/// What the page's main part shows holds `words`.
fn shows(words: &str) -> String { format!("document.querySelector('main') && document.querySelector('main').innerText.includes({})", serde_json::json!(words)) }

#[test]
fn its_name_labels_and_largest_machine_are_changed_in_settings_and_the_control_plane_takes_them() {
    let Some(mut b) = Browser::open(1300, 900) else { eprintln!("no Chrome here: skipped"); return };
    let served = serve(|_| {});
    let home = signed_in(&served);
    let (mut child, link) = dashboard(&home);
    let setting = |n: &str| served.aws.settings(PLANE).into_iter().find(|(k, _)| k == n).map(|(_, v)| v);
    b.goto(&link);
    b.wait(&shows("Usage"));
    b.click("nav a", "Settings");
    b.wait(&shows("What your CI is called"));

    // Its name, in its dialog.
    b.click(r#"button[onclick*="'name'"]"#, "Change");
    b.wait("document.getElementById('name').open");
    b.fill("#name input[name=name]", "Acme CI");
    b.click("#name button", "Save");
    b.wait("document.querySelector('.brand span').innerText === 'Acme CI'");
    assert_eq!(setting("NAME").as_deref(), Some("Acme CI"));
    assert!(b.text("main").contains("Acme CI"));

    // A label more (first: the one examples show).
    b.click(r#"button[onclick*="'labels'"]"#, "Change");
    b.wait("document.getElementById('labels').open");
    b.fill("#labels form.limit-do input[name=label]", "gpu");
    b.click("#labels form.limit-do button", "");
    b.wait(&shows("gpu"));
    assert_eq!(setting("LABELS").as_deref(), Some(r#"["gpu","superci"]"#));
    // And taken away again.
    b.click(r#"button[onclick*="'labels'"]"#, "Change");
    b.wait("document.getElementById('labels').open");
    b.click(r#"#labels form.kept-label:has(input[value="gpu"]) button"#, "Remove");
    b.wait(&format!("!({})", shows("gpu")));
    assert_eq!(setting("LABELS").as_deref(), Some(r#"["superci"]"#));

    // The largest machine a label may ask for.
    b.fill("form.limit input[name=max_cpu]", "16");
    b.click("form.limit:has(input[name=max_cpu]) button", "Save");
    let max_cpu = || setting("ROUTING").and_then(|r| serde_json::from_str::<serde_json::Value>(&r).ok()).map(|r| r["max_cpu"].clone());
    let until = std::time::Instant::now() + std::time::Duration::from_secs(15);
    while max_cpu() != Some(16.into()) { assert!(std::time::Instant::now() < until, "saved: {:?}; the page shows {}", max_cpu(), b.text("main")); std::thread::sleep(std::time::Duration::from_millis(100)) }
    // The page read again shows it.
    b.wait("document.readyState === 'complete' && document.querySelector('form.limit input[name=max_cpu]') && document.querySelector('form.limit input[name=max_cpu]').value === '16'");

    assert!(served.aws.unanswered.lock().unwrap().is_empty(), "{:?}", served.aws.unanswered.lock().unwrap());
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_jobs_log_is_read_in_steps_searched_and_shown_with_its_times() {
    let Some(mut b) = Browser::open(1300, 900) else { eprintln!("no Chrome here: skipped"); return };
    let served = serve(|_| {});
    let home = home(&served);
    played(&served, &home.join("checkout"));
    let (mut child, link) = dashboard(&home);
    b.goto(&link);
    // From the Overview's recent jobs to the job's page.
    b.wait(&shows("Usage"));
    b.click("nav a", "Jobs");
    b.wait(&shows("Recent jobs"));
    b.click("main a", "tests");
    b.wait("document.querySelector('details.lstep')");
    assert_eq!(b.text("details.lstep summary"), "Run make test 7s");
    assert!(b.eval("document.querySelector('details.lstep').classList.contains('success')") == true);
    // A step that passed is closed; opened, its lines, in colour.
    b.click("details.lstep summary", "");
    b.wait("document.querySelector('details.lstep').open");
    assert_eq!(b.eval("document.querySelectorAll('details.lstep .ll').length"), 12);
    assert_eq!(b.eval("document.querySelector('details.lstep .ll:last-child .lx').innerHTML"), r#"<span class="a32">test 12 passed</span>"#);
    // Searching keeps the lines that hold the words.
    b.fill("[data-log-search]", "test 12");
    b.wait("document.querySelectorAll('.ll.hit').length === 1");
    assert_eq!(b.text(".ll.hit .lx"), "test 12 passed");
    b.fill("[data-log-search]", "");
    b.wait("document.querySelectorAll('.ll.hit').length === 0");
    // Times shown and hidden.
    b.click("[data-log-times]", "Times");
    b.wait("document.querySelector('.log-card').classList.contains('times')");
    b.click("[data-log-times]", "Times");
    b.wait("!document.querySelector('.log-card').classList.contains('times')");
    // The page reads itself again: the step stays open as it was.
    b.wait("new Promise(r => setTimeout(() => r(true), 2500))");
    assert!(b.eval("document.querySelector('details.lstep').open") == true);
    assert_eq!(b.eval("document.querySelector('a[href^=\"/job/raw\"]').innerText"), "Raw log");
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

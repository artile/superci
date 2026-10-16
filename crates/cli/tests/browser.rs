//! The dashboard in a browser, used as a person uses it: headless Chrome (`superci_testkit::Browser`) clicks, types
//! and saves, the dashboard's own scripts running, against a control plane served on this computer and the test
//! kit's stand-in for AWS (where the dashboard writes the control plane's settings, and the control plane reads them).
//! Skipped where no Chrome is installed.
mod common;
use common::{dashboard, home, played, signed_in};
use superci_testkit::{serve, Browser, PLANE};

/// Waits up to 15 seconds for `holds`, else fails saying `what`.
fn until(what: &str, holds: impl Fn() -> bool) {
    let end = std::time::Instant::now() + std::time::Duration::from_secs(15);
    while !holds() { assert!(std::time::Instant::now() < end, "waited for {what}"); std::thread::sleep(std::time::Duration::from_millis(100)) }
}

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
    b.click("nav a", "Runs");
    b.wait(&shows("Recent runs"));
    b.click(".tabs a", "Jobs");
    b.wait(&shows("Recent jobs"));
    b.click("main a", "tests");
    b.wait("document.querySelector('details.lstep')");
    assert_eq!(b.text("details.lstep summary"), "Run make test 7s");
    assert!(b.eval("document.querySelector('details.lstep').classList.contains('success')") == true);
    // A step that passed is closed; opened, its lines, in colour.
    b.click("details.lstep summary", "");
    b.wait("document.querySelector('details.lstep').open");
    // (Its lines are drawn once it is open, not before.)
    b.wait("document.querySelectorAll('details.lstep .ll').length === 12");
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

#[test]
fn a_runner_providers_limits_and_location_are_set_in_its_dialog_and_it_is_removed() {
    let Some(mut b) = Browser::open(1300, 900) else { eprintln!("no Chrome here: skipped"); return };
    let served = serve(|_| {});
    let home = signed_in(&served);
    let (mut child, link) = dashboard(&home);
    let setting = |n: &str| served.aws.settings(PLANE).into_iter().find(|(k, _)| k == n).map(|(_, v)| v);
    let pool = || setting("ROUTING").and_then(|r| serde_json::from_str::<serde_json::Value>(&r).ok()).map(|r| r["order"][0].clone()).unwrap_or_default();
    b.goto(&link);
    b.wait(&shows("Usage"));
    b.click("nav a", "Runners");
    b.wait(&shows("Runner providers"));

    // Its settings: jobs at once, money a month, where its containers start.
    b.click(r#"button[aria-label="Cloudflare settings"]"#, "");
    b.wait("document.getElementById('pool-0').open");
    b.fill("#pool-0 input[name=max]", "5");
    b.fill("#pool-0 input[name=usd]", "30");
    b.eval("(() => { const s = document.querySelector('#pool-0 select[name=location]'); s.value = 'weur'; s.dispatchEvent(new Event('change', {bubbles: true})); })()");
    b.click("#pool-0 button.primary", "Save");
    until("the provider's limits", || pool()["max_jobs"] == 5 && pool()["monthly_usd"] == 30.0);
    until("its location", || setting("CF_LOCATION").as_deref() == Some("weur"));
    // (Once the page shows what was saved: a dialog opened before it is drawn anew would go with the old page.)
    b.wait(&format!("{} && !document.body.classList.contains('busy')", shows("5 at once")));
    b.click(r#"button[aria-label="Cloudflare settings"]"#, "");
    b.wait("document.getElementById('pool-0').open");
    assert_eq!(b.eval("[document.querySelector('#pool-0 input[name=max]').value, document.querySelector('#pool-0 input[name=usd]').value, document.querySelector('#pool-0 select[name=location]').value]"), serde_json::json!(["5", "30", "weur"]));

    b.click("#pool-0 button.secondary", "Cancel");

    // Removed, from its account's dialog under Clouds: no job goes there from then.
    b.click(r#"button[aria-label="Cloudflare account settings"]"#, "");
    b.wait("document.getElementById('pool-100').open");
    b.click("#pool-100 button.danger", "Remove");
    b.wait("document.getElementById('pool-remove-100').open");
    b.click("#pool-remove-100 button.danger", "");
    until("its containers off", || setting("CONTAINERS").as_deref() == Some("off"));
    b.wait(&shows("Removed Cloudflare"));
    assert!(pool().is_null(), "{}", pool());
    assert!(served.aws.unanswered.lock().unwrap().is_empty(), "{:?}", served.aws.unanswered.lock().unwrap());
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_runner_provider_is_taken_out_of_the_order_and_put_back_and_awss_on_demand_machines_get_their_own_regions() {
    let Some(mut b) = Browser::open(1300, 900) else { eprintln!("no Chrome here: skipped"); return };
    // A control plane whose own AWS account runs jobs, with Cloudflare's containers: AWS spot, AWS on-demand, Cloudflare.
    let served = serve(|c| {
        c.containers = true;
        (c.aws_own, c.aws_own_creds) = (Some(("111111111111".into(), "us-east-1".into())), Some(superci_core::aws::Credentials { access_key_id: "ASIAX".into(), secret_access_key: "secret".into(), session_token: Some("tok".into()), expires_at_ms: u64::MAX }));
    });
    let home = signed_in(&served);
    // (Another workspace on this computer: what is changed says which workspace it is for.)
    std::fs::create_dir_all(home.join("workspaces/globex")).unwrap();
    std::fs::write(home.join("workspaces/globex/sign-ins.json"), "{}").unwrap();
    let (mut child, link) = dashboard(&home);
    let setting = |n: &str| served.aws.settings(PLANE).into_iter().find(|(k, _)| k == n).map(|(_, v)| v);
    let order = || setting("ROUTING").and_then(|r| serde_json::from_str::<serde_json::Value>(&r).ok()).map(|r| r["order"].clone()).unwrap_or_default();
    b.goto(&link);
    b.wait(&shows("Usage"));
    b.click("nav a", "Runners");
    b.wait(&shows("Runner providers"));
    // Dragged: Cloudflare to the top, and saved.
    b.eval(r#"(() => { const list = document.querySelector('ol.pools'), moved = list.querySelector('[data-cloud="cloudflare"]'), first = list.querySelector('.pool'), dt = new DataTransfer();
        moved.dispatchEvent(new DragEvent('dragstart', { bubbles: true, dataTransfer: dt }));
        first.dispatchEvent(new DragEvent('dragover', { bubbles: true, cancelable: true, dataTransfer: dt, clientY: first.getBoundingClientRect().top + 1 }));
        moved.dispatchEvent(new DragEvent('dragend', { bubbles: true, dataTransfer: dt })); })()"#);
    b.wait("document.getElementById('order-note').textContent.includes('Order saved')");
    assert_eq!(order().as_array().map(|o| o.iter().map(|p| p["cloud"].clone()).collect::<Vec<_>>()), Some(vec![serde_json::json!("cloudflare"), serde_json::json!("aws"), serde_json::json!("aws-on-demand")]));
    b.goto(&format!("{link}&p=runners"));
    b.wait(&shows("Runner providers"));
    // AWS on-demand's settings, like AWS spot's: its regions (here N. Virginia, where the control plane is set up).
    b.click(r#"button[aria-label="AWS on-demand settings"]"#, "");
    b.wait("document.getElementById('pool-2').open");
    assert_eq!(b.eval("[...document.querySelectorAll('#pool-2 .rg')].map(r => r.dataset.region)"), serde_json::json!(["us-east-1"]));
    b.fill("#pool-2 input[name=max]", "3");
    b.click("#pool-2 button.primary", "Save");
    until("on-demand's limit and regions", || order().as_array().is_some_and(|o| o.iter().any(|p| p["cloud"] == "aws-on-demand" && p["max_jobs"] == 3 && p["regions"] == serde_json::json!(["us-east-1"]))));
    b.wait(&format!("{} && !document.body.classList.contains('busy')", shows("3 at once")));
    // Taken out from its dialog: out of the order, said below it; nothing deleted.
    b.click(r#"button[aria-label="AWS on-demand settings"]"#, "");
    b.wait("document.getElementById('pool-2').open");
    b.click("#pool-2 button.secondary", "Take out of the order");
    until("on-demand taken out", || order().as_array().is_some_and(|o| o.iter().any(|p| p["cloud"] == "aws-on-demand" && p["off"] == true && p["max_jobs"] == 3)));
    b.wait(&shows("Taken out of the order"));
    assert_eq!(b.eval(r#"document.querySelector('li.pool[data-cloud="aws-on-demand"]') === null"#), serde_json::json!(true));
    // Put back: at the end of the order, its settings as they were.
    b.click(".limits.out button.chip", "Put back");
    until("on-demand put back", || order().as_array().is_some_and(|o| o.last().is_some_and(|p| p["cloud"] == "aws-on-demand" && p.get("off").is_none() && p["max_jobs"] == 3)));
    b.wait(r#"document.querySelector('li.pool[data-cloud="aws-on-demand"]') !== null"#);
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn money_limits_the_longest_job_the_cache_and_public_repositories_are_set_in_the_browser() {
    let Some(mut b) = Browser::open(1300, 900) else { eprintln!("no Chrome here: skipped"); return };
    // A control plane whose own AWS account runs jobs, with GitHub connected (its App installed on acme/app).
    let served = serve(|c| {
        c.containers = true;
        (c.aws_own, c.aws_own_creds) = (Some(("111111111111".into(), "us-east-1".into())), Some(superci_core::aws::Credentials { access_key_id: "ASIAX".into(), secret_access_key: "secret".into(), session_token: Some("tok".into()), expires_at_ms: u64::MAX }));
    });
    served.connect_github();
    served.github.set_public(true);
    let home = signed_in(&served);
    let (mut child, link) = dashboard(&home);
    let setting = |n: &str| served.aws.settings(PLANE).into_iter().find(|(k, _)| k == n).map(|(_, v)| v);
    let routing = || setting("ROUTING").and_then(|r| serde_json::from_str::<serde_json::Value>(&r).ok()).unwrap_or_default();
    let settled = |words: &str| format!("{} && !document.body.classList.contains('busy')", shows(words));
    b.goto(&link);
    b.wait(&shows("Usage"));
    b.click("nav a", "Runners");
    b.wait(&shows("Limits"));

    // Everything: $50 a month, $5 a day.
    b.click(r#"button[aria-label="Everything limits"]"#, "");
    b.wait("document.getElementById('limit-all').open");
    b.fill("#limit-all input[name=usd]", "50");
    b.fill("#limit-all input[name=daily]", "5");
    b.click("#limit-all button.primary", "Save");
    until("the CI's limits", || routing()["monthly_usd"] == 50.0 && routing()["daily_usd"] == 5.0);
    b.wait(&settled("$50.00"));
    // AWS's account: $40 a month (its spot and on-demand machines together), no budget in AWS itself.
    b.click(r#"button[aria-label="AWS account 111111111111 limits"]"#, "");
    b.wait("document.getElementById('limit-aws').open");
    b.fill("#limit-aws input[name=usd]", "40");
    b.click("#limit-aws button.primary", "Save");
    until("the account's limit", || routing()["account_usd"]["aws"] == 40.0 && routing().get("aws_budget").is_none());
    b.wait(&settled("$40.00"));
    // Lifted again: empty is no limit.
    b.click(r#"button[aria-label="AWS account 111111111111 limits"]"#, "");
    b.wait("document.getElementById('limit-aws').open");
    b.fill("#limit-aws input[name=usd]", "");
    b.click("#limit-aws button.primary", "Save");
    until("the account's limit lifted", || routing().get("account_usd").is_none());
    b.wait(&format!("!document.body.classList.contains('busy') && !({})", shows("$40.00")));

    // Settings: the longest job, in hours.
    b.click(".side a", "Settings");
    b.wait(&shows("Longest job"));
    b.fill("input[name=max_hours]", "12");
    b.eval("document.querySelector('input[name=max_hours]').form.querySelector('button').click()");
    until("the longest job", || routing()["max_minutes"] == 720);
    b.wait("document.querySelector('input[name=max_hours]') && document.querySelector('input[name=max_hours]').value === '12' && !document.body.classList.contains('busy')");
    // The cache: what a repository keeps, and how long an entry may go unused.
    b.fill("input[name=size_gb]", "50");
    b.eval("document.querySelector('input[name=size_gb]').form.querySelector('button').click()");
    until("the cache's size", || routing()["cache"]["size_gb"] == 50);
    b.wait("document.querySelector('input[name=size_gb]').value === '50' && !document.body.classList.contains('busy')");
    b.fill("input[name=days]", "3");
    b.eval("document.querySelector('input[name=days]').form.querySelector('button').click()");
    until("the cache's days", || routing()["cache"]["days"] == 3);
    b.wait("document.querySelector('input[name=days]').value === '3' && !document.body.classList.contains('busy')");
    // A public repository allowed to run jobs here (anyone can open a pull request on one).
    b.wait(&shows("Public repositories"));
    b.click("button.secondary", "Choose…");
    b.wait("document.getElementById('public-repos').open");
    b.eval("document.querySelector('#public-repos input[value=\"acme/app\"]').checked = true");
    b.click("#public-repos button.primary", "");
    until("the public repository allowed", || routing()["public_repos"] == serde_json::json!(["acme/app"]));
    b.wait(&settled("app"));
    assert!(served.aws.unanswered.lock().unwrap().is_empty(), "{:?}", served.aws.unanswered.lock().unwrap());
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn the_default_machine_is_picked_on_workflows_and_the_workspace_renamed_in_settings() {
    let Some(mut b) = Browser::open(1300, 900) else { eprintln!("no Chrome here: skipped"); return };
    let served = serve(|_| {});
    let home = signed_in(&served);
    let (mut child, link) = dashboard(&home);
    let setting = |n: &str| served.aws.settings(PLANE).into_iter().find(|(k, _)| k == n).map(|(_, v)| v);
    b.goto(&link);
    b.wait(&shows("Usage"));
    b.click("nav a", "Workflows");
    b.wait(&shows("Default machine"));

    // 8 CPUs, arm64, no spot machines: what the picker says runs it, then saved.
    b.click(r#"button[onclick*="'default-machine'"]"#, "Change");
    b.wait("document.getElementById('default-machine').open");
    b.eval("(() => { const f = document.querySelector('form[action=\"/machine\"]'); f.cpu.value = '8'; f.arch.value = 'arm64'; f.nospot.checked = true; f.dispatchEvent(new Event('change', {bubbles: true})); })()");
    b.click(r#"form[action="/machine"] button.primary"#, "Save");
    until("the default machine", || setting("MACHINE").is_some_and(|m| serde_json::from_str::<serde_json::Value>(&m).is_ok_and(|m| m["cpu"] == 8)));
    let machine: serde_json::Value = serde_json::from_str(&setting("MACHINE").unwrap()).unwrap();
    assert_eq!((machine["arch"].as_str(), machine["spot"].as_bool()), (Some("arm64"), Some(false)), "{machine}");
    b.wait(&shows("8 CPU"));

    // The workspace's id: its folder on this computer is renamed, and the dashboard goes on in it.
    b.click("nav a", "Settings");
    b.wait(&shows("what --workspace takes"));
    b.click(r#"button[onclick*="'rename-workspace'"]"#, "Change");
    b.wait("document.getElementById('rename-workspace').open");
    b.fill("#rename-workspace input[name=id]", "acme");
    b.click("#rename-workspace button.primary", "Save");
    until("the workspace's folder renamed", || home.join("workspaces/acme/sign-ins.json").exists() && !home.join("workspaces/default").exists());
    b.wait(&shows("acme"));
    b.click("nav a", "Runs");
    b.wait(&shows("Recent runs"));
    b.click(".tabs a", "Jobs");
    b.wait(&shows("Recent jobs"));
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_workspace_is_made_from_the_switcher_switched_to_and_removed_from_its_settings() {
    let Some(mut b) = Browser::open(1300, 900) else { eprintln!("no Chrome here: skipped"); return };
    let served = serve(|_| {});
    let home = signed_in(&served);
    let (mut child, link) = dashboard(&home);
    b.goto(&link);
    b.wait(&shows("Usage"));
    // A new one, from the switcher: named, it opens on its own first screen; its folder is kept on this computer.
    b.eval("document.querySelector('details.brand-menu').open = true");
    b.click("#side-switch a", "New workspace…");
    b.wait("document.getElementById('new-workspace').open");
    b.fill("#new-workspace input[name=name]", "Globex");
    b.click("#new-workspace button", "Create workspace");
    b.wait("new URLSearchParams(location.search).get('w') === 'globex'");
    b.wait(&shows("Where is your control plane?"));
    assert!(home.join("workspaces/globex").exists());
    // Back to the one before, from the switcher: its own pages, nothing of Globex's.
    b.eval("document.querySelector('details.brand-menu').open = true");
    b.wait(r#"document.querySelector('#side-switch a.space[href*="w=default"]') !== null"#);
    b.eval(r#"document.querySelector('#side-switch a.space[href*="w=default"]').click()"#);
    b.wait("new URLSearchParams(location.search).get('w') === 'default'");
    b.wait(&shows("Usage"));
    // Globex removed from its settings: forgotten on this computer, nothing deleted in any cloud.
    b.goto(&format!("{}&w=globex&p=settings", link.split("&w=").next().unwrap()));
    b.wait(r#"[...document.querySelectorAll('main button')].some(x => x.textContent === 'Remove')"#);
    b.eval(r#"[...document.querySelectorAll('main button')].find(x => x.textContent === 'Remove').click()"#);
    b.wait("document.getElementById('remove-workspace').open");
    b.click("#remove-workspace button.danger", "Remove from this computer");
    let gone = std::time::Instant::now() + std::time::Duration::from_secs(15);
    while home.join("workspaces/globex").exists() && std::time::Instant::now() < gone { std::thread::sleep(std::time::Duration::from_millis(100)) }
    assert!(!home.join("workspaces/globex").exists() && home.join("workspaces/default/sign-ins.json").exists());
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

//! The dashboard in a browser, used as a person uses it: headless Chrome (`superci_testkit::Browser`) clicks, types
//! and saves, the dashboard's own scripts running, against a control plane served on this computer and the test
//! kit's stand-in for AWS (where the dashboard writes the control plane's settings, and the control plane reads them).
//! Skipped where no Chrome is installed.
mod common;
use common::{words, dashboard, home, played, played_steps, signed_in};
use superci_testkit::{serve, Browser, PLANE};

/// Waits up to 15 seconds for `holds`, else fails saying `what`.
fn until(what: &str, holds: impl Fn() -> bool) {
    let end = std::time::Instant::now() + std::time::Duration::from_secs(15);
    while !holds() { assert!(std::time::Instant::now() < end, "waited for {what}"); std::thread::sleep(std::time::Duration::from_millis(100)) }
}

/// What the page shows (anywhere in it) holds `words`.
fn page_shows(words: &str) -> String { format!("document.body && document.body.innerText.includes({})", serde_json::json!(words)) }

/// The same, its lines and spaces read as single spaces (a figure and its label in elements of their own).
fn reads(words: &str) -> String { format!("document.querySelector('main') && document.querySelector('main').innerText.replace(/\\s+/g, ' ').includes({})", serde_json::json!(words)) }

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
    b.wait(r#"document.querySelector('button[aria-label="Cloudflare settings"]') !== null"#);

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
    // (Its heading shows while the control plane is still read: its providers are what to wait for.)
    b.wait(r#"document.querySelector('ol.pools [data-cloud="cloudflare"]') !== null"#);
    // Dragged: Cloudflare to the top, and saved.
    b.eval(r#"(() => { const list = document.querySelector('ol.pools'), moved = list.querySelector('[data-cloud="cloudflare"]'), first = list.querySelector('.pool'), dt = new DataTransfer();
        moved.dispatchEvent(new DragEvent('dragstart', { bubbles: true, dataTransfer: dt }));
        first.dispatchEvent(new DragEvent('dragover', { bubbles: true, cancelable: true, dataTransfer: dt, clientY: first.getBoundingClientRect().top + 1 }));
        moved.dispatchEvent(new DragEvent('dragend', { bubbles: true, dataTransfer: dt })); })()"#);
    b.wait("document.getElementById('order-note').textContent.includes('Order saved')");
    assert_eq!(order().as_array().map(|o| o.iter().map(|p| p["cloud"].clone()).collect::<Vec<_>>()), Some(vec![serde_json::json!("cloudflare"), serde_json::json!("aws"), serde_json::json!("aws-on-demand")]));
    b.goto(&format!("{link}&p=runners"));
    b.wait(r#"document.querySelector('button[aria-label="AWS on-demand settings"]') !== null"#);
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
    // (Its rows come with the page's live part: on a slow machine a moment after the page itself.)
    b.wait("[...document.querySelectorAll('#side-switch a')].some(a => a.textContent.includes('New workspace'))");
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

#[test]
fn what_runners_and_workflows_refuse_is_said_in_the_browser_and_changes_nothing() {
    let Some(mut b) = Browser::open(1300, 900) else { eprintln!("no Chrome here: skipped"); return };
    // Its own AWS account runs jobs (signed in to it here), with Cloudflare's containers.
    let served = serve(|c| {
        c.containers = true;
        (c.aws_own, c.aws_own_creds) = (Some(("111111111111".into(), "us-east-1".into())), Some(superci_core::aws::Credentials { access_key_id: "ASIAX".into(), secret_access_key: "secret".into(), session_token: Some("tok".into()), expires_at_ms: u64::MAX }));
    });
    let home = signed_in(&served);
    let (mut child, link) = dashboard(&home);
    let setting = |n: &str| served.aws.settings(PLANE).into_iter().find(|(k, _)| k == n).map(|(_, v)| v);
    let routing = || setting("ROUTING").and_then(|r| serde_json::from_str::<serde_json::Value>(&r).ok()).unwrap_or_default();
    let runners = |b: &mut Browser| { b.goto(&format!("{link}&p=runners")); b.wait(&format!("{} && !document.body.classList.contains('busy')", shows("Clouds"))); };
    runners(&mut b);

    // The AWS account's budget in AWS: it needs the monthly limit, and an email address that is one.
    b.click(r#"button[aria-label="AWS account 111111111111 limits"]"#, "");
    b.wait("document.getElementById('limit-aws').open");
    b.eval("document.querySelector('#limit-aws select[name=budget]').value = 'emails'");
    b.click("#limit-aws button.primary", "Save");
    b.wait("document.body.innerText.includes('A budget in AWS needs the monthly limit')");
    // (The page's own field refuses an address that is none before it is sent; the dashboard checks it again.)
    runners(&mut b);
    assert_eq!(b.eval("document.querySelector('#limit-aws input[name=email]').type"), serde_json::json!("email"));
    let (status, _, said) = common::post(&link, "/routing", &[("action", "account_usd"), ("account", "aws"), ("usd", "40"), ("budget", "emails"), ("email", "not an address")]);
    assert!(status == 400 && common::words(&said).contains("That is no email address"), "{status} {}", common::words(&said));
    let (status, _, said) = common::post(&link, "/routing", &[("action", "account_usd"), ("account", "aws"), ("usd", "40"), ("budget", "lift")]);
    assert!(status == 400 && common::words(&said).contains("There is no budget in AWS to lift"), "{status} {}", common::words(&said));
    assert!(routing().get("account_usd").is_none() && routing().get("aws_budget").is_none(), "{}", routing());

    // Cloudflare's image, under Clouds: an address that is https and plain, else said.
    runners(&mut b);
    b.click(r#"button[aria-label="Cloudflare account settings"]"#, "");
    b.wait("[...document.querySelectorAll('dialog')].some(d => d.open && d.querySelector('input[name=image]'))");
    b.eval("(() => { const d = [...document.querySelectorAll('dialog')].find(d => d.open); d.querySelector('input[name=image]').type = 'text'; d.querySelector('input[name=image]').value = 'ftp://images.example.com/full'; d.querySelector('button.primary').click(); })()");
    b.wait("document.body.innerText.includes('https://')");
    assert_eq!(setting("CF_IMAGE"), None);
    runners(&mut b);
    b.click(r#"button[aria-label="Cloudflare account settings"]"#, "");
    b.wait("[...document.querySelectorAll('dialog')].some(d => d.open && d.querySelector('input[name=image]'))");
    b.eval("(() => { const d = [...document.querySelectorAll('dialog')].find(d => d.open); d.querySelector('input[name=image]').value = 'https://images.example.com/full'; d.querySelector('button.primary').click(); })()");
    until("Cloudflare's image", || setting("CF_IMAGE").as_deref() == Some("https://images.example.com/full"));

    // Your own network, under the AWS account: what it says is checked as it is written.
    runners(&mut b);
    b.click(r#"button[aria-label="AWS account 111111111111 settings"]"#, "");
    b.wait("[...document.querySelectorAll('dialog')].some(d => d.open && d.querySelector('textarea[name=networks]'))");
    b.eval("(() => { const d = [...document.querySelectorAll('dialog')].find(d => d.open); d.querySelector('textarea[name=networks]').value = 'us-east-1 not-a-subnet'; d.querySelector('button.primary').click(); })()");
    b.wait("document.body.innerText.includes('Check your networks')");
    assert_eq!(setting("AWS_NETWORKS"), None);
    // One that AWS has (as a real account answered for its subnets and security group): kept, private as said.
    runners(&mut b);
    b.click(r#"button[aria-label="AWS account 111111111111 settings"]"#, "");
    b.wait("[...document.querySelectorAll('dialog')].some(d => d.open && d.querySelector('textarea[name=networks]'))");
    b.eval("(() => { const d = [...document.querySelectorAll('dialog')].find(d => d.open); d.querySelector('textarea[name=networks]').value = 'us-east-1: subnet-00000000000000001, subnet-00000000000000002, sg-00000000000000001, private'; d.querySelector('button.primary').click(); })()");
    until("your own network", || setting("AWS_NETWORKS").is_some_and(|n| n.contains("subnet-00000000000000002") && n.contains("sg-00000000000000001")));
    let networks: serde_json::Value = serde_json::from_str(&setting("AWS_NETWORKS").unwrap()).unwrap();
    assert_eq!(networks["us-east-1"]["private"], true, "{networks}");
    // A subnet AWS does not have there, and a group it does not have: said, and what was kept stays.
    for (text, why) in [("us-east-1 subnet-00000000000000099 sg-00000000000000001", "no subnet subnet-00000000000000099 there"), ("us-east-1 subnet-00000000000000001 sg-00000000000000099", "no security group sg-00000000000000099")] {
        let (status, _, said) = common::post(&link, "/routing", &[("action", "account"), ("cloud", "aws"), ("networks", text)]);
        assert!(status == 400 && common::words(&said).contains(why), "{text}: {status} {}", common::words(&said));
    }
    assert!(setting("AWS_NETWORKS").unwrap().contains("subnet-00000000000000002"));

    // A new order: each provider once, and only ones it has (as a page made up would say).
    for fields in [&[("action", "order"), ("cloud_0", "aws"), ("cloud_1", "aws")][..], &[("action", "order"), ("cloud_0", "nowhere")],
        &[("action", "order"), ("cloud_0", "aws"), ("out_0", "aws")], &[("action", "order"), ("cloud_0", "aws"), ("out_0", "elsewhere")]] {
        let (status, _, said) = common::post(&link, "/routing", fields);
        assert!(status == 400 && common::words(&said).contains("Unknown provider"), "{fields:?}: {status}");
    }
    // The default machine: Windows only on x64.
    let (status, _, said) = common::post(&link, "/machine", &[("os", "windows"), ("arch", "arm64")]);
    assert!(status == 400 && common::words(&said).contains("Windows machines are x64"), "{status} {}", common::words(&said));
    assert_eq!(setting("MACHINE"), None);
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn steps_run_side_by_side_are_shown_nested_with_what_they_are_as_github_shows_them() {
    let Some(mut b) = Browser::open(1300, 900) else { eprintln!("no Chrome here: skipped"); return };
    let served = serve(|_| {});
    let home = home(&served);
    let workflow = "jobs:\n  tests:\n    runs-on: superci\n    steps:\n      - name: Boot the stack\n        id: stack\n        run: sleep 30\n        background: true\n      - run: npm ci\n      - parallel:\n          - name: Lint\n            run: npm run lint\n          - name: Unit tests\n            run: npm test\n      - name: Wait for the stack\n        wait: stack\n      - run: npm run e2e\n";
    played_steps(&served, &home.join("checkout"), workflow, &[("Boot the stack", "succeeded"), ("Run npm ci", "succeeded"), ("Lint", "succeeded"), ("Unit tests", "succeeded"), ("Parallel group", "succeeded"), ("Wait for the stack", "succeeded"), ("Run npm run e2e", "succeeded")]);
    let (mut child, link) = dashboard(&home);
    b.goto(&link);
    b.wait(&shows("Usage"));
    b.click("nav a", "Runs");
    b.wait(&shows("Recent runs"));
    b.click(".tabs a", "Jobs");
    b.wait(&shows("Recent jobs"));
    b.click("main a", "tests");
    b.wait("document.querySelectorAll('details.lstep').length === 7");
    // Every step closed at first, the group too: its steps hidden; opened, shown; closed, hidden again.
    assert_eq!(b.eval("[...document.querySelectorAll('details.lstep')].filter(d => d.open).length"), 0);
    b.wait("document.querySelectorAll('details.lchild.lhid').length === 2");
    b.click("details.lgroup summary", "");
    b.wait("document.querySelectorAll('details.lchild.lhid').length === 0");
    // The group's row before its steps, which are under it; each says what it is.
    let rows = b.eval("[...document.querySelectorAll('details.lstep')].map(d => (d.classList.contains('lchild') ? '  ' : '') + d.querySelector('.lsn').innerText + ' ' + d.querySelector('.lsb').innerText.replace(/\\s+/g, ' ').trim())");
    assert_eq!(rows, serde_json::json!(["Boot the stack BACKGROUND", "Run npm ci ", "Parallel group BACKGROUND PARALLEL · 2 steps", "  Lint BACKGROUND", "  Unit tests BACKGROUND", "Wait for the stack WAIT Boot the stack", "Run npm run e2e "]));
    if let Ok(dir) = std::env::var("SUPERCI_PREVIEW_DIR") { b.eval("window.scrollTo(0, document.querySelector('.log-card').offsetTop - 20)"); b.wait("new Promise(r => setTimeout(() => r(true), 400))"); b.screenshot(&std::path::Path::new(&dir).join("parallel-steps.png")) }
    b.click("details.lgroup summary", "");
    b.wait("document.querySelectorAll('details.lchild.lhid').length === 2");
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_new_workspace_signed_in_to_an_account_with_cis_in_it_picks_one_and_looks_after_it() {
    let Some(mut b) = Browser::open(1300, 900) else { eprintln!("no Chrome here: skipped"); return };
    // Two CIs in the account: the default workspace's, and Globex's (named, whose jobs it runs said where the
    // account's sign-in reads them).
    let served = serve(|_| {});
    served.connect_github();
    let aws = served.aws.clone();
    let other = superci_testkit::serve_as("testplane0002", move |c| c.read_settings(&aws.settings("testplane0002")));
    for (k, v) in other.aws.settings("testplane0002") { served.aws.put(&format!("/superci/testplane0002/{k}"), &v) }
    served.aws.put("/superci/testplane0002/GITHUB_APP", &superci_testkit::GitHub::app().to_string());
    served.aws.put("/superci/testplane0002/NAME", "GlobexCI");
    served.aws.put("/superci/testplane0002/OWNERS", "github:globex");
    let home = signed_in(&served);
    served.aws.replaying(include_str!("../fixtures/aws/planes/delete.jsonl"), &[("http://new-plane.test", &other.url)]);
    // A new workspace, signed in to the same account (as its sign-in would leave it), nothing of its own yet.
    std::fs::create_dir_all(home.join("workspaces/globex")).unwrap();
    let mut theirs: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(home.join("workspaces/default/sign-ins.json")).unwrap()).unwrap();
    for k in ["plane", "planes_more", "key"] { theirs.as_object_mut().unwrap().remove(k); }
    std::fs::write(home.join("workspaces/globex/sign-ins.json"), theirs.to_string()).unwrap();
    let (mut child, link) = dashboard(&home);
    b.goto(&format!("{}&w=globex", link));
    // Which CI: both, each named; the default workspace's opens that workspace, the other is used here.
    b.wait(&shows("Which CI?"));
    b.wait(&shows("GlobexCI"));
    let words = b.text("main");
    assert!(words.contains("GitHub globex") && (words.contains("Open Test") || words.contains("Open default")), "{words}");
    if let Ok(dir) = std::env::var("SUPERCI_PREVIEW_DIR") { b.screenshot(&std::path::Path::new(&dir).join("which-ci.png")) }
    b.click("main button", "Use this one");
    b.wait(&shows("This workspace looks after"));
    let kept: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(home.join("workspaces/globex/sign-ins.json")).unwrap()).unwrap();
    assert_eq!(kept["plane"]["Aws"]["plane_id"], "testplane0002", "{kept}");
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_secret_for_runs_from_a_computer_is_added_in_settings_listed_without_its_value_and_removed() {
    let Some(mut b) = Browser::open(1300, 900) else { eprintln!("no Chrome here: skipped"); return };
    let served = serve(|_| {});
    let home = signed_in(&served);
    let (mut child, link) = dashboard(&home);
    let kept = || served.aws.settings(PLANE).into_iter().filter(|(k, _)| k.starts_with("SECRET_")).collect::<Vec<_>>();
    b.goto(&link);
    b.wait(&page_shows("Usage"));
    b.click("nav a", "Settings");
    b.wait(&page_shows("None kept"));

    // Added in its dialog, for one repository's release branches.
    b.click(r#"button[onclick*="'add-secret'"]"#, "Add");
    b.wait("document.getElementById('add-secret').open");
    b.fill("#add-secret input[name=name]", "deploy_key");
    b.fill("#add-secret textarea[name=value]", "deploy-value-0001");
    b.eval("document.querySelector('#add-secret details').open = true");
    b.fill("#add-secret input[name=repo]", "acme/app");
    b.fill("#add-secret input[name=branch]", "release/*");
    b.click("#add-secret button.primary", "Add");
    b.wait(&page_shows("DEPLOY_KEY"));
    let settings = kept();
    assert!(settings.len() == 1 && settings[0].1.contains("deploy-value-0001") && settings[0].1.contains("release/*"), "{settings:?}");
    let main = b.text("body");
    assert!(main.contains("Repo acme/app, branch release/*") && main.contains("not given to a run yet") && !main.contains("deploy-value"), "{main}");
    // A name GitHub would refuse is refused here, and nothing changes.
    b.click(r#"button[onclick*="'add-secret'"]"#, "Add");
    b.wait("document.getElementById('add-secret').open");
    b.fill("#add-secret input[name=name]", "GITHUB_TOKEN");
    b.fill("#add-secret textarea[name=value]", "x");
    b.click("#add-secret button.primary", "Add");
    b.wait(&page_shows("That secret was not added"));
    assert_eq!(kept().len(), 1);
    b.click("a", "Back");
    b.wait(&shows("Usage"));
    b.click("nav a", "Settings");
    b.wait(&page_shows("DEPLOY_KEY"));

    // Removed.
    b.eval("window.confirm = () => true");
    b.click(r#"form[action="/secrets/remove"] button"#, "Remove");
    b.wait(&page_shows("None kept"));
    assert!(kept().is_empty());
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_jobs_page_says_what_it_had_from_the_caches_docker_images() {
    let Some(mut b) = Browser::open(1300, 900) else { eprintln!("no Chrome here: skipped"); return };
    let served = serve(|_| {});
    let home = home(&served);
    played(&served, &home.join("checkout"));
    // What its machine said after the job, on its record (see `layers-kept`).
    {
        let mut store = served.store.0.lock().unwrap();
        let key = store.keys().find(|k| k.starts_with("job:")).cloned().unwrap();
        let mut j: serde_json::Value = serde_json::from_str(&store[&key]).unwrap();
        j["images"] = serde_json::json!({ "served": 103, "served_bytes": 2_700_000_000u64, "pulled": 0, "pulled_bytes": 0, "uploaded": 0, "uploaded_bytes": 0, "at": 1 });
        store.insert(key, j.to_string());
    }
    let (mut child, link) = dashboard(&home);
    b.goto(&link);
    b.wait(&shows("Usage"));
    b.click("nav a", "Runs");
    b.wait(&shows("Recent runs"));
    b.click(".tabs a", "Jobs");
    b.wait(&shows("Recent jobs"));
    b.click("main a", "tests");
    b.wait(&shows("Docker images"));
    assert!(b.text("main").contains("103 layers from the cache (2.7 GB)"), "{}", b.text("main"));
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn keeping_docker_images_is_turned_off_and_on_in_settings() {
    let Some(mut b) = Browser::open(1300, 900) else { eprintln!("no Chrome here: skipped"); return };
    let served = serve(|c| {
        (c.aws_own, c.aws_own_creds) = (Some(("111111111111".into(), "us-east-1".into())), Some(superci_core::aws::Credentials { access_key_id: "ASIAX".into(), secret_access_key: "secret".into(), session_token: Some("tok".into()), expires_at_ms: u64::MAX }));
    });
    let home = signed_in(&served);
    let (mut child, link) = dashboard(&home);
    let routing = || served.aws.settings(PLANE).into_iter().find(|(k, _)| k == "ROUTING").map(|(_, v)| v).unwrap_or_default();
    b.goto(&link);
    b.wait(&shows("Usage"));
    b.click(".side a", "Settings");
    b.wait(&shows("A job's next run reads the images it pulled from your bucket"));
    b.click(r#"form:has(input[value="cache_images"]) button"#, "Turn off");
    until("images not kept", || routing().contains(r#""images_off":true"#));
    b.wait(&shows("Docker pulls every image as it always has"));
    b.click(r#"form:has(input[value="cache_images"]) button"#, "Turn on");
    until("images kept again", || !routing().contains("images_off"));
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}


#[test]
fn a_control_plane_from_before_the_keys_new_name_says_why_with_a_key_alone_and_is_updated_in_the_browser_once_signed_in() {
    let Some(mut b) = Browser::open(1300, 900) else { eprintln!("no Chrome here: skipped"); return };
    let served = serve(|_| {});
    let plane = served.plane.clone();
    // Its settings under their old names, as such a control plane keeps them.
    // (With a launcher in Cloudflare, which this workspace is not signed in to.)
    served.aws.put(&format!("/superci/{plane}/AGENTS"), r#"[{"cloud":"cloudflare","url":"https://superci-runners-testplane0001.acme.workers.dev"}]"#);
    *served.aws.old_version.lock().unwrap() = Some("0.19.12".into());
    // With this computer's key alone (no sign-in to hand it under the old name): its pages say why they do not load.
    let home = home(&served);
    let (mut child, link) = common::dashboard_with(&home, &[("SUPERCI_KEY_WAIT_S", "1")]);
    b.goto(&link);
    b.wait(&shows("Usage"));
    b.click("nav a", "Runs");
    b.wait(&shows("reads this computer's key under its old name"));
    assert!(b.eval("document.querySelector('main').innerText").as_str().unwrap().contains("superci planes update"));
    let _ = child.kill();
    // Signed in where it runs: the key handed under its old name, its pages read; updated from its page, it runs this
    // version, its settings under the new names.
    let home = signed_in(&served);
    // (AWS's answers to an update, as recorded.)
    served.aws.replaying(include_str!("../fixtures/aws/planes/update.jsonl"), &[("testplane0002", superci_testkit::PLANE), ("http://new-plane.test", &served.url)]);
    let (mut child, link) = dashboard(&home);
    b.goto(&link);
    b.wait(&shows("Usage"));
    b.click("nav a", "Runs");
    b.wait(&shows("No runs yet"));
    b.click(".side a", "Control plane");
    b.wait(&shows("Update to "));
    b.click("main button, main a", &format!("Update to {}", env!("CARGO_PKG_VERSION")));
    b.wait(&shows("Updating…"));
    // (Its new code uploaded and started, the converter beside it, the old names taken out: a while.)
    let names = || served.aws.settings(&plane).into_iter().map(|(k, _)| k).collect::<Vec<String>>();
    let end = std::time::Instant::now() + std::time::Duration::from_secs(120);
    while names().iter().any(|k| k == "AGENTS" || k.starts_with("DASHBOARD_KEY_")) { assert!(std::time::Instant::now() < end, "the update did not end: {:?}; {}", names(), b.eval("document.querySelector('#side-update').innerText")); std::thread::sleep(std::time::Duration::from_millis(300)) }
    assert!(served.aws.old_version.lock().unwrap().is_none() && names().iter().any(|k| k.starts_with("DEVICE_KEY_")), "{:?}", names());
    // Its launcher in Cloudflare was not updated with it, and that is said.
    assert!(names().iter().any(|k| k == "LAUNCHERS"), "its launchers under the new name: {:?}", names());
    b.wait(&page_shows("Its Cloudflare launcher was not updated"));
    b.wait(&format!("{} && !document.querySelector('main').innerText.includes('Update to ')", shows(env!("CARGO_PKG_VERSION"))));
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}


#[test]
fn a_cloudflare_sign_in_cloudflare_no_longer_takes_is_said_on_every_page_as_cloudflare_answered() {
    let Some(mut b) = Browser::open(1300, 900) else { eprintln!("no Chrome here: skipped"); return };
    // Its token refused (Cloudflare's 9109), and one due whose renewal Cloudflare refuses (invalid_grant): both over.
    for due in [false, true] {
        let served = serve(|_| {});
        let home = signed_in(&served);
        served.aws.replaying(include_str!("../fixtures/cloudflare/sign-in/ended.jsonl"), &[]);
        let file = home.join("workspaces/default/sign-ins.json");
        let mut kept: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        kept["cloudflare"] = serde_json::json!({ "access": "example-access_token-ended", "expires_at_ms": if due { 0u64 } else { 4102444800000u64 }, "refresh": "example-refresh_token-ended" });
        std::fs::write(&file, kept.to_string()).unwrap();
        let (mut child, link) = dashboard(&home);
        b.goto(&link);
        b.wait(&shows("Your Cloudflare sign-in has expired"));
        // The rest of the page as it is: the control plane in AWS is read, and a page of its own says it too.
        b.wait(&shows("Set up"));
        b.click(".side a", "Control plane");
        b.wait(&format!("{} && {}", shows("Your Cloudflare sign-in has expired"), shows("AWS · 111111111111")));
        assert!(b.eval("[...document.querySelectorAll('main a, main button')].some(e => e.innerText.includes('Sign in with Cloudflare'))") == true, "due {due}");
        // Something that needs Cloudflare, asked for now: said that the sign-in ended, nothing done.
        let (got, text) = post(&mut b, "/runners/cloudflare", &[]);
        assert!(got >= 400 && text.contains("Cloudflare sign-in has expired"), "due {due}: {got} {text}");
        assert!(served.aws.unanswered.lock().unwrap().is_empty(), "{:?}", served.aws.unanswered.lock().unwrap());
        let _ = child.kill();
        let _ = std::fs::remove_dir_all(&home);
    }
}


#[test]
fn another_control_plane_in_the_account_is_deleted_from_its_menu_and_the_one_in_use_stays() {
    let Some(mut b) = Browser::open(1300, 900) else { eprintln!("no Chrome here: skipped"); return };
    // The one in use, and another beside it in the same account, AWS answering as it did a real account.
    let served = serve(|_| {});
    let other = superci_testkit::serve_as("testplane0002", |_| {});
    let home = signed_in(&served);
    served.aws.replaying(include_str!("../fixtures/aws/planes/delete.jsonl"), &[("http://new-plane.test", &other.url)]);
    for (k, v) in other.aws.settings("testplane0002") { served.aws.put(&format!("/superci/testplane0002/{k}"), &v) }
    let (mut child, link) = dashboard(&home);
    b.goto(&link);
    b.wait(&shows("Set up"));
    b.click(".side a", "Control plane");
    b.wait(&shows(&other.url));
    // Its menu (the one in use has no Delete in its own), then its dialog.
    b.eval("[...document.querySelectorAll('details.menu')].find(m => m.querySelector('.menu-pop').textContent.includes('Delete')).querySelector('summary').click(); true");
    b.click("details.menu[open] .menu-pop button", "Delete…");
    b.wait("document.querySelector('dialog[open]') !== null");
    assert!(b.eval("document.querySelector('dialog[open]').innerText").as_str().unwrap().contains("its function, table, schedule, parameters and role"));
    b.click("dialog[open] button.danger", "");
    // Gone from the page and from AWS; the one in use keeps everything.
    b.wait(&format!("!document.querySelector('main').innerText.includes({:?})", other.url));
    until("its settings deleted", || served.aws.settings("testplane0002").is_empty());
    assert!(!served.aws.settings(superci_testkit::PLANE).is_empty());
    let asked: Vec<String> = served.aws.asked.lock().unwrap().iter().map(|a| format!("{} {}", a["method"].as_str().unwrap(), a["url"].as_str().unwrap())).collect();
    assert!(asked.iter().any(|a| a == "DELETE https://lambda.us-east-1.amazonaws.com/2015-03-31/functions/superci-plane-testplane0002"), "{asked:?}");
    assert!(!asked.iter().any(|a| a.contains("functions/superci-plane-testplane0001") && a.starts_with("DELETE")), "{asked:?}");
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_modal_sign_in_modal_no_longer_takes_is_said_on_every_page_as_modal_answered() {
    let Some(mut b) = Browser::open(1300, 900) else { eprintln!("no Chrome here: skipped"); return };
    let served = serve(|_| {});
    let home = signed_in(&served);
    // (Modal's answer to a token it does not know: Unauthenticated, "Token not found".)
    served.aws.replaying(include_str!("../fixtures/modal/sign-in/ended.jsonl"), &[]);
    let file = home.join("workspaces/default/sign-ins.json");
    let mut kept: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    kept["modal"] = serde_json::json!({ "token_id": "ak-superciendedrecording00", "token_secret": "as-superciendedrecording00", "workspace": "acmeci" });
    std::fs::write(&file, kept.to_string()).unwrap();
    let (mut child, link) = dashboard(&home);
    b.goto(&link);
    b.wait(&shows("Modal no longer accepts your sign-in"));
    // The control plane in AWS is read all the same, and its page says it too.
    b.wait(&shows("Set up"));
    b.click(".side a", "Control plane");
    b.wait(&format!("{} && {}", shows("Modal no longer accepts your sign-in"), shows("AWS · 111111111111")));
    // Something that needs Modal, asked for now: said that the sign-in ended, nothing done.
    let (got, text) = post(&mut b, "/runners/modal", &[]);
    assert!(got >= 400 && text.contains("Modal no longer accepts"), "{got} {text}");
    assert!(served.aws.unanswered.lock().unwrap().is_empty(), "{:?}", served.aws.unanswered.lock().unwrap());
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

/// A form posted from the page itself (as a person who edited its fields could): the answer's status and words.
fn post(b: &mut Browser, path: &str, fields: &[(&str, &str)]) -> (u16, String) {
    let body = fields.iter().map(|(k, v)| format!("{}={}", k, v.replace('%', "%25").replace('&', "%26").replace('=', "%3D").replace(' ', "%20").replace('+', "%2B").replace('@', "%40"))).collect::<Vec<_>>().join("&");
    let got = b.eval(&format!("fetch({path:?}, {{method: 'POST', credentials: 'same-origin', redirect: 'manual', headers: {{'content-type': 'application/x-www-form-urlencoded'}}, body: {body:?}}}).then(async r => [r.status, r.type === 'opaqueredirect' ? 'redirected' : (await r.text()).replace(/<(style|script)[^>]*>[\\s\\S]*?<\\/\\1>/g, ' ').replace(/<[^>]*>/g, ' ').replace(/&#39;/g, '\\x27').replace(/&quot;/g, '\\x22').replace(/&lt;/g, '<').replace(/&gt;/g, '>').replace(/&amp;/g, '&').replace(/\\s+/g, ' ')])"));
    (got[0].as_u64().unwrap_or(0) as u16, got[1].as_str().unwrap_or_default().to_string())
}

#[test]
fn what_settings_forms_must_not_take_is_refused_or_left_out_and_nothing_else_changes() {
    let Some(mut b) = Browser::open(1300, 900) else { eprintln!("no Chrome here: skipped"); return };
    let served = serve(|c| {
        (c.aws_own, c.aws_own_creds) = (Some(("111111111111".into(), "us-east-1".into())), Some(superci_core::aws::Credentials { access_key_id: "ASIAX".into(), secret_access_key: "secret".into(), session_token: Some("tok".into()), expires_at_ms: u64::MAX }));
    });
    served.connect_github();
    let home = signed_in(&served);
    let (mut child, link) = dashboard(&home);
    let routing = || served.aws.settings(PLANE).into_iter().find(|(k, _)| k == "ROUTING").and_then(|(_, v)| serde_json::from_str::<serde_json::Value>(&v).ok()).unwrap_or_default();
    b.goto(&link);
    b.wait(&shows("Usage"));
    b.click(".side a", "Settings");
    b.wait(&shows("Limits"));
    // Refused, saying what it takes.
    for (fields, said) in [
        (&[("action", "account_usd"), ("account", "gcp"), ("usd", "10")][..], "Which account?"),
        (&[("action", "account_usd"), ("account", "aws"), ("usd", "10"), ("budget", "sometimes")], "What AWS does at the limit is none, emails or stops"),
        (&[("action", "account_usd"), ("account", "aws"), ("usd", "10"), ("budget", "lift")], "There is no budget in AWS to lift"),
        (&[("action", "account_usd"), ("account", "aws"), ("usd", ""), ("budget", "emails")], "A budget in AWS needs the monthly limit"),
        (&[("action", "account_usd"), ("account", "aws"), ("usd", "40"), ("budget", "emails"), ("email", "not an address")], "That is no email address"),
        (&[("action", "add"), ("repo", "not a repository"), ("cloud", "aws")], "A repository is owner/name, owner/* or *"),
        (&[("action", "max_hours"), ("max_hours", "0")], "From 1 to 120 hours"),
        (&[("action", "max_hours"), ("max_hours", "121")], "From 1 to 120 hours"),
        (&[("action", "public_add"), ("repo", "acme")], "A repository is owner/name or owner/*"),
        (&[("action", "public_set"), ("repo", "acme/app"), ("repo", "no slash")], "A repository is owner/name"),
        (&[("action", "cache_days"), ("days", "0")], "days"),
        (&[("action", "order"), ("cloud_0", "elsewhere")], "Unknown provider"),
        (&[("action", "order"), ("cloud_0", "aws"), ("cloud_1", "aws")], "Unknown provider"),
        (&[("action", "order"), ("cloud_0", "aws"), ("out_0", "elsewhere")], "Unknown provider"),
        (&[("action", "order"), ("out_0", "aws")], "Keep one provider in the order"),
        (&[("action", "take_out"), ("cloud", "elsewhere")], "Unknown provider"),
        (&[("action", "pool"), ("cloud", "elsewhere")], "Unknown provider"),
        (&[("action", "pool"), ("cloud", "aws"), ("region_0", "mars-north-1")], "Choose a region from the list"),
        (&[("action", "account"), ("cloud", "aws"), ("networks", "not networks")], "Check your networks"),
        (&[("action", "pool"), ("cloud", "cloudflare"), ("image", "ftp://example.com/image")], "https://"),
        (&[("action", "nothing-of-the-kind")], "Unknown change"),
    ] {
        let before = routing();
        let (status, text) = post(&mut b, "/routing", fields);
        assert!(status >= 400 && text.contains(said), "{fields:?}: {status} {text}");
        assert_eq!(routing(), before, "{fields:?}: nothing changed");
    }
    // Left out: a limit that is no amount, a largest machine out of range (or the standard one).
    for usd in ["-5", "abc", "0"] {
        post(&mut b, "/routing", &[("action", "monthly_usd"), ("usd", usd)]);
        until("the limit left out", || routing().get("monthly_usd").is_none_or(|v| v.is_null()));
    }
    for cpu in ["0", "500", "nope", "32"] {
        post(&mut b, "/routing", &[("action", "max_cpu"), ("max_cpu", cpu)]);
        until("no largest machine", || routing().get("max_cpu").is_none_or(|v| v.is_null()));
    }
    post(&mut b, "/routing", &[("action", "max_cpu"), ("max_cpu", "16")]);
    until("the largest machine", || routing()["max_cpu"] == 16);
    // A rule for every repository, and taken out; a public repository added once however often it is asked.
    post(&mut b, "/routing", &[("action", "add"), ("repo", "*"), ("cloud", "aws")]);
    until("the rule", || routing()["rules"].as_array().is_some_and(|r| r.iter().any(|r| r["repo"] == "*")));
    post(&mut b, "/routing", &[("action", "remove"), ("repo", "*")]);
    until("the rule gone", || !routing()["rules"].as_array().is_some_and(|r| r.iter().any(|r| r["repo"] == "*")));
    // A provider's settings: what is no number left out, a region named twice once, Cloudflare's location.
    let said = post(&mut b, "/routing", &[("action", "pool"), ("cloud", "aws"), ("max", "0"), ("usd", "-1"), ("region_0", "us-east-1"), ("region_1", ""), ("region_2", "us-east-1")]);
    assert_eq!(said.1, "redirected", "{said:?}");
    until("AWS's settings", || routing()["order"].as_array().is_some_and(|o| o.iter().any(|p| p["cloud"] == "aws" && p["max_jobs"].is_null() && p["monthly_usd"].is_null() && p["regions"] == serde_json::json!(["us-east-1"]))));
    post(&mut b, "/routing", &[("action", "pool"), ("cloud", "aws"), ("max", "1000"), ("usd", "abc")]);
    post(&mut b, "/routing", &[("action", "pool"), ("cloud", "aws"), ("max", "5"), ("usd", "20")]);
    until("AWS's limits", || routing()["order"].as_array().is_some_and(|o| o.iter().any(|p| p["cloud"] == "aws" && p["max_jobs"] == 5 && p["monthly_usd"] == 20.0)));
    post(&mut b, "/routing", &[("action", "pool"), ("cloud", "cloudflare"), ("location", "weur"), ("image", "")]);
    until("Cloudflare's location", || served.aws.settings(PLANE).iter().any(|(k, v)| k == "CF_LOCATION" && v == "weur"));
    // The last provider in the order is not taken out.
    let only = routing()["order"].as_array().map(|o| o.iter().filter(|p| p["off"] != true).count()).unwrap_or(0);
    if only == 1 {
        let cloud = routing()["order"].as_array().unwrap().iter().find(|p| p["off"] != true).unwrap()["cloud"].as_str().unwrap().to_string();
        let (status, text) = post(&mut b, "/routing", &[("action", "take_out"), ("cloud", &cloud)]);
        assert!(status >= 400 && text.contains("Keep one provider in the order"), "{status} {text}");
    }
    for _ in 0..2 { post(&mut b, "/routing", &[("action", "public_add"), ("repo", "acme/site")]); }
    until("the public repository", || routing()["public_repos"].as_array().is_some_and(|r| r.iter().filter(|r| r.as_str().is_some_and(|s| s.eq_ignore_ascii_case("acme/site"))).count() == 1));
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn another_control_plane_s_row_says_what_can_be_done_with_it_as_it_stands() {
    let Some(mut b) = Browser::open(1300, 900) else { eprintln!("no Chrome here: skipped"); return };
    // (The other one's address: served, or nothing answering there.)
    for (case, said) in [("older", "Update to "), ("in use older", "Update the one in use first"), ("update fails", "The update stopped"), ("key refused", "reads this computer's key under its old name")] {
        let served = serve(|_| {});
        // (The other one reads its settings where the account keeps them, as a real one does: the key handed to it too.)
        let aws = served.aws.clone();
        // (Or, "key refused", its own: the key handed to it never reaches it.)
        let shared = case != "key refused";
        let other = superci_testkit::serve_as("testplane0002", move |c| { let s = aws.settings("testplane0002"); if shared && !s.is_empty() { c.read_settings(&s) } });
        let home = signed_in(&served);
        let url = if case == "not answering" { "http://127.0.0.1:9".to_string() } else { other.url.clone() };
        served.aws.replaying(include_str!("../fixtures/aws/planes/delete.jsonl"), &[("http://new-plane.test", &url)]);
        for (k, v) in other.aws.settings("testplane0002") { served.aws.put(&format!("/superci/testplane0002/{k}"), &v) }
        match case { "older" | "update fails" | "key refused" => *other.aws.old_version.lock().unwrap() = Some("0.9.2".into()), "in use older" => *served.aws.old_version.lock().unwrap() = Some("0.9.2".into()), _ => {} }
        let (mut child, link) = dashboard(&home);
        b.goto(&link);
        b.wait(&page_shows("Overview"));
        b.click(".side a", "Control plane");
        b.wait(&shows(&url));
        if case == "update fails" || case == "key refused" {
            // (AWS has no answer for the update here: it stops, and says why, in its row.)
            b.eval(&format!("[...document.querySelectorAll('main .pick, main .row, main li, main div')].reverse().find(r => r.innerText.includes({:?}) && [...r.querySelectorAll('button')].some(x => x.innerText.startsWith('Update to'))).querySelector('button').click(); true", url));
        }
        b.wait(&shows(said));
        let _ = child.kill();
        let _ = std::fs::remove_dir_all(&home);
    }
}

#[test]
fn everything_a_ci_has_comes_along_when_it_moves_to_another_control_plane_from_the_browser() {
    let Some(mut b) = Browser::open(1300, 900) else { eprintln!("no Chrome here: skipped"); return };
    // The one in use with all it can carry: GitHub and GitLab, a name, labels, Cloudflare's location and image, its
    // own networks in AWS, a secret for runs from a computer. Another in the same account, reading its settings there.
    let served = serve(|_| {});
    served.connect_github();
    served.connect_gitlab();
    let plane = PLANE;
    for (k, v) in [("NAME", "AcmeCI"), ("LABELS", r#"["acmeci","acmeci-big"]"#), ("CF_LOCATION", "weur"), ("CF_IMAGE", "https://images.example.com/full:1"),
        ("AWS_NETWORKS", r#"{"us-east-1":{"subnets":["subnet-0123456789abcdef0"],"security_groups":["sg-0123456789abcdef0"]}}"#), ("CONTAINERS", "off"),
        // (A second GitLab, under its own id.)
        ("GITLAB_gsecond", r#"{"url":"https://gitlab.com","token":"gitlab-token-for-tests","hook_secret":"a-hook-secret-for-tests","id":"gsecond"}"#)] {
        served.aws.put(&format!("/superci/{plane}/{k}"), v);
    }
    let aws = served.aws.clone();
    let new = superci_testkit::serve_as("testplane0002", move |c| c.read_settings(&aws.settings("testplane0002")));
    for (k, v) in new.aws.settings("testplane0002") { served.aws.put(&format!("/superci/testplane0002/{k}"), &v) }
    let home = signed_in(&served);
    let o = common::command(&home).args(["secrets", "create", "NPM_TOKEN", "--value-env=THE_VALUE"]).env("THE_VALUE", "npm-token-for-tests").output().unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    served.aws.replaying(include_str!("../fixtures/aws/planes/delete.jsonl"), &[("http://new-plane.test", &new.url)]);
    let (mut child, link) = dashboard(&home);
    b.goto(&link);
    b.wait(&page_shows("Overview"));
    b.click(".side a", "Control plane");
    b.wait(&shows(&new.url));
    b.click("main button", "Move here");
    b.wait("document.querySelector('dialog[open]') !== null");
    b.click("dialog[open] button.primary", "");
    // The new one, in use, has it all.
    let has = |k: &str| served.aws.settings("testplane0002").into_iter().find(|(n, _)| n == k).map(|(_, v)| v);
    let end = std::time::Instant::now() + std::time::Duration::from_secs(90);
    while served.store.under("moved_to").is_empty() { assert!(std::time::Instant::now() < end, "moved: {:?}; page {}", served.aws.settings("testplane0002").iter().map(|(k, _)| k.clone()).collect::<Vec<_>>(), b.eval("document.querySelector('main').innerText.slice(0, 600)")); std::thread::sleep(std::time::Duration::from_millis(300)) }
    for k in ["GITHUB_APP", "GITLAB", "GITLAB_gsecond", "NAME", "LABELS", "CF_LOCATION", "CF_IMAGE", "AWS_NETWORKS"] { assert!(has(k).is_some(), "{k}: {:?}", served.aws.settings("testplane0002")) }
    assert_eq!(has("NAME").as_deref(), Some("AcmeCI"));
    assert!(served.aws.settings("testplane0002").iter().any(|(k, _)| k.starts_with("SECRET_")), "the secret came along");
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn an_address_edited_by_hand_is_read_only_as_far_as_it_holds_what_it_may() {
    let Some(mut b) = Browser::open(1300, 900) else { eprintln!("no Chrome here: skipped"); return };
    let served = serve(|_| {});
    let home = home(&served);
    played(&served, &home.join("checkout"));
    let (mut child, link) = dashboard(&home);
    b.goto(&link);
    b.wait(&shows("Usage"));
    let get = |b: &mut Browser, path: &str| -> (u16, String) {
        let got = b.eval(&format!("fetch({path:?}, {{credentials: 'same-origin'}}).then(async r => [r.status, (await r.text()).replace(/<(style|script)[^>]*>[\\s\\S]*?<\\/\\1>/g, ' ').replace(/<[^>]*>/g, ' ').replace(/\\s+/g, ' ')])"));
        (got[0].as_u64().unwrap_or(0) as u16, got[1].as_str().unwrap_or_default().to_string())
    };
    // A run's and a job's address with what they may not hold: the rest is read, and nothing is found by it.
    let (status, text) = get(&mut b, "/?p=run&repo=acme/my-app_1.x%20%3Cb%3E&id=12a3&fragment=1");
    assert!(status == 200 && !text.contains("<b>"), "{text}");
    let (status, text) = get(&mut b, "/?p=job&id=9x9&gl=ab-1Z%21&fragment=1");
    assert!(status == 200 && text.contains("Not among the latest jobs"), "{text}");
    let (status, _) = get(&mut b, "/?p=gitlab&g=ab1Z%21&fragment=1");
    assert_eq!(status, 200);
    // A control plane named in the address with what an id may not hold is no name: the page as without it.
    for c in ["UPPER", "a234567890123456789012345", "has-dash"] {
        let (status, text) = get(&mut b, &format!("/?c={c}&fragment=1"));
        assert!(status == 200 && text.contains("Usage"), "{c}: {text}");
    }
    // One that holds only what it may, and is not known here: an action for it changes nothing and says so.
    let (status, text) = post(&mut b, "/labels?c=nosuchplane1", &[("labels", "superci")]);
    assert!(status == 404 && text.contains("That control plane is not known here"), "{status} {text}");
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}


#[test]
fn the_github_app_installed_everywhere_on_some_repositories_or_nowhere_yet_is_said_as_it_is() {
    let Some(mut b) = Browser::open(1300, 900) else { eprintln!("no Chrome here: skipped"); return };
    for (selected, nowhere, said) in [(false, false, "Jobs from acme (all repositories)"), (true, false, "Jobs from acme (selected repositories)"), (false, true, "install it where your runners should take jobs")] {
        let served = serve(|_| {});
        served.connect_github();
        served.github.set_installed(selected, nowhere);
        let home = home(&served);
        let (mut child, link) = dashboard(&home);
        b.goto(&link);
        // (Not installed: Overview's step for it says so, its button to Repositories.)
        if nowhere { b.wait(&shows("it still has to be installed where your runners should take jobs")) } else { b.wait(&shows("Usage")) }
        b.click("nav a", "Repositories");
        b.wait(&shows(said));
        let _ = child.kill();
        let _ = std::fs::remove_dir_all(&home);
    }
}

#[test]
fn removing_a_runner_provider_is_refused_while_it_runs_jobs_or_its_cloud_is_not_signed_in_and_says_what_is_left() {
    let Some(mut b) = Browser::open(1300, 900) else { eprintln!("no Chrome here: skipped"); return };
    // A control plane whose own AWS account runs jobs and with Cloudflare's containers too, GitHub connected, a job
    // running on Cloudflare.
    let served = serve(|c| {
        c.containers = true;
        (c.aws_own, c.aws_own_creds) = (Some(("111111111111".into(), "us-east-1".into())), Some(superci_core::aws::Credentials { access_key_id: "ASIAX".into(), secret_access_key: "secret".into(), session_token: Some("tok".into()), expires_at_ms: u64::MAX }));
    });
    served.connect_github();
    let (id, queued) = served.github.queue("test", "superci/provider=cloudflare", served.now_ms());
    assert_eq!(served.webhook(queued), 202);
    assert_eq!(served.webhook(served.github.start(id)), 202);
    // With this computer's key alone (no cloud signed in).
    let home = home(&served);
    let (mut child, link) = dashboard(&home);
    b.goto(&link);
    b.wait(&shows("Usage"));
    b.click("nav a", "Runners");
    b.wait(&shows("Runner providers"));
    for (fields, status, said) in [
        (&[("cloud", "cloudflare"), ("plane", "anotherplane")][..], 409, "Another control plane is in use now"),
        (&[("cloud", "elsewhere")], 400, "Unknown provider"),
        (&[("cloud", "cloudflare")], 409, "1 job is running on Cloudflare"),
        (&[("cloud", "aws")], 403, "Sign in with AWS first"),
    ] {
        let (got, text) = post(&mut b, "/runners/remove", fields);
        assert!(got == status && text.contains(said), "{fields:?}: {got} {text}");
    }
    // Its update likewise: not without the sign-in where it runs.
    let (got, text) = post(&mut b, "/plane/update", &[]);
    assert!(got == 403 && text.contains("Sign in with AWS to update it"), "{got} {text}");
    // Signed in, AWS from SuperCI only: forgotten here, and what is left in the account said.
    let _ = child.kill();
    let home = signed_in(&served);
    let (mut child, link) = dashboard(&home);
    b.goto(&link);
    b.wait(&shows("Usage"));
    // Signed in, its update waits for the job running on Cloudflare (a new setup would stop it).
    let (got, text) = post(&mut b, "/plane/update", &[]);
    assert!(got == 409 && text.contains("1 job is running on Cloudflare"), "{got} {text}");
    let (got, text) = post(&mut b, "/runners/remove", &[("cloud", "aws"), ("only", "on")]);
    assert!(got < 400, "{got} {text}");
    until("AWS forgotten", || served.aws.settings(PLANE).iter().any(|(k, v)| k == "AWS_RUNNERS" && v == "off"));
    b.goto(&format!("{link}&p=runners"));
    b.wait(&page_shows("the role superci-plane-"));
    // Cloudflare with its running job stopped: its containers off.
    let (got, text) = post(&mut b, "/runners/remove", &[("cloud", "cloudflare"), ("stop", "on")]);
    assert!(got < 400, "{got} {text}");
    until("Cloudflare's containers off", || served.aws.settings(PLANE).iter().any(|(k, v)| k == "CONTAINERS" && v == "off"));
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}


#[test]
fn a_day_of_github_and_gitlab_jobs_is_summed_up_on_overview_for_a_day_a_week_and_a_month() {
    let Some(mut b) = Browser::open(1300, 900) else { eprintln!("no Chrome here: skipped"); return };
    // A day's jobs: from GitHub a short pass, a failure, a two-hour pass and one still starting; from GitLab a pass.
    let served = serve(|_| {});
    served.connect_github();
    served.connect_gitlab();
    let gh = |name: &str, end: Option<&str>, ms: u64| { let (id, q) = served.github.queue(name, "superci", served.now_ms()); served.webhook(q); if let Some(e) = end { served.webhook(served.github.start(id)); served.later(ms); served.webhook(served.github.finish(id, e, "log", served.now_ms())); } };
    gh("build", Some("success"), 120_000);
    gh("test", Some("failure"), 30_000);
    gh("lint", Some("success"), 7_000_000);
    let (gid, p) = served.gitlab.pending("gl-test", &["superci"], served.now_ms());
    served.gitlab_webhook(p);
    served.gitlab_webhook(served.gitlab.running(gid, served.now_ms()));
    served.later(60_000);
    served.gitlab_webhook(served.gitlab.finish(gid, "success", "ok", served.now_ms()));
    gh("stuck", None, 0);
    let home = signed_in(&served);
    let (mut child, link) = dashboard(&home);
    b.goto(&link);
    b.wait(&format!("{} && {} && {} && {}", reads("5 jobs"), reads("4 ran"), reads("as GitHub and GitLab bill them"), reads("saved")));
    assert!(b.eval("document.querySelector('main').innerText").as_str().unwrap().contains("machine minutes"));
    for (period, word) in [("7 days", "7 days"), ("30 days", "30 days")] {
        b.click("main a, main button", period);
        b.wait(&format!("{} && !document.body.classList.contains('busy')", shows(word)));
        b.wait(&reads("5 jobs"));
    }
    b.click("nav a", "Runs");
    b.wait(&format!("{} && {}", shows("Pipeline"), shows("Failed")));
    b.click(".tabs a", "Jobs");
    b.wait(&format!("{} && {} && {}", shows("gl-test"), shows("Starting"), shows("116m")));
    // A GitLab job's page: its log, read from GitLab, in steps.
    b.click("main a", "gl-test");
    b.wait("document.querySelector('details.lstep')");
    b.click("details.lstep summary", "");
    b.wait("[...document.querySelectorAll('.ll .lx')].some(l => l.textContent.includes('ok'))");
    // A GitHub job's: read from GitHub once it ended.
    b.click("nav a", "Runs");
    b.click(".tabs a", "Jobs");
    b.wait(&shows("lint"));
    b.click("main a", "lint");
    b.wait("document.querySelector('details.lstep')");
    // Limits below what the day spent: at its limit, for the month and the day.
    let (got, text) = post(&mut b, "/routing", &[("action", "monthly_usd"), ("usd", "0.5"), ("daily", "0.25")]);
    assert!(got < 400, "{got} {text}");
    b.goto(&format!("{link}&p=runners"));
    b.wait(&format!("{} && {}", reads("$0.80 of $0.50 At its limit"), reads("$0.80 of $0.25 today At its limit")));
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}


#[test]
fn control_planes_are_set_up_in_cloudflare_and_modal_from_the_browser_and_deleted_from_their_menus_as_both_answered() {
    let Some(mut b) = Browser::open(1300, 900) else { eprintln!("no Chrome here: skipped"); return };
    // The CI's control plane in AWS, signed in to Cloudflare and Modal too; the Worker and Modal's app, once deployed,
    // served here, reading the secrets they were given.
    let served = serve(|_| {});
    served.connect_github();
    let aws = served.aws.clone();
    let cf = superci_testkit::serve_as("cfplane00003", move |c| {
        let settings = aws.worker_settings("superci");
        c.read_settings(&settings);
        if let Some((_, on)) = settings.iter().find(|(k, _)| k == "CONTAINERS") { c.containers = on == "on" }
    });
    cf.aws.params.lock().unwrap().remove("/superci/cfplane00003/CONTAINERS");
    let aws = served.aws.clone();
    let md = superci_testkit::serve_as("mdplane00003", move |c| { c.read_settings(&aws.modal_plane_settings()); c.own_cloud = "modal".into(); c.containers = false; });
    served.aws.workers.lock().unwrap().insert("superci.acme.workers.dev".into(), cf.url.clone());
    served.aws.workers.lock().unwrap().insert("acmeorg--superci-plane-mdplane00003.modal.run".into(), md.url.clone());
    let home = signed_in(&served);
    let file = home.join("workspaces/default/sign-ins.json");
    let mut kept: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    kept["cloudflare"] = serde_json::json!({ "access": "cloudflare-access-for-tests", "expires_at_ms": 4102444800000u64, "refresh": "cloudflare-refresh-for-tests" });
    kept["modal"] = serde_json::json!({ "token_id": "ak-modal-for-tests", "token_secret": "as-modal-for-tests", "workspace": "acmeorg" });
    std::fs::write(&file, kept.to_string()).unwrap();
    let fixture = |name: &str| std::fs::read_to_string(format!("{}/fixtures/{name}.jsonl", env!("CARGO_MANIFEST_DIR"))).unwrap();
    served.aws.replaying(&[fixture("cloudflare/sign-in/usage"), fixture("modal/05-dashboard")].concat(), &[]);
    // In Cloudflare: its row's Set up, its progress, then it is listed.
    served.aws.replaying(&fixture("update/01-deploy-cf"), &[]);
    let (mut child, link) = common::dashboard_with(&home, &[("SUPERCI_NEW_PLANE_ID", "cfplane00003")]);
    b.goto(&link);
    b.wait(&page_shows("Overview"));
    b.click(".side a", "Control plane");
    b.wait(&shows("Set up"));
    b.click("form[action=\"/plane/cloudflare\"] button", "Set up");
    // (Deployed: its row, with what can be done with it.)
    let end = std::time::Instant::now() + std::time::Duration::from_secs(120);
    while !common::words(&common::part(&link, "/?p=planes&fragment=1").1).contains("superci.acme.workers.dev Move here") { assert!(std::time::Instant::now() < end, "Cloudflare's: {}", b.eval("document.querySelector('main').innerText.slice(0, 500)")); std::thread::sleep(std::time::Duration::from_millis(500)) }
    let _ = child.kill();
    // In Modal likewise.
    served.aws.replaying(&fixture("update/02-deploy-modal"), &[]);
    let (mut child, link) = common::dashboard_with(&home, &[("SUPERCI_NEW_PLANE_ID", "mdplane00003")]);
    b.goto(&format!("{link}&p=planes"));
    b.wait(&shows("superci.acme.workers.dev"));
    b.click("form[action=\"/plane/modal\"] button", "Set up");
    let end = std::time::Instant::now() + std::time::Duration::from_secs(120);
    while !common::words(&common::part(&link, "/?p=planes&fragment=1").1).contains("modal.run Move here") { assert!(std::time::Instant::now() < end, "Modal's: {}", common::words(&common::part(&link, "/?p=planes&fragment=1").1).chars().take(800).collect::<String>()); std::thread::sleep(std::time::Duration::from_millis(500)) }
    // Each one's own pages: opened by its id.
    for id in ["cfplane00003", "mdplane00003"] {
        b.goto(&format!("{link}&c={id}&p=add-runners"));
        b.wait(&page_shows("Add a provider"));
    }
    // Each updated (to this version again), as both clouds answered: the Worker uploaded again, Modal's app deployed again.
    for (step, id) in [("update/05-update-cf", "cfplane00003"), ("update/06-update-md", "mdplane00003")] {
        served.aws.replaying(&fixture(step), &[]);
        b.goto(&format!("{link}&p=planes"));
        b.wait(&shows("Move here"));
        let (got, text) = post(&mut b, "/plane/update", &[("plane", id)]);
        assert!(got < 400, "{id}: {got} {text}");
        let end = std::time::Instant::now() + std::time::Duration::from_secs(90);
        while common::words(&common::part(&link, "/?p=planes&fragment=1").1).contains("Updating") { assert!(std::time::Instant::now() < end, "{id} updated"); std::thread::sleep(std::time::Duration::from_millis(500)) }
        assert!(!common::words(&common::part(&link, "/?p=planes&fragment=1").1).contains("The update stopped"), "{}", common::words(&common::part(&link, "/?p=planes&fragment=1").1));
    }
    // Each deleted from its menu: gone from the list, and from its cloud.
    for (step, row) in [("update/07-delete-cf", "superci.acme.workers.dev"), ("update/08-delete-md", "modal.run")] {
        served.aws.replaying(&fixture(step), &[]);
        b.goto(&format!("{link}&p=planes"));
        b.wait(&shows(row));
        b.eval(&format!("[...document.querySelectorAll('details.menu')].find(m => m.closest('.pick, .row, li, div') && m.parentElement.closest('div').innerText.includes({row:?}) && m.querySelector('.menu-pop').textContent.includes('Delete')).querySelector('summary').click(); true"));
        b.click("details.menu[open] .menu-pop button", "Delete…");
        b.wait("document.querySelector('dialog[open]') !== null");
        b.click("dialog[open] button.danger", "");
        let end = std::time::Instant::now() + std::time::Duration::from_secs(90);
        while common::words(&common::part(&link, "/?p=planes&fragment=1").1).contains(&format!("{row} Move here")) { assert!(std::time::Instant::now() < end, "{row} deleted"); std::thread::sleep(std::time::Duration::from_millis(500)) }
    }
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}


#[test]
fn the_aws_accounts_own_budget_set_raised_made_to_stop_lifted_and_removed_shows_on_the_limits_card() {
    let Some(mut b) = Browser::open(1300, 900) else { eprintln!("no Chrome here: skipped"); return };
    let served = serve(|c| {
        (c.aws_own, c.aws_own_creds) = (Some(("111111111111".into(), "us-east-1".into())), Some(superci_core::aws::Credentials { access_key_id: "ASIAX".into(), secret_access_key: "secret".into(), session_token: Some("tok".into()), expires_at_ms: u64::MAX }));
    });
    let home = signed_in(&served);
    // Each step as a real account answered it (`fixtures/aws/budget`).
    let step = |file: &str| served.aws.replaying(&std::fs::read_to_string(format!("{}/fixtures/aws/budget/{file}", env!("CARGO_MANIFEST_DIR"))).unwrap(), &[("abcdefghijkl", superci_testkit::PLANE)]);
    let (mut child, link) = dashboard(&home);
    b.goto(&link);
    b.wait(&shows("Usage"));
    b.click("nav a", "Runners");
    b.wait(&shows("Limits"));
    let account = |usd: &str, budget: &str| -> Vec<(String, String)> { let mut f = vec![("action".to_string(), "account_usd".to_string()), ("account".into(), "aws".into()), ("usd".into(), usd.into())]; if !budget.is_empty() { f.push(("budget".into(), budget.into())) } f };
    for (file, fields, card, gone) in [
        ("02-emails.jsonl", { let mut f = account("100", "emails"); f.push(("email".into(), "someone@example.com".into())); f }, "$0.00 of $100.00 AWS emails", ""),
        ("04-raise.jsonl", account("120", ""), "$0.00 of $120.00 AWS emails", ""),
        ("06-stop.jsonl", account("120", "stops"), "AWS stops at the limit", ""),
        ("08-lift.jsonl", account("120", "lift"), "AWS stops at the limit", ""),
        ("09-remove.jsonl", account("120", "none"), "$0.00 of $120.00", "AWS emails"),
    ] {
        step(file);
        let (got, text) = post(&mut b, "/routing", &fields.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect::<Vec<_>>());
        assert!(got < 400, "{file}: {got} {text}");
        b.goto(&format!("{link}&p=runners"));
        b.wait(&reads(card));
        if !gone.is_empty() { assert!(!b.eval("document.querySelector('main').innerText").as_str().unwrap().replace('\n', " ").contains(gone), "{file}") }
    }
    assert!(served.aws.unanswered.lock().unwrap().is_empty(), "{:?}", served.aws.unanswered.lock().unwrap());
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn the_runner_image_is_copied_into_another_region_from_the_aws_accounts_section_as_aws_answered() {
    let Some(mut b) = Browser::open(1300, 900) else { eprintln!("no Chrome here: skipped"); return };
    let served = serve(|_| {});
    let home = signed_in(&served);
    let (mut child, link) = dashboard(&home);
    b.goto(&link);
    b.wait(&page_shows("Overview"));
    b.click(".side a", "Control plane");
    b.wait(&shows("Copy into Canada"));
    // Refused: a region not on the list, an account not signed in to.
    for (fields, status, said) in [(&[("region", "mars-north-1")][..], 400, "Choose a region from the list"), (&[("region", "ca-central-1"), ("account", "222222222222")], 403, "Sign in with AWS first")] {
        let (got, text) = post(&mut b, "/aws/image", fields);
        assert!(got == status && text.contains(said), "{fields:?}: {got} {text}");
    }
    // Copied in (as a real account answered): the section says it is on its way.
    served.aws.replaying(include_str!("../fixtures/aws/images/01-create.jsonl"), &[]);
    let (got, text) = post(&mut b, "/aws/image", &[("account", "111111111111"), ("region", "ca-central-1"), ("next", "?p=planes")]);
    assert!(got < 400, "{got} {text}");
    assert!(served.aws.asked.lock().unwrap().iter().any(|a| a["asked"].as_str().is_some_and(|s| s.contains("Action=CopyImage"))), "the copy asked for");
    // (AWS then lists it as being made.)
    served.aws.replaying(include_str!("../fixtures/aws/images/03-list-pending.jsonl"), &[]);
    b.goto(&format!("{link}&p=planes"));
    b.wait(&shows("It is being copied into Canada: about ten minutes"));
    // A newer build asked for while a copy is still on its way: nothing more is copied.
    served.aws.replaying(include_str!("../fixtures/aws/images/02-coming.jsonl"), &[]);
    let (got, text) = post(&mut b, "/aws/image", &[("region", "ca-central-1"), ("newer", "1")]);
    assert!(got < 400, "{got} {text}");
    let copies = served.aws.asked.lock().unwrap().iter().filter(|a| a["asked"].as_str().is_some_and(|s| s.contains("Action=CopyImage"))).count();
    assert_eq!(copies, 1, "one copy asked for");
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn what_an_aws_account_allows_is_said_in_its_section_as_real_accounts_answered() {
    let Some(mut b) = Browser::open(1300, 900) else { eprintln!("no Chrome here: skipped"); return };
    // A project of AWS's new sign-up (paid, and newly activated), and a classic account: as each answered.
    for (look, said) in [
        (include_str!("../fixtures/aws/project-paid/look.jsonl"), "AWS keeps it to Stockholm and allows it no spot machines"),
        (include_str!("../fixtures/aws/project-activated/look.jsonl"), "The other 11 are refused. By its AWS organization"),
        (include_str!("../fixtures/aws/classic/look.jsonl"), "All 12 SuperCI works in"),
    ] {
        // (Its control plane's own account runs jobs: AWS is a provider on Runners.)
        let served = serve(|c| {
            (c.aws_own, c.aws_own_creds) = (Some(("111111111111".into(), "us-east-1".into())), Some(superci_core::aws::Credentials { access_key_id: "ASIAX".into(), secret_access_key: "secret".into(), session_token: Some("tok".into()), expires_at_ms: u64::MAX }));
        });
        served.aws.replaying(look, &[]);
        let home = signed_in(&served);
        let (mut child, link) = dashboard(&home);
        b.goto(&format!("{link}&p=planes"));
        b.wait(&reads(said));
        // And where machines are added, and on Runners its provider's row.
        b.goto(&format!("{link}&p=add-runners"));
        b.wait(&page_shows("Add a provider"));
        b.goto(&format!("{link}&p=runners"));
        b.wait(&format!("{} && !document.querySelector('main').innerText.includes('Reading your control plane')", reads("Runner providers")));
        let _ = child.kill();
        let _ = std::fs::remove_dir_all(&home);
    }
}


#[test]
fn the_permissions_card_says_what_each_place_still_needs_and_with_which_sign_in() {
    let Some(mut b) = Browser::open(1300, 900) else { eprintln!("no Chrome here: skipped"); return };
    for (case, said) in [("older app", "1 permission to add in its App"), ("other account", "Signed in to account 222222222222; its role is in 111111111111"), ("key only", "Sign in to check its role")] {
        let served = serve(|_| {});
        served.connect_github();
        // (An App made before it asked for Actions; a sign-in to another account than its role's; no sign-in.)
        if case == "older app" { served.github.set_older_app(true) }
        let home = if case == "key only" { home(&served) } else { signed_in(&served) };
        if case == "other account" {
            let file = home.join("workspaces/default/sign-ins.json");
            let mut kept: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
            kept["aws"]["account_id"] = "222222222222".into();
            std::fs::write(&file, kept.to_string()).unwrap();
        }
        let (mut child, link) = dashboard(&home);
        b.goto(&format!("{link}&p=planes"));
        b.wait(&reads(said));
        if case == "older app" {
            // Its review: what to add, and why.
            b.eval("[...document.querySelectorAll('main button')].find(x => x.innerText === 'Review').click(); true");
            b.wait("document.querySelector('dialog[open]') && document.querySelector('dialog[open]').innerText.includes('Actions (write)')");
        }
        let _ = child.kill();
        let _ = std::fs::remove_dir_all(&home);
    }
}


#[test]
fn a_ci_whose_control_plane_is_in_a_cloud_not_signed_in_to_asks_for_that_sign_in_on_its_pages() {
    let Some(mut b) = Browser::open(1300, 900) else { eprintln!("no Chrome here: skipped"); return };
    // Its control plane in Cloudflare (seen from AWS, where its runners' role is, as a real account answered);
    // this workspace signed in to AWS only.
    let served = serve(|_| {});
    served.connect_github();
    let cf = superci_testkit::serve_as("cfplane00004", |_| {});
    served.aws.workers.lock().unwrap().insert("superci.acme.workers.dev".into(), cf.url.clone());
    served.aws.replaying(include_str!("../fixtures/runners/10-cf-remove-aws.jsonl"), &[]);
    let home = signed_in(&served);
    let file = home.join("workspaces/default/sign-ins.json");
    let mut kept: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    kept["plane"] = serde_json::json!({ "Seen": { "url": "https://superci.acme.workers.dev", "plane_id": "cfplane00004" } });
    kept["key"]["planes"] = serde_json::json!(["cfplane00004"]);
    std::fs::write(&file, kept.to_string()).unwrap();
    let (mut child, link) = dashboard(&home);
    // Its pages: dimmed behind that sign-in; Control plane lists it, in use, with the sign-in to see it.
    b.goto(&link);
    b.wait(&shows("Your control plane is in Cloudflare"));
    b.goto(&format!("{link}&p=runners"));
    b.wait(&shows("Sign in with Cloudflare to see it"));
    b.goto(&format!("{link}&p=planes"));
    b.wait(&format!("{} && {}", reads("superci.acme.workers.dev Sign in with Cloudflare In use"), shows("A Lambda function with a table")));
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn moves_deletions_and_labels_a_control_plane_cannot_take_are_refused_and_change_nothing() {
    let Some(mut b) = Browser::open(1300, 900) else { eprintln!("no Chrome here: skipped"); return };
    // The one in use (GitHub), and another in the account serving a code host of its own (GitLab).
    let served = serve(|_| {});
    served.connect_github();
    let aws = served.aws.clone();
    let other = superci_testkit::serve_as("testplane0002", move |c| { let s = aws.settings("testplane0002"); if !s.is_empty() { c.read_settings(&s) } });
    other.connect_gitlab();
    for (k, v) in other.aws.settings("testplane0002") { served.aws.put(&format!("/superci/testplane0002/{k}"), &v) }
    let home = signed_in(&served);
    served.aws.replaying(include_str!("../fixtures/aws/planes/delete.jsonl"), &[("http://new-plane.test", &other.url)]);
    let (mut child, link) = dashboard(&home);
    b.goto(&format!("{link}&c={PLANE}&p=planes"));
    b.wait(&page_shows("Control plane"));
    // (What these change: its labels and its routing. This session's key is handed to it meanwhile.)
    let settings = || served.aws.settings(PLANE).into_iter().filter(|(k, _)| k == "LABELS" || k == "ROUTING").collect::<Vec<_>>();
    let before = settings();
    for (path, fields, status, said) in [
        ("/plane/delete", &[("plane", PLANE)][..], 409, "This control plane is in use"),
        ("/labels", &[("action", "remove"), ("label", "nosuch")], 400, "It does not answer to that label"),
        ("/labels", &[("action", "remove"), ("label", "superci")], 409, "This is its only label"),
        ("/labels", &[("label", "Not A Label!")], 400, "That cannot be a label"),
    ] {
        let (got, text) = post(&mut b, &format!("{path}?c={PLANE}"), fields);
        assert!(got == status && text.contains(said), "{path} {fields:?}: {got} {text}");
    }
    // Moving onto itself: nothing to do.
    let (got, text) = post(&mut b, &format!("/plane/move?c={PLANE}"), &[("plane", PLANE)]);
    assert!(got < 400, "{got} {text}");
    assert_eq!(settings(), before, "nothing changed");
    let _ = child.kill();
    // A control plane too old for more labels: refused, saying to update it.
    *served.aws.old_version.lock().unwrap() = Some("0.11.0".into());
    let (mut child, link) = dashboard(&home);
    b.goto(&format!("{link}&c={PLANE}&p=settings"));
    b.wait(&page_shows("Settings"));
    let (got, text) = post(&mut b, &format!("/labels?c={PLANE}"), &[("label", "acmeci")]);
    assert!(got == 409 && text.contains("Labels can be added from control plane 0.12.0 on"), "{got} {text}");
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_first_control_plane_is_set_up_in_aws_from_the_browser_started_over_once_and_made_as_aws_answered() {
    let Some(mut b) = Browser::open(1300, 900) else { eprintln!("no Chrome here: skipped"); return };
    // A workspace signed in to an AWS account with no control plane yet.
    let account = superci_testkit::Aws::serve();
    let new = superci_testkit::serve_as("deployplane1", |_| {});
    let home = common::temp("first-home");
    let ws = home.join("workspaces/default");
    std::fs::create_dir_all(&ws).unwrap();
    std::fs::write(ws.join("sign-ins.json"), serde_json::json!({ "aws": { "region": "us-east-1", "account_id": "111111111111", "who": "arn:aws:sts::111111111111:assumed-role/Admin/someone",
        "creds": { "access_key_id": "ASIAEXAMPLE", "secret_access_key": "example", "session_token": "example", "expires_at_ms": 4102444800000u64 }, "refresh_token": "", "dpop": { "private_b64": "", "kid": "" } } }).to_string()).unwrap();
    std::fs::write(home.join("aws-at"), &account.url).unwrap();
    let (mut child, link) = common::dashboard_with(&home, &[("SUPERCI_NEW_PLANE_ID", "deployplane1")]);
    b.goto(&link);
    b.wait(&page_shows("Overview"));
    b.goto(&format!("{link}&p=plane"));
    b.wait(&shows("Set up"));
    // Refused: a region not listed, an account not signed in to.
    for (fields, status, said) in [(&[("region", "mars-north-1")][..], 400, "Choose a region from the list"), (&[("region", "us-east-1"), ("account", "222222222222")], 403, "Sign in with AWS first")] {
        let (got, text) = post(&mut b, "/plane/aws", fields);
        assert!(got == status && text.contains(said), "{fields:?}: {got} {text}");
    }
    // With nothing answered by AWS it stops, and says so; started over, what it made is removed.
    let (got, text) = post(&mut b, "/plane/aws", &[("region", "us-east-1"), ("account", "111111111111")]);
    assert!(got < 400, "{got} {text}");
    let end = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while !common::words(&common::part(&link, "/?p=plane&fragment=1").1).contains("Try again") { assert!(std::time::Instant::now() < end, "{}", common::words(&common::part(&link, "/?p=plane&fragment=1").1)); std::thread::sleep(std::time::Duration::from_millis(300)) }
    let (got, text) = post(&mut b, "/plane/start-over", &[]);
    assert!(got < 400, "{got} {text}");
    // As AWS answered a real one: made, and the overview leads on to connecting GitHub or GitLab.
    account.replaying(include_str!("../fixtures/aws/planes/deploy.jsonl"), &[("testplane0002", "deployplane1"), ("http://new-plane.test", &new.url)]);
    let (got, text) = post(&mut b, "/plane/aws", &[("region", "us-east-1"), ("account", "111111111111")]);
    assert!(got < 400, "{got} {text}");
    let end = std::time::Instant::now() + std::time::Duration::from_secs(120);
    loop {
        let page = common::words(&common::part(&link, "/?p=overview&fragment=1").1);
        if page.contains("Connect GitHub or GitLab") && page.contains("Running") { break }
        assert!(std::time::Instant::now() < end, "{page}");
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn with_several_workspaces_each_page_and_change_says_which_and_one_is_renamed_and_forgotten() {
    let Some(mut b) = Browser::open(1300, 900) else { eprintln!("no Chrome here: skipped"); return };
    let served = serve(|_| {});
    let home = signed_in(&served);
    let (mut child, link) = dashboard(&home);
    b.goto(&link);
    b.wait(&shows("Usage"));
    let get = |b: &mut Browser, path: &str| -> (u16, String) {
        let got = b.eval(&format!("fetch({path:?}, {{credentials: 'same-origin', redirect: 'manual'}}).then(async r => [r.status, r.type === 'opaqueredirect' ? 'redirected' : (await r.text()).replace(/<(style|script)[^>]*>[\\s\\S]*?<\\/\\1>/g, ' ').replace(/<[^>]*>/g, ' ').replace(/\\s+/g, ' ')])"));
        (got[0].as_u64().unwrap_or(0) as u16, got[1].as_str().unwrap_or_default().to_string())
    };
    // A second one, made from the page.
    let (got, _) = post(&mut b, "/workspaces", &[("name", "Globex")]);
    assert!(got < 400);
    assert!(home.join("workspaces/globex").exists());
    // A page that names no workspace goes to one; one naming a workspace not kept here says so; the list of them.
    assert_eq!(get(&mut b, "/?p=overview").1, "redirected");
    let (status, text) = get(&mut b, "/?w=nowhere&p=overview&fragment=1");
    assert!(status == 200 && text.contains("That workspace is not kept on this computer"), "{text}");
    assert_eq!(get(&mut b, "/?w=nowhere&p=overview").1, "redirected");
    let (status, text) = get(&mut b, "/workspaces");
    assert!(status == 200 && text.contains("Globex"), "{text}");
    // A change that names none: refused, nothing changed.
    let (got, text) = post(&mut b, "/labels", &[("label", "acmeci")]);
    assert!(got == 409 && text.contains("Which workspace?"), "{got} {text}");
    let (got, text) = post(&mut b, "/workspaces/remove", &[]);
    assert!(got == 409 && text.contains("Which workspace?"), "{got} {text}");
    // Renamed: to its own id (nothing to do), to one another has (refused), to a new one; then forgotten.
    let (got, _) = post(&mut b, "/workspaces/rename", &[("w", "globex"), ("id", "globex")]);
    assert!(got < 400);
    let (got, text) = post(&mut b, "/workspaces/rename", &[("w", "globex"), ("id", "default")]);
    assert!(got == 400 && text.contains("There is a workspace with the id default already"), "{got} {text}");
    let (got, _) = post(&mut b, "/workspaces/rename", &[("w", "globex"), ("id", "initech")]);
    assert!(got < 400 && home.join("workspaces/initech").exists() && !home.join("workspaces/globex").exists());
    let (got, _) = post(&mut b, "/workspaces/remove", &[("w", "initech")]);
    assert!(got < 400 && !home.join("workspaces/initech").exists() && home.join("workspaces/default").exists());
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn sign_ins_that_ended_before_are_said_when_the_dashboard_opens_and_the_ci_is_still_read_with_its_key() {
    let Some(mut b) = Browser::open(1300, 900) else { eprintln!("no Chrome here: skipped"); return };
    // Its last run noted each cloud's sign-in as ended; this computer's key still reaches the control plane.
    for (flag, said) in [("aws_ended", "AWS sign-in has ended"), ("cloudflare_ended", "Cloudflare sign-in has expired"), ("modal_ended", "Modal no longer accepts")] {
        let served = serve(|_| {});
        let home = home(&served);
        let file = home.join("workspaces/default/sign-ins.json");
        let mut kept: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        kept[flag] = true.into();
        std::fs::write(&file, kept.to_string()).unwrap();
        let (mut child, link) = dashboard(&home);
        b.goto(&link);
        b.wait(&format!("{} && {}", page_shows(said), shows("Usage")));
        let _ = child.kill();
        let _ = std::fs::remove_dir_all(&home);
    }
}


#[test]
fn a_cloudflare_token_given_and_refused_is_said_as_that_token_not_as_a_sign_in_that_ended() {
    let Some(mut b) = Browser::open(1300, 900) else { eprintln!("no Chrome here: skipped"); return };
    let served = serve(|_| {});
    let home = signed_in(&served);
    // (Cloudflare's answer to a token it does not take.)
    served.aws.replaying(include_str!("../fixtures/cloudflare/sign-in/ended.jsonl"), &[]);
    let (mut child, link) = common::dashboard_with(&home, &[("SUPERCI_CLOUDFLARE_TOKEN", "a-token-cloudflare-refuses")]);
    b.goto(&link);
    b.wait(&shows("Cloudflare refuses the token in SUPERCI_CLOUDFLARE_TOKEN"));
    assert!(!b.eval("document.querySelector('main').innerText").as_str().unwrap().contains("sign-in has ended"));
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}


#[test]
fn a_control_plane_that_does_not_answer_is_said_on_its_pages_instead_of_a_guess_at_what_it_has() {
    let Some(mut b) = Browser::open(1300, 900) else { eprintln!("no Chrome here: skipped"); return };
    // Its address answering nothing (down, or its cloud's network).
    let served = serve(|_| {});
    let home = signed_in(&served);
    let file = home.join("workspaces/default/sign-ins.json");
    let mut kept: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    kept["plane"]["Aws"]["url"] = "http://127.0.0.1:9".into();
    std::fs::write(&file, kept.to_string()).unwrap();
    let (mut child, link) = common::dashboard_with(&home, &[("SUPERCI_KEY_WAIT_S", "1")]);
    b.goto(&link);
    b.wait(&shows("Your control plane is not answering"));
    // Not "no runners yet", "connect GitHub" or "sign in": that it does not answer.
    for p in ["runners", "repos", "workflows", "runs", "jobs"] {
        b.goto(&format!("{link}&p={p}"));
        b.wait(&shows("Your control plane is not answering"));
        assert!(!b.eval("document.querySelector('main').innerText").as_str().unwrap().contains("No runners yet"), "{p}");
    }
    b.goto(&format!("{link}&p=planes"));
    b.wait(&reads("http://127.0.0.1:9 Not answering In use"));
    // Settings: the workspace's own, which need no control plane.
    b.goto(&format!("{link}&p=settings"));
    b.wait(&shows("Workspace"));
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn gitlabs_are_connected_one_and_another_and_again_from_the_browser_and_what_is_not_one_is_refused() {
    let Some(mut b) = Browser::open(1300, 900) else { eprintln!("no Chrome here: skipped"); return };
    let served = serve(|_| {});
    let home = signed_in(&served);
    let token = superci_testkit::gitlab::GitLab::connection()["token"].as_str().unwrap().to_string();
    let (mut child, link) = dashboard(&home);
    b.goto(&format!("{link}&p=repos"));
    b.wait(&shows("Where jobs come from"));
    let gitlabs = || served.aws.settings(PLANE).into_iter().filter(|(k, _)| k.starts_with("GITLAB")).map(|(k, _)| k).collect::<Vec<_>>();
    // Refused, nothing kept: an address that is not one, a token GitLab does not take, a GitLab not connected here.
    for (fields, said) in [
        (vec![("url", "gitlab.com"), ("token", token.as_str()), ("which", "new")], "GitLab's address starts with https://"),
        (vec![("url", "https://gitlab.com"), ("token", "glpat-not-a-token"), ("which", "new")], "GitLab did not accept the token"),
        (vec![("url", "https://gitlab.com"), ("token", token.as_str()), ("which", "gnothere")], "That GitLab is not connected here"),
    ] {
        let (got, text) = post(&mut b, "/gitlab/connect", &fields);
        assert!(got == 400 && text.contains(said), "{fields:?}: {got} {text}");
        assert!(gitlabs().is_empty());
    }
    // A GitHub App to start for a name that is none, at an address that is none, for a name GitHub does not know.
    for (fields, said) in [(&[("login", "not a name!")][..], "That is not a GitHub name"), (&[("login", "acme"), ("host", "not an address!")], "Check the address of your GitHub"), (&[("login", "nobody-here-at-all")], "Check the name")] {
        let (got, text) = post(&mut b, "/github/start", fields);
        assert!(got == 400 && text.contains(said), "{fields:?}: {got} {text}");
    }
    // The first, another beside it, and the first again (its webhooks given the new secret).
    for which in ["new", "new", ""] {
        let (got, text) = post(&mut b, "/gitlab/connect", &[("url", "https://gitlab.com"), ("token", &token), ("which", which)]);
        assert!(got < 400, "{which}: {got} {text}");
    }
    assert_eq!(gitlabs().len(), 2, "{:?}", gitlabs());
    // Repositories lists them both.
    b.goto(&format!("{link}&p=repos"));
    b.wait(&reads("GitLab"));
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn every_provider_s_runners_on_an_aws_control_plane_and_then_a_move_to_cloudflare_are_shown_on_each_page() {
    let Some(mut b) = Browser::open(1300, 900) else { eprintln!("no Chrome here: skipped"); return };
    // The workspace's control plane in AWS, serving GitHub (the one in use); another in the same account, made here.
    let served = serve(|_| {});
    served.connect_github();
    let aws = served.aws.clone();
    let creds = superci_core::aws::Credentials { access_key_id: "ASIAX".into(), secret_access_key: "secret".into(), session_token: Some("tok".into()), expires_at_ms: u64::MAX };
    let own = creds.clone();
    let second = superci_testkit::serve_as("awsplane0001", move |c| {
        c.read_settings(&aws.settings("awsplane0001"));
        (c.aws_own, c.aws_own_creds) = (Some(("111111111111".into(), "us-east-1".into())), Some(own.clone()));
        c.aws_runners_off = aws.settings("awsplane0001").iter().any(|(k, v)| k == "AWS_RUNNERS" && v == "off");
    });
    second.aws.params.lock().unwrap().remove("/superci/awsplane0001/CONTAINERS");
    let aws = served.aws.clone();
    let cf = superci_testkit::serve_as("cfplane00004", move |c| {
        let settings = aws.worker_settings("superci");
        c.read_settings(&settings);
        c.containers = settings.iter().any(|(k, v)| k == "CONTAINERS" && v == "on");
        // (As the Worker reads its AWS connection in progress: the region and the one-time token.)
        if let Some(v) = settings.iter().find(|(k, _)| k == "AWS_CONNECT").and_then(|(_, v)| serde_json::from_str::<serde_json::Value>(v).ok()) {
            c.aws_region = v["region"].as_str().map(str::to_string);
            c.aws_connect_token = v["token"].as_str().map(str::to_string);
        }
    });
    cf.aws.params.lock().unwrap().remove("/superci/cfplane00004/CONTAINERS");
    {
        let mut w = served.aws.workers.lock().unwrap();
        w.insert("superci.acme.workers.dev".into(), cf.url.clone());
        // (The launchers' health, which the dashboard waits for once each is deployed: a server here answers.)
        for agent in ["acmeorg--superci-runners-awsplane0001.modal.run", "acmeorg--superci-runners-cfplane00004.modal.run", "superci-runners-awsplane0001.acme.workers.dev"] { w.insert(agent.into(), cf.url.clone()); }
    }
    let home = common::signed_in(&served);
    let file = home.join("workspaces/default/sign-ins.json");
    let mut kept: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    // (The workspace looks after the one the recording has in the account: the one served here, as it is called there.)
    kept["plane"]["Aws"]["plane_id"] = "otherplane01".into();
    kept["key"]["planes"] = serde_json::json!(["otherplane01"]);
    kept["cloudflare"] = serde_json::json!({ "access": "cloudflare-access-for-tests", "expires_at_ms": 4102444800000u64, "refresh": "cloudflare-refresh-for-tests" });
    kept["modal"] = serde_json::json!({ "token_id": "ak-modal-for-tests", "token_secret": "as-modal-for-tests", "workspace": "acmeorg" });
    std::fs::write(&file, kept.to_string()).unwrap();
    let step = |name: &str| served.aws.replaying(&std::fs::read_to_string(format!("{}/fixtures/runners/{name}.jsonl", env!("CARGO_MANIFEST_DIR"))).unwrap(), &[("http://new-plane.test", &second.url)]);
    let run = |id: &str, args: &[&str]| {
        let o = common::command(&home).args(args).env("SUPERCI_NEW_PLANE_ID", id).output().unwrap();
        (o.status.success(), String::from_utf8_lossy(&o.stdout).to_string() + &String::from_utf8_lossy(&o.stderr))
    };
    step("01-deploy-aws");
    let (ok, said) = run("awsplane0001", &["planes", "create", "aws", "--region=us-east-1"]);
    assert!(ok && said.contains("The control plane is running"), "{said}");
    step("02-aws-runners");
    let (ok, said) = run("", &["runners", "create", "aws", "--plane=awsplane0001"]);
    assert!(ok && said.contains("aws runs jobs now."), "{said}");
    step("03-cf-agent");
    let (ok, said) = run("", &["runners", "create", "cloudflare", "--plane=awsplane0001"]);
    assert!(ok && said.contains("cloudflare runs jobs now."), "{said}");
    step("04-modal-agent");
    let (ok, said) = run("", &["runners", "create", "modal", "--plane=awsplane0001"]);
    assert!(ok && said.contains("modal runs jobs now."), "{said}");
    let agents = served.aws.settings("awsplane0001").into_iter().find(|(k, _)| k == "LAUNCHERS").map(|(_, v)| v).unwrap_or_default();
    assert!(agents.contains("\"cloud\":\"cloudflare\"") && agents.contains("\"cloud\":\"modal\""), "{agents}");
    let (ok, said) = run("", &["runners", "list", "--plane=awsplane0001"]);
    assert!(ok && said.contains("aws ") && said.contains("cloudflare") && said.contains("modal"), "{said}");
    step("06-dashboard");
    // Each page with three providers, two through launchers.
    let (mut child, link) = common::dashboard(&home);
    let walk = |b: &mut Browser, plane: &str, wanted: &[(&str, &str)]| {
        for (p, said) in wanted {
            b.goto(&format!("{link}&c={plane}&p={p}"));
            b.wait(&format!("{} && !document.querySelector('main').innerText.includes('Reading your control plane')", reads(said)));
        }
    };
    walk(&mut b, "awsplane0001", &[("runners", "Modal"), ("add-runners", "Add a provider"), ("planes", "Control plane"), ("overview", "Usage"), ("settings", "Limits")]);
    assert!(b.eval("document.querySelector('main').innerText").as_str().is_some());
    let _ = child.kill();
    // A Cloudflare control plane, then everything moved there: AWS's machines through a role it is given, Cloudflare's
    // its own containers, Modal's through an agent deployed for it.
    step("07-deploy-cf");
    let (ok, said) = run("cfplane00004", &["planes", "create", "cloudflare"]);
    assert!(ok && said.contains("The control plane is running: https://superci.acme.workers.dev"), "{said}");
    step("08-move");
    let (ok, said) = run("", &["planes", "move", "cfplane00004", "--plane=awsplane0001"]);
    assert!(ok && said.contains("Moved."), "{said}");
    // The Cloudflare one in use, AWS's machines through its role: its pages.
    let (mut child, link) = common::dashboard(&home);
    for p in ["runners", "planes", "overview"] {
        b.goto(&format!("{link}&c=cfplane00004&p={p}"));
        b.wait(&page_shows(match p { "runners" => "Runner providers", "planes" => "Control plane", _ => "Usage" }));
    }
    let _ = child.kill();
    // AWS's machines taken out of the Cloudflare one (its role and their network deleted), then both deleted.
    step("10-cf-remove-aws");
    let (ok, said) = run("", &["runners", "delete", "aws", "--plane=cfplane00004", "--confirm"]);
    assert!(ok && said.contains("Removed AWS."), "{said}");
    step("11-delete-cf");
    let (ok, said) = run("", &["planes", "delete", "cfplane00004", "--confirm"]);
    assert!(ok && said.contains("Deleted Cloudflare · Acme."), "{said}");
    step("12-delete-aws");
    let (ok, said) = run("", &["planes", "delete", "awsplane0001", "--confirm"]);
    assert!(ok && said.contains("Deleted AWS"), "{said}");
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_job_s_page_says_how_its_aws_machine_was_had_spot_after_a_full_pool_or_on_demand_as_aws_answered() {
    let Some(mut b) = Browser::open(1300, 900) else { eprintln!("no Chrome here: skipped"); return };
    for (recording, label, said) in [(include_str!("../fixtures/aws/launch/spot.jsonl"), "superci/cpu=2", "AWS's choice of 4 pools"), (include_str!("../fixtures/aws/launch/on-demand.jsonl"), "superci/cpu=2/spot=false", "m7a.large")] {
        let served = serve(|c| {
            (c.aws_own, c.aws_own_creds) = (Some(("111111111111".into(), "us-east-1".into())), Some(superci_core::aws::Credentials { access_key_id: "ASIAX".into(), secret_access_key: "secret".into(), session_token: Some("tok".into()), expires_at_ms: u64::MAX }));
        });
        served.aws.replaying(recording, &[]);
        let home = home(&served);
        let dir = home.join("checkout");
        std::fs::create_dir_all(dir.join(".github/workflows")).unwrap();
        std::fs::write(dir.join(".github/workflows/ci.yml"), format!("jobs:\n  tests:\n    runs-on: {label}\n    steps:\n      - run: make test\n")).unwrap();
        for args in [&["init", "-q"][..], &["add", "-A"], &["-c", "user.name=t", "-c", "user.email=t@example.com", "commit", "-qm", "t"]] { assert!(std::process::Command::new("git").current_dir(&dir).args(args).status().unwrap().success()) }
        let o = std::process::Command::new(env!("CARGO_BIN_EXE_superci")).current_dir(&dir).args(["runs", "create", "--workflow=.github/workflows/ci.yml", "--detach"])
            .env("SUPERCI_HOME", dir.join(".cli")).env("SUPERCI_PLANE", &served.url).env("SUPERCI_KEY", &served.run_key).env("SUPERCI_STAND_IN", &served.aws.url).output().unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        let job = loop { if let Some((_, j)) = served.store.under("job:").into_iter().next().filter(|(_, j)| j["machine_id"].is_string()) { break j } std::thread::sleep(std::time::Duration::from_millis(50)) };
        let id = job["job_id"].to_string();
        // While it starts: its page says the machine and how it was had.
        let (mut child, link) = dashboard(&home);
        b.goto(&format!("{link}&p=job&id={id}"));
        b.wait(&format!("{} && {}", shows("Its start"), shows(said)));
        // Taken by its runner and ended: the same, with its steps.
        let run = served.store.under("run:").into_iter().next().unwrap().0.trim_start_matches("run:").to_string();
        let runner = loop { if let Some(r) = superci_testkit::Runner::of_run(&served, &run) { break r } std::thread::sleep(std::time::Duration::from_millis(50)) };
        runner.take();
        runner.steps(&[("eeeeeeee-0000-0000-0000-000000000001", "Run make test", 1, "completed", Some("succeeded"), Some(1))]);
        runner.log(1, "2026-10-15T10:00:01.0000000Z ok");
        runner.finish("succeeded");
        b.goto(&format!("{link}&p=job&id={id}"));
        b.wait(&format!("{} && {}", shows("Passed"), shows("Its start")));
        b.goto(&format!("{link}&p=runs"));
        b.wait(&shows("Passed"));
        let _ = child.kill();
        let _ = std::fs::remove_dir_all(&home);
    }
}

#[test]
fn cloudflare_jobs_estimated_costs_are_replaced_by_what_cloudflare_measured_once_it_has() {
    let Some(mut b) = Browser::open(1300, 900) else { eprintln!("no Chrome here: skipped"); return };
    // Three jobs on Cloudflare's containers, ended ten minutes ago; Cloudflare's usage for their containers as it answered.
    let served = serve(|_| {});
    served.connect_github();
    for n in 1..=3 { let (id, q) = served.github.queue(&format!("job{n}"), "superci", served.now_ms()); served.webhook(q); served.webhook(served.github.start(id)); served.later(60_000); served.webhook(served.github.finish(id, "success", "ok", served.now_ms())); }
    // Each one's container as Cloudflare named it, ended ten minutes ago.
    {
        let mut store = served.store.0.lock().unwrap();
        let keys: Vec<String> = store.keys().filter(|k| k.starts_with("job:")).cloned().collect();
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
        for (i, k) in keys.iter().enumerate() {
            let mut j: serde_json::Value = serde_json::from_str(&store[k]).unwrap();
            j["machine_id"] = format!("{:064x}", i + 1).into();
            j["machine_type"] = "4cpu-12gb".into();
            j["started_ms"] = (now - 603_000).into(); j["ended_ms"] = (now - 600_000).into(); j["launched_ms"] = (now - 670_000).into();
            store.insert(k.clone(), j.to_string());
        }
    }
    let home = signed_in(&served);
    let file = home.join("workspaces/default/sign-ins.json");
    let mut kept: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    kept["cloudflare"] = serde_json::json!({ "access": "cloudflare-access-for-tests", "expires_at_ms": 4102444800000u64, "refresh": "cloudflare-refresh-for-tests" });
    std::fs::write(&file, kept.to_string()).unwrap();
    served.aws.replaying(&[include_str!("../fixtures/cloudflare/01-look.jsonl"), include_str!("../fixtures/cloudflare/sign-in/usage.jsonl")].concat(), &[]);
    let (mut child, link) = dashboard(&home);
    b.goto(&format!("{link}&p=jobs"));
    b.wait(&reads("3s $0.0006 Passed"));
    assert!(!b.eval("document.querySelector('main').innerText").as_str().unwrap().contains("≈"), "measured, not estimated");
    until("kept as measured", || served.store.under("job:").iter().all(|(_, j)| j["cost_from"] == "measured"));
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_ci_with_nothing_yet_moves_to_another_control_plane_from_the_browser_and_the_old_one_says_where() {
    let Some(mut b) = Browser::open(1300, 900) else { eprintln!("no Chrome here: skipped"); return };
    // No code host, its name and labels as they come; another in the same account, reading its settings there.
    let served = serve(|_| {});
    served.aws.put(&format!("/superci/{PLANE}/CONTAINERS"), "off");
    let aws = served.aws.clone();
    let new = superci_testkit::serve_as("testplane0002", move |c| c.read_settings(&aws.settings("testplane0002")));
    for (k, v) in new.aws.settings("testplane0002") { served.aws.put(&format!("/superci/testplane0002/{k}"), &v) }
    let home = signed_in(&served);
    served.aws.replaying(include_str!("../fixtures/aws/planes/delete.jsonl"), &[("http://new-plane.test", &new.url)]);
    let (mut child, link) = dashboard(&home);
    b.goto(&format!("{link}&p=planes"));
    b.wait(&shows(&new.url));
    b.click("main button", "Move here");
    b.wait("document.querySelector('dialog[open]') !== null");
    b.click("dialog[open] button.primary", "");
    until("moved", || !served.store.under("moved_to").is_empty());
    let health: serde_json::Value = ureq::get(&format!("{}/health", served.url)).call().unwrap().body_mut().read_json().unwrap();
    assert_eq!(health["moved_to"].as_str(), Some(new.url.as_str()), "{health}");
    assert!(!served.aws.settings("testplane0002").iter().any(|(k, _)| k == "GITHUB_APP" || k == "NAME"), "nothing it did not have");
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn sign_ins_made_elsewhere_while_the_dashboard_runs_are_taken_in_on_the_next_page() {
    let Some(mut b) = Browser::open(1300, 900) else { eprintln!("no Chrome here: skipped"); return };
    let served = serve(|_| {});
    let home = signed_in(&served);
    served.aws.replaying(&[include_str!("../fixtures/cloudflare/01-look.jsonl"), include_str!("../fixtures/cloudflare/sign-in/usage.jsonl"), include_str!("../fixtures/modal/05-dashboard.jsonl")].concat(), &[]);
    let (mut child, link) = dashboard(&home);
    b.goto(&format!("{link}&p=planes"));
    b.wait(&reads("Cloudflare A Worker with its own storage Connect"));
    // Signed in to Cloudflare and Modal elsewhere (another terminal's `superci login`): the next page has them.
    let file = home.join("workspaces/default/sign-ins.json");
    let mut kept: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    kept["cloudflare"] = serde_json::json!({ "access": "cloudflare-access-for-tests", "expires_at_ms": 4102444800000u64, "refresh": "cloudflare-refresh-for-tests" });
    kept["modal"] = serde_json::json!({ "token_id": "ak-modal-for-tests", "token_secret": "as-modal-for-tests", "workspace": "acmeorg" });
    std::fs::write(&file, kept.to_string()).unwrap();
    b.goto(&format!("{link}&p=planes"));
    b.wait(&format!("{} && {}", reads("A Worker with its own storage · "), reads("workspace acmeorg Set up")));
    // AWS signed out elsewhere: the next page no longer has it.
    let mut kept: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    kept.as_object_mut().unwrap().remove("aws");
    std::fs::write(&file, kept.to_string()).unwrap();
    b.goto(&format!("{link}&p=planes"));
    b.wait(&reads("Sign in"));
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn signing_out_from_settings_ends_each_sign_in_and_hands_back_this_computers_key() {
    let Some(mut b) = Browser::open(1300, 900) else { eprintln!("no Chrome here: skipped"); return };
    let served = serve(|_| {});
    let home = signed_in(&served);
    let file = home.join("workspaces/default/sign-ins.json");
    let mut kept: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    kept["cloudflare"] = serde_json::json!({ "access": "example-access_token-1", "expires_at_ms": 4102444800000u64, "refresh": "example-refresh_token-2" });
    kept["modal"] = serde_json::json!({ "token_id": "ak-modal-for-tests", "token_secret": "as-modal-for-tests", "workspace": "acmeorg" });
    std::fs::write(&file, kept.to_string()).unwrap();
    // (Cloudflare asked to end it, as it answered; its usage and Modal's billing read meanwhile.)
    served.aws.replaying(&[include_str!("../fixtures/cloudflare/01-look.jsonl"), include_str!("../fixtures/cloudflare/sign-in/usage.jsonl"), include_str!("../fixtures/modal/05-dashboard.jsonl"), include_str!("../fixtures/cloudflare/sign-in/sign-out.jsonl")].concat(), &[]);
    let keys = || served.aws.settings(PLANE).into_iter().filter(|(k, _)| k.starts_with("DEVICE_KEY_")).count();
    let (mut child, link) = dashboard(&home);
    b.goto(&format!("{link}&p=settings"));
    b.wait(&shows("This device"));
    let before = keys();
    let (got, text) = post(&mut b, "/signout", &[]);
    assert!(got < 400, "{got} {text}");
    // Nothing kept on this computer, and the control plane no longer takes its key.
    until("signed out", || !file.exists());
    assert!(keys() < before, "its key handed back");
    assert!(served.aws.asked.lock().unwrap().iter().any(|a| a["url"].as_str().is_some_and(|u| u.contains("dash.cloudflare.com/oauth2/revoke"))), "Cloudflare asked to end it");
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn removing_aws_when_aws_answers_nothing_forgets_it_and_says_what_is_left_there() {
    let Some(mut b) = Browser::open(1300, 900) else { eprintln!("no Chrome here: skipped"); return };
    // A control plane whose own account runs jobs, with a budget in AWS for its limit; AWS answering nothing more.
    let served = serve(|c| {
        (c.aws_own, c.aws_own_creds) = (Some(("111111111111".into(), "us-east-1".into())), Some(superci_core::aws::Credentials { access_key_id: "ASIAX".into(), secret_access_key: "secret".into(), session_token: Some("tok".into()), expires_at_ms: u64::MAX }));
    });
    served.aws.put(&format!("/superci/{PLANE}/ROUTING"), r#"{"account_usd":{"aws":100.0},"aws_budget":{"stop":false},"default":"aws","rules":[{"repo":"acme/*","cloud":"aws"}],"order":[{"cloud":"aws"},{"cloud":"aws-on-demand"},{"cloud":"cloudflare"}]}"#);
    let home = signed_in(&served);
    let (mut child, link) = dashboard(&home);
    b.goto(&format!("{link}&p=runners"));
    b.wait(&shows("Runner providers"));
    let (got, text) = post(&mut b, "/runners/remove", &[("cloud", "aws")]);
    assert!(got < 400, "{got} {text}");
    // Forgotten: out of the order (on-demand with it), its rule and default gone, its budget too.
    until("forgotten", || served.aws.settings(PLANE).iter().any(|(k, v)| k == "AWS_RUNNERS" && v == "off"));
    let routing: serde_json::Value = serde_json::from_str(&served.aws.settings(PLANE).into_iter().find(|(k, _)| k == "ROUTING").unwrap().1).unwrap();
    assert!(routing["order"].as_array().unwrap().iter().all(|p| p["cloud"] != "aws" && p["cloud"] != "aws-on-demand") && routing["rules"].as_array().unwrap().is_empty() && routing["aws_budget"].is_null(), "{routing}");
    // Said on the page, with what could not be deleted there (AWS answering nothing): its role, its budget.
    b.wait(&format!("{} && {}", page_shows("Removed AWS. Left:"), page_shows("its budget in AWS")));
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn the_default_machine_and_keys_forms_take_only_what_they_may_and_the_machine_is_shown_as_set() {
    let Some(mut b) = Browser::open(1300, 900) else { eprintln!("no Chrome here: skipped"); return };
    let served = serve(|_| {});
    let home = signed_in(&served);
    let (mut child, link) = dashboard(&home);
    b.goto(&format!("{link}&p=workflows"));
    b.wait(&shows("Default machine"));
    let machine = || served.aws.settings(PLANE).into_iter().find(|(k, _)| k == "MACHINE").map(|(_, v)| v);
    // Refused: a value that is no value, parts that do not go together.
    for (fields, said) in [(&[("cpu", "lots")][..], "That machine is not understood"), (&[("os", "windows"), ("arch", "arm64")], "Windows machines are x64"), (&[("family", "m8g+m8a")], "That machine does not go together")] {
        let (got, text) = post(&mut b, "/machine", fields);
        assert!(got >= 400 && text.contains(said), "{fields:?}: {got} {text}");
    }
    // What is the standard anyway (x64, spot, left to the provider) is not kept; what is set is shown.
    post(&mut b, "/machine", &[("cpu", "default"), ("arch", "x64"), ("spot", "true"), ("os", "linux")]);
    until("nothing kept", || machine().is_none_or(|m| m == "{}"));
    let (got, text) = post(&mut b, "/machine", &[("cpu", "8"), ("ram", "32"), ("scratch", "ramdisk"), ("family", "m8a"), ("provider", "aws"), ("disk-speed", "500")]);
    assert!(got < 400, "{got} {text}");
    b.goto(&format!("{link}&p=workflows"));
    b.wait(&format!("{} && {} && {}", reads("8 CPU"), reads("RAM disk"), reads("m8a")));
    // A key to take out that is no device key's name: refused, nothing changed.
    let (got, text) = post(&mut b, "/keys/remove", &[("name", "DEVICE_KEY_not-one")]);
    assert!(got == 409 && text.contains("That is not a device key"), "{got} {text}");
    let (got, text) = post(&mut b, "/keys/remove", &[("name", "READER")]);
    assert!(got == 409 && text.contains("That is not a device key"), "{got} {text}");
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn overview_sums_up_a_single_gitlab_job_and_a_day_of_long_ones() {
    let Some(mut b) = Browser::open(1300, 900) else { eprintln!("no Chrome here: skipped"); return };
    // One GitLab job alone (one run, it ran); and from GitHub three jobs of 40 hours each (over 6,000 minutes).
    for case in ["one gitlab job", "long jobs"] {
        let served = serve(|_| {});
        served.connect_github();
        served.connect_gitlab();
        if case == "one gitlab job" {
            let (gid, p) = served.gitlab.pending("gl-test", &["superci"], served.now_ms());
            served.gitlab_webhook(p);
            served.gitlab_webhook(served.gitlab.running(gid, served.now_ms()));
            served.later(30_000);
            served.gitlab_webhook(served.gitlab.finish(gid, "success", "ok", served.now_ms()));
        } else {
            for n in 0..3 {
                let (id, q) = served.github.queue(&format!("soak{n}"), "superci", served.now_ms());
                served.webhook(q);
                served.webhook(served.github.start(id));
                served.later(40 * 3_600_000);
                served.webhook(served.github.finish(id, "success", "ok", served.now_ms()));
            }
        }
        let home = home(&served);
        let (mut child, link) = dashboard(&home);
        b.goto(&link);
        if case == "one gitlab job" {
            b.wait(&format!("{} && {}", reads("1 job"), reads("as GitLab bills them")));
            b.click("main a, main button", "7 days");
            b.wait(&reads("1 job"));
        } else {
            b.click("main a, main button", "7 days");
            b.wait(&format!("{} && {}", reads("3 jobs"), reads("machine hours")));
        }
        let _ = child.kill();
        let _ = std::fs::remove_dir_all(&home);
    }
}

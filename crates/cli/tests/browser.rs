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
    served.aws.put(&format!("/superci/{plane}/AGENTS"), "[]");
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
    assert!(served.aws.unanswered.lock().unwrap().is_empty(), "{:?}", served.aws.unanswered.lock().unwrap());
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

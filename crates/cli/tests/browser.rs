//! The dashboard in a browser, used as a person uses it: headless Chrome (`superci_testkit::Browser`) clicks, types
//! and saves, the dashboard's own scripts running, against a control plane served on this computer and the test
//! kit's stand-in for AWS (where the dashboard writes the control plane's settings, and the control plane reads them).
//! Skipped where no Chrome is installed.
mod common;
use common::{dashboard, home, played, played_steps, signed_in};
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

/// What a person sees of a scenario played on `served`: the jobs list, and each job's page with its state and, when it
/// has one, why. (Each scenario that ends in jobs ends with this: the dashboard has to say what happened.)
fn seen_in_browser(served: &superci_testkit::Served) {
    let Some(mut b) = Browser::open(1300, 900) else { eprintln!("no Chrome here: skipped"); return };
    let home = signed_in(served);
    let (mut child, link) = common::dashboard_with(&home, &[("SUPERCI_VIEWS_S", "0")]);
    b.goto(&format!("{link}&p=jobs"));
    b.wait("document.querySelector('main') && !document.querySelector('main').innerText.includes('Reading your control plane')");
    let mut jobs: Vec<serde_json::Value> = served.store.under("job:").into_iter().map(|(_, j)| j).collect();
    jobs.sort_by_key(|j| std::cmp::Reverse(j["at_ms"].as_u64().unwrap_or_default()));
    for j in jobs.iter().take(12) {
        let gl = j["provider"] == "gitlab";
        let id = j["job_id"].as_u64().unwrap_or_default();
        b.goto(&format!("{link}&p=job&id={id}{}", if gl { format!("&gl={}", j["gitlab"].as_str().unwrap_or_default()) } else { String::new() }));
        b.wait("document.querySelector('main') && !document.querySelector('main').innerText.includes('Reading your control plane')");
        let page = b.eval("document.querySelector('main').innerText").as_str().unwrap_or_default().replace('\n', " ").to_lowercase();
        if page.contains("not among the latest jobs") { continue }
        // (Or the page says why it shows no job: the control plane moved, a sign-in to make.)
        if !page.contains("status") && page.contains("control plane") && page.contains("sign in") { continue }
        // (A waiting job's reason is said as it is; a failed one's in plain words, a cancelled one's not at all.)
        let why = if j["state"] == "waiting" { j["error"].as_str().unwrap_or_default().trim_start_matches("waiting: ").split(" (").next().unwrap_or_default().chars().take(40).collect::<String>().to_lowercase() } else { String::new() };
        assert!(page.contains("status") && (why.is_empty() || page.contains(&why)), "job {id} ({}): {why:?} not on its page:\n{page}", j["state"]);
        // Its log, as its page reads it, and as it is (the page's Raw log).
        let gl_q = if gl { format!("&gl={}", j["gitlab"].as_str().unwrap_or_default()) } else { String::new() };
        for what in ["view", "raw"] {
            let got = b.eval(&format!("fetch('/job/{what}?id={id}{gl_q}', {{credentials: 'same-origin', redirect: 'manual'}}).then(r => r.status)"));
            assert!(got.as_u64().is_none_or(|s| s < 500), "job {id}'s {what}: {got}");
        }
    }
    b.goto(&format!("{link}&p=runs"));
    b.wait("document.querySelector('main') && !document.querySelector('main').innerText.includes('Reading your control plane')");
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

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
    // AWS refusing the budget, as AWS words a refusal by the organization's policy: said plainly, nothing kept.
    served.aws.fail("budgets.amazonaws.com", 400, &superci_testkit::json_error("AccessDeniedException", "User: arn:aws:sts::111111111111:assumed-role/Admin/someone is not authorized to perform: budgets:ModifyBudget on resource: arn:aws:budgets::111111111111:budget/superci-plane-testplane0001 with an explicit deny in a service control policy: arn:aws:organizations::222222222222:policy/o-example001/service_control_policy/p-example1"), Some(1));
    let (status, _, said) = common::post(&link, "/routing", &[("action", "account_usd"), ("account", "aws"), ("usd", "40"), ("budget", "emails"), ("email", "ops@example.com")]);
    let said = common::words(&said);
    assert!(status == 502 && said.contains("AWS did not take the budget") && said.contains("the AWS organization the account is in does not allow it") && said.contains("p-example1"), "{status} {said}");
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
    // None given while none is kept: nothing written.
    let (status, _, _) = common::post(&link, "/routing", &[("action", "account"), ("cloud", "aws"), ("networks", "")]);
    assert!(status < 400 && setting("AWS_NETWORKS").is_none(), "{:?}", setting("AWS_NETWORKS"));
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
    // The same again: nothing asked of AWS. Then none: back to SuperCI's network; none again changes nothing.
    let asked = served.aws.asked.lock().unwrap().len();
    let kept = setting("AWS_NETWORKS").unwrap();
    let (status, _, _) = common::post(&link, "/routing", &[("action", "account"), ("cloud", "aws"), ("networks", "us-east-1: subnet-00000000000000001, subnet-00000000000000002, sg-00000000000000001, private")]);
    assert!(status < 400 && setting("AWS_NETWORKS").as_deref() == Some(kept.as_str()) && !served.aws.asked.lock().unwrap()[asked..].iter().any(|a| a["asked"].as_str().unwrap_or_default().contains("DescribeSubnets")));
    for _ in 0..2 {
        let (status, _, said) = common::post(&link, "/routing", &[("action", "account"), ("cloud", "aws"), ("networks", "")]);
        assert!(status < 400 && setting("AWS_NETWORKS").as_deref() == Some("{}"), "{status} {} {:?}", common::words(&said), setting("AWS_NETWORKS"));
    }
    // Cloudflare's location as it is already: nothing written. A limit that is no amount (not finite, below zero): none.
    let (status, _, _) = common::post(&link, "/routing", &[("action", "pool"), ("cloud", "cloudflare"), ("location", "weur")]);
    assert!(status < 400 && setting("CF_LOCATION").as_deref() == Some("weur"), "{:?}", setting("CF_LOCATION"));
    let (status, _, _) = common::post(&link, "/routing", &[("action", "pool"), ("cloud", "cloudflare"), ("location", "weur")]);
    assert!(status < 400 && setting("CF_LOCATION").as_deref() == Some("weur"));
    for usd in ["inf", "-5", "NaN"] {
        let (status, _, _) = common::post(&link, "/routing", &[("action", "pool"), ("cloud", "cloudflare"), ("usd", usd), ("max", "1000")]);
        assert!(status < 400, "{usd}");
        let routing: serde_json::Value = serde_json::from_str(&setting("ROUTING").unwrap_or_default()).unwrap_or_default();
        let pool = routing["order"].as_array().into_iter().flatten().find(|p| p["cloud"] == "cloudflare").cloned().unwrap_or_default();
        assert!(pool["monthly_usd"].is_null() && pool["max_jobs"].is_null(), "{usd}: {pool}");
    }

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

    // The same one again: kept already, nothing changed. A new value for one not kept: refused; for this one: taken.
    let (got, text) = post(&mut b, "/secrets/add", &[("name", "DEPLOY_KEY"), ("value", "again"), ("repo", "acme/app"), ("branch", "release/*")]);
    assert!(got == 409 && text.contains("kept for") && text.contains("already"), "{got} {text}");
    let cli = |args: &[&str]| { let o = common::command(&home).args(args).env("THE_VALUE", "deploy-value-0002").output().unwrap(); (o.status.success(), String::from_utf8_lossy(&o.stdout).to_string() + &String::from_utf8_lossy(&o.stderr)) };
    let (ok, said) = cli(&["secrets", "update", "NOT_KEPT", "--value-env=THE_VALUE"]);
    assert!(!ok && said.contains("is not kept"), "{said}");
    let (ok, said) = cli(&["secrets", "update", "DEPLOY_KEY", "--repo=acme/app", "--branch=release/*", "--value-env=THE_VALUE"]);
    assert!(ok, "{said}");
    assert!(kept()[0].1.contains("deploy-value-0002"), "{:?}", kept());
    // A hundred kept: no more.
    for n in 0..99 {
        let stored = superci_core::secrets::Stored { name: format!("S{n}"), value: "v".into(), scope: Default::default(), updated_ms: 1 };
        served.aws.put(&format!("/superci/{PLANE}/{}", superci_core::secrets::setting(&format!("S{n}"), &Default::default())), &serde_json::to_string(&stored).unwrap());
    }
    let (got, text) = post(&mut b, "/secrets/add", &[("name", "ONE_MORE"), ("value", "v")]);
    assert!(got == 409 && text.contains("100 secrets at most"), "{got} {text}");
    served.aws.params.lock().unwrap().retain(|k, _| !k.contains("/SECRET_S"));
    b.goto(&format!("{link}&p=settings"));
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
    // Every other shape its machine may say, each read again on the same page.
    let page = b.eval("location.href").as_str().unwrap().to_string();
    for (images, said) in [
        (serde_json::json!({ "error": "the bucket was gone" }), "not kept (the bucket was gone)"),
        (serde_json::json!({ "served": 1, "served_bytes": 1_000_000 }), "1 layer from the cache (1.0 MB)"),
        (serde_json::json!({ "pulled": 4, "pulled_bytes": 40_000_000, "uploaded": 3, "had": 1 }), "4 layers pulled (40 MB), kept for the next job"),
        (serde_json::json!({ "pulled": 2, "pulled_bytes": 20_000_000, "keeping": true }), "2 layers pulled (20 MB), being kept for the next job"),
        (serde_json::json!({ "pulled": 1, "pulled_bytes": 5_000_000 }), "1 layer pulled (5.0 MB), none kept for the next job"),
        (serde_json::json!({ "pulled": 9, "pulled_bytes": 90_000_000, "uploaded": 2, "uploaded_bytes": 20_000_000, "missing": 1, "missing_bytes": 10_000_000, "refused": 3, "failed": 3, "note": "the bucket said 503" }),
            "2 of them kept for the next job (20 MB): 1 layer with no whole file on the machine (10 MB), 3 layers the cache would not take, 3 layers not uploaded (the bucket said 503)"),
        (serde_json::json!({ "pulled": 2, "pulled_bytes": 2_000_000, "failed": 2 }), "2 layers not uploaded (failed)"),
        (serde_json::json!({ "off": "no FUSE on it" }), "the cache was not set up on its machine (no FUSE on it)"),
        (serde_json::json!({ "served": 5, "served_bytes": 5_000_000, "pulled": 1, "pulled_bytes": 1_000_000, "off": "no FUSE on it" }), "5 layers from the cache (5.0 MB); 1 layer pulled (1.0 MB), none kept for the next job; the cache was not set up"),
    ] {
        {
            let mut store = served.store.0.lock().unwrap();
            let key = store.keys().find(|k| k.starts_with("job:")).cloned().unwrap();
            let mut j: serde_json::Value = serde_json::from_str(&store[&key]).unwrap();
            j["images"] = images;
            store.insert(key, j.to_string());
        }
        b.goto(&page);
        b.wait(&shows("Docker images"));
        assert!(b.text("main").contains(said), "{said}: {}", b.text("main"));
    }
    // Nothing used: no line said.
    {
        let mut store = served.store.0.lock().unwrap();
        let key = store.keys().find(|k| k.starts_with("job:")).cloned().unwrap();
        let mut j: serde_json::Value = serde_json::from_str(&store[&key]).unwrap();
        j["images"] = serde_json::json!({});
        store.insert(key, j.to_string());
    }
    b.goto(&page);
    b.wait(&shows("Status"));
    assert!(!b.text("main").contains("Docker images"), "{}", b.text("main"));
    seen_in_browser(&served);
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_jobs_page_says_each_step_of_its_start_and_its_runner_as_its_machine_said() {
    let Some(mut b) = Browser::open(1300, 900) else { eprintln!("no Chrome here: skipped"); return };
    let served = serve(|_| {});
    let home = home(&served);
    played(&served, &home.join("checkout"));
    let (key, first) = {
        let store = served.store.0.lock().unwrap();
        let key = store.keys().find(|k| k.starts_with("job:")).cloned().unwrap();
        let j: serde_json::Value = serde_json::from_str(&store[&key]).unwrap();
        (key, j)
    };
    let t = first["at_ms"].as_u64().unwrap();
    let (mut child, link) = dashboard(&home);
    b.goto(&link);
    b.wait(&shows("Usage"));
    b.click("nav a", "Runs");
    b.wait(&shows("Recent runs"));
    b.click(".tabs a", "Jobs");
    b.wait(&shows("Recent jobs"));
    b.click("main a", "tests");
    b.wait(&shows("Its start"));
    let page = b.eval("location.href").as_str().unwrap().to_string();
    let asks = || serde_json::json!([
        { "at": 1, "ms": 300, "kind": "m8a.2xlarge", "spot": true, "said": "InsufficientInstanceCapacity", "zone": "us-east-1a", "usd": 0.135 },
        { "at": 1, "ms": 300, "kind": "m8a.2xlarge", "spot": true, "said": "InsufficientInstanceCapacity", "zone": "us-east-1b" },
        { "at": 1, "ms": 300, "kind": "m7a.2xlarge", "spot": true, "said": "MaxSpotInstanceCountExceeded" },
        { "at": 1, "ms": 300, "kind": "c7a.2xlarge", "spot": true, "said": "RequestLimitExceeded" },
        { "at": 1, "ms": 300, "kind": "c6a.2xlarge", "spot": true, "said": "Unsupported" },
        { "at": 1, "ms": 300, "kind": "r8a.2xlarge", "spot": true, "said": "InsufficientInstanceCapacity", "pools": 14 },
        { "at": 1, "ms": 300, "kind": "r7a.2xlarge", "spot": true, "said": "VcpuLimitExceeded", "pools": 3 },
        { "at": 1, "ms": 300, "kind": "r6a.2xlarge", "spot": true, "said": "Unsupported", "pools": 2 },
        { "at": 1, "ms": 300, "kind": "m8a.2xlarge", "spot": false, "said": "InsufficientInstanceCapacity" },
        { "at": 1, "ms": 300, "kind": "m6a.large", "spot": false, "said": "started", "zone": "us-east-1c" },
        { "at": 1, "ms": 300, "kind": "m6a.2xlarge", "spot": true, "said": "started", "zone": "us-east-1d", "pools": 5 },
    ]);
    let asked = || "AWS gave a machine (no spot m8a.2xlarge in 2 zones; your account's limit refused spot m7a.2xlarge; AWS took no more requests for spot c7a.2xlarge; Unsupported spot c6a.2xlarge; \
        no spot machine in 14 other pools; your account's limit refused 3 other pools; Unsupported for 2 other pools; no on-demand m8a.2xlarge; on-demand m6a.large in us-east-1c; m6a.2xlarge in us-east-1d, AWS's choice of 5 pools)".to_string();
    let aws = |more: serde_json::Value| { let mut m = serde_json::json!({ "cloud": "aws", "machine_id": "i-0a1b2c3d4e5f60718", "launched_ms": t + 3_000 }); for (k, v) in more.as_object().unwrap() { m[k] = v.clone() } m };
    for (more, said) in [
        // From GitHub, after a wait under the spot limit, every step its machine timed, and what its runner logged.
        (aws(serde_json::json!({ "provider": "github", "queued_ms": t - 2_000, "spot_waited_ms": 1_000, "started_ms": t + 41_000, "cpu": 8, "machine_type": "m8a.2xlarge", "disk_gb": 60, "disk_mibs": 500, "ramdisk": true, "on_demand": true,
            "machine": { "script_ms": t + 20_000, "boot_s": 5.0, "read_ms": t + 22_000, "ready_ms": t + 25_000, "listening_ms": t + 40_000, "github_wait_s": 12, "log": "[RUNNER] waiting ... 30 s ..." } })),
            vec!["GitHub told SuperCI of the job", "waited for room under your AWS spot limit", "AWS gave a machine", "the machine was made and switched on", "it booted", "its disk and Docker were set up",
                "the runner's files were read from the image (GitHub refused the first connection; reachable after 12 s)", "the runner started and reached GitHub", "GitHub gave it the job",
                "What its runner logged while it was slow to reach GitHub", "[RUNNER] waiting ... 30 s ...", "8 CPU · m8a.2xlarge · 500 MiB/s disk · RAM disk · on-demand"]),
        // A run from a computer's: no read of the image timed; the control plane gave it the job.
        (aws(serde_json::json!({ "started_ms": t + 30_000, "machine_type": "c8a.xlarge", "disk_gb": 100, "disk_mibs": 300, "local_disk": true,
            "machine": { "script_ms": t + 20_000, "boot_s": 5.0, "ready_ms": t + 25_000, "listening_ms": t + 28_000, "log": "[RUNNER] retrying" } })),
            vec!["its start-up script ran", "the runner started and reached the control plane", "the control plane gave it the job", "slow to reach the control plane", "c8a.xlarge · 100 GB disk at 300 MiB/s · local disk"]),
        // Its runner listening, no job taken yet.
        (aws(serde_json::json!({ "machine_type": "c8a.xlarge", "disk_gb": 200, "machine": { "script_ms": t + 20_000, "boot_s": 5.0, "ready_ms": t + 25_000, "listening_ms": t + 28_000 } })),
            vec!["the runner started and reached the control plane", "c8a.xlarge · 200 GB disk"]),
        (aws(serde_json::json!({ "provider": "github", "machine": { "script_ms": t + 20_000, "boot_s": 5.0, "ready_ms": t + 25_000, "listening_ms": t + 28_000 } })),
            vec!["the runner started and reached GitHub"]),
        // The job taken with no time of its runner listening.
        (aws(serde_json::json!({ "started_ms": t + 30_000, "machine": { "script_ms": t + 20_000, "boot_s": 5.0, "ready_ms": t + 25_000 } })),
            vec!["the runner started and took the job"]),
        // No machine given yet: only GitHub's word.
        (serde_json::json!({ "provider": "github", "queued_ms": t - 2_000 }), vec!["GitHub told SuperCI of the job"]),
        // From before computers were set aside.
        (serde_json::json!({ "cloud": "machine" }), vec!["Your computer"]),
        // What AWS answered each ask, said in a few words beside its step.
        (aws(serde_json::json!({ "machine_type": "m6a.2xlarge", "started_ms": t + 30_000, "asks": asks() })), vec![&*asked()]),
    ] {
        let mut j = first.clone();
        for k in ["machine", "started_ms", "queued_ms", "launched_ms", "machine_id", "spot_waited_ms", "machine_type", "cpu", "disk_gb", "disk_mibs"] { j.as_object_mut().unwrap().remove(k); }
        for (k, v) in more.as_object().unwrap() { j[k] = v.clone() }
        // (What its machine said is kept apart from its record: see `MachineStart`.)
        let hello = format!("hello:{}", key.trim_start_matches("job:"));
        {
            let mut store = served.store.0.lock().unwrap();
            match j.as_object_mut().unwrap().remove("machine") { Some(m) => { store.insert(hello, m.to_string()); } None => { store.remove(&hello); } }
            store.insert(key.clone(), j.to_string());
        }
        b.goto(&page);
        b.wait(&shows("Status"));
        let main = b.text("main");
        for s in said { assert!(main.contains(s), "{s}: {main}") }
    }
    // Not its first choice, on the jobs list: the types AWS had no spot machine of, where and at what price.
    b.click("a.chip", "Jobs");
    b.wait(&shows("Not the first choice: AWS had no spot m8a.2xlarge in us-east-1a ($0.135 an hour) or r8a.2xlarge"));
    served.store.0.lock().unwrap().insert(key.clone(), first.to_string());
    seen_in_browser(&served);
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
    // Cloudflare's other words for a token it no longer takes: the same.
    for said in ["Invalid access token", "Your sign-in expired"] {
        let served = serve(|_| {});
        let home = signed_in(&served);
        served.aws.fail("api.cloudflare.com", 403, &serde_json::json!({ "success": false, "errors": [{ "code": 10000, "message": said }], "messages": [], "result": null }).to_string(), None);
        let file = home.join("workspaces/default/sign-ins.json");
        let mut kept: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        kept["cloudflare"] = serde_json::json!({ "access": "example-access_token-ended", "expires_at_ms": 4102444800000u64, "refresh": "example-refresh_token-ended" });
        std::fs::write(&file, kept.to_string()).unwrap();
        let (mut child, link) = dashboard(&home);
        b.goto(&link);
        b.wait(&page_shows("Sign in with Cloudflare"));
        let _ = child.kill();
        let _ = std::fs::remove_dir_all(&home);
    }
    // A token given in SUPERCI_CLOUDFLARE_TOKEN that Cloudflare refuses: said as that token, no sign-in offered.
    let served = serve(|_| {});
    let home = signed_in(&served);
    served.aws.replaying(include_str!("../fixtures/cloudflare/sign-in/ended.jsonl"), &[]);
    let (mut child, link) = common::dashboard_with(&home, &[("SUPERCI_CLOUDFLARE_TOKEN", "example-access_token-ended")]);
    b.goto(&link);
    b.wait(&page_shows("SUPERCI_CLOUDFLARE_TOKEN"));
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
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
    // (A budget in AWS made for it, and its network that AWS will not delete yet: each said as left.)
    other.aws.put("/superci/testplane0002/ROUTING", r#"{"aws_budget":{"stop":false},"account_usd":{"aws":100.0}}"#);
    served.aws.put("/superci/testplane0002/ROUTING", r#"{"aws_budget":{"stop":false},"account_usd":{"aws":100.0}}"#);
    served.aws.fail("DeleteVpc", 400, &superci_testkit::ec2_error("DependencyViolation", "The vpc 'vpc-00000000000000001' has dependencies and cannot be deleted."), None);
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
    b.goto(&format!("{link}&p=planes"));
    b.wait(&shows("Control plane"));
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
    // (And a second organization's App.)
    let mut second = superci_testkit::github::GitHub::app();
    second["id"] = 2002.into(); second["owner"] = "globex".into(); second["slug"] = "superci-globex".into();
    served.aws.put(&format!("/superci/{plane}/GITHUB_APP_2002"), &second.to_string());
    let aws = served.aws.clone();
    let new = superci_testkit::serve_as("testplane0002", move |c| c.read_settings(&aws.settings("testplane0002")));
    for (k, v) in new.aws.settings("testplane0002") { served.aws.put(&format!("/superci/testplane0002/{k}"), &v) }
    // Its history: a finished GitHub job, a finished GitLab job with its runner's record; one of them kept at the new
    // one already (taken once).
    let now = served.now_ms();
    let done = |id: &str, provider: &str| serde_json::json!({ "job_id": id.trim_start_matches("gl").parse::<u64>().unwrap(), "run_id": 3, "repo": "acme/app", "state": "done", "at_ms": now - 60_000, "started_ms": now - 50_000, "ended_ms": now - 10_000,
        "cloud": "cloudflare", "provider": provider, "seen_in_progress": true, "began": true, "conclusion": "success" }).to_string();
    {
        let mut m = served.store.0.lock().unwrap();
        m.insert("job:900".into(), done("900", ""));
        m.insert("job:gl17000000901".into(), done("gl17000000901", "gitlab"));
        m.insert("glrunner:999".into(), serde_json::json!({ "id": 999, "job": 17000000901u64 }).to_string());
    }
    new.store.0.lock().unwrap().insert("job:900".into(), done("900", ""));
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
    for k in ["GITHUB_APP", "GITHUB_APP_2002", "GITLAB", "GITLAB_gsecond", "NAME", "LABELS", "CF_LOCATION", "CF_IMAGE", "AWS_NETWORKS"] { assert!(has(k).is_some(), "{k}: {:?}", served.aws.settings("testplane0002")) }
    assert_eq!(has("NAME").as_deref(), Some("AcmeCI"));
    assert!(new.store.0.lock().unwrap().contains_key("glrunner:999"), "its GitLab runner's record came along");
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
    // A job's log asked for by an address that is no job's: refused. Each job's log opened (more than the few kept
    // for their next opens), and a GitHub job's raw log.
    let ask = |b: &mut Browser, path: &str| -> u64 { b.eval(&format!("fetch({path:?}, {{credentials: 'same-origin', redirect: 'manual'}}).then(r => r.status)")).as_u64().unwrap_or(0) };
    assert_eq!(ask(&mut b, "/job/view?id=abc"), 400);
    let ids: Vec<String> = served.store.under("job:").into_iter().filter_map(|(_, j)| j["job_id"].as_u64().map(|n| n.to_string())).collect();
    assert!(ids.len() >= 5, "{ids:?}");
    for id in &ids { assert!(ask(&mut b, &format!("/job/view?id={id}")) < 400) }
    assert!(ask(&mut b, &format!("/job/raw?id={}", ids[0])) < 400);
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
        // (Asked again meanwhile: the one under way goes on, nothing more is started.)
        let (got, _) = post(&mut b, "/plane/update", &[("plane", id)]);
        assert!(got < 400);
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
    // What a budget in AWS must not be: no email address, an answer that is none of AWS's, a lift with nothing to
    // lift, a budget with no monthly limit. Each refused, nothing changed.
    for (fields, said) in [
        ({ let mut f = account("120", "emails"); f.push(("email".into(), "not an address".into())); f }, "That is no email address"),
        ({ let mut f = account("130", "emails"); f.push(("email".into(), "nobody-at-example.com".into())); f }, "That is no email address"),
        (account("120", "sometimes"), "none, emails or stops"),
        (account("120", "lift"), "There is no budget in AWS to lift"),
        (account("", "emails"), "A budget in AWS needs the monthly limit"),
    ] {
        let (got, text) = post(&mut b, "/routing", &fields.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect::<Vec<_>>());
        assert!(got == 400 && text.contains(said), "{fields:?}: {got} {text}");
    }
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
    // Its update: not from here, without that sign-in.
    let (got, text) = post(&mut b, "/plane/update", &[]);
    assert!(got >= 400 && text.contains("Sign in where this control plane runs to update it"), "{got} {text}");
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
    // As a classic account answered: a region where the image jobs run on is not yet, refused. As a project of AWS's
    // new sign-up: a region its organization refuses, refused, saying whose rule it is.
    for (look, region, marker, said) in [(include_str!("../fixtures/aws/classic/look.jsonl"), "ca-central-1", "Not in Canada", "No job could start in Canada"),
        (include_str!("../fixtures/aws/project-activated/look.jsonl"), "us-east-1", "The other 11 are refused", "AWS account 111111111111 cannot use N. Virginia")] {
        account.replaying(look, &[]);
        // (Looked at again, as its Refresh does; the page shows that account before anything is asked of it.)
        post(&mut b, "/aws/look", &[("next", "?p=plane")]);
        // (Looked at in the background: read again until the page has the new look, under load a while.)
        let end = std::time::Instant::now() + std::time::Duration::from_secs(90);
        loop {
            b.goto(&format!("{link}&p=plane"));
            b.wait("document.querySelector('main') !== null");
            if b.eval(&reads(marker)).as_bool() == Some(true) { break }
            assert!(std::time::Instant::now() < end, "{marker}: {}", b.text("main"));
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
        let (got, text) = post(&mut b, "/plane/aws", &[("region", region), ("account", "111111111111")]);
        assert!(got == 409 && text.contains(said), "{region}: {got} {text}");
    }
    // (Back to the classic account, where N. Virginia is allowed.)
    account.replaying(include_str!("../fixtures/aws/classic/look.jsonl"), &[]);
    post(&mut b, "/aws/look", &[("next", "?p=plane")]);
    let end = std::time::Instant::now() + std::time::Duration::from_secs(90);
    loop {
        b.goto(&format!("{link}&p=plane"));
        b.wait("document.querySelector('main') !== null");
        if b.eval(&reads("Not in Canada")).as_bool() == Some(true) { break }
        assert!(std::time::Instant::now() < end, "{}", b.text("main"));
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    // With nothing answered by AWS it stops, and says so; started over, what it made is removed.
    let (got, text) = post(&mut b, "/plane/aws", &[("region", "us-east-1"), ("account", "111111111111")]);
    assert!(got < 400, "{got} {text}");
    // (Asked again while it runs: the one under way, nothing more started.)
    let (got, _) = post(&mut b, "/plane/aws", &[("region", "us-east-1"), ("account", "111111111111")]);
    assert!(got < 500);
    let end = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while !common::words(&common::part(&link, "/?p=plane&fragment=1").1).contains("Try again") { assert!(std::time::Instant::now() < end, "{}", common::words(&common::part(&link, "/?p=plane&fragment=1").1)); std::thread::sleep(std::time::Duration::from_millis(300)) }
    let (got, text) = post(&mut b, "/plane/start-over", &[]);
    assert!(got < 400, "{got} {text}");
    // As AWS answered a real one: made, and the overview leads on to connecting GitHub or GitLab.
    account.replaying(include_str!("../fixtures/aws/planes/deploy.jsonl"), &[("testplane0002", "deployplane1"), ("http://new-plane.test", &new.url)]);
    // (Its id given, as Try again gives it: the names the first try made, again.)
    let (got, text) = post(&mut b, "/plane/aws", &[("region", "us-east-1"), ("account", "111111111111"), ("plane", "deployplane1")]);
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
    // Repositories lists them both, and Permissions has a row for each.
    b.goto(&format!("{link}&p=repos"));
    b.wait(&reads("GitLab"));
    b.goto(&format!("{link}&p=planes"));
    b.wait(&reads("Permissions"));
    assert!(b.eval("document.querySelector('main').innerText").as_str().unwrap().matches("GitLab").count() >= 2);
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
    served.connect_gitlab();
    // (And a GitLab job, the third container; and three more that are not measured: one measured already, one whose
    // container is no Cloudflare one, one ended a moment ago.)
    let (gid, p) = served.gitlab.pending("gl-test", &["superci"], served.now_ms());
    served.gitlab_webhook(p);
    served.gitlab_webhook(served.gitlab.running(gid, served.now_ms()));
    served.gitlab_webhook(served.gitlab.finish(gid, "success", "ok", served.now_ms()));
    for n in 1..=5 { let (id, q) = served.github.queue(&format!("job{n}"), "superci", served.now_ms()); served.webhook(q); served.webhook(served.github.start(id)); served.later(60_000); served.webhook(served.github.finish(id, "success", "ok", served.now_ms())); }
    // Each one's container as Cloudflare named it, ended ten minutes ago.
    {
        let mut store = served.store.0.lock().unwrap();
        let keys: Vec<String> = store.keys().filter(|k| k.starts_with("job:")).cloned().collect();
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
        let mut keys = keys;
        keys.sort_by_key(|k| (!k.starts_with("job:gl"), k.clone()));
        for (i, k) in keys.iter().enumerate() {
            let mut j: serde_json::Value = serde_json::from_str(&store[k]).unwrap();
            // The GitLab job the third container, the first two GitHub jobs the others; then the three left out.
            let n = if i == 0 { 3 } else { i };
            j["machine_id"] = if i == 4 { "not-a-cloudflare-container".to_string() } else { format!("{n:064x}") }.into();
            j["machine_type"] = "4cpu-12gb".into();
            let ended = if i == 5 { now - 60_000 } else { now - 600_000 };
            j["started_ms"] = (ended - 3_000).into(); j["ended_ms"] = ended.into(); j["launched_ms"] = (ended - 10_000).into();
            if i == 3 { j["cost_from"] = "measured".into(); j["usd"] = 0.001.into() }
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
    until("kept as measured", || served.store.under("job:").iter().filter(|(_, j)| j["cost_from"] == "measured").count() >= 4);
    assert!(served.store.under("job:").iter().any(|(k, j)| k.starts_with("job:gl") && j["cost_from"] == "measured"), "the GitLab job measured too");
    // Runners: Cloudflare's month as it bills it (what its Workers Paid plan includes, and what is billed after).
    b.goto(&format!("{link}&p=runners"));
    b.wait(&reads("this month ·"));
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
    // The page after: the new one in use, the old one listed as moved away (a place to move back to).
    b.goto(&format!("{link}&p=planes"));
    b.wait(&format!("{} && !document.querySelector('main').innerText.includes('Reading your control plane')", shows("Control plane")));
    b.goto(&format!("{}/job/view?id=0", link.split("/?").next().unwrap()));
    let (got, _) = post(&mut b, "/plane/move", &[("plane", PLANE)]);
    assert!(got < 500, "{got}");
    b.goto(&format!("{link}&p=planes"));
    b.wait("document.querySelector('main') !== null");
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

#[test]
fn a_second_control_plane_given_a_code_host_of_its_own_makes_two_cis_listed_and_acted_on_one_at_a_time() {
    let Some(mut b) = Browser::open(1300, 900) else { eprintln!("no Chrome here: skipped"); return };
    // The CI in use (GitHub); a control plane set up in Cloudflare beside it, as this workspace sets one up to move to.
    let served = serve(|_| {});
    served.connect_github();
    let aws = served.aws.clone();
    let cf = superci_testkit::serve_as("cfplane00003", move |c| {
        let settings = aws.worker_settings("superci");
        c.read_settings(&settings);
        if let Some((_, on)) = settings.iter().find(|(k, _)| k == "CONTAINERS") { c.containers = on == "on" }
    });
    cf.aws.params.lock().unwrap().remove("/superci/cfplane00003/CONTAINERS");
    served.aws.workers.lock().unwrap().insert("superci.acme.workers.dev".into(), cf.url.clone());
    let home = signed_in(&served);
    let file = home.join("workspaces/default/sign-ins.json");
    let mut kept: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    kept["cloudflare"] = serde_json::json!({ "access": "cloudflare-access-for-tests", "expires_at_ms": 4102444800000u64, "refresh": "cloudflare-refresh-for-tests" });
    std::fs::write(&file, kept.to_string()).unwrap();
    let fixture = |name: &str| std::fs::read_to_string(format!("{}/fixtures/{name}.jsonl", env!("CARGO_MANIFEST_DIR"))).unwrap();
    served.aws.replaying(&[fixture("cloudflare/sign-in/usage"), fixture("update/01-deploy-cf")].concat(), &[]);
    let (mut child, link) = common::dashboard_with(&home, &[("SUPERCI_NEW_PLANE_ID", "cfplane00003"), ("SUPERCI_LAST_GOOD_S", "1"), ("SUPERCI_VIEWS_S", "0")]);
    b.goto(&format!("{link}&p=planes"));
    b.wait(&shows("Set up"));
    b.click("form[action=\"/plane/cloudflare\"] button", "Set up");
    let end = std::time::Instant::now() + std::time::Duration::from_secs(120);
    while !common::words(&common::part(&link, "/?p=planes&fragment=1").1).contains("superci.acme.workers.dev Move here") { assert!(std::time::Instant::now() < end, "set up"); std::thread::sleep(std::time::Duration::from_millis(500)) }
    // GitLab connected to the Cloudflare one directly, from its own page, rather than moving: two CIs live here.
    let token = superci_testkit::gitlab::GitLab::connection()["token"].as_str().unwrap().to_string();
    b.goto(&format!("{link}&c=cfplane00003&p=repos"));
    b.wait(&page_shows("Repositories"));
    let (got, text) = post(&mut b, "/gitlab/connect?c=cfplane00003", &[("url", "https://gitlab.com"), ("token", &token), ("which", "new")]);
    assert!(got < 400, "{got} {text}");
    // The dashboard, naming neither: the list of them, to open one.
    b.goto(&link);
    b.wait(&format!("{} && {} && {}", shows("Control planes"), reads("GitHub: acme · AWS · 111111111111"), reads("GitLab: gitlab.com · Cloudflare · Acme")));
    // The GitLab one stops answering: no longer a live CI, the dashboard opens the other; the list (asked for) says
    // which does not answer. Answering again, as before.
    cf.down.store(true, std::sync::atomic::Ordering::SeqCst);
    std::thread::sleep(std::time::Duration::from_secs(2));
    b.goto(&link);
    b.wait(&shows("Usage"));
    b.goto(&format!("{link}&p=all"));
    b.wait(&reads("Not answering"));
    cf.down.store(false, std::sync::atomic::Ordering::SeqCst);
    b.goto(&link);
    b.wait(&format!("{} && {}", shows("Control planes"), reads("GitLab: gitlab.com · Cloudflare · Acme")));
    // A change that names neither: refused, nothing changed.
    let (got, text) = post(&mut b, "/labels", &[("label", "acmeci")]);
    assert!(got == 409 && text.contains("Which control plane?"), "{got} {text}");
    // One opened: its pages; Control plane lists the other as a CI of its own, to open.
    b.goto(&format!("{link}&c={PLANE}&p=planes"));
    b.wait(&shows("Serves a code host of its own"));
    b.goto(&format!("{link}&c=cfplane00003&p=overview"));
    b.wait(&page_shows("Overview"));
    // The other CI is no place to move to, nor to delete as a spare: refused, saying why.
    let (got, text) = post(&mut b, &format!("/plane/move?c={PLANE}"), &[("plane", "cfplane00003")]);
    assert!(got == 409 && text.contains("That control plane serves a code host of its own"), "{got} {text}");
    let (got, text) = post(&mut b, &format!("/plane/delete?c={PLANE}"), &[("plane", "cfplane00003")]);
    assert!(got == 409 && text.contains("This control plane serves a code host"), "{got} {text}");
    // SuperCI stopped for the GitLab one, named: GitLab let go of and it deleted (as Cloudflare answered); the other CI
    // left as it is.
    served.aws.replaying(&fixture("update/07-delete-cf"), &[]);
    let (got, text) = post(&mut b, "/plane/leave?c=cfplane00003", &[("plane", "cfplane00003"), ("confirm", "superci")]);
    assert!(got < 400, "{got} {text}");
    let end = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while common::words(&common::part(&link, &format!("/?c={PLANE}&p=planes&fragment=1")).1).contains("superci.acme.workers.dev") { assert!(std::time::Instant::now() < end, "the GitLab one deleted"); std::thread::sleep(std::time::Duration::from_millis(500)) }
    assert!(served.aws.settings(PLANE).iter().any(|(k, _)| k == "GITHUB_APP"), "the other CI as it was");
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_cloud_that_does_not_answer_while_control_planes_are_looked_for_is_noted_and_an_aws_sign_in_aws_will_not_renew_is_said() {
    let Some(mut b) = Browser::open(1300, 900) else { eprintln!("no Chrome here: skipped"); return };
    // Cloudflare signed in, answering nothing: said on the page, which shows the control plane in AWS all the same.
    let served = serve(|_| {});
    let home = signed_in(&served);
    let file = home.join("workspaces/default/sign-ins.json");
    let mut kept: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    kept["cloudflare"] = serde_json::json!({ "access": "cloudflare-access-for-tests", "expires_at_ms": 4102444800000u64, "refresh": "cloudflare-refresh-for-tests" });
    std::fs::write(&file, kept.to_string()).unwrap();
    let (mut child, link) = dashboard(&home);
    b.goto(&link);
    b.wait(&format!("{} && {}", page_shows("Cloudflare did not answer ("), shows("AWS · 111111111111")));
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
    // AWS's credentials due, and AWS not renewing them (its answer a refusal): the sign-in is said to have ended.
    let served = serve(|_| {});
    let home = signed_in(&served);
    let file = home.join("workspaces/default/sign-ins.json");
    let mut kept: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    kept["aws"]["creds"]["expires_at_ms"] = 0.into();
    kept["aws"]["refresh_token"] = "superci-ended-refresh-token-for-a-recording".into();
    kept["aws"]["dpop"] = serde_json::to_value(superci_core::crypto::SigningKeyStore::generate()).unwrap();
    std::fs::write(&file, kept.to_string()).unwrap();
    // (AWS's answer to a refresh token it does not take, as it answered.)
    served.aws.replaying(include_str!("../fixtures/aws/sign-in/ended.jsonl"), &[]);
    let (mut child, link) = dashboard(&home);
    b.goto(&link);
    b.wait(&page_shows("AWS sign-in has ended"));
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}


#[test]
fn a_job_whose_machine_could_not_start_is_counted_as_one_that_did_not_run_and_listed_as_failed() {
    let Some(mut b) = Browser::open(1300, 900) else { eprintln!("no Chrome here: skipped"); return };
    // A job for AWS, which does not give its machine.
    let served = serve(|c| {
        (c.aws_own, c.aws_own_creds) = (Some(("111111111111".into(), "us-east-1".into())), Some(superci_core::aws::Credentials { access_key_id: "ASIAX".into(), secret_access_key: "secret".into(), session_token: Some("tok".into()), expires_at_ms: u64::MAX }));
    });
    served.connect_github();
    let (_, q) = served.github.queue("build", "superci/provider=aws", served.now_ms());
    served.webhook(q);
    for _ in 0..3 { served.later(600_000); served.upkeep(); }
    let home = home(&served);
    let (mut child, link) = dashboard(&home);
    b.goto(&link);
    b.wait(&reads("1 job · 1 did not run"));
    b.click("nav a", "Runs");
    b.click(".tabs a", "Jobs");
    b.wait(&reads("Failed"));
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}


#[test]
fn control_planes_that_stop_answering_after_they_did_are_said_so_and_come_back_when_they_answer() {
    let Some(mut b) = Browser::open(1300, 900) else { eprintln!("no Chrome here: skipped"); return };
    // The one in use (GitHub), and another in the account; both answering at first.
    let served = serve(|_| {});
    served.connect_github();
    let aws = served.aws.clone();
    let other = superci_testkit::serve_as("testplane0002", move |c| { let s = aws.settings("testplane0002"); if !s.is_empty() { c.read_settings(&s) } });
    for (k, v) in other.aws.settings("testplane0002") { served.aws.put(&format!("/superci/testplane0002/{k}"), &v) }
    let home = signed_in(&served);
    served.aws.replaying(include_str!("../fixtures/aws/planes/delete.jsonl"), &[("http://new-plane.test", &other.url)]);
    // (Read again on every page, and a passing miss not held over: as a minute or two later would be.)
    let (mut child, link) = common::dashboard_with(&home, &[("SUPERCI_LAST_GOOD_S", "1"), ("SUPERCI_KEY_WAIT_S", "1"), ("SUPERCI_VIEWS_S", "0")]);
    b.goto(&format!("{link}&p=planes"));
    b.wait(&reads(&format!("{} Move here", other.url)));
    // The other stops answering: its row says so (not "Move here").
    other.down.store(true, std::sync::atomic::Ordering::SeqCst);
    std::thread::sleep(std::time::Duration::from_secs(2));
    b.goto(&format!("{link}&p=planes"));
    b.wait(&reads(&format!("{} Not answering", other.url)));
    // The one in use stops too: every page says so.
    served.down.store(true, std::sync::atomic::Ordering::SeqCst);
    std::thread::sleep(std::time::Duration::from_secs(2));
    for p in ["overview", "runners", "jobs"] {
        b.goto(&format!("{link}&p={p}"));
        b.wait(&shows("Your control plane is not answering"));
    }
    b.goto(&format!("{link}&p=planes"));
    b.wait(&reads(&format!("{} Not answering In use", served.url)));
    // Answering again: its pages as before.
    served.down.store(false, std::sync::atomic::Ordering::SeqCst);
    b.goto(&link);
    b.wait(&shows("Usage"));
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}


#[test]
fn a_second_organizations_app_is_listed_beside_the_first_and_removed_from_repositories() {
    let Some(mut b) = Browser::open(1300, 900) else { eprintln!("no Chrome here: skipped"); return };
    // acme's App, and globex's added beside it (the test kit's GitHub answers both alike).
    let served = serve(|_| {});
    served.connect_github();
    let mut second = superci_testkit::github::GitHub::app();
    second["id"] = 2002.into(); second["owner"] = "globex".into(); second["slug"] = "superci-globex".into();
    served.aws.put(&format!("/superci/{PLANE}/GITHUB_APP_2002"), &second.to_string());
    let home = signed_in(&served);
    let (mut child, link) = dashboard(&home);
    b.goto(&format!("{link}&p=repos"));
    b.wait(&format!("{} && {}", reads("GitHub · acme"), reads("GitHub · globex")));
    b.goto(&format!("{link}&p=planes"));
    b.wait(&format!("{} && {}", reads("GitHub acme: App"), reads("GitHub globex: App")));
    // Refused: an organization that is none, or not one added here (the first is not removed this way).
    for app in ["acme", "1001", "9999"] {
        let (got, _) = post(&mut b, "/github/remove", &[("app", app)]);
        assert!(got >= 400, "{app}");
    }
    // globex removed: uninstalled, forgotten here; acme's as it was.
    let (got, text) = post(&mut b, "/github/remove", &[("app", "2002")]);
    assert!(got < 400, "{got} {text}");
    until("forgotten", || !served.aws.settings(PLANE).iter().any(|(k, _)| k == "GITHUB_APP_2002"));
    assert!(served.aws.settings(PLANE).iter().any(|(k, _)| k == "GITHUB_APP"));
    b.goto(&format!("{link}&p=repos"));
    b.wait(&format!("{} && {} && !document.querySelector('main').innerText.includes('GitHub · globex')", reads("Removed globex. Its App"), reads("Jobs from acme")));
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn repositories_on_a_control_plane_too_old_for_more_organizations_or_gitlabs_says_to_update_it() {
    let Some(mut b) = Browser::open(1300, 900) else { eprintln!("no Chrome here: skipped"); return };
    let served = serve(|_| {});
    served.connect_github();
    served.connect_gitlab();
    *served.aws.old_version.lock().unwrap() = Some("0.9.30".into());
    let home = signed_in(&served);
    let (mut child, link) = dashboard(&home);
    b.goto(&format!("{link}&p=repos"));
    b.wait(&format!("{} && {}", reads("several organizations need 0.9.31 or newer"), reads("0.9.37")));
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn providers_in_the_order_that_this_control_plane_has_not_added_are_said_and_rules_from_before_shown() {
    let Some(mut b) = Browser::open(1300, 900) else { eprintln!("no Chrome here: skipped"); return };
    // Cloudflare's containers added; the order (as after a move) names Modal too, then AWS's on-demand machines.
    let served = serve(|_| {});
    let home = home(&served);
    for (order, said) in [(r#"[{"cloud":"cloudflare"},{"cloud":"modal"}]"#, "Modal is not added to this control plane yet · Add it"),
        (r#"[{"cloud":"cloudflare"},{"cloud":"modal"},{"cloud":"aws-on-demand"}]"#, "are not added to this control plane yet · Add them")] {
        served.aws.put(&format!("/superci/{PLANE}/ROUTING"), &format!(r#"{{"default":null,"rules":[{{"repo":"acme/*","cloud":"cloudflare"}}],"order":{order}}}"#));
        let (mut child, link) = dashboard(&home);
        b.goto(&format!("{link}&p=runners"));
        b.wait(&format!("{} && {}", reads(said), reads("acme/* only uses")));
        let _ = child.kill();
    }
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn the_limits_card_on_control_planes_from_before_its_limits_says_what_they_lack() {
    let Some(mut b) = Browser::open(1300, 900) else { eprintln!("no Chrome here: skipped"); return };
    // Before account limits (0.14): said in their place. Before a day's (0.15): the month's only.
    for (version, said) in [("0.14.0", "A limit for everything, and for a cloud account, needs a newer control plane"), ("0.15.3", "this month")] {
        let served = serve(|_| {});
        *served.aws.old_version.lock().unwrap() = Some(version.into());
        let home = signed_in(&served);
        let (mut child, link) = dashboard(&home);
        b.goto(&format!("{link}&p=runners"));
        b.wait(&format!("{} && !document.querySelector('main').innerText.includes('Reading your control plane')", reads(said)));
        if version == "0.15.3" { assert!(!b.eval("document.querySelector('main').innerText").as_str().unwrap().contains("today"), "no day's limit") }
        // What it cannot keep is refused, saying to update it; what it can is set.
        let refused: Vec<Vec<(&str, &str)>> = if version == "0.14.0" { vec![vec![("action", "monthly_usd"), ("usd", "100")], vec![("action", "account_usd"), ("account", "aws"), ("usd", "100")]] }
            else { vec![vec![("action", "monthly_usd"), ("usd", "100"), ("daily", "10")], vec![("action", "account_usd"), ("account", "aws"), ("usd", "100"), ("budget", "emails")]] };
        for fields in refused {
            let (got, text) = post(&mut b, "/routing", &fields);
            assert!(got == 409 && text.contains("Update the control plane first"), "{version} {fields:?}: {got} {text}");
        }
        if version == "0.15.3" {
            for fields in [vec![("action", "monthly_usd"), ("usd", "100")], vec![("action", "account_usd"), ("account", "aws"), ("usd", "50")]] {
                let (got, text) = post(&mut b, "/routing", &fields);
                assert!(got < 400, "{fields:?}: {got} {text}");
            }
            b.goto(&format!("{link}&p=runners"));
            b.wait(&reads("$100.00"));
        }
        let _ = child.kill();
        let _ = std::fs::remove_dir_all(&home);
    }
}

#[test]
fn a_control_plane_with_a_modal_launcher_and_modal_not_signed_in_says_its_runners_work_without_it() {
    let Some(mut b) = Browser::open(1300, 900) else { eprintln!("no Chrome here: skipped"); return };
    let served = serve(|_| {});
    served.aws.put(&format!("/superci/{PLANE}/LAUNCHERS"), r#"[{"cloud":"modal","url":"https://acme--superci-runners.modal.run"}]"#);
    let home = signed_in(&served);
    let (mut child, link) = dashboard(&home);
    b.goto(&format!("{link}&p=planes"));
    b.wait(&reads("Optional: its runners work without it"));
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}


#[test]
fn a_modal_sign_in_started_from_the_dashboard_waits_for_its_approval_on_the_pages_that_need_it() {
    let Some(mut b) = Browser::open(1300, 900) else { eprintln!("no Chrome here: skipped"); return };
    // A Modal launcher, Modal not signed in; its sign-in started here (Modal's token flow, as it answered).
    let served = serve(|_| {});
    served.aws.put(&format!("/superci/{PLANE}/LAUNCHERS"), r#"[{"cloud":"modal","url":"https://acme--superci-runners.modal.run"}]"#);
    served.aws.replaying(include_str!("../fixtures/modal/sign-in/token-flow.jsonl"), &[]);
    let home = signed_in(&served);
    let (mut child, link) = dashboard(&home);
    b.goto(&format!("{link}&p=planes"));
    b.wait(&reads("Optional: its runners work without it"));
    // (To Modal's page, in its own tab.)
    let (got, _) = post(&mut b, "/modal/signin", &[("next", "?p=planes")]);
    assert!(got < 400);
    b.goto(&format!("{link}&p=planes"));
    b.wait(&reads("Approve it in Modal's tab, then close the tab Waiting"));
    for p in ["overview", "add-runners", "runners"] { b.goto(&format!("{link}&p={p}")); b.wait(&format!("!document.querySelector('main').innerText.includes('Reading your control plane') && {}", page_shows("Modal"))); }
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_modal_sign_in_started_before_any_other_waits_on_every_page_and_all_control_planes_are_listed() {
    let Some(mut b) = Browser::open(1300, 900) else { eprintln!("no Chrome here: skipped"); return };
    // Signed in nowhere: Modal's sign-in started from the first page, each page then waits for its approval.
    let account = superci_testkit::Aws::serve();
    account.replaying(include_str!("../fixtures/modal/sign-in/token-flow.jsonl"), &[]);
    let home = common::temp("modal-first");
    std::fs::create_dir_all(home.join("workspaces/default")).unwrap();
    std::fs::write(home.join("aws-at"), &account.url).unwrap();
    let (mut child, link) = dashboard(&home);
    b.goto(&link);
    b.wait("document.querySelector('main') !== null");
    let (got, _) = post(&mut b, "/modal/signin", &[]);
    assert!(got < 400);
    for p in ["overview", "runners", "settings"] {
        b.goto(&format!("{link}&p={p}"));
        b.wait("document.querySelector('main') !== null && document.querySelector('main').innerText.length > 0");
    }
    // Approved in Modal (as its token flow answered, after a while): kept, wherever the page was.
    let file = home.join("workspaces/default/sign-ins.json");
    let end = std::time::Instant::now() + std::time::Duration::from_secs(90);
    while !std::fs::read_to_string(&file).unwrap_or_default().contains("token_id") {
        assert!(std::time::Instant::now() < end, "not approved: {}", b.text("body"));
        b.goto(&format!("{link}&p=overview"));
        b.wait("document.querySelector('main') !== null");
    }
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
    // Every control plane, listed (the one in use marked).
    let served = serve(|_| {});
    let home = signed_in(&served);
    let (mut child, link) = dashboard(&home);
    b.goto(&format!("{link}&p=all"));
    b.wait("document.querySelector('main') !== null && document.querySelector('main').innerText.length > 0");
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_workspace_with_no_control_plane_yet_is_asked_which_ci_from_every_page_and_control_plane_lists_the_one_found() {
    let Some(mut b) = Browser::open(1300, 900) else { eprintln!("no Chrome here: skipped"); return };
    let served = serve(|_| {});
    let home = signed_in(&served);
    let file = home.join("workspaces/default/sign-ins.json");
    let mut kept: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    for f in ["plane", "planes_more", "key"] { kept.as_object_mut().unwrap().remove(f); }
    std::fs::write(&file, kept.to_string()).unwrap();
    let (mut child, link) = dashboard(&home);
    for (p, said) in [("runners", "Which CI?"), ("jobs", "Which CI?"), ("planes", "Or set up a new one")] {
        b.goto(&format!("{link}&p={p}"));
        b.wait(&format!("document.querySelector('main') && document.body.innerText.includes({said:?})"));
    }
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn what_each_kind_of_aws_account_allows_is_said_by_the_cli_as_aws_answered_and_on_its_page() {
    let Some(mut b) = Browser::open(1300, 900) else { eprintln!("no Chrome here: skipped"); return };
    for look in [include_str!("../fixtures/aws/classic/look.jsonl"), include_str!("../fixtures/aws/project-activated/look.jsonl"), include_str!("../fixtures/aws/project-paid/look.jsonl")] {
        let served = serve(|_| {});
        served.aws.replaying(look, &[]);
        let home = signed_in(&served);
        for args in [&["accounts", "retrieve", "aws"][..], &["accounts", "retrieve", "aws", "--json"], &["images", "list"], &["status"]] {
            let o = common::command(&home).args(args).output().unwrap();
            let said = String::from_utf8_lossy(&o.stdout).to_string() + &String::from_utf8_lossy(&o.stderr);
            assert!(o.status.code().is_some_and(|c| c <= 3) && !said.contains("panicked"), "{args:?}: {said}");
        }
        let (mut child, link) = dashboard(&home);
        b.goto(&format!("{link}&p=planes"));
        b.wait(&format!("{} && !document.querySelector('main').innerText.includes('Reading your control plane')", shows("AWS account")));
        let _ = child.kill();
        let _ = std::fs::remove_dir_all(&home);
    }
}

#[test]
fn the_debug_commands_answer_as_the_clouds_answered_and_the_pages_stay() {
    let Some(mut b) = Browser::open(1300, 900) else { eprintln!("no Chrome here: skipped"); return };
    let served = serve(|_| {});
    served.aws.replaying(include_str!("../fixtures/aws/launch/spot.jsonl"), &[]);
    let started: String = include_str!("../fixtures/aws/launch/on-demand.jsonl").lines().filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .find(|v| v["asked"].as_str().unwrap_or_default().contains("Action=RunInstances") && v["status"] == 200).map(|v| v["answer"].as_str().unwrap_or_default().to_string()).unwrap();
    served.aws.fail("RunInstances", 200, &started, None);
    let home = signed_in(&served);
    let file = home.join("workspaces/default/sign-ins.json");
    let mut kept: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    kept["cloudflare"] = serde_json::json!({ "access": "cloudflare-access-for-tests", "expires_at_ms": 4102444800000u64, "refresh": "cloudflare-refresh-for-tests" });
    std::fs::write(&file, kept.to_string()).unwrap();
    served.aws.fail("/client/v4/accounts", 200, r#"{"result":[{"id":"1111111111111111111111111111111a","name":"Acme"}],"success":true,"errors":[],"messages":[]}"#, None);
    let body = home.join("body.json");
    std::fs::write(&body, r#"{"query":"query{viewer{accounts{g:containersUsageAdaptiveGroups(limit:1){sum{cpuTimeSec}}}}}","variables":{}}"#).unwrap();
    let body = body.to_string_lossy().to_string();
    let bucket = superci_core::cache::bucket(PLANE, "us-east-1");
    let s3 = format!("https://{bucket}.s3.us-east-1.amazonaws.com/x");
    let table: Vec<Vec<&str>> = vec![
        vec!["debug", "networks"], vec!["debug", "networks", "us-east-1"], vec!["debug", "budget"], vec!["debug", "budget", "set", "100", "ops@example.com", "stop"], vec!["debug", "budget", "lift"], vec!["debug", "budget", "remove"],
        vec!["debug", "launch", "us-east-1", "spot", "c8a.large"], vec!["debug", "launch", "us-east-1", "on-demand"], vec!["debug", "presign", "GET", "us-east-1", &bucket, "trees/x", "2"], vec!["debug", "presign", "GET", "us-east-1", &bucket, "x", "many"],
        vec!["debug", "rest", "us-east-1", "s3", "GET", &s3], vec!["debug", "cloudflare", "GET", "/accounts"], vec!["debug", "cloudflare", "POST", "/graphql", &body],
        vec!["debug", "ec2", "us-east-1", "DescribeRegions"], vec!["debug", "iam", "GetRole", "RoleName=superci-plane-testplane0001"], vec!["debug", "json", "us-east-1", "ssm", "AmazonSSM.GetParameter", r#"{"Name":"/superci/x"}"#],
        vec!["debug", "abandon", "us-east-1", "testplane0009"],
    ];
    for args in &table {
        let o = common::command(&home).args(args).output().unwrap();
        let said = String::from_utf8_lossy(&o.stdout).to_string() + &String::from_utf8_lossy(&o.stderr);
        assert!(o.status.code().is_some_and(|c| c <= 3) && !said.contains("panicked"), "{args:?}: {said}");
    }
    let (mut child, link) = dashboard(&home);
    b.goto(&link);
    b.wait("document.querySelector('main') !== null");
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_change_needing_a_cloud_noted_as_ended_or_given_for_the_run_says_which() {
    let Some(mut b) = Browser::open(1300, 900) else { eprintln!("no Chrome here: skipped"); return };
    // AWS's, Cloudflare's and Modal's sign-ins noted as ended (AWS's kept beside, as it was signed out): a change
    // needing each says it ended.
    let served = serve(|_| {});
    served.aws.put(&format!("/superci/{PLANE}/LAUNCHERS"), r#"[{"cloud":"cloudflare","url":"https://superci-runners.acme.workers.dev"},{"cloud":"modal","url":"https://acme--superci-runners.modal.run"}]"#);
    let home = signed_in(&served);
    let file = home.join("workspaces/default/sign-ins.json");
    let (mut child, link) = dashboard(&home);
    b.goto(&link);
    b.wait("document.querySelector('main') !== null");
    let _ = child.kill();
    let mut kept: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    for k in ["aws", "aws_more"] { kept.as_object_mut().unwrap().remove(k); }
    for k in ["aws_ended", "cloudflare_ended", "modal_ended"] { kept[k] = true.into(); }
    std::fs::write(&file, kept.to_string()).unwrap();
    let (mut child, link) = dashboard(&home);
    b.goto(&link);
    b.wait("document.querySelector('main') !== null");
    b.goto(&format!("{}/job/view?id=0", link.split("/?").next().unwrap()));
    for (path, fields) in [("/aws/image", vec![("region", "us-east-1")]), ("/plane/modal", vec![]), ("/runners/remove", vec![("cloud", "cloudflare")]), ("/runners/remove", vec![("cloud", "modal")]), ("/plane/aws", vec![("region", "us-east-1")])] {
        let (got, text) = post(&mut b, path, &fields);
        assert!(got >= 400 && (text.contains("ended") || text.contains("expired") || text.contains("no longer") || text.contains("Sign in")), "{path}: {got} {text}");
    }
    let _ = child.kill();
    // Given for this run by name (SUPERCI_CLOUDFLARE_TOKEN, SUPERCI_MODAL_TOKEN_ID): what they refuse said as theirs.
    let (mut child, link) = common::dashboard_with(&home, &[("SUPERCI_CLOUDFLARE_TOKEN", "cf-token-given"), ("SUPERCI_MODAL_TOKEN_ID", "ak-given"), ("SUPERCI_MODAL_TOKEN_SECRET", "as-given")]);
    b.goto(&link);
    b.wait("document.querySelector('main') !== null");
    b.goto(&format!("{}/job/view?id=0", link.split("/?").next().unwrap()));
    for (path, fields) in [("/runners/remove", vec![("cloud", "cloudflare")]), ("/runners/remove", vec![("cloud", "modal")]), ("/aws/image", vec![("region", "us-east-1")])] {
        let (got, _) = post(&mut b, path, &fields);
        assert!(got < 600, "{path}");
    }
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn removing_aws_or_modal_while_signed_in_says_each_part_its_cloud_would_not_delete() {
    let Some(mut b) = Browser::open(1300, 900) else { eprintln!("no Chrome here: skipped"); return };
    let served = serve(|c| {
        (c.aws_own, c.aws_own_creds) = (Some(("111111111111".into(), "us-east-1".into())), Some(superci_core::aws::Credentials { access_key_id: "ASIAX".into(), secret_access_key: "secret".into(), session_token: Some("tok".into()), expires_at_ms: u64::MAX }));
    });
    served.aws.put(&format!("/superci/{PLANE}/ROUTING"), r#"{"order":[{"cloud":"aws"},{"cloud":"aws-on-demand"},{"cloud":"modal"}],"account_usd":{"aws":100.0},"aws_budget":{"stop":false}}"#);
    served.aws.put(&format!("/superci/{PLANE}/LAUNCHERS"), r#"[{"cloud":"modal","url":"https://acme--superci-runners.modal.run"}]"#);
    let home = signed_in(&served);
    let file = home.join("workspaces/default/sign-ins.json");
    let mut kept: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    kept["modal"] = serde_json::json!({ "token_id": "ak-modal-for-tests", "token_secret": "as-modal-for-tests", "workspace": "acmeorg" });
    std::fs::write(&file, kept.to_string()).unwrap();
    // AWS refusing each part: its role's policy, its budget, its network.
    served.aws.fail("DeleteRolePolicy", 500, &superci_testkit::ec2_error("ServiceFailure", "Service failure"), None);
    served.aws.fail("budgets", 500, &superci_testkit::json_error("InternalErrorException", "internal error"), None);
    served.aws.fail("DeleteVpc", 400, &superci_testkit::ec2_error("DependencyViolation", "has dependencies"), None);
    served.aws.fail("api.modal.com", 503, "unavailable", None);
    let (mut child, link) = dashboard(&home);
    b.goto(&link);
    b.wait("document.querySelector('main') !== null");
    b.goto(&format!("{}/job/view?id=0", link.split("/?").next().unwrap()));
    for cloud in ["aws", "modal"] {
        let (got, text) = post(&mut b, "/runners/remove", &[("cloud", cloud)]);
        assert!(got < 500, "{cloud}: {got} {text}");
    }
    b.goto(&format!("{link}&p=runners"));
    b.wait("document.querySelector('main') !== null");
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_control_plane_updated_from_the_cli_is_followed_to_its_end_and_its_page_says_its_version() {
    let Some(mut b) = Browser::open(1300, 900) else { eprintln!("no Chrome here: skipped"); return };
    let served = serve(|_| {});
    let home = signed_in(&served);
    served.aws.replaying(include_str!("../fixtures/aws/planes/update.jsonl"), &[("testplane0002", PLANE), ("http://new-plane.test", &served.url)]);
    served.aws.put(&format!("/superci/{PLANE}/DASHBOARD_KEY_4102444800_TEST"), &served.key);
    *served.aws.old_version.lock().unwrap() = Some("0.19.12".into());
    let o = common::command(&home).args(["planes", "update"]).output().unwrap();
    let said = String::from_utf8_lossy(&o.stdout).to_string() + &String::from_utf8_lossy(&o.stderr);
    assert!(o.status.code().is_some_and(|c| c <= 3) && !said.contains("panicked"), "{said}");
    let (mut child, link) = dashboard(&home);
    b.goto(&format!("{link}&p=planes"));
    b.wait("document.querySelector('main') !== null");
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_first_control_plane_made_from_the_cli_is_followed_to_its_end_and_shown_on_the_dashboard() {
    let Some(mut b) = Browser::open(1300, 900) else { eprintln!("no Chrome here: skipped"); return };
    let account = superci_testkit::Aws::serve();
    let new = superci_testkit::serve_as("deployplane1", |_| {});
    let home = common::temp("cli-first-home");
    let ws = home.join("workspaces/default");
    std::fs::create_dir_all(&ws).unwrap();
    std::fs::write(ws.join("sign-ins.json"), serde_json::json!({ "aws": { "region": "us-east-1", "account_id": "111111111111", "who": "arn:aws:sts::111111111111:assumed-role/Admin/someone",
        "creds": { "access_key_id": "ASIAEXAMPLE", "secret_access_key": "example", "session_token": "example", "expires_at_ms": 4102444800000u64 }, "refresh_token": "", "dpop": { "private_b64": "", "kid": "" } } }).to_string()).unwrap();
    std::fs::write(home.join("aws-at"), &account.url).unwrap();
    account.replaying(include_str!("../fixtures/aws/classic/look.jsonl"), &[]);
    account.replaying(include_str!("../fixtures/aws/planes/deploy.jsonl"), &[("testplane0002", "deployplane1"), ("http://new-plane.test", &new.url)]);
    for args in [&["planes", "create", "aws", "--region=mars-1"][..], &["planes", "create", "aws", "--region=us-east-1", "--dry-run"], &["planes", "create", "aws", "--region=us-east-1"]] {
        let o = common::command(&home).args(args).env("SUPERCI_NEW_PLANE_ID", "deployplane1").output().unwrap();
        let said = String::from_utf8_lossy(&o.stdout).to_string() + &String::from_utf8_lossy(&o.stderr);
        assert!(o.status.code().is_some_and(|c| c <= 3) && !said.contains("panicked"), "{args:?}: {said}");
    }
    let (mut child, link) = dashboard(&home);
    b.goto(&format!("{link}&p=planes"));
    b.wait("document.querySelector('main') !== null");
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn jobs_the_sweep_ends_over_hours_and_days_are_said_on_their_pages_as_it_ended_them() {
    let Some(mut b) = Browser::open(1300, 900) else { eprintln!("no Chrome here: skipped"); return };
    // One whose machine never takes it, one that runs and never ends, one that ended; then hours and days pass.
    let served = serve(|_| {});
    served.connect_github();
    let (_, q) = served.github.queue("never-taken", "superci", served.now_ms()); served.webhook(q);
    let (b2, q) = served.github.queue("never-ends", "superci", served.now_ms()); served.webhook(q); served.webhook(served.github.start(b2));
    let (c, q) = served.github.queue("ended", "superci", served.now_ms()); served.webhook(q); served.webhook(served.github.start(c)); served.webhook(served.github.finish(c, "success", "ok", served.now_ms()));
    for ms in [11 * 60_000, 6 * 3_600_000, 25 * 3_600_000, 3 * 86_400_000] { served.later(ms); served.upkeep(); }
    let home = home(&served);
    let (mut child, link) = dashboard(&home);
    b.goto(&link);
    b.wait(&shows("Usage"));
    b.click("nav a", "Runs");
    b.click(".tabs a", "Jobs");
    // Past its longest time; never started, three times: each said; the one that ended as it ended.
    b.wait(&format!("{} && {} && {}", reads("ran past its longest time (6 hours)"), reads("did not start a machine for it, three times"), reads("ended")));
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}


#[test]
fn jobs_waiting_for_room_start_once_there_is_some_and_one_waiting_a_day_is_given_up() {
    let Some(mut b) = Browser::open(1300, 900) else { eprintln!("no Chrome here: skipped"); return };
    // Cloudflare runs one job at a time: a GitHub job running, another and a GitLab job waiting.
    let served = serve(|_| {});
    served.connect_github();
    served.connect_gitlab();
    served.aws.put(&format!("/superci/{PLANE}/ROUTING"), r#"{"default":null,"rules":[],"order":[{"cloud":"cloudflare","max_jobs":1}]}"#);
    let (a, q) = served.github.queue("first", "superci", served.now_ms()); served.webhook(q); served.webhook(served.github.start(a));
    let (_, q) = served.github.queue("waits", "superci", served.now_ms()); served.webhook(q);
    let (_, p) = served.gitlab.pending("gl-waits", &["superci"], served.now_ms()); served.gitlab_webhook(p);
    let home = home(&served);
    let (mut child, link) = dashboard(&home);
    b.goto(&format!("{link}&p=jobs"));
    b.wait(&reads("Cloudflare runs 1 at once — Waiting"));
    let _ = child.kill();
    // Hours on: the first swept, the next started in its place; a day on, the GitLab one given up.
    for ms in [30 * 60_000, 7 * 3_600_000, 25 * 3_600_000] { served.later(ms); served.upkeep(); }
    let (mut child, link) = dashboard(&home);
    b.goto(&format!("{link}&p=jobs"));
    b.wait(&format!("{} && {}", reads("Waited a day for room — Failed"), reads("ran past its longest time")));
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn removing_a_cloudflare_or_modal_launcher_needs_its_sign_in_or_is_from_superci_only_saying_what_is_left_there() {
    let Some(mut b) = Browser::open(1300, 900) else { eprintln!("no Chrome here: skipped"); return };
    // Launchers in Cloudflare and Modal, neither signed in.
    let served = serve(|_| {});
    let launchers = r#"[{"cloud":"modal","url":"https://acme--superci-runners.modal.run"},{"cloud":"cloudflare","url":"https://superci-runners.acme.workers.dev"}]"#;
    served.aws.put(&format!("/superci/{PLANE}/LAUNCHERS"), launchers);
    served.aws.put(&format!("/superci/{PLANE}/CONTAINERS"), "off");
    let home = signed_in(&served);
    let (mut child, link) = dashboard(&home);
    b.goto(&format!("{link}&p=runners"));
    b.wait(&shows("Runner providers"));
    for (cloud, said) in [("modal", "Sign in with Modal first"), ("cloudflare", "Sign in with Cloudflare first")] {
        let (got, text) = post(&mut b, "/runners/remove", &[("cloud", cloud)]);
        assert!(got == 403 && text.contains(said), "{cloud}: {got} {text}");
    }
    // From SuperCI only: forgotten, and each one's launcher said as left there. (Asked from a page that does not
    // look again by itself: what it says once done is left for the next page.)
    for (cloud, left) in [("cloudflare", format!("Left: the Cloudflare launcher superci-runners-{PLANE}")), ("modal", format!("Left: the Modal app superci-runners-{PLANE} in your workspace"))] {
        b.goto(&format!("{}/job/view?id=0", link.split("/?").next().unwrap()));
        let (got, text) = post(&mut b, "/runners/remove", &[("cloud", cloud), ("only", "on")]);
        assert!(got < 400, "{cloud}: {got} {text}");
        // (Under load the removal may still be going on when the page is first read: read again until it says.)
        let mut said = false;
        for _ in 0..30 {
            b.goto(&format!("{link}&p=runners"));
            b.wait(&shows("Add provider"));
            // (Its note is shown once: read on this page until it has loaded, not on the next.)
            for _ in 0..20 {
                if b.text("body").contains(&left) { said = true; break }
                if !b.eval("!!document.querySelector('[data-loading]')").as_bool().unwrap_or(false) && b.text("main").contains("Clouds") { break }
                std::thread::sleep(std::time::Duration::from_millis(250));
            }
            if said || b.text("body").contains(&left) { said = true; break }
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
        // (Its note is shown once, and under load another read of the page may take it first: what was removed is
        // checked in the settings either way.)
        let launchers = served.aws.settings(PLANE).into_iter().find(|(k, _)| k == "LAUNCHERS").map(|(_, v)| v).unwrap_or_default();
        assert!(said || !launchers.contains(&format!("\"{cloud}\"")), "{cloud}: {launchers} {}", b.text("body"));
    }
    assert!(served.aws.settings(PLANE).iter().any(|(k, v)| k == "LAUNCHERS" && v == "[]"));
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_cloud_that_answers_nothing_is_a_note_on_the_pages_which_keep_working() {
    let Some(mut b) = Browser::open(1300, 900) else { eprintln!("no Chrome here: skipped"); return };
    // A control plane in AWS with a Modal launcher; Modal signed in, and answering nothing at all.
    let served = serve(|_| {});
    served.aws.put(&format!("/superci/{PLANE}/LAUNCHERS"), r#"[{"cloud":"modal","url":"https://acme--superci-runners.modal.run"}]"#);
    let home = signed_in(&served);
    let file = home.join("workspaces/default/sign-ins.json");
    let mut kept: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    kept["modal"] = serde_json::json!({ "token_id": "ak-modal-for-tests", "token_secret": "as-modal-for-tests", "workspace": "acmeorg" });
    std::fs::write(&file, kept.to_string()).unwrap();
    let (mut child, link) = dashboard(&home);
    for (p, said) in [("overview", "Usage"), ("runners", "Runner providers"), ("planes", "AWS · 111111111111")] {
        b.goto(&format!("{link}&p={p}"));
        b.wait(&format!("{} && {}", shows(said), page_shows("Modal did not answer (")));
    }
    // Its launcher removed all the same: forgotten, and its app said as left in Modal.
    b.goto(&format!("{}/job/view?id=0", link.split("/?").next().unwrap()));
    let (got, text) = post(&mut b, "/runners/remove", &[("cloud", "modal")]);
    assert!(got < 400, "{got} {text}");
    assert!(served.aws.settings(PLANE).iter().any(|(k, v)| k == "LAUNCHERS" && v == "[]"));
    b.goto(&format!("{link}&p=runners"));
    b.wait(&page_shows(&format!("Left: its Modal app superci-runners-{PLANE}")));
    let _ = child.kill();
    // From the CLI: what it did, and the note.
    let o = common::command(&home).args(["planes", "list"]).output().unwrap();
    let said = String::from_utf8_lossy(&o.stdout).to_string() + &String::from_utf8_lossy(&o.stderr);
    assert!(o.status.success() && said.contains("Modal did not answer ("), "{said}");
    let _ = std::fs::remove_dir_all(&home);
}


/// What the AWS account card says of each kind of account, as AWS words its answers (an organization's policy, the
/// role's permissions, an organization the account is in or manages, AWS not answering, no runner image there).
#[test]
fn the_aws_account_card_says_what_each_kind_of_account_allows_and_whose_rule_a_refusal_is() {
    let Some(mut b) = Browser::open(1300, 1400) else { eprintln!("no Chrome here: skipped"); return };
    let scp = |action: &str| superci_testkit::ec2_error("UnauthorizedOperation", &format!("You are not authorized to perform this operation. User: arn:aws:sts::111111111111:assumed-role/Admin/someone is not authorized to perform: {action} with an explicit deny in a service control policy: arn:aws:organizations::222222222222:policy/o-example001/service_control_policy/p-example1"));
    let role = |action: &str| superci_testkit::ec2_error("UnauthorizedOperation", &format!("You are not authorized to perform this operation. User: arn:aws:sts::111111111111:assumed-role/ReadOnly/someone is not authorized to perform: {action} because no identity-based policy allows the {action} action"));
    let org = |master: &str| serde_json::json!({ "Organization": { "Id": "o-example001", "Arn": "arn:aws:organizations::222222222222:organization/o-example001", "MasterAccountId": master, "MasterAccountArn": format!("arn:aws:organizations::{master}:account/o-example001/{master}") } }).to_string();
    let others: Vec<String> = superci_core::page::REGIONS.iter().filter(|r| **r != "us-east-1").map(|r| format!("DescribeVpcs|ec2.{r}.")).collect();
    type Fails = Vec<(String, u16, String)>;
    let cases: Vec<(&str, Fails, &[&str])> = vec![
        ("kept to one region by its organization", others.iter().map(|o| (o.clone(), 403, scp("ec2:DescribeVpcs"))).collect(),
            &["Some regions", "SuperCI works there in N. Virginia. Its AWS organization keeps it out of the other regions.", "The other 11 are refused. By its AWS organization (policy p-example1).", "How to allow more", "Runner image\nIn N. Virginia"]),
        ("spot refused by its organization", vec![("RunInstances|MarketType".into(), 403, scp("ec2:RunInstances"))],
            &["Restricted", "Spot machines\nNot allowed\nBy its AWS organization (policy p-example1). Jobs run on on-demand machines, at about twice the price."]),
        ("on-demand refused by the role", vec![("RunInstances|MarketType".into(), 412, superci_testkit::ec2_error("DryRunOperation", "Request would have succeeded, but DryRun flag is set.")), ("RunInstances".into(), 403, role("ec2:RunInstances"))],
            &["Restricted", "On-demand machines\nNot allowed\nBy the permissions of the role or user signed in as."]),
        ("a member of an organization", vec![("AWSOrganizationsV20161128.DescribeOrganization".into(), 200, org("222222222222"))],
            &["Nothing refused", "Organization\no-example001", "Managed from account 222222222222"]),
        ("its organization's management account", vec![("AWSOrganizationsV20161128.DescribeOrganization".into(), 200, org("111111111111"))],
            &["The organization's management account. A CI is better kept in an account of its own.", "Another AWS account"]),
        ("AWS not answering", vec![("DescribeVpcs".into(), 503, superci_testkit::ec2_error("Unavailable", "The service is unavailable. Please try again shortly."))],
            &["Not known", "AWS did not answer when asked what this account allows"]),
        ("the runner image in none of its regions", vec![("DescribeImages".into(), 200, "<DescribeImagesResponse xmlns=\"http://ec2.amazonaws.com/doc/2016-11-15/\"><requestId>00000000-0000-4000-8000-000000000001</requestId><imagesSet/></DescribeImagesResponse>".into())],
            &["Runner image\nNot in", "No job can start there until it is copied in"]),
    ];
    for (what, fails, says) in cases {
        let served = serve(|c| {
            (c.aws_own, c.aws_own_creds) = (Some(("111111111111".into(), "us-east-1".into())), Some(superci_core::aws::Credentials { access_key_id: "ASIAX".into(), secret_access_key: "secret".into(), session_token: Some("tok".into()), expires_at_ms: u64::MAX }));
        });
        for (action, status, answer) in fails { served.aws.fail(&action, status, &answer, None) }
        let home = signed_in(&served);
        let (mut child, link) = dashboard(&home);
        b.goto(&format!("{link}&p=add-runners"));
        b.wait("document.getElementById('aws-account') && !document.body.innerText.includes('Asking AWS')");
        let card = b.eval("document.getElementById('aws-account').nextElementSibling.innerText").as_str().unwrap_or_default().to_string();
        for said in says { assert!(card.contains(said), "{what}: no {said:?} in\n{card}") }
        let _ = child.kill();
        let _ = std::fs::remove_dir_all(&home);
    }
}

#[test]
fn a_page_whose_dashboard_stopped_keeps_what_it_showed_marked_stale_and_says_when_a_new_one_runs() {
    let Some(mut b) = Browser::open(1300, 900) else { eprintln!("no Chrome here: skipped"); return };
    let served = serve(|_| {});
    let home = signed_in(&served);
    let (child, link) = dashboard(&home);
    b.goto(&format!("{link}&p=runners"));
    b.wait(&format!("{} && !document.body.classList.contains('busy')", shows("Clouds")));
    // Stopped: the page keeps what it showed, dimmed, with a bar saying so (its next look finds nothing).
    drop(child);
    b.eval("document.querySelector('.side .nav a[href*=\"p=overview\"]').click()");
    b.wait("document.body.classList.contains('stale') && document.getElementById('stale-bar')");
    let bar = b.eval("document.getElementById('stale-bar').innerText").as_str().unwrap_or_default().to_string();
    // (Its address and the sidebar stay on the page it shows, not the one that could not open.)
    assert!(b.eval("location.search.includes('p=runners') && document.querySelector('.side .nav a[aria-current=true]').href.includes('p=runners')").as_bool() == Some(true));
    if let Some(at) = std::env::var_os("SUPERCI_SHOTS") { std::thread::sleep(std::time::Duration::from_millis(500)); b.screenshot(&std::path::Path::new(&at).join("stale-stopped.png")) }
    assert!(bar.contains("The dashboard stopped.") && bar.contains("Run superci dashboard again."), "{bar}");
    assert!(b.eval("document.querySelector('main').innerText.includes('Clouds')").as_bool() == Some(true), "what it showed is kept");
    assert_eq!(b.eval("getComputedStyle(document.querySelector('main > .cards, main > section, main > div:not(.stale-bar)')).pointerEvents").as_str(), Some("none"), "nothing on it can be pressed");
    // A new one on the same address: it does not take this tab's old key, which is said; nothing else changes.
    let port = link.split(':').nth(2).and_then(|p| p.split('/').next()).unwrap().to_string();
    let (mut child, _) = common::dashboard_with(&home, &[("SUPERCI_PORT", &port)]);
    b.wait("document.getElementById('stale-bar') && document.getElementById('stale-bar').innerText.includes('A new dashboard is running.')");
    if let Some(at) = std::env::var_os("SUPERCI_SHOTS") { b.screenshot(&std::path::Path::new(&at).join("stale-new.png")) }
    assert!(b.eval("document.querySelector('main').innerText.includes('Clouds')").as_bool() == Some(true));
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

/// Jobs GitHub sends to a control plane whose AWS refuses their machines, for each kind of job and each pair of
/// answers (its spot machine's, its on-demand one's), as AWS documents its errors: each job's page says what became
/// of it, and the Jobs page lists them all.
#[test]
fn jobs_aws_refuses_say_on_their_pages_what_became_of_them_for_every_pair_of_refusals() {
    let Some(mut b) = Browser::open(1300, 900) else { eprintln!("no Chrome here: skipped"); return };
    let served = serve(|c| {
        (c.aws_own, c.aws_own_creds) = (Some(("111111111111".into(), "us-east-1".into())), Some(superci_core::aws::Credentials { access_key_id: "ASIAX".into(), secret_access_key: "secret".into(), session_token: Some("tok".into()), expires_at_ms: u64::MAX }));
    });
    served.connect_github();
    // AWS only (its containers off in the order): a refusal is not passed on to Cloudflare.
    served.aws.put(&format!("/superci/{PLANE}/ROUTING"), r#"{"default":null,"rules":[],"order":[{"cloud":"aws"},{"cloud":"aws-on-demand"},{"cloud":"cloudflare","off":true}]}"#);
    // What AWS answers that is not refused: as it answered Test (its images, network, a machine started).
    served.aws.replaying(include_str!("../fixtures/aws/launch/spot.jsonl"), &[]);
    let refusal = |code: &str| match code {
        "500" => (500, superci_testkit::ec2_error("InternalError", "An internal error has occurred")),
        "UnauthorizedOperation" => (403, superci_testkit::ec2_error(code, "You are not authorized to perform this operation.")),
        c => (400, superci_testkit::ec2_error(c, "no")),
    };
    // (A machine started last: what AWS refused is tried while none of this kind's are running.)
    let answers = ["InsufficientInstanceCapacity", "VcpuLimitExceeded", "MaxSpotInstanceCountExceeded", "Unsupported", "UnauthorizedOperation", "500", ""];
    // A machine started, as AWS answered Test's request for one.
    let started: String = include_str!("../fixtures/aws/launch/on-demand.jsonl").lines().filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .find(|v| v["asked"].as_str().unwrap_or_default().contains("Action=RunInstances") && v["status"] == 200).map(|v| v["answer"].as_str().unwrap_or_default().to_string()).unwrap();
    let home = signed_in(&served);
    // (Each page read anew: the jobs come between them.)
    let (mut child, link) = common::dashboard_with(&home, &[("SUPERCI_VIEWS_S", "0")]);
    let (mut said, mut later): (std::collections::BTreeMap<String, usize>, std::collections::BTreeMap<String, usize>) = Default::default();
    // Each kind of job, and three setups beside: on-demand turned off, Cloudflare after AWS (a refusal passes the job
    // on), and a second region (the first refusing, the second as answered).
    let only_aws = r#"{"default":null,"rules":[],"order":[{"cloud":"aws"},{"cloud":"aws-on-demand"},{"cloud":"cloudflare","off":true}]}"#;
    let kinds: Vec<(&str, &str, Option<&str>)> = vec![("superci", only_aws, None), ("superci/spot=false", only_aws, None), ("superci/gpu=any", only_aws, None), ("superci/gpu=any/spot=false", only_aws, None),
        ("superci/cpu=2/arch=arm64", only_aws, None),
        ("superci", r#"{"default":null,"rules":[],"order":[{"cloud":"aws"},{"cloud":"aws-on-demand","off":true},{"cloud":"cloudflare","off":true}]}"#, None),
        ("superci", r#"{"default":null,"rules":[],"order":[{"cloud":"aws"},{"cloud":"aws-on-demand"},{"cloud":"cloudflare"}]}"#, None),
        ("superci", only_aws, Some(r#"["us-east-1","us-east-2"]"#))];
    for (label, routing, regions) in kinds {
        served.aws.put(&format!("/superci/{PLANE}/ROUTING"), routing);
        if let Some(r) = regions { served.aws.put(&format!("/superci/{PLANE}/AWS_REGIONS"), r) } else { served.aws.params.lock().unwrap().remove(&format!("/superci/{PLANE}/AWS_REGIONS")); }
        let mut jobs = vec![];
        for spot in answers {
            for od in answers {
                if label.contains("spot=false") && !spot.is_empty() { continue }
                served.aws.fail_none();
                // (Each case as if first: the pools AWS had no machine in a moment ago are not remembered.)
                served.store.0.lock().unwrap().retain(|k, _| !k.starts_with("aws:pool:") && !k.starts_with("aws:unoffered:") && !k.starts_with("aws:no-fleet") && !k.starts_with("spot:"));
                // (Pools asked one by one, as when a role may not ask for many at once: each refusal is AWS's word
                // for that pool.)
                served.store.0.lock().unwrap().insert("aws:no-fleet".into(), served.now_ms().to_string());
                let (s, a) = if spot.is_empty() { (200, started.clone()) } else { refusal(spot) };
                served.aws.fail("RunInstances|MarketType", s, &a, None);
                let (s, a) = if od.is_empty() { (200, started.clone()) } else { refusal(od) };
                served.aws.fail("RunInstances", s, &a, None);
                let (id, q) = served.github.queue(&format!("{label} {spot} {od}"), label, served.now_ms());
                assert!(served.webhook(q) < 300);
                jobs.push((id, spot, od));
            }
        }
        served.aws.fail_none();
        // Each job's state, as its page says it, and why when there is a why: at once, and once the sweeps have
        // asked again (a sweep a minute, AWS answering as it answered Test).
        let mut look = |said: &mut std::collections::BTreeMap<String, usize>, every: usize| for (id, spot, od) in jobs.iter().step_by(every) {
            let job = served.store.under(&format!("job:{id}"))[0].1.clone();
            b.goto(&format!("{link}&p=job&id={id}"));
            b.wait(&format!("{} && !document.querySelector('main').innerText.includes('Reading your control plane')", shows("Status")));
            let page = b.eval("document.querySelector('main').innerText").as_str().unwrap_or_default().replace('\n', " ");
            let state = job["state"].as_str().unwrap_or_default();
            let why = job["error"].as_str().unwrap_or_default().trim_start_matches("waiting: ").split(" (").next().unwrap_or_default().chars().take(40).collect::<String>();
            assert!(why.is_empty() || page.to_lowercase().contains(&why.to_lowercase()), "{label} spot {spot} on-demand {od}: {state} {why:?} not on its page:\n{page}");
            *said.entry(format!("{state}: {}", job["error"].as_str().unwrap_or_default().chars().take(140).collect::<String>())).or_default() += 1;
        };
        look(&mut said, 1);
        for _ in 0..3 { served.later(61_000); served.upkeep(); }
        // (Once asked again: every fifth, enough to see each kind of end.)
        look(&mut later, 5);
        // This kind's machines ended (their jobs done), and its waiting jobs given up, before the next kind.
        for (id, _, _) in &jobs {
            let job = served.store.under(&format!("job:{id}"))[0].1.clone();
            if job["machine_id"].is_string() { served.webhook(served.github.start(*id)); served.webhook(served.github.finish(*id, "success", "ok", served.now_ms())); }
            else { served.webhook(served.github.cancel(*id, served.now_ms())); }
        }
    }
    // Every kind of end the table has, among them.
    // And single refusals that steer a start elsewhere, on a spot machine with on-demand starting: its network or
    // image gone, AWS's limit on requests, spot refused by the account's organization, AWS's budget stopping it.
    let scp = superci_testkit::ec2_error("UnauthorizedOperation", "You are not authorized to perform this operation. User: arn:aws:sts::111111111111:assumed-role/superci-plane-testplane0001/superci is not authorized to perform: ec2:RunInstances on resource: arn:aws:ec2:us-east-1:111111111111:spot-instances-request/* with an explicit deny in a service control policy: arn:aws:organizations::222222222222:policy/o-example001/service_control_policy/p-example1");
    let budget = superci_testkit::ec2_error("UnauthorizedOperation", &format!("You are not authorized to perform this operation. User: arn:aws:sts::111111111111:assumed-role/superci-plane-testplane0001/superci is not authorized to perform: ec2:RunInstances with an explicit deny in an identity-based policy: arn:aws:iam::111111111111:policy/{}", superci_core::aws::stop_policy_name(PLANE)));
    let singles: Vec<(&str, u16, String)> = vec![("its subnet gone", 400, superci_testkit::ec2_error("InvalidSubnetID.NotFound", "The subnet ID 'subnet-00000000000000001' does not exist")),
        ("its security group gone", 400, superci_testkit::ec2_error("InvalidGroup.NotFound", "The security group 'sg-00000000000000001' does not exist")),
        ("its security group id malformed", 400, superci_testkit::ec2_error("InvalidSecurityGroupID.Malformed", "Invalid id: \"sg-x\"")),
        ("its image gone", 400, superci_testkit::ec2_error("InvalidAMIID.NotFound", "The image id '[ami-00000000000000001]' does not exist")),
        ("too many requests", 503, superci_testkit::ec2_error("RequestLimitExceeded", "Request limit exceeded.")),
        ("spot refused by the organization", 403, scp), ("stopped by AWS's budget", 403, budget)];
    let mut singled = vec![];
    for (what, status, answer) in &singles {
        served.aws.fail_none();
        served.store.0.lock().unwrap().retain(|k, _| !k.starts_with("aws:"));
        served.store.0.lock().unwrap().insert("aws:no-fleet".into(), served.now_ms().to_string());
        served.aws.fail("RunInstances|MarketType", *status, answer, None);
        served.aws.fail("RunInstances", 200, &started, None);
        let (id, q) = served.github.queue(what, "superci", served.now_ms());
        assert!(served.webhook(q) < 300);
        singled.push((id, *what));
    }
    served.aws.fail_none();
    for (id, what) in &singled {
        let job = served.store.under(&format!("job:{id}"))[0].1.clone();
        b.goto(&format!("{link}&p=job&id={id}"));
        b.wait(&format!("{} && !document.querySelector('main').innerText.includes('Reading your control plane')", shows("Status")));
        let page = b.eval("document.querySelector('main').innerText").as_str().unwrap_or_default().replace('\n', " ");
        let why = job["error"].as_str().unwrap_or_default().trim_start_matches("waiting: ").split(" (").next().unwrap_or_default().chars().take(40).collect::<String>();
        assert!(why.is_empty() || page.to_lowercase().contains(&why.to_lowercase()), "{what}: {why:?} not on its page:\n{page}");
        *said.entry(format!("{what}: {} {}", job["state"].as_str().unwrap_or_default(), job["error"].as_str().unwrap_or_default().chars().take(100).collect::<String>())).or_default() += 1;
    }
    // Every kind of end, among them: at once, and once asked again.
    for kind in ["launched", "AWS lets this account run no GPU machines yet", "asking AWS's other spot pools", "Your AWS account's limits do not let this machine start", "no provider could start its machine",
        "AWS had no room for it", "InternalError", "UnauthorizedOperation"] {
        assert!(said.keys().any(|k| k.contains(kind)), "no job ended as {kind:?}: {said:#?}");
    }
    for kind in ["launched", "failed: "] { assert!(later.keys().any(|k| k.contains(kind)), "none {kind:?} later: {later:#?}") }
    // Each of those got its machine on-demand, its spot refusal noted with it.
    for (id, what) in &singled {
        let job = served.store.under(&format!("job:{id}"))[0].1.clone();
        assert!(job["state"] == "launched" && job["on_demand"] == true, "{what}: {job}");
    }
    b.goto(&format!("{link}&p=jobs"));
    b.wait(&shows("Recent jobs"));
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

/// Jobs with each kind of label on AWS: each job's page says the machine it got, as AWS answered Test.
#[test]
fn each_kind_of_label_gets_its_aws_machine_and_its_page_says_which() {
    let Some(mut b) = Browser::open(1300, 900) else { eprintln!("no Chrome here: skipped"); return };
    let served = serve(|c| {
        (c.aws_own, c.aws_own_creds) = (Some(("111111111111".into(), "us-east-1".into())), Some(superci_core::aws::Credentials { access_key_id: "ASIAX".into(), secret_access_key: "secret".into(), session_token: Some("tok".into()), expires_at_ms: u64::MAX }));
    });
    served.connect_github();
    served.connect_gitlab();
    // (Behind its public address: its machines get their own links, Linux's and Windows'.)
    *served.public.lock().unwrap() = Some("https://plane.acme.test".into());
    served.aws.put(&format!("/superci/{PLANE}/ROUTING"), r#"{"default":null,"rules":[],"order":[{"cloud":"aws"},{"cloud":"aws-on-demand"},{"cloud":"cloudflare","off":true}]}"#);
    served.aws.replaying(include_str!("../fixtures/aws/launch/spot.jsonl"), &[]);
    let labels = ["superci", "superci/cpu=8", "superci/cpu=2/ram=16", "superci/arch=arm64", "superci/os=windows", "superci/scratch=ramdisk", "superci/scratch=localdisk", "superci/family=c8id",
        "superci/family=m7gd/scratch=localdisk", "superci/disk=200/disk-speed=500", "superci/disk-iops=6000", "superci/gpu=any", "superci/image=ubuntu26", "superci/image=ubuntu26/docker-cache=true",
        "superci/image=ubuntu26/docker-cache=false", "superci/image=ubuntu24", "superci/spot=false/cpu=16", "superci/cpu=64/family=m8a+r8a"];
    let mut jobs = vec![];
    for label in labels {
        served.store.0.lock().unwrap().retain(|k, _| !k.starts_with("aws:pool:") && !k.starts_with("aws:no-fleet"));
        let (id, q) = served.github.queue(label, label, served.now_ms());
        assert!(served.webhook(q) < 300, "{label}");
        jobs.push((id, label));
    }
    // GitLab's jobs with a few of them, as their tags.
    let mut gl = vec![];
    for label in ["superci", "superci/os=windows", "superci/image=ubuntu26/docker-cache=true", "superci/spot=false"] {
        served.store.0.lock().unwrap().retain(|k, _| !k.starts_with("aws:pool:") && !k.starts_with("aws:no-fleet"));
        let (id, p) = served.gitlab.pending(label, &[label], served.now_ms());
        assert!(served.gitlab_webhook(p) < 300, "{label}");
        gl.push((id, label));
    }
    let home = signed_in(&served);
    let (mut child, link) = dashboard(&home);
    for (id, label) in &jobs {
        let job = served.store.under(&format!("job:{id}"))[0].1.clone();
        b.goto(&format!("{link}&p=job&id={id}"));
        b.wait(&format!("{} && !document.querySelector('main').innerText.includes('Reading your control plane')", shows("Status")));
        let page = b.eval("document.querySelector('main').innerText").as_str().unwrap_or_default().replace('\n', " ");
        match job["machine_type"].as_str() {
            // Its machine, and its start (where its time went, what AWS was asked).
            Some(kind) => assert!(page.contains(kind) && page.contains("Its start"), "{label}: {kind} not on its page:\n{page}"),
            // Or why it has none.
            None => { let why = job["error"].as_str().unwrap_or_default().trim_start_matches("waiting: ").split(" (").next().unwrap_or_default().to_lowercase(); assert!(page.to_lowercase().contains(&why), "{label}: {why:?} not on its page:\n{page}") }
        }
    }
    for (id, label) in &gl {
        let job = served.store.under(&format!("job:gl{id}"))[0].1.clone();
        b.goto(&format!("{link}&p=job&id={id}&gl="));
        b.wait(&format!("{} && !document.querySelector('main').innerText.includes('Reading your control plane')", shows("Status")));
        let page = b.eval("document.querySelector('main').innerText").as_str().unwrap_or_default().replace('\n', " ");
        if let Some(kind) = job["machine_type"].as_str() { assert!(page.contains(kind), "GitLab {label}: {kind} not on its page:\n{page}") }
    }
    let launched = jobs.iter().filter(|(id, _)| served.store.under(&format!("job:{id}"))[0].1["machine_type"].is_string()).count();
    assert!(launched >= labels.len() - 2, "most get a machine: {launched} of {}", labels.len());
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

/// Browser tests of runs: what a person sees of them in the dashboard.
mod runs_in_browser {
    use super::*;
    use std::process::{Command, Stdio};
    use superci_testkit::Runner;

    fn checkout_with(files: &[(&str, &str)]) -> std::path::PathBuf {
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!("superci-e2e-files-{}-{}-{}", std::process::id(), N.fetch_add(1, std::sync::atomic::Ordering::SeqCst), superci_core_free_id()));
        for (p, text) in files { let f = dir.join(p); std::fs::create_dir_all(f.parent().unwrap()).unwrap(); std::fs::write(f, text).unwrap(); }
        for args in [&["init", "-q"][..], &["add", "-A"], &["-c", "user.name=t", "-c", "user.email=t@example.com", "commit", "-qm", "t"]] { assert!(Command::new("git").current_dir(&dir).args(args).status().unwrap().success()) }
        dir
    }

    /// A run with a called workflow, a matrix made by an expression, a job that may fail, one past its timeout and
    /// one that runs always, played to its end: its page and its jobs' pages say how each went.
    #[test]
    fn a_run_with_calls_a_matrix_a_job_that_may_fail_and_a_timeout_is_followed_on_its_pages() {
        let Some(mut b) = Browser::open(1300, 1000) else { eprintln!("no Chrome here: skipped"); return };
        let served = serve(|_| {});
        let home = signed_in(&served);
        let dir = checkout_with(&[
            (".github/workflows/ci.yml", "jobs:\n  setup:\n    runs-on: superci\n    outputs:\n      shards: ${{ steps.s.outputs.shards }}\n    steps:\n      - id: s\n        run: echo shards\n  build:\n    uses: ./.github/workflows/build.yml\n    with:\n      target: web\n  test:\n    needs: setup\n    runs-on: superci\n    strategy:\n      matrix:\n        shard: ${{ fromJSON(needs.setup.outputs.shards) }}\n    steps:\n      - run: echo shard ${{ matrix.shard }}\n  flaky:\n    runs-on: superci\n    continue-on-error: true\n    steps:\n      - run: exit 1\n  slow:\n    needs: flaky\n    runs-on: superci\n    timeout-minutes: 1\n    steps:\n      - run: sleep 600\n  report:\n    needs: [test, slow]\n    if: always()\n    runs-on: superci\n    steps:\n      - run: echo report\n"),
            (".github/workflows/build.yml", "on:\n  workflow_call:\n    inputs:\n      target:\n        type: string\n        required: true\njobs:\n  compile:\n    runs-on: superci\n    steps:\n      - run: echo ${{ inputs.target }}\n"),
        ]);
        let o = superci(&served, &dir, &["runs", "create", "--workflow=.github/workflows/ci.yml", "--detach"]).output().unwrap();
        assert!(o.status.success(), "{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
        let run = run_of(&served);
        let done = |name: &str, result: &str| { let r = runner_of(&served, &run, name); r.take(); r.finish(result); };
        let setup = runner_of(&served, &run, "setup");
        setup.take();
        setup.finish_with("succeeded", &[("shards", "[1,2]")]);
        done("build / compile", "succeeded");
        done("test (1)", "succeeded");
        done("test (2)", "failed");
        done("flaky", "failed");
        // The slow one runs past its minute: ended by the upkeep.
        let slow = runner_of(&served, &run, "slow");
        slow.take();
        served.later(91_000);
        served.upkeep();
        done("report", "succeeded");
        let (mut child, link) = dashboard(&home);
        // The run's page: each job, and how it went.
        b.goto(&format!("{link}&p=runs"));
        b.wait(&shows(".github/workflows/ci.yml"));
        b.click("main a", ".github/workflows/ci.yml");
        b.wait(&format!("{} && {} && {}", shows("build / compile"), shows("test (2)"), shows("report")));
        let page = b.eval("document.querySelector('main').innerText").as_str().unwrap_or_default().to_string();
        for job in ["setup", "build / compile", "test (1)", "test (2)", "flaky", "slow", "report"] { assert!(page.contains(job), "{job} not on the run's page:\n{page}") }
        assert!(page.contains("Failed"), "{page}");
        // The one past its timeout: its page says so.
        b.click("main a", "slow");
        b.wait(&shows("timeout-minutes (1)"));
        let _ = child.kill();
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&home);
    }

    fn checkout() -> std::path::PathBuf {
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!("superci-e2e-{}-{}-{}", std::process::id(), N.fetch_add(1, std::sync::atomic::Ordering::SeqCst), superci_core_free_id()));
        std::fs::create_dir_all(dir.join(".github/workflows")).unwrap();
        std::fs::write(dir.join(".github/workflows/ci.yml"), "jobs:\n  tests:\n    runs-on: superci\n    steps:\n      - run: make\n      - run: make test\n").unwrap();
        std::fs::write(dir.join("Makefile"), "test:\n\techo ok\n").unwrap();
        for args in [&["init", "-q"][..], &["add", "-A"], &["-c", "user.name=t", "-c", "user.email=t@example.com", "commit", "-qm", "t"]] {
            assert!(Command::new("git").current_dir(&dir).args(args).status().unwrap().success());
        }
        dir
    }
    fn superci_core_free_id() -> u128 { std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos() }

    #[test]
    fn a_run_from_a_computer_prints_every_line_once_in_order_and_ends_with_how_each_step_went() {
        let served = serve(|_| {});
        let dir = checkout();
        let home = dir.join(".home");
        let cli = Command::new(env!("CARGO_BIN_EXE_superci")).current_dir(&dir).args(["runs", "create", "--workflow=.github/workflows/ci.yml"])
            .env("SUPERCI_HOME", &home).env("SUPERCI_PLANE", &served.url).env("SUPERCI_KEY", &served.run_key).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
        // The machine started; its runner (played here) takes the job.
        let run = loop {
            if let Some((k, _)) = served.store.under("run:").into_iter().next() { break k.trim_start_matches("run:").to_string() }
            std::thread::sleep(std::time::Duration::from_millis(50));
        };
        let runner = loop { if let Some(r) = Runner::of_run(&served, &run) { break r } std::thread::sleep(std::time::Duration::from_millis(50)) };
        let line = |n: u64| format!("build line {n}");
        let (a, b) = ("aaaaaaaa-0000-0000-0000-000000000001", "bbbbbbbb-0000-0000-0000-000000000002");
        runner.take();
        assert_eq!(runner.steps(&[(a, "Run make", 1, "inProgress", None, None), (b, "Run make test", 2, "pending", None, None)]), 200);
        // Lines 1-3 as they come, then 7-8: the runner dropped 4-6 (it holds 1,024 at most).
        assert_eq!(runner.lines(a, 1, &(1..=3).map(line).collect::<Vec<_>>()), 200);
        std::thread::sleep(std::time::Duration::from_millis(1800));
        assert_eq!(runner.lines(a, 7, &(7..=8).map(line).collect::<Vec<_>>()), 200);
        std::thread::sleep(std::time::Duration::from_millis(1800));
        // The step ends: its whole log; then the next step, and the job.
        assert_eq!(runner.log(1, &(1..=8).map(|n| format!("2026-10-15T10:00:0{}.0000000Z {}", n % 10, line(n))).collect::<Vec<_>>().join("\n")), 200);
        assert_eq!(runner.steps(&[(a, "Run make", 1, "completed", Some("succeeded"), Some(1)), (b, "Run make test", 2, "inProgress", None, None)]), 200);
        assert_eq!(runner.lines(b, 1, &["ok".to_string()]), 200);
        assert_eq!(runner.log(2, "2026-10-15T10:00:09.0000000Z ok"), 200);
        assert_eq!(runner.steps(&[(a, "Run make", 1, "completed", Some("succeeded"), Some(1)), (b, "Run make test", 2, "completed", Some("succeeded"), Some(2))]), 200);
        assert_eq!(runner.finish("succeeded"), 200);
        let out = cli.wait_with_output().unwrap();
        let said = String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "{said}");
        // Every line once, in the order printed: 1-3 as they came, 7-8, then 4-6 once the step ended, said as late.
        let printed: Vec<&str> = said.lines().map(str::trim).collect();
        let at = |s: &str| printed.iter().position(|l| l.ends_with(s)).unwrap_or_else(|| panic!("no {s:?} in:\n{said}"));
        for n in 1..=8 { assert_eq!(printed.iter().filter(|l| l.ends_with(&line(n))).count(), 1, "{n} once:\n{said}") }
        assert!(at(&line(3)) < at(&line(7)) && at(&line(8)) < at("lines 4–6 of this step, which came late:") && at("which came late:") < at(&line(4)) && !said.contains("when it ends"), "{said}");
        // How it ended: each step, then the run.
        let has = |s: &str| printed.iter().position(|l| l.contains(s)).unwrap_or_else(|| panic!("no {s:?} in:\n{said}"));
        assert!(has("✓ Run make 7 s") < has("✓ Run make test 7 s") && said.contains("The run ended: success"), "{said}");
        // (Its log has no tests: no line of them.)
        assert!(!said.contains("Tests:"), "{said}");
        // Once it ran, how its start went (where the time from sending it to its first step went).
        assert!(has("Running (started in ") < has(&line(1)) && said.contains("its machine was asked for "), "{said}");
        // Its end said after its lines, the late ones too.
        assert!(at(&line(4)) < has("Ended: success"), "{said}");
        let _ = std::fs::remove_dir_all(&dir);
    }
    fn play(served: &superci_testkit::Served, run: &str, lines: &[String], result: &str) {
        let runner = loop { if let Some(r) = Runner::of_run(served, run) { break r } std::thread::sleep(std::time::Duration::from_millis(50)) };
        let a = "cccccccc-0000-0000-0000-000000000003";
        runner.take();
        runner.steps(&[(a, "Run make test", 1, "inProgress", None, None)]);
        runner.lines(a, 1, lines);
        runner.log(1, &lines.iter().map(|l| format!("2026-10-15T10:00:01.0000000Z {l}")).collect::<Vec<_>>().join("\n"));
        runner.steps(&[(a, "Run make test", 1, "completed", Some(if result == "succeeded" { "succeeded" } else { "failed" }), Some(1))]);
        runner.finish(result);
    }
    fn run_of(served: &superci_testkit::Served) -> String {
        loop {
            if let Some((k, _)) = served.store.under("run:").into_iter().next() { return k.trim_start_matches("run:").to_string() }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }
    fn superci(served: &superci_testkit::Served, dir: &std::path::Path, args: &[&str]) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_superci"));
        // (A run over waits 3 seconds at most for steps' logs here, where a minute would only slow the tests.)
        c.current_dir(dir).args(args).env("SUPERCI_HOME", dir.join(".home")).env("SUPERCI_PLANE", &served.url).env("SUPERCI_KEY", &served.run_key).env("SUPERCI_LOGS_WAIT_S", "3").stdout(Stdio::piped()).stderr(Stdio::piped());
        c
    }
    /// The runner of the job shown as `name`, once it is started.
    fn runner_of(served: &superci_testkit::Served, run: &str, name: &str) -> Runner {
        let end = std::time::Instant::now() + std::time::Duration::from_secs(20);
        loop {
            if let Some(r) = Runner::of_job(served, run, name) { return r }
            let names: Vec<String> = served.store.under(&format!("run:{run}")).into_iter().next().map(|(_, r)| r["jobs"].as_array().into_iter().flatten().map(|j| format!("{}:{}", j["name"], j["state"])).collect()).unwrap_or_default();
            assert!(std::time::Instant::now() < end, "no job {name} started: {names:?}");
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }
    #[test]
    fn a_run_from_a_computer_is_followed_in_the_dashboard_from_runs_to_its_jobs_log() {
        let Some(mut b) = Browser::open(1300, 900) else { eprintln!("no Chrome here: skipped"); return };
        let served = serve(|_| {});
        let dir = checkout();
        let started = superci(&served, &dir, &["runs", "create", "--workflow=.github/workflows/ci.yml", "--detach"]).output().unwrap();
        assert!(started.status.success(), "{}", String::from_utf8_lossy(&started.stderr));
        let run = run_of(&served);
        play(&served, &run, &["compiling".to_string(), "test 1 passed".to_string()], "succeeded");
        let home = common::signed_in(&served);
        let (mut child, link) = common::dashboard(&home);
        let shows = |words: &str| format!("document.querySelector('main') && document.querySelector('main').innerText.includes({})", serde_json::json!(words));
        b.goto(&link);
        b.wait(&shows("Usage"));
        // Runs: the run, by its workflow; its page: its job; the job's page: its steps, and their lines.
        b.click("nav a", "Runs");
        b.wait(r#"document.querySelector('main a[href*="p=run&"]') !== null"#);
        b.eval(r#"document.querySelector('main a[href*="p=run&"]').click()"#);
        b.wait(&shows("tests"));
        b.wait(r#"document.querySelector('main a[href*="p=job&"]') !== null"#);
        b.eval(r#"document.querySelector('main a[href*="p=job&"]').click()"#);
        b.wait("document.querySelector('details.lstep') !== null");
        b.click("details.lstep summary", "Run make test");
        b.wait("[...document.querySelectorAll('.ll .lx')].some(l => l.textContent === 'test 1 passed')");
        let _ = child.kill();
        let _ = std::fs::remove_dir_all(&home);
        let _ = std::fs::remove_dir_all(&dir);
    }
    #[test]
    fn overviews_period_and_the_runs_and_jobs_tabs_switch_in_the_browser() {
        let Some(mut b) = Browser::open(1300, 900) else { eprintln!("no Chrome here: skipped"); return };
        let served = serve(|_| {});
        let dir = checkout();
        let started = superci(&served, &dir, &["runs", "create", "--workflow=.github/workflows/ci.yml", "--detach"]).output().unwrap();
        assert!(started.status.success(), "{}", String::from_utf8_lossy(&started.stderr));
        play(&served, &run_of(&served), &["ok".to_string()], "succeeded");
        let home = common::signed_in(&served);
        let (mut child, link) = common::dashboard(&home);
        let shows = |words: &str| format!("document.querySelector('main') && document.querySelector('main').innerText.includes({})", serde_json::json!(words));
        let on = |period: &str| format!(r#"document.querySelector('nav.seg-links a[aria-current="true"]') && document.querySelector('nav.seg-links a[aria-current="true"]').textContent === {}"#, serde_json::json!(period));
        b.goto(&link);
        b.wait(&shows("Usage"));
        // The period: 24 hours unless chosen; 7 days, then 30, each marked and kept in the address.
        b.wait(&on("24 hours"));
        b.click("nav.seg-links a", "7 days");
        b.wait(&format!("{} && new URLSearchParams(location.search).get('usage') === 'week'", on("7 days")));
        b.click("nav.seg-links a", "30 days");
        b.wait(&format!("{} && new URLSearchParams(location.search).get('usage') === 'month'", on("30 days")));
        // Runs and Jobs: one tab each, the run there and its job here.
        b.click("nav a", "Runs");
        b.wait(r#"document.querySelector('a.tab[aria-selected="true"]') && document.querySelector('a.tab[aria-selected="true"]').textContent === 'Runs'"#);
        b.wait(&shows("ci.yml"));
        b.click("a.tab", "Jobs");
        b.wait(r#"document.querySelector('a.tab[aria-selected="true"]') && document.querySelector('a.tab[aria-selected="true"]').textContent === 'Jobs'"#);
        b.wait(&shows("tests"));
        let _ = child.kill();
        let _ = std::fs::remove_dir_all(&home);
        let _ = std::fs::remove_dir_all(&dir);
    }
    #[test]
    fn a_running_run_is_cancelled_from_its_page_in_the_browser() {
        let Some(mut b) = Browser::open(1300, 900) else { eprintln!("no Chrome here: skipped"); return };
        let served = serve(|_| {});
        let dir = checkout();
        let o = superci(&served, &dir, &["runs", "create", "--workflow=.github/workflows/ci.yml", "--detach"]).output().unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        let run = run_of(&served);
        let runner = runner_of(&served, &run, "tests");
        runner.take();
        runner.steps(&[("abababab-0000-0000-0000-000000000007", "Run make", 1, "inProgress", None, None)]);
        let home = common::signed_in(&served);
        let (mut child, link) = common::dashboard(&home);
        let shows = |words: &str| format!("document.querySelector('main') && document.querySelector('main').innerText.includes({})", serde_json::json!(words));
        b.goto(&link);
        b.wait(&shows("Usage"));
        b.click("nav a", "Runs");
        b.wait(r#"document.querySelector('main a[href*="p=run&"]') !== null"#);
        b.eval(r#"document.querySelector('main a[href*="p=run&"]').click()"#);
        // Running: its Cancel; clicked, its runner is told to stop, as GitHub tells one.
        b.wait(r#"[...document.querySelectorAll('main button')].some(x => x.textContent === 'Cancel run')"#);
        b.eval(r#"[...document.querySelectorAll('main button')].find(x => x.textContent === 'Cancel run').click()"#);
        let end = std::time::Instant::now() + std::time::Duration::from_secs(15);
        let told = loop {
            let (status, m) = runner.message();
            if status == 200 { break m }
            assert!(std::time::Instant::now() < end, "its runner was not told");
        };
        assert_eq!(told["messageType"], "JobCancellation", "{told}");
        runner.finish("canceled");
        b.wait(&shows("Cancelled"));
        let _ = child.kill();
        let _ = std::fs::remove_dir_all(&home);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// Browser tests of github: what a person sees of them in the dashboard.
mod github_in_browser {
    use super::*;
    use common::{json, part, superci, words};
    use superci_testkit::Served;

    fn unanswered(served: &Served) -> Vec<String> { served.github.unanswered.lock().unwrap().iter().filter(|u| u.contains("github")).cloned().collect() }

    /// A job of `acme/app` labelled `superci`, from queued to ended as GitHub tells it: its number.
    fn ran(served: &Served, conclusion: &str) -> u64 {
        let (id, queued) = served.github.queue("test", "superci", served.now_ms());
        assert_eq!(served.webhook(queued), 202);
        // A machine for it, its runner registered with GitHub for exactly this job.
        assert_eq!(served.machines.0.lock().unwrap().len(), 1, "its machine");
        let job = served.store.under(&format!("job:{id}")).into_iter().next().expect("the job kept").1;
        assert_eq!((job["state"].as_str(), job["repo"].as_str()), (Some("launched"), Some("acme/app")), "{job}");
        assert_eq!(served.webhook(served.github.start(id)), 202);
        assert_eq!(served.store.under(&format!("job:{id}"))[0].1["state"], "running");
        assert_eq!(served.webhook(served.github.finish(id, conclusion, &github_log(), served.now_ms())), 202);
        id
    }
    /// GitHub's log of a job, as it keeps it (each line timed; a step's lines in a group).
    fn github_log() -> String {
        ["2026-10-15T10:00:00.1000000Z ##[group]Set up job", "2026-10-15T10:00:00.2000000Z Current runner version: '2.329.0'", "2026-10-15T10:00:00.3000000Z ##[endgroup]",
            "2026-10-15T10:00:01.0000000Z ##[group]Run make test", "2026-10-15T10:00:01.1000000Z \u{1b}[36;1mmake test\u{1b}[0m", "2026-10-15T10:00:01.2000000Z ##[endgroup]",
            "2026-10-15T10:00:02.0000000Z test 1 passed", "2026-10-15T10:00:03.0000000Z \u{1b}[32mall 2 passed\u{1b}[0m", "2026-10-15T10:00:04.0000000Z Cleaning up orphan processes"].join("\n")
    }
    #[test]
    fn a_github_jobs_page_shows_its_log_in_steps_and_leads_to_github() {
        let served = serve(|_| {});
        served.connect_github();
        let home = signed_in(&served);
        let id = ran(&served, "failure");
        let (mut child, link) = dashboard(&home);
        // Its page as the dashboard's page fetches it, and its log in steps.
        let (_, page) = part(&link, &format!("/?p=job&id={id}&fragment=1"));
        let shown = words(&page);
        assert!(shown.contains("app · Build and Test") && shown.contains("Status Failed") && !shown.contains("It did not start"), "{shown}");
        assert!(page.contains(&format!("/actions/runs/")) && page.contains("On GitHub"), "{shown}");
        let (_, view) = part(&link, &format!("/job/view?id={id}"));
        let view: serde_json::Value = serde_json::from_str(&view).unwrap();
        let steps: Vec<&str> = view["steps"].as_array().unwrap().iter().filter_map(|s| s["name"].as_str()).collect();
        assert!(steps.contains(&"Set up job") && steps.contains(&"Run make test"), "{view}");
        // Its raw log is GitHub's own link.
        let (status, _) = part(&link, &format!("/job/raw?id={id}"));
        assert_eq!(status, 302);
        // Repositories: the App connected in acme, its repository.
        let (_, repos) = part(&link, "/?p=repos&fragment=1");
        assert!(words(&repos).contains("acme"), "{}", words(&repos));

        // In Chrome: from Jobs to the job, its failed step open with its lines.
        if let Some(mut b) = Browser::open(1300, 900) {
            b.goto(&link);
            b.wait("document.querySelector('main') && document.querySelector('main').innerText.includes('Usage')");
            b.click("nav a", "Runs");
            b.wait("document.querySelector('main').innerText.includes('Recent runs')");
            b.click(".tabs a", "Jobs");
            b.wait("document.querySelector('main').innerText.includes('Recent jobs')");
            b.click("main a", "test");
            b.wait("document.querySelector('details.lstep')");
            b.wait("[...document.querySelectorAll('details.lstep')].some(d => d.innerText.includes('Run make test'))");
            let on_github = b.eval("document.querySelector('a[href^=\"https://github.com/acme/app/actions/runs/\"]').innerText");
            assert!(on_github.as_str().unwrap_or_default().contains("On GitHub"), "{on_github}");
        }
        assert!(unanswered(&served).is_empty(), "{:?}", unanswered(&served));
        let _ = child.kill();
        let _ = std::fs::remove_dir_all(&home);
    }
    #[test]
    fn a_finished_jobs_page_shows_its_whole_log_however_large() {
        let served = serve(|_| {});
        served.connect_github();
        let home = signed_in(&served);
        let (id, queued) = served.github.queue("test", "superci", served.now_ms());
        served.webhook(queued);
        served.webhook(served.github.start(id));
        // 2.4 MB: more than the last part a running job's page reads.
        // (Its lines in the second its steps began, as GitHub says of them.)
        let at = served.github.job(id)["started_at"].as_str().unwrap().trim_end_matches('Z').to_string();
        let mut log = vec![format!("{at}.1000000Z ##[group]Set up job"), format!("{at}.2000000Z ##[endgroup]"), format!("{at}.3000000Z ##[group]Run make test")];
        log.extend((1..=40_000).map(|n| format!("{at}.4000000Z test {n} of the suite passed in good time")));
        let log = log.join("\n");
        assert!(log.len() > 2_000_000);
        served.webhook(served.github.finish(id, "success", &log, served.now_ms()));
        let (mut child, link) = dashboard(&home);
        let (_, view) = part(&link, &format!("/job/view?id={id}"));
        let view: serde_json::Value = serde_json::from_str(&view).unwrap();
        assert_eq!(view["truncated"], false);
        let step = view["steps"].as_array().unwrap().iter().find(|s| s["name"] == "Run make test").unwrap();
        let lines = step["lines"].as_array().unwrap();
        assert_eq!((lines.len(), lines[1][2].as_str(), lines[40_000][2].as_str()), (40_001, Some("test 1 of the suite passed in good time"), Some("test 40000 of the suite passed in good time")));
        // In Chrome: nothing of it drawn until a step is opened, then its first 2,000 lines and a way to more; search
        // finds a line far down without drawing the step whole.
        if let Some(mut b) = Browser::open(1300, 900) {
            b.goto(&format!("{link}&p=job&id={id}"));
            b.wait("document.querySelector('details.lstep')");
            assert_eq!(b.eval("document.querySelectorAll('.ll').length"), 0);
            b.click("details.lstep summary", "Run make test");
            b.wait("document.querySelectorAll('.ll').length === 2000");
            assert_eq!(b.text(".ll-more"), "Show 4,000 more of 38,001 lines");
            b.click(".ll-more", "");
            b.wait("document.querySelectorAll('.ll').length === 6000");
            assert_eq!(b.eval("document.querySelectorAll('.ll')[5999].querySelector('.ln').textContent"), "6000", "each line its number");
            b.fill("[data-log-search]", "test 39999 of");
            b.wait("document.querySelectorAll('.ll.hit').length === 1");
            assert_eq!((b.text(".ll.hit .lx"), b.eval("document.querySelectorAll('.ll').length")), ("test 39999 of the suite passed in good time".to_string(), serde_json::json!(1)));
            // Cleared: the step as it was opened (6,000 lines shown).
            b.fill("[data-log-search]", "");
            b.wait("document.querySelectorAll('.ll').length === 6000");
        }
        let _ = child.kill();
        let _ = std::fs::remove_dir_all(&home);
    }
    #[test]
    fn a_github_job_runs_on_its_machine_and_is_read_with_its_log_in_the_cli() {
        let served = serve(|_| {});
        served.connect_github();
        let home = signed_in(&served);
        let id = ran(&served, "success");
        let job = served.store.under(&format!("job:{id}"))[0].1.clone();
        assert_eq!((job["state"].as_str(), job["conclusion"].as_str()), (Some("done"), Some("success")), "{job}");

        // Listed, with its repository and its page at GitHub.
        let jobs = json(&home, &["jobs", "list"]).1;
        let listed = &jobs["jobs"][0];
        assert_eq!((listed["job"].as_str(), listed["ended"].as_str()), (Some("test"), Some("passed")), "{jobs}");
        assert!(listed["link"].as_str().unwrap().starts_with("https://github.com/acme/app/actions/runs/"), "{jobs}");
        // Its log, from GitHub.
        let o = superci(&home, &["jobs", "retrieve", &id.to_string()]);
        let said = String::from_utf8_lossy(&o.stdout);
        assert!(o.status.success() && said.contains("Run make test") && said.contains("all 2 passed"), "{said}{}", String::from_utf8_lossy(&o.stderr));
        // The whole of it into a file, from GitHub's own link.
        let out = home.join("job.log");
        let (code, saved) = json(&home, &["jobs", "retrieve", &id.to_string(), &format!("--out={}", out.display())]);
        assert_eq!(code, 0, "{saved}");
        assert_eq!(std::fs::read_to_string(&out).unwrap(), github_log());
        // What its log says of tests (none of the runners' that are read), and where its artifacts are: at GitHub.
        let said = String::from_utf8_lossy(&superci(&home, &["jobs", "retrieve", &id.to_string(), "--tests"]).stdout).into_owned();
        assert!(said.contains("No test results in its log") && said.contains(&format!("Its artifacts: https://github.com/acme/app/actions/runs/{}#artifacts", job["run_id"])), "{said}");
        // The App, its installation and what it may do.
        let connections = json(&home, &["connections", "list"]).1;
        assert_eq!(connections["github"][0]["owner"].as_str().or(connections["installations"][0]["account"].as_str()), Some("acme"), "{connections}");
        assert!(unanswered(&served).is_empty(), "{:?}", unanswered(&served));
        let _ = std::fs::remove_dir_all(&home);
        super::seen_in_browser(&served);
    }

    #[test]
    fn a_public_repositorys_jobs_wait_until_it_is_allowed_then_run() {
        let served = serve(|_| {});
        served.connect_github();
        let home = signed_in(&served);
        served.github.set_public(true);
        // Refused: anyone could open a pull request that runs code on these machines.
        let (first, queued) = served.github.queue("test", "superci", served.now_ms());
        assert_eq!(served.webhook(queued), 202);
        let job = served.store.under(&format!("job:{first}"))[0].1.clone();
        assert!(job["state"] == "failed" && job["error"].as_str().unwrap().contains("a public repository"), "{job}");
        assert!(served.machines.0.lock().unwrap().is_empty());
        let jobs = json(&home, &["jobs", "list"]).1;
        assert!(jobs["jobs"][0]["why"].as_str().unwrap_or_default().to_lowercase().contains("public"), "{jobs}");
        // Allowed in Settings: its next job runs.
        assert_eq!(json(&home, &["public_repos", "create", "acme/app"]).0, 0);
        assert_eq!(json(&home, &["public_repos", "list"]).1["public_repos"], serde_json::json!(["acme/app"]));
        let (_, queued) = served.github.queue("test", "superci", served.now_ms());
        assert_eq!(served.webhook(queued), 202);
        assert_eq!(served.machines.0.lock().unwrap().len(), 1);
        assert_eq!(json(&home, &["public_repos", "delete", "acme/app"]).0, 0);
        assert!(unanswered(&served).is_empty(), "{:?}", unanswered(&served));
        super::seen_in_browser(&served);
    }

    #[test]
    fn a_job_cancelled_before_its_runner_took_it_ends_its_machine() {
        let served = serve(|_| {});
        served.connect_github();
        let (id, queued) = served.github.queue("test", "superci", served.now_ms());
        served.webhook(queued);
        assert_eq!(served.webhook(served.github.cancel(id, served.now_ms())), 202);
        let job = served.store.under(&format!("job:{id}"))[0].1.clone();
        assert!(["cancelled", "done"].contains(&job["state"].as_str().unwrap()) && job["ended_ms"].is_u64(), "{job}");
        // Its runner is taken back from GitHub.
        assert!(served.github.asked.lock().unwrap().iter().any(|a| a.starts_with("DELETE https://api.github.com/orgs/acme/actions/runners/")), "{:?}", served.github.asked.lock().unwrap());
        super::seen_in_browser(&served);
    }

    #[test]
    fn a_job_github_no_longer_waits_for_is_noticed_by_the_upkeep_and_its_machine_ended() {
        let served = serve(|_| {});
        served.connect_github();
        let (id, queued) = served.github.queue("test", "superci", served.now_ms());
        served.webhook(queued);
        // Another runner took it and it ended; GitHub's word of that never came.
        served.github.ended_untold(id);
        served.later(30_000);
        served.upkeep();
        assert_eq!(served.store.under(&format!("job:{id}"))[0].1["state"], "launched", "not asked before it has had time to come up");
        served.later(60_000);
        served.upkeep();
        let job = served.store.under(&format!("job:{id}"))[0].1.clone();
        assert!(["swept", "done"].contains(&job["state"].as_str().unwrap()), "{job}");
        assert!(served.github.asked.lock().unwrap().iter().any(|a| a == &format!("GET https://api.github.com/repos/acme/app/actions/jobs/{id}")));
        super::seen_in_browser(&served);
    }

    #[test]
    fn a_job_its_machine_began_whose_event_was_lost_is_seen_running_by_the_upkeep() {
        let served = serve(|_| {});
        served.connect_github();
        let (id, queued) = served.github.queue("test", "superci", served.now_ms());
        served.webhook(queued);
        // Its runner took it; GitHub's word of that never came.
        let _ = served.github.start(id);
        served.later(91_000);
        served.upkeep();
        let job = served.store.under(&format!("job:{id}"))[0].1.clone();
        assert!(job["state"] == "running" && job["machine_id"].is_string(), "{job}");
        super::seen_in_browser(&served);
    }

    #[test]
    fn a_machine_whose_runner_took_another_job_untold_is_kept_running_it() {
        let served = serve(|_| {});
        served.connect_github();
        let (a, q) = served.github.queue("first", "superci", served.now_ms());
        served.webhook(q);
        let (b, q) = served.github.queue("second", "superci", served.now_ms());
        served.webhook(q);
        let runner_of = |id: u64| served.store.under(&format!("job:{id}"))[0].1["runner"].as_str().unwrap().to_string();
        // The first's runner took the second, and the first ran elsewhere; neither event came.
        let _ = served.github.start_on(b, &runner_of(a));
        served.github.ended_untold(a);
        served.later(91_000);
        served.upkeep();
        let (first, second) = (served.store.under(&format!("job:{a}"))[0].1.clone(), served.store.under(&format!("job:{b}"))[0].1.clone());
        // The first's machine runs a job (its runner is busy): kept. The second's began nothing and its job is taken: ended.
        assert_eq!(first["state"], "running", "{first}");
        assert!(["swept", "done"].contains(&second["state"].as_str().unwrap()), "{second}");
        super::seen_in_browser(&served);
    }

    #[test]
    fn a_webhook_not_signed_by_the_app_is_refused() {
        let served = serve(|_| {});
        served.connect_github();
        let (_, (body, _)) = served.github.queue("test", "superci", served.now_ms());
        assert_eq!(served.webhook((body, format!("sha256={}", "0".repeat(64)))), 401);
        assert!(served.machines.0.lock().unwrap().is_empty() && served.store.under("job:").is_empty());
        super::seen_in_browser(&served);
    }

    #[test]
    fn github_is_connected_from_repositories_and_its_jobs_then_run() {
        let served = serve(|_| {});
        let home = signed_in(&served);
        let (mut child, link) = dashboard(&home);
        // Repositories → Create its App: a form that takes the App's manifest to GitHub, for the organization named.
        let (status, _, form) = common::post(&link, "/github/start", &[("gh-where", "public"), ("host", ""), ("login", "acme")]);
        assert_eq!(status, 200, "{}", words(&form));
        assert!(form.contains("https://github.com/organizations/acme/settings/apps/new?state="), "{form}");
        let state = form.split("state=").nth(1).unwrap().split(|c: char| !c.is_ascii_alphanumeric() && c != '-' && c != '_').next().unwrap().to_string();
        let manifest = form.split("name=\"manifest\" value=\"").nth(1).unwrap().split('"').next().unwrap().replace("&quot;", "\"").replace("&amp;", "&");
        let manifest: serde_json::Value = serde_json::from_str(&manifest).unwrap();
        assert_eq!((manifest["hook_attributes"]["url"].as_str(), manifest["default_events"][0].as_str()), (Some(format!("{}/webhook", served.url).as_str()), Some("workflow_job")), "{manifest}");
        // A code that is no code GitHub gives (too short, too long, with what codes have not) is not taken to GitHub.
        for code in ["abc", &"a".repeat(101), "a1b2c3d4%21%21"] {
            let (status, _, said) = common::get(&link, &format!("/github/callback?code={code}&state={state}"));
            assert!(status >= 400 && words(&said).contains("invalid manifest code"), "{code}: {status} {}", words(&said));
        }
        // GitHub made it and comes back with its code: the App goes into the control plane's settings, then GitHub's
        // page to install it.
        let (status, to, _) = common::get(&link, &format!("/github/callback?code=a1b2c3d4e5f6&state={state}"));
        assert_eq!((status, to.as_str()), (303, "https://github.com/apps/superci-acme/installations/new"));
        let kept: serde_json::Value = serde_json::from_str(&served.aws.settings(superci_testkit::PLANE).into_iter().find(|(k, _)| k == "GITHUB_APP").unwrap().1).unwrap();
        assert_eq!((kept["id"].as_u64(), kept["owner"].as_str(), kept["owner_is_org"].as_bool()), (Some(1001), Some("acme"), Some(true)));
        // A code that is not this tab's is refused.
        assert_eq!(common::get(&link, "/github/callback?code=a1b2c3d4e5f6&state=other").0, 400);
        // Installed: back here, and Repositories shows acme.
        assert_eq!(common::get(&link, "/github/installed").0, 200);
        let (_, repos) = part(&link, "/?p=repos&fragment=1");
        assert!(words(&repos).contains("acme"), "{}", words(&repos));
        // Its jobs run.
        ran(&served, "success");
        assert!(unanswered(&served).is_empty(), "{:?}", unanswered(&served));
        let _ = child.kill();
        let _ = std::fs::remove_dir_all(&home);
        super::seen_in_browser(&served);
    }

    #[test]
    fn a_logs_odd_lines_and_a_run_logs_zip_with_odd_files_are_read_as_far_as_they_hold() {
        let served = serve(|_| {});
        served.connect_github();
        let home = signed_in(&served);
        // A log with what a job's log can hold: a January time, colours (one never closed), a tab and a carriage
        // return, an escape that is no colour, a line with no time, an erase-line code.
        let log = "2026-01-15T10:00:00.0000000Z ##[group]Run make\n2026-01-15T10:00:01.0000000Z \u{1b}[1;31mred\u{1b}[0m plain \u{1b}[32mstill open\n2026-01-15T10:00:02.0000000Z tab\there\r\n2026-01-15T10:00:03.0000000Z esc\u{1b}X not a colour\nshort\n2026-01-15T10:00:04.0000000Z \u{1b}[2Kerased\n2026-01-15T10:00:05.0000000Z ##[endgroup]\n";
        let (id, q) = served.github.queue("odd log", "superci", served.now_ms()); served.webhook(q);
        served.webhook(served.github.start(id));
        served.webhook(served.github.finish(id, "success", log, served.now_ms()));
        let (mut child, link) = dashboard(&home);
        let (status, view) = part(&link, &format!("/job/view?id={id}"));
        assert!(status == 200 && view.contains("still open"), "{status} {view}");
        // Steps run side by side, with its run's logs zipped: files named as no step is, and one zip whose directory is
        // damaged (its steps' lines as they came, then).
        fn zip_of(files: &[(&str, &[u8])], damaged: bool) -> Vec<u8> {
            let (mut out, mut dir) = (vec![], vec![]);
            for (name, data) in files {
                let at = out.len() as u32;
                let (n, size) = (name.len() as u16, data.len() as u32);
                out.extend([0x50, 0x4b, 0x03, 0x04, 20, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
                out.extend(size.to_le_bytes()); out.extend(size.to_le_bytes()); out.extend(n.to_le_bytes()); out.extend(0u16.to_le_bytes());
                out.extend(name.as_bytes()); out.extend(*data);
                dir.extend([0x50, 0x4b, if damaged { 0x09 } else { 0x01 }, 0x02, 20, 0, 20, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
                dir.extend(size.to_le_bytes()); dir.extend(size.to_le_bytes()); dir.extend(n.to_le_bytes());
                dir.extend([0u8; 12]); dir.extend(at.to_le_bytes()); dir.extend(name.as_bytes());
            }
            let (cd_at, cd_len, count) = (out.len() as u32, dir.len() as u32, files.len() as u16);
            out.extend(dir);
            out.extend([0x50, 0x4b, 0x05, 0x06, 0, 0, 0, 0]);
            out.extend(count.to_le_bytes()); out.extend(count.to_le_bytes()); out.extend(cd_len.to_le_bytes()); out.extend(cd_at.to_le_bytes()); out.extend([0, 0]);
            out
        }
        for damaged in [false, true] {
            let (id, q) = served.github.queue("test", "superci", served.now_ms()); served.webhook(q);
            served.webhook(served.github.start(id));
            served.webhook(served.github.finish(id, "success", &github_log(), served.now_ms()));
            served.github.set_steps(id, serde_json::from_str(include_str!("../fixtures/github/parallel-steps.json")).unwrap());
            let job = served.github.job(id)["name"].as_str().unwrap_or("tests").to_string();
            let files: Vec<(String, Vec<u8>)> = vec![(format!("{job}/1_Set up job.txt"), b"2026-10-15T10:00:00.0000000Z set up\n".to_vec()), (format!("{job}/notastep.txt"), b"x".to_vec()),
                (format!("{job}/x_named.txt"), b"x".to_vec()), (format!("{job}/_nothing.txt"), b"x".to_vec()), ("another job/1_Set up job.txt".to_string(), b"x".to_vec())];
            let refs: Vec<(&str, &[u8])> = files.iter().map(|(n, d)| (n.as_str(), d.as_slice())).collect();
            served.github.run_ended(id, zip_of(&refs, damaged));
            let (status, _) = part(&link, &format!("/job/view?id={id}"));
            assert_eq!(status, 200);
        }
        let _ = child.kill();
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn a_jobs_steps_run_side_by_side_show_their_own_lines_once_its_run_has_ended() {
        let served = serve(|_| {});
        served.connect_github();
        let home = signed_in(&served);
        let id = ran(&served, "success");
        // GitHub's steps and run logs for a job with background steps and a parallel group (a probe on GitHub). Its own
        // log has their lines mixed: "two" after the next step's heading.
        served.github.set_steps(id, serde_json::from_str(include_str!("../fixtures/github/parallel-steps.json")).unwrap());
        let (mut child, link) = dashboard(&home);
        let lines = |view: &serde_json::Value, name: &str| -> Vec<String> {
            let s = view["steps"].as_array().unwrap().iter().find(|s| s["name"] == name).unwrap_or_else(|| panic!("{name}: {view}"));
            s["lines"].as_array().unwrap().iter().map(|l| l[2].as_str().unwrap().to_string()).collect()
        };
        // While its run goes on (GitHub keeps a run's logs once it has ended): its lines as they came, and said so.
        let (_, view) = part(&link, &format!("/job/view?id={id}"));
        let view: serde_json::Value = serde_json::from_str(&view).unwrap();
        assert_eq!(view["mixed"], true, "{view}");
        // Its run ended: each step's own lines, from its run's logs (not the view kept from before).
        served.github.run_ended(id, include_bytes!("../fixtures/github/run-logs.zip").to_vec());
        let (_, view) = part(&link, &format!("/job/view?id={id}"));
        let view: serde_json::Value = serde_json::from_str(&view).unwrap();
        assert_eq!(view.get("mixed"), None, "{view}");
        assert_eq!(lines(&view, "Run echo two; sleep 2").last().map(String::as_str), Some("two"));
        assert_eq!(lines(&view, "Run echo while they run").last().map(String::as_str), Some("while they run"));
        assert!(lines(&view, "Parallel group").iter().any(|l| l.contains("First: Succeeded")), "{view}");
        let _ = child.kill();
        let _ = std::fs::remove_dir_all(&home);
        super::seen_in_browser(&served);
    }

    #[test]
    fn a_github_run_is_cancelled_from_the_cli_by_github_and_its_running_jobs_machine_ends() {
        let served = serve(|_| {});
        served.connect_github();
        let home = signed_in(&served);
        let (id, queued) = served.github.queue("test", "superci", served.now_ms());
        assert_eq!(served.webhook(queued), 202);
        assert_eq!(served.webhook(served.github.start(id)), 202);
        let run = served.github.job(id)["run_id"].as_u64().unwrap().to_string();
        // Cancelled from the CLI: GitHub is asked, with the App's token for its repository.
        let o = superci(&home, &["runs", "cancel", &run]);
        let said = String::from_utf8_lossy(&o.stdout).to_string() + &String::from_utf8_lossy(&o.stderr);
        assert!(o.status.success() && said.contains(&format!("GitHub cancels run {run}")), "{said}");
        assert!(served.github.asked.lock().unwrap().iter().any(|a| a == &format!("POST https://api.github.com/repos/acme/app/actions/runs/{run}/cancel")), "{:?}", served.github.asked.lock().unwrap());
        // GitHub stops its runner's job and says so: its machine is ended.
        assert_eq!(served.webhook(served.github.cancel(id, served.now_ms())), 202);
        let job = served.store.under(&format!("job:{id}"))[0].1.clone();
        assert!(job["ended_ms"].is_u64() && job["conclusion"] == "cancelled", "{job}");
        // A run nobody here knows: said so.
        let o = superci(&home, &["runs", "cancel", "12345"]);
        assert!(!o.status.success() && String::from_utf8_lossy(&o.stderr).contains("No run 12345 here"), "{}", String::from_utf8_lossy(&o.stderr));
        // Another run, cancelled from the dashboard (its page's Cancel run): GitHub is asked the same way, and the page
        // is shown again.
        let (other, queued) = served.github.queue("test", "superci", served.now_ms());
        assert_eq!(served.webhook(queued), 202);
        assert_eq!(served.webhook(served.github.start(other)), 202);
        let run = served.github.job(other)["run_id"].as_u64().unwrap().to_string();
        let (mut child, link) = dashboard(&home);
        let (status, to, said) = common::post(&link, "/run/cancel", &[("id", &run), ("repo", "acme/app"), ("attempt", "1")]);
        assert!(status == 303 && to.starts_with(&format!("/?p=run&repo=acme%2Fapp&id={run}&attempt=1")), "{status} {to} {}", words(&said));
        assert!(served.github.asked.lock().unwrap().iter().any(|a| a == &format!("POST https://api.github.com/repos/acme/app/actions/runs/{run}/cancel")));
        let (status, _, said) = common::post(&link, "/run/cancel", &[("id", "not-a-run")]);
        assert!(status == 400 && words(&said).contains("Which run?"), "{status}");
        let _ = child.kill();
        let _ = std::fs::remove_dir_all(&home);
        super::seen_in_browser(&served);
    }

    #[test]
    fn a_job_asking_for_more_than_allowed_fails_at_once_saying_why_and_the_dashboard_shows_why() {
        let served = serve(|_| {});
        served.connect_github();
        let home = signed_in(&served);
        // More than any machine (a label that says no machine), and more than this CI allows (32 CPUs unless set).
        for (label, why) in [("superci/cpu=999", "cpu is a number of CPUs from 1 to 192"), ("superci/cpu=64", "the most allowed is 32")] {
            let (id, queued) = served.github.queue("test", label, served.now_ms());
            assert_eq!(served.webhook(queued), 202);
            let job = served.store.under(&format!("job:{id}"))[0].1.clone();
            assert!(job["state"] == "failed" && job["failed_fast"] == true && job["error"].as_str().unwrap_or_default().contains(why), "{label}: {job}");
        }
        assert!(served.machines.0.lock().unwrap().iter().all(|m| m.ends_with("-x")), "no machine for it, only one that fails it on GitHub: {:?}", served.machines.0.lock().unwrap());
        let (mut child, link) = dashboard(&home);
        let (_, page) = part(&link, "/?p=jobs&fragment=1");
        assert!(words(&page).contains("the most allowed is 32"), "{}", words(&page));
        let _ = child.kill();
        let _ = std::fs::remove_dir_all(&home);
        super::seen_in_browser(&served);
    }

    #[test]
    fn each_jobs_row_says_where_it_ran_and_why_in_plain_words() {
        let served = serve(|_| {});
        served.connect_github();
        let home = signed_in(&served);
        // Jobs as the control plane keeps them after what AWS and Cloudflare said (each a real job of acme/app, its
        // record then given what that situation writes).
        let cases: Vec<(serde_json::Value, &str)> = vec![
            (serde_json::json!({ "state": "done", "cloud": "aws", "machine_type": "c8a.xlarge", "cpu": 4, "on_demand": true, "spot_refused": "us-east-1: MaxSpotInstanceCountExceeded: Max spot instance count exceeded", "spot_ending": true }), "On-demand: your AWS spot limit in N. Virginia was reached (machines AWS was still ending count too)"),
            (serde_json::json!({ "state": "done", "cloud": "aws", "machine_type": "c8a.xlarge", "cpu": 4, "on_demand": true, "spot_refused": "us-east-1: InsufficientInstanceCapacity" }), "On-demand: AWS had no spot machine for it"),
            (serde_json::json!({ "state": "done", "cloud": "aws", "machine_type": "c8a.xlarge", "cpu": 4, "on_demand": true, "spot_refused": format!("us-east-1: {}", superci_core::plane::SPOT_NOT_ALLOWED) }), "On-demand: your AWS organization does not allow spot machines"),
            (serde_json::json!({ "state": "done", "cloud": "aws", "machine_type": "c8a.xlarge", "cpu": 4, "on_demand": true, "spot_refused": "us-east-1: RequestLimitExceeded" }), "On-demand: AWS refused the spot request for its rate"),
            (serde_json::json!({ "state": "done", "cloud": "aws", "machine_type": "c8a.xlarge", "cpu": 4, "on_demand": true, "after_reclaim": true }), "On-demand: run again after AWS took back its spot machine"),
            (serde_json::json!({ "state": "running", "cloud": "aws", "machine_type": "c8a.2xlarge", "cpu": 8, "spot_again": true }), "On a spot machine again: an on-demand one does not fit"),
            (serde_json::json!({ "state": "done", "cloud": "aws", "machine_type": "m8a.xlarge", "cpu": 4, "local_disk": true, "disk_gb": 120, "disk_mibs": 500 }), "4 CPU · m8a.xlarge · 120 GB disk at 500 MiB/s · local disk"),
            (serde_json::json!({ "state": "failed", "cloud": "cloudflare", "error": "runner container: 502 the container did not start in time" }), "Cloudflare did not start its container"),
            (serde_json::json!({ "state": "failed", "cloud": "cloudflare", "error": "the container cannot be kept running" }), "Cloudflare could not keep its container running"),
            (serde_json::json!({ "state": "failed", "cloud": "", "error": format!("waiting: {}", "x".repeat(130)) }), &"…"),
            (serde_json::json!({ "state": "swept", "cloud": "aws", "error": null }), "Its machine never started the job"),
            (serde_json::json!({ "state": "orphan", "cloud": "aws", "error": null }), "Cancelled"),
            // Kept off on-demand before its cloud is said; a RAM disk at more than AWS's own speed; spot again, but on
            // an on-demand machine after all, or on Cloudflare (neither said to be on spot again).
            (serde_json::json!({ "state": "launching", "cloud": "", "spot_again": true }), "On a spot machine again"),
            (serde_json::json!({ "state": "done", "cloud": "aws", "machine_type": "r8a.4xlarge", "cpu": 16, "ramdisk": true, "disk_mibs": 1000 }), "16 CPU · r8a.4xlarge · 1000 MiB/s disk · RAM disk"),
            (serde_json::json!({ "state": "done", "cloud": "aws", "machine_type": "c8a.xlarge", "cpu": 4, "on_demand": true, "spot_again": true, "after_reclaim": true }), "4 CPU · c8a.xlarge · on-demand"),
            (serde_json::json!({ "state": "done", "cloud": "cloudflare", "machine_type": "4cpu-12gb", "spot_again": true }), "4 CPU · 12 GB"),
            (serde_json::json!({ "state": "done", "cloud": "cloudflare", "machine_type": "2cpu-8gb", "on_demand": true }), "2 CPU · 8 GB"),
            (serde_json::json!({ "state": "done", "cloud": "aws", "machine_type": "c8a.large", "cpu": 2, "on_demand": true }), "2 CPU · c8a.large · on-demand"),
            (serde_json::json!({ "state": "waiting", "cloud": "", "error": "waiting: Cloudflare runs 2 at once" }), "Cloudflare runs 2 at once"),
        ];
        let mut ids = vec![];
        for (fields, _) in &cases {
            let (id, queued) = served.github.queue("test", "superci", served.now_ms());
            ids.push(id);
            assert_eq!(served.webhook(queued), 202);
            let key = format!("job:{id}");
            let mut m = served.store.0.lock().unwrap();
            let mut j: serde_json::Value = serde_json::from_str(&m[&key]).unwrap();
            for (k, v) in fields.as_object().unwrap() { j[k] = v.clone() }
            m.insert(key, j.to_string());
        }
        let (mut child, link) = dashboard(&home);
        let (_, page) = part(&link, "/?p=jobs&fragment=1");
        let said = words(&page);
        for (_, want) in &cases { assert!(said.contains(want), "no {want:?} in: {said}") }
        assert_eq!(said.matches("On a spot machine again").count(), 2, "{said}");
        assert_eq!(said.matches("run again after AWS took back its spot machine").count(), 2, "{said}");
        // A job's own page says why it failed, or why it waits.
        for (i, want) in [(7, "Cloudflare did not start its container"), (cases.len() - 1, "Cloudflare runs 2 at once")] {
            let (_, page) = part(&link, &format!("/?p=job&id={}&fragment=1", ids[i]));
            assert!(words(&page).contains(want), "{want}: {}", words(&page));
        }
        // The spot limit reached in the last day: said once above the jobs, with where to ask AWS for more.
        assert!(said.contains("in the last day ran on on-demand machines, at about twice the price: your AWS account's spot limit in N. Virginia was reached") && page.contains("servicequotas"), "{said}");
        let _ = child.kill();
        let _ = std::fs::remove_dir_all(&home);
        super::seen_in_browser(&served);
    }

    #[test]
    fn github_jobs_reach_the_cache_with_githubs_own_token_each_branch_in_its_own_scope_reading_the_default_branchs() {
        // A control plane with its own AWS account; a machine's link to its cache; GitHub's App connected.
        let served = serve(|c| {
            (c.aws_own, c.aws_own_creds) = (Some(("111111111111".into(), "us-east-1".into())), Some(superci_core::aws::Credentials { access_key_id: "ASIAX".into(), secret_access_key: "secret".into(), session_token: Some("tok".into()), expires_at_ms: u64::MAX }));
        });
        served.connect_github();
        let link = "machine-cache-link-000000000002";
        served.store.0.lock().unwrap().insert(format!("cachem:{link}"), serde_json::json!({ "region": "us-east-1", "arch": "x86_64", "until": u64::MAX / 4, "runner": "a-runner" }).to_string());
        let post = |what: &str, body: serde_json::Value| -> (u16, serde_json::Value) {
            let agent: ureq::Agent = ureq::Agent::config_builder().http_status_as_error(false).build().into();
            let mut r = agent.post(&format!("{}/cache/{link}/{what}", served.url)).send(body.to_string()).unwrap();
            (r.status().as_u16(), serde_json::from_str(&r.body_mut().read_to_string().unwrap_or_default()).unwrap_or_default())
        };
        let put = |url: &str, bytes: &[u8]| {
            let agent: ureq::Agent = ureq::Agent::config_builder().http_status_as_error(false).build().into();
            agent.put(&format!("{}/{}", served.aws.url, url.trim_start_matches("https://"))).send(bytes).unwrap().status().as_u16()
        };
        // GitHub's tokens: a push to main (writes main's), a pull request's branch (writes its own, reads main's), a job
        // that only reads, another repository's.
        let exp = 4_102_444_800u64;
        let token = |repo: u64, ac: serde_json::Value| superci_testkit::GitHub::job_token(&serde_json::json!({ "repository_id": repo.to_string(), "ac": ac.to_string(), "exp": exp }));
        let main = token(197814629, serde_json::json!([{ "Scope": "refs/heads/main", "Permission": 3 }]));
        let branch = token(197814629, serde_json::json!([{ "Scope": "refs/heads/fix", "Permission": 3 }, { "Scope": "refs/heads/main", "Permission": 1 }]));
        let reads = token(197814629, serde_json::json!([{ "Scope": "refs/heads/main", "Permission": 1 }]));
        let other = token(5, serde_json::json!([{ "Scope": "refs/heads/main", "Permission": 3 }]));
        let version = "c".repeat(64);
        let save = |t: &str, key: &str, bytes: &[u8]| {
            assert_eq!(post("begin", serde_json::json!({ "token": t, "key": key, "version": version })).1["ok"], true, "{key}");
            let (_, signed) = post("sign", serde_json::json!({ "token": t, "key": key, "version": version, "names": ["p00000"] }));
            assert_eq!(put(signed["urls"][0].as_str().unwrap(), bytes), 200);
            assert_eq!(post("finish", serde_json::json!({ "token": t, "key": key, "version": version, "pieces": [["p00000", bytes.len()]] })).1["ok"], true);
        };
        let found = |t: &str, key: &str| post("lookup", serde_json::json!({ "token": t, "key": key, "restore_keys": ["deps-"], "version": version })).1;
        save(&main, "deps-main", b"main's");
        // The branch reads main's; saves its own, which it then finds first; main does not see the branch's.
        assert_eq!(found(&branch, "deps-x")["key"], "deps-main");
        save(&branch, "deps-fix", b"fix's");
        assert_eq!(found(&branch, "deps-x")["key"], "deps-fix");
        assert_eq!(found(&main, "deps-fix")["key"], "deps-main", "(main reads only its own)");
        // One that only reads: finds, may not save. Another repository: finds nothing.
        assert_eq!(found(&reads, "deps-main")["hit"], true);
        let (status, said) = post("begin", serde_json::json!({ "token": reads, "key": "deps-new", "version": version }));
        assert_eq!((status, said["why"].as_str()), (403, Some("this job may only read the cache")), "{said}");
        assert_eq!(found(&other, "deps-main")["hit"], false);
        // Its artifacts are GitHub's to keep.
        let (status, said) = post("artifact-begin", serde_json::json!({ "token": main, "name": "report" }));
        assert_eq!((status, said["why"].as_str()), (403, Some("a job of GitHub's keeps its artifacts at GitHub")), "{said}");
        // Refused: a token past its time, one with no repository, one not GitHub's (signed by a key GitHub has not).
        let late = superci_testkit::GitHub::job_token(&serde_json::json!({ "repository_id": "197814629", "ac": "[]", "exp": 1 }));
        let nameless = superci_testkit::GitHub::job_token(&serde_json::json!({ "ac": "[]", "exp": exp }));
        let mut forged = main.clone();
        forged.truncate(forged.len() - 4);
        forged.push_str("AAAA");
        for (t, why) in [(late.as_str(), "expired"), (nameless.as_str(), "a token without its repository"), (forged.as_str(), "bad signature"), ("not.a.token", "not a job's token")] {
            let (status, said) = post("lookup", serde_json::json!({ "token": t, "key": "deps-main", "version": version }));
            assert_eq!((status, said["why"].as_str()), (403, Some(why)), "{said}");
        }
        // Versions GitHub's client would not send, a token whose repository is no number, a save finished with no
        // pieces, a machine's link too short: refused.
        for v in ["".to_string(), "a-b".to_string(), "c".repeat(129)] {
            assert_eq!(post("begin", serde_json::json!({ "token": main, "key": "deps-v", "version": v })).0, 400, "{v:?}");
        }
        let odd = superci_testkit::GitHub::job_token(&serde_json::json!({ "repository_id": "abc", "ac": "[]", "exp": exp }));
        assert_eq!(post("lookup", serde_json::json!({ "token": odd, "key": "deps-main", "version": version })).1["why"], "a token without its repository");
        assert_eq!(post("begin", serde_json::json!({ "token": main, "key": "deps-empty", "version": version })).1["ok"], true);
        assert!(post("finish", serde_json::json!({ "token": main, "key": "deps-empty", "version": version, "pieces": [] })).0 >= 400);
        let agent: ureq::Agent = ureq::Agent::config_builder().http_status_as_error(false).build().into();
        assert_eq!(agent.post(&format!("{}/cache/short/lookup", served.url)).send("{}").unwrap().status().as_u16(), 404);
        // GitHub's keys were asked for once, and kept.
        assert_eq!(served.github.jwks_asked(), 1);
        // The cache's list: the repository by its name, as GitHub has it; its entries by scope; a branch's taken out.
        let home = signed_in(&served);
        let (_, listed) = json(&home, &["caches", "list"]);
        assert_eq!((listed["repos"][0]["name"].as_str(), listed["repos"][0]["entries"].as_u64()), (Some("acme/app"), Some(2)), "{listed}");
        let (_, entries) = json(&home, &["caches", "list", "--repo=acme/app"]);
        let scopes: Vec<&str> = entries["entries"].as_array().unwrap().iter().filter_map(|e| e["scope"].as_str()).collect();
        assert!(scopes.contains(&"refs/heads/main") && scopes.contains(&"refs/heads/fix"), "{entries}");
        let o = superci(&home, &["caches", "delete", "--repo=acme/app", "deps-fix", "--scope=refs/heads/fix", "--confirm"]);
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        assert_eq!(found(&branch, "deps-x")["key"], "deps-main");
        // By the repository's id too.
        let o = superci(&home, &["caches", "delete", "--repo=gh197814629", "deps-none", "--confirm"]);
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        let o = superci(&home, &["caches", "delete", "--repo=acme/app", "--confirm"]);
        assert!(o.status.success() && String::from_utf8_lossy(&o.stdout).contains("acme/app's cache entries are out."), "{}", String::from_utf8_lossy(&o.stderr));
        assert_eq!(found(&main, "deps-main")["hit"], false);
        let (_, listed) = json(&home, &["caches", "list"]);
        assert_eq!(listed["repos"], serde_json::json!([]), "{listed}");
        super::seen_in_browser(&served);
    }

    #[test]
    fn the_runs_page_says_how_each_run_stands_how_long_it_took_and_the_job_it_waited_for() {
        let served = serve(|_| {});
        served.connect_github();
        let home = signed_in(&served);
        let now = served.now_ms();
        // Runs of acme/app as the control plane keeps their jobs: (run, attempt, workflow, each job's name, state,
        // conclusion, minutes from queued to started and to ended).
        type Job<'a> = (&'a str, &'a str, Option<&'a str>, u64, u64);
        let runs: Vec<(u64, u64, &str, Vec<Job>)> = vec![
            (9001, 1, "Build", vec![("lint", "done", Some("success"), 0, 1), ("e2e", "done", Some("success"), 2, 7)]),
            (9002, 1, "Tests", vec![("unit", "done", Some("failure"), 0, 2), ("types", "done", Some("success"), 0, 1)]),
            (9003, 1, "Deploy", vec![("ship", "done", Some("cancelled"), 0, 1)]),
            (9004, 1, "Docs", vec![("pages", "done", Some("skipped"), 0, 0), ("site", "done", Some("success"), 0, 1)]),
            (9005, 1, "Nightly", vec![("sweep", "done", None, 0, 3)]),
            (9006, 2, "Retry", vec![("flaky", "done", Some("success"), 0, 1)]),
            (9007, 1, "", vec![("old", "done", Some("success"), 0, 1)]),
            (9008, 1, "Live", vec![("long", "running", None, 0, 0)]),
        ];
        for (i, (run, attempt, workflow, jobs)) in runs.iter().enumerate() {
            for (name, state, conclusion, started, ended) in jobs {
                let (id, queued) = served.github.queue(name, "superci", now);
                assert_eq!(served.webhook(queued), 202);
                let key = format!("job:{id}");
                let mut m = served.store.0.lock().unwrap();
                let mut j: serde_json::Value = serde_json::from_str(&m[&key]).unwrap();
                let at = now - (10 - i as u64) * 3_600_000;
                for (k, v) in [("run_id", serde_json::json!(run)), ("attempt", serde_json::json!(attempt)), ("workflow", serde_json::json!(workflow)), ("name", serde_json::json!(name)), ("state", serde_json::json!(state)),
                    ("conclusion", serde_json::json!(conclusion)), ("at_ms", serde_json::json!(at)), ("started_ms", serde_json::json!(at + started * 60_000))] { j[k] = v }
                if *state == "done" { j["ended_ms"] = serde_json::json!(at + ended * 60_000) }
                m.insert(key, j.to_string());
            }
        }
        let (mut child, link) = dashboard(&home);
        let (_, page) = part(&link, "/?p=runs&fragment=1");
        let said = words(&page);
        let row = |run: u64| { let at = page.find(&format!("id={run}&amp;")).unwrap_or_else(|| panic!("no run {run}: {said}")); let end = page[at..].find("</a>").map(|e| at + e).unwrap_or(page.len()); words(&page[at..end]) };
        // How each stands: passed (skipped jobs too), failed, cancelled, done without a word of how, running.
        for (run, word) in [(9001, "Passed"), (9002, "Failed"), (9003, "Cancelled"), (9004, "Passed"), (9005, "Done"), (9008, "Running")] { assert!(row(run).contains(word), "{run} {word}: {}", row(run)) }
        // The job a run of several waited for, and how long it waited for its machine; a run of one says none.
        assert!(row(9001).contains("Last to end: e2e, 5m after 2m waiting for a machine") && row(9001).contains("7m"), "{}", row(9001));
        assert!(row(9002).contains("Last to end: unit, 2m") && !row(9002).contains("waiting for a machine"), "{}", row(9002));
        assert!(!row(9003).contains("Last to end"), "{}", row(9003));
        // A second attempt says so; a run with no workflow's name goes by its repository.
        assert!(row(9006).contains("Retry") && row(9006).contains("attempt 2"), "{}", row(9006));
        assert!(row(9007).contains("app acme"), "{}", row(9007));
        // A run's own page: its jobs, and GitHub's page of that attempt.
        let (_, run) = part(&link, "/?p=run&repo=acme%2Fapp&id=9006&attempt=2&fragment=1");
        assert!(run.contains("https://github.com/acme/app/actions/runs/9006/attempts/2") && words(&run).contains("flaky"), "{}", words(&run));
        let _ = child.kill();
        let _ = std::fs::remove_dir_all(&home);
        super::seen_in_browser(&served);
    }

    #[test]
    fn a_github_job_cancelled_while_it_waits_for_room_or_before_its_first_word_came_starts_nothing() {
        // Room for one job at a time on its containers.
        let served = serve(|c| { c.routing.order = vec![superci_core::plane::Pool { max_jobs: Some(1), ..superci_core::plane::Pool::of("cloudflare") }]; });
        served.connect_github();
        let state = |id: u64| served.store.under(&format!("job:{id}")).into_iter().next().map(|(_, j)| j).unwrap_or_default();
        let (a, queued) = served.github.queue("lint", "superci", served.now_ms());
        assert_eq!(served.webhook(queued), 202);
        // A second waits for the room the first has; cancelled, it ends here, and the upkeep starts nothing for it.
        let (b, queued) = served.github.queue("docs", "superci", served.now_ms());
        assert_eq!(served.webhook(queued), 202);
        assert_eq!(state(b)["state"], "waiting", "{}", state(b));
        assert_eq!(served.webhook(served.github.cancel(b, served.now_ms())), 202);
        assert_eq!(state(b)["state"], "cancelled", "{}", state(b));
        assert_eq!(served.webhook(served.github.start(a)), 202);
        assert_eq!(served.webhook(served.github.finish(a, "success", &github_log(), served.now_ms())), 202);
        served.upkeep();
        assert_eq!(served.machines.0.lock().unwrap().len(), 1, "only the first's machine");
        // Its end came before its queued event (GitHub sends a job's events side by side): the queued one starts nothing.
        let (c, late) = served.github.queue("deploy", "superci", served.now_ms());
        assert_eq!(served.webhook(served.github.cancel(c, served.now_ms())), 202);
        served.webhook(late);
        assert!(!["launching", "launched", "running"].contains(&state(c)["state"].as_str().unwrap_or_default()), "{}", state(c));
        assert_eq!(served.machines.0.lock().unwrap().len(), 1, "no machine for it");
        super::seen_in_browser(&served);
    }

    #[test]
    fn a_job_past_a_spend_limit_fails_saying_which_limit_and_one_under_them_starts() {
        // $80 spent today on Cloudflare's containers (the only provider); a job queued past each limit.
        let refused = |set: fn(&mut superci_core::plane::Routing)| -> serde_json::Value {
            let served = serve(move |c| { c.routing.order = vec![superci_core::plane::Pool::of("cloudflare")]; set(&mut c.routing) });
            served.connect_github();
            let now = served.now_ms();
            {
                let mut m = served.store.0.lock().unwrap();
                m.insert("job:999".into(), serde_json::json!({ "job_id": 999, "run_id": 1, "repo": "acme/app", "state": "done", "cloud": "cloudflare", "at_ms": now - 60_000, "started_ms": now - 50_000, "ended_ms": now - 10_000, "cost_usd": 80.0, "installation_id": 1, "seen_in_progress": true }).to_string());
                m.insert(format!("day:{:06}", now / 86_400_000), serde_json::json!({ "spend": { "cloudflare": 80.0 }, "hours": [] }).to_string());
            }
            let (id, queued) = served.github.queue("test", "superci", now);
            served.webhook(queued);
            // (It fails at GitHub with the reason: a small machine's runner takes it and says why.)
            let j = served.store.under(&format!("job:{id}")).into_iter().next().map(|(_, j)| j).unwrap_or_default();
            assert!(j["state"] == "failed" && j["failed_fast"] == true, "{j}");
            j
        };
        let said = |j: &serde_json::Value| format!("{} {}", j["state"], j["error"]);
        let j = refused(|r| r.monthly_usd = Some(60.0));
        assert!(said(&j).contains("reached its limit of $60 a month ($80.00 spent on machines)"), "{}", said(&j));
        let j = refused(|r| r.daily_usd = Some(50.0));
        assert!(said(&j).contains("reached its limit of $50 a day ($80.00 spent on machines today)"), "{}", said(&j));
        let j = refused(|r| r.order[0].monthly_usd = Some(40.0));
        assert!(said(&j).contains("Cloudflare reached its $40 a month"), "{}", said(&j));
        let j = refused(|r| r.order[0].off = true);
        assert!(said(&j).contains("Cloudflare is turned off"), "{}", said(&j));
        // Under each: it starts.
        let served = serve(|c| { c.routing.order = vec![superci_core::plane::Pool { monthly_usd: Some(500.0), ..superci_core::plane::Pool::of("cloudflare") }]; c.routing.monthly_usd = Some(1000.0); c.routing.daily_usd = Some(900.0); });
        served.connect_github();
        let (id, queued) = served.github.queue("test", "superci", served.now_ms());
        served.webhook(queued);
        let j = served.store.under(&format!("job:{id}")).into_iter().next().map(|(_, j)| j).unwrap_or_default();
        assert!(j["state"] == "launched" && j["failed_fast"].is_null(), "{j}");
        super::seen_in_browser(&served);
    }

    #[test]
    fn a_further_organization_is_removed_from_the_dashboard_its_app_uninstalled_and_forgotten() {
        let served = serve(|_| {});
        served.connect_github();
        // A second organization's App beside acme's (as the dashboard keeps a further one).
        let mut globex = superci_testkit::GitHub::app();
        for (k, v) in [("id", serde_json::json!(2002)), ("slug", serde_json::json!("superci-globex")), ("owner", serde_json::json!("globex"))] { globex[k] = v }
        served.aws.put(&format!("/superci/{}/GITHUB_APP_2002", superci_testkit::PLANE), &globex.to_string());
        let home = signed_in(&served);
        let (mut child, link) = dashboard(&home);
        // (The control plane has both.)
        let connections = json(&home, &["connections", "list"]).1;
        assert!(connections["github"].as_array().unwrap().iter().any(|a| a["id"] == 2002 && a["owner"] == "globex"), "{connections}");
        // The first organization is not one to remove here (that is stopping SuperCI); one not here neither.
        for app in ["1001", "3003"] {
            let (status, _, said) = common::post(&link, "/github/remove", &[("app", app)]);
            assert!(status >= 400 && words(&said).contains("That organization is not one added here"), "{app}: {status} {}", words(&said));
        }
        // Removed: its App uninstalled at GitHub, its secret gone, and what to do on GitHub said.
        let (status, to, _) = common::post(&link, "/github/remove", &[("app", "2002")]);
        assert!(status == 303 && to.contains("p=repos"), "{status} {to}");
        assert!(served.github.uninstalled());
        assert!(served.aws.settings(superci_testkit::PLANE).iter().all(|(k, _)| k != "GITHUB_APP_2002"));
        let (_, page) = part(&link, "/?p=repos&fragment=1");
        assert!(words(&page).contains("Removed globex. Its App") && words(&page).contains("is uninstalled; delete the App itself on GitHub"), "{}", words(&page));
        let _ = child.kill();
        let _ = std::fs::remove_dir_all(&home);
        super::seen_in_browser(&served);
    }

    #[test]
    fn a_running_github_jobs_lines_as_its_machine_sends_them_are_read_before_github_has_its_log() {
        // Without a bucket (kept in the store), and with one (kept there).
        for bucket in [false, true] {
            let served = serve(move |c| {
                if bucket { (c.aws_own, c.aws_own_creds) = (Some(("111111111111".into(), "us-east-1".into())), Some(superci_core::aws::Credentials { access_key_id: "ASIAX".into(), secret_access_key: "secret".into(), session_token: Some("tok".into()), expires_at_ms: u64::MAX })); }
                c.routing.order = vec![superci_core::plane::Pool::of("cloudflare")];
            });
            if bucket { served.github.put_object(&format!("{}/{}", superci_core::cache::bucket(superci_testkit::PLANE, "us-east-1"), superci_core::cache::program_key("x86_64")), b"program") }
            served.connect_github();
            let home = signed_in(&served);
            let (id, queued) = served.github.queue("test", "superci", served.now_ms());
            assert_eq!(served.webhook(queued), 202);
            assert_eq!(served.webhook(served.github.start(id)), 202);
            // (Its machine's own link back, as the control plane writes one into an AWS machine's start-up script.)
            let hello = "machine-link-0123456789ab".to_string();
            let runner = {
                let key = format!("job:{id}");
                let mut m = served.store.0.lock().unwrap();
                let mut j: serde_json::Value = serde_json::from_str(&m[&key]).unwrap();
                j["hello"] = serde_json::json!(hello);
                m.insert(key, j.to_string());
                j["runner"].as_str().unwrap().to_string()
            };
            // Its runner's pages of a step's log, as its machine sends them (the second from where the first ended).
            let step = "aaaaaaaa-0000-0000-0000-000000000001";
            let first = "2026-10-15T10:00:01.0000000Z ##[group]Run make test\n2026-10-15T10:00:02.0000000Z test 1 passed\n";
            let second = "2026-10-15T10:00:03.0000000Z test 2 passed\n";
            let agent: ureq::Agent = ureq::Agent::config_builder().http_status_as_error(false).build().into();
            for (at, page) in [(0, first), (first.len(), second)] {
                let r = agent.post(&format!("{}/hello?runner={runner}&t={hello}&live=timeline_{step}_1.log&at={at}", served.url)).send(page).unwrap();
                assert_eq!(r.status().as_u16(), 200);
            }
            // An empty page is nothing to keep; a link not its own is not taken.
            assert_eq!(agent.post(&format!("{}/hello?runner={runner}&t={hello}&live=timeline_{step}_2.log&at=0", served.url)).send("").unwrap().status().as_u16(), 200);
            assert_eq!(agent.post(&format!("{}/hello?runner={runner}&t=not-its-link-at-all&live=x_1.log", served.url)).send("x").unwrap().status().as_u16(), 404);
            assert_eq!(served.github.objects().iter().filter(|(k, _)| k.contains("/live/")).count(), if bucket { 2 } else { 0 }, "{:?}", served.github.objects());
            let o = superci(&home, &["jobs", "retrieve", &id.to_string()]);
            let said = String::from_utf8_lossy(&o.stdout);
            assert!(o.status.success() && said.contains("test 1 passed") && said.contains("test 2 passed"), "bucket {bucket}: {said}{}", String::from_utf8_lossy(&o.stderr));
            let _ = std::fs::remove_dir_all(&home);
            super::seen_in_browser(&served);
        }
    }

    #[test]
    fn what_github_says_of_the_events_it_sent_is_read_and_one_it_could_not_deliver_is_sent_again_once() {
        let served = serve(|_| {});
        served.connect_github();
        let home = signed_in(&served);
        let id = ran(&served, "success");
        // Listed, newest first: each event, its action, and what the control plane answered.
        let o = superci(&home, &["deliveries", "list"]);
        let said = String::from_utf8_lossy(&o.stdout);
        assert!(o.status.success() && said.contains("workflow_job") && said.contains("queued") && said.contains("completed") && said.contains("202"), "{said}");
        // One whole: where it was sent, the job it was of, and the answer.
        let (_, listed) = json(&home, &["deliveries", "list"]);
        let first = listed[0]["deliveries"].as_array().unwrap().last().unwrap()["id"].as_u64().unwrap();
        let o = superci(&home, &["deliveries", "retrieve", &first.to_string()]);
        let said = String::from_utf8_lossy(&o.stdout);
        assert!(o.status.success() && said.contains(&format!("sent to {}/webhook", served.url)) && said.contains(&format!("job {id} test")) && said.contains("answer:"), "{said}");
        let o = superci(&home, &["deliveries", "retrieve", "1"]);
        assert!(String::from_utf8_lossy(&o.stdout).contains("GitHub would not say"), "{}", String::from_utf8_lossy(&o.stdout));
        // One GitHub could not deliver (the control plane was restarting): asked to be sent again, once.
        let (other, queued) = served.github.queue("lint", "superci", served.now_ms());
        served.github.delivered(&queued.0, 0, "", &format!("{}/webhook", served.url), served.now_ms());
        let failed = served.github.redelivered().len();
        served.upkeep();
        served.later(120_000);
        served.upkeep();
        let asked = served.github.redelivered();
        assert_eq!(asked.len(), failed + 1, "{asked:?} (job {other})");
        super::seen_in_browser(&served);
    }

    #[test]
    fn usage_says_what_jobs_cost_beside_githubs_runners_how_many_did_not_run_and_the_wait_for_a_machine() {
        let served = serve(|_| {});
        served.connect_github();
        let home = signed_in(&served);
        // A job that waited half a minute for its machine and ran ten minutes; one asking for what nothing here runs.
        let (id, queued) = served.github.queue("test", "superci", served.now_ms());
        assert_eq!(served.webhook(queued), 202);
        served.later(30_000);
        assert_eq!(served.webhook(served.github.start(id)), 202);
        served.later(600_000);
        assert_eq!(served.webhook(served.github.finish(id, "success", &github_log(), served.now_ms())), 202);
        let (_, refused) = served.github.queue("gpu", "superci/gpu=h100", served.now_ms());
        assert_eq!(served.webhook(refused), 202);
        let (code, v) = json(&home, &["usage", "retrieve", "--period=day"]);
        assert_eq!(code, 0, "{v}");
        let o = superci(&home, &["usage", "retrieve"]);
        let said = String::from_utf8_lossy(&o.stdout);
        // (Half a minute, and the moment the test itself took.)
        assert!(said.contains("2 jobs, 1 ran, 1 did not") && said.contains("saved") && (said.contains("Median wait for a machine: 30 s") || said.contains("Median wait for a machine: 31 s")), "{said}");
        super::seen_in_browser(&served);
    }

    #[test]
    fn a_personal_accounts_app_registers_its_runners_in_the_repository_and_its_manifest_goes_to_the_persons_settings() {
        // The App of a person's own account (not an organization's).
        let served = serve(|_| {});
        let mut app = superci_testkit::GitHub::app();
        app["owner_is_org"] = serde_json::json!(false);
        served.aws.put(&format!("/superci/{}/GITHUB_APP", superci_testkit::PLANE), &app.to_string());
        let (id, queued) = served.github.queue("test", "superci", served.now_ms());
        assert_eq!(served.webhook(queued), 202);
        let asked = || served.github.asked.lock().unwrap().clone();
        assert!(asked().iter().any(|a| a == "POST https://api.github.com/repos/acme/app/actions/runners/generate-jitconfig"), "{:?}", asked());
        // Cancelled before its runner took it: its runner taken back from the repository.
        assert_eq!(served.webhook(served.github.cancel(id, served.now_ms())), 202);
        assert!(asked().iter().any(|a| a.starts_with("DELETE https://api.github.com/repos/acme/app/actions/runners/")), "{:?}", asked());
        // Made from Repositories for a person: GitHub's page for an App of their own.
        let home = signed_in(&served);
        let (_child, link) = dashboard(&home);
        let (status, _, form) = common::post(&link, "/github/start", &[("gh-where", "public"), ("host", ""), ("login", "octocat")]);
        assert!(status == 200 && form.contains("https://github.com/settings/apps/new?state="), "{status} {}", words(&form));
        let (status, _, said) = common::post(&link, "/github/start", &[("gh-where", "public"), ("host", ""), ("login", "nobody-here")]);
        assert!(status == 400 && words(&said).contains("There is no GitHub organization or user named nobody-here"), "{status} {}", words(&said));
        super::seen_in_browser(&served);
    }

    #[test]
    fn a_github_jobs_lines_go_to_the_step_that_was_going_and_a_step_of_the_same_second_from_its_own_first_line() {
        let served = serve(|_| {});
        served.connect_github();
        let home = signed_in(&served);
        let (id, queued) = served.github.queue("test", "superci", served.now_ms());
        assert_eq!(served.webhook(queued), 202);
        assert_eq!(served.webhook(served.github.start(id)), 202);
        let t = |s: u32, text: &str| format!("2026-10-15T10:00:{s:02}.5000000Z {text}");
        let log = [t(0, "##[group]Set up job"), t(0, "Current runner version: '2.329.0'"), t(1, "##[endgroup]"), t(1, "set up, still"), t(1, "##[group]Run make test"), t(2, "test 1 passed"),
            t(3, "Post job cleanup."), t(3, "removing the checkout"), t(3, "Cleaning up orphan processes")].join("\n");
        assert_eq!(served.webhook(served.github.finish(id, "success", &log, served.now_ms())), 202);
        let step = |n: u64, name: &str, from: u32, to: u32| serde_json::json!({ "name": name, "status": "completed", "conclusion": "success", "number": n,
            "started_at": format!("2026-10-15T10:00:{from:02}Z"), "completed_at": format!("2026-10-15T10:00:{to:02}Z") });
        served.github.set_steps(id, serde_json::json!([step(1, "Set up job", 0, 1), step(2, "Run make test", 1, 3), step(3, "Post Run actions/checkout@v5", 3, 3), step(4, "Complete job", 3, 4)]));
        let (_child, link) = dashboard(&home);
        let (_, view) = part(&link, &format!("/job/view?id={id}"));
        let view: serde_json::Value = serde_json::from_str(&view).unwrap_or_else(|_| panic!("{view}"));
        let lines = |name: &str| view["steps"].as_array().unwrap().iter().find(|s| s["name"] == name).map(|s| s["lines"].to_string()).unwrap_or_else(|| panic!("no {name}: {view}"));
        // A line of the second a step began goes to it only from its heading on.
        assert!(lines("Set up job").contains("set up, still") && !lines("Set up job").contains("test 1 passed"), "{}", lines("Set up job"));
        assert!(lines("Run make test").contains("test 1 passed") && !lines("Run make test").contains("removing the checkout"), "{}", lines("Run make test"));
        assert!(lines("Post Run actions/checkout@v5").contains("removing the checkout"), "{}", lines("Post Run actions/checkout@v5"));
        assert!(lines("Complete job").contains("Cleaning up orphan processes"), "{}", lines("Complete job"));
        super::seen_in_browser(&served);
    }

}

/// Browser tests of gitlab: what a person sees of them in the dashboard.
mod gitlab_in_browser {
    use super::*;
    use common::{json, part, superci, words};
    use superci_testkit::{gitlab, Served};

    #[test]
    fn gitlab_is_connected_and_a_project_switched_on_and_off_in_the_browser() {
        let Some(mut b) = superci_testkit::Browser::open(1300, 900) else { eprintln!("no Chrome here: skipped"); return };
        let served = serve(|_| {});
        let home = signed_in(&served);
        let (mut child, link) = dashboard(&home);
        let shows = |words: &str| format!("document.querySelector('main') && document.querySelector('main').innerText.includes({})", serde_json::json!(words));
        let enabled = || json(&home, &["projects", "list"]).1["projects"][0]["enabled"].as_bool();
        b.goto(&link);
        b.wait(&shows("Usage"));
        // Repositories: GitLab's Connect, its token pasted in the dialog (GitLab.com).
        b.click("nav a", "Repositories");
        b.wait(r#"[...document.querySelectorAll('main button')].some(x => x.textContent === 'Connect' && (x.getAttribute('onclick') || '').includes('gl-add'))"#);
        b.eval(r#"[...document.querySelectorAll('main button')].find(x => x.textContent === 'Connect' && (x.getAttribute('onclick') || '').includes('gl-add')).click()"#);
        b.wait("document.getElementById('gl-add').open");
        b.fill("#gl-add input[name=token]", gitlab::TOKEN);
        b.eval("document.querySelector('#gl-add input[name=token]').form.requestSubmit()");
        // Its page: its projects, each off until turned on (a webhook for its job events), and off again.
        b.wait(&shows("acme/app"));
        b.wait(r#"[...document.querySelectorAll('main form[action="/gitlab/project"] button')].some(x => x.textContent === 'Turn on')"#);
        b.eval(r#"[...document.querySelectorAll('main form[action="/gitlab/project"] button')].find(x => x.textContent === 'Turn on').click()"#);
        let until = |what: &str, holds: &dyn Fn() -> bool| { let end = std::time::Instant::now() + std::time::Duration::from_secs(15); while !holds() { assert!(std::time::Instant::now() < end, "{what}"); std::thread::sleep(std::time::Duration::from_millis(200)) } };
        until("the project on", &|| enabled() == Some(true));
        assert!(served.gitlab.asked.lock().unwrap().iter().any(|a| a == &format!("POST https://gitlab.com/api/v4/projects/{}/hooks", gitlab::PROJECT)));
        b.wait(r#"[...document.querySelectorAll('main form[action="/gitlab/project"] button')].some(x => x.textContent === 'Turn off') && !document.body.classList.contains('busy')"#);
        b.eval(r#"[...document.querySelectorAll('main form[action="/gitlab/project"] button')].find(x => x.textContent === 'Turn off').click()"#);
        until("the project off", &|| enabled() == Some(false));
        let _ = child.kill();
        let _ = std::fs::remove_dir_all(&home);
    }
    /// GitLab's log of a job, as its trace has it (sections are its steps).
    fn gitlab_log() -> String {
        ["\u{1b}[0KRunning with gitlab-runner 19.4.1", "\u{1b}[0Ksection_start:1760455360:get_sources\r\u{1b}[0K\u{1b}[36;1mGetting source from Git repository\u{1b}[0;m", "Fetching changes...",
            "\u{1b}[0Ksection_end:1760455363:get_sources\r\u{1b}[0K", "\u{1b}[0Ksection_start:1760455363:step_script\r\u{1b}[0K\u{1b}[36;1mExecuting \"step_script\" stage\u{1b}[0;m",
            "$ make test", "\u{1b}[32mall 2 passed\u{1b}[0m", "\u{1b}[0Ksection_end:1760455370:step_script\r\u{1b}[0K", "\u{1b}[32;1mJob succeeded\u{1b}[0;m"].join("\n")
    }

    /// A job of `acme/app` tagged `superci`, from pending to ended as GitLab tells it: its id.
    fn ran(served: &Served, status: &str) -> u64 {
        let (id, pending) = served.gitlab.pending("test", &["superci"], served.now_ms());
        assert!(served.gitlab_webhook(pending) < 300);
        assert_eq!(served.machines.0.lock().unwrap().len(), 1, "its machine");
        let job = served.store.under(&format!("job:gl{id}")).into_iter().next().expect("the job kept").1;
        assert_eq!((job["provider"].as_str(), job["state"].as_str()), (Some("gitlab"), Some("launched")), "{job}");
        assert!(served.gitlab.asked.lock().unwrap().iter().any(|a| a == "POST https://gitlab.com/api/v4/user/runners"), "a runner for it");
        assert!(served.gitlab_webhook(served.gitlab.running(id, served.now_ms())) < 300);
        assert_eq!(served.store.under(&format!("job:gl{id}"))[0].1["state"], "running");
        assert!(served.gitlab_webhook(served.gitlab.finish(id, status, &gitlab_log(), served.now_ms())) < 300);
        id
    }

    #[test]
    fn a_gitlab_job_runs_on_its_machine_and_is_read_in_the_cli_and_the_dashboard() {
        let served = serve(|_| {});
        served.connect_gitlab();
        let home = signed_in(&served);
        let id = ran(&served, "success");
        let job = served.store.under(&format!("job:gl{id}"))[0].1.clone();
        assert_eq!(job["state"], "done", "{job}");
        // Its runner is taken back once its job is over.
        assert!(served.gitlab.asked.lock().unwrap().iter().any(|a| a.starts_with("DELETE https://gitlab.com/api/v4/runners/") || a.starts_with("PUT https://gitlab.com/api/v4/runners/")), "{:?}", served.gitlab.asked.lock().unwrap());

        let jobs = json(&home, &["jobs", "list"]).1;
        let listed = &jobs["jobs"][0];
        assert_eq!((listed["job"].as_str(), listed["ended"].as_str()), (Some("test"), Some("passed")), "{jobs}");
        assert_eq!(listed["link"].as_str(), Some(format!("https://gitlab.com/acme/app/-/jobs/{id}").as_str()), "{jobs}");
        let o = superci(&home, &["jobs", "retrieve", &id.to_string()]);
        let said = String::from_utf8_lossy(&o.stdout);
        assert!(o.status.success() && said.contains("all 2 passed"), "{said}{}", String::from_utf8_lossy(&o.stderr));

        let (mut child, link) = dashboard(&home);
        let (_, page) = part(&link, &format!("/?p=job&id={id}&gl=&fragment=1"));
        assert!(words(&page).contains("On GitLab") && page.contains(&format!("https://gitlab.com/acme/app/-/jobs/{id}")), "{}", words(&page));
        let (_, view) = part(&link, &format!("/job/view?id={id}"));
        let view: serde_json::Value = serde_json::from_str(&view).unwrap();
        let steps: Vec<&str> = view["steps"].as_array().unwrap().iter().filter_map(|s| s["name"].as_str()).collect();
        assert!(steps.contains(&"Getting source from Git repository") && steps.contains(&"Executing \"step_script\" stage"), "{view}");
        let _ = child.kill();
        let _ = std::fs::remove_dir_all(&home);
        super::seen_in_browser(&served);
    }

    #[test]
    fn gitlab_is_connected_from_the_dashboard_and_a_project_switched_on_and_off() {
        let served = serve(|_| {});
        let home = signed_in(&served);
        let (mut child, link) = dashboard(&home);
        // A token that is not valid is said as that.
        let (status, _, refused) = common::post(&link, "/gitlab/connect", &[("which", "new"), ("url", "https://gitlab.com"), ("token", "wrong")]);
        assert!(status == 400 && words(&refused).contains("the token is not valid"), "{status} {}", words(&refused));
        let (status, _, said) = common::post(&link, "/gitlab/connect", &[("which", "new"), ("url", "https://gitlab.com"), ("token", gitlab::TOKEN)]);
        assert!(status < 400, "{status} {}", words(&said));
        let kept: serde_json::Value = serde_json::from_str(&served.aws.settings(superci_testkit::PLANE).into_iter().find(|(k, _)| k == "GITLAB").unwrap().1).unwrap();
        assert_eq!((kept["url"].as_str(), kept["token"].as_str()), (Some("https://gitlab.com"), Some(gitlab::TOKEN)));
        // Its projects, each off until switched on: a webhook to the control plane, with the connection's secret.
        let (_, page) = part(&link, "/?p=gitlab&fragment=1");
        assert!(words(&page).contains("acme/app"), "{}", words(&page));
        let (status, _, _) = common::post(&link, "/gitlab/project", &[("gitlab", ""), ("id", &gitlab::PROJECT.to_string()), ("enabled", "true")]);
        assert!(status < 400);
        assert!(served.gitlab.asked.lock().unwrap().iter().any(|a| a == &format!("POST https://gitlab.com/api/v4/projects/{}/hooks", gitlab::PROJECT)));
        let projects = json(&home, &["projects", "list"]).1;
        assert_eq!(projects["projects"][0]["enabled"], true, "{projects}");
        common::post(&link, "/gitlab/project", &[("gitlab", ""), ("id", &gitlab::PROJECT.to_string()), ("enabled", "false")]);
        assert_eq!(json(&home, &["projects", "list"]).1["projects"][0]["enabled"], false);
        // Disconnected: its projects all off, and the connection kept as off.
        common::post(&link, "/gitlab/project", &[("gitlab", ""), ("id", &gitlab::PROJECT.to_string()), ("enabled", "true")]);
        let (status, to, _) = common::post(&link, "/gitlab/disconnect", &[("gitlab", "")]);
        assert!(status == 303 && to.contains("p=repos"), "{status} {to}");
        assert_eq!(served.aws.settings(superci_testkit::PLANE).into_iter().find(|(k, _)| k == "GITLAB").map(|(_, v)| v).as_deref(), Some("off"));
        assert!(served.gitlab.asked.lock().unwrap().iter().any(|a| a.starts_with(&format!("DELETE https://gitlab.com/api/v4/projects/{}/hooks/", gitlab::PROJECT))), "its webhook removed");
        let _ = child.kill();
        let _ = std::fs::remove_dir_all(&home);
        super::seen_in_browser(&served);
    }

    #[test]
    fn a_job_with_other_tags_is_left_alone_and_an_event_without_the_secret_refused() {
        let served = serve(|_| {});
        served.connect_gitlab();
        let (_, pending) = served.gitlab.pending("lint", &["docker"], served.now_ms());
        assert!(served.gitlab_webhook(pending.clone()) < 300);
        assert!(served.machines.0.lock().unwrap().is_empty());
        // (Sent with another secret.)
        let host = served.url.trim_start_matches("http://").to_string();
        let r = ureq::post(&format!("http://{host}/gitlab/webhook")).header("x-gitlab-token", "not-the-secret-at-all").config().http_status_as_error(false).build().send(&pending[..]).unwrap();
        assert_eq!(r.status().as_u16(), 401);
        super::seen_in_browser(&served);
    }

    #[test]
    fn a_gitlab_job_asking_for_windows_or_only_modal_is_said_to_have_nowhere_to_run() {
        // Modal's sandboxes beside Cloudflare's containers: neither runs Docker for GitLab on Windows, and Modal runs none.
        let served = serve(|c| { c.containers = true; c.launchers = vec![superci_core::plane::Launcher { cloud: "modal".into(), url: "https://acme--superci-runners.modal.run".into() }]; });
        served.connect_gitlab();
        for (tags, why) in [(&["superci/os=windows"][..], "runs no GitLab jobs on Windows"), (&["superci/provider=modal"], "Modal runs no Docker")] {
            let (id, pending) = served.gitlab.pending("test", tags, served.now_ms());
            assert!(served.gitlab_webhook(pending) < 300);
            let job = served.store.under(&format!("job:gl{id}")).into_iter().next().map(|(_, j)| j).unwrap_or_default();
            let error = job["error"].as_str().unwrap_or_default().to_string();
            assert!(error.contains(why), "{tags:?}: {job}");
        }
        assert!(served.machines.0.lock().unwrap().is_empty(), "no machine started");
        super::seen_in_browser(&served);
    }

    #[test]
    fn github_and_gitlab_jobs_side_by_side_are_each_shown_with_their_own_host_and_runs() {
        let served = serve(|_| {});
        served.connect_github();
        served.connect_gitlab();
        let home = signed_in(&served);
        // A GitLab job that failed, a GitHub job that passed.
        let gl = ran(&served, "failed");
        let (gh, queued) = served.github.queue("build", "superci", served.now_ms());
        assert_eq!(served.webhook(queued), 202);
        assert_eq!(served.webhook(served.github.start(gh)), 202);
        assert_eq!(served.webhook(served.github.finish(gh, "success", "2026-10-15T10:00:00.1000000Z ok", served.now_ms())), 202);
        let (mut child, link) = dashboard(&home);
        // Overview: the minutes as both code hosts bill them.
        let (_, page) = part(&link, "/?p=overview&fragment=1");
        assert!(words(&page).contains("as GitHub and GitLab bill them"), "{}", words(&page));
        // Runs: GitHub's run and GitLab's pipeline, each linked to its own host, one passed and one failed.
        let (_, page) = part(&link, "/?p=runs&fragment=1");
        let said = words(&page);
        assert!(said.contains("Pipeline") && said.contains("Passed") && said.contains("Failed"), "{said}");
        assert!(page.contains("/-/pipelines/") || page.contains("p=run&"), "{page}");
        // Another job of the pipeline, running: its page says it runs, and has no Cancel (a GitLab pipeline is not
        // cancelled from here yet).
        let (more, pending) = served.gitlab.pending("test 2", &["superci"], served.now_ms());
        assert!(served.gitlab_webhook(pending) < 300);
        assert!(served.gitlab_webhook(served.gitlab.running(more, served.now_ms())) < 300);
        let pipeline = served.store.under(&format!("job:gl{gl}"))[0].1["run_id"].as_u64().unwrap();
        assert_eq!(served.store.under(&format!("job:gl{more}"))[0].1["run_id"].as_u64(), Some(pipeline), "the same pipeline");
        // (The dashboard reads the control plane again every few seconds.)
        // (Asked as the page asks, every few seconds.)
        let end = std::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            std::thread::sleep(std::time::Duration::from_secs(4));
            if words(&part(&link, "/?p=jobs&fragment=1").1).contains("test 2") { break }
            assert!(std::time::Instant::now() < end, "the running job not shown");
        }
        let (_, page) = part(&link, &format!("/?p=run&repo=acme/app&id={pipeline}&attempt=0&fragment=1"));
        assert!(words(&page).contains("Running") && words(&page).contains("Jobs here 2") && !page.contains("Cancel run"), "{}", words(&page));
        let _ = child.kill();
        let _ = std::fs::remove_dir_all(&home);
        super::seen_in_browser(&served);
    }

    #[test]
    fn gitlab_jobs_cancelled_at_each_point_end_here_too_and_one_on_a_spot_machine_aws_took_back_is_run_again() {
        // Room for one job at a time on its containers, so a second one waits.
        let served = serve(|c| { c.routing.order = vec![superci_core::plane::Pool { max_jobs: Some(1), ..superci_core::plane::Pool::of("cloudflare") }]; });
        served.connect_gitlab();
        let state = |id: u64| served.store.under(&format!("job:gl{id}")).into_iter().next().map(|(_, j)| j).unwrap_or_default();
        let now = || served.now_ms();
        // Cancelled while its runner runs it: over here, its runner removed.
        let (a, pending) = served.gitlab.pending("lint", &["superci"], now());
        assert!(served.gitlab_webhook(pending) < 300);
        // A second while the first has the only room: it waits; cancelled, it ends here without a machine.
        let (b, pending) = served.gitlab.pending("docs", &["superci"], now());
        assert!(served.gitlab_webhook(pending) < 300);
        assert_eq!(state(b)["state"], "waiting", "{}", state(b));
        assert!(served.gitlab_webhook(served.gitlab.finish(b, "canceled", "", now())) < 300);
        assert_eq!(state(b)["state"], "cancelled", "{}", state(b));
        assert!(served.gitlab_webhook(served.gitlab.running(a, now())) < 300);
        let runners_before = served.gitlab.asked.lock().unwrap().iter().filter(|x| x.starts_with("DELETE https://gitlab.com/api/v4/runners/")).count();
        assert!(served.gitlab_webhook(served.gitlab.finish(a, "canceled", "", now())) < 300);
        assert_eq!((state(a)["state"].as_str(), state(a)["conclusion"].as_str()), (Some("cancelled"), Some("cancelled")), "{}", state(a));
        assert_eq!(served.gitlab.asked.lock().unwrap().iter().filter(|x| x.starts_with("DELETE https://gitlab.com/api/v4/runners/")).count(), runners_before + 1);
        // Cancelled before its runner took it: its machine is left to end (no job on it).
        let (c, pending) = served.gitlab.pending("build", &["superci"], now());
        assert!(served.gitlab_webhook(pending) < 300);
        assert_eq!(state(c)["state"], "launched", "{}", state(c));
        assert!(served.gitlab_webhook(served.gitlab.finish(c, "canceled", "", now())) < 300);
        assert_eq!(state(c)["state"], "orphan", "{}", state(c));
        // Cancelled before its first word came here (GitLab's events arrive side by side): its start, behind, starts nothing.
        let machines = served.machines.0.lock().unwrap().len();
        let (d, late) = served.gitlab.pending("deploy", &["superci"], now());
        assert!(served.gitlab_webhook(served.gitlab.finish(d, "canceled", "", now())) < 300);
        assert!(served.gitlab_webhook(late) < 300);
        assert_eq!(served.machines.0.lock().unwrap().len(), machines, "no machine for it");
        // On a spot machine AWS took back, it failed: run again (GitLab makes a new job of it), off spot machines.
        served.upkeep();
        let (e, pending) = served.gitlab.pending("test", &["superci"], now());
        assert!(served.gitlab_webhook(pending) < 300);
        assert!(served.gitlab_webhook(served.gitlab.running(e, now())) < 300);
        {
            let key = format!("job:gl{e}");
            let mut m = served.store.0.lock().unwrap();
            let mut j: serde_json::Value = serde_json::from_str(&m[&key]).unwrap();
            j["interrupted"] = serde_json::json!(true);
            m.insert(key, j.to_string());
        }
        assert!(served.gitlab_webhook(served.gitlab.finish(e, "failed", "", now())) < 300);
        let j = state(e);
        assert_eq!((j["state"].as_str(), j["error"].as_str()), (Some("failed"), Some("AWS took back its spot machine; SuperCI ran the job again by itself, off spot machines")), "{j}");
        assert!(served.gitlab.asked.lock().unwrap().iter().any(|x| x.ends_with(&format!("/jobs/{e}/retry"))), "{:?}", served.gitlab.asked.lock().unwrap());
        super::seen_in_browser(&served);
    }

    #[test]
    fn a_failed_gitlab_jobs_log_with_times_streams_and_nested_sections_is_shown_in_its_steps() {
        let served = serve(|_| {});
        served.connect_gitlab();
        let home = signed_in(&served);
        let (id, pending) = served.gitlab.pending("e2e", &["superci"], served.now_ms());
        assert!(served.gitlab_webhook(pending) < 300);
        assert!(served.gitlab_webhook(served.gitlab.running(id, served.now_ms())) < 300);
        // A runner with times on (each line its time and stream, "00O" out, "01E" error, "+" a line's rest): a section
        // whose heading comes on the next line, one inside another, and the script's, after which the job failed.
        let t = |n: u32, stream: &str, text: &str| format!("2026-10-15T10:00:{n:02}.123456Z {stream} {text}");
        let log = [
            t(0, "00O", "\u{1b}[0KRunning with gitlab-runner 19.4.1"),
            t(1, "00O", "\u{1b}[0Ksection_start:1760522401:prepare_executor\r\u{1b}[0K"),
            t(1, "00O+", "\u{1b}[0K\u{1b}[36;1mPreparing the \"docker\" executor\u{1b}[0;m"),
            t(2, "00O", "Using Docker executor with image node:22 ..."),
            t(3, "00O", "\u{1b}[0Ksection_end:1760522403:prepare_executor\r\u{1b}[0K"),
            t(4, "00O", "\u{1b}[0Ksection_start:1760522404:step_script\r\u{1b}[0K\u{1b}[36;1mExecuting \"step_script\" stage\u{1b}[0;m"),
            t(5, "00O", "\u{1b}[0Ksection_start:1760522405:install[collapsed=true]\r\u{1b}[0K\u{1b}[1mnpm ci\u{1b}[0m"),
            t(6, "00O", "added 312 packages"),
            t(7, "00O", "\u{1b}[0Ksection_end:1760522407:install\r\u{1b}[0K"),
            t(8, "01E", "WARNING: a deprecated option"),
            t(9, "01E", "\u{1b}[31m2 tests failed\u{1b}[0m\u{7}"),
            t(9, "00O", "\u{1b}[2Kprogress\u{1b}[1Gdone"),
            t(10, "00O", "\u{1b}[0Ksection_end:1760522410:step_script\r\u{1b}[0K"),
            t(10, "01E", "\u{1b}[31;1mERROR: Job failed: exit code 1\u{1b}[0;m"),
        ].join("\n");
        assert!(served.gitlab_webhook(served.gitlab.finish(id, "failed", &log, served.now_ms())) < 300);
        let (mut child, link) = dashboard(&home);
        let (_, view) = part(&link, &format!("/job/view?id={id}"));
        let view: serde_json::Value = serde_json::from_str(&view).unwrap_or_else(|_| panic!("{view}"));
        let steps: Vec<(String, String)> = view["steps"].as_array().unwrap().iter().map(|s| (s["name"].as_str().unwrap_or_default().to_string(), s["state"].as_str().unwrap_or_default().to_string())).collect();
        let named = |n: &str| steps.iter().find(|(name, _)| name == n).map(|(_, s)| s.clone()).unwrap_or_else(|| panic!("no step {n}: {steps:?}"));
        assert_eq!(named("Preparing the \"docker\" executor"), "success", "the heading on the next line names it");
        assert_eq!(named("Executing \"step_script\" stage"), "failure", "{steps:?}");
        let text = view.to_string();
        assert!(text.contains("npm ci") && text.contains("added 312 packages") && text.contains("2 tests failed") && !text.contains("00O") && !text.contains("01E"), "{text}");
        let _ = child.kill();
        let _ = std::fs::remove_dir_all(&home);
        super::seen_in_browser(&served);
    }

    /// The same from the CLI: connected with a token named by its variable, a project switched on and off, all of them
    /// on at once, and disconnected (with --confirm).
    #[test]
    fn gitlab_is_connected_from_the_cli_and_its_projects_switched() {
        let served = serve(|_| {});
        let home = signed_in(&served);
        let run = |args: &[&str], token: Option<&str>| {
            let mut c = common::command(&home);
            c.env_remove("SUPERCI_GITLAB_TOKEN");
            if let Some(t) = token { c.env("ACME_GITLAB_TOKEN", t); }
            let o = c.args(args).output().unwrap();
            (o.status.code().unwrap_or(-1), format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)))
        };
        let (code, said) = run(&["connections", "create", "gitlab", "--token-env=ACME_GITLAB_TOKEN"], None);
        assert!(code == 2 && said.contains("Give the GitLab token by name: ACME_GITLAB_TOKEN"), "{code} {said}");
        let (code, said) = run(&["connections", "create", "gitlab", "--token-env=ACME_GITLAB_TOKEN", "--dry-run"], Some(gitlab::TOKEN));
        assert!(code == 0 && said.contains("connect https://gitlab.com with the token in ACME_GITLAB_TOKEN"), "{code} {said}");
        let (code, said) = run(&["connections", "create", "gitlab", "--token-env=ACME_GITLAB_TOKEN"], Some(gitlab::TOKEN));
        assert!(code == 0 && said.contains("GitLab is connected."), "{code} {said}");
        let project = gitlab::PROJECT.to_string();
        for (args, expect) in [(&["projects", "update", &project, "--enabled=true"][..], "Its jobs come here now"), (&["projects", "update", &project, "--enabled=false"], "Its jobs no longer come here"),
            (&["projects", "update", "--all", "--enabled=true"], "Its jobs come here now")] {
            let (code, said) = run(args, None);
            assert!(code == 0 && said.contains(expect), "{args:?}: {code} {said}");
        }
        assert_eq!(json(&home, &["projects", "list"]).1["projects"][0]["enabled"], true);
        for (args, expect) in [(&["projects", "update", &project][..], "Say which way"), (&["projects", "update", "--all", "--enabled=false"], "one at a time"), (&["projects", "update", "--enabled=true"], "Say which")] {
            let (code, said) = run(args, None);
            assert!(code == 2 && said.contains(expect), "{args:?}: {code} {said}");
        }
        let (code, said) = run(&["connections", "delete", "gitlab"], None);
        assert!(code != 0 && said.contains("--confirm"), "{code} {said}");
        let (code, said) = run(&["connections", "delete", "gitlab", "--confirm"], None);
        assert!(code == 0 && said.contains("GitLab is disconnected."), "{code} {said}");
        assert_eq!(served.aws.settings(superci_testkit::PLANE).into_iter().find(|(k, _)| k == "GITLAB").map(|(_, v)| v).as_deref(), Some("off"));
        let _ = std::fs::remove_dir_all(&home);
        super::seen_in_browser(&served);
    }

    #[test]
    fn gitlab_jobs_whose_events_were_lost_are_settled_by_the_upkeep_as_gitlab_has_them() {
        let served = serve(|_| {});
        served.connect_gitlab();
        // One its machine's runner took, then finished; one cancelled before any runner took it; one still waiting.
        let (ran, p) = served.gitlab.pending("ran", &["superci"], served.now_ms());
        served.gitlab_webhook(p);
        let _ = served.gitlab.running(ran, served.now_ms());
        let (gone, p) = served.gitlab.pending("gone", &["superci"], served.now_ms());
        served.gitlab_webhook(p);
        let _ = served.gitlab.finish(gone, "canceled", "", served.now_ms());
        let (waits, p) = served.gitlab.pending("waits", &["superci"], served.now_ms());
        served.gitlab_webhook(p);
        let state = |id: u64| served.store.under(&format!("job:gl{id}"))[0].1.clone();
        // None of their events came. A minute on, the one its runner took is running (GitLab is asked once its machine
        // has had time to come up); the cancelled one is given the time a machine may take.
        served.later(61_000);
        served.upkeep();
        assert_eq!((state(ran)["state"].as_str(), state(gone)["state"].as_str()), (Some("running"), Some("launched")));
        // Past that: the one GitLab no longer has waiting is ended.
        served.later(3 * 60_000);
        served.upkeep();
        assert!(["swept", "cancelled", "done"].contains(&state(gone)["state"].as_str().unwrap()), "{}", state(gone));
        assert_ne!(state(waits)["state"], "running", "{}", state(waits));
        super::seen_in_browser(&served);
    }

}


/// The runs from a computer, as `superci runs create` runs them, each ending with what the dashboard shows of it.
mod runs_suite_in_browser {
    use super::*;
    use std::process::{Command, Stdio};
    use superci_testkit::Runner;

    fn checkout() -> std::path::PathBuf {
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!("superci-e2e-{}-{}-{}", std::process::id(), N.fetch_add(1, std::sync::atomic::Ordering::SeqCst), superci_core_free_id()));
        std::fs::create_dir_all(dir.join(".github/workflows")).unwrap();
        std::fs::write(dir.join(".github/workflows/ci.yml"), "jobs:\n  tests:\n    runs-on: superci\n    steps:\n      - run: make\n      - run: make test\n").unwrap();
        std::fs::write(dir.join("Makefile"), "test:\n\techo ok\n").unwrap();
        for args in [&["init", "-q"][..], &["add", "-A"], &["-c", "user.name=t", "-c", "user.email=t@example.com", "commit", "-qm", "t"]] {
            assert!(Command::new("git").current_dir(&dir).args(args).status().unwrap().success());
        }
        dir
    }

    fn superci_core_free_id() -> u128 { std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos() }

    #[test]
    fn a_run_from_a_computer_prints_every_line_once_in_order_and_ends_with_how_each_step_went() {
        let served = serve(|_| {});
        let dir = checkout();
        let home = dir.join(".home");
        let cli = Command::new(env!("CARGO_BIN_EXE_superci")).current_dir(&dir).args(["runs", "create", "--workflow=.github/workflows/ci.yml"])
            .env("SUPERCI_HOME", &home).env("SUPERCI_PLANE", &served.url).env("SUPERCI_KEY", &served.run_key).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
        // The machine started; its runner (played here) takes the job.
        let run = loop {
            if let Some((k, _)) = served.store.under("run:").into_iter().next() { break k.trim_start_matches("run:").to_string() }
            std::thread::sleep(std::time::Duration::from_millis(50));
        };
        let runner = loop { if let Some(r) = Runner::of_run(&served, &run) { break r } std::thread::sleep(std::time::Duration::from_millis(50)) };
        let line = |n: u64| format!("build line {n}");
        let (a, b) = ("aaaaaaaa-0000-0000-0000-000000000001", "bbbbbbbb-0000-0000-0000-000000000002");
        runner.take();
        assert_eq!(runner.steps(&[(a, "Run make", 1, "inProgress", None, None), (b, "Run make test", 2, "pending", None, None)]), 200);
        // Lines 1-3 as they come, then 7-8: the runner dropped 4-6 (it holds 1,024 at most).
        assert_eq!(runner.lines(a, 1, &(1..=3).map(line).collect::<Vec<_>>()), 200);
        std::thread::sleep(std::time::Duration::from_millis(1800));
        assert_eq!(runner.lines(a, 7, &(7..=8).map(line).collect::<Vec<_>>()), 200);
        std::thread::sleep(std::time::Duration::from_millis(1800));
        // The step ends: its whole log; then the next step, and the job.
        assert_eq!(runner.log(1, &(1..=8).map(|n| format!("2026-10-15T10:00:0{}.0000000Z {}", n % 10, line(n))).collect::<Vec<_>>().join("\n")), 200);
        assert_eq!(runner.steps(&[(a, "Run make", 1, "completed", Some("succeeded"), Some(1)), (b, "Run make test", 2, "inProgress", None, None)]), 200);
        assert_eq!(runner.lines(b, 1, &["ok".to_string()]), 200);
        assert_eq!(runner.log(2, "2026-10-15T10:00:09.0000000Z ok"), 200);
        assert_eq!(runner.steps(&[(a, "Run make", 1, "completed", Some("succeeded"), Some(1)), (b, "Run make test", 2, "completed", Some("succeeded"), Some(2))]), 200);
        assert_eq!(runner.finish("succeeded"), 200);
        let out = cli.wait_with_output().unwrap();
        let said = String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "{said}");
        // Every line once, in the order printed: 1-3 as they came, 7-8, then 4-6 once the step ended, said as late.
        let printed: Vec<&str> = said.lines().map(str::trim).collect();
        let at = |s: &str| printed.iter().position(|l| l.ends_with(s)).unwrap_or_else(|| panic!("no {s:?} in:\n{said}"));
        for n in 1..=8 { assert_eq!(printed.iter().filter(|l| l.ends_with(&line(n))).count(), 1, "{n} once:\n{said}") }
        assert!(at(&line(3)) < at(&line(7)) && at(&line(8)) < at("lines 4–6 of this step, which came late:") && at("which came late:") < at(&line(4)) && !said.contains("when it ends"), "{said}");
        // How it ended: each step, then the run.
        let has = |s: &str| printed.iter().position(|l| l.contains(s)).unwrap_or_else(|| panic!("no {s:?} in:\n{said}"));
        assert!(has("✓ Run make 7 s") < has("✓ Run make test 7 s") && said.contains("The run ended: success"), "{said}");
        // (Its log has no tests: no line of them.)
        assert!(!said.contains("Tests:"), "{said}");
        // Once it ran, how its start went (where the time from sending it to its first step went).
        assert!(has("Running (started in ") < has(&line(1)) && said.contains("its machine was asked for "), "{said}");
        // Its end said after its lines, the late ones too.
        assert!(at(&line(4)) < has("Ended: success"), "{said}");
        let _ = std::fs::remove_dir_all(&dir);
        super::seen_in_browser(&served);
    }

    /// Plays a one-step run to its end: `lines` as they come (all of them), its whole log, and how it ended.
    #[test]
    fn a_long_runs_lines_come_a_few_mb_at_a_time_and_every_one_is_printed_when_it_is_followed_after_it_ended() {
        // Two MB of lines, sent as a runner sends them (a hundred at a time), the run over before it is followed: its
        // status gives them in more than one answer (a runtime's holds six MB), and every one is printed, in order.
        let served = serve(|_| {});
        let dir = checkout();
        assert!(superci(&served, &dir, &["runs", "create", "--workflow=.github/workflows/ci.yml", "--detach"]).output().unwrap().status.success());
        let run = run_of(&served);
        let runner = loop { if let Some(r) = Runner::of_run(&served, &run) { break r } std::thread::sleep(std::time::Duration::from_millis(50)) };
        let a = "cccccccc-0000-0000-0000-000000000003";
        let lines: Vec<String> = (1..=4000).map(|n| format!("line {n:04} {}", "x".repeat(490))).collect();
        runner.take();
        runner.steps(&[(a, "Run make test", 1, "inProgress", None, None)]);
        for (k, chunk) in lines.chunks(100).enumerate() { runner.lines(a, k as u64 * 100 + 1, chunk); }
        runner.log(1, &lines.iter().map(|l| format!("2026-10-15T10:00:01.0000000Z {l}")).collect::<Vec<_>>().join("\n"));
        runner.steps(&[(a, "Run make test", 1, "completed", Some("succeeded"), Some(1))]);
        runner.finish("succeeded");
        // Asked from the start: a part, and where to ask from next; from there, the rest.
        let status = |after: u64| -> serde_json::Value { ureq::get(&format!("{}/runs/{run}?after={after}", served.url)).header("authorization", &format!("Bearer {}", served.run_key)).call().unwrap().body_mut().read_json().unwrap() };
        let first = status(0);
        let (got, next) = (first["jobs"][0]["lines"].as_array().unwrap().len(), first["jobs"][0]["next"].as_u64().unwrap());
        assert!(got > 0 && got < 4000, "{got} lines in the first answer");
        assert_eq!(got + status(next)["jobs"][0]["lines"].as_array().unwrap().len(), 4000);
        let out = superci(&served, &dir, &["runs", "wait", &run]).output().unwrap();
        let said = String::from_utf8_lossy(&out.stdout).to_string();
        assert!(out.status.success(), "{}{}", said.chars().take(500).collect::<String>(), String::from_utf8_lossy(&out.stderr));
        let printed: Vec<&str> = said.lines().filter_map(|l| l.split("line ").nth(1)).map(|l| &l[..4]).collect();
        assert_eq!(printed.len(), 4000, "every line once");
        assert!(printed.windows(2).all(|w| w[0] < w[1]), "in order");
        let _ = std::fs::remove_dir_all(&dir);
        super::seen_in_browser(&served);
    }

    fn play(served: &superci_testkit::Served, run: &str, lines: &[String], result: &str) {
        let runner = loop { if let Some(r) = Runner::of_run(served, run) { break r } std::thread::sleep(std::time::Duration::from_millis(50)) };
        let a = "cccccccc-0000-0000-0000-000000000003";
        runner.take();
        runner.steps(&[(a, "Run make test", 1, "inProgress", None, None)]);
        runner.lines(a, 1, lines);
        runner.log(1, &lines.iter().map(|l| format!("2026-10-15T10:00:01.0000000Z {l}")).collect::<Vec<_>>().join("\n"));
        runner.steps(&[(a, "Run make test", 1, "completed", Some(if result == "succeeded" { "succeeded" } else { "failed" }), Some(1))]);
        runner.finish(result);
    }

    fn run_of(served: &superci_testkit::Served) -> String {
        loop {
            if let Some((k, _)) = served.store.under("run:").into_iter().next() { return k.trim_start_matches("run:").to_string() }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }

    fn superci(served: &superci_testkit::Served, dir: &std::path::Path, args: &[&str]) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_superci"));
        // (A run over waits 3 seconds at most for steps' logs here, where a minute would only slow the tests.)
        c.current_dir(dir).args(args).env("SUPERCI_HOME", dir.join(".home")).env("SUPERCI_PLANE", &served.url).env("SUPERCI_KEY", &served.run_key).env("SUPERCI_LOGS_WAIT_S", "3").stdout(Stdio::piped()).stderr(Stdio::piped());
        c
    }

    #[test]
    fn quiet_a_failed_run_says_how_each_step_went_and_the_end_of_the_one_that_failed_and_fails() {
        let served = serve(|_| {});
        let dir = checkout();
        let cli = superci(&served, &dir, &["runs", "create", "--workflow=.github/workflows/ci.yml", "--quiet"]).spawn().unwrap();
        let run = run_of(&served);
        let mut lines: Vec<String> = (1..=60).map(|n| format!("test {n} ok")).collect();
        lines.push("test 61 FAILED - expected 2, got 3".into());
        play(&served, &run, &lines, "failed");
        let out = cli.wait_with_output().unwrap();
        let said = String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success(), "{said}");
        assert!(said.contains("✗ Run make test") && said.contains("test 61 FAILED") && said.contains("The run ended: failure"), "{said}");
        assert!(!said.contains("test 1 ok") && said.contains("test 60 ok"), "only the last lines of the failed step:\n{said}");
        let _ = std::fs::remove_dir_all(&dir);
        super::seen_in_browser(&served);
    }

    #[test]
    fn a_runs_summary_quiet_too_says_what_each_job_had_from_the_caches_docker_images() {
        // A job whose machine keeps Docker's images: what it had and pulled, as its machine said when the job ended (before
        // keeping them); one whose machine has said nothing yet: where to read it once it has.
        for (images, line) in [(Some(serde_json::json!({ "served": 103, "served_bytes": 2_700_000_000u64, "pulled": 12, "pulled_bytes": 300_000_000u64, "keeping": true, "at": 1 })),
                "Docker images: 103 layers from the cache (2.7 GB); 12 layers pulled (300 MB), being kept for the next job."), (None, "Docker images: not said yet by its machine (`superci jobs retrieve ")] {
            let served = serve(|_| {});
            let dir = checkout();
            let cli = superci(&served, &dir, &["runs", "create", "--workflow=.github/workflows/ci.yml", "--quiet"]).spawn().unwrap();
            let run = run_of(&served);
            let runner = loop { if let Some(r) = Runner::of_run(&served, &run) { break r } std::thread::sleep(std::time::Duration::from_millis(50)) };
            let a = "cccccccc-0000-0000-0000-000000000003";
            runner.take();
            {
                let mut store = served.store.0.lock().unwrap();
                let key = store.keys().find(|k| k.starts_with("job:")).unwrap().clone();
                let mut j: serde_json::Value = serde_json::from_str(&store[&key]).unwrap();
                j["keeps"] = true.into();
                if let Some(i) = &images { j["images"] = i.clone() }
                store.insert(key, j.to_string());
            }
            runner.steps(&[(a, "Run make test", 1, "inProgress", None, None)]);
            runner.log(1, "2026-10-15T10:00:01.0000000Z ok");
            runner.steps(&[(a, "Run make test", 1, "completed", Some("succeeded"), Some(1))]);
            runner.finish("succeeded");
            let out = cli.wait_with_output().unwrap();
            let said = String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
            assert!(out.status.success() && said.contains(line), "{said}");
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    /// A way to the control plane that can be cut (every connection dropped, new ones refused) and put back, as when
    /// this computer's network or DNS drops for a while.
    struct Cut { url: String, down: std::sync::Arc<std::sync::atomic::AtomicBool>, open: std::sync::Arc<std::sync::Mutex<Vec<std::net::TcpStream>>> }
    impl Cut {
        fn to(target: &str) -> Cut {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            let target = target.trim_start_matches("http://").trim_end_matches('/').to_string();
            let (down, open) = (std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)), std::sync::Arc::new(std::sync::Mutex::new(Vec::<std::net::TcpStream>::new())));
            let (d, o) = (down.clone(), open.clone());
            std::thread::spawn(move || for client in listener.incoming().flatten() {
                if d.load(std::sync::atomic::Ordering::SeqCst) { drop(client); continue }
                let Ok(server) = std::net::TcpStream::connect(&target) else { continue };
                o.lock().unwrap().extend([client.try_clone().unwrap(), server.try_clone().unwrap()]);
                for (mut from, mut to) in [(client.try_clone().unwrap(), server.try_clone().unwrap()), (server, client)] {
                    std::thread::spawn(move || { let _ = std::io::copy(&mut from, &mut to); let _ = to.shutdown(std::net::Shutdown::Both); });
                }
            });
            Cut { url, down, open }
        }
        fn set(&self, down: bool) {
            self.down.store(down, std::sync::atomic::Ordering::SeqCst);
            if down { for s in self.open.lock().unwrap().drain(..) { let _ = s.shutdown(std::net::Shutdown::Both); } }
        }
    }

    #[test]
    fn a_run_followed_through_a_network_that_drops_for_a_while_is_followed_to_its_end() {
        let served = serve(|_| {});
        let dir = checkout();
        let cut = Cut::to(&served.url);
        let cli = superci(&served, &dir, &["runs", "create", "--workflow=.github/workflows/ci.yml", "--quiet"]).env("SUPERCI_PLANE", &cut.url).spawn().unwrap();
        let run = run_of(&served);
        let runner = loop { if let Some(r) = Runner::of_run(&served, &run) { break r } std::thread::sleep(std::time::Duration::from_millis(50)) };
        let a = "cccccccc-0000-0000-0000-000000000003";
        runner.take();
        runner.steps(&[(a, "Run make test", 1, "inProgress", None, None)]);
        std::thread::sleep(std::time::Duration::from_secs(2));
        // Out of touch for a while; the run ends meanwhile.
        cut.set(true);
        std::thread::sleep(std::time::Duration::from_secs(5));
        runner.log(1, "2026-10-15T10:00:01.0000000Z ok");
        runner.steps(&[(a, "Run make test", 1, "completed", Some("succeeded"), Some(1))]);
        runner.finish("succeeded");
        cut.set(false);
        let out = cli.wait_with_output().unwrap();
        let said = String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success() && said.contains("Out of touch with the control plane (") && said.contains("In touch with the control plane again after ") && said.contains("The run ended: success"), "{said}");
        assert_eq!(said.matches("Out of touch").count(), 1, "said once: {said}");
        // Out of touch for longer than it waits: given up, saying how to follow it again (the run goes on).
        let dir2 = checkout();
        let served2 = serve(|_| {});
        let cut2 = Cut::to(&served2.url);
        let cli = superci(&served2, &dir2, &["runs", "create", "--workflow=.github/workflows/ci.yml", "--quiet"]).env("SUPERCI_PLANE", &cut2.url).env("SUPERCI_LOST_S", "4").spawn().unwrap();
        let run2 = run_of(&served2);
        let runner2 = loop { if let Some(r) = Runner::of_run(&served2, &run2) { break r } std::thread::sleep(std::time::Duration::from_millis(50)) };
        runner2.take();
        std::thread::sleep(std::time::Duration::from_secs(2));
        cut2.set(true);
        let out = cli.wait_with_output().unwrap();
        let said = String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success() && said.contains(&format!("`superci runs wait {run2}` follows it again")), "{said}");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&dir2);
        super::seen_in_browser(&served);
    }

    #[test]
    fn a_run_is_started_as_a_labelled_pull_request_or_a_manual_start_with_its_inputs_as_github_would_send_them() {
        let workflow = "on:\n  pull_request:\n  workflow_dispatch:\n    inputs:\n      record:\n        type: boolean\n        default: false\njobs:\n  tests:\n    if: contains(github.event.pull_request.labels.*.name, 'e2e-recordings') || inputs.record\n    runs-on: superci\n    steps:\n      - run: make test\n";
        let started = |args: &[&str], runs: bool| {
            let served = serve(|_| {});
            let dir = checkout();
            std::fs::write(dir.join(".github/workflows/ci.yml"), workflow).unwrap();
            let mut all = vec!["runs", "create", "--workflow=.github/workflows/ci.yml", "--quiet"];
            all.extend(args);
            let cli = superci(&served, &dir, &all).spawn().unwrap();
            let run = run_of(&served);
            let ran = (0..60).find_map(|_| { std::thread::sleep(std::time::Duration::from_millis(100)); Runner::of_run(&served, &run) });
            assert_eq!(ran.is_some(), runs, "{args:?} {}", served.store.under(&format!("run:{run}"))[0].1);
            // (The job its runner takes: with the payload in its `github` context.)
            let taken = ran.map(|r| { let (_, m) = r.message(); r.finish("succeeded"); m.to_string() }).unwrap_or_default();
            let out = cli.wait_with_output().unwrap();
            let said = String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
            // (Kept with the run: the payload as GitHub sends it.)
            let kept = served.store.under(&format!("run:{run}"))[0].1.clone();
            let _ = std::fs::remove_dir_all(&dir);
            (said, kept, taken)
        };
        // A pull request with the label: its job runs, the payload labelled; without it, skipped.
        let (_, kept, taken) = started(&["--event=pull_request", "--label=wip", "--label=e2e-recordings"], true);
        assert!(taken.contains("e2e-recordings") && taken.contains("head_ref"), "{taken}");
        assert_eq!((kept["event"]["action"].as_str(), kept["event"]["label"]["name"].as_str(), kept["event"]["pull_request"]["labels"][1]["name"].as_str(), kept["event"]["pull_request"]["head"]["ref"].is_string()), (Some("labeled"), Some("e2e-recordings"), Some("e2e-recordings"), true), "{kept}");
        let (said, _, _) = started(&["--event=pull_request"], false);
        assert!(said.contains("skipped"), "{said}");
        // Started by hand with the input: its job runs, the input typed; its defaults otherwise.
        let (_, kept, taken) = started(&["--event=workflow_dispatch", "--input=record=true"], true);
        assert!(taken.contains("record"), "{taken}");
        assert_eq!((kept["inputs"]["record"].clone(), kept["event"]["inputs"]["record"].clone()), (serde_json::json!(true), serde_json::json!("true")), "{kept}");
        // What does not go together: refused before anything is sent.
        let served = serve(|_| {});
        let dir = checkout();
        std::fs::write(dir.join(".github/workflows/ci.yml"), workflow).unwrap();
        std::fs::write(dir.join("event.json"), r#"{"pull_request": {"draft": true}}"#).unwrap();
        for (args, refused) in [(&["--label=x"][..], "--label is a pull request's label"), (&["--event=workflow_dispatch", "--input=nope=1"], "The workflow has no input nope (its inputs: record)"),
            (&["--event=workflow_dispatch", "--input=record=yes"], "true or false, not “yes”"), (&["--event=release"], "--event is push, pull_request or workflow_dispatch"), (&["--event=pull_request", "--input=record=true"], "--input is a manual start's input")] {
            let mut all = vec!["runs", "create", "--workflow=.github/workflows/ci.yml", "--dry-run"];
            all.extend(args);
            let o = superci(&served, &dir, &all).output().unwrap();
            assert!(!o.status.success() && String::from_utf8_lossy(&o.stderr).contains(refused), "{args:?}: {}", String::from_utf8_lossy(&o.stderr));
        }
        let o = superci(&served, &dir, &["runs", "create", "--workflow=.github/workflows/ci.yml", "--dry-run", "--event=pull_request", "--event-payload=event.json"]).output().unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        assert!(served.store.under("run:").is_empty());
        let _ = std::fs::remove_dir_all(&dir);
        super::seen_in_browser(&served);
    }

    #[test]
    fn a_dry_run_checks_the_workflow_its_job_and_the_tree_and_starts_nothing() {
        let served = serve(|_| {});
        let dir = checkout();
        let o = superci(&served, &dir, &["runs", "create", "--workflow=.github/workflows/ci.yml", "--dry-run"]).output().unwrap();
        let said = String::from_utf8_lossy(&o.stdout).into_owned();
        assert!(o.status.success() && said.starts_with("Would send the working tree (") && said.contains("and run .github/workflows/ci.yml: its job ") && said.contains("Nothing was sent and no machine was started."), "{said}{}", String::from_utf8_lossy(&o.stderr));
        let o = superci(&served, &dir, &["runs", "create", "--workflow=.github/workflows/ci.yml", "--job=nope", "--dry-run"]).output().unwrap();
        assert!(!o.status.success() && String::from_utf8_lossy(&o.stderr).contains("has no job nope (its jobs: "), "{}", String::from_utf8_lossy(&o.stderr));
        assert!(served.store.under("run:").is_empty() && served.store.under("rtree").is_empty() && served.machines.0.lock().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
        super::seen_in_browser(&served);
    }

    #[test]
    fn a_run_started_and_left_is_followed_later_to_its_end() {
        let served = serve(|_| {});
        let dir = checkout();
        let out = superci(&served, &dir, &["runs", "create", "--workflow=.github/workflows/ci.yml", "--detach"]).output().unwrap();
        let said = String::from_utf8_lossy(&out.stdout).to_string();
        assert!(out.status.success() && said.contains("Follow it: superci runs wait"), "{said}{}", String::from_utf8_lossy(&out.stderr));
        let run = run_of(&served);
        let wait = superci(&served, &dir, &["runs", "wait", &run]).spawn().unwrap();
        play(&served, &run, &["all good".to_string()], "succeeded");
        let out = wait.wait_with_output().unwrap();
        let said = String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success() && said.contains("all good") && said.contains("followed from here"), "{said}");
        let _ = std::fs::remove_dir_all(&dir);
        super::seen_in_browser(&served);
    }

    #[test]
    fn with_a_key_for_coding_agents_what_ran_is_read_and_a_jobs_whole_log_is_kept_in_a_file() {
        let served = serve(|_| {});
        let dir = checkout();
        let cli = superci(&served, &dir, &["runs", "create", "--workflow=.github/workflows/ci.yml", "--quiet"]).spawn().unwrap();
        let run = run_of(&served);
        play(&served, &run, &(1..=5).map(|n| format!("line {n}")).collect::<Vec<_>>(), "succeeded");
        assert!(cli.wait_with_output().unwrap().status.success());
        let read = |args: &[&str]| { let o = superci(&served, &dir, args).output().unwrap(); (o.status.success(), String::from_utf8_lossy(&o.stdout).to_string() + &String::from_utf8_lossy(&o.stderr)) };
        let (ok, jobs) = read(&["jobs", "list"]);
        assert!(ok && jobs.contains("passed") && jobs.contains("tests"), "{jobs}");
        let job = jobs.split_whitespace().next().unwrap().to_string();
        let (ok, runs) = read(&["runs", "list"]);
        assert!(ok && runs.contains(&run), "{runs}");
        let (ok, usage) = read(&["usage", "retrieve"]);
        assert!(ok && usage.contains("The last 24 hours: 1 jobs, 1 ran"), "{usage}");
        let file = dir.join("job.log");
        let (ok, kept) = read(&["jobs", "retrieve", &job, &format!("--out={}", file.display())]);
        assert!(ok && kept.contains("Its whole log"), "{kept}");
        let text = std::fs::read_to_string(&file).unwrap();
        assert!((1..=5).all(|n| text.contains(&format!("line {n}"))), "{text}");
        let _ = std::fs::remove_dir_all(&dir);
        super::seen_in_browser(&served);
    }

    #[test]
    fn a_machine_that_never_begins_its_job_is_started_again_twice_then_the_run_fails_saying_why() {
        let served = serve(|_| {});
        let dir = checkout();
        let cli = Command::new(env!("CARGO_BIN_EXE_superci")).current_dir(&dir).args(["runs", "create", "--workflow=.github/workflows/ci.yml"])
            .env("SUPERCI_HOME", dir.join(".home")).env("SUPERCI_PLANE", &served.url).env("SUPERCI_KEY", &served.run_key).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
        while served.machines.0.lock().unwrap().is_empty() { std::thread::sleep(std::time::Duration::from_millis(50)) }
        // Its runner never takes the job. Within three minutes that is waited for; after, its machine is started anew.
        served.later(60_000);
        served.upkeep();
        assert_eq!(served.machines.0.lock().unwrap().len(), 1);
        for starts in [2, 3] {
            served.later(4 * 60_000);
            served.upkeep();
            assert_eq!(served.machines.0.lock().unwrap().len(), starts);
        }
        served.later(4 * 60_000);
        served.upkeep();
        let job = served.store.under("job:").into_iter().next().unwrap().1;
        assert_eq!((job["state"].as_str(), job["error"].as_str()), (Some("failed"), Some("Cloudflare did not start a machine for it, three times")), "{job}");
        // The run from this computer ends with it.
        let out = cli.wait_with_output().unwrap();
        let said = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        assert!(!out.status.success() && said.contains("did not start a machine for it, three times"), "{said}");
        super::seen_in_browser(&served);
    }

    #[test]
    fn a_job_that_runs_past_its_longest_time_is_ended_by_the_upkeep_and_the_run_with_it() {
        let served = serve(|_| {});
        let dir = checkout();
        let cli = superci(&served, &dir, &["runs", "create", "--workflow=.github/workflows/ci.yml", "--quiet"]).spawn().unwrap();
        let run = run_of(&served);
        let runner = loop { if let Some(r) = Runner::of_run(&served, &run) { break r } std::thread::sleep(std::time::Duration::from_millis(50)) };
        runner.take();
        runner.steps(&[("cccccccc-0000-0000-0000-000000000003", "Run make test", 1, "inProgress", None, None)]);
        // Within its longest time (6 hours here, and 5 minutes more), it runs on.
        served.later(6 * 3_600_000);
        served.upkeep();
        assert_eq!(served.store.under("job:")[0].1["state"], "running");
        served.later(10 * 60_000);
        served.upkeep();
        let job = served.store.under("job:")[0].1.clone();
        assert_eq!(job["state"], "swept", "{job}");
        let out = cli.wait_with_output().unwrap();
        let said = String::from_utf8_lossy(&out.stdout);
        assert!(!out.status.success() && said.contains("failure (it ran past its longest time (6 hours), and its machine was ended)"), "{said}");
        super::seen_in_browser(&served);
    }

    #[test]
    fn a_failed_runs_tests_are_read_from_its_log_names_files_errors_and_counts() {
        let served = serve(|_| {});
        let dir = checkout();
        let cli = superci(&served, &dir, &["runs", "create", "--workflow=.github/workflows/ci.yml", "--quiet"]).spawn().unwrap();
        let run = run_of(&served);
        let vitest: Vec<String> = [" ❯ src/sum.test.ts (3 tests | 1 failed) 12ms", "⎯⎯⎯⎯⎯⎯⎯ Failed Tests 1 ⎯⎯⎯⎯⎯⎯⎯", "", " FAIL  src/sum.test.ts > sums > adds two numbers",
            "AssertionError: expected 3 to be 4 // Object.is equality", "", "⎯⎯⎯⎯⎯⎯⎯⎯⎯⎯⎯⎯⎯[1/1]⎯", " Test Files  1 failed | 4 passed (5)", "      Tests  1 failed | 41 passed (42)",
            // Then Playwright in two processes at once, each line marked by its own, then a long teardown.
            "[slot 1] Running 3 tests using 1 worker", "[slot 2] Running 2 tests using 1 worker", "[slot 2]   1) [chromium] › e2e/billing.spec.ts:21:3 › shows the invoice ───", "[slot 1]   3 passed (4.0s)",
            "[slot 2]     Error: expect(locator).toHaveText(expected) failed", "[slot 2]   1 failed", "[slot 2]   1 passed (5.1s)"].iter().map(|s| s.to_string()).chain((1..=50).map(|n| format!("stopping container {n}"))).collect();
        play(&served, &run, &vitest, "failed");
        // Quiet, the failed step's tests are said, though its last lines are the teardown's.
        let out = cli.wait_with_output().unwrap();
        let quiet = String::from_utf8_lossy(&out.stdout).into_owned();
        assert!(quiet.contains("✗ [chromium] shows the invoice (e2e/billing.spec.ts:21:3)\n        Error: expect(locator).toHaveText(expected) failed") && quiet.contains("✗ sums > adds two numbers (src/sum.test.ts)"), "{quiet}");
        assert!(quiet.contains("Tests: 2 failed, 45 passed (vitest, playwright)"), "{quiet}");
        // As a coding agent asks: the failed test, its file and error, and the counts.
        let o = superci(&served, &dir, &["runs", "retrieve", &run, "--tests", "--json"]).output().unwrap();
        let got: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap_or_else(|_| panic!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)));
        let tests = &got["runs"][0]["job_list"][0]["tests"];
        assert_eq!((tests["runners"][0].as_str(), tests["runners"][1].as_str(), tests["failed"].as_u64(), tests["passed"].as_u64()), (Some("vitest"), Some("playwright"), Some(2), Some(45)), "{got}");
        assert_eq!((tests["failures"][0]["name"].as_str(), tests["failures"][0]["file"].as_str()), (Some("sums > adds two numbers"), Some("src/sum.test.ts")));
        assert!(tests["failures"][0]["error"].as_str().unwrap().starts_with("AssertionError: expected 3 to be 4"));
        // As a person reads it.
        let said = String::from_utf8_lossy(&superci(&served, &dir, &["runs", "retrieve", &run, "--tests"]).output().unwrap().stdout).into_owned();
        assert!(said.contains("Tests: 2 failed, 45 passed (vitest, playwright)") && said.contains("✗ [chromium] shows the invoice  (e2e/billing.spec.ts:21:3)") && said.contains("✗ sums > adds two numbers  (src/sum.test.ts)") && said.contains("AssertionError: expected 3 to be 4"), "{said}");
        super::seen_in_browser(&served);
    }

    #[test]
    fn a_step_with_no_log_of_its_own_still_gives_its_tests_and_quiet_says_each_failure_whole_and_the_counts() {
        // A control plane with its own AWS account (logs into its bucket); a step whose whole log never came.
        let served = serve(|c| {
            (c.aws_own, c.aws_own_creds) = (Some(("111111111111".into(), "us-east-1".into())), Some(superci_core::aws::Credentials { access_key_id: "ASIAX".into(), secret_access_key: "secret".into(), session_token: Some("tok".into()), expires_at_ms: u64::MAX }));
            c.routing.order = vec![superci_core::plane::Pool::of("cloudflare")];
        });
        let bucket = superci_core::cache::bucket(superci_testkit::PLANE, "us-east-1");
        served.github.put_object(&format!("{bucket}/{}", superci_core::cache::program_key("x86_64")), b"program");
        let dir = checkout();
        let cli = superci(&served, &dir, &["runs", "create", "--workflow=.github/workflows/ci.yml", "--quiet"]).env("SUPERCI_STAND_IN", &served.aws.url).spawn().unwrap();
        let run = run_of(&served);
        let runner = loop { if let Some(r) = Runner::of_run(&served, &run) { break r } std::thread::sleep(std::time::Duration::from_millis(50)) };
        let (a, b) = ("aaaaaaaa-0000-0000-0000-000000000001", "bbbbbbbb-0000-0000-0000-000000000002");
        runner.take();
        runner.steps(&[(a, "Run make", 1, "inProgress", None, None), (b, "Run the tests", 2, "pending", None, None)]);
        runner.log(1, "2026-10-16T10:00:01.0000000Z built");
        runner.steps(&[(a, "Run make", 1, "completed", Some("succeeded"), Some(1)), (b, "Run the tests", 2, "inProgress", None, None)]);
        let diff: Vec<String> = (1..=20).map(|n| format!("[slot 1]       - expected line {n}")).collect();
        let lines: Vec<String> = ["[slot 1] Running 3 tests using 1 worker", "[slot 1]   1) [chromium] › e2e/settings.spec.ts:9:3 › saves ───", "[slot 1]", "[slot 1]     Error: expect(received).toEqual(expected)"].iter().map(|s| s.to_string())
            .chain(diff).chain(["[slot 1]   1 failed", "[slot 1]   2 passed (3.1s)"].iter().map(|s| s.to_string())).collect();
        runner.lines(b, 1, &lines);
        runner.steps(&[(a, "Run make", 1, "completed", Some("succeeded"), Some(1)), (b, "Run the tests", 2, "completed", Some("failed"), None)]);
        runner.finish("failed");
        let out = cli.wait_with_output().unwrap();
        let quiet = String::from_utf8_lossy(&out.stdout).into_owned();
        // Quiet: the failure with its whole error, and what the tests came to.
        assert!(quiet.contains("✗ [chromium] saves (e2e/settings.spec.ts:9:3)") && quiet.contains("Error: expect(received).toEqual(expected)") && quiet.contains("- expected line 20"), "{quiet}");
        assert!(quiet.contains("Tests: 1 failed, 2 passed (playwright)"), "{quiet}");
        // Waited for that step's log a while; it never came: the counts said to be from what came.
        assert!(quiet.contains(&format!("(Counted from what came: 1 step's log had not come. `superci runs retrieve {run} --tests` counts it once it is here.)")), "{quiet}");
        // Its tests read from the job's whole log, the step with no log of its own in it.
        let o = superci(&served, &dir, &["runs", "retrieve", &run, "--tests", "--json"]).env("SUPERCI_STAND_IN", &served.aws.url).output().unwrap();
        let got: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap_or_else(|_| panic!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)));
        let tests = &got["runs"][0]["job_list"][0]["tests"];
        assert_eq!((tests["failed"].as_u64(), tests["passed"].as_u64(), tests["failures"][0]["file"].as_str()), (Some(1), Some(2), Some("e2e/settings.spec.ts:9:3")), "{got}");
        let _ = std::fs::remove_dir_all(&dir);
        super::seen_in_browser(&served);
    }

    #[test]
    fn a_run_whose_test_runner_found_no_test_says_so() {
        let served = serve(|_| {});
        let dir = checkout();
        let cli = superci(&served, &dir, &["runs", "create", "--workflow=.github/workflows/ci.yml", "--quiet"]).spawn().unwrap();
        let run = run_of(&served);
        play(&served, &run, &["Running 0 tests using 0 workers".to_string(), "Error: No tests found.".to_string(), "Make sure that arguments are regular expressions matching test files.".to_string()], "failed");
        let quiet = String::from_utf8_lossy(&cli.wait_with_output().unwrap().stdout).into_owned();
        assert!(quiet.contains("Tests: 0 failed, 0 passed (playwright)") && quiet.contains("Playwright found no test to run: a `file:line` must be the first line of a test"), "{quiet}");
        let said = String::from_utf8_lossy(&superci(&served, &dir, &["runs", "retrieve", &run, "--tests"]).output().unwrap().stdout).into_owned();
        assert!(said.contains("Playwright found no test to run"), "{said}");
        let _ = std::fs::remove_dir_all(&dir);
        super::seen_in_browser(&served);
    }

    #[test]
    fn a_green_run_whose_slot_ran_no_tests_says_so_lists_what_ran_and_its_job_is_found_by_its_run() {
        let served = serve(|_| {});
        let dir = checkout();
        let cli = superci(&served, &dir, &["runs", "create", "--workflow=.github/workflows/ci.yml", "--quiet"]).spawn().unwrap();
        let run = run_of(&served);
        // Three slots: one's `file:line` no longer points at a test, and no tests found is allowed there.
        let lines: Vec<String> = ["[slot 1] Running 2 tests using 1 worker", "[slot 1]   ✓  1 [chromium] › e2e/cart.spec.ts:12:3 › adds to cart (2.0s)", "[slot 1]   ✘  2 [chromium] › e2e/cart.spec.ts:30:3 › known bug (1.0s)", "[slot 1]   2 passed (3.0s)",
            "[slot 2] Running 0 tests using 0 workers", "[slot 3] Running 1 test using 1 worker", "[slot 3]   ✓  1 [chromium] › e2e/login.spec.ts:5:3 › logs in (1.0s)", "[slot 3]   1 passed (1.0s)"].iter().map(|s| s.to_string()).collect();
        play(&served, &run, &lines, "succeeded");
        let out = cli.wait_with_output().unwrap();
        let quiet = String::from_utf8_lossy(&out.stdout).into_owned();
        assert!(out.status.success(), "{quiet}");
        assert!(quiet.contains("Tests: 0 failed, 3 passed (playwright), of the passed 1 failed as expected") && quiet.contains("[slot 2] Playwright ran no tests."), "{quiet}");
        assert!(quiet.contains("What ran:") && quiet.contains("✓ [chromium] adds to cart  (e2e/cart.spec.ts:12:3)") && quiet.contains("✓ (failed as expected) [chromium] known bug"), "{quiet}");
        // Followed without --quiet, the same at its end.
        let all = String::from_utf8_lossy(&superci(&served, &dir, &["runs", "wait", &run]).output().unwrap().stdout).into_owned();
        assert!(all.contains("Tests: 0 failed, 3 passed (playwright)") && all.contains("[slot 2] Playwright ran no tests."), "{all}");
        // A passed job's tests too, with what was said of them.
        let said = String::from_utf8_lossy(&superci(&served, &dir, &["runs", "retrieve", &run, "--tests"]).output().unwrap().stdout).into_owned();
        assert!(said.contains("[slot 2] Playwright ran no tests."), "{said}");
        // Its job by its run, for a script: no id read out of text.
        let file = dir.join("job.log");
        let o = superci(&served, &dir, &["jobs", "retrieve", &format!("--run={run}"), &format!("--out={}", file.display())]).output().unwrap();
        assert!(o.status.success() && std::fs::read_to_string(&file).unwrap().contains("[slot 2] Running 0 tests"), "{}", String::from_utf8_lossy(&o.stderr));
        let o = superci(&served, &dir, &["jobs", "retrieve", &format!("--run={run}"), "--job=nope"]).output().unwrap();
        assert!(!o.status.success() && String::from_utf8_lossy(&o.stderr).contains("has no job nope (its jobs: tests)"), "{}", String::from_utf8_lossy(&o.stderr));
        let _ = std::fs::remove_dir_all(&dir);
        super::seen_in_browser(&served);
    }

    #[test]
    fn a_run_whose_tests_passed_and_a_type_check_failed_names_that_step_and_a_value_shells_read_is_noted() {
        let served = serve(|_| {});
        let dir = checkout();
        let cli = superci(&served, &dir, &["runs", "create", "--workflow=.github/workflows/ci.yml", "--quiet", "--var=CURRENCY=gbp|eur"]).spawn().unwrap();
        let run = run_of(&served);
        let runner = loop { if let Some(r) = Runner::of_run(&served, &run) { break r } std::thread::sleep(std::time::Duration::from_millis(50)) };
        let (a, b) = ("aaaaaaaa-0000-0000-0000-000000000001", "bbbbbbbb-0000-0000-0000-000000000002");
        runner.take();
        runner.steps(&[(a, "Run npm test", 1, "inProgress", None, None), (b, "Run npm run typecheck", 2, "pending", None, None)]);
        runner.log(1, "2026-10-16T10:00:01.0000000Z  Test Files  5 passed (5)\n2026-10-16T10:00:01.0000000Z       Tests  41 passed (41)");
        runner.steps(&[(a, "Run npm test", 1, "completed", Some("succeeded"), Some(1)), (b, "Run npm run typecheck", 2, "inProgress", None, None)]);
        runner.log(2, "2026-10-16T10:00:02.0000000Z src/a.ts(3,1): error TS2322: Type 'string' is not assignable to type 'number'.");
        runner.steps(&[(a, "Run npm test", 1, "completed", Some("succeeded"), Some(1)), (b, "Run npm run typecheck", 2, "completed", Some("failed"), Some(2))]);
        runner.finish("failed");
        let out = cli.wait_with_output().unwrap();
        let said = String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
        assert!(said.contains("Note: --var CURRENCY's value has characters a shell runs (|)"), "{said}");
        assert!(said.contains("Tests: 0 failed, 41 passed (vitest)") && said.contains("Failed: Run npm run typecheck") && said.contains("The run ended: failure"), "{said}");
        let tests = String::from_utf8_lossy(&superci(&served, &dir, &["runs", "retrieve", &run, "--tests"]).output().unwrap().stdout).into_owned();
        assert!(tests.contains("no test in its log is read as failed (failed: Run npm run typecheck)"), "{tests}");
        let _ = std::fs::remove_dir_all(&dir);
        super::seen_in_browser(&served);
    }

    #[test]
    fn a_failed_run_followed_with_json_gives_its_jobs_ids() {
        let served = serve(|_| {});
        let dir = checkout();
        let cli = superci(&served, &dir, &["runs", "create", "--workflow=.github/workflows/ci.yml", "--json"]).spawn().unwrap();
        let run = run_of(&served);
        play(&served, &run, &["broken".to_string()], "failed");
        let out = cli.wait_with_output().unwrap();
        assert!(!out.status.success());
        let got: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|_| panic!("{}", String::from_utf8_lossy(&out.stdout)));
        assert_eq!((got["ok"].as_bool(), got["result"].as_str(), got["run_id"].as_str()), (Some(false), Some("failure"), Some(run.as_str())), "{got}");
        let job = got["jobs"][0]["job_id"].as_str().unwrap_or_else(|| panic!("{got}")).to_string();
        assert!(got["error"].as_str().unwrap().contains("The run ended: failure"));
        let o = superci(&served, &dir, &["jobs", "retrieve", &job]).output().unwrap();
        assert!(o.status.success() && String::from_utf8_lossy(&o.stdout).contains("broken"), "{}", String::from_utf8_lossy(&o.stdout));
        let _ = std::fs::remove_dir_all(&dir);
        super::seen_in_browser(&served);
    }

    #[test]
    fn a_job_on_a_machine_without_superci_s_cache_is_told_of_a_miss_as_github_says_it_and_artifacts_are_refused_saying_why() {
        let served = serve(|_| {});
        let dir = checkout();
        let o = superci(&served, &dir, &["runs", "create", "--workflow=.github/workflows/ci.yml", "--detach"]).output().unwrap();
        assert!(o.status.success());
        let run = run_of(&served);
        let runner = loop { if let Some(r) = Runner::of_run(&served, &run) { break r } std::thread::sleep(std::time::Duration::from_millis(50)) };
        let results = runner.at.trim_end_matches("/_apis/v1").to_string() + "/twirp/github.actions.results.api.v1.";
        let call = |what: &str| { let (status, _, body) = http("POST", &format!("{results}{what}"), br#"{"key":"setup-zig","version":"1"}"#); (status, serde_json::from_slice::<serde_json::Value>(&body).unwrap_or_default()) };
        // (JSON each time: the cache's client asks again five times for an answer that is not.)
        for what in ["CacheService/GetCacheEntryDownloadURL", "CacheService/CreateCacheEntry", "CacheService/FinalizeCacheEntryUpload"] {
            assert_eq!(call(what), (200, serde_json::json!({ "ok": false })), "{what}");
        }
        let (status, said) = call("ArtifactService/CreateArtifact");
        assert!(status == 501 && said["code"] == "unimplemented" && said["msg"].as_str().unwrap().contains("AWS machines only"), "{status} {said}");
        let _ = std::fs::remove_dir_all(&dir);
        super::seen_in_browser(&served);
    }

    #[test]
    fn a_job_starts_on_an_aws_spot_machine_as_aws_answered_its_first_pool_full_then_a_fleet_and_a_cancel_ends_it() {
        // A control plane with its own AWS account: a job's machine is asked of AWS, which answers as it did on Test (the
        // first pool, c8a.large in us-east-1f, had no machine; the rest were asked in one request, and AWS gave one).
        let served = serve(|c| {
            (c.aws_own, c.aws_own_creds) = (Some(("111111111111".into(), "us-east-1".into())), Some(superci_core::aws::Credentials { access_key_id: "ASIAX".into(), secret_access_key: "secret".into(), session_token: Some("tok".into()), expires_at_ms: u64::MAX }));
        });
        served.aws.replaying(include_str!("../fixtures/aws/launch/spot.jsonl"), &[]);
        // (Two CPUs: the machines asked for on Test.)
        let dir = checkout_with(&[(".github/workflows/ci.yml", "jobs:\n  tests:\n    runs-on: superci/cpu=2\n    steps:\n      - run: make test\n")]);
        let o = superci(&served, &dir, &["runs", "create", "--workflow=.github/workflows/ci.yml", "--detach"]).env("SUPERCI_STAND_IN", &served.aws.url).output().unwrap();
        assert!(o.status.success(), "{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
        let run = run_of(&served);
        let job = loop {
            if let Some((_, j)) = served.store.under("job:").into_iter().next().filter(|(_, j)| j["machine_id"].is_string() || j["state"] == "failed") { break j }
            std::thread::sleep(std::time::Duration::from_millis(50));
        };
        let calls: Vec<String> = served.aws.asked.lock().unwrap().iter().map(|a| { let asked = a["asked"].as_str().unwrap_or_default(); format!("{} {}", a["status"], asked.split('&').find(|p| p.starts_with("Action=")).unwrap_or(a["target"].as_str().unwrap_or_default())) }).collect();
        assert_eq!((job["cloud"].as_str(), job["machine_id"].as_str(), job["machine_type"].as_str(), job["zone"].as_str()), (Some("aws"), Some("i-00000000000000001"), Some("c8a.large"), Some("us-east-1d")), "{job}\n{calls:?}\n{:?}", served.aws.unanswered.lock().unwrap());
        // How its start went: the full pool, then the request for the rest.
        let asks = job["asks"].as_array().cloned().unwrap_or_default();
        assert!(asks.iter().any(|a| a["said"] == "InsufficientInstanceCapacity" && a["zone"] == "us-east-1f"), "{job}");
        // Cancelled before its runner began: its machine is ended in AWS.
        let o = superci(&served, &dir, &["runs", "cancel", &run]).output().unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        let ended = served.aws.asked.lock().unwrap().iter().any(|a| a["asked"].as_str().unwrap_or_default().contains("Action=TerminateInstances") && a["asked"].as_str().unwrap_or_default().contains("i-00000000000000001"));
        assert!(ended, "{:?}", served.aws.asked.lock().unwrap().iter().map(|a| a["asked"].as_str().unwrap_or_default().chars().take(60).collect::<String>()).collect::<Vec<_>>());
        let _ = std::fs::remove_dir_all(&dir);
        super::seen_in_browser(&served);
    }

    #[test]
    fn a_job_whose_label_says_spot_false_starts_on_an_aws_on_demand_machine_as_aws_answered() {
        let served = serve(|c| {
            (c.aws_own, c.aws_own_creds) = (Some(("111111111111".into(), "us-east-1".into())), Some(superci_core::aws::Credentials { access_key_id: "ASIAX".into(), secret_access_key: "secret".into(), session_token: Some("tok".into()), expires_at_ms: u64::MAX }));
        });
        served.aws.replaying(include_str!("../fixtures/aws/launch/on-demand.jsonl"), &[]);
        let dir = checkout_with(&[(".github/workflows/ci.yml", "jobs:\n  tests:\n    runs-on: superci/cpu=2/spot=false\n    steps:\n      - run: make test\n")]);
        let o = superci(&served, &dir, &["runs", "create", "--workflow=.github/workflows/ci.yml", "--detach"]).env("SUPERCI_STAND_IN", &served.aws.url).output().unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        let job = loop {
            if let Some((_, j)) = served.store.under("job:").into_iter().next().filter(|(_, j)| j["machine_id"].is_string() || j["state"] == "failed") { break j }
            std::thread::sleep(std::time::Duration::from_millis(50));
        };
        assert_eq!((job["cloud"].as_str(), job["on_demand"].as_bool(), job["machine_id"].as_str(), job["machine_type"].as_str(), job["zone"].as_str()), (Some("aws"), Some(true), Some("i-00000000000000002"), Some("m7a.large"), Some("us-east-1a")), "{job}\n{:?}", served.aws.unanswered.lock().unwrap());
        // (No spot price asked: an on-demand machine is asked by its types' order.)
        assert!(!served.aws.asked.lock().unwrap().iter().any(|a| a["asked"].as_str().unwrap_or_default().contains("DescribeSpotPriceHistory")));
        let _ = std::fs::remove_dir_all(&dir);
        super::seen_in_browser(&served);
    }

    /// A checkout with these files, committed.
    fn checkout_with(files: &[(&str, &str)]) -> std::path::PathBuf {
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!("superci-e2e-files-{}-{}-{}", std::process::id(), N.fetch_add(1, std::sync::atomic::Ordering::SeqCst), superci_core_free_id()));
        for (p, text) in files { let f = dir.join(p); std::fs::create_dir_all(f.parent().unwrap()).unwrap(); std::fs::write(f, text).unwrap(); }
        for args in [&["init", "-q"][..], &["add", "-A"], &["-c", "user.name=t", "-c", "user.email=t@example.com", "commit", "-qm", "t"]] { assert!(Command::new("git").current_dir(&dir).args(args).status().unwrap().success()) }
        dir
    }

    /// A runner's context data as plain JSON (`{t: 2, d: [{k, v}]}` a mapping, `{t: 1, a: […]}` a list).
    fn plain(v: &serde_json::Value) -> serde_json::Value {
        match v["t"].as_u64() {
            Some(2) => serde_json::Value::Object(v["d"].as_array().into_iter().flatten().map(|e| (e["k"].as_str().unwrap_or_default().to_string(), plain(&e["v"]))).collect()),
            Some(1) => serde_json::Value::Array(v["a"].as_array().into_iter().flatten().map(plain).collect()),
            _ => v.clone(),
        }
    }

    /// The runner of the job shown as `name`, once it is started.
    fn runner_of(served: &superci_testkit::Served, run: &str, name: &str) -> Runner {
        let end = std::time::Instant::now() + std::time::Duration::from_secs(20);
        loop {
            if let Some(r) = Runner::of_job(served, run, name) { return r }
            let names: Vec<String> = served.store.under(&format!("run:{run}")).into_iter().next().map(|(_, r)| r["jobs"].as_array().into_iter().flatten().map(|j| format!("{}:{}", j["name"], j["state"])).collect()).unwrap_or_default();
            assert!(std::time::Instant::now() < end, "no job {name} started: {names:?}");
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }

    #[test]
    fn a_job_that_calls_a_workflow_of_the_repository_runs_its_jobs_with_their_inputs_and_secrets_and_gives_its_outputs_on() {
        let served = serve(|_| {});
        let dir = checkout_with(&[
            (".github/workflows/ci.yml", "jobs:\n  build:\n    uses: ./.github/workflows/build.yml\n    with:\n      target: web\n    secrets: inherit\n  after:\n    needs: build\n    runs-on: superci\n    steps:\n      - run: echo ${{ needs.build.outputs.artifact }}\n"),
            (".github/workflows/build.yml", "on:\n  workflow_call:\n    inputs:\n      target:\n        type: string\n        required: true\n      fast:\n        type: boolean\n        default: true\n    outputs:\n      artifact:\n        value: ${{ jobs.compile.outputs.name }}\njobs:\n  compile:\n    runs-on: superci\n    outputs:\n      name: ${{ steps.c.outputs.name }}\n    steps:\n      - id: c\n        run: echo \"name=${{ inputs.target }}-bundle\" >> $GITHUB_OUTPUT\n  test:\n    needs: compile\n    runs-on: superci\n    steps:\n      - run: echo testing ${{ needs.compile.outputs.name }}\n"),
        ]);
        let o = superci(&served, &dir, &["runs", "create", "--workflow=.github/workflows/ci.yml", "--detach", "--secret=DEPLOY_TOKEN=s3cret-value"]).output().unwrap();
        assert!(o.status.success(), "{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
        let run = run_of(&served);
        // The called workflow's first job: its inputs (given and by default), and the run's secrets (inherited).
        let compile = runner_of(&served, &run, "build / compile");
        let message = compile.handed(&served);
        assert_eq!(plain(&message["contextData"]["inputs"]), serde_json::json!({ "target": "web", "fast": true }));
        assert_eq!(message["variables"]["DEPLOY_TOKEN"], serde_json::json!({ "value": "s3cret-value", "isSecret": true }));
        compile.take();
        compile.finish_with("succeeded", &[("name", "web-bundle")]);
        // Its next job, after it: what it needs named as in its own workflow.
        let test = runner_of(&served, &run, "build / test");
        assert_eq!(plain(&test.handed(&served)["contextData"]["needs"])["compile"]["outputs"]["name"], "web-bundle");
        test.take();
        test.finish("succeeded");
        // The job that needs the call: its result, and the called workflow's output made of its jobs'.
        let after = runner_of(&served, &run, "after");
        let needs = plain(&after.handed(&served)["contextData"]["needs"]);
        assert_eq!((needs["build"]["result"].as_str(), needs["build"]["outputs"]["artifact"].as_str()), (Some("success"), Some("web-bundle")), "{needs}");
        after.take();
        after.finish("succeeded");
        super::seen_in_browser(&served);
    }

    #[test]
    fn one_job_asked_for_brings_the_jobs_it_needs_a_matrix_with_nothing_in_it_runs_nothing_and_a_secret_not_kept_is_said() {
        let served = serve(|_| {});
        let dir = checkout_with(&[(".github/workflows/ci.yml", "jobs:\n  build:\n    runs-on: superci\n    steps:\n      - run: echo ${{ secrets.NOT_KEPT }}\n  none:\n    needs: build\n    runs-on: superci\n    strategy:\n      matrix:\n        include: []\n    steps:\n      - run: echo none\n  test:\n    needs: [build, none]\n    runs-on: superci\n    steps:\n      - run: echo test\n  other:\n    runs-on: superci\n    steps:\n      - run: echo other\n")]);
        let o = superci(&served, &dir, &["runs", "create", "--workflow=.github/workflows/ci.yml", "--job=test", "--detach"]).output().unwrap();
        let said = String::from_utf8_lossy(&o.stdout).to_string() + &String::from_utf8_lossy(&o.stderr);
        assert!(o.status.success(), "{said}");
        let run = run_of(&served);
        let kept = served.store.under(&format!("run:{run}"))[0].1.clone();
        let keys: Vec<&str> = kept["jobs"].as_array().unwrap().iter().filter_map(|j| j["key"].as_str()).collect();
        assert!(keys.contains(&"build") && keys.contains(&"test") && !keys.contains(&"other") && !keys.contains(&"none"), "{keys:?}");
        let _ = std::fs::remove_dir_all(&dir);
        super::seen_in_browser(&served);
    }

    #[test]
    fn a_job_past_its_timeout_fails_the_call_after_it_is_skipped_a_matrix_of_nothing_fails_and_a_cancel_not_heeded_ends_its_machine() {
        let served = serve(|_| {});
        let dir = checkout_with(&[
            (".github/workflows/ci.yml", "jobs:\n  first:\n    runs-on: superci\n    timeout-minutes: 1\n    outputs:\n      shards: ${{ steps.s.outputs.shards }}\n    steps:\n      - id: s\n        run: echo 'shards=[]' >> $GITHUB_OUTPUT\n  call:\n    needs: first\n    uses: ./.github/workflows/build.yml\n  shards:\n    needs: first\n    if: always()\n    runs-on: superci\n    strategy:\n      matrix:\n        shard: ${{ fromJSON(needs.first.outputs.shards || '[]') }}\n    steps:\n      - run: echo ${{ matrix.shard }}\n"),
            (".github/workflows/build.yml", "on:\n  workflow_call:\njobs:\n  compile:\n    runs-on: superci\n    steps:\n      - run: echo compile\n"),
        ]);
        // Past its timeout-minutes, its runner still going: its machine ended, it fails; the call after it is
        // skipped; the matrix read from it has nothing in it, and fails as GitHub fails it.
        let o = superci(&served, &dir, &["runs", "create", "--workflow=.github/workflows/ci.yml", "--detach"]).output().unwrap();
        assert!(o.status.success(), "{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
        let run = run_of(&served);
        let first = runner_of(&served, &run, "first");
        first.take();
        served.later(2 * 60_000);
        served.upkeep();
        let kept = served.store.under(&format!("run:{run}"))[0].1.clone();
        let names: Vec<String> = kept["jobs"].as_array().unwrap().iter().map(|j| format!("{} {} {}", j["name"].as_str().or(j["key"].as_str()).unwrap_or_default(), j["state"], j["result"])).collect();
        assert!(names.iter().any(|n| n.contains("call / compile") && n.contains("skipped")), "{names:?}");
        // Another run, cancelled while its first job runs and its runner does not stop it: ended after the grace time.
        let o = superci(&served, &dir, &["runs", "create", "--workflow=.github/workflows/ci.yml", "--detach"]).output().unwrap();
        assert!(o.status.success());
        let second = served.store.under("run:").into_iter().map(|(k, _)| k.trim_start_matches("run:").to_string()).find(|k| *k != run).unwrap();
        let r = runner_of(&served, &second, "first");
        r.take();
        let o = superci(&served, &dir, &["runs", "cancel", &second]).output().unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        served.later(10 * 60_000);
        served.upkeep();
        let kept = served.store.under(&format!("run:{second}"))[0].1.clone();
        assert!(kept["jobs"].as_array().unwrap().iter().all(|j| j["state"] == "done"), "{kept}");
        let _ = std::fs::remove_dir_all(&dir);
        super::seen_in_browser(&served);
    }

    #[test]
    fn a_job_in_an_environment_gets_its_secrets_a_call_names_what_it_gives_and_runs_on_another_runner_is_the_default_machine() {
        let served = serve(|_| {});
        let dir = checkout_with(&[
            (".github/workflows/ci.yml", "jobs:\n  deploy:\n    runs-on: ubuntu-latest\n    environment:\n      name: production\n    steps:\n      - run: echo ${{ secrets.DEPLOY_KEY }}\n  build:\n    needs: deploy\n    uses: ./.github/workflows/build.yml\n    secrets:\n      TOKEN: ${{ secrets.DEPLOY_KEY }}\n"),
            (".github/workflows/build.yml", "on:\n  workflow_call:\n    secrets:\n      TOKEN:\n        required: true\njobs:\n  compile:\n    runs-on: superci\n    steps:\n      - run: echo ${{ secrets.TOKEN }}\n"),
        ]);
        // Kept for production only, and for any run.
        for (environment, value) in [(Some("production"), "production-value"), (None, "any-value")] {
            let scope = superci_core::secrets::Scope { environment: environment.map(str::to_string), ..Default::default() };
            let stored = superci_core::secrets::Stored { name: "DEPLOY_KEY".into(), value: value.into(), scope: scope.clone(), updated_ms: 1 };
            served.aws.put(&format!("/superci/{}/{}", superci_testkit::PLANE, superci_core::secrets::setting("DEPLOY_KEY", &scope)), &serde_json::to_string(&stored).unwrap());
        }
        let o = superci(&served, &dir, &["runs", "create", "--workflow=.github/workflows/ci.yml", "--detach"]).output().unwrap();
        assert!(o.status.success(), "{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
        let run = run_of(&served);
        // Its environment's value; on the default machine (its runs-on names another runner).
        let deploy = runner_of(&served, &run, "deploy");
        let message = deploy.handed(&served);
        assert_eq!(message["variables"]["DEPLOY_KEY"]["value"], "production-value", "{}", message["variables"]);
        deploy.take();
        deploy.finish("succeeded");
        // The call's job: the secret its caller names, as it gives it.
        let compile = runner_of(&served, &run, "build / compile");
        assert_eq!(compile.handed(&served)["variables"]["TOKEN"]["value"], "any-value");
        compile.take();
        compile.finish("succeeded");
        let _ = std::fs::remove_dir_all(&dir);
        super::seen_in_browser(&served);
    }

    #[test]
    fn a_matrix_made_by_an_expression_is_read_once_the_job_it_needs_has_ended() {
        let served = serve(|_| {});
        let dir = checkout_with(&[(".github/workflows/ci.yml", "jobs:\n  setup:\n    runs-on: superci\n    outputs:\n      shards: ${{ steps.s.outputs.shards }}\n    steps:\n      - id: s\n        run: echo 'shards=[1,2,3]' >> $GITHUB_OUTPUT\n  test:\n    needs: setup\n    runs-on: superci\n    strategy:\n      matrix:\n        shard: ${{ fromJSON(needs.setup.outputs.shards) }}\n        os: [linux]\n    steps:\n      - run: echo shard ${{ matrix.shard }}\n")]);
        let o = superci(&served, &dir, &["runs", "create", "--workflow=.github/workflows/ci.yml", "--detach"]).output().unwrap();
        assert!(o.status.success(), "{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
        let run = run_of(&served);
        let setup = runner_of(&served, &run, "setup");
        setup.take();
        setup.finish_with("succeeded", &[("shards", "[1,2,3]")]);
        // Three jobs of it, each its combination.
        for shard in 1..=3 {
            let r = runner_of(&served, &run, &format!("test ({shard}, linux)"));
            assert_eq!(plain(&r.handed(&served)["contextData"]["matrix"]), serde_json::json!({ "shard": shard as f64, "os": "linux" }));
            r.take();
            r.finish("succeeded");
        }
        super::seen_in_browser(&served);
    }

    #[test]
    fn a_job_that_may_fail_lets_the_run_go_on_and_one_past_its_timeout_is_ended() {
        let served = serve(|_| {});
        let dir = checkout_with(&[(".github/workflows/ci.yml", "jobs:\n  flaky:\n    runs-on: superci\n    continue-on-error: true\n    steps:\n      - run: exit 1\n  after:\n    needs: flaky\n    runs-on: superci\n    timeout-minutes: 1\n    steps:\n      - run: sleep 600\n")]);
        let o = superci(&served, &dir, &["runs", "create", "--workflow=.github/workflows/ci.yml", "--detach"]).output().unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        let run = run_of(&served);
        // It fails; as on GitHub, the job that needs it sees a success and runs.
        let flaky = runner_of(&served, &run, "flaky");
        flaky.take();
        flaky.finish("failed");
        let after = runner_of(&served, &run, "after");
        assert_eq!(plain(&after.handed(&served)["contextData"]["needs"])["flaky"]["result"], "success");
        let record = served.store.under(&format!("run:{run}"))[0].1.clone();
        let flaky_said = record["jobs"].as_array().unwrap().iter().find(|j| j["name"] == "flaky").unwrap().clone();
        assert_eq!((flaky_said["result"].as_str(), flaky_said["failed"].as_bool()), (Some("success"), Some(true)), "{flaky_said}");
        // It runs past its minute: the upkeep ends it, and says why.
        after.take();
        after.steps(&[("eeeeeeee-0000-0000-0000-000000000005", "Run sleep 600", 1, "inProgress", None, None)]);
        served.later(30_000);
        served.upkeep();
        assert!(served.store.under("job:").iter().all(|(_, j)| j["error"].as_str().is_none_or(|e| !e.contains("timeout-minutes"))), "not yet");
        served.later(60_000);
        served.upkeep();
        let ended = served.store.under("job:").into_iter().map(|(_, j)| j).find(|j| j["name"] == "after").unwrap();
        assert_eq!((ended["state"].as_str(), ended["error"].as_str()), (Some("failed"), Some("it ran past its timeout-minutes (1), and its machine was ended")), "{ended}");
        let record = served.store.under(&format!("run:{run}"))[0].1.clone();
        assert_eq!(record["jobs"].as_array().unwrap().iter().find(|j| j["name"] == "after").unwrap()["result"], "failure");
        super::seen_in_browser(&served);
    }

    #[test]
    fn a_job_whose_spot_machine_aws_took_back_is_started_again_off_spot_and_the_run_goes_on() {
        let served = serve(|_| {});
        let dir = checkout();
        let cli = superci(&served, &dir, &["runs", "create", "--workflow=.github/workflows/ci.yml", "--quiet"]).spawn().unwrap();
        let run = run_of(&served);
        let first = loop { if let Some(r) = Runner::of_run(&served, &run) { break r } std::thread::sleep(std::time::Duration::from_millis(50)) };
        let token = |served: &superci_testkit::Served| served.store.under(&format!("run:{run}"))[0].1["jobs"][0]["token"].as_str().unwrap_or_default().to_string();
        let was = token(&served);
        first.take();
        first.steps(&[("eeeeeeee-0000-0000-0000-000000000005", "Run make", 1, "inProgress", None, None)]);
        // AWS takes its spot machine back (its machine said so, with its link: here, noted on its job), and the runner
        // ends the job as failed.
        let key = served.store.under("job:").into_iter().map(|(k, _)| k).find(|k| !k.contains(':') || k.starts_with("job:")).unwrap();
        { let mut m = served.store.0.lock().unwrap(); let mut j: serde_json::Value = serde_json::from_str(&m[&key]).unwrap(); j["interrupted"] = true.into(); m.insert(key.clone(), j.to_string()); }
        first.finish("failed");
        // Started again, on a machine of its own off spot machines: a new job of the run, and the first said so.
        let end = std::time::Instant::now() + std::time::Duration::from_secs(15);
        while token(&served) == was || token(&served).is_empty() { assert!(std::time::Instant::now() < end, "not started again: {}", served.store.under(&format!("run:{run}"))[0].1); std::thread::sleep(std::time::Duration::from_millis(50)) }
        let first_job = served.store.under(&key)[0].1.clone();
        assert!(first_job["error"].as_str().unwrap_or_default().contains("SuperCI starts the job again by itself, off spot machines") && first_job.get("rerun").is_none(), "{first_job}");
        let again = served.store.under("job:").into_iter().find(|(k, _)| *k != key).map(|(_, j)| j).unwrap();
        assert_eq!(again["no_spot"], true, "{again}");
        play(&served, &run, &["ok".to_string()], "succeeded");
        let out = cli.wait_with_output().unwrap();
        let said = String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success() && said.contains("The run ended: success"), "{said}");
        // Its summary says it was started again, and which job its first attempt was.
        let first_id = first_job["job_id"].to_string();
        assert!(said.contains(&format!("Started again on an on-demand machine: AWS took back its spot machine (its first attempt was job {first_id}).")), "{said}");
        let _ = std::fs::remove_dir_all(&dir);
        super::seen_in_browser(&served);
    }

    #[test]
    fn a_job_taken_back_that_succeeded_anyway_is_not_started_again_nor_one_started_again_already() {
        let served = serve(|_| {});
        let dir = checkout();
        let interrupted = |served: &superci_testkit::Served, job: &str| { let mut m = served.store.0.lock().unwrap(); let mut j: serde_json::Value = serde_json::from_str(&m[job]).unwrap(); j["interrupted"] = true.into(); m.insert(job.to_string(), j.to_string()); };
        let jobs_of = |served: &superci_testkit::Served, run: &str| served.store.under("job:").into_iter().filter(|(_, j)| j["run_id"].as_u64().is_some_and(|r| r.to_string() == run)).map(|(k, _)| k).collect::<Vec<_>>();
        // Its machine taken back, but the job had succeeded: the run is over, a success, with that one job.
        let cli = superci(&served, &dir, &["runs", "create", "--workflow=.github/workflows/ci.yml", "--quiet"]).spawn().unwrap();
        let run = run_of(&served);
        let first = loop { if let Some(r) = Runner::of_run(&served, &run) { break r } std::thread::sleep(std::time::Duration::from_millis(50)) };
        first.take();
        interrupted(&served, &jobs_of(&served, &run)[0]);
        first.finish("succeeded");
        let said = String::from_utf8_lossy(&cli.wait_with_output().unwrap().stdout).into_owned();
        assert!(said.contains("The run ended: success") && jobs_of(&served, &run).len() == 1, "{said}");
        // Started again off spot once, and taken back again (an on-demand machine is not, but its record may say so): it
        // fails, not a third time.
        let cli = superci(&served, &dir, &["runs", "create", "--workflow=.github/workflows/ci.yml", "--quiet"]).spawn().unwrap();
        let second = loop { if let Some((k, _)) = served.store.under("run:").into_iter().find(|(k, _)| *k != format!("run:{run}")) { break k.trim_start_matches("run:").to_string() } std::thread::sleep(std::time::Duration::from_millis(50)) };
        let runner = loop { if let Some(r) = Runner::of_run(&served, &second) { break r } std::thread::sleep(std::time::Duration::from_millis(50)) };
        runner.take();
        interrupted(&served, &jobs_of(&served, &second)[0]);
        runner.finish("failed");
        let end = std::time::Instant::now() + std::time::Duration::from_secs(15);
        while jobs_of(&served, &second).len() < 2 { assert!(std::time::Instant::now() < end, "not started again"); std::thread::sleep(std::time::Duration::from_millis(50)) }
        let again = loop { if let Some(r) = Runner::of_run(&served, &second) { if r.at != runner.at { break r } } std::thread::sleep(std::time::Duration::from_millis(50)) };
        again.take();
        let off_spot = jobs_of(&served, &second).into_iter().find(|k| served.store.under(k)[0].1["no_spot"] == true).unwrap();
        interrupted(&served, &off_spot);
        again.finish("failed");
        let out = cli.wait_with_output().unwrap();
        let said = String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
        assert!(said.contains("The run ended: failure") && jobs_of(&served, &second).len() == 2, "{} jobs; {}\n{said}", jobs_of(&served, &second).len(), served.store.under(&format!("run:{second}"))[0].1);
        let _ = std::fs::remove_dir_all(&dir);
        super::seen_in_browser(&served);
    }

    #[test]
    fn a_run_from_a_computer_is_cancelled_its_running_job_told_to_stop_the_job_waiting_for_it_never_started_and_an_always_job_runs() {
        let served = serve(|_| {});
        let dir = checkout_with(&[(".github/workflows/ci.yml", "jobs:\n  build:\n    runs-on: superci\n    steps:\n      - run: sleep 600\n  after:\n    needs: build\n    runs-on: superci\n    steps:\n      - run: echo after\n  report:\n    needs: build\n    if: always()\n    runs-on: superci\n    steps:\n      - run: echo report\n")]);
        let o = superci(&served, &dir, &["runs", "create", "--workflow=.github/workflows/ci.yml", "--detach"]).output().unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        let run = run_of(&served);
        let build = runner_of(&served, &run, "build");
        let message = build.handed(&served);
        build.take();
        build.steps(&[("ffffffff-0000-0000-0000-000000000006", "Run sleep 600", 1, "inProgress", None, None)]);
        // Cancelled, with the key that started it.
        let o = superci(&served, &dir, &["runs", "cancel", &run]).output().unwrap();
        let said = String::from_utf8_lossy(&o.stdout).to_string() + &String::from_utf8_lossy(&o.stderr);
        assert!(o.status.success() && said.contains(&format!("Cancelled run {run}")), "{said}");
        // The running one's runner is told, as GitHub tells it, once; the jobs waiting for it wait on.
        let jobs = || served.store.under(&format!("run:{run}"))[0].1["jobs"].clone();
        let of = |key: &str| jobs().as_array().unwrap().iter().find(|j| j["key"] == key).cloned().unwrap();
        assert_eq!(of("after")["state"], "pending");
        let (status, told) = build.message();
        assert_eq!((status, told["messageType"].as_str()), (200, Some("JobCancellation")), "{told}");
        let body: serde_json::Value = serde_json::from_str(told["body"].as_str().unwrap()).unwrap();
        assert_eq!((body["jobId"].clone(), body["timeout"].as_str()), (message["jobId"].clone(), Some("00:05:00")));
        assert_eq!(build.message().0, 204, "told once");
        // It stops its steps and ends cancelled. As on GitHub, the job that waited for it ends cancelled, never started,
        // and the one whose `if` holds after a cancel (`always()`) runs; then the run is over, cancelled.
        build.finish("canceled");
        let after = of("after");
        assert_eq!((after["state"].as_str(), after["result"].as_str(), after.get("token")), (Some("done"), Some("cancelled"), None), "{after}");
        let report = runner_of(&served, &run, "report");
        report.take();
        report.finish("succeeded");
        let o = superci(&served, &dir, &["runs", "wait", &run, "--quiet"]).output().unwrap();
        let said = String::from_utf8_lossy(&o.stdout).to_string() + &String::from_utf8_lossy(&o.stderr);
        assert!(said.contains("The run ended: cancelled"), "{said}");
        assert_eq!(superci(&served, &dir, &["runs", "cancel", &run]).output().map(|o| String::from_utf8_lossy(&o.stdout).contains("was over already")).ok(), Some(true));
        let _ = std::fs::remove_dir_all(&dir);
        super::seen_in_browser(&served);
    }

    #[test]
    fn a_run_cancelled_before_its_machine_began_its_job_ends_that_machine_at_once() {
        let served = serve(|_| {});
        let dir = checkout();
        let o = superci(&served, &dir, &["runs", "create", "--workflow=.github/workflows/ci.yml", "--detach"]).output().unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        let run = run_of(&served);
        let _ = runner_of(&served, &run, "tests");
        let o = superci(&served, &dir, &["runs", "cancel", &run]).output().unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        let job = served.store.under("job:").into_iter().map(|(_, j)| j).find(|j| j["provider"] == "local").unwrap();
        assert_eq!((job["state"].as_str(), job["ended_ms"].is_u64()), (Some("cancelled"), true), "{job}");
        let jobs = served.store.under(&format!("run:{run}"))[0].1["jobs"].clone();
        assert_eq!((jobs[0]["state"].as_str(), jobs[0]["result"].as_str()), (Some("done"), Some("cancelled")), "{jobs}");
        let _ = std::fs::remove_dir_all(&dir);
        super::seen_in_browser(&served);
    }

    #[test]
    fn a_called_workflows_required_input_not_given_is_said_and_nothing_runs() {
        let served = serve(|_| {});
        let dir = checkout_with(&[
            (".github/workflows/ci.yml", "jobs:\n  build:\n    uses: ./.github/workflows/build.yml\n    with:\n      fast: false\n"),
            (".github/workflows/build.yml", "on:\n  workflow_call:\n    inputs:\n      target:\n        type: string\n        required: true\n      fast:\n        type: boolean\n        default: true\njobs:\n  compile:\n    runs-on: superci\n    steps:\n      - run: make ${{ inputs.target }}\n"),
        ]);
        let o = superci(&served, &dir, &["runs", "create", "--workflow=.github/workflows/ci.yml", "--quiet"]).output().unwrap();
        let said = String::from_utf8_lossy(&o.stdout).to_string() + &String::from_utf8_lossy(&o.stderr);
        assert!(!o.status.success() && said.contains("the called workflow's input target is required, and not given"), "{said}");
        assert!(served.machines.0.lock().unwrap().is_empty(), "no machine started");
        let _ = std::fs::remove_dir_all(&dir);
        super::seen_in_browser(&served);
    }

    /// A plain GET on this computer (the control plane, or the stand-in for S3): its status, its `location`, its body.
    fn fetch(url: &str) -> (u16, String, Vec<u8>) { http("GET", url, &[]) }

    /// A request on this computer: its status, its `location`, its body.
    fn http(method: &str, url: &str, body: &[u8]) -> (u16, String, Vec<u8>) {
        use std::io::{Read, Write};
        let u = url.trim_start_matches("http://");
        let (host, rest) = u.split_once('/').unwrap_or((u, ""));
        let mut s = std::net::TcpStream::connect(host).unwrap();
        s.write_all(format!("{method} /{rest} HTTP/1.1\r\nhost: {host}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n", body.len()).as_bytes()).unwrap();
        s.write_all(body).unwrap();
        let mut out = vec![];
        let _ = s.read_to_end(&mut out);
        let at = out.windows(4).position(|w| w == b"\r\n\r\n").unwrap_or(out.len());
        let head = String::from_utf8_lossy(&out[..at]).to_string();
        let location = head.lines().find_map(|l| l.strip_prefix("location: ").or_else(|| l.strip_prefix("Location: "))).unwrap_or_default().to_string();
        (head.get(9..12).and_then(|c| c.parse().ok()).unwrap_or(0), location, out.get(at + 4..).unwrap_or_default().to_vec())
    }

    /// What a gzipped tar holds, unpacked as text (its headers name its files; small files' text follows).
    fn gunzipped(bytes: &[u8]) -> String {
        // (macOS tar pads what it gzips with zeros, which gzip warns of.)
        let mut gz = Command::new("gzip").arg("-dc").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
        { use std::io::Write; gz.stdin.take().unwrap().write_all(bytes).unwrap(); }
        String::from_utf8_lossy(&gz.wait_with_output().unwrap().stdout).replace('\0', " ")
    }

    #[test]
    fn a_second_run_from_the_same_checkout_sends_only_what_changed_on_top_of_the_tree_kept_in_the_bucket() {
        // A control plane with its own AWS account, whose bucket is ready (its cache program is there), machines its containers.
        let served = serve(|c| {
            (c.aws_own, c.aws_own_creds) = (Some(("111111111111".into(), "us-east-1".into())), Some(superci_core::aws::Credentials { access_key_id: "ASIAX".into(), secret_access_key: "secret".into(), session_token: Some("tok".into()), expires_at_ms: u64::MAX }));
            c.routing.order = vec![superci_core::plane::Pool::of("cloudflare")];
        });
        let bucket = superci_core::cache::bucket(superci_testkit::PLANE, "us-east-1");
        served.github.put_object(&format!("{bucket}/{}", superci_core::cache::program_key("x86_64")), b"program");
        let big: String = (0..4000).map(|n| format!("line {n} of a file that stays as it is\n")).collect();
        let dir = checkout_with(&[(".github/workflows/ci.yml", "jobs:\n  tests:\n    runs-on: superci\n    steps:\n      - run: make test\n"), ("big.txt", &big), ("small.txt", "one\n"), ("gone.txt", "soon gone\n")]);
        assert!(Command::new("git").current_dir(&dir).args(["remote", "add", "origin", "git@github.com:acme/app.git"]).status().unwrap().success());
        let home = common::temp("runs-base");
        let run = || {
            let o = superci(&served, &dir, &["runs", "create", "--workflow=.github/workflows/ci.yml", "--detach"]).env("SUPERCI_HOME", &home).env("SUPERCI_STAND_IN", &served.aws.url).output().unwrap();
            let said = String::from_utf8_lossy(&o.stdout).to_string() + &String::from_utf8_lossy(&o.stderr);
            assert!(o.status.success(), "{said}");
            // ("Run ID of … started.")
            let id = said.lines().find_map(|l| l.strip_prefix("Run ")).and_then(|l| l.split_whitespace().next()).unwrap_or_default().to_string();
            (said, id)
        };
        // The first: all of it, into the bucket, kept there as the base for the next.
        let (said, first) = run();
        assert!(said.contains("Sending the working tree (") && !said.contains("part"), "{said}");
        let kept: Vec<String> = served.github.objects().into_iter().map(|(k, _)| k).filter(|k| k.contains("/bases/")).collect();
        assert_eq!(kept.len(), 1, "{:?}", served.github.objects());
        // The run is of the repository its checkout's origin is on GitHub.
        assert_eq!(served.store.under(&format!("run:{first}"))[0].1["repository"], "acme/app");
        // The second, after a file changed and one removed: only those, on top of the base.
        std::fs::write(dir.join("small.txt"), "two\n").unwrap();
        std::fs::remove_file(dir.join("gone.txt")).unwrap();
        let (said, second) = run();
        assert!(said.contains("Sending what changed since this folder's last run, just now: 1 file, 1 removed ("), "{said}");
        // Its machine's checkout: the base first, then what changed and what was removed.
        let runner = loop { if let Some(r) = Runner::of_job(&served, &second, "tests") { break r } std::thread::sleep(std::time::Duration::from_millis(50)) };
        let tree = runner.at.trim_end_matches("/_apis/v1").to_string() + "/superci/tree";
        let part = |n: u32| {
            let (status, link, _) = fetch(&format!("{tree}?part={n}"));
            assert_eq!(status, 302, "part {n}");
            let (status, _, bytes) = fetch(&format!("{}/{}", served.aws.url, link.trim_start_matches("https://")));
            assert_eq!(status, 200, "{link}");
            gunzipped(&bytes)
        };
        let (base, changed) = (part(0), part(1));
        // (Sent while its machine started: the run was made first, and its machine told to wait until the computer said
        // all of it was there.)
        assert!(served.store.under("rtree-later:").is_empty(), "the computer said it was all sent");
        let token = runner.at.trim_end_matches("/_apis/v1").rsplit('/').next().unwrap().to_string();
        let handed = served.store.under(&format!("rjob:{token}"))[0].1.clone();
        let tree_id = handed["tree"].as_str().unwrap().to_string();
        let at = served.store.under(&format!("rtree-at:{tree_id}"))[0].1.to_string().trim_matches('"').to_string();
        served.store.0.lock().unwrap().insert(format!("rtree-later:{tree_id}"), at);
        assert_eq!(fetch(&format!("{tree}?part=0")).0, 425, "still on its way");
        served.store.0.lock().unwrap().remove(&format!("rtree-later:{tree_id}"));
        assert!(base.contains("big.txt") && base.contains("soon gone") && base.contains("one\n"), "the base as it was");
        assert!(changed.contains("small.txt") && changed.contains("two\n") && !changed.contains("big.txt") && changed.contains(".superci-removed") && changed.contains("gone.txt\n"), "{changed}");
        // The next: only what changed since that run (the file removed then, back now), as a layer on that run's.
        std::fs::write(dir.join("gone.txt"), "back\n").unwrap();
        let (said, third) = run();
        assert!(said.contains("since this folder's last run, just now: 1 file, 0 removed ("), "{said}");
        let runner = loop { if let Some(r) = Runner::of_job(&served, &third, "tests") { break r } std::thread::sleep(std::time::Duration::from_millis(50)) };
        let tree = runner.at.trim_end_matches("/_apis/v1").to_string() + "/superci/tree";
        let part = |n: u32| {
            let (status, link, _) = fetch(&format!("{tree}?part={n}"));
            assert_eq!(status, 302, "part {n}");
            gunzipped(&fetch(&format!("{}/{}", served.aws.url, link.trim_start_matches("https://"))).2)
        };
        let (base_again, first_layer, second_layer) = (part(0), part(1), part(2));
        assert!(base_again == base && first_layer == changed, "the base and the layer before, as they were");
        assert!(second_layer.contains("gone.txt") && second_layer.contains("back\n") && !second_layer.contains("small.txt"), "{second_layer}");
        let token = runner.at.trim_end_matches("/_apis/v1").rsplit('/').next().unwrap().to_string();
        let handed = served.store.under(&format!("rjob:{token}"))[0].1.clone();
        assert_eq!((handed["parts"].as_u64(), handed["layered"].as_bool()), (Some(3), Some(true)), "three parts, each on the one before");
        // Most of the tree changed: all of it again, kept as the next base.
        std::fs::write(dir.join("big.txt"), big.replace("stays", "moved")).unwrap();
        let (said, _) = run();
        assert!(said.contains("Sending the working tree (") && !said.contains("what changed"), "{said}");
        // The base kept gone from the bucket (its own rule removes old ones): all of it again.
        for (k, _) in served.github.objects().into_iter().filter(|(k, _)| k.contains("/bases/")) { served.github.remove_object(&k) }
        std::fs::write(dir.join("small.txt"), "three\n").unwrap();
        let (said, _) = run();
        assert!(said.contains("Sending the working tree (") && !said.contains("what changed"), "{said}");
        // Two changed since that base, nothing removed.
        std::fs::write(dir.join("small.txt"), "four\n").unwrap();
        std::fs::write(dir.join("new.txt"), "new\n").unwrap();
        let (said, _) = run();
        assert!(said.contains("Sending what changed since this folder's last run, just now: 2 files, 0 removed ("), "{said}");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&home);
        super::seen_in_browser(&served);
    }

    #[test]
    fn a_working_tree_the_bucket_refused_cancels_its_run_and_says_why() {
        let served = serve(|c| {
            (c.aws_own, c.aws_own_creds) = (Some(("111111111111".into(), "us-east-1".into())), Some(superci_core::aws::Credentials { access_key_id: "ASIAX".into(), secret_access_key: "secret".into(), session_token: Some("tok".into()), expires_at_ms: u64::MAX }));
            c.routing.order = vec![superci_core::plane::Pool::of("cloudflare")];
        });
        let bucket = superci_core::cache::bucket(superci_testkit::PLANE, "us-east-1");
        served.github.put_object(&format!("{bucket}/{}", superci_core::cache::program_key("x86_64")), b"program");
        served.github.refuse_puts(true);
        let dir = checkout();
        let o = superci(&served, &dir, &["runs", "create", "--workflow=.github/workflows/ci.yml", "--detach"]).env("SUPERCI_STAND_IN", &served.aws.url).output().unwrap();
        let said = String::from_utf8_lossy(&o.stdout).to_string() + &String::from_utf8_lossy(&o.stderr);
        let run = run_of(&served);
        assert!(!o.status.success() && said.contains("The working tree was not sent (the bucket said 403") && said.contains(&format!("run {run} was cancelled")), "{said}");
        assert_eq!(served.store.under(&format!("run:{run}"))[0].1["cancelled"], true);
        let _ = std::fs::remove_dir_all(&dir);
        super::seen_in_browser(&served);
    }

    #[test]
    fn what_a_run_from_a_computer_cannot_start_or_follow_is_said() {
        let served = serve(|_| {});
        let said = |dir: &std::path::Path, args: &[&str]| { let o = superci(&served, dir, args).output().unwrap(); (o.status.success(), String::from_utf8_lossy(&o.stdout).to_string() + &String::from_utf8_lossy(&o.stderr)) };
        // Not in a git checkout.
        let bare = common::temp("runs-bare");
        std::fs::create_dir_all(bare.join(".github/workflows")).unwrap();
        std::fs::write(bare.join(".github/workflows/ci.yml"), "jobs:\n  t:\n    runs-on: superci\n    steps:\n      - run: make\n").unwrap();
        let (ok, out) = said(&bare, &["runs", "create", "--workflow=.github/workflows/ci.yml"]);
        assert!(!ok && out.contains("Run it in a git checkout"), "{out}");
        // A workflow that is not there; one whose job calls a workflow that is not there.
        let dir = checkout_with(&[(".github/workflows/ci.yml", "jobs:\n  build:\n    uses: ./.github/workflows/build.yml\n  again:\n    uses: ./.github/workflows/build.yml\n  other:\n    uses: acme/shared/.github/workflows/x.yml@main\n")]);
        let (ok, out) = said(&dir, &["runs", "create", "--workflow=.github/workflows/nope.yml"]);
        assert!(!ok && out.contains(".github/workflows/nope.yml:"), "{out}");
        let (ok, out) = said(&dir, &["runs", "create", "--workflow=.github/workflows/ci.yml"]);
        assert!(!ok && out.contains(".github/workflows/build.yml, which a job calls:"), "{out}");
        assert!(served.machines.0.lock().unwrap().is_empty(), "no machine started");
        // Following: a run named wrong, and one that is not there.
        let (ok, out) = said(&dir, &["runs", "wait", "abc"]);
        assert!(!ok && out.contains("Say which run: `superci runs wait ID`"), "{out}");
        let (ok, out) = said(&dir, &["runs", "wait", "999"]);
        assert!(!ok && out.contains("No run 999 among this control plane's runs from a computer"), "{out}");
        // Its artifacts: of a run that is not there; one a run does not have.
        let (ok, out) = said(&dir, &["artifacts", "list", "--run=999"]);
        assert!(!ok && out.contains("No such run among this control plane's runs from a computer"), "{out}");
        let real = checkout();
        let o = superci(&served, &real, &["runs", "create", "--workflow=.github/workflows/ci.yml", "--detach"]).output().unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        let run = run_of(&served);
        let (ok, out) = said(&real, &["artifacts", "list", &format!("--run={run}")]);
        assert!(ok && out.contains("No artifacts"), "{out}");
        let (ok, out) = said(&real, &["artifacts", "retrieve", "report", &format!("--run={run}")]);
        assert!(!ok && out.contains("The run has no artifact report."), "{out}");
        // A run with no jobs here, asked for its job; a run cancelled that is not here.
        let (ok, out) = said(&real, &["jobs", "retrieve", "--run=999"]);
        assert!(!ok && out.contains("No jobs of run 999 among the latest"), "{out}");
        let (ok, out) = said(&real, &["jobs", "retrieve", "--run=999", "--job=tests"]);
        assert!(!ok && out.contains("No jobs of run 999 among the latest"), "{out}");
        let (ok, out) = said(&real, &["runs", "cancel", "999"]);
        assert!(!ok && out.contains("No run 999 here"), "{out}");
        // A control plane's address that is not one, and a key it does not take.
        let o = superci(&served, &real, &["jobs", "list"]).env("SUPERCI_PLANE", "ftp://plane.example").output().unwrap();
        assert!(!o.status.success() && String::from_utf8_lossy(&o.stderr).contains("SUPERCI_PLANE is the control plane's address"), "{}", String::from_utf8_lossy(&o.stderr));
        let o = superci(&served, &real, &["jobs", "list"]).env("SUPERCI_KEY", "superci_read_not-a-key-of-its-own").output().unwrap();
        assert!(!o.status.success() && String::from_utf8_lossy(&o.stderr).contains("did not take SUPERCI_KEY"), "{}", String::from_utf8_lossy(&o.stderr));
        for d in [bare, dir, real] { let _ = std::fs::remove_dir_all(d); }
        super::seen_in_browser(&served);
    }

    #[test]
    fn a_steps_lines_that_came_late_are_printed_from_its_log_without_the_runners_times_and_others_as_they_are() {
        let served = serve(|_| {});
        let dir = checkout();
        let cli = superci(&served, &dir, &["runs", "create", "--workflow=.github/workflows/ci.yml"]).spawn().unwrap();
        let run = run_of(&served);
        let runner = loop { if let Some(r) = Runner::of_run(&served, &run) { break r } std::thread::sleep(std::time::Duration::from_millis(50)) };
        let a = "dddddddd-0000-0000-0000-000000000004";
        runner.take();
        runner.steps(&[(a, "Run make test", 1, "inProgress", None, None)]);
        runner.lines(a, 1, &["first".to_string()]);
        std::thread::sleep(std::time::Duration::from_millis(1800));
        // Its whole log: the first line with the runner's time (and the mark a log begins with), then lines that only
        // look like times: digits with no dash, a date with no time, letters, a time with nothing after it near.
        let log = ["\u{feff}2026-10-15T10:00:01.0000000Z first", "12345678901234567890123 counted", "2026-10-15 a date, no time, then more", "abcd-ef-ghTxxxxxxxxxxxx letters", "2026-10-15T10:00:02.0000000Zno space after the time at all here", "short"].join("\n");
        runner.log(1, &log);
        runner.steps(&[(a, "Run make test", 1, "completed", Some("succeeded"), Some(1))]);
        runner.finish("succeeded");
        let out = cli.wait_with_output().unwrap();
        let said = String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "{said}");
        for l in ["12345678901234567890123 counted", "2026-10-15 a date, no time, then more", "abcd-ef-ghTxxxxxxxxxxxx letters", "2026-10-15T10:00:02.0000000Zno space after the time at all here", "short"] {
            assert!(said.lines().any(|s| s.trim_end().ends_with(l)), "{l:?} as it is in:\n{said}");
        }
        assert_eq!(said.lines().filter(|s| s.trim_end().ends_with("first")).count(), 1, "{said}");
        let _ = std::fs::remove_dir_all(&dir);
        super::seen_in_browser(&served);
    }

    /// A zip as `upload-artifact` makes one (stored), of these files.
    fn zip_of(files: &[(&str, &[u8])]) -> Vec<u8> {
        let (mut out, mut dir) = (vec![], vec![]);
        for (name, data) in files {
            let at = out.len() as u32;
            let (n, size) = (name.len() as u16, data.len() as u32);
            out.extend([0x50, 0x4b, 0x03, 0x04, 20, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
            out.extend(size.to_le_bytes()); out.extend(size.to_le_bytes()); out.extend(n.to_le_bytes()); out.extend(0u16.to_le_bytes());
            out.extend(name.as_bytes()); out.extend(*data);
            dir.extend([0x50, 0x4b, 0x01, 0x02, 20, 0, 20, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
            dir.extend(size.to_le_bytes()); dir.extend(size.to_le_bytes()); dir.extend(n.to_le_bytes());
            dir.extend([0u8; 12]); dir.extend(at.to_le_bytes()); dir.extend(name.as_bytes());
        }
        let (cd_at, cd_len, count) = (out.len() as u32, dir.len() as u32, files.len() as u16);
        out.extend(dir);
        out.extend([0x50, 0x4b, 0x05, 0x06, 0, 0, 0, 0]);
        out.extend(count.to_le_bytes()); out.extend(count.to_le_bytes()); out.extend(cd_len.to_le_bytes()); out.extend(cd_at.to_le_bytes()); out.extend([0, 0]);
        out
    }

    #[test]
    fn a_run_from_a_computer_restores_what_github_jobs_saved_once_its_repository_is_known_and_says_when_its_cache_is_its_own() {
        // GitHub's App connected (the repository known by its id), the bucket ready.
        let served = serve(|c| {
            (c.aws_own, c.aws_own_creds) = (Some(("111111111111".into(), "us-east-1".into())), Some(superci_core::aws::Credentials { access_key_id: "ASIAX".into(), secret_access_key: "secret".into(), session_token: Some("tok".into()), expires_at_ms: u64::MAX }));
            c.routing.order = vec![superci_core::plane::Pool::of("cloudflare")];
        });
        served.connect_github();
        let bucket = superci_core::cache::bucket(superci_testkit::PLANE, "us-east-1");
        served.github.put_object(&format!("{bucket}/{}", superci_core::cache::program_key("x86_64")), b"program");
        let gh_link = "machine-cache-link-000000000003";
        served.store.0.lock().unwrap().insert(format!("cachem:{gh_link}"), serde_json::json!({ "region": "us-east-1", "arch": "x86_64", "until": u64::MAX / 4, "runner": "a-runner" }).to_string());
        let cache = |link: &str, what: &str, body: serde_json::Value| { let (s, _, b) = http("POST", &format!("{}/cache/{link}/{what}", served.url), body.to_string().as_bytes()); (s, serde_json::from_slice::<serde_json::Value>(&b).unwrap_or_default()) };
        let s3 = |method: &str, url: &str, body: &[u8]| http(method, &format!("{}/{}", served.aws.url, url.trim_start_matches("https://")), body);
        let version = "a".repeat(64);
        // A GitHub job on main saves the test times.
        let main = superci_testkit::GitHub::job_token(&serde_json::json!({ "repository_id": "197814629", "ac": serde_json::json!([{ "Scope": "refs/heads/main", "Permission": 3 }]).to_string(), "exp": 4_102_444_800u64 }));
        assert_eq!(cache(gh_link, "begin", serde_json::json!({ "token": main, "key": "test-times-1", "version": version })).1["ok"], true);
        let (_, signed) = cache(gh_link, "sign", serde_json::json!({ "token": main, "key": "test-times-1", "version": version, "names": ["p00000"] }));
        assert_eq!(s3("PUT", signed["urls"][0].as_str().unwrap(), b"times").0, 200);
        assert_eq!(cache(gh_link, "finish", serde_json::json!({ "token": main, "key": "test-times-1", "version": version, "pieces": [["p00000", 5]] })).1["size"], 5);
        // A checkout with no GitHub remote (a copy of the repository): its cache is its own, and it says so.
        let dir = checkout();
        let o = superci(&served, &dir, &["runs", "create", "--workflow=.github/workflows/ci.yml", "--detach"]).env("SUPERCI_STAND_IN", &served.aws.url).output().unwrap();
        let said = String::from_utf8_lossy(&o.stdout).to_string();
        assert!(o.status.success() && said.contains("Its cache is its own: this checkout has no GitHub remote") && said.contains("--repo=OWNER/NAME"), "{said}{}", String::from_utf8_lossy(&o.stderr));
        let first = run_of(&served);
        assert!(served.store.under(&format!("run:{first}"))[0].1["cache"]["ns"].as_str().unwrap_or_default().starts_with("local-"));
        let o = superci(&served, &dir, &["runs", "cancel", &first]).output().unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        // A --repo that is no repository: refused, nothing started.
        let o = superci(&served, &dir, &["runs", "create", "--workflow=.github/workflows/ci.yml", "--repo=acme", "--detach"]).output().unwrap();
        assert!(!o.status.success() && String::from_utf8_lossy(&o.stderr).contains("--repo is a GitHub repository, OWNER/NAME"), "{}", String::from_utf8_lossy(&o.stderr));
        // Named with --repo (as its origin would say it): the repository's entries, as GitHub's jobs saved them.
        let o = superci(&served, &dir, &["runs", "create", "--workflow=.github/workflows/ci.yml", "--repo=acme/app", "--detach"]).env("SUPERCI_STAND_IN", &served.aws.url).output().unwrap();
        assert!(o.status.success() && !String::from_utf8_lossy(&o.stdout).contains("Its cache is its own"), "{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
        let run = served.store.under("run:").into_iter().map(|(k, _)| k.trim_start_matches("run:").to_string()).find(|k| *k != first).unwrap();
        let runner = loop { if let Some(r) = Runner::of_run(&served, &run) { break r } std::thread::sleep(std::time::Duration::from_millis(50)) };
        let token = runner.handed(&served)["resources"]["endpoints"][0]["authorization"]["parameters"]["AccessToken"].as_str().unwrap().to_string();
        let machine = served.machines.0.lock().unwrap().last().unwrap().clone();
        let link = "machine-cache-link-000000000004";
        served.store.0.lock().unwrap().insert(format!("cachem:{link}"), serde_json::json!({ "region": "us-east-1", "arch": "x86_64", "until": u64::MAX / 4, "runner": machine }).to_string());
        let kept = served.store.under(&format!("run:{run}"))[0].1.clone();
        let (_, hit) = cache(link, "lookup", serde_json::json!({ "token": token, "key": "test-times-2", "restore_keys": ["test-times-"], "version": version }));
        assert_eq!((hit["hit"].as_bool(), hit["key"].as_str()), (Some(true), Some("test-times-1")), "{hit}\nrun cache: {} ref {} repository {}", kept["cache"], kept["ref"], kept["repository"]);
        // Its runner fetches its checkout, and the working tree's part it points at; a token no runner has: nothing.
        let rtoken = served.store.under(&format!("run:{run}"))[0].1["jobs"][0]["token"].as_str().unwrap_or_default().to_string();
        let (status, _, body) = http("GET", &format!("{}/runner/{rtoken}/superci/checkout.tar.gz", served.url), b"");
        assert!(status == 200 && !body.is_empty(), "{status}");
        let (status, _, _) = http("GET", &format!("{}/runner/{rtoken}/superci/tree?part=0", served.url), b"");
        assert!(status < 500, "{status}");
        assert_eq!(http("GET", &format!("{}/runner/short/superci/checkout.tar.gz", served.url), b"").0, 404);
        assert_eq!(http("GET", &format!("{}/runner/no-runner-has-this-token-000/superci/checkout.tar.gz", served.url), b"").0, 404);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_run_from_a_computers_job_saves_and_restores_its_cache_keeps_artifacts_and_they_are_fetched_from_the_computer() {
        // A control plane with its own AWS account and its bucket ready; the job's machine given its link to the cache.
        let served = serve(|c| {
            (c.aws_own, c.aws_own_creds) = (Some(("111111111111".into(), "us-east-1".into())), Some(superci_core::aws::Credentials { access_key_id: "ASIAX".into(), secret_access_key: "secret".into(), session_token: Some("tok".into()), expires_at_ms: u64::MAX }));
            c.routing.order = vec![superci_core::plane::Pool::of("cloudflare")];
        });
        let bucket = superci_core::cache::bucket(superci_testkit::PLANE, "us-east-1");
        served.github.put_object(&format!("{bucket}/{}", superci_core::cache::program_key("x86_64")), b"program");
        let dir = checkout();
        let o = superci(&served, &dir, &["runs", "create", "--workflow=.github/workflows/ci.yml", "--detach"]).env("SUPERCI_STAND_IN", &served.aws.url).output().unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        let run = run_of(&served);
        let runner = loop { if let Some(r) = Runner::of_run(&served, &run) { break r } std::thread::sleep(std::time::Duration::from_millis(50)) };
        let token = runner.handed(&served)["resources"]["endpoints"][0]["authorization"]["parameters"]["AccessToken"].as_str().unwrap().to_string();
        let machine = served.machines.0.lock().unwrap()[0].clone();
        let link = "machine-cache-link-000000000001";
        served.store.0.lock().unwrap().insert(format!("cachem:{link}"), serde_json::json!({ "region": "us-east-1", "arch": "x86_64", "until": u64::MAX / 4, "runner": machine }).to_string());
        let cache = |what: &str, body: serde_json::Value| { let (s, _, b) = http("POST", &format!("{}/cache/{link}/{what}", served.url), body.to_string().as_bytes()); (s, serde_json::from_slice::<serde_json::Value>(&b).unwrap_or_default()) };
        let s3 = |method: &str, url: &str, body: &[u8]| http(method, &format!("{}/{}", served.aws.url, url.trim_start_matches("https://")), body);
        let version = "a".repeat(64);
        // Its program, from the bucket.
        let (status, to, _) = fetch(&format!("{}/cache/{link}/program", served.url));
        assert!(status == 302 && to.contains(&bucket) && to.contains("cache-"), "{status} {to}");
        assert_eq!(fetch(&format!("{}/cache/not-a-machine-link-0000/program", served.url)).0, 404);
        // Nothing kept yet; then a save: begun, its piece's link, put there, finished.
        assert_eq!(cache("lookup", serde_json::json!({ "token": token, "key": "deps-1", "restore_keys": ["deps-"], "version": version })).1["hit"], false);
        assert_eq!(cache("begin", serde_json::json!({ "token": token, "key": "deps-1", "version": version })).1["ok"], true);
        let (status, said) = cache("begin", serde_json::json!({ "token": token, "key": "deps-1", "version": version }));
        assert_eq!((status, said["why"].as_str()), (409, Some("exists")), "{said}");
        assert_eq!(cache("begin", serde_json::json!({ "token": token, "key": "a,b", "version": version })).0, 400);
        for names in [serde_json::json!([]), serde_json::json!(["bad/name"]), serde_json::json!((0..65).map(|n| format!("p{n}")).collect::<Vec<_>>())] {
            assert_eq!(cache("sign", serde_json::json!({ "token": token, "key": "deps-1", "version": version, "names": names })).0, 400, "{names}");
        }
        assert_eq!(cache("sign", serde_json::json!({ "token": token, "key": "never-begun", "version": version, "names": ["p00000"] })).0, 404);
        let (_, signed) = cache("sign", serde_json::json!({ "token": token, "key": "deps-1", "version": version, "names": ["p00000"] }));
        assert_eq!(s3("PUT", signed["urls"][0].as_str().unwrap(), b"node_modules, packed").0, 200);
        assert_eq!(cache("finish", serde_json::json!({ "token": token, "key": "deps-1", "version": version, "pieces": [["p00000", 20]] })).1["size"], 20);
        // Restored by its key's beginning, and read back.
        let (_, hit) = cache("lookup", serde_json::json!({ "token": token, "key": "deps-2", "restore_keys": ["deps-"], "version": version }));
        assert_eq!((hit["hit"].as_bool(), hit["key"].as_str()), (Some(true), Some("deps-1")), "{hit}");
        assert_eq!(s3("GET", hit["pieces"][0]["url"].as_str().unwrap(), b"").2, b"node_modules, packed");
        // Without a job's token: refused; a token of no one's: refused.
        let (status, said) = cache("lookup", serde_json::json!({ "key": "deps-1", "version": version }));
        assert_eq!((status, said["ok"].as_bool()), (403, Some(false)), "{said}");
        assert_eq!(cache("lookup", serde_json::json!({ "token": "eyJhbGciOiJFUzI1NiJ9.e30.c2ln", "key": "deps-1", "version": version })).0, 403);
        assert_eq!(http("POST", &format!("{}/cache/not-a-machine-link-0000/lookup", served.url), b"{}").0, 404);
        // A tool's outputs (Turbo's), by the machine's job, with no token: one piece, then two more, and read back.
        assert_eq!(cache("tool-begin", serde_json::json!({ "tool": "make", "hash": "abc" })).0, 400);
        let hash = "b".repeat(64);
        assert_eq!(cache("tool-get", serde_json::json!({ "tool": "turbo", "hash": hash })).1["hit"], false);
        let (_, begun) = cache("tool-begin", serde_json::json!({ "tool": "turbo", "hash": hash }));
        assert_eq!(s3("PUT", begun["url"].as_str().unwrap(), b"dist ").0, 200);
        let (_, more) = cache("tool-sign", serde_json::json!({ "tool": "turbo", "hash": hash, "names": ["p00001"] }));
        assert_eq!(s3("PUT", more["urls"][0].as_str().unwrap(), b"files").0, 200);
        assert_eq!(cache("tool-sign", serde_json::json!({ "tool": "turbo", "hash": hash, "names": ["../x"] })).0, 400);
        assert_eq!(cache("tool-sign", serde_json::json!({ "tool": "turbo", "hash": hash, "names": [] })).0, 400);
        assert_eq!(cache("tool-sign", serde_json::json!({ "tool": "turbo", "hash": hash, "names": (0..65).map(|n| format!("p{n:05}")).collect::<Vec<_>>() })).0, 400);
        assert_eq!(cache("tool-finish", serde_json::json!({ "tool": "turbo", "hash": hash, "pieces": [["p00000", 5], ["p00001", 5]], "meta": "{\"duration\":12}" })).1["size"], 10);
        assert_eq!(cache("tool-begin", serde_json::json!({ "tool": "turbo", "hash": hash })).0, 409);
        assert_eq!(cache("tool-finish", serde_json::json!({ "tool": "nx", "hash": hash, "size": 3 })).0, 404);
        let (_, got) = cache("tool-get", serde_json::json!({ "tool": "turbo", "hash": hash }));
        assert_eq!((got["size"].as_u64(), got["meta"].as_str()), (Some(10), Some("{\"duration\":12}")), "{got}");
        let back: Vec<u8> = got["pieces"].as_array().unwrap().iter().flat_map(|p| s3("GET", p["url"].as_str().unwrap(), b"").2).collect();
        assert_eq!(back, b"dist files");
        // Its artifacts: kept in the run (a zip, a file that is not one, a zip naming a place outside its folder).
        let keep = |name: &str, bytes: &[u8]| {
            assert_eq!(cache("artifact-begin", serde_json::json!({ "token": token, "name": name })).1["ok"], true, "{name}");
            let (_, signed) = cache("artifact-sign", serde_json::json!({ "token": token, "name": name, "names": ["p00000"] }));
            assert_eq!(s3("PUT", signed["urls"][0].as_str().unwrap(), bytes).0, 200);
            cache("artifact-finish", serde_json::json!({ "token": token, "name": name, "pieces": [["p00000", bytes.len()]] })).1
        };
        let report = zip_of(&[("index.html", b"<h1>all passed</h1>"), ("data/results.json", b"{\"passed\":3}")]);
        assert!(keep("report", &report)["id"].as_u64().is_some());
        keep("notes", b"plain words, not a zip");
        keep("evil", &zip_of(&[("../outside.txt", b"x")]));
        // Zips as other tools make them: a folder's own entry, a file deflated (one stored block), packed in a way not
        // known, a damaged directory, zip64's markers, a name from the root or with a drive, and two bytes only.
        let with_method = |method: u16, data: &[u8]| { let mut z = zip_of(&[("f.txt", data)]); z[8] = method as u8; let cd = z.len() - 22 - (46 + 5); z[cd + 10] = method as u8; z };
        let deflated = [&[1u8, 5, 0, 0xfa, 0xff][..], b"hello"].concat();
        let mut damaged = zip_of(&[("a.txt", b"a")]);
        let cd = damaged.len() - 22 - (46 + 5);
        damaged[cd + 2] = 0;
        let mut zip64 = zip_of(&[("a.txt", b"a")]);
        let end = zip64.len() - 22;
        zip64[end + 10] = 0xff; zip64[end + 11] = 0xff;
        for (name, bytes) in [("folders", zip_of(&[("site/", b""), ("site/a.html", b"<p>a</p>")])), ("deflated", with_method(8, &deflated)), ("odd", with_method(99, b"?????")),
            ("damaged", damaged), ("zip64", zip64), ("rooted", zip_of(&[("/etc/x", b"x")])), ("drive", zip_of(&[("c:x", b"x")])), ("tiny", b"PK".to_vec())] { keep(name, &bytes); }
        assert_eq!(cache("artifact-begin", serde_json::json!({ "token": token, "name": "report" })).0, 409);
        assert_eq!(cache("artifact-begin", serde_json::json!({ "token": token, "name": "a/b:c" })).0, 400);
        assert_eq!(cache("artifact-sign", serde_json::json!({ "token": token, "name": "none", "names": ["p00000"] })).0, 404);
        assert_eq!(cache("artifact-sign", serde_json::json!({ "token": token, "name": "report", "names": ["p00001"] })).0, 404, "(kept already)");
        assert_eq!(cache("artifact-begin", serde_json::json!({ "token": token, "name": "late" })).1["ok"], true);
        assert_eq!(cache("artifact-sign", serde_json::json!({ "token": token, "name": "late", "names": [] })).0, 400);
        assert_eq!(cache("artifact-sign", serde_json::json!({ "token": token, "name": "late", "names": ["a/b"] })).0, 400);
        assert_eq!(cache("artifact-sign", serde_json::json!({ "token": token, "name": "late", "names": (0..65).map(|n| format!("p{n:05}")).collect::<Vec<_>>() })).0, 400);
        // A runner's link too short, or with what a link has not: nothing there.
        for at in ["/runner/short/superci/tree", "/runner/aaaaaaaaaaaaaaaa%21%21/superci/tree"] { assert_eq!(fetch(&format!("{}{at}", served.url)).0, 404, "{at}") }
        assert_eq!(cache("artifact-finish", serde_json::json!({ "token": token, "name": "none", "pieces": [] })).0, 404);
        assert_eq!(cache("artifacts", serde_json::json!({ "token": token })).1["artifacts"].as_array().map(Vec::len), Some(11), "(the three, and the eight as other tools make them)");
        assert_eq!(cache("artifact-get", serde_json::json!({ "token": token, "name": "report" })).1["size"], report.len());
        assert_eq!(cache("artifact-get", serde_json::json!({ "token": token, "name": "none" })).0, 404);
        // From the computer: listed, and each fetched: the zip unpacked, the file kept as it is, the bad zip refused.
        let cli = |args: &[&str]| { let o = superci(&served, &dir, args).env("SUPERCI_STAND_IN", &served.aws.url).output().unwrap(); (o.status.success(), String::from_utf8_lossy(&o.stdout).to_string() + &String::from_utf8_lossy(&o.stderr)) };
        let (ok, said) = cli(&["artifacts", "list", &format!("--run={run}")]);
        assert!(ok && said.contains("report") && said.contains("notes") && said.contains("evil"), "{said}");
        let out = common::temp("artifact");
        let (ok, said) = cli(&["artifacts", "retrieve", "report", &format!("--run={run}"), &format!("--out={}", out.display())]);
        assert!(ok && said.contains("2 files"), "{said}");
        assert_eq!(std::fs::read_to_string(out.join("data/results.json")).unwrap(), "{\"passed\":3}");
        let (ok, said) = cli(&["artifacts", "retrieve", "notes", &format!("--run={run}"), &format!("--out={}", out.join("n").display())]);
        assert!(ok && said.contains("1 file") && std::fs::read_to_string(out.join("n/notes")).unwrap() == "plain words, not a zip", "{said}");
        let (ok, said) = cli(&["artifacts", "retrieve", "evil", &format!("--run={run}"), &format!("--out={}", out.join("e").display())]);
        assert!(!ok && said.contains("evil could not be unpacked: a name outside its folder: ../outside.txt"), "{said}");
        // In the dashboard: the run's page lists them, each a download (the zip as it was kept).
        let home = common::signed_in(&served);
        let repository = served.store.under(&format!("run:{run}"))[0].1["repository"].as_str().unwrap().to_string();
        let (mut child, link) = common::dashboard(&home);
        let page_at = format!("/?p=run&repo={}&id={run}&attempt=0&fragment=1", url_encoded(&repository));
        let page = (0..40).map(|_| { std::thread::sleep(std::time::Duration::from_millis(250)); common::part(&link, &page_at).1 }).find(|p| p.contains("Its artifacts")).unwrap_or_default();
        assert!(page.contains(&format!("/run/artifact?run={run}&amp;name=report")) && page.contains("name=notes"), "{}", common::words(&page));
        let (status, zip) = common::part(&link, &format!("/run/artifact?run={run}&name=report"));
        assert!(status == 200 && zip.contains("data/results.json") && zip.len() == report.len(), "{status} {}", zip.len());
        for bad in ["/run/artifact?run=x&name=report", "/run/artifact?run=1&name="] { assert_eq!(common::part(&link, bad).0, 400, "{bad}") }
        // One whose name a file's cannot have, downloaded under a name it can.
        keep("test results (unit)", &zip_of(&[("r.txt", b"ok")]));
        let agent: ureq::Agent = ureq::Agent::config_builder().http_status_as_error(false).build().into();
        let (base, key) = link.split_once("/?k=").unwrap();
        let port = base.rsplit(':').next().unwrap();
        let r = agent.get(&format!("{base}/run/artifact?run={run}&name=test%20results%20%28unit%29")).header("cookie", &format!("superci_local_{port}={key}")).header("sec-fetch-site", "same-origin").call().unwrap();
        let disposition = r.headers().get("content-disposition").and_then(|v| v.to_str().ok()).unwrap_or_default().to_string();
        assert!(disposition.contains("test_results__unit_.zip"), "{} {disposition}", r.status());
        let _ = child.kill();
        for (name, want, ok) in [("folders", "1 file", true), ("deflated", "1 file", true), ("tiny", "1 file", true), ("odd", "f.txt is packed in a way this does not know (99)", false), ("damaged", "a damaged directory", false),
            ("zip64", "over 4 GB (zip64)", false), ("rooted", "a name outside its folder: /etc/x", false), ("drive", "a name outside its folder: c:x", false)] {
            let to = out.join(name);
            let (done, said) = cli(&["artifacts", "retrieve", name, &format!("--run={run}"), &format!("--out={}", to.display())]);
            assert!(done == ok && said.contains(want), "{name}: {said}");
        }
        assert_eq!(std::fs::read_to_string(out.join("folders/site/a.html")).unwrap(), "<p>a</p>");
        assert_eq!(std::fs::read_to_string(out.join("deflated/f.txt")).unwrap(), "hello");
        assert_eq!(std::fs::read(out.join("tiny/tiny")).unwrap(), b"PK");
        // Deleted by the job; gone from the bucket.
        assert_eq!(cache("artifact-delete", serde_json::json!({ "token": token, "name": "notes" })).1["ok"], true);
        assert_eq!(cache("artifact-delete", serde_json::json!({ "token": token, "name": "notes" })).0, 404);
        assert_eq!(cache("nothing", serde_json::json!({ "token": token })).0, 404);
        // The run's repository's cache entries, and removed.
        let (_, listed) = common::json(&home, &["caches", "list"]);
        assert_eq!((listed["repos"][0]["name"].as_str(), listed["repos"][0]["entries"].as_u64()), (Some(repository.as_str()), Some(2)), "{listed}");
        let (_, entries) = common::json(&home, &["caches", "list", &format!("--repo={repository}")]);
        assert!(entries["entries"].as_array().unwrap().iter().any(|e| e["key"] == "deps-1"), "{entries}");
        let o = common::superci(&home, &["caches", "delete", &format!("--repo={repository}"), "deps-1", "--confirm"]);
        assert!(o.status.success() && String::from_utf8_lossy(&o.stdout).contains("deps-1 is out of"), "{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
        let (_, entries) = common::json(&home, &["caches", "list", &format!("--repo={repository}")]);
        assert!(!entries.to_string().contains("deps-1"), "{entries}");
        assert_eq!(cache("lookup", serde_json::json!({ "token": token, "key": "deps-1", "version": version })).1["hit"], false);
        // The job played to its end, its tests in its log: what they say, with the run's artifacts to fetch.
        let step = "eeeeeeee-0000-0000-0000-000000000005";
        runner.take();
        runner.steps(&[(step, "Run cargo test", 1, "inProgress", None, None)]);
        let lines: Vec<String> = (1..=40).map(|n| format!("test tests::case_{n} ... ok")).chain(["test result: ok. 40 passed; 0 failed; 2 ignored; 0 measured; 0 filtered out".to_string()]).collect();
        runner.lines(step, 1, &lines);
        runner.log(1, &lines.iter().map(|l| format!("2026-10-15T10:00:01.0000000Z {l}")).collect::<Vec<_>>().join("\n"));
        runner.steps(&[(step, "Run cargo test", 1, "completed", Some("succeeded"), Some(1))]);
        runner.finish("succeeded");
        let job = loop {
            let done = served.store.under("job:").into_iter().find(|(_, j)| j["run_id"].to_string() == run && j["state"] == "done");
            if let Some((_, j)) = done { break j["job_id"].to_string() }
            std::thread::sleep(std::time::Duration::from_millis(50));
        };
        let said = String::from_utf8_lossy(&common::superci(&home, &["jobs", "retrieve", &job, "--tests"]).stdout).into_owned();
        assert!(said.contains("Tests: 0 failed, 40 passed, 2 skipped (cargo)") && said.contains(&format!("Artifact report: `superci artifacts retrieve report --run={run}`")), "{said}");
        // Its log's end only, when asked for a few bytes; then the run, over, is not cancelled again.
        let said = String::from_utf8_lossy(&common::superci(&home, &["jobs", "retrieve", &job, "--bytes=100"]).stdout).into_owned();
        assert!(said.contains("The end of its log (") && said.contains("40 passed"), "{said}");
        let said = String::from_utf8_lossy(&common::superci(&home, &["runs", "cancel", &run]).stdout).into_owned();
        assert!(said.contains(&format!("Run {run} was over already.")), "{said}");
        // A day later the upkeep forgets the run: its jobs' messages, lines and logs, its artifacts (and their pieces
        // in the bucket), its working tree. The cache keeps its entries for their days.
        let artifacts_in_bucket = || served.github.objects().into_iter().filter(|(k, _)| k.contains(&format!("run-{run}"))).count();
        assert!(artifacts_in_bucket() > 0);
        served.later(2 * 86_400_000);
        served.upkeep();
        for prefix in [format!("run:{run}"), "rjob:".into(), "rline:".into(), "rlog:".into(), format!("artifact:{run}"), "rtree".into()] {
            assert!(served.store.under(&prefix).is_empty(), "{prefix} kept: {:?}", served.store.under(&prefix).iter().map(|(k, _)| k).collect::<Vec<_>>());
        }
        assert_eq!(artifacts_in_bucket(), 0, "{:?}", served.github.objects());
        let (ok, said) = cli(&["artifacts", "list", &format!("--run={run}")]);
        assert!(!ok && said.contains("No such run"), "{said}");
        // Unused past its days (7): gone, its pieces from the bucket too.
        let pieces = || served.github.objects().into_iter().filter(|(k, _)| !k.contains("/bases/") && !k.contains("superci/cache-") && !k.contains("run-")).count();
        assert!(pieces() > 0, "{:?}", served.github.objects());
        served.later(8 * 86_400_000);
        served.upkeep();
        assert_eq!(pieces(), 0, "{:?}", served.github.objects());
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&out);
        super::seen_in_browser(&served);
    }

    fn url_encoded(s: &str) -> String { url::form_urlencoded::byte_serialize(s.as_bytes()).collect() }

    #[test]
    fn jobs_if_conditions_are_read_as_github_reads_its_expressions() {
        let served = serve(|_| {});
        // Each job runs when its condition holds as GitHub would have it: numbers in hex and with exponents, negatives,
        // text and booleans as numbers, `&&` and `||` giving a side's value, `join` and `format`, an id with a dash,
        // a filter over an object.
        let holds = [
            "0x10 == 16 && 1.5e2 == 150 && -2 < 0 && 2.5 > 2",
            "'' == 0 && true == 1 && ' ' == 0 && null == 0 && '0x1F' == 31",
            "(false || 'x') == 'x' && (0 && 'y') == 0 && !(null)",
            "join(fromJSON('[1,2]')) == '1,2' && join(fromJSON('[\"a\",\"b\"]'), '-') == 'a-b' && join('one') == 'one'",
            "format('{0}{1}', 3.0, 0.5) == '30.5' && format('{{x}}') == '{x}'",
            "contains(github.*, 'push') && github.event_name >= 'PUSH' && 'b' > 'A'",
            "fromJSON('{\"my-key\": 1e3}').my-key == 1000",
        ];
        let fails = ["0x10 == 15", "'a' && ''", "-1 > 0 || 'NaN' == 0", "1e400 < 0"];
        let mut wf = String::from("jobs:\n");
        // (One job for all that hold: each is read, as each holds. A job each for what does not.)
        wf += &format!("  yes:\n    if: >-\n      ${{{{ {} }}}}\n    runs-on: superci\n    steps:\n      - run: echo\n", holds.map(|c| format!("({c})")).join(" && "));
        for (i, c) in fails.iter().enumerate() { wf += &format!("  no-{i}:\n    if: >-\n      ${{{{ {c} }}}}\n    runs-on: superci\n    steps:\n      - run: echo\n") }
        let dir = checkout_with(&[(".github/workflows/ci.yml", &wf)]);
        let o = superci(&served, &dir, &["runs", "create", "--workflow=.github/workflows/ci.yml", "--detach"]).output().unwrap();
        assert!(o.status.success(), "{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
        let run = run_of(&served);
        let jobs = served.store.under(&format!("run:{run}"))[0].1["jobs"].clone();
        let result = |name: &str| jobs.as_array().unwrap().iter().find(|j| j["key"] == name).map(|j| (j["state"].as_str().unwrap_or_default().to_string(), j["result"].as_str().unwrap_or_default().to_string())).unwrap_or_default();
        let (state, ended) = result("yes");
        assert!(state == "started" && ended.is_empty(), "started: {jobs}");
        for (i, c) in fails.iter().enumerate() { assert_eq!(result(&format!("no-{i}")).1, "skipped", "{c}: {jobs}") }
        let _ = std::fs::remove_dir_all(&dir);
        super::seen_in_browser(&served);
    }

    #[test]
    fn a_tree_too_large_for_one_request_goes_in_parts_two_jobs_may_call_one_workflow_and_a_file_not_yaml_is_said() {
        let served = serve(|_| {});
        // 4 MB that does not pack smaller (random), through the control plane in parts of 3 MB.
        let mut noise = vec![0u8; 4_000_000];
        let mut x: u64 = 0x9e3779b97f4a7c15;
        for b in noise.iter_mut() { x ^= x << 13; x ^= x >> 7; x ^= x << 17; *b = x as u8 }
        let dir = checkout_with(&[
            (".github/workflows/ci.yml", "jobs:\n  web:\n    uses: ./.github/workflows/build.yml\n  api:\n    uses: ./.github/workflows/build.yml\n"),
            (".github/workflows/build.yml", "on:\n  workflow_call:\njobs:\n  compile:\n    runs-on: superci\n    steps:\n      - run: make\n"),
        ]);
        std::fs::write(dir.join("noise.bin"), &noise).unwrap();
        assert!(Command::new("git").current_dir(&dir).args(["add", "-A"]).status().unwrap().success());
        let o = superci(&served, &dir, &["runs", "create", "--workflow=.github/workflows/ci.yml", "--detach"]).output().unwrap();
        let said = String::from_utf8_lossy(&o.stdout).to_string() + &String::from_utf8_lossy(&o.stderr);
        assert!(o.status.success() && said.contains("2 parts)"), "{said}");
        let run = run_of(&served);
        let names: Vec<String> = served.store.under(&format!("run:{run}"))[0].1["jobs"].as_array().unwrap().iter().filter_map(|j| j["name"].as_str().map(str::to_string)).collect();
        assert!(names.contains(&"web / compile".to_string()) && names.contains(&"api / compile".to_string()), "{names:?}");
        // A workflow file that is not YAML: said by the control plane, nothing started.
        std::fs::write(dir.join(".github/workflows/bad.yml"), "jobs: [unclosed\n").unwrap();
        let o = superci(&served, &dir, &["runs", "create", "--workflow=.github/workflows/bad.yml"]).output().unwrap();
        let said = String::from_utf8_lossy(&o.stdout).to_string() + &String::from_utf8_lossy(&o.stderr);
        assert!(!o.status.success() && said.contains("the control plane said 400") && said.contains("not YAML"), "{said}");
        let _ = std::fs::remove_dir_all(&dir);
        super::seen_in_browser(&served);
    }

    #[test]
    fn a_run_with_a_run_key_gets_the_secrets_the_ci_keeps_and_says_which_it_names_that_are_not_set() {
        let served = serve(|_| {});
        served.aws.put(&format!("/superci/{}/SECRET_KEPT_ALL", superci_testkit::PLANE), &serde_json::json!({ "name": "KEPT", "value": "kept-value-0001", "updated_ms": 1 }).to_string());
        let dir = checkout_with(&[(".github/workflows/ci.yml", "jobs:\n  tests:\n    runs-on: superci\n    steps:\n      - run: echo ${{ secrets.KEPT }} ${{ secrets.NOT_KEPT }}\n")]);
        let o = superci(&served, &dir, &["runs", "create", "--workflow=.github/workflows/ci.yml", "--detach"]).output().unwrap();
        let said = String::from_utf8_lossy(&o.stdout);
        assert!(o.status.success() && said.contains("Not set in this CI, empty as on GitHub: NOT_KEPT"), "{said}{}", String::from_utf8_lossy(&o.stderr));
        let run = run_of(&served);
        let handed = loop { if let Some((_, v)) = served.store.under("rjob:").into_iter().next() { break v } std::thread::sleep(std::time::Duration::from_millis(50)) };
        assert_eq!(handed["message"]["variables"]["KEPT"]["value"], "kept-value-0001");
        assert!(ureq::post(&format!("{}/runs/{run}/cancel", served.url)).header("authorization", &format!("Bearer {}", served.run_key)).send_empty().is_ok());
        let _ = std::fs::remove_dir_all(&dir);
        super::seen_in_browser(&served);
    }

    #[test]
    fn a_step_whose_result_never_came_is_judged_by_its_own_lines_and_its_failed_test_named() {
        let served = serve(|_| {});
        let dir = checkout();
        let cli = superci(&served, &dir, &["runs", "create", "--workflow=.github/workflows/ci.yml", "--quiet"]).spawn().unwrap();
        let run = run_of(&served);
        let runner = loop { if let Some(r) = Runner::of_run(&served, &run) { break r } std::thread::sleep(std::time::Duration::from_millis(50)) };
        let (a, b) = ("aaaaaaaa-0000-0000-0000-000000000001", "bbbbbbbb-0000-0000-0000-000000000002");
        runner.take();
        runner.steps(&[(a, "Run make", 1, "completed", Some("succeeded"), Some(1)), (b, "Run the tests", 2, "inProgress", None, None)]);
        let lines: Vec<String> = ["Running 2 tests using 1 worker", "  1) [chromium] › e2e/cart.spec.ts:4:3 › adds an item ───", "", "    Error: expected 2, got 1", "", "  1 failed", "  1 passed (2.0s)"].iter().map(|s| s.to_string()).collect();
        runner.lines(b, 1, &lines);
        // The job ends with that step's result never sent.
        runner.finish("failed");
        let out = cli.wait_with_output().unwrap();
        let quiet = String::from_utf8_lossy(&out.stdout).into_owned();
        assert!(!out.status.success(), "{quiet}");
        assert!(quiet.contains("✗ Run the tests") && quiet.contains("✗ [chromium] adds an item (e2e/cart.spec.ts:4:3)") && quiet.contains("Error: expected 2, got 1"), "{quiet}");
        assert!(quiet.contains("Failed: Run the tests") && quiet.contains("Tests: 1 failed, 1 passed (playwright)"), "{quiet}");
        // (While it waited for that result, the run was said to be over, its summary still to come.)
        assert!(quiet.contains("The run is over; its summary comes once its runner has sent the steps' logs"), "{quiet}");
        let _ = std::fs::remove_dir_all(&dir);
        super::seen_in_browser(&served);
    }

    #[test]
    fn a_run_the_control_plane_cannot_run_is_refused_with_why_and_starts_nothing() {
        let served = serve(|_| {});
        let w = ".github/workflows/";
        let called = "on: workflow_call\njobs:\n  inner:\n    runs-on: superci\n    steps:\n      - run: true\n";
        let calls_on = "on: workflow_call\njobs:\n  inner:\n    uses: ./.github/workflows/called.yml\n";
        let matrix65 = format!("jobs:\n  t:\n    runs-on: superci\n    strategy:\n      matrix:\n        n: [{}]\n    steps:\n      - run: true\n", (0..65).map(|n| n.to_string()).collect::<Vec<_>>().join(", "));
        let cases: Vec<(&str, String, &[&str], &str)> = vec![
            ("ci.yml", "on: push\njobs: {}\n".into(), &[], "has no jobs"),
            ("ci.yml", "jobs:\n  a:\n    runs-on: superci\n    steps:\n      - run: true\n".into(), &["--job=nope"], "has no job named nope: a"),
            ("ci.yml", "jobs:\n  a:\n    runs-on: superci\n    needs: gone\n    steps:\n      - run: true\n".into(), &[], "the job a needs gone, which"),
            ("ci.yml", "jobs:\n  a:\n    uses: octo/repo/.github/workflows/x.yml@v1\n".into(), &[], "a workflow of another repository is not run from a computer yet"),
            ("ci.yml", "jobs:\n  a:\n    uses: ./.github/workflows/push.yml\n".into(), &[], "which is not a workflow that may be called (on: workflow_call)"),
            ("ci.yml", "jobs:\n  a:\n    strategy:\n      matrix:\n        n: [1, 2]\n    uses: ./.github/workflows/called.yml\n".into(), &[], "calls a workflow with a matrix: not run from a computer yet"),
            ("ci.yml", "jobs:\n  a:\n    uses: ./.github/workflows/calls.yml\n".into(), &[], "calls a workflow in turn: not run from a computer yet"),
            ("ci.yml", matrix65, &[], "65 jobs: a run from a computer runs 64 at most"),
        ];
        for (name, text, extra, refused) in cases {
            let path = format!("{w}{name}");
            let dir = checkout_with(&[(path.as_str(), text.as_str()), (".github/workflows/called.yml", called), (".github/workflows/calls.yml", calls_on), (".github/workflows/push.yml", "on: push\njobs:\n  p:\n    runs-on: superci\n    steps:\n      - run: true\n")]);
            let mut args = vec!["runs", "create", "--workflow=.github/workflows/ci.yml", "--detach"];
            args.extend_from_slice(extra);
            let o = superci(&served, &dir, &args).output().unwrap();
            let err = String::from_utf8_lossy(&o.stderr).into_owned();
            assert!(!o.status.success() && err.contains(refused), "{refused}: {err}{}", String::from_utf8_lossy(&o.stdout));
            assert!(served.store.under("run:").is_empty() && served.machines.0.lock().unwrap().is_empty(), "{refused}");
            let _ = std::fs::remove_dir_all(&dir);
        }
        super::seen_in_browser(&served);
    }

    #[test]
    fn a_gpu_job_on_an_account_with_no_gpu_quota_fails_saying_which_quota_and_a_type_a_zone_lacks_is_said_as_aws_answered() {
        let own = |c: &mut superci_core::plane::Config| {
            (c.aws_own, c.aws_own_creds) = (Some(("111111111111".into(), "us-east-1".into())), Some(superci_core::aws::Credentials { access_key_id: "ASIAX".into(), secret_access_key: "secret".into(), session_token: Some("tok".into()), expires_at_ms: u64::MAX }));
        };
        let job_of = |served: &superci_testkit::Served| loop {
            if let Some((_, j)) = served.store.under("job:").into_iter().next().filter(|(_, j)| j["machine_id"].is_string() || ["failed", "waiting"].contains(&j["state"].as_str().unwrap_or_default())) { break j }
            std::thread::sleep(std::time::Duration::from_millis(50));
        };
        // A GPU machine on-demand, on an account AWS allows none (its quota for them is 0), as AWS answered Test.
        let served = serve(own);
        served.aws.replaying(include_str!("../fixtures/aws/launch/spot.jsonl"), &[]);
        served.aws.replaying(include_str!("../fixtures/aws/launch/gpu-on-demand.jsonl"), &[]);
        let dir = checkout_with(&[(".github/workflows/ci.yml", "jobs:\n  train:\n    runs-on: superci/gpu=any/spot=false\n    steps:\n      - run: make train\n")]);
        let o = superci(&served, &dir, &["runs", "create", "--workflow=.github/workflows/ci.yml", "--detach"]).env("SUPERCI_STAND_IN", &served.aws.url).output().unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        let job = job_of(&served);
        assert_eq!(job["state"], "failed", "{job}");
        assert!(job["error"].as_str().unwrap().starts_with("AWS lets this account run no GPU machines yet: ask AWS for more in Service Quotas, “Running On-Demand G and VT instances”"), "{job}");
        let _ = std::fs::remove_dir_all(&dir);
        // A type the zone asked does not offer, as AWS answered Test for us-east-1e (here for every zone): each zone's
        // refusal is kept with the job's start, and the job goes to the next in the order, Cloudflare.
        let served = serve(own);
        served.aws.replaying(include_str!("../fixtures/aws/launch/spot.jsonl"), &[]);
        served.aws.replaying(include_str!("../fixtures/aws/launch/unsupported.jsonl"), &[]);
        let dir = checkout_with(&[(".github/workflows/ci.yml", "jobs:\n  tests:\n    runs-on: superci/cpu=2/spot=false\n    steps:\n      - run: make test\n")]);
        let o = superci(&served, &dir, &["runs", "create", "--workflow=.github/workflows/ci.yml", "--detach"]).env("SUPERCI_STAND_IN", &served.aws.url).output().unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        let job = job_of(&served);
        let asks = job["asks"].as_array().cloned().unwrap_or_default();
        assert!(job["cloud"] == "cloudflare" && asks.len() == 6 && asks.iter().all(|a| a["said"] == "Unsupported" && a["spot"] == false), "{job}");
        assert!(served.store.under("aws:unoffered:us-east-1:").len() >= 6, "a type a zone lacks is not asked there again: {:?}", served.store.under("aws:"));
        let _ = std::fs::remove_dir_all(&dir);
        super::seen_in_browser(&served);
    }

    #[test]
    fn a_job_with_the_accounts_spot_and_on_demand_limits_used_up_waits_saying_which_limits_as_aws_answered() {
        // Spot refused for the account's count of spot machines, on-demand for its 64 vCPUs (as AWS answered Test while
        // both were in use): the job waits for room, saying the account's limits keep its machine from starting.
        let served = serve(|c| {
            (c.aws_own, c.aws_own_creds) = (Some(("111111111111".into(), "us-east-1".into())), Some(superci_core::aws::Credentials { access_key_id: "ASIAX".into(), secret_access_key: "secret".into(), session_token: Some("tok".into()), expires_at_ms: u64::MAX }));
        });
        served.aws.replaying(include_str!("../fixtures/aws/launch/spot.jsonl"), &[]);
        served.aws.replaying(include_str!("../fixtures/aws/launch/at-limits.jsonl"), &[("mixplane0001", superci_testkit::PLANE)]);
        // (Only AWS: Cloudflare off in the order, nowhere else to go.)
        served.aws.put(&format!("/superci/{}/ROUTING", superci_testkit::PLANE), r#"{"default":null,"rules":[],"order":[{"cloud":"aws"},{"cloud":"aws-on-demand"},{"cloud":"cloudflare","off":true}]}"#);
        let dir = checkout_with(&[(".github/workflows/ci.yml", "jobs:\n  tests:\n    runs-on: superci/cpu=2\n    steps:\n      - run: make test\n")]);
        let o = superci(&served, &dir, &["runs", "create", "--workflow=.github/workflows/ci.yml", "--detach"]).env("SUPERCI_STAND_IN", &served.aws.url).output().unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        let job = loop {
            if let Some((_, j)) = served.store.under("job:").into_iter().next().filter(|(_, j)| j["state"] != "launching") { break j }
            std::thread::sleep(std::time::Duration::from_millis(50));
        };
        let asks = job["asks"].as_array().cloned().unwrap_or_default();
        assert!(asks.iter().any(|a| a["said"] == "MaxSpotInstanceCountExceeded" && a["spot"] == true) && asks.iter().any(|a| a["said"] == "VcpuLimitExceeded" && a["spot"] == false), "{job}");
        assert!(job["state"] == "waiting" && job["error"].as_str().unwrap_or_default().contains("Your AWS account's limits do not let this machine start"), "{job}");
        let _ = std::fs::remove_dir_all(&dir);
        super::seen_in_browser(&served);
    }
}

/// The dashboard as its own page asks it (each page, sign-ins, what it is given, where it listens), each scenario
/// ending with what a person sees of it.
mod dashboard_suite_in_browser {
    use super::*;
    use common::{json, part, played, words};

    #[test]
    fn each_page_shows_what_the_control_plane_has() {
        let served = serve(|_| {});
        let home = home(&served);
        played(&served, &home.join("checkout"));
        let (mut child, link) = dashboard(&home);
        let page = |p: &str| { let (status, body) = part(&link, &format!("/?p={p}&fragment=1")); assert_eq!(status, 200, "{p}"); words(&body) };
        let has = |p: &str, all: &[&str]| { let w = page(p); for s in all { assert!(w.contains(s), "{p} lacks {s:?}: {w}") } w };
        let job = served.store.under("job:").into_iter().next().unwrap().1;
        let id = job["job_id"].as_u64().unwrap();

        has("overview", &["Usage", "24 hours", "7 days", "30 days", "1 job", "Control plane", "Running"]);
        has("jobs", &["Recent jobs", "tests", "checkout · .github/workflows/ci.yml", "4 CPU · 12 GB", "Passed"]);
        // A run, and its page by the link the list gives.
        let (_, runs) = part(&link, "/?p=runs&fragment=1");
        assert!(words(&runs).contains(".github/workflows/ci.yml checkout 1 job"), "{}", words(&runs));
        let run_link = runs.split("href=\"").filter_map(|h| h.split('"').next()).find(|h| h.starts_with("/?p=run&")).expect("a run's link").replace("&amp;", "&");
        has(run_link.trim_start_matches("/?p="), &["Run from a computer: it has no page at GitHub.", "Passed", "tests"]);
        let shown = has(&format!("job&id={id}"), &["tests", "Run from a computer", "Status Passed", "Its start", "Its log", "Raw log"]);
        assert!(shown.contains("from queued to running"), "{shown}");
        has("runners", &["Runner providers", "Jobs at once", "Monthly budget"]);
        has("add-runners", &["Add a provider", "AWS", "Modal", "Cloudflare"]);
        has("workflows", &["Default machine", "Connect GitHub or GitLab first"]);
        has("repos", &["Repositories", "GitHub", "Create its App"]);
        has("gitlab", &["GitLab is not connected"]);
        has("planes", &["Control plane", "AWS · 111111111111 · us-east-1", env!("CARGO_PKG_VERSION"), "In use"]);
        has("settings", &["Name", "SuperCI", "Labels", "superci", "Largest machine", "Workspace", "default"]);
        has("changes", &["Changelog", env!("CARGO_PKG_VERSION")]);

        // Its log as the page reads it: in steps, its lines in colour, their times.
        let (status, view) = part(&link, &format!("/job/view?id={id}"));
        let view: serde_json::Value = serde_json::from_str(&view).unwrap();
        assert_eq!((status, view["going"].as_bool(), view["errors"].as_u64()), (200, Some(false), Some(0)));
        let step = &view["steps"][0];
        assert_eq!((step["name"].as_str(), step["state"].as_str(), step["lines"].as_array().map(Vec::len)), (Some("Run make test"), Some("success"), Some(12)));
        assert_eq!(step["lines"][11], serde_json::json!(["10:00:01", "", r#"<span class="a32">test 12 passed</span>"#]));
        // And as it is, to save.
        let (status, raw) = part(&link, &format!("/job/raw?id={id}"));
        assert!(status == 200 && raw.lines().count() == 12 && raw.trim_end().ends_with("test 12 passed\u{1b}[0m"), "{raw}");
        // A job not asked well.
        assert_eq!(part(&link, "/job/view?id=x1").0, 400);
        let _ = child.kill();
        let _ = std::fs::remove_dir_all(&home);
        super::seen_in_browser(&served);
    }

    /// An AWS sign-in that AWS finishes (as a real one answered, its tokens replaced): the account is kept, with who
    /// signed in; once its credentials are due, they are renewed with its renewal token, and the renewal kept.
    #[test]
    fn an_aws_sign_in_is_kept_and_renewed_as_aws_answered_a_real_one() {
        let served = serve(|_| {});
        served.aws.replaying(&[include_str!("../fixtures/aws/sign-in/sign-in.jsonl"), include_str!("../fixtures/aws/sign-in/renew.jsonl")].concat(), &[]);
        let home = home(&served);
        std::fs::write(home.join("aws-at"), &served.aws.url).unwrap();
        let (mut child, link) = dashboard(&home);
        let (_, to, page) = common::post(&link, "/aws/signin", &[("next", "?p=overview"), ("w", "default")]);
        let at = if to.is_empty() { page } else { to };
        let state = at.split("state=").nth(1).and_then(|r| r.split(['&', '"']).next()).expect("a state").to_string();
        let (status, _, done) = common::get(&link, &format!("/oauth/callback?code=a-code-from-aws&state={state}"));
        assert!(status < 400 && words(&done).contains("Signed in with AWS"), "{status} {}", words(&done));
        let _ = child.kill();
        let file = home.join("workspaces/default/sign-ins.json");
        let kept = || -> serde_json::Value { serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap() };
        let aws = &kept()["aws"];
        assert_eq!((aws["account_id"].as_str(), aws["who"].as_str(), aws["creds"]["access_key_id"].as_str(), aws["refresh_token"].as_str()),
            (Some("111111111111"), Some("arn:aws:iam::111111111111:root"), Some("example-accessKeyId-1"), Some("example-refreshToken-1")), "{aws}");
        // Due: the next command renews them first, and keeps what AWS gave.
        let mut k = kept();
        k["aws"]["creds"]["expires_at_ms"] = 0.into();
        std::fs::write(&file, k.to_string()).unwrap();
        let o = common::superci(&home, &["debug", "iam", "ListRoles", "MaxItems=1000", "--json"]);
        assert!(o.status.success(), "{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
        let aws = &kept()["aws"];
        assert_eq!((aws["creds"]["access_key_id"].as_str(), aws["refresh_token"].as_str()), (Some("example-accessKeyId-2"), Some("example-refreshToken-2")), "{aws}");
        let _ = std::fs::remove_dir_all(&home);
        super::seen_in_browser(&served);
    }

    /// A Cloudflare sign-in as a real one answered (its tokens replaced), on the port Cloudflare's sign-in comes back to
    /// (this test's own, standing for 8976): kept; renewed once due, the renewal kept; ended at Cloudflare at sign-out.
    #[test]
    fn a_cloudflare_sign_in_is_kept_renewed_and_ended_as_cloudflare_answered_a_real_one() {
        use std::io::BufRead;
        let served = serve(|_| {});
        served.aws.replaying(&[include_str!("../fixtures/cloudflare/sign-in/sign-in.jsonl"), include_str!("../fixtures/cloudflare/sign-in/renew.jsonl"), include_str!("../fixtures/cloudflare/sign-in/sign-out.jsonl")].concat(), &[]);
        let home = home(&served);
        std::fs::write(home.join("aws-at"), &served.aws.url).unwrap();
        let port = { let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap(); l.local_addr().unwrap().port() };
        let mut child = common::command(&home).args(["dashboard", "--no-browser", "--workspace=default"]).env("SUPERCI_PORT", port.to_string()).env("SUPERCI_OAUTH_PORT", port.to_string())
            .stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::null()).spawn().unwrap();
        let mut lines = std::io::BufReader::new(child.stdout.take().unwrap()).lines().map_while(Result::ok);
        let link = lines.find_map(|l| l.split_whitespace().find(|w| w.starts_with("http://localhost")).map(str::to_string)).expect("the dashboard's link");
        std::thread::spawn(move || lines.for_each(drop));
        let key = link.split_once("/?k=").unwrap().1.to_string();
        let agent: ureq::Agent = ureq::Agent::config_builder().http_status_as_error(false).max_redirects(0).build().into();
        let get = |path: &str| { let mut r = agent.get(&format!("http://127.0.0.1:{port}{path}")).header("cookie", &format!("superci_local={key}")).header("sec-fetch-site", "same-origin").call().unwrap();
            let to = r.headers().get("location").and_then(|v| v.to_str().ok()).unwrap_or_default().to_string(); (r.status().as_u16(), to, r.body_mut().read_to_string().unwrap_or_default()) };
        // To Cloudflare's page, with the state it comes back with; back, with its code.
        let (_, to, page) = get("/connect/cloudflare");
        let to = if to.is_empty() { page.split("https://dash.cloudflare.com/oauth2/auth?").nth(1).map(|r| format!("https://dash.cloudflare.com/oauth2/auth?{}", r.split(['"', '\'']).next().unwrap().replace("&amp;", "&"))).unwrap_or_default() } else { to };
        assert!(to.contains(&format!("redirect_uri=http%3A%2F%2Flocalhost%3A{port}%2Foauth%2Fcallback")), "{to}");
        let state = to.split("state=").nth(1).and_then(|r| r.split('&').next()).unwrap().to_string();
        let (status, _, done) = get(&format!("/oauth/callback?code=a-code-from-cloudflare&state={state}"));
        assert!(status < 400 && words(&done).contains("Signed in with Cloudflare"), "{status} {}", words(&done));
        let _ = child.kill();
        let file = home.join("workspaces/default/sign-ins.json");
        let kept = || -> serde_json::Value { serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap() };
        assert_eq!((kept()["cloudflare"]["access"].as_str(), kept()["cloudflare"]["refresh"].as_str()), (Some("example-access_token-1"), Some("example-refresh_token-1")), "{}", kept());
        // Due: renewed by the next command, and what Cloudflare gave kept (it takes a renewal token once).
        let mut k = kept();
        k["cloudflare"]["expires_at_ms"] = 0.into();
        std::fs::write(&file, k.to_string()).unwrap();
        let (code, accounts) = json(&home, &["debug", "cloudflare", "GET", "/accounts"]);
        assert!(code == 0 && accounts.to_string().contains("00000000000000000000000000000001"), "{accounts}");
        assert_eq!(kept()["cloudflare"]["refresh"].as_str(), Some("example-refresh_token-2"));
        // Signed out: Cloudflare asked to end it.
        let o = common::superci(&home, &["logout"]);
        assert!(String::from_utf8_lossy(&o.stdout).contains("Cloudflare ended SuperCI's sign-in"), "{}", String::from_utf8_lossy(&o.stdout));
        let _ = std::fs::remove_dir_all(&home);
        super::seen_in_browser(&served);
    }

    /// `superci login modal`: Modal's page (its token flow made), then waited on until approved there (as a real one went:
    /// a while of "not yet", then the token): kept, with the workspace's name, and the command ends by itself.
    #[test]
    fn a_modal_sign_in_waits_for_its_approval_and_keeps_the_token_as_modal_answered_a_real_one() {
        use std::io::BufRead;
        let served = serve(|_| {});
        served.aws.replaying(include_str!("../fixtures/modal/sign-in/token-flow.jsonl"), &[]);
        let home = home(&served);
        std::fs::write(home.join("aws-at"), &served.aws.url).unwrap();
        let port = { let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap(); l.local_addr().unwrap().port() };
        let mut child = common::command(&home).args(["login", "modal", "--no-browser", "--workspace=default"]).env("SUPERCI_PORT", port.to_string())
            .stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::null()).spawn().unwrap();
        let out = child.stdout.take().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || for l in std::io::BufReader::new(out).lines().map_while(Result::ok) { let _ = tx.send(l); });
        let link = loop { let l = rx.recv_timeout(std::time::Duration::from_secs(20)).expect("its link"); if let Some(w) = l.split_whitespace().find(|w| w.starts_with("http://localhost")) { break w.to_string() } };
        // The browser opens the sign-in: to Modal's page, its flow made.
        let (_, _, page) = common::get(&link, "/connect/modal");
        assert!(page.contains("https://modal.com/token-flow/tf-EXAMPLE"), "{}", words(&page));
        // Approved there: the command says so and ends.
        let until = std::time::Instant::now() + std::time::Duration::from_secs(60);
        let mut said = vec![];
        while let Ok(l) = rx.recv_timeout(until.saturating_duration_since(std::time::Instant::now())) { said.push(l) }
        let status = child.wait().unwrap();
        assert!(status.success() && said.iter().any(|l| l.contains("Modal workspace acmeci1")), "{status} {said:?}");
        let kept: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(home.join("workspaces/default/sign-ins.json")).unwrap()).unwrap();
        assert_eq!((kept["modal"]["token_id"].as_str(), kept["modal"]["workspace"].as_str()), (Some("ak-EXAMPLE000000000000001"), Some("acmeci1")), "{kept}");
        let _ = std::fs::remove_dir_all(&home);
        super::seen_in_browser(&served);
    }

    #[test]
    fn an_aws_sign_in_that_aws_does_not_finish_says_so_and_its_button_signs_in_again() {
        // With one workspace kept, and with another beside it (its button then names the workspace it is for).
        for several in [false, true] {
        let served = serve(|_| {});
        let home = home(&served);
        std::fs::write(home.join("aws-at"), &served.aws.url).unwrap();
        if several { std::fs::create_dir_all(home.join("workspaces/globex")).unwrap(); std::fs::write(home.join("workspaces/globex/sign-ins.json"), "{}").unwrap(); }
        let (mut child, link) = dashboard(&home);
        // Signing in: AWS's page, with the state it comes back with.
        let (_, to, page) = common::post(&link, "/aws/signin", &[("next", "?p=overview"), ("w", "default")]);
        let at = if to.is_empty() { page } else { to };
        let state = at.split("state=").nth(1).and_then(|r| r.split(['&', '"']).next()).expect("a state").to_string();
        // AWS comes back, and then refuses its last step.
        let (status, _, refused) = common::get(&link, &format!("/oauth/callback?code=a-code-from-aws&state={state}"));
        let said = words(&refused);
        assert!(status == 400 && said.contains("AWS did not finish the sign-in") && said.contains("is otherwise malformed") && said.contains("Sign in again") && !said.contains("Something went wrong"), "{status} {said}");
        // Its button: on the dashboard's own address, signing in again (signed out of AWS first), then back where it was.
        let again = refused.split("class=\"button primary\" href=\"").nth(1).and_then(|r| r.split('"').next()).unwrap().replace("&amp;", "&");
        assert!(again.starts_with("http://localhost:") && again.contains("/aws/again?next=") && again.contains("p%3Doverview"), "{again}");
        let (status, to, said) = common::get(&link, &again[again.find("/aws/again").unwrap()..]);
        assert!(status == 303 && to.contains("logout") && to.contains("state%3D"), "several: {several}: {again} {status} {to} {}", words(&said));
        let _ = child.kill();
        let _ = std::fs::remove_dir_all(&home);
        }
    }

    #[test]
    fn what_the_dashboard_is_given_is_checked_each_part_on_its_own_and_a_refusal_changes_nothing() {
        // With Cloudflare's own containers: a GitHub job running there (for removing a provider that runs one).
        let served = serve(|c| c.containers = true);
        served.connect_github();
        let home = signed_in(&served);
        let (id, queued) = served.github.queue("test", "superci", served.now_ms());
        assert_eq!(served.webhook(queued), 202);
        assert_eq!(served.webhook(served.github.start(id)), 202);
        let (mut child, link) = dashboard(&home);
        let said = |r: (u16, String)| (r.0, words(&r.1));

        // An artifact: a run's id in digits, and a name.
        for q in ["/run/artifact?run=&name=a", "/run/artifact?run=12a&name=a", "/run/artifact?run=12&name="] {
            let (status, page) = said(part(&link, q));
            assert!(status == 400 && page.contains("Which artifact?"), "{q}: {status} {page}");
        }
        // A job's log: its id in digits, and a GitLab connection's name as one is written.
        for q in ["/job/view?id=", "/job/view?id=1x", "/job/view?id=1&gl=Not-This"] {
            let (status, page) = said(part(&link, q));
            assert!(status == 400 && page.contains("Which job?"), "{q}: {status} {page}");
        }
        assert_eq!(part(&link, &format!("/job/view?id={id}")).0, 200, "its own");

        // GitLab's address: https, and no spaces; a connection named that is not here.
        for (url, which, refused) in [("http://gitlab.com", "new", "GitLab's address starts with https://"), ("https://git lab.com", "new", "GitLab's address starts with https://"), ("https://gitlab.com", "gnope", "That GitLab is not connected here")] {
            let (status, _, page) = common::post(&link, "/gitlab/connect", &[("which", which), ("url", url), ("token", superci_testkit::gitlab::TOKEN)]);
            assert!(status == 400 && words(&page).contains(refused), "{url} {which}: {status} {}", words(&page));
        }
        assert!(served.aws.settings(superci_testkit::PLANE).iter().all(|(k, _)| k != "GITLAB"), "nothing connected");

        // Removing a provider: for the control plane it was shown for; one of the known; and not under a running job
        // unless that job is to be stopped too.
        let (status, _, page) = common::post(&link, "/runners/remove", &[("plane", "otherplane01"), ("cloud", "cloudflare")]);
        assert!(status == 409 && words(&page).contains("Another control plane is in use now"), "{status} {}", words(&page));
        let (status, _, page) = common::post(&link, "/runners/remove", &[("plane", superci_testkit::PLANE), ("cloud", "nowhere")]);
        assert!(status == 400 && words(&page).contains("Unknown provider"), "{status} {}", words(&page));
        let (status, _, page) = common::post(&link, "/runners/remove", &[("plane", superci_testkit::PLANE), ("cloud", "cloudflare")]);
        assert!(status == 409 && words(&page).contains("1 job is running on Cloudflare"), "{status} {}", words(&page));
        assert!(served.aws.settings(superci_testkit::PLANE).iter().any(|(k, v)| k == "CONTAINERS" && v == "on"), "still there");

        // A key's name: letters and digits, 24 at most; and its days, 1 to 366.
        let keys = || json(&home, &["keys", "list"]).1["keys"].as_array().map(Vec::len);
        let before = keys();
        for (args, refused) in [(&["keys", "create", "agent-1"][..], "letters and digits"), (&["keys", "create", "abcdefghijklmnopqrstuvwxy"], "letters and digits"), (&["keys", "create", "agent", "--days=0"], "1 to 366 days"), (&["keys", "create", "agent", "--days=367"], "1 to 366 days")] {
            let (code, v) = json(&home, args);
            assert!(code != 0 && v["error"].as_str().unwrap_or_default().contains(refused), "{args:?}: {v}");
        }
        assert_eq!(keys(), before, "no key made");
        let _ = child.kill();
        let _ = std::fs::remove_dir_all(&home);
        super::seen_in_browser(&served);
    }

    #[test]
    fn a_page_asked_for_often_still_shows_what_happened_a_few_seconds_ago() {
        let served = serve(|_| {});
        served.connect_github();
        let home = signed_in(&served);
        let (mut child, link) = dashboard(&home);
        // Read once, with no jobs yet.
        assert!(!words(&part(&link, "/?p=jobs&fragment=1").1).contains("build"));
        // A job starts; the page is asked for every half second (several tabs, or a page that looks often).
        let (id, queued) = served.github.queue("build", "superci", served.now_ms());
        assert_eq!(served.webhook(queued), 202);
        assert_eq!(served.webhook(served.github.start(id)), 202);
        let (began, mut shown) = (std::time::Instant::now(), false);
        while began.elapsed() < std::time::Duration::from_secs(15) {
            if words(&part(&link, "/?p=jobs&fragment=1").1).contains("build") { shown = true; break }
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
        assert!(shown, "the new job not shown after 15 s of asking every half second");
        assert!(began.elapsed() < std::time::Duration::from_secs(8), "shown only after {:?}", began.elapsed());
        let _ = child.kill();
        let _ = std::fs::remove_dir_all(&home);
        super::seen_in_browser(&served);
    }

    /// What the Runners and Settings pages' forms change, each as its page posts it: where jobs go (the default cloud, a
    /// rule for repositories, taken out again), a read key taken out, a repository's cache entries taken out; and what is
    /// refused, with nothing changed (a repository that is no repository, an unknown change, a budget choice AWS has not).
    #[test]
    fn the_routing_keys_and_cache_forms_change_the_control_plane() {
        let served = superci_testkit::serve(|_| {});
        let home = common::signed_in(&served);
        let (mut child, link) = common::dashboard(&home);
        let routing = || served.aws.settings(superci_testkit::PLANE).into_iter().find(|(k, _)| k == "ROUTING").and_then(|(_, v)| serde_json::from_str::<serde_json::Value>(&v).ok()).unwrap_or_default();
        let post = |path: &str, fields: &[(&str, &str)]| common::post(&link, path, fields);
        assert!(post("/routing", &[("action", "default"), ("cloud", "aws")]).0 < 400);
        assert_eq!(routing()["default"], "aws");
        assert!(post("/routing", &[("action", "add"), ("repo", "acme/*"), ("cloud", "cloudflare")]).0 < 400);
        assert_eq!(routing()["rules"], serde_json::json!([{ "repo": "acme/*", "cloud": "cloudflare" }]));
        let (status, _, said) = post("/routing", &[("action", "add"), ("repo", "not a repository"), ("cloud", "aws")]);
        assert!(status == 400 && common::words(&said).contains("A repository is owner/name, owner/* or *"), "{status} {}", common::words(&said));
        assert!(post("/routing", &[("action", "remove"), ("repo", "acme/*")]).0 < 400);
        assert_eq!(routing()["rules"], serde_json::json!([]));
        assert_eq!(post("/routing", &[("action", "nothing-like-it")]).0, 400);
        let (status, _, said) = post("/routing", &[("action", "account_usd"), ("account", "aws"), ("usd", "40"), ("budget", "maybe")]);
        assert!(status == 400 && common::words(&said).contains("What AWS does at the limit is none, emails or stops"), "{status} {}", common::words(&said));
        // A read key taken out from Settings → Keys.
        let read = |n: &str| served.aws.settings(superci_testkit::PLANE).iter().any(|(k, _)| k.starts_with("READ_KEY_") && k.ends_with(n));
        assert!(read("_AGENT"));
        assert!(post("/keys/remove", &[("read", "agent")]).0 < 400);
        assert!(!read("_AGENT"));
        // A repository's cache entries taken out (from the control plane's own store).
        let (status, to, _) = post("/caches/delete", &[("repo", "acme/app")]);
        assert!(status == 303 && to.contains("p=settings"), "{status} {to}");
        let _ = child.kill();
        let _ = std::fs::remove_dir_all(&home);
        super::seen_in_browser(&served);
    }

    /// A control plane that does not answer (starting with a new setting, or gone a moment): what changes it says to try
    /// again in a few seconds, and nothing is changed (labels too: one that does not answer is not taken for an old one).
    #[test]
    fn changes_to_a_control_plane_that_does_not_answer_say_to_try_again() {
        let served = serve(|_| {});
        let home = common::signed_in(&served);
        let silent = { let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap(); format!("http://127.0.0.1:{}", l.local_addr().unwrap().port()) };
        let file = home.join("workspaces/default/sign-ins.json");
        std::fs::write(&file, std::fs::read_to_string(&file).unwrap().replace(&served.url, &silent)).unwrap();
        let (mut child, link) = dashboard(&home);
        for (path, fields) in [("/routing", &[("action", "default"), ("cloud", "aws")][..]), ("/plane/update", &[("plane", "")]), ("/labels", &[("label", "superci-gpu")]),
            ("/runners/remove", &[("cloud", "cloudflare")])] {
            let (status, _, said) = common::post(&link, path, fields);
            assert!(status == 503 && words(&said).contains("The control plane did not answer yet"), "{path}: {status} {}", words(&said));
        }
        let _ = child.kill();
        let _ = std::fs::remove_dir_all(&home);
        super::seen_in_browser(&served);
    }

    /// Where the dashboard listens: the port asked for, refused when taken; else the usual one, or the next free one
    /// when another program has it (said, and how two share sign-ins); a Cloudflare sign-in needs the usual one itself.
    #[test]
    fn the_dashboard_listens_where_it_can_and_says_where_and_why_not() {
        use std::io::BufRead;
        let served = serve(|_| {});
        let home = home(&served);
        let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = taken.local_addr().unwrap().port();
        // (The other address of the same port too, as another program listening on both would.)
        let _taken6 = std::net::TcpListener::bind(format!("[::1]:{port}"));
        let first_line = |c: &mut std::process::Child| -> String { std::io::BufReader::new(c.stdout.take().unwrap()).lines().map_while(Result::ok).take(2).collect::<Vec<_>>().join("\n") };
        // Asked for a port that is taken: refused.
        let o = common::command(&home).args(["dashboard", "--no-browser", "--workspace=default"]).env("SUPERCI_PORT", port.to_string()).output().unwrap();
        assert!(!o.status.success() && String::from_utf8_lossy(&o.stderr).contains(&format!("port {port} is in use")), "{}", String::from_utf8_lossy(&o.stderr));
        // Its usual port taken: the next one, said.
        let mut c = common::command(&home).args(["dashboard", "--no-browser", "--workspace=default"]).env("SUPERCI_OAUTH_PORT", port.to_string()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::null()).spawn().unwrap();
        let said = first_line(&mut c);
        let _ = c.kill();
        assert!(said.contains(&format!("Port {port} is taken (another SuperCI?): this one is on ")) && said.contains("http://localhost:"), "{said}");
        // A Cloudflare sign-in comes back to that port only: with it taken, said why it cannot start.
        let o = common::command(&home).args(["login", "cloudflare", "--no-browser"]).env("SUPERCI_OAUTH_PORT", port.to_string()).output().unwrap();
        let e = String::from_utf8_lossy(&o.stderr).to_string() + &String::from_utf8_lossy(&o.stdout);
        assert!(!o.status.success() && e.contains(&format!("Cloudflare's sign-in comes back to port {port} only")), "{e}");
        let _ = std::fs::remove_dir_all(&home);
        super::seen_in_browser(&served);
    }

    /// Who the dashboard answers: this computer's own address only, changes only from its own pages (by Sec-Fetch-Site, or
    /// else by Origin), and only a tab its link connected (by its cookie); a sign-in's way back passes without the cookie.
    #[test]
    fn only_this_computers_own_address_its_own_pages_and_a_connected_tab_are_answered() {
        use std::io::{Read, Write};
        let served = serve(|_| {});
        let home = home(&served);
        let (_child, link) = dashboard(&home);
        let (base, key) = link.split_once("/?k=").unwrap();
        let port = base.rsplit(':').next().unwrap().to_string();
        let ask = |method: &str, path: &str, headers: &[(&str, &str)]| -> (u16, String) {
            let mut s = std::net::TcpStream::connect(("127.0.0.1", port.parse::<u16>().unwrap())).unwrap();
            let mut req = format!("{method} {path} HTTP/1.1\r\nconnection: close\r\ncontent-length: 0\r\n");
            for (k, v) in headers { req.push_str(&format!("{k}: {v}\r\n")) }
            req.push_str("\r\n");
            s.write_all(req.as_bytes()).unwrap();
            let mut out = String::new();
            let _ = s.read_to_string(&mut out);
            (out.split_whitespace().nth(1).and_then(|c| c.parse().ok()).unwrap_or(0), words(&out))
        };
        let cookie = format!("superci_local={key}");
        let here = format!("localhost:{port}");
        // Another name pointing here, or no port, or one that is no number: not this computer's address.
        for host in ["evil.example:8976".to_string(), "localhost:".to_string(), "localhost:abc".to_string()] {
            let (status, said) = ask("GET", "/", &[("host", &host), ("cookie", &cookie)]);
            assert!(status == 403 && said.contains("Not this computer's address"), "{host}: {status}");
        }
        // A change from another site: by Sec-Fetch-Site, or by its Origin where that is not said.
        for headers in [vec![("sec-fetch-site", "cross-site")], vec![("origin", "http://evil.example")]] {
            let mut h = vec![("host", here.as_str()), ("cookie", cookie.as_str())];
            h.extend(headers.iter().copied());
            let (status, said) = ask("POST", "/labels", &h);
            assert!(status == 403 && said.contains("Not from your dashboard"), "{headers:?}: {status} {said}");
        }
        // From its own page (an Origin of null, as its no-referrer forms send, or its own): let through.
        let own = format!("http://{here}");
        for origin in ["null", own.as_str()] {
            let (status, said) = ask("POST", "/routing", &[("host", &here), ("cookie", &cookie), ("origin", origin)]);
            assert!(!said.contains("Not from your dashboard"), "{origin}: {status}");
        }
        // A tab its link did not connect: said so; a sign-in's way back passes on to be checked by its state.
        let (status, said) = ask("GET", "/?p=overview", &[("host", &here)]);
        assert!(status == 403 && said.contains("This tab is not connected to your dashboard"), "{status}");
        let (_, said) = ask("GET", "/oauth/callback?code=x&state=y", &[("host", &here)]);
        assert!(!said.contains("This tab is not connected"), "{said}");
        let _ = std::fs::remove_dir_all(&home);
        super::seen_in_browser(&served);
    }
}

/// A machine's own word, as a spot machine on AWS sends it: how its start went, its runner's log while it waits for
/// GitHub, a step's lines as they are written, then AWS taking it back. Each is read on the job's page.
#[test]
fn a_machines_own_word_its_start_its_runners_log_its_lines_and_aws_taking_it_back_is_said_on_its_page() {
    let Some(mut b) = Browser::open(1300, 1000) else { eprintln!("no Chrome here: skipped"); return };
    let served = serve(|c| {
        (c.aws_own, c.aws_own_creds) = (Some(("111111111111".into(), "us-east-1".into())), Some(superci_core::aws::Credentials { access_key_id: "ASIAX".into(), secret_access_key: "secret".into(), session_token: Some("tok".into()), expires_at_ms: u64::MAX }));
    });
    // (Behind its public address, as a deployed one is: its machines get links of their own.)
    *served.public.lock().unwrap() = Some("https://plane.acme.test".into());
    served.connect_github();
    served.aws.put(&format!("/superci/{PLANE}/ROUTING"), r#"{"default":null,"rules":[],"order":[{"cloud":"aws"},{"cloud":"aws-on-demand"},{"cloud":"cloudflare","off":true}]}"#);
    served.aws.replaying(include_str!("../fixtures/aws/launch/spot.jsonl"), &[]);
    let started: String = include_str!("../fixtures/aws/launch/on-demand.jsonl").lines().filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .find(|v| v["asked"].as_str().unwrap_or_default().contains("Action=RunInstances") && v["status"] == 200).map(|v| v["answer"].as_str().unwrap_or_default().to_string()).unwrap();
    served.store.0.lock().unwrap().insert("aws:no-fleet".into(), served.now_ms().to_string());
    served.aws.fail("RunInstances", 200, &started, None);
    let (id, q) = served.github.queue("tests", "superci", served.now_ms());
    assert!(served.webhook(q) < 300);
    served.aws.fail_none();
    let job = served.store.under(&format!("job:{id}"))[0].1.clone();
    let (runner, hello, notice) = (job["runner"].as_str().unwrap().to_string(), job["hello"].as_str().expect("a link to say how its start went").to_string(), job["notice"].as_str().expect("a link to say AWS takes it back").to_string());
    let agent = ureq::Agent::new_with_defaults();
    let say = |q: &str, body: &str| agent.post(&format!("{}/{q}", served.url)).send(body.to_string()).map(|r| r.status().as_u16()).unwrap_or_else(|e| match e { ureq::Error::StatusCode(c) => c, _ => 0 });
    let now = served.now_ms();
    // (Its region's bucket ready: what its runner writes as it goes is kept there, and read back from there.)
    let bucket = superci_core::cache::bucket(PLANE, "us-east-1");
    served.github.put_object(&format!("{bucket}/{}", superci_core::cache::program_key("x86_64")), b"program");
    // How its start went: booted, its script began, its runner ready and listening.
    assert_eq!(say(&format!("hello?runner={runner}&t={hello}&boot=6.61&script={}&read={}&ready={}", now - 9000, now - 8000, now - 7000), ""), 200);
    assert_eq!(say(&format!("hello?runner={runner}&t={hello}&listening={}&gh=12", now - 3000), ""), 200);
    // Without its link: nothing. Times far from now, and a wait for GitHub out of range, are not taken.
    assert_eq!(say(&format!("hello?runner={runner}&t=not-its-link-at-all-xx&ready={now}"), ""), 404);
    assert_eq!(say(&format!("hello?runner={runner}&t={hello}&read={}&gh=999", now - 86_400_000), ""), 200);
    // By a GitLab job's id that is none, or no id at all: nothing.
    for q in ["hello?gl=&t=x", "hello?gl=NOT-AN-ID&t=x", "hello?t=x", "interrupted?gl=NOT-AN-ID&t=x", "interrupted?t=x",
        // (An id as GitLab's are written that is no job here; one with a letter no id has.)
        "hello?gl=ab-12x&t=x", "hello?gl=ab!&t=x", "hello?gl=a-&t=x", "interrupted?gl=ab-12x&t=x", "interrupted?gl=ab!&t=x", "interrupted?gl=9-&t=x", "interrupted?gl=&t=x"] { assert_eq!(say(q, ""), 404, "{q}") }
    // A link too short to be one of SuperCI's (a record from before, edited): taken as none.
    let key = format!("job:{id}");
    let edit = |f: &dyn Fn(&mut serde_json::Value)| { let mut m = served.store.0.lock().unwrap(); let mut j: serde_json::Value = serde_json::from_str(&m[&key]).unwrap(); f(&mut j); m.insert(key.clone(), j.to_string()); };
    edit(&|j| { j["hello"] = "short".into(); j["notice"] = "short".into(); });
    assert_eq!(say(&format!("hello?runner={runner}&t=short"), ""), 404);
    assert_eq!(say(&format!("interrupted?runner={runner}&t=short"), ""), 404);
    edit(&|j| { j["hello"] = hello.clone().into(); j["notice"] = notice.clone().into(); });
    // A page larger than any a machine sends, and one named as no page is: nothing kept.
    assert_eq!(say(&format!("hello?runner={runner}&t={hello}&live=aaaa_4444_1.log&at=0"), &"x".repeat(4_000_001)), 200);
    assert_eq!(say(&format!("hello?runner={runner}&t={hello}&live=aaaa_4444_1.txt&at=0"), "x"), 200);
    // The start of its runner's own log, with the job message in it: its steps' shape; one without: nothing made of it.
    let message = include_str!("../../core/tests/fixtures/runner/parallel.github.json");
    assert_eq!(say(&format!("hello?runner={runner}&t={hello}&worker=1"), &format!("[2026-10-15 10:00:02Z INFO Worker] Job message:\n {message}\n[2026-10-15 10:00:02Z INFO Worker] Starting the job {{\n")), 200);
    assert_eq!(say(&format!("hello?runner={runner}&t={hello}&worker=1"), "[2026-10-15 10:00:02Z INFO Worker] Waiting\n"), 200);
    // A page with nothing in it, or named as no page is: nothing kept.
    assert_eq!(say(&format!("hello?runner={runner}&t={hello}&live=aaaa_3333_1.log&at=0"), ""), 200);
    assert_eq!(say(&format!("hello?runner={runner}&t={hello}&live={}.log&at=0", "a".repeat(120)), "x"), 200);
    // Its runner's log as it comes, a bell and bytes that are no text among it: text kept, the rest left out; an empty
    // one is nothing.
    let raw = |q: &str, body: Vec<u8>| agent.post(&format!("{}/{q}", served.url)).send(body).map(|r| r.status().as_u16()).unwrap_or(0);
    assert_eq!(raw(&format!("hello?runner={runner}&t={hello}&log=runner"), [b"[2026-10-13 22:56:40Z INFO Runner] Starting now
".as_slice(), &[0xff, 0xfe], b" end
"].concat()), 200);
    assert_eq!(say(&format!("hello?runner={runner}&t={hello}&log=runner"), ""), 200);
    // Pages named with each kind of letter a page's name has; one with another kind is no page.
    assert_eq!(say(&format!("hello?runner={runner}&t={hello}&live=Ab-9_x.y_1.log&at=0"), "2026-10-15T10:00:04.0000000Z early\n"), 200);
    assert_eq!(say(&format!("hello?runner={runner}&t={hello}&live=a%2Fb_1.log&at=0"), "x"), 200);
    // Its runner's own log, while it waits for GitHub.
    assert_eq!(say(&format!("hello?runner={runner}&t={hello}&log=runner"), "[2026-10-13 22:56:41Z WARN GitHubActionsService] Attempt 1 of POST request failed (Socket Error: TryAgain).\n"), 200);
    // The job taken; a step's lines as they are written, before GitHub has any.
    assert!(served.webhook(served.github.start(id)) < 300);
    assert_eq!(say(&format!("hello?runner={runner}&t={hello}&live=aaaa_2222_1.log&at=0"), "2026-10-15T10:00:05.0000000Z ##[group]Run npm test\n2026-10-15T10:00:06.0000000Z ok 1 - adds\n"), 200);
    let home = signed_in(&served);
    let (mut child, link) = common::dashboard_with(&home, &[("SUPERCI_VIEWS_S", "0")]);
    b.goto(&format!("{link}&p=job&id={id}"));
    b.wait(&format!("{} && {}", shows("Its start"), shows("Running")));
    let page = b.eval("document.querySelector('main').innerText").as_str().unwrap_or_default().replace('\n', " ");
    for said in ["6.6 s it booted", "GitHub refused the first connection; reachable after 12 s", "GitHub gave it the job", "Attempt 1 of POST request failed"] { assert!(page.contains(said), "no {said:?}:\n{page}") }
    // Its step's lines, as its machine sent them, in the log its page shows (GitHub has none yet).
    let view = b.eval(&format!("fetch('/job/view?id={id}', {{credentials: 'same-origin'}}).then(r => r.text())")).as_str().unwrap_or_default().to_string();
    assert!(view.contains("ok 1 - adds") && view.contains("Run npm test"), "{view}");
    // GitHub has the first of its log by now (its runner sends it in blocks): GitHub's, then what its machine sent after.
    let _ = served.github.finish(id, "success", "2026-10-15T10:00:04.5000000Z from GitHub's block\n", served.now_ms());
    let raw = b.eval(&format!("fetch('/job/raw?id={id}', {{credentials: 'same-origin', redirect: 'manual'}}).then(r => r.status)"));
    assert!(raw.as_u64().is_some(), "{raw}");
    let view = b.eval(&format!("fetch('/job/view?id={id}', {{credentials: 'same-origin'}}).then(r => r.text())")).as_str().unwrap_or_default().to_string();
    assert!(view.contains("from GitHub") && view.contains("ok 1 - adds"), "{view}");
    // AWS takes it back: said by its machine (with its own link), the job then failed by GitHub.
    assert_eq!(say(&format!("interrupted?runner={runner}&t=not-its-link-at-all-xx"), ""), 404);
    // (Its zone not kept, as on a record from before: no pool is put last for it.)
    edit(&|j| { j.as_object_mut().unwrap().remove("zone"); });
    let pools = served.store.under("aws:pool:").len();
    assert_eq!(say(&format!("interrupted?runner={runner}&t={notice}"), ""), 200);
    assert_eq!(served.store.under("aws:pool:").len(), pools);
    // Said again (its watch may say it twice): one record.
    assert_eq!(say(&format!("interrupted?runner={runner}&t={notice}"), ""), 200);
    assert_eq!(served.store.under("spot:interrupted:").len(), 1);
    assert!(served.webhook(served.github.finish(id, "failure", "2026-10-15T10:00:07.0000000Z The runner has received a shutdown signal.", served.now_ms())) < 300);
    b.goto(&format!("{link}&p=job&id={id}"));
    b.wait(&shows("Status"));
    b.wait(&format!("{} && {}", shows("AWS took back its spot machine"), shows("Failed")));
    // Its run over at GitHub: the next sweep asks GitHub to run the job again, off spot machines.
    served.upkeep();
    assert!(served.github.asked.lock().unwrap().iter().any(|a| a.contains(&format!("/actions/jobs/{id}/rerun"))), "{:?}", served.github.asked.lock().unwrap().iter().filter(|a| a.contains("rerun")).collect::<Vec<_>>());
    assert_eq!(served.store.under(&format!("job:{id}"))[0].1["rerun"]["state"], "asked");
    // Its notice again once its job has ended: its runner leads to no job any more.
    assert_eq!(say(&format!("interrupted?runner={runner}&t={notice}"), ""), 404);
    assert_eq!(served.store.under("spot:interrupted:").len(), 1);
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

/// Where a job goes, by the order, its limits and rules and what the job asks, on a control plane with its own
/// Cloudflare containers and an AWS account: each job's page says where it runs, or why it waits or failed.
#[test]
fn where_each_job_goes_by_the_order_its_limits_rules_and_labels_is_said_on_its_page() {
    let Some(mut b) = Browser::open(1300, 900) else { eprintln!("no Chrome here: skipped"); return };
    let started: String = include_str!("../fixtures/aws/launch/on-demand.jsonl").lines().filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .find(|v| v["asked"].as_str().unwrap_or_default().contains("Action=RunInstances") && v["status"] == 200).map(|v| v["answer"].as_str().unwrap_or_default().to_string()).unwrap();
    // A month's spend on AWS before (two hours at $2 an hour), for the limits.
    let spent = |served: &superci_testkit::Served| { let now = served.now_ms(); served.store.0.lock().unwrap().insert("job:50".into(), serde_json::json!({ "job_id": 50, "run_id": 1, "repo": "acme/app", "state": "done", "at_ms": now - 7_200_000, "ended_ms": now - 60_000,
        "installation_id": 42, "cloud": "aws", "usd_per_hour": 2.0, "seen_in_progress": true }).to_string()); };
    // Each case: the routing, what was spent before, the jobs (their labels), and what each one's page says.
    type Case = (&'static str, &'static str, bool, Vec<(&'static str, &'static str)>);
    let cases: Vec<Case> = vec![
        ("Cloudflare one job at a time", r#"{"order":[{"cloud":"cloudflare","max_jobs":1},{"cloud":"aws","off":true},{"cloud":"aws-on-demand","off":true}]}"#, false,
            vec![("superci", "Starting"), ("superci", "Cloudflare runs 1 at once")]),
        ("a job too large for Cloudflare goes to AWS", r#"{"order":[{"cloud":"cloudflare"},{"cloud":"aws"}]}"#, false, vec![("superci/cpu=8", "8 CPU · m8a.2xlarge"), ("superci", "4 CPU · 12 GB")]),
        ("a job only Cloudflare may run, too large for it", r#"{"order":[{"cloud":"cloudflare"},{"cloud":"aws"}]}"#, false, vec![("superci/provider=cloudflare/cpu=16", "Cloudflare")]),
        ("a rule sends the repository to Cloudflare", r#"{"order":[{"cloud":"aws"},{"cloud":"cloudflare"}],"rules":[{"repo":"acme/*","cloud":"cloudflare"}]}"#, false, vec![("superci", "4 CPU · 12 GB")]),
        ("AWS spot past its month's limit: on-demand", r#"{"order":[{"cloud":"aws","monthly_usd":1.0},{"cloud":"cloudflare","off":true}]}"#, true, vec![("superci", "c8a.xlarge · on-demand")]),
        ("past its limit with on-demand off: nowhere", r#"{"order":[{"cloud":"aws","monthly_usd":1.0},{"cloud":"aws-on-demand","off":true},{"cloud":"cloudflare","off":true}]}"#, true, vec![("superci", "Nowhere to run the default machine: AWS reached its $1 a month")]),
        ("the whole CI past its month's limit", r#"{"order":[{"cloud":"aws"}],"monthly_usd":1.0}"#, true, vec![("superci", "reached its limit")]),
        ("the whole CI past its day's limit", r#"{"order":[{"cloud":"aws"}],"daily_usd":1.0}"#, true, vec![("superci", "a day")]),
        ("larger than the largest machine allowed", r#"{"order":[{"cloud":"aws"}],"max_cpu":8}"#, false, vec![("superci/cpu=16", "16")]),
        ("a label that is not understood", r#"{"order":[{"cloud":"aws"}]}"#, false, vec![("superci/cpus=8", "cpus")]),
        ("nothing to run Windows", r#"{"order":[{"cloud":"cloudflare"},{"cloud":"aws","off":true},{"cloud":"aws-on-demand","off":true}]}"#, false, vec![("superci/os=windows", "Nowhere to run Windows: Cloudflare runs Linux only")]),
        ("macOS, which SuperCI does not run", r#"{"order":[{"cloud":"aws"}]}"#, false, vec![("superci/os=macos", "macOS")]),
    ];
    // And with settings beside the routing ("-aws": AWS not connected).
    let launcher = r#"[{"cloud":"cloudflare","url":"https://superci-runners.acme.workers.dev"}]"#;
    let more: Vec<(&'static str, &'static str, bool, Vec<(&'static str, &'static str)>, Vec<(&str, &str)>)> = vec![
        ("the AWS account past its month's limit", r#"{"order":[{"cloud":"aws"},{"cloud":"cloudflare","off":true}],"account_usd":{"aws":1.0}}"#, true, vec![("superci", "the AWS account reached its $1 a month")], vec![]),
        ("the AWS account past its day's limit", r#"{"order":[{"cloud":"aws"},{"cloud":"cloudflare","off":true}],"account_daily_usd":{"aws":1.0}}"#, true, vec![("superci", "the AWS account reached its $1 a day")], vec![]),
        ("a default and no order", r#"{"default":"aws"}"#, false, vec![("superci", "c8a.xlarge")], vec![]),
        ("a default not added here", r#"{"default":"modal"}"#, false, vec![("superci", "Modal is not connected")], vec![]),
        ("a rule naming a provider not added here", r#"{"rules":[{"repo":"acme/*","cloud":"modal"}]}"#, false, vec![("superci", "Modal is not connected")], vec![]),
        ("a label naming a provider not added here", r#"{"order":[{"cloud":"aws"}]}"#, false, vec![("superci/provider=modal", "Its label needs Modal runners")], vec![]),
        ("a label naming AWS, which the order leaves out", r#"{"order":[{"cloud":"cloudflare"}]}"#, false, vec![("superci/provider=aws", "c8a.xlarge")], vec![]),
        ("a rule naming AWS, which the order leaves out", r#"{"order":[{"cloud":"cloudflare"}],"rules":[{"repo":"acme/*","cloud":"aws"}]}"#, false, vec![("superci", "c8a.xlarge")], vec![]),
        ("nothing connected", r#"{}"#, false, vec![("superci", "No cloud connected for jobs")], vec![("CONTAINERS", "off"), ("-aws", "")]),
        ("Cloudflare through a launcher", r#"{}"#, false, vec![("superci", "Status")], vec![("CONTAINERS", "off"), ("LAUNCHERS", launcher), ("-aws", "")]),
        ("Cloudflare's own containers and a launcher beside", r#"{}"#, false, vec![("superci", "Status")], vec![("LAUNCHERS", launcher), ("-aws", "")]),
    ];
    let cases: Vec<(&str, &str, bool, Vec<(&str, &str)>, Vec<(&str, &str)>)> = cases.into_iter().map(|(a, b, c, d)| (a, b, c, d, vec![])).chain(more).collect();
    let mut wrong = vec![];
    for (what, routing, with_spend, jobs, settings) in cases {
        let with_aws = !settings.iter().any(|(k, _)| *k == "-aws");
        let served = serve(move |c| {
            if with_aws { (c.aws_own, c.aws_own_creds) = (Some(("111111111111".into(), "us-east-1".into())), Some(superci_core::aws::Credentials { access_key_id: "ASIAX".into(), secret_access_key: "secret".into(), session_token: Some("tok".into()), expires_at_ms: u64::MAX })); }
        });
        served.connect_github();
        for (k, v) in &settings { if *k != "-aws" { served.aws.put(&format!("/superci/{PLANE}/{k}"), v) } }
        served.aws.put(&format!("/superci/{PLANE}/ROUTING"), routing);
        served.aws.replaying(include_str!("../fixtures/aws/launch/spot.jsonl"), &[]);
        served.store.0.lock().unwrap().insert("aws:no-fleet".into(), served.now_ms().to_string());
        served.aws.fail("RunInstances", 200, &started, None);
        if with_spend { spent(&served) }
        let ids: Vec<(u64, &str, &str)> = jobs.iter().map(|(label, said)| { let (id, q) = served.github.queue(label, label, served.now_ms()); assert!(served.webhook(q) < 300, "{what}"); (id, *label, *said) }).collect();
        let home = signed_in(&served);
        let (mut child, link) = dashboard(&home);
        for (id, label, said) in ids {
            b.goto(&format!("{link}&p=job&id={id}"));
            b.wait(&format!("{} && !document.querySelector('main').innerText.includes('Reading your control plane')", shows("Status")));
            let page = b.eval("document.querySelector('main').innerText").as_str().unwrap_or_default().replace('\n', " ");
            let job = served.store.under(&format!("job:{id}"))[0].1.clone();
            if !page.contains(said) { wrong.push(format!("{what}: {label}: no {said:?} ({} {}): {}", job["state"], job["error"], page.chars().take(400).collect::<String>())) }
        }
        let _ = child.kill();
        let _ = std::fs::remove_dir_all(&home);
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n\n"));
}

/// Jobs on each other's runners, as GitHub hands a job to any runner with its label, and a machine that never begins
/// its job: each job's page says where it ran and how it ended, the sweep's tries and end too.
#[test]
fn jobs_on_each_others_runners_and_a_machine_that_never_begins_are_said_on_their_pages() {
    let Some(mut b) = Browser::open(1300, 900) else { eprintln!("no Chrome here: skipped"); return };
    let served = serve(|_| {});
    served.connect_github();
    let state = |id: u64| served.store.under(&format!("job:{id}"))[0].1.clone();
    let runner_of = |id: u64| state(id)["runner"].as_str().unwrap().to_string();
    // Two jobs, each its machine; the second's runner takes the first, and the first's machine then the second.
    let (a, q) = served.github.queue("first", "superci", served.now_ms()); served.webhook(q);
    let (c, q) = served.github.queue("second", "superci", served.now_ms()); served.webhook(q);
    let (ra, rc) = (runner_of(a), runner_of(c));
    served.webhook(served.github.start_on(a, &rc));
    served.webhook(served.github.start_on(c, &ra));
    served.webhook(served.github.finish(a, "success", "2026-10-15T10:00:01.0000000Z ok", served.now_ms()));
    served.webhook(served.github.finish(c, "failure", "2026-10-15T10:00:01.0000000Z not ok", served.now_ms()));
    // A third whose machine was given its job, then the job went to another runner after its machine failed.
    let (d, q) = served.github.queue("third", "superci", served.now_ms()); served.webhook(q);
    let (e, q) = served.github.queue("fourth", "superci", served.now_ms()); served.webhook(q);
    { let mut m = served.store.0.lock().unwrap(); let k = format!("job:{d}"); let mut j: serde_json::Value = serde_json::from_str(&m[&k]).unwrap(); j["state"] = "failed".into(); m.insert(k, j.to_string()); }
    served.webhook(served.github.start_on(d, &runner_of(e)));
    // A fifth whose machine never begins it: tried again by the sweep, twice, then failed with why.
    let (f, q) = served.github.queue("fifth", "superci", served.now_ms()); served.webhook(q);
    for _ in 0..4 { served.later(4 * 60_000); served.upkeep(); }
    let home = signed_in(&served);
    let (mut child, link) = dashboard(&home);
    for (id, said) in [(a, "Passed"), (c, "Failed"), (d, "Running"), (f, "Failed")] {
        b.goto(&format!("{link}&p=job&id={id}"));
        b.wait(&format!("{} && !document.querySelector('main').innerText.includes('Reading your control plane')", shows("Status")));
        let page = b.eval("document.querySelector('main').innerText").as_str().unwrap_or_default().replace('\n', " ");
        assert!(page.contains(said), "job {id}: no {said:?} ({}):\n{page}", state(id));
    }
    // The fifth's page says why it ended.
    let why = state(f)["error"].as_str().unwrap_or_default().chars().take(30).collect::<String>();
    b.goto(&format!("{link}&p=job&id={f}"));
    b.wait(&shows(&why[..1].to_uppercase()));
    assert!(b.eval("document.querySelector('main').innerText").as_str().unwrap_or_default().to_lowercase().contains(&why.to_lowercase()), "{}", state(f));
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

/// What the dashboard does to the clouds (the order, stopping SuperCI, AWS removed and added again, a deploy, a move, an
/// account looked at), as a real account answered, each scenario ending with what a person sees of it.
mod commands_dashboard_in_browser {
    use super::*;
    use common::{json, superci};

    #[test]
    fn the_order_takes_providers_out_and_puts_them_back_and_each_of_awss_has_its_own_regions() {
        // A control plane whose own AWS account runs jobs (its spot and on-demand machines), with Cloudflare's containers.
        let served = serve(|c| {
            c.containers = true;
            (c.aws_own, c.aws_own_creds) = (Some(("111111111111".into(), "us-east-1".into())), Some(superci_core::aws::Credentials { access_key_id: "ASIAX".into(), secret_access_key: "secret".into(), session_token: Some("tok".into()), expires_at_ms: u64::MAX }));
        });
        let home = common::signed_in(&served);
        let text = |args: &[&str]| { let o = superci(&home, args); String::from_utf8_lossy(&o.stdout).to_string() };
        // The order: the providers named, in that order; the others taken out (kept, and put back by naming them).
        let order = json(&home, &["runners", "order", "cloudflare"]).1;
        assert_eq!((order["order"].clone(), order["taken_out"].clone()), (serde_json::json!(["cloudflare"]), serde_json::json!(["aws", "aws-on-demand"])), "{order}");
        assert!(text(&["runners", "list"]).contains("Taken out of the order: aws, aws-on-demand"));
        let order = json(&home, &["runners", "order", "aws", "cloudflare", "aws-on-demand"]).1;
        assert_eq!((order["order"].clone(), order["taken_out"].clone()), (serde_json::json!(["aws", "cloudflare", "aws-on-demand"]), serde_json::json!([])), "{order}");
        // AWS's on-demand machines in regions of their own (here: N. Virginia, where the control plane is set up already).
        let listed = json(&home, &["runners", "update", "aws-on-demand", "--regions=us-east-1"]).1;
        let on_demand = listed["order"].as_array().unwrap().iter().find(|p| p["cloud"] == "aws-on-demand").cloned().unwrap_or_default();
        assert_eq!((on_demand["regions"].clone(), on_demand["taken_out"].as_bool()), (serde_json::json!(["us-east-1"]), Some(false)), "{listed}");
        assert!(text(&["runners", "list"]).contains("aws-on-demand  in us-east-1"));
        // Not one left in the order: refused (no job could start).
        let (code, none) = json(&home, &["runners", "order", "nowhere"]);
        assert_eq!(code, 2, "{none}");
        // In the dashboard: its rows, AWS's two alike; each one's Take out, and Put back (to the end of the order).
        let (mut child, link) = common::dashboard(&home);
        let routing = || serde_json::from_str::<serde_json::Value>(&served.aws.settings(superci_testkit::PLANE).into_iter().find(|(k, _)| k == "ROUTING").map(|(_, v)| v).unwrap_or("{}".into())).unwrap();
        let (_, page) = common::part(&link, "/?p=runners&fragment=1");
        assert!(page.contains("<strong>AWS spot</strong>") && page.contains("<strong>AWS on-demand</strong>") && page.contains("<h2>Clouds</h2>") && page.matches("data-regions").count() == 2, "{}", common::words(&page));
        let current = "aws,cloudflare,aws-on-demand";
        let (status, to, said) = common::post(&link, "/routing", &[("action", "take_out"), ("cloud", "aws"), ("current", current)]);
        assert!(status == 303 && to.contains("p=runners"), "{status} {said}");
        assert_eq!(routing()["order"], serde_json::json!([{ "cloud": "cloudflare" }, { "cloud": "aws-on-demand", "regions": ["us-east-1"] }, { "cloud": "aws", "off": true, "regions": ["us-east-1"] }]), "(spot's regions kept as they were when on-demand's were set)");
        let (_, page) = common::part(&link, "/?p=runners&fragment=1");
        assert!(common::words(&page).contains("AWS spot Taken out of the order") && !page.contains(r#"data-cloud="aws""#), "{}", common::words(&page));
        common::post(&link, "/routing", &[("action", "put_back"), ("cloud", "aws"), ("current", "cloudflare,aws-on-demand,aws")]);
        assert_eq!(routing()["order"], serde_json::json!([{ "cloud": "cloudflare" }, { "cloud": "aws-on-demand", "regions": ["us-east-1"] }, { "cloud": "aws", "regions": ["us-east-1"] }]));
        // The last one in the order is not taken out.
        common::post(&link, "/routing", &[("action", "take_out"), ("cloud", "aws"), ("current", "cloudflare,aws-on-demand,aws")]);
        common::post(&link, "/routing", &[("action", "take_out"), ("cloud", "aws-on-demand"), ("current", "cloudflare,aws-on-demand,aws")]);
        let (status, _, said) = common::post(&link, "/routing", &[("action", "take_out"), ("cloud", "cloudflare"), ("current", "cloudflare,aws-on-demand,aws")]);
        assert!(status == 400 && said.contains("Keep one provider in the order"), "{status} {said}");
        let _ = child.kill();
        let _ = std::fs::remove_dir_all(&home);
        super::seen_in_browser(&served);
    }

    #[test]
    fn superci_is_stopped_from_the_dashboard_github_and_gitlab_let_go_of_and_its_control_plane_deleted_as_aws_answered() {
        let served = serve(|_| {});
        served.connect_github();
        served.connect_gitlab();
        let home = common::signed_in(&served);
        let (mut child, link) = common::dashboard(&home);
        // A GitLab project sending its jobs here.
        let project = superci_testkit::gitlab::PROJECT.to_string();
        let (status, _, _) = common::post(&link, "/gitlab/project", &[("gitlab", ""), ("id", &project), ("enabled", "true")]);
        assert!(status < 400);
        // Not typed as asked: nothing is done.
        let (status, _, said) = common::post(&link, "/plane/leave", &[("confirm", "yes")]);
        assert!(status == 400 && common::words(&said).contains("Type superci to confirm"), "{status} {}", common::words(&said));
        assert!(!served.github.uninstalled());
        // Stopped: GitLab's webhook and GitHub's App let go of, then the control plane deleted (AWS answering as a real
        // account answered a control plane's deletion, this one's id in it).
        served.aws.replaying(include_str!("../fixtures/aws/planes/delete.jsonl"), &[("testplane0002", superci_testkit::PLANE), ("http://new-plane.test", &served.url)]);
        let (status, _, said) = common::post(&link, "/plane/leave", &[("confirm", "superci")]);
        let said = common::words(&said);
        assert!(status == 200 && said.contains("SuperCI is stopped") && said.contains("Delete the GitHub App superci-acme on GitHub") && said.contains("Change runs-on: superci back"), "{status} {said}");
        assert!(served.github.uninstalled(), "its App uninstalled");
        assert!(served.gitlab.asked.lock().unwrap().iter().any(|a| a.starts_with(&format!("DELETE https://gitlab.com/api/v4/projects/{project}/hooks/"))), "{:?}", served.gitlab.asked.lock().unwrap());
        assert!(served.aws.settings(superci_testkit::PLANE).is_empty(), "{:?}", served.aws.settings(superci_testkit::PLANE));
        let asked: Vec<String> = served.aws.asked.lock().unwrap().iter().map(|a| format!("{} {}", a["method"].as_str().unwrap(), a["url"].as_str().unwrap())).collect();
        assert!(asked.iter().any(|a| a == &format!("DELETE https://lambda.us-east-1.amazonaws.com/2015-03-31/functions/superci-plane-{}", superci_testkit::PLANE)), "{asked:?}");
        assert!(served.aws.unanswered.lock().unwrap().is_empty(), "{:?}", served.aws.unanswered.lock().unwrap());
        let _ = child.kill();
        let _ = std::fs::remove_dir_all(&home);
        super::seen_in_browser(&served);
    }

    #[test]
    fn awss_runners_are_removed_and_added_again_from_the_dashboard_their_role_and_networks_deleted_as_aws_answered() {
        // A control plane in AWS whose own account runs jobs, with Cloudflare's containers beside (the test kit's).
        let served = serve(|c| {
            (c.aws_own, c.aws_own_creds) = (Some(("111111111111".into(), "us-east-1".into())), Some(superci_core::aws::Credentials { access_key_id: "ASIAX".into(), secret_access_key: "secret".into(), session_token: Some("tok".into()), expires_at_ms: u64::MAX }));
        });
        let home = common::signed_in(&served);
        let (mut child, link) = common::dashboard(&home);
        let setting = |n: &str| served.aws.settings(superci_testkit::PLANE).into_iter().find(|(k, _)| k == n).map(|(_, v)| v);
        let page = || common::words(&common::part(&link, "/?p=runners&fragment=1").1);
        // Refused: a provider that is not one; a dialog shown for another control plane.
        let (status, _, said) = common::post(&link, "/runners/remove", &[("cloud", "gcp")]);
        assert!(status == 400 && common::words(&said).contains("Unknown provider"), "{status}");
        let (status, _, said) = common::post(&link, "/runners/remove", &[("cloud", "aws"), ("plane", "anotherplane1")]);
        assert!(status == 409 && common::words(&said).contains("Another control plane is in use now"), "{status}");
        // Removed from SuperCI only: forgotten here, what is left in AWS said.
        let (status, _, _) = common::post(&link, "/runners/remove", &[("cloud", "aws"), ("only", "on")]);
        assert_eq!(status, 303);
        assert_eq!(setting("AWS_RUNNERS").as_deref(), Some("off"));
        assert!(page().contains("Removed AWS. Left: in AWS account 111111111111: the role superci-plane-testplane0001's machines policy"), "{}", page());
        // Added again: its role given the machines' policy again (as AWS answered a real deploy), and on.
        served.aws.replaying(include_str!("../fixtures/aws/planes/deploy.jsonl"), &[("testplane0002", superci_testkit::PLANE), ("http://new-plane.test", &served.url)]);
        let (status, to, said) = common::post(&link, "/runners/aws-own", &[]);
        assert!(status == 303 && to.contains("p=runners"), "{status} {}", common::words(&said));
        assert_eq!(setting("AWS_RUNNERS").as_deref(), Some("on"));
        // Its role given the permissions it lacks (a newer version's), and told so; a control plane not known here refused.
        let puts = || served.aws.asked.lock().unwrap().iter().filter(|a| a["asked"].as_str().unwrap_or_default().starts_with("Action=PutRolePolicy")).count();
        let before = puts();
        let (status, to, _) = common::post(&link, "/permissions/aws", &[("plane", superci_testkit::PLANE)]);
        assert!(status == 303 && to.contains("#permissions") && puts() > before, "{status} {to}");
        let (status, _, said) = common::post(&link, "/permissions/aws", &[("plane", "nowhere00000")]);
        assert!(status >= 400 && common::words(&said).contains("Which control plane?"), "{status}");
        // Removed with its part in AWS: the machines' policy, then their network (as AWS answered a real deletion).
        served.aws.replaying(include_str!("../fixtures/aws/planes/delete.jsonl"), &[("testplane0002", superci_testkit::PLANE), ("http://new-plane.test", &served.url)]);
        let (status, _, _) = common::post(&link, "/runners/remove", &[("cloud", "aws")]);
        assert_eq!(status, 303);
        let asked: Vec<String> = served.aws.asked.lock().unwrap().iter().map(|a| a["asked"].as_str().unwrap_or_default().chars().take(60).collect()).collect();
        for done in ["Action=PutRolePolicy", "Action=DeleteRolePolicy", "Action=DeleteVpc&"] { assert!(asked.iter().any(|a| a.starts_with(done)), "{done}: {asked:?}") }
        assert!(page().contains("Removed AWS.") && !page().contains("Left:"), "{}", page());
        // Cloudflare's containers, its own: switched off.
        let (status, _, _) = common::post(&link, "/runners/remove", &[("cloud", "cloudflare")]);
        assert_eq!(status, 303);
        assert_eq!(setting("CONTAINERS").as_deref(), Some("off"));
        assert!(served.aws.unanswered.lock().unwrap().is_empty(), "{:?}", served.aws.unanswered.lock().unwrap());
        let _ = child.kill();
        let _ = std::fs::remove_dir_all(&home);
        super::seen_in_browser(&served);
    }

    #[test]
    fn an_aws_accounts_look_is_said_by_the_cli_and_on_add_runners_as_aws_answered_each_kind_of_account() {
        // Each kind as a real account answered (`fixtures/aws/…/look.jsonl`): what the CLI says, and what Add runners shows.
        let cases = [
            ("classic/look", include_str!("../fixtures/aws/classic/look.jsonl"), "Kind          open: nothing SuperCI does is refused",
                &["Nothing SuperCI does there is refused", "64 on-demand · 96 spot", "Not in Canada, Stockholm, which this account may also use", "Copy into Canada"][..]),
            ("project-activated/look", include_str!("../fixtures/aws/project-activated/look.jsonl"), "Kind          an AWS project (AWS's new sign-up): it works in eu-north-1 only, with spot machines",
                &["A project of AWS's new sign-up: it works in Stockholm only", "The other 11 are refused. By its AWS organization (policy p-example1)", "16 on-demand · 32 spot", "Runner image In Stockholm", "Managed from account 222222222222"]),
            ("project-paid/look", include_str!("../fixtures/aws/project-paid/look.jsonl"), "Kind          an AWS project (AWS's new sign-up): it works in eu-north-1 only, and spot machines are refused",
                &["AWS keeps it to Stockholm and allows it no spot machines, so jobs run on on-demand machines", "Spot machines Not allowed By its AWS organization (policy p-example2)", "5 on-demand · 5 spot",
                    "Not in Stockholm No job can start there until it is copied in", "Managed from account 222222222222"]),
        ];
        for (name, recording, said, shown) in cases {
            let served = serve(|_| {});
            let home = common::signed_in(&served);
            served.aws.replaying(recording, &[]);
            let o = superci(&home, &["accounts", "retrieve", "aws"]);
            let text = String::from_utf8_lossy(&o.stdout);
            assert!(o.status.success() && text.contains(said), "{name}: {text}{}", String::from_utf8_lossy(&o.stderr));
            // The image jobs run on, by region: published there, or to copy in.
            let (code, images) = json(&home, &["images", "list"]);
            assert!(code == 0 && images["images"].as_array().is_some_and(|i| !i.is_empty()), "{name}: {images}");
            let (mut child, link) = common::dashboard(&home);
            // (Looked at in the background once signed in: shown when it is in.)
            let page = (0..40).map(|_| { std::thread::sleep(std::time::Duration::from_millis(250)); common::words(&common::part(&link, "/?p=add-runners&fragment=1").1) }).find(|p| p.contains("AWS account aws 111111111111") && !p.contains("Looking")).unwrap_or_default();
            for want in shown { assert!(page.contains(want), "{name}: no {want:?} in {page}") }
            // Looked at again when asked (on the page's next view).
            let before = served.aws.asked.lock().unwrap().len();
            let (status, _, _) = common::post(&link, "/aws/look", &[]);
            assert!(status < 400, "{name}: {status}");
            assert!((0..40).any(|_| { common::part(&link, "/?p=add-runners&fragment=1"); std::thread::sleep(std::time::Duration::from_millis(100)); served.aws.asked.lock().unwrap().len() > before }), "{name}: looked again");
            let _ = child.kill();
            let _ = std::fs::remove_dir_all(&home);
        }
    }

    #[test]
    fn a_control_plane_is_deployed_to_aws_from_the_dashboard_as_aws_answered_a_real_account() {
        // The workspace's control plane, and the new one the deploy brings up (its address, as AWS gave it).
        let served = serve(|_| {});
        let new = superci_testkit::serve_as("deployplane1", |_| {});
        let home = common::signed_in(&served);
        served.aws.replaying(include_str!("../fixtures/aws/planes/deploy.jsonl"), &[("testplane0002", "deployplane1"), ("http://new-plane.test", &new.url)]);
        let (mut child, link) = common::dashboard(&home);
        // A region not in the list is refused before anything is made.
        let (status, _, said) = common::post(&link, "/plane/aws", &[("region", "mars-1")]);
        assert!(status == 400 && common::words(&said).contains("Choose a region from the list"), "{status}");
        // Deployed (the same id as the recording's, as a try after a stop reuses its first's).
        let (status, _, _) = common::post(&link, "/plane/aws", &[("region", "us-east-1"), ("plane", "deployplane1")]);
        assert_eq!(status, 303);
        let listed = (0..120).find_map(|_| {
            std::thread::sleep(std::time::Duration::from_millis(250));
            let (_, page) = common::part(&link, "/?p=planes&fragment=1");
            let w = common::words(&page);
            (w.contains(&new.url) && w.contains("Move here")).then_some(w)
        });
        let (_, plane_page) = common::part(&link, "/?p=plane&fragment=1");
        assert!(listed.is_some(), "{}", common::words(&plane_page));
        let asked: Vec<String> = served.aws.asked.lock().unwrap().iter().map(|a| format!("{} {}", a["method"].as_str().unwrap(), a["url"].as_str().unwrap())).collect();
        assert!(asked.iter().any(|a| a == "POST https://lambda.us-east-1.amazonaws.com/2015-03-31/functions"), "{asked:?}");
        assert!(asked.iter().any(|a| a == "GET https://lambda.us-east-1.amazonaws.com/2015-03-31/functions/superci-plane-deployplane1-convert/configuration"), "the layer store's converter made beside it: {asked:?}");
        assert!(served.aws.unanswered.lock().unwrap().is_empty(), "{:?}", served.aws.unanswered.lock().unwrap());
        // Its settings written for it (the key the dashboard reads it with among them).
        let settings = served.aws.settings("deployplane1");
        assert!(settings.iter().any(|(k, _)| k.starts_with("DEVICE_KEY_")), "{:?}", settings.iter().map(|(k, _)| k).collect::<Vec<_>>());
        let _ = child.kill();
        let _ = std::fs::remove_dir_all(&home);
        super::seen_in_browser(&served);
    }

    #[test]
    fn everything_moves_to_another_control_plane_from_the_dashboard_and_the_old_one_says_where() {
        // The one in use, with GitHub's App and a job it ran; another in the same account (as a deploy leaves one),
        // reading its settings where the dashboard writes them.
        let served = serve(|_| {});
        served.connect_github();
        let (id, queued) = served.github.queue("test", "superci", served.now_ms());
        assert_eq!(served.webhook(queued), 202);
        assert_eq!(served.webhook(served.github.start(id)), 202);
        assert_eq!(served.webhook(served.github.finish(id, "success", "ok", served.now_ms())), 202);
        let aws = served.aws.clone();
        let new = superci_testkit::serve_as("testplane0002", move |c| c.read_settings(&aws.settings("testplane0002")));
        for (k, v) in new.aws.settings("testplane0002") { served.aws.put(&format!("/superci/testplane0002/{k}"), &v) }
        // (Its runners stay with it: nothing to bring along.)
        served.aws.put(&format!("/superci/{}/CONTAINERS", superci_testkit::PLANE), "off");
        let home = common::signed_in(&served);
        served.aws.replaying(include_str!("../fixtures/aws/planes/delete.jsonl"), &[("http://new-plane.test", &new.url)]);
        let (mut child, link) = common::dashboard(&home);
        // (Found in the account, on Control planes.)
        assert!((0..60).any(|_| { std::thread::sleep(std::time::Duration::from_millis(250)); common::words(&common::part(&link, "/?p=planes&fragment=1").1).contains(&new.url) }), "the other one found");
        let (status, _, said) = common::post(&link, "/plane/move", &[("plane", "nowhere00000")]);
        assert!(status >= 400 && common::words(&said).contains("That control plane is not known here"), "{status}");
        let (status, _, said) = common::post(&link, "/plane/move", &[("plane", "testplane0002")]);
        assert_eq!(status, 303, "{}", common::words(&said));
        // The old one passes on what still reaches it; the new one, in use, has GitHub's App and the history.
        let moved = (0..120).any(|_| { std::thread::sleep(std::time::Duration::from_millis(250)); served.store.under("moved_to").len() == 1 && new.store.under("standby").is_empty() && !new.store.under("claimed_at").is_empty() });
        let (_, page) = common::part(&link, "/?p=planes&fragment=1");
        assert!(moved, "{}", common::words(&page));
        assert!(served.aws.settings("testplane0002").iter().any(|(k, _)| k == "GITHUB_APP"));
        assert!(!new.store.under(&format!("job:{id}")).is_empty(), "the job it ran came along");
        let health: serde_json::Value = ureq::get(&format!("{}/health", served.url)).call().unwrap().body_mut().read_json().unwrap();
        assert_eq!(health["moved_to"].as_str(), Some(new.url.as_str()), "{health}");
        // A job GitHub still sends to the old one: passed on to the new one, signed as it came; an event signed by no
        // App is not passed on.
        let (later, queued) = served.github.queue("after the move", "superci", served.now_ms());
        let status = served.webhook(queued);
        assert!(status < 300, "{status} {:?}", served.github.redelivered());
        assert!((0..60).any(|_| { std::thread::sleep(std::time::Duration::from_millis(100)); !new.store.under(&format!("job:{later}")).is_empty() }), "passed on to the new one");
        let (_, (body, _)) = served.github.queue("forged", "superci", served.now_ms());
        assert_eq!(served.webhook((body, format!("sha256={}", "0".repeat(64)))), 401);
        let _ = child.kill();
        let _ = std::fs::remove_dir_all(&home);
        super::seen_in_browser(&served);
    }

    #[test]
    fn the_first_control_plane_deployed_in_a_named_workspace_is_called_what_the_workspace_is() {
        // An account with no control plane yet (AWS answering as it did a real deploy), and a workspace named AcmeCI
        // signed in to it.
        let account = superci_testkit::Aws::serve();
        let new = superci_testkit::serve_as("deployplane1", |_| {});
        account.replaying(include_str!("../fixtures/aws/planes/deploy.jsonl"), &[("testplane0002", "deployplane1"), ("http://new-plane.test", &new.url)]);
        let home = common::temp("named-home");
        let ws = home.join("workspaces/default");
        std::fs::create_dir_all(&ws).unwrap();
        std::fs::write(ws.join("sign-ins.json"), serde_json::json!({ "aws": { "region": "us-east-1", "account_id": "111111111111", "who": "arn:aws:sts::111111111111:assumed-role/Admin/someone",
            "creds": { "access_key_id": "ASIAEXAMPLE", "secret_access_key": "example", "session_token": "example", "expires_at_ms": 4102444800000u64 }, "refresh_token": "", "dpop": { "private_b64": "", "kid": "" } } }).to_string()).unwrap();
        std::fs::write(ws.join("workspace.json"), serde_json::json!({ "name": "AcmeCI" }).to_string()).unwrap();
        std::fs::write(home.join("aws-at"), &account.url).unwrap();
        let (_child, link) = common::dashboard(&home);
        let (status, to, _) = common::post(&link, "/plane/aws", &[("region", "us-east-1"), ("plane", "deployplane1")]);
        assert!(status == 303 && to.contains("p=plane"), "{status} {to}");
        // (Taken in when a page is shown next.)
        let named = (0..120).any(|_| { std::thread::sleep(std::time::Duration::from_millis(250)); common::part(&link, "/?p=plane&fragment=1"); account.settings("deployplane1").iter().any(|(k, v)| k == "NAME" && v == "AcmeCI") });
        assert!(named, "{:?}", account.settings("deployplane1").iter().map(|(k, _)| k).collect::<Vec<_>>());
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn another_ci_in_the_same_account_is_listed_apart_and_a_workspace_of_its_own_picks_it() {
        // Two control planes in the account, each serving GitHub (two CIs, side by side): the workspace looks after its own.
        let served = serve(|_| {});
        served.connect_github();
        // (The other reads its settings in the account, where the dashboard writes them.)
        let aws = served.aws.clone();
        let other = superci_testkit::serve_as("testplane0002", move |c| c.read_settings(&aws.settings("testplane0002")));
        for (k, v) in other.aws.settings("testplane0002") { served.aws.put(&format!("/superci/testplane0002/{k}"), &v) }
        served.aws.put("/superci/testplane0002/GITHUB_APP", &superci_testkit::GitHub::app().to_string());
        served.aws.put("/superci/testplane0002/NAME", "GlobexCI");
        served.aws.put("/superci/testplane0002/OWNERS", "github:globex");
        let home = common::signed_in(&served);
        served.aws.replaying(include_str!("../fixtures/aws/planes/delete.jsonl"), &[("http://new-plane.test", &other.url)]);
        let keys = || served.aws.settings("testplane0002").iter().filter(|(k, _)| k.starts_with("DEVICE_KEY_")).count();
        let keys_before = keys();
        // Commands are for this workspace's CI, with nothing named; the other one is not this workspace's to name.
        let (code, v) = json(&home, &["planes", "allow", "--dry-run"]);
        assert!(code == 0 && v["would"].as_str().unwrap_or_default().contains(superci_testkit::PLANE), "{v}");
        let (code, v) = json(&home, &["planes", "allow", "--dry-run", "--plane=testplane0002"]);
        assert!(code != 0, "{v}");
        assert_eq!(json(&home, &["jobs", "list"]).0, 0);
        // Listed apart, with its name and whose jobs it runs (read from the account, before anything of this computer's
        // is in it); its settings are not touched.
        let (code, planes) = json(&home, &["planes", "list"]);
        assert!(code == 0 && planes["control_planes"].as_array().unwrap().len() == 1 && planes["control_planes"][0]["in_use"] == true, "{planes}");
        assert_eq!((planes["found"][0]["id"].as_str(), planes["found"][0]["name"].as_str(), planes["found"][0]["owners"].as_str()), (Some("testplane0002"), Some("GlobexCI"), Some("github:globex")), "{planes}");
        let o = superci(&home, &["planes", "list"]);
        let said = String::from_utf8_lossy(&o.stdout).to_string();
        assert!(said.contains("Other CIs in your signed-in clouds, not this workspace's:") && said.contains("testplane0002") && said.contains("GlobexCI"), "{said}");
        assert_eq!(keys(), keys_before, "no key of this computer's in another CI");
        // A workspace of its own: made, signed in to the same account, and it picks that CI.
        assert!(superci(&home, &["workspaces", "create", "Globex"]).status.success());
        let mine = std::fs::read_to_string(home.join("workspaces/default/sign-ins.json")).unwrap();
        let mut theirs: serde_json::Value = serde_json::from_str(&mine).unwrap();
        for k in ["plane", "planes_more", "key", "cloudflare", "modal"] { theirs.as_object_mut().unwrap().remove(k); }
        std::fs::write(home.join("workspaces/globex/sign-ins.json"), theirs.to_string()).unwrap();
        let (code, v) = json(&home, &["planes", "list", "--workspace=globex"]);
        assert!(code == 0 && v["control_planes"].as_array().unwrap().is_empty() && v["found"].as_array().unwrap().len() == 2, "found, none taken in: {v}");
        let (code, v) = json(&home, &["workspaces", "update", "globex", "--plane=testplane0002"]);
        assert!(code == 0 && v["plane"] == "testplane0002", "{v}");
        let (_, v) = json(&home, &["planes", "list", "--workspace=globex"]);
        assert_eq!((v["control_planes"][0]["id"].as_str(), v["found"][0]["id"].as_str()), (Some("testplane0002"), Some(superci_testkit::PLANE)), "{v}");
        assert_eq!(keys(), keys_before + 1, "its key written in the CI it picked");
        // One that looks after a CI already takes no second one.
        let (code, v) = json(&home, &["workspaces", "update", "default", "--plane=testplane0002"]);
        assert!(code != 0 && v["error"].as_str().unwrap_or_default().contains("looks after a CI already"), "{v}");
        // The dashboard: the other CI is offered as the workspace that looks after it.
        let (_child, link) = common::dashboard(&home);
        assert!((0..40).any(|_| { std::thread::sleep(std::time::Duration::from_millis(250)); let w = common::words(&common::part(&link, "/?p=planes&fragment=1").1); w.contains("Other CIs in your signed-in accounts") && w.contains("GlobexCI") && w.contains("Open Globex") }));
        // Each CI in the account has a name of its own: this one is not renamed to the other's (whatever the case).
        let (status, _, said) = common::post(&link, "/name?w=default", &[("name", "globexci")]);
        assert!(status == 409 && common::words(&said).contains("testplane0002 in the same AWS account is called globexci"), "{status} {}", common::words(&said));
        assert!(!served.aws.settings(superci_testkit::PLANE).iter().any(|(k, v)| k == "NAME" && v == "globexci"));
        // The duplicate is said in the list of workspaces.
        let o = superci(&home, &["workspaces", "list"]);
        assert!(String::from_utf8_lossy(&o.stdout).lines().all(|l| !l.contains("same CI as")), "none yet: {}", String::from_utf8_lossy(&o.stdout));
        let _ = std::fs::remove_dir_all(&home);
        super::seen_in_browser(&served);
    }
}

/// Settings: a secret added in its dialog (and what it refuses), taken out again; another device's key and an
/// agent's read key taken out.
#[test]
fn secrets_are_added_and_taken_out_and_keys_removed_from_settings() {
    let Some(mut b) = Browser::open(1300, 1000) else { eprintln!("no Chrome here: skipped"); return };
    let served = serve(|_| {});
    // Another device that looks after this CI, beside this one.
    served.aws.put(&format!("/superci/{PLANE}/DEVICE_KEY_4102444800_LAPTOP"), "another-devices-key-for-tests-0001");
    let home = signed_in(&served);
    let (mut child, link) = dashboard(&home);
    let settings = |b: &mut Browser| { b.goto(&format!("{link}&p=settings")); b.wait(&format!("{} && !document.querySelector('main').innerText.includes('Reading your control plane')", shows("Secrets"))); };
    settings(&mut b);
    // Added in its dialog, for one repository: listed by name and where it may be used, never by its value.
    b.eval("document.querySelector('button[onclick*=add-secret]').click()");
    b.wait("document.getElementById('add-secret').open");
    b.eval("(() => { const d = document.getElementById('add-secret'); d.querySelector('[name=name]').value = 'NPM_TOKEN'; d.querySelector('[name=value]').value = 'npm-secret-value-for-tests'; d.querySelector('details').open = true; d.querySelector('[name=repo]').value = 'acme/app'; d.querySelector('button.primary').click(); })()");
    b.wait(&format!("{} && {}", shows("NPM_TOKEN"), shows("acme/app")));
    assert!(!b.eval("document.body.innerText").as_str().unwrap_or_default().contains("npm-secret-value-for-tests"));
    assert!(served.aws.settings(PLANE).iter().any(|(k, v)| k.starts_with("SECRET_") && v.contains("npm-secret-value-for-tests")));
    // Refused: the same again, a name no secret may have, a scope that is none; nothing changes.
    b.goto(&format!("{}/job/view?id=0", link.split("/?").next().unwrap()));
    for (fields, said) in [(&[("name", "NPM_TOKEN"), ("value", "x"), ("repo", "acme/app")][..], "is kept for"), (&[("name", "1BAD"), ("value", "x")], "That secret was not added"),
        (&[("name", "OK_NAME"), ("value", "x"), ("repo", "not a repository")], "That cannot be its scope"), (&[("name", "GONE"), ("repo", "acme/app")], "That secret was not taken out")] {
        let path = if fields.iter().any(|(k, _)| *k == "value") { "/secrets/add" } else { "/secrets/remove" };
        let (got, text) = post(&mut b, path, fields);
        assert!(got >= 400 && text.contains(said), "{fields:?}: {got} {text}");
    }
    assert_eq!(served.aws.settings(PLANE).iter().filter(|(k, _)| k.starts_with("SECRET_")).count(), 1);
    // Taken out by its button.
    settings(&mut b);
    b.eval("window.confirm = () => true");
    b.eval("[...document.querySelectorAll('form[action=\"/secrets/remove\"] button')][0].click()");
    until("the secret taken out", || !served.aws.settings(PLANE).iter().any(|(k, _)| k.starts_with("SECRET_")));
    settings(&mut b);
    // The other device's key, and the agent's read key, removed.
    b.eval("window.confirm = () => true");
    b.eval("[...document.querySelectorAll('form[action=\"/keys/remove\"]')].find(f => f.querySelector('[name=name]') && f.querySelector('[name=name]').value.includes('LAPTOP')).querySelector('button').click()");
    until("the other device's key removed", || !served.aws.settings(PLANE).iter().any(|(k, _)| k.contains("LAPTOP")));
    settings(&mut b);
    let reads = || served.aws.settings(PLANE).iter().filter(|(k, _)| k.starts_with("READ_KEY_")).count();
    let before = reads();
    b.eval("[...document.querySelectorAll('form[action=\"/keys/remove\"]')].find(f => f.querySelector('[name=read]')).querySelector('button').click()");
    until("the read key removed", || reads() < before);
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

/// Signing out with AWS, Cloudflare and Modal signed in, from Settings: each said, the workspace's sign-ins gone,
/// and the first screen of a new SuperCI after.
#[test]
fn signing_out_with_every_cloud_signed_in_says_what_happened_to_each() {
    let Some(mut b) = Browser::open(1300, 1000) else { eprintln!("no Chrome here: skipped"); return };
    let served = serve(|_| {});
    let home = signed_in(&served);
    let file = home.join("workspaces/default/sign-ins.json");
    let mut kept: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    kept["cloudflare"] = serde_json::json!({ "access": "cloudflare-access-for-tests", "expires_at_ms": 4102444800000u64, "refresh": "cloudflare-refresh-for-tests" });
    kept["modal"] = serde_json::json!({ "token_id": "ak-modal-for-tests", "token_secret": "as-modal-for-tests", "workspace": "acmeorg" });
    std::fs::write(&file, kept.to_string()).unwrap();
    let (mut child, link) = dashboard(&home);
    b.goto(&format!("{link}&p=settings"));
    b.wait(&shows("Sign out"));
    // This computer's key was in the control plane; signing out takes it out there too.
    let keys = || served.aws.settings(PLANE).into_iter().filter(|(k, _)| k.starts_with("DEVICE_KEY_")).count();
    let before = keys();
    b.eval("window.confirm = () => true");
    b.eval("[...document.querySelectorAll('form[action=\"/signout\"] button')][0].click()");
    b.wait(&page_shows("Signed out."));
    until("its sign-ins removed", || !file.exists());
    assert!(keys() <= before, "{before} → {}", keys());
    // Afterwards: the first screen, nothing of the workspace's.
    b.goto(&link);
    b.wait("document.querySelector('main') && !document.querySelector('main').innerText.includes('Reading your control plane')");
    assert!(!b.eval("document.querySelector('main').innerText").as_str().unwrap_or_default().contains("AWS · 111111111111"));
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

/// The Runners page's settings, sent as its forms send them: limits a control plane too old for them refuses, an
/// order that would leave none and one with a provider it did not list, the last provider kept in, public
/// repositories chosen, an account's limit without its sign-in, a region added with its network as AWS answered.
#[test]
fn the_runners_settings_refuse_what_cannot_be_and_add_a_region_with_its_network() {
    let Some(mut b) = Browser::open(1300, 1000) else { eprintln!("no Chrome here: skipped"); return };
    let own = |c: &mut superci_core::plane::Config| {
        (c.aws_own, c.aws_own_creds) = (Some(("111111111111".into(), "us-east-1".into())), Some(superci_core::aws::Credentials { access_key_id: "ASIAX".into(), secret_access_key: "secret".into(), session_token: Some("tok".into()), expires_at_ms: u64::MAX }));
    };
    let routing = |served: &superci_testkit::Served| served.aws.settings(PLANE).into_iter().find(|(k, _)| k == "ROUTING").and_then(|(_, v)| serde_json::from_str::<serde_json::Value>(&v).ok()).unwrap_or_default();
    // Too old for a limit for the whole CI (before 0.15.0), then for a day's limit and AWS's budget (before 0.16.0).
    for (version, fields, said) in [("0.14.0", &[("action", "monthly_usd"), ("usd", "100")][..], "Update the control plane first"),
        ("0.15.0", &[("action", "monthly_usd"), ("usd", "100"), ("daily", "10")], "A limit for a day"), ("0.15.0", &[("action", "account_usd"), ("account", "aws"), ("usd", "40"), ("budget", "emails")], "A limit for a day")] {
        let served = serve(own);
        *served.aws.old_version.lock().unwrap() = Some(version.into());
        let home = signed_in(&served);
        let (mut child, link) = dashboard(&home);
        b.goto(&link);
        b.goto(&link);
    b.goto(&format!("{}/job/view?id=0", link.split("/?").next().unwrap()));
        let (got, text) = post(&mut b, "/routing", fields);
        assert!(got == 409 && text.contains(said), "{version} {fields:?}: {got} {text}");
        assert!(routing(&served).get("monthly_usd").is_none());
        let _ = child.kill();
        let _ = std::fs::remove_dir_all(&home);
    }
    let served = serve(own);
    let home = signed_in(&served);
    let (mut child, link) = dashboard(&home);
    b.goto(&link);
    b.goto(&format!("{}/job/view?id=0", link.split("/?").next().unwrap()));
    // An order with every provider taken out, and the last one taken out: refused.
    let (got, text) = post(&mut b, "/routing", &[("action", "order"), ("out_0", "aws"), ("out_1", "aws-on-demand"), ("out_2", "cloudflare")]);
    assert!(got == 400 && text.contains("Keep one provider in the order"), "{got} {text}");
    for cloud in ["aws", "aws-on-demand"] { let (got, text) = post(&mut b, "/routing", &[("action", "take_out"), ("cloud", cloud), ("current", "aws,aws-on-demand,cloudflare")]); assert!(got < 400, "{cloud}: {got} {text}") }
    let (got, text) = post(&mut b, "/routing", &[("action", "take_out"), ("cloud", "cloudflare"), ("current", "cloudflare")]);
    assert!(got == 400 && text.contains("Keep one provider in the order"), "{got} {text}");
    // An order naming one provider: the others stay, after it.
    let (got, text) = post(&mut b, "/routing", &[("action", "order"), ("cloud_0", "cloudflare")]);
    assert!(got < 400, "{got} {text}");
    let order: Vec<String> = routing(&served)["order"].as_array().unwrap().iter().map(|p| p["cloud"].as_str().unwrap().to_string()).collect();
    assert_eq!(order[0], "cloudflare", "{order:?}");
    // Public repositories chosen: only owner/name; patterns kept.
    let (got, text) = post(&mut b, "/routing", &[("action", "public_set"), ("repo", "not-a-repository")]);
    assert!(got == 400 && text.contains("A repository is owner/name"), "{got} {text}");
    let (got, text) = post(&mut b, "/routing", &[("action", "public_set"), ("repo", "acme/site"), ("repo", "acme/site")]);
    assert!(got < 400, "{got} {text}");
    assert_eq!(routing(&served)["public_repos"], serde_json::json!(["acme/site"]));
    // A region added to AWS's spot machines: its network made first in the account signed in to, as AWS answered.
    served.aws.replaying(include_str!("../fixtures/aws/regions/add.jsonl"), &[("mixplane0001", PLANE)]);
    let (got, text) = post(&mut b, "/routing", &[("action", "pool"), ("cloud", "aws"), ("region_0", "us-east-1"), ("region_1", "eu-north-1")]);
    assert!(got < 400, "{got} {text}");
    until("the region kept", || served.aws.settings(PLANE).iter().any(|(k, v)| k == "AWS_REGIONS" && v.contains("eu-north-1")));
    assert!(served.aws.asked.lock().unwrap().iter().any(|a| a["url"].as_str().unwrap_or_default().contains("ec2.eu-north-1") && a["asked"].as_str().unwrap_or_default().contains("Action=CreateVpc")), "its network made");
    b.goto(&format!("{link}&p=runners"));
    b.wait(&format!("{} && {}", shows("Runner providers"), shows("Stockholm")));
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
    // Not signed in to the AWS account whose limit it is: refused, saying so.
    let served = serve(own);
    let home = common::home(&served);
    let (mut child, link) = dashboard(&home);
    b.goto(&link);
    b.goto(&format!("{}/job/view?id=0", link.split("/?").next().unwrap()));
    let (got, text) = post(&mut b, "/routing", &[("action", "account_usd"), ("account", "aws"), ("usd", "40"), ("budget", "emails"), ("email", "ops@example.com")]);
    assert!(got == 403 && text.contains("Sign in with AWS first"), "{got} {text}");
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

/// A control plane updated from its page, as AWS answered a real update: its steps, then what it says when done: its
/// settings under their new names, a launcher updated with it or said as not, a refusal with Try again.
#[test]
fn a_control_plane_is_updated_from_its_page_its_launchers_with_it_or_said_as_not() {
    let Some(mut b) = Browser::open(1300, 1000) else { eprintln!("no Chrome here: skipped"); return };
    // Each case: its launchers, signed in to Cloudflare and Modal, what AWS refuses, and what its page says when done.
    type Case = (&'static str, Option<&'static str>, bool, bool, &'static str);
    let cases: Vec<Case> = vec![
        ("from before the new names", None, false, false, "Updated to"),
        ("with launchers in Cloudflare and Modal, both signed in", Some(r#"[{"cloud":"cloudflare","url":"https://superci-runners-testplane0001.acme.workers.dev"},{"cloud":"modal","url":"https://acme--superci-runners-testplane0001.modal.run"}]"#), true, false, "Updated to"),
        ("with a launcher in Cloudflare, not signed in", Some(r#"[{"cloud":"cloudflare","url":"https://superci-runners-testplane0001.acme.workers.dev"}]"#), false, false, "Cloudflare is not signed in to here"),
        ("with a launcher in Cloudflare, its sign-in ended", Some(r#"[{"cloud":"cloudflare","url":"https://superci-runners-testplane0001.acme.workers.dev"}]"#), false, false, "the Cloudflare sign-in has expired"),
        ("AWS refusing its new code", None, false, true, "Rate exceeded"),
    ];
    for (what, launchers, clouds, refuse, said) in cases {
        let served = serve(|_| {});
        let home = signed_in(&served);
        if let Some(l) = launchers { served.aws.put(&format!("/superci/{PLANE}/AGENTS"), l); served.aws.put(&format!("/superci/{PLANE}/CONTAINERS"), "off"); }
        if clouds {
            let file = home.join("workspaces/default/sign-ins.json");
            let mut kept: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
            kept["cloudflare"] = serde_json::json!({ "access": "cloudflare-access-for-tests", "expires_at_ms": 4102444800000u64, "refresh": "cloudflare-refresh-for-tests" });
            kept["modal"] = serde_json::json!({ "token_id": "ak-modal-for-tests", "token_secret": "as-modal-for-tests", "workspace": "acmeorg" });
            std::fs::write(&file, kept.to_string()).unwrap();
            served.aws.replaying(include_str!("../fixtures/aws/planes/update-with-launchers.jsonl"), &[("mixplane0001", PLANE), ("http://new-plane.test", &served.url)]);
            let mut w = served.aws.workers.lock().unwrap();
            for host in [format!("superci-runners-{PLANE}.acme.workers.dev"), format!("acmeorg--superci-runners-{PLANE}.modal.run")] { w.insert(host, served.url.clone()); }
        } else {
            served.aws.replaying(include_str!("../fixtures/aws/planes/update.jsonl"), &[("testplane0002", PLANE), ("http://new-plane.test", &served.url)]);
        }
        if what.contains("sign-in ended") {
            let file = home.join("workspaces/default/sign-ins.json");
            let mut kept: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
            kept["cloudflare_ended"] = true.into();
            std::fs::write(&file, kept.to_string()).unwrap();
        }
        if refuse { served.aws.fail(&format!("functions/superci-plane-{PLANE}/code"), 429, &superci_testkit::json_error("TooManyRequestsException", "Rate exceeded"), None) }
        // A control plane from before 0.19.13, with its settings under their old names: the page offers the update.
        served.aws.put(&format!("/superci/{PLANE}/DASHBOARD_KEY_4102444800_TEST"), &served.key);
        *served.aws.old_version.lock().unwrap() = Some("0.19.12".into());
        let (mut child, link) = dashboard(&home);
        b.goto(&format!("{link}&p=planes"));
        b.wait(&format!("document.querySelector('form[action=\"/plane/update\"] button') && {}", shows("Control plane")));
        b.eval("document.querySelector('form[action=\"/plane/update\"] button').click()");
        b.wait(&format!("{} || {}", page_shows("Updating to"), page_shows(said)));
        let end = std::time::Instant::now() + std::time::Duration::from_secs(120);
        loop {
            b.goto(&format!("{link}&p=planes"));
            b.wait("document.querySelector('main') && !document.querySelector('main').innerText.includes('Reading your control plane')");
            let text = b.eval("document.body.innerText").as_str().unwrap_or_default().to_string();
            if text.contains(said) { break }
            assert!(std::time::Instant::now() < end, "{what}: no {said:?}:\n{text}");
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
        if refuse { assert!(b.eval("!!document.querySelector('form[action=\"/plane/update\"] button')").as_bool() == Some(true), "{what}: Try again offered") }
        else { assert!(served.aws.settings(PLANE).iter().any(|(k, _)| k == "LAUNCHERS") || launchers.is_none(), "{what}: its launchers under the new name") }
        let _ = child.kill();
        let _ = std::fs::remove_dir_all(&home);
    }
}

/// What the sweep does as time passes, each said on the job's page: a start cut off midway placed again (three times,
/// then failed), what an AWS machine sent to the internet added to its job's cost once CloudWatch has counted it (as
/// AWS documents its answer), and a machine whose job was cancelled before it began taken back.
#[test]
fn the_sweep_places_a_cut_off_start_again_settles_what_a_machine_sent_and_takes_back_a_cancelled_jobs_machine() {
    let Some(mut b) = Browser::open(1300, 1000) else { eprintln!("no Chrome here: skipped"); return };
    let served = serve(|c| {
        (c.aws_own, c.aws_own_creds) = (Some(("111111111111".into(), "us-east-1".into())), Some(superci_core::aws::Credentials { access_key_id: "ASIAX".into(), secret_access_key: "secret".into(), session_token: Some("tok".into()), expires_at_ms: u64::MAX }));
    });
    served.connect_github();
    served.aws.put(&format!("/superci/{PLANE}/ROUTING"), r#"{"default":null,"rules":[],"order":[{"cloud":"aws"},{"cloud":"aws-on-demand"},{"cloud":"cloudflare","off":true}]}"#);
    served.aws.replaying(include_str!("../fixtures/aws/launch/spot.jsonl"), &[]);
    let started: String = include_str!("../fixtures/aws/launch/on-demand.jsonl").lines().filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .find(|v| v["asked"].as_str().unwrap_or_default().contains("Action=RunInstances") && v["status"] == 200).map(|v| v["answer"].as_str().unwrap_or_default().to_string()).unwrap();
    served.store.0.lock().unwrap().insert("aws:no-fleet".into(), served.now_ms().to_string());
    served.aws.fail("RunInstances", 200, &started, None);
    // (What CloudWatch counted a machine sent: 2 GB, as its GetMetricStatistics answers.)
    served.aws.fail("GetMetricStatistics", 200, "<GetMetricStatisticsResponse xmlns=\"http://monitoring.amazonaws.com/doc/2010-08-01/\"><GetMetricStatisticsResult><Datapoints><member><Sum>1500000000</Sum><Unit>Bytes</Unit></member><member><Sum>500000000</Sum><Unit>Bytes</Unit></member></Datapoints><Label>NetworkOut</Label></GetMetricStatisticsResult><ResponseMetadata><RequestId>00000000-0000-4000-8000-000000000001</RequestId></ResponseMetadata></GetMetricStatisticsResponse>", None);
    let state = |id: u64| served.store.under(&format!("job:{id}"))[0].1.clone();
    let edit = |id: u64, f: &dyn Fn(&mut serde_json::Value)| { let mut m = served.store.0.lock().unwrap(); let k = format!("job:{id}"); let mut j: serde_json::Value = serde_json::from_str(&m[&k]).unwrap(); f(&mut j); m.insert(k, j.to_string()); };
    // One whose start was cut off midway (the runtime stopped while AWS was asked), and one cut off for the third time.
    let (cut, q) = served.github.queue("cut off", "superci", served.now_ms()); served.webhook(q);
    let (thrice, q) = served.github.queue("cut off thrice", "superci", served.now_ms()); served.webhook(q);
    let old = served.now_ms() - 5 * 60_000;
    edit(cut, &|j| { j["state"] = "launching".into(); j["launched_ms"] = old.into(); });
    edit(thrice, &|j| { j["state"] = "launching".into(); j["launched_ms"] = old.into(); j["retries"] = 2.into(); });
    // One that ran and ended on its AWS machine.
    let (ran, q) = served.github.queue("ran", "superci", served.now_ms()); served.webhook(q);
    served.webhook(served.github.start(ran));
    served.webhook(served.github.finish(ran, "success", "2026-10-15T10:00:01.0000000Z ok", served.now_ms()));
    // One cancelled before any runner took it, the note of that missed by its start.
    let (gone, q) = served.github.queue("gone", "superci", served.now_ms()); served.webhook(q);
    served.store.0.lock().unwrap().insert(format!("gone:{gone}"), served.now_ms().to_string());
    served.github.ended_untold(gone);
    served.upkeep();
    assert_eq!((state(cut)["state"].as_str(), state(cut)["retries"].as_u64()), (Some("launched"), Some(1)), "{}", state(cut));
    assert_eq!(state(thrice)["state"], "failed", "{}", state(thrice));
    // A quarter of an hour on: what its machine sent, counted.
    served.later(15 * 60_000);
    served.upkeep();
    let sent = state(ran)["sent_gb"].as_f64();
    assert!(sent.is_some_and(|g| (g - 2.0).abs() < 0.01), "{}", state(ran));
    served.aws.fail_none();
    let home = signed_in(&served);
    let (mut child, link) = dashboard(&home);
    for (id, said) in [(cut, "Status"), (thrice, "three times"), (ran, "Passed"), (gone, "Status")] {
        b.goto(&format!("{link}&p=job&id={id}"));
        b.wait(&format!("{} && !document.querySelector('main').innerText.includes('Reading your control plane')", shows("Status")));
        let page = b.eval("document.querySelector('main').innerText").as_str().unwrap_or_default().replace('\n', " ");
        assert!(page.contains(said), "job {id}: no {said:?} ({}):\n{page}", state(id));
    }
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

/// The sweep finding jobs in each state their events left them (some events lost): a failing runner GitHub still has
/// queued, or no longer; machines whose jobs were taken elsewhere, failed or ended, one GitHub has on its very runner,
/// one whose runner is busy with another job, one handed to another job still waiting; and notes past their time.
#[test]
fn the_sweep_finds_out_what_became_of_jobs_whose_events_were_lost_and_says_it_on_their_pages() {
    let Some(mut b) = Browser::open(1300, 1000) else { eprintln!("no Chrome here: skipped"); return };
    let served = serve(|c| {
        (c.aws_own, c.aws_own_creds) = (Some(("111111111111".into(), "us-east-1".into())), Some(superci_core::aws::Credentials { access_key_id: "ASIAX".into(), secret_access_key: "secret".into(), session_token: Some("tok".into()), expires_at_ms: u64::MAX }));
    });
    served.connect_github();
    served.aws.put(&format!("/superci/{PLANE}/ROUTING"), r#"{"default":null,"rules":[],"order":[{"cloud":"aws"},{"cloud":"aws-on-demand"},{"cloud":"cloudflare","off":true}]}"#);
    served.aws.replaying(include_str!("../fixtures/aws/launch/spot.jsonl"), &[]);
    let started: String = include_str!("../fixtures/aws/launch/on-demand.jsonl").lines().filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .find(|v| v["asked"].as_str().unwrap_or_default().contains("Action=RunInstances") && v["status"] == 200).map(|v| v["answer"].as_str().unwrap_or_default().to_string()).unwrap();
    served.store.0.lock().unwrap().insert("aws:no-fleet".into(), served.now_ms().to_string());
    served.aws.fail("RunInstances", 200, &started, None);
    let state = |id: u64| served.store.under(&format!("job:{id}"))[0].1.clone();
    let edit = |id: u64, f: &dyn Fn(&mut serde_json::Value)| { let mut m = served.store.0.lock().unwrap(); let k = format!("job:{id}"); let mut j: serde_json::Value = serde_json::from_str(&m[&k]).unwrap(); f(&mut j); m.insert(k, j.to_string()); };
    let labelled = |name: &str, label: &str| { let (id, q) = served.github.queue(name, label, served.now_ms()); assert!(served.webhook(q) < 300); id };
    let job = |name: &str| labelled(name, "superci");
    let now = served.now_ms();
    // A failing runner not started yet: GitHub still has its job queued (its failing runner asked for again); no
    // longer queued; tried for over half an hour.
    let (pending, no_longer, too_long) = (job("fail pending"), job("fail pending, ended"), job("fail pending, long ago"));
    for id in [pending, no_longer, too_long] { edit(id, &|j| { j["state"] = "failed".into(); j["fail_pending"] = "the workflow asks for no runner SuperCI has".into(); j["ended_ms"] = now.into(); j["began"] = true.into(); }) }
    edit(too_long, &|j| { j["ended_ms"] = (now - 31 * 60_000).into(); });
    served.github.ended_untold(no_longer);
    // Machines that came up and began nothing, nine minutes on: its job failed at once meanwhile, over, its own job
    // ended elsewhere untold.
    let (failed_fast, over, elsewhere) = (job("failed fast"), job("over"), job("elsewhere"));
    let old = now - 9 * 60_000;
    for id in [failed_fast, over, elsewhere] { edit(id, &|j| { j["launched_ms"] = old.into(); }) }
    edit(failed_fast, &|j| { j["failed_fast"] = true.into(); });
    edit(over, &|j| { j["over"] = true.into(); });
    served.github.ended_untold(elsewhere);
    // Two minutes on: one GitHub has on its very runner, the event never came; one whose job ended elsewhere and whose
    // runner is busy with another job; one whose job ended elsewhere while another job still waits with no machine
    // coming for it, and one whose job ended while the job it would go to has ended too.
    // (Those handed on with a label of their own: no other machine here is handed to them.)
    let handing = |name: &str| labelled(name, "superci/cpu=2");
    let (on_its_runner, busy, handed, waits, handed_late, waited) = (job("on its runner"), job("busy"), handing("handed on"), handing("still waits"), handing("handed late"), handing("waited"));
    let two = now - 2 * 60_000;
    for id in [on_its_runner, busy, handed, handed_late] { edit(id, &|j| { j["launched_ms"] = two.into(); }) }
    let runner = |id: u64| state(id)["runner"].as_str().unwrap().to_string();
    let _ = served.github.start_on(on_its_runner, &runner(on_its_runner));
    served.github.ended_untold(busy);
    let other = job("on the busy runner");
    let _ = served.github.start_on(other, &runner(busy));
    // (Its machine was handed to another: it waits with none coming, as far as the control plane knows.)
    for id in [waits, waited] { edit(id, &|j| { j["state"] = "done".into(); j["seen_in_progress"] = true.into(); }) }
    served.github.ended_untold(handed);
    served.github.ended_untold(waited);
    // Notes past their time, a launch template left behind a day ago, a machine kept for its images past its time.
    {
        let mut m = served.store.0.lock().unwrap();
        for k in ["spot:interrupted:1", "rerun:2", "rerun-asked:3", "gone:4"] { m.insert(k.into(), (now - 8 * 86_400_000).to_string()); }
        m.insert("aws:template:us-east-1:superci-fleet-1".into(), (now - 2 * 86_400_000).to_string());
        m.insert("aws:template:us-east-1:superci-fleet-2".into(), now.to_string());
        m.insert(format!("keeping:{over}"), (now - 1).to_string());
    }
    served.upkeep();
    let st = |id: u64| state(id)["state"].as_str().unwrap_or_default().to_string();
    assert!(state(pending)["failed_fast"] == true, "its failing runner asked for again: {}", state(pending));
    assert!(state(no_longer)["fail_pending"].is_null() && state(too_long)["fail_pending"].is_null(), "{} {}", state(no_longer), state(too_long));
    assert_eq!((st(failed_fast), st(over), st(elsewhere)), ("swept".into(), "done".into(), "swept".into()));
    assert_eq!(st(on_its_runner), "running", "{}", state(on_its_runner));
    assert_eq!(st(busy), "running", "{}", state(busy));
    assert_eq!(state(handed)["for_job"].as_u64(), Some(waits), "{}", state(handed));
    {
        let m = served.store.0.lock().unwrap();
        for k in ["spot:interrupted:1", "rerun:2", "rerun-asked:3", "gone:4", "aws:template:us-east-1:superci-fleet-1", &format!("keeping:{over}")] { assert!(!m.contains_key(k), "{k} kept") }
        assert!(m.contains_key("aws:template:us-east-1:superci-fleet-2"), "a template of a moment ago is left for its removal");
    }
    served.aws.fail_none();
    let home = signed_in(&served);
    let (mut child, link) = dashboard(&home);
    for id in [pending, no_longer, too_long, failed_fast, over, elsewhere, on_its_runner, busy, handed, handed_late] {
        b.goto(&format!("{link}&p=job&id={id}"));
        b.wait(&format!("{} && !document.querySelector('main').innerText.includes('Reading your control plane')", shows("Status")));
    }
    seen_in_browser(&served);
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

/// The sweep, more of what it finds: a start under way a moment ago, a job past its longest time (90 minutes) with a
/// reason said already, one running that never said so, a cancelled job's machine meant for another, its runner kept
/// by GitHub, GitHub failing to say whether a runner is busy, one whose own job began elsewhere, one handed on late,
/// a template AWS would not remove, runs and notes kept and gone, machines keeping images, and a move a moment ago.
#[test]
fn the_sweep_finds_more_states_of_jobs_runs_and_notes_and_says_them_on_their_pages() {
    let Some(mut b) = Browser::open(1300, 1000) else { eprintln!("no Chrome here: skipped"); return };
    let served = serve(|c| {
        (c.aws_own, c.aws_own_creds) = (Some(("111111111111".into(), "us-east-1".into())), Some(superci_core::aws::Credentials { access_key_id: "ASIAX".into(), secret_access_key: "secret".into(), session_token: Some("tok".into()), expires_at_ms: u64::MAX }));
    });
    served.connect_github();
    served.aws.put(&format!("/superci/{PLANE}/ROUTING"), r#"{"order":[{"cloud":"aws","max_jobs":100},{"cloud":"aws-on-demand"},{"cloud":"cloudflare","off":true}],"max_minutes":90}"#);
    served.aws.replaying(include_str!("../fixtures/aws/launch/spot.jsonl"), &[]);
    let started: String = include_str!("../fixtures/aws/launch/on-demand.jsonl").lines().filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .find(|v| v["asked"].as_str().unwrap_or_default().contains("Action=RunInstances") && v["status"] == 200).map(|v| v["answer"].as_str().unwrap_or_default().to_string()).unwrap();
    served.store.0.lock().unwrap().insert("aws:no-fleet".into(), served.now_ms().to_string());
    served.aws.fail("RunInstances", 200, &started, None);
    let state = |id: u64| served.store.under(&format!("job:{id}"))[0].1.clone();
    let edit = |id: u64, f: &dyn Fn(&mut serde_json::Value)| { let mut m = served.store.0.lock().unwrap(); let k = format!("job:{id}"); let mut j: serde_json::Value = serde_json::from_str(&m[&k]).unwrap(); f(&mut j); m.insert(k, j.to_string()); };
    let labelled = |name: &str, label: &str| { let (id, q) = served.github.queue(name, label, served.now_ms()); assert!(served.webhook(q) < 300); id };
    let job = |name: &str| labelled(name, "superci");
    let now = served.now_ms();
    let (just, past, never_said, meant, kept_by_github, unsure, begun) = (job("started a moment ago"), job("past its longest time"), job("running, never said"), job("meant for another"), job("its runner kept"), job("GitHub unsure"), job("begun elsewhere"));
    edit(just, &|j| { j["state"] = "launching".into(); j["launched_ms"] = (now - 30_000).into(); j["seen_in_progress"] = true.into(); });
    edit(past, &|j| { j["state"] = "running".into(); j["seen_in_progress"] = true.into(); j["launched_ms"] = (now - 100 * 60_000).into(); j["error"] = "said before".into(); });
    edit(never_said, &|j| { j["state"] = "running".into(); j["launched_ms"] = (now - 11 * 60_000).into(); });
    let other = job("the other");
    edit(meant, &|j| { j["for_job"] = other.into(); j["launched_ms"] = (now - 2 * 60_000).into(); });
    served.store.0.lock().unwrap().insert(format!("gone:{meant}"), now.to_string());
    served.store.0.lock().unwrap().insert(format!("gone:{kept_by_github}"), now.to_string());
    let runner_of = |id: u64| state(id)["runner_id"].as_u64().unwrap();
    served.github.fail("DELETE", &format!("/runners/{}", runner_of(kept_by_github)), 422, "Bad request - Runner is still running a job");
    served.github.ended_untold(unsure);
    edit(unsure, &|j| { j["launched_ms"] = (now - 2 * 60_000).into(); });
    served.github.fail("GET", &format!("/runners/{}", runner_of(unsure)), 502, "Server Error");
    served.github.ended_untold(begun);
    edit(begun, &|j| { j["launched_ms"] = (now - 9 * 60_000).into(); j["began"] = true.into(); });
    // Handed on late: its own job ended, another still waiting with no machine coming, nine minutes on.
    let handing = |name: &str| labelled(name, "superci/cpu=2");
    let (late, waiting_one) = (handing("handed late"), handing("waiting with none coming"));
    served.github.ended_untold(late);
    edit(late, &|j| { j["launched_ms"] = (now - 9 * 60_000).into(); });
    edit(waiting_one, &|j| { j["state"] = "done".into(); j["seen_in_progress"] = true.into(); });
    // Not waited for here: a failed one's failing runner pending, one to be run again.
    let (pending_fail, to_rerun) = (job("failing runner pending"), job("to run again"));
    edit(pending_fail, &|j| { j["fail_pending"] = "why".into(); j["state"] = "done".into(); j["ended_ms"] = now.into(); });
    edit(to_rerun, &|j| { j["rerun"] = serde_json::json!({ "job_id": 1, "run_id": 1, "repo": "acme/app", "name": "x", "state": "asked", "at_ms": now }); j["state"] = "done".into(); });
    // An ended AWS machine whose sending is counted already, and one ended days ago.
    let (counted, days) = (job("counted"), job("days ago"));
    edit(counted, &|j| { j["state"] = "done".into(); j["ended_ms"] = (now - 20 * 60_000).into(); j["cost_from"] = "list".into(); j["sent_gb"] = 1.0.into(); });
    edit(days, &|j| { j["state"] = "done".into(); j["ended_ms"] = (now - 3 * 86_400_000).into(); j["cost_from"] = "list".into(); });
    {
        let mut m = served.store.0.lock().unwrap();
        // A template AWS will not remove yet (an hour old): kept for the next sweep.
        m.insert("aws:template:us-east-1:superci-fleet-9".into(), (now - 3_600_000).to_string());
        // A run whose key is no number, one kept a day with a log in a bucket no longer there, a tree a moment ago.
        m.insert("run:abc".into(), serde_json::json!({ "jobs": [{ "state": "running" }] }).to_string());
        m.insert("run:5".into(), serde_json::json!({ "at_ms": now - 2 * 86_400_000, "jobs": [{ "state": "done", "token": "tok0000000000000000001" }] }).to_string());
        m.insert("rlog:tok0000000000000000001:1:001".into(), "\"s3:us-east-1\"".into());
        m.insert("rtree-at:fresh".into(), now.to_string());
        // A machine keeping images till later, and one for a job no longer kept.
        m.insert(format!("keeping:{counted}"), (now + 600_000).to_string());
        m.insert("keeping:4242".into(), (now - 1).to_string());
        // A move here a moment ago.
        m.insert("claimed_at".into(), now.to_string());
    }
    served.aws.fail("DeleteLaunchTemplate", 403, &superci_testkit::ec2_error("UnauthorizedOperation", "You are not authorized to perform this operation."), None);
    served.upkeep();
    let st = |id: u64| state(id)["state"].as_str().unwrap_or_default().to_string();
    assert_eq!(st(just), "launching");
    assert_eq!((st(past), state(past)["error"].as_str().unwrap_or_default().to_string()), ("swept".into(), "said before".into()));
    assert_eq!(st(never_said), "swept", "{}", state(never_said));
    {
        let m = served.store.0.lock().unwrap();
        for k in ["aws:template:us-east-1:superci-fleet-9", "rtree-at:fresh", &format!("keeping:{counted}")] { assert!(m.contains_key(k), "{k} gone") }
        assert!(!m.contains_key("keeping:4242"));
    }
    served.github.fail_none();
    served.aws.fail_none();
    let home = signed_in(&served);
    let (mut child, link) = dashboard(&home);
    b.goto(&format!("{link}&p=jobs"));
    b.wait(&shows("Recent jobs"));
    b.goto(&format!("{link}&p=job&id={past}"));
    b.wait(&format!("{} && !document.querySelector('main').innerText.includes('Reading your control plane')", shows("Status")));
    seen_in_browser(&served);
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

/// A failing runner taking a job that waits for room (failed for the first one's reason, the first given a failing
/// runner again), and one taking a job already over (left as it is). Each page says how it stands.
#[test]
fn a_failing_runner_taking_a_waiting_job_or_one_over_settles_each_and_their_pages_say_so() {
    let Some(mut b) = Browser::open(1300, 900) else { eprintln!("no Chrome here: skipped"); return };
    let served = serve(|_| {});
    served.connect_github();
    let state = |id: u64| served.store.under(&format!("job:{id}"))[0].1.clone();
    let edit = |id: u64, f: &dyn Fn(&mut serde_json::Value)| { let mut m = served.store.0.lock().unwrap(); let k = format!("job:{id}"); let mut j: serde_json::Value = serde_json::from_str(&m[&k]).unwrap(); f(&mut j); m.insert(k, j.to_string()); };
    let job = |name: &str, label: &str| { let (id, q) = served.github.queue(name, label, served.now_ms()); assert!(served.webhook(q) < 300); id };
    // A label no machine has, its failing runner started; its record nameless (from before names were kept).
    let first = job("first", "superci/cpus=8");
    edit(first, &|j| { j["name"] = "".into(); });
    let waiting = job("waiting", "superci");
    edit(waiting, &|j| { j["state"] = "waiting".into(); });
    let failing = format!("superci-{PLANE}-{first}-x");
    served.webhook(served.github.start_on(waiting, &failing));
    served.webhook(served.github.finish(waiting, "failure", "2026-10-15T10:00:01.0000000Z Error: the label is not understood", served.now_ms()));
    assert!(state(waiting)["state"] == "failed" && state(waiting)["error"].as_str().unwrap_or_default().contains("another job"), "{}", state(waiting));
    assert!(state(first)["fail_pending"].is_string(), "{}", state(first));
    // Another such label; its failing runner takes a job already over: left as it is.
    let second = job("second", "superci/cpus=4");
    let over = job("over", "superci");
    edit(over, &|j| { j["state"] = "done".into(); j["seen_in_progress"] = true.into(); });
    let failing = format!("superci-{PLANE}-{second}-x");
    served.webhook(served.github.start_on(over, &failing));
    served.webhook(served.github.finish(over, "failure", "2026-10-15T10:00:01.0000000Z Error", served.now_ms()));
    assert!(state(over)["failed_fast"] != true, "{}", state(over));
    let home = signed_in(&served);
    let (mut child, link) = dashboard(&home);
    for id in [first, waiting, over] {
        b.goto(&format!("{link}&p=job&id={id}"));
        b.wait(&format!("{} && !document.querySelector('main').innerText.includes('Reading your control plane')", shows("Status")));
    }
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

/// GitLab's jobs on AWS whose events never came, found at the sweep as GitLab has them: running on their runners,
/// passed, cancelled (GitLab refusing to remove its runner), and still pending (its machine tried again).
#[test]
fn the_sweep_settles_gitlab_jobs_whose_events_never_came_as_gitlab_has_them() {
    let Some(mut b) = Browser::open(1300, 1000) else { eprintln!("no Chrome here: skipped"); return };
    let served = serve(|c| {
        (c.aws_own, c.aws_own_creds) = (Some(("111111111111".into(), "us-east-1".into())), Some(superci_core::aws::Credentials { access_key_id: "ASIAX".into(), secret_access_key: "secret".into(), session_token: Some("tok".into()), expires_at_ms: u64::MAX }));
    });
    *served.public.lock().unwrap() = Some("https://plane.acme.test".into());
    served.connect_gitlab();
    served.aws.put(&format!("/superci/{PLANE}/ROUTING"), r#"{"order":[{"cloud":"aws","max_jobs":100},{"cloud":"aws-on-demand"},{"cloud":"cloudflare","off":true}]}"#);
    served.aws.replaying(include_str!("../fixtures/aws/launch/spot.jsonl"), &[]);
    let started: String = include_str!("../fixtures/aws/launch/on-demand.jsonl").lines().filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .find(|v| v["asked"].as_str().unwrap_or_default().contains("Action=RunInstances") && v["status"] == 200).map(|v| v["answer"].as_str().unwrap_or_default().to_string()).unwrap();
    served.store.0.lock().unwrap().insert("aws:no-fleet".into(), served.now_ms().to_string());
    served.aws.fail("RunInstances", 200, &started, None);
    let state = |id: u64| served.store.under(&format!("job:gl{id}"))[0].1.clone();
    let edit = |id: u64, f: &dyn Fn(&mut serde_json::Value)| { let mut m = served.store.0.lock().unwrap(); let k = format!("job:gl{id}"); let mut j: serde_json::Value = serde_json::from_str(&m[&k]).unwrap(); f(&mut j); m.insert(k, j.to_string()); };
    let mut ids = vec![];
    for (name, ends) in [("running", ""), ("passed", "success"), ("cancelled", "canceled"), ("pending", "-")] {
        let (id, p) = served.gitlab.pending(name, &["superci"], served.now_ms());
        assert!(served.gitlab_webhook(p) < 300);
        let runner = state(id)["runner_id"].as_u64().unwrap();
        if ends != "-" { let _ = served.gitlab.running_on(id, runner, served.now_ms()); }
        if !ends.is_empty() && ends != "-" { let _ = served.gitlab.finish(id, ends, "ok", served.now_ms()); }
        ids.push((id, name));
    }
    let nine = served.now_ms() - 9 * 60_000;
    for (id, _) in &ids { edit(*id, &|j| { j["launched_ms"] = nine.into(); }) }
    // (GitLab refusing to remove the cancelled one's runner only: the others' go.)
    served.gitlab.fail("DELETE", &format!("/runners/{}", state(ids[2].0)["runner_id"]), 500);
    served.upkeep();
    served.gitlab.fail_none();
    let st = |id: u64| state(id)["state"].as_str().unwrap_or_default().to_string();
    assert_eq!(st(ids[0].0), "running", "{}", state(ids[0].0));
    assert_eq!(st(ids[1].0), "done", "{}", state(ids[1].0));
    assert_eq!(st(ids[2].0), "cancelled", "{}", state(ids[2].0));
    served.aws.fail_none();
    let home = signed_in(&served);
    let (mut child, link) = dashboard(&home);
    for (id, _) in &ids {
        b.goto(&format!("{link}&p=job&id={id}&gl="));
        b.wait(&format!("{} && !document.querySelector('main').innerText.includes('Reading your control plane')", shows("Status")));
    }
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

/// A runner taking another job than its own, for each way its own job and that other job stand: the other job with no
/// machine coming gets this one (its own machine ended, failed, or never had), the runner's own job then needing none
/// (cancelled, failed by a failing runner, over, its machine meant for that other job) or a new one; the other job
/// with a machine still coming has that one handed to this runner's job. Each job's page says how it stands.
#[test]
fn a_runner_taking_another_jobs_work_swaps_machines_for_each_way_the_two_jobs_stand() {
    let Some(mut b) = Browser::open(1300, 1000) else { eprintln!("no Chrome here: skipped"); return };
    let served = serve(|c| {
        (c.aws_own, c.aws_own_creds) = (Some(("111111111111".into(), "us-east-1".into())), Some(superci_core::aws::Credentials { access_key_id: "ASIAX".into(), secret_access_key: "secret".into(), session_token: Some("tok".into()), expires_at_ms: u64::MAX }));
    });
    served.connect_github();
    served.aws.put(&format!("/superci/{PLANE}/ROUTING"), r#"{"default":null,"rules":[],"order":[{"cloud":"aws","max_jobs":100},{"cloud":"aws-on-demand"},{"cloud":"cloudflare","off":true}]}"#);
    served.aws.replaying(include_str!("../fixtures/aws/launch/spot.jsonl"), &[]);
    let started: String = include_str!("../fixtures/aws/launch/on-demand.jsonl").lines().filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .find(|v| v["asked"].as_str().unwrap_or_default().contains("Action=RunInstances") && v["status"] == 200).map(|v| v["answer"].as_str().unwrap_or_default().to_string()).unwrap();
    served.store.0.lock().unwrap().insert("aws:no-fleet".into(), served.now_ms().to_string());
    served.aws.fail("RunInstances", 200, &started, None);
    let state = |id: u64| served.store.under(&format!("job:{id}"))[0].1.clone();
    let edit = |id: u64, f: &dyn Fn(&mut serde_json::Value)| { let mut m = served.store.0.lock().unwrap(); let k = format!("job:{id}"); let mut j: serde_json::Value = serde_json::from_str(&m[&k]).unwrap(); f(&mut j); m.insert(k, j.to_string()); };
    let job = |name: &str| { let (id, q) = served.github.queue(name, "superci", served.now_ms()); assert!(served.webhook(q) < 300); id };
    let now = served.now_ms();
    let owns: [(&str, &dyn Fn(&mut serde_json::Value, u64)); 5] = [
        ("its own", &|_, _| {}),
        ("orphan", &|j, _| { j["state"] = "orphan".into(); }),
        ("failed fast", &|j, _| { j["failed_fast"] = true.into(); }),
        ("over", &|j, _| { j["over"] = true.into(); }),
        ("for the other", &|j, other| { j["for_job"] = other.into(); }),
    ];
    let others: [(&str, &dyn Fn(&mut serde_json::Value)); 5] = [
        ("its machine ended", &|j| { j["state"] = "swept".into(); j["ended_ms"] = now.into(); }),
        ("no machine", &|j| { j["state"] = "failed".into(); j.as_object_mut().unwrap().remove("machine_id"); }),
        ("its machine coming", &|_| {}),
        ("running, not begun", &|j| { j["state"] = "running".into(); j["seen_in_progress"] = true.into(); j["began"] = false.into(); }),
        ("running, begun", &|j| { j["state"] = "running".into(); j["seen_in_progress"] = true.into(); j["began"] = true.into(); }),
    ];
    let mut pairs = vec![];
    for (own_is, own_edit) in &owns {
        for (other_is, other_edit) in &others {
            let (a, o) = (job(&format!("{own_is} / {other_is}")), job(&format!("other: {own_is} / {other_is}")));
            edit(a, &|j| own_edit(j, o));
            edit(o, &|j| other_edit(j));
            let runner = state(a)["runner"].as_str().unwrap_or_else(|| panic!("{own_is} / {other_is}: {}", state(a))).to_string();
            assert!(served.webhook(served.github.start_on(o, &runner)) < 300);
            pairs.push((a, o, *own_is, *other_is));
        }
    }
    for (a, o, own_is, other_is) in &pairs {
        let (sa, so) = (state(*a), state(*o));
        let (a_state, o_state) = (sa["state"].as_str().unwrap_or_default(), so["state"].as_str().unwrap_or_default());
        let swapped = ["its machine ended", "no machine"].contains(other_is);
        if swapped {
            assert_eq!(o_state, "running", "{own_is} / {other_is}: {so}");
            let want = match *own_is { "orphan" => "cancelled", "failed fast" => "failed", "over" | "for the other" => "done", _ => "launched" };
            assert_eq!(a_state, want, "{own_is} / {other_is}: {sa}");
        } else {
            assert_eq!(sa["ran"].as_u64(), Some(*o), "{own_is} / {other_is}: {sa}");
            assert!(so["began"] == true, "{own_is} / {other_is}: {so}");
            if *other_is == "its machine coming" { assert!(so["for_job"].as_u64().is_some(), "{own_is} / {other_is}: {so}") }
        }
    }
    // Its own start told twice; a machine left idle by a cancel taking its own job after all; a job begun on a runner
    // none of ours told again.
    let twice = job("started twice");
    let (q1, q2) = (served.github.start(twice), served.github.start(twice));
    assert!(served.webhook(q1) < 300 && served.webhook(q2) < 300);
    assert_eq!(state(twice)["state"], "running");
    let idle = job("left idle");
    edit(idle, &|j| { j["state"] = "orphan".into(); });
    assert!(served.webhook(served.github.start(idle)) < 300);
    assert_eq!(state(idle)["state"], "running", "{}", state(idle));
    let elsewhere = job("begun elsewhere");
    edit(elsewhere, &|j| { j["began"] = true.into(); });
    assert!(served.webhook(served.github.start_elsewhere(elsewhere, "someone-elses-runner")) < 300);
    // (And one not begun yet, taken by a runner none of ours: begun, as far as this control plane is concerned.)
    let unbegun = job("taken elsewhere");
    assert!(served.webhook(served.github.start_elsewhere(unbegun, "someone-elses-runner")) < 300);
    assert_eq!(state(unbegun)["began"], true, "{}", state(unbegun));
    served.aws.fail_none();
    let home = signed_in(&served);
    let (mut child, link) = dashboard(&home);
    // (Each of the latest jobs' pages: see `seen_in_browser`.)
    b.goto(&format!("{link}&p=jobs"));
    b.wait(&shows("Recent jobs"));
    seen_in_browser(&served);
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

/// A job's end as GitHub says it, for each way its record stands: its machine keeping its images a while; cancelled
/// while it waited for room, while its machine was asked for, before any runner took it (taken back, or already over);
/// ended on another job's runner; its record holding another job's machine; a conclusion said again differently; and
/// one ended before this control plane knew of it. Each job's page says how it stands.
#[test]
fn a_jobs_end_is_settled_for_each_way_its_record_stands_and_said_on_its_page() {
    let Some(mut b) = Browser::open(1300, 1000) else { eprintln!("no Chrome here: skipped"); return };
    let served = serve(|c| {
        (c.aws_own, c.aws_own_creds) = (Some(("111111111111".into(), "us-east-1".into())), Some(superci_core::aws::Credentials { access_key_id: "ASIAX".into(), secret_access_key: "secret".into(), session_token: Some("tok".into()), expires_at_ms: u64::MAX }));
    });
    served.connect_github();
    served.aws.put(&format!("/superci/{PLANE}/ROUTING"), r#"{"default":null,"rules":[],"order":[{"cloud":"aws","max_jobs":100},{"cloud":"aws-on-demand"},{"cloud":"cloudflare","off":true}]}"#);
    served.aws.replaying(include_str!("../fixtures/aws/launch/spot.jsonl"), &[]);
    let started: String = include_str!("../fixtures/aws/launch/on-demand.jsonl").lines().filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .find(|v| v["asked"].as_str().unwrap_or_default().contains("Action=RunInstances") && v["status"] == 200).map(|v| v["answer"].as_str().unwrap_or_default().to_string()).unwrap();
    served.store.0.lock().unwrap().insert("aws:no-fleet".into(), served.now_ms().to_string());
    served.aws.fail("RunInstances", 200, &started, None);
    let state = |id: u64| served.store.under(&format!("job:{id}"))[0].1.clone();
    let edit = |id: u64, f: &dyn Fn(&mut serde_json::Value)| { let mut m = served.store.0.lock().unwrap(); let k = format!("job:{id}"); let mut j: serde_json::Value = serde_json::from_str(&m[&k]).unwrap(); f(&mut j); m.insert(k, j.to_string()); };
    let job = |name: &str| { let (id, q) = served.github.queue(name, "superci", served.now_ms()); assert!(served.webhook(q) < 300); id };
    let gone = |id: u64| served.store.0.lock().unwrap().contains_key(&format!("gone:{id}"));
    let now = served.now_ms();
    // Its machine keeps its job's images: ended later, if it has not gone by itself.
    let keeps = job("keeps its images");
    assert!(served.webhook(served.github.start(keeps)) < 300);
    edit(keeps, &|j| { j["keeps"] = true.into(); });
    assert!(served.webhook(served.github.finish(keeps, "success", "ok", now)) < 300);
    assert!(served.store.0.lock().unwrap().contains_key(&format!("keeping:{keeps}")), "{}", state(keeps));
    // Cancelled while it waited for room, and while its machine was being asked for.
    let (waiting, launching) = (job("waiting"), job("launching"));
    edit(waiting, &|j| { j["state"] = "waiting".into(); for k in ["machine_id", "runner", "runner_id"] { j.as_object_mut().unwrap().remove(k); } });
    edit(launching, &|j| { j["state"] = "launching".into(); });
    for id in [waiting, launching] { assert!(served.webhook(served.github.cancel(id, now)) < 300) }
    assert!(state(waiting)["state"] == "cancelled" && gone(waiting), "{}", state(waiting));
    assert!(gone(launching), "{}", state(launching));
    // Cancelled before any runner took it: its runner taken back (its machine ended), or already over.
    let (taken_back, over) = (job("taken back"), job("already over"));
    edit(over, &|j| { j["over"] = true.into(); });
    for id in [taken_back, over] { assert!(served.webhook(served.github.cancel(id, now)) < 300) }
    assert!(state(taken_back)["began"] == true && state(over)["over"] == true, "{} {}", state(taken_back), state(over));
    // Ended on another job's runner (that job still queued): its own machine is for that one now.
    let (ran_on, runner_of) = (job("ran on another's runner"), job("whose runner it ran on"));
    let runner = state(runner_of)["runner"].as_str().unwrap().to_string();
    let _ = served.github.start_on(ran_on, &runner);
    assert!(served.webhook(served.github.finish(ran_on, "success", "ok", now)) < 300);
    assert!(state(ran_on)["over"] == true && state(ran_on)["for_job"].as_u64() == Some(runner_of), "{}", state(ran_on));
    // Its record holding a machine busy with another job's work; and one whose conclusion is said again differently.
    let (holding, again) = (job("holding another's machine"), job("said again"));
    edit(holding, &|j| { j["state"] = "running".into(); j["seen_in_progress"] = true.into(); j["began"] = false.into(); j["runner"] = "superci-elsewhere".into(); });
    edit(again, &|j| { j["state"] = "done".into(); j["seen_in_progress"] = true.into(); j["began"] = true.into(); j["conclusion"] = "failure".into(); j["runner"] = "superci-elsewhere".into(); });
    for id in [holding, again] { assert!(served.webhook(served.github.cancel(id, now)) < 300) }
    assert!(state(holding)["began"] == true && state(again)["conclusion"] == "cancelled", "{} {}", state(holding), state(again));
    // Ended before this control plane knew of it: its queued event, behind, starts nothing.
    let (unknown, queued) = served.github.queue("unknown", "superci", now);
    assert!(served.webhook(served.github.cancel(unknown, now)) < 300);
    assert!(gone(unknown));
    assert!(served.webhook(queued) < 300);
    // Ending on another job's runner while waiting, and while its machine was asked for.
    let host = job("host runner");
    let host_runner = state(host)["runner"].as_str().unwrap().to_string();
    let (w2, l2) = (job("waiting, on another's runner"), job("launching, on another's runner"));
    edit(w2, &|j| { j["state"] = "waiting".into(); for k in ["machine_id", "runner", "runner_id"] { j.as_object_mut().unwrap().remove(k); } });
    edit(l2, &|j| { j["state"] = "launching".into(); });
    for id in [w2, l2] { let _ = served.github.start_on(id, &host_runner); assert!(served.webhook(served.github.finish(id, "success", "ok", now)) < 300); }
    // Seen running, then cancelled with no runner; its runner kept by GitHub (taken back refused): left idle.
    let (seen, kept_runner) = (job("seen, cancelled"), job("runner kept"));
    edit(seen, &|j| { j["seen_in_progress"] = true.into(); });
    served.github.fail("DELETE", &format!("/runners/{}", state(kept_runner)["runner_id"]), 422, "Runner is still running a job");
    for id in [seen, kept_runner] { assert!(served.webhook(served.github.cancel(id, now)) < 300) }
    served.github.fail_none();
    assert_eq!(state(kept_runner)["state"], "orphan", "{}", state(kept_runner));
    // The same end said twice: nothing more.
    assert!(served.webhook(served.github.cancel(again, now)) < 300);
    // A job not known here ended on a runner of ours.
    let (stranger, _) = served.github.queue("stranger", "superci", now);
    let other_host = job("other host");
    let _ = served.github.start_on(stranger, &state(other_host)["runner"].as_str().unwrap().to_string());
    assert!(served.webhook(served.github.finish(stranger, "success", "ok", now)) < 300);
    served.aws.fail_none();
    let home = signed_in(&served);
    let (mut child, link) = dashboard(&home);
    for id in [keeps, waiting, launching, taken_back, over, ran_on, holding, again] {
        b.goto(&format!("{link}&p=job&id={id}"));
        b.wait(&format!("{} && !document.querySelector('main').innerText.includes('Reading your control plane')", shows("Status")));
    }
    seen_in_browser(&served);
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

/// Jobs whose spot machines AWS took back, run again once their runs have finished, for each way GitHub answers: run
/// again alone, two of one run together, a run still going (asked later), a run gone, six hours on, the App not
/// allowed, GitHub refusing, GitHub failing (asked later). Each job's page says what became of it.
#[test]
fn jobs_aws_took_back_are_run_again_once_their_runs_end_or_say_why_not_for_each_way_github_answers() {
    let Some(mut b) = Browser::open(1300, 1000) else { eprintln!("no Chrome here: skipped"); return };
    let served = serve(|c| {
        (c.aws_own, c.aws_own_creds) = (Some(("111111111111".into(), "us-east-1".into())), Some(superci_core::aws::Credentials { access_key_id: "ASIAX".into(), secret_access_key: "secret".into(), session_token: Some("tok".into()), expires_at_ms: u64::MAX }));
    });
    served.connect_github();
    served.aws.put(&format!("/superci/{PLANE}/ROUTING"), r#"{"default":null,"rules":[],"order":[{"cloud":"aws","max_jobs":100},{"cloud":"aws-on-demand"},{"cloud":"cloudflare","off":true}]}"#);
    served.aws.replaying(include_str!("../fixtures/aws/launch/spot.jsonl"), &[]);
    let started: String = include_str!("../fixtures/aws/launch/on-demand.jsonl").lines().filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .find(|v| v["asked"].as_str().unwrap_or_default().contains("Action=RunInstances") && v["status"] == 200).map(|v| v["answer"].as_str().unwrap_or_default().to_string()).unwrap();
    served.store.0.lock().unwrap().insert("aws:no-fleet".into(), served.now_ms().to_string());
    served.aws.fail("RunInstances", 200, &started, None);
    let state = |id: u64| served.store.under(&format!("job:{id}"))[0].1.clone();
    let edit = |id: u64, f: &dyn Fn(&mut serde_json::Value)| { let mut m = served.store.0.lock().unwrap(); let k = format!("job:{id}"); let mut j: serde_json::Value = serde_json::from_str(&m[&k]).unwrap(); f(&mut j); m.insert(k, j.to_string()); };
    // Each taken back as it ran, its runner ending the job as failed.
    let taken_back = |name: &str| {
        let (id, q) = served.github.queue(name, "superci", served.now_ms());
        assert!(served.webhook(q) < 300);
        assert!(served.webhook(served.github.start(id)) < 300);
        edit(id, &|j| { j["interrupted"] = true.into(); });
        assert!(served.webhook(served.github.finish(id, "failure", "2026-10-15T10:00:01.0000000Z The runner has received a shutdown signal.", served.now_ms())) < 300);
        assert_eq!(state(id)["rerun"]["state"], "pending", "{name}: {}", state(id));
        id
    };
    let alone = taken_back("alone");
    let (first, second) = (taken_back("first of two"), taken_back("second of two"));
    let run_of = |id: u64| state(id)["run_id"].as_u64().unwrap();
    let first_run = run_of(first);
    edit(second, &|j| { j["rerun"]["run_id"] = first_run.into(); j["run_id"] = first_run.into(); });
    let going = taken_back("its run still going");
    let (still, q) = served.github.queue("still running", "superci", served.now_ms()); served.webhook(q); served.webhook(served.github.start(still));
    let still_run = run_of(still);
    edit(going, &|j| { j["rerun"]["run_id"] = still_run.into(); });
    let gone = taken_back("its run gone");
    edit(gone, &|j| { j["rerun"]["run_id"] = 39_999_999_999u64.into(); });
    let late = taken_back("six hours on");
    let long_ago = served.now_ms() - 7 * 3_600_000;
    edit(late, &|j| { j["rerun"]["at_ms"] = long_ago.into(); });
    let (not_allowed, refused, failing) = (taken_back("the App may not"), taken_back("GitHub refuses"), taken_back("GitHub fails"));
    served.github.fail("POST", &format!("/actions/jobs/{not_allowed}/rerun"), 403, "Resource not accessible by integration");
    served.github.fail("POST", &format!("/actions/jobs/{refused}/rerun"), 422, "This job cannot be re-run");
    served.github.fail("POST", &format!("/actions/jobs/{failing}/rerun"), 502, "Server Error");
    served.upkeep();
    let rerun = |id: u64| state(id)["rerun"]["state"].as_str().unwrap_or_default().to_string();
    assert_eq!(rerun(alone), "asked", "{}", state(alone));
    assert_eq!((rerun(first), rerun(second)), ("asked".into(), "asked".into()), "{} {}", state(first), state(second));
    assert!(served.github.asked.lock().unwrap().iter().any(|a| a.contains(&format!("/actions/runs/{first_run}/rerun-failed-jobs"))), "the two asked for together");
    assert_eq!(rerun(going), "pending");
    assert!(rerun(gone).contains("its run is gone"), "{}", state(gone));
    assert!(rerun(late).contains("six hours later"), "{}", state(late));
    assert!(rerun(not_allowed).contains("GitHub's App may not"), "{}", state(not_allowed));
    assert!(rerun(refused).starts_with("not run again: "), "{}", state(refused));
    assert_eq!(rerun(failing), "pending", "{}", state(failing));
    assert!(!served.store.0.lock().unwrap().contains_key(&format!("rerun-asked:{failing}")), "asked again later");
    served.github.fail_none();
    served.aws.fail_none();
    let home = signed_in(&served);
    let (mut child, link) = dashboard(&home);
    for (id, said) in [(alone, "ran the job again by itself"), (going, "SuperCI runs the job again by itself"), (gone, "its run is gone"), (late, "six hours later"), (not_allowed, "GitHub's App may not")] {
        b.goto(&format!("{link}&p=job&id={id}"));
        b.wait(&format!("{} && !document.querySelector('main').innerText.includes('Reading your control plane')", shows("Status")));
        let page = b.eval("document.querySelector('main').innerText").as_str().unwrap_or_default().replace('\n', " ");
        assert!(page.contains(said), "{id}: no {said:?}:\n{page}");
    }
    seen_in_browser(&served);
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

/// A job whose label nothing here can run is failed by a small runner where one starts: on AWS (no containers of its
/// own); not at all when AWS refuses it with on-demand turned off (tried again later); through a Modal launcher; and
/// nowhere when nothing is connected. Each job's page says why it failed.
#[test]
fn a_job_nothing_can_run_is_failed_by_a_small_runner_wherever_one_starts_and_its_page_says_why() {
    let Some(mut b) = Browser::open(1300, 1000) else { eprintln!("no Chrome here: skipped"); return };
    let started: String = include_str!("../fixtures/aws/launch/on-demand.jsonl").lines().filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .find(|v| v["asked"].as_str().unwrap_or_default().contains("Action=RunInstances") && v["status"] == 200).map(|v| v["answer"].as_str().unwrap_or_default().to_string()).unwrap();
    // Each setup: AWS connected, its settings, what AWS answers a machine, and whether the small runner started.
    let modal = r#"[{"cloud":"modal","url":"https://acme--superci-runners.modal.run"}]"#;
    let cases: Vec<(&str, bool, Vec<(&str, &str)>, bool, bool)> = vec![
        ("on AWS", true, vec![("CONTAINERS", "off")], true, true),
        ("AWS refusing, on-demand off", true, vec![("CONTAINERS", "off"), ("ROUTING", r#"{"order":[{"cloud":"aws"},{"cloud":"aws-on-demand","off":true}]}"#)], false, false),
        ("through a Modal launcher", false, vec![("CONTAINERS", "off"), ("LAUNCHERS", modal)], true, true),
        ("nowhere", false, vec![("CONTAINERS", "off")], true, false),
    ];
    for (what, with_aws, settings, aws_starts, fails_fast) in cases {
        let served = serve(move |c| {
            if with_aws { (c.aws_own, c.aws_own_creds) = (Some(("111111111111".into(), "us-east-1".into())), Some(superci_core::aws::Credentials { access_key_id: "ASIAX".into(), secret_access_key: "secret".into(), session_token: Some("tok".into()), expires_at_ms: u64::MAX })); }
        });
        served.connect_github();
        for (k, v) in &settings { served.aws.put(&format!("/superci/{PLANE}/{k}"), v) }
        served.aws.replaying(include_str!("../fixtures/aws/launch/spot.jsonl"), &[]);
        if aws_starts { served.aws.fail("RunInstances", 200, &started, None) } else { served.aws.fail("RunInstances", 400, &superci_testkit::ec2_error("InsufficientInstanceCapacity", "no"), None) }
        let (id, q) = served.github.queue(what, "superci/cpus=8", served.now_ms());
        assert!(served.webhook(q) < 300, "{what}");
        let job = served.store.under(&format!("job:{id}"))[0].1.clone();
        assert_eq!(job["failed_fast"] == true, fails_fast, "{what}: {job}");
        if with_aws && !fails_fast { assert!(job["fail_pending"].is_string(), "{what}: tried again later: {job}") }
        served.aws.fail_none();
        let home = signed_in(&served);
        let (mut child, link) = dashboard(&home);
        b.goto(&format!("{link}&p=job&id={id}"));
        b.wait(&format!("{} && !document.querySelector('main').innerText.includes('Reading your control plane')", shows("Status")));
        let page = b.eval("document.querySelector('main').innerText").as_str().unwrap_or_default().replace('\n', " ");
        assert!(page.contains("cpus"), "{what}: {page}");
        let _ = child.kill();
        let _ = std::fs::remove_dir_all(&home);
    }
}

/// Jobs GitHub never told this control plane of (their events lost): found at a sweep by asking GitHub for the waiting
/// jobs of the repositories that had jobs here, today's and a quieter one's, and given machines as if told. Each one's
/// page then says how it stands.
#[test]
fn jobs_github_never_told_of_are_found_by_asking_and_started_and_their_pages_say_so() {
    let Some(mut b) = Browser::open(1300, 1000) else { eprintln!("no Chrome here: skipped"); return };
    let served = serve(|_| {});
    served.connect_github();
    let known = |id: u64| served.store.0.lock().unwrap().contains_key(&format!("job:{id}"));
    // A job told of, in the repository; then one whose event never comes.
    let (told, q) = served.github.queue("told", "superci", served.now_ms());
    assert!(served.webhook(q) < 300);
    let (lost, _) = served.github.queue("lost", "superci", served.now_ms());
    served.upkeep();
    assert!(!known(lost), "made a moment ago: its event may still come");
    // Minutes on, the next look: found, and started as if told.
    served.later(5 * 60_000);
    served.upkeep();
    assert!(known(lost), "found by asking");
    // The repository quiet since (its last job two days ago): still asked, among the quieter ones.
    let ago = served.now_ms() - 2 * 86_400_000;
    for id in [told, lost] {
        let mut m = served.store.0.lock().unwrap();
        let k = format!("job:{id}");
        let mut j: serde_json::Value = serde_json::from_str(&m[&k]).unwrap();
        j["at_ms"] = ago.into();
        m.insert(k, j.to_string());
    }
    let (quiet, _) = served.github.queue("after a quiet weekend", "superci", served.now_ms());
    served.later(5 * 60_000);
    served.upkeep();
    assert!(known(quiet), "found in a quieter repository too");
    // One waiting with a label of another runner's: not this control plane's to start. A repository not heard of
    // for two weeks: no longer asked.
    let (not_ours, _) = served.github.queue("on GitHub's runners", "ubuntu-latest", served.now_ms());
    served.later(5 * 60_000);
    served.upkeep();
    assert!(!known(not_ours));
    let weeks = served.now_ms() - 15 * 86_400_000;
    { let mut m = served.store.0.lock().unwrap(); for (k, v) in m.clone().into_iter().filter(|(k, _)| k.starts_with("repo:")) { let mut j: serde_json::Value = serde_json::from_str(&v).unwrap(); j["at_ms"] = weeks.into(); m.insert(k, j.to_string()); } }
    let (forgotten, _) = served.github.queue("after two weeks", "superci", served.now_ms());
    served.later(5 * 60_000);
    served.upkeep();
    let _ = forgotten;
    let home = signed_in(&served);
    let (mut child, link) = dashboard(&home);
    for id in [lost, quiet] {
        b.goto(&format!("{link}&p=job&id={id}"));
        b.wait(&format!("{} && !document.querySelector('main').innerText.includes('Reading your control plane')", shows("Status")));
    }
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

/// Jobs through launchers in Cloudflare (with GitHub's full image) and Modal (one with a GPU), each started as the
/// launcher answered and stopped once over; a GitLab job Modal cannot run; and both launchers refusing what a job
/// asks, which fails it at once. Each job's page says where it ran, or why not.
#[test]
fn jobs_through_cloudflare_and_modal_launchers_start_stop_and_are_refused_as_each_launcher_answered() {
    let Some(mut b) = Browser::open(1300, 1000) else { eprintln!("no Chrome here: skipped"); return };
    let launchers = r#"[{"cloud":"cloudflare","url":"https://superci-runners.acme.workers.dev"},{"cloud":"modal","url":"https://acme--superci-runners.modal.run"}]"#;
    let served = serve(|_| {});
    served.connect_github();
    served.connect_gitlab();
    for (k, v) in [("CONTAINERS", "off"), ("LAUNCHERS", launchers), ("CF_IMAGE", "https://images.example.com/full:1")] { served.aws.put(&format!("/superci/{PLANE}/{k}"), v) }
    let state = |id: u64| served.store.under(&format!("job:{id}"))[0].1.clone();
    let job = |label: &str| { let (id, q) = served.github.queue(label, label, served.now_ms()); assert!(served.webhook(q) < 300); id };
    let on_cf = job("superci");
    let on_modal = job("superci/provider=modal/gpu=t4");
    for (id, cloud) in [(on_cf, "cloudflare"), (on_modal, "modal")] { assert!(state(id)["state"] == "launched" && state(id)["cloud"] == cloud, "{cloud}: {}", state(id)) }
    let image_sent = served.github.asked.lock().unwrap().iter().any(|a| a.contains("workers.dev/launch"));
    assert!(image_sent, "{:?}", served.github.asked.lock().unwrap());
    for id in [on_cf, on_modal] {
        assert!(served.webhook(served.github.start(id)) < 300);
        assert!(served.webhook(served.github.finish(id, "success", "2026-10-15T10:00:01.0000000Z ok", served.now_ms())) < 300);
    }
    // A GitLab job: not on Modal (it runs no Docker); on Cloudflare.
    let (gl, p) = served.gitlab.pending("gitlab", &["superci/provider=modal"], served.now_ms());
    assert!(served.gitlab_webhook(p) < 300);
    // Both launchers refusing what the job asks: it fails at once, saying each.
    served.github.fail("POST", "/launch", 400, "no such size");
    let refused = job("superci/cpu=2");
    served.github.fail_none();
    assert_eq!(state(refused)["state"], "failed", "{}", state(refused));
    let home = signed_in(&served);
    let (mut child, link) = dashboard(&home);
    let _ = gl;
    b.goto(&format!("{link}&p=jobs"));
    b.wait(&shows("Recent jobs"));
    seen_in_browser(&served);
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

/// How AWS's spot machines stand for each job: paused after AWS took two back (on-demand), not allowed for the role
/// lately, none now with on-demand at its limit (waits), kept off spot with on-demand at its limit (back on spot),
/// stopped by AWS's budget everywhere; a machine family with its own disk, memory alone asked, Windows with its
/// files in memory or behind no public address, a wait under the spot limit over, a job from before labels were
/// kept, Docker's images kept from the start. Each job's page says how it stands.
#[test]
fn each_way_aws_spot_machines_stand_decides_how_a_job_starts_and_its_page_says_so() {
    let Some(mut b) = Browser::open(1300, 1000) else { eprintln!("no Chrome here: skipped"); return };
    let served = serve(|c| {
        (c.aws_own, c.aws_own_creds) = (Some(("111111111111".into(), "us-east-1".into())), Some(superci_core::aws::Credentials { access_key_id: "ASIAX".into(), secret_access_key: "secret".into(), session_token: Some("tok".into()), expires_at_ms: u64::MAX }));
    });
    served.connect_github();
    served.aws.put(&format!("/superci/{PLANE}/ROUTING"), r#"{"order":[{"cloud":"aws","max_jobs":100},{"cloud":"aws-on-demand"},{"cloud":"cloudflare","off":true}]}"#);
    served.aws.replaying(include_str!("../fixtures/aws/launch/spot.jsonl"), &[]);
    let started: String = include_str!("../fixtures/aws/launch/on-demand.jsonl").lines().filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .find(|v| v["asked"].as_str().unwrap_or_default().contains("Action=RunInstances") && v["status"] == 200).map(|v| v["answer"].as_str().unwrap_or_default().to_string()).unwrap();
    let state = |id: u64| served.store.under(&format!("job:{id}"))[0].1.clone();
    let edit = |id: u64, f: &dyn Fn(&mut serde_json::Value)| { let mut m = served.store.0.lock().unwrap(); let k = format!("job:{id}"); let mut j: serde_json::Value = serde_json::from_str(&m[&k]).unwrap(); f(&mut j); m.insert(k, j.to_string()); };
    let fresh = || { served.aws.fail_none(); served.store.0.lock().unwrap().retain(|k, _| !k.starts_with("aws:") && !k.starts_with("spot:")); served.store.0.lock().unwrap().insert("aws:no-fleet".into(), served.now_ms().to_string()); };
    let job = |label: &str| { let (id, q) = served.github.queue(label, label, served.now_ms()); assert!(served.webhook(q) < 300); id };
    let no_room = superci_testkit::ec2_error("InsufficientInstanceCapacity", "no");
    let at_limit = superci_testkit::ec2_error("VcpuLimitExceeded", "You have requested more vCPU capacity than your current vCPU limit");
    let budget = superci_testkit::ec2_error("UnauthorizedOperation", &format!("You are not authorized to perform this operation. User: arn:aws:sts::111111111111:assumed-role/superci-plane-testplane0001/superci is not authorized to perform: ec2:RunInstances with an explicit deny in an identity-based policy: arn:aws:iam::111111111111:policy/{}", superci_core::aws::stop_policy_name(PLANE)));
    let mut ids = vec![];
    // Paused after AWS took two spot machines back a moment ago: on-demand.
    fresh();
    for n in 0..2 { served.store.0.lock().unwrap().insert(format!("spot:interrupted:{}:{n}", served.now_ms() - 60_000 - n), (served.now_ms() - 60_000).to_string()); }
    served.aws.fail("RunInstances", 200, &started, None);
    let paused = job("superci");
    assert_eq!(state(paused)["on_demand"], true, "{}", state(paused));
    ids.push(paused);
    // Spot not allowed for the role lately: on-demand straight away.
    fresh();
    served.store.0.lock().unwrap().insert("aws:spot-not-allowed".into(), served.now_ms().to_string());
    served.aws.fail("RunInstances", 200, &started, None);
    let not_allowed = job("superci");
    assert_eq!(state(not_allowed)["on_demand"], true, "{}", state(not_allowed));
    ids.push(not_allowed);
    // No spot machine now, on-demand at the account's limit: it waits for a spot one.
    fresh();
    served.aws.fail("RunInstances|MarketType", 400, &no_room, None);
    served.aws.fail("RunInstances", 400, &at_limit, None);
    let waits = job("superci");
    // (Its other spot pools asked round by round first.)
    for _ in 0..10 { if !state(waits)["error"].as_str().unwrap_or_default().contains("other spot pools") { break } served.later(25_000); served.upkeep(); }
    assert!(state(waits)["error"].as_str().unwrap_or_default().starts_with("waiting: AWS has no spot machine for it now, and your AWS account's on-demand limit"), "{}", state(waits));
    ids.push(waits);
    // Kept off spot (paused), on-demand at its limit: a spot machine is asked for after all.
    fresh();
    for n in 0..2 { served.store.0.lock().unwrap().insert(format!("spot:interrupted:{}:{n}", served.now_ms() - 60_000 - n), (served.now_ms() - 60_000).to_string()); }
    served.aws.fail("RunInstances|MarketType", 200, &started, None);
    served.aws.fail("RunInstances", 400, &at_limit, None);
    let back = job("superci");
    for _ in 0..10 { if !state(back)["error"].as_str().unwrap_or_default().contains("other spot pools") && state(back)["state"] != "waiting" { break } served.later(25_000); served.upkeep(); }
    ids.push(back);
    // AWS's budget stopping every new machine: said, and not tried again and again.
    fresh();
    served.aws.fail("RunInstances", 403, &budget, None);
    let stopped = job("superci");
    ids.push(stopped);
    // A family with its own disk (as AWS gave one), memory alone asked, Windows in memory and Windows with no public
    // address for its cache, Docker's images kept from the start on Ubuntu 26.04 (its files in memory too).
    fresh();
    served.aws.fail("RunInstances", 200, &started.replace("c8a.xlarge", "m6id.xlarge"), None);
    let local = job("superci/family=m6id");
    assert_eq!(state(local)["local_disk"], true, "{}", state(local));
    served.aws.fail_none();
    served.aws.fail("RunInstances", 200, &started, None);
    for label in ["superci/ram=16", "superci/os=windows/scratch=ramdisk", "superci/os=windows", "superci/image=ubuntu26/docker-cache=true", "superci/image=ubuntu26/docker-cache=true/scratch=ramdisk"] { ids.push(job(label)) }
    // Waiting under the spot limit for longer than it waits: asked again now; a waiting job with no label kept.
    let (limit_wait, q) = served.github.queue("waited under the spot limit", "superci", served.now_ms());
    served.aws.fail_none();
    served.aws.fail("RunInstances", 400, &no_room, None);
    assert!(served.webhook(q) < 300);
    let (no_label, q) = served.github.queue("no label kept", "superci", served.now_ms());
    assert!(served.webhook(q) < 300);
    let long_ago = served.now_ms() - 5 * 60_000;
    edit(limit_wait, &|j| { j["state"] = "waiting".into(); j["spot_limit_wait"] = long_ago.into(); });
    edit(no_label, &|j| { j["state"] = "waiting".into(); j["label"] = "".into(); });
    fresh();
    served.aws.fail("RunInstances", 200, &started, None);
    served.later(30_000);
    served.upkeep();
    assert_eq!(state(limit_wait)["state"], "launched", "{}", state(limit_wait));
    ids.extend([limit_wait, no_label]);
    served.aws.fail_none();
    let home = signed_in(&served);
    let (mut child, link) = dashboard(&home);
    b.goto(&format!("{link}&p=jobs"));
    b.wait(&shows("Recent jobs"));
    seen_in_browser(&served);
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

/// Starts as AWS takes its time and as jobs end meanwhile: spot pools asked until the start's time is up (the next
/// try below spot machines), a job cancelled while its machine was asked for (the machine ended at once), one whose
/// record ended meanwhile, a machine family with its own disk SuperCI does not know. Each job's page says how it stands.
#[test]
fn starts_that_take_aws_long_or_whose_jobs_end_meanwhile_settle_each_job_and_its_page_says_so() {
    let Some(mut b) = Browser::open(1300, 1000) else { eprintln!("no Chrome here: skipped"); return };
    let served = serve(|c| {
        (c.aws_own, c.aws_own_creds) = (Some(("111111111111".into(), "us-east-1".into())), Some(superci_core::aws::Credentials { access_key_id: "ASIAX".into(), secret_access_key: "secret".into(), session_token: Some("tok".into()), expires_at_ms: u64::MAX }));
    });
    served.connect_github();
    served.aws.put(&format!("/superci/{PLANE}/ROUTING"), r#"{"order":[{"cloud":"aws","max_jobs":100},{"cloud":"aws-on-demand"},{"cloud":"cloudflare","off":true}]}"#);
    served.aws.replaying(include_str!("../fixtures/aws/launch/spot.jsonl"), &[]);
    let started: String = include_str!("../fixtures/aws/launch/on-demand.jsonl").lines().filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .find(|v| v["asked"].as_str().unwrap_or_default().contains("Action=RunInstances") && v["status"] == 200).map(|v| v["answer"].as_str().unwrap_or_default().to_string()).unwrap();
    let state = |id: u64| served.store.under(&format!("job:{id}"))[0].1.clone();
    let fresh = || { served.aws.fail_none(); served.aws.during.lock().unwrap().clear(); served.store.0.lock().unwrap().retain(|k, _| !k.starts_with("aws:") && !k.starts_with("spot:")); served.store.0.lock().unwrap().insert("aws:no-fleet".into(), served.now_ms().to_string()); };
    let no_room = superci_testkit::ec2_error("InsufficientInstanceCapacity", "no");
    let mut ids = vec![];
    // Every spot pool full, AWS taking six seconds a request: out of time, the next try below spot machines.
    fresh();
    served.aws.fail("RunInstances|MarketType", 400, &no_room, None);
    served.aws.fail("RunInstances", 200, &started, None);
    let tick = served.ticker();
    served.aws.during("RunInstances", move || tick(6_000));
    let (slow, q) = served.github.queue("slow", "superci", served.now_ms());
    assert!(served.webhook(q) < 300);
    for _ in 0..10 { if state(slow)["state"] != "waiting" { break } served.later(25_000); served.upkeep(); }
    assert!(state(slow)["no_spot"] == true || state(slow)["on_demand"] == true || state(slow)["error"].as_str().unwrap_or_default().contains("out of time"), "{}", state(slow));
    ids.push(slow);
    // Cancelled while its machine was asked for: the machine it is given is ended at once, the job cancelled.
    fresh();
    served.aws.fail("RunInstances", 200, &started, None);
    let (gone, q) = served.github.queue("gone meanwhile", "superci", served.now_ms());
    let store = served.store.0.clone();
    let now = served.now_ms();
    served.aws.during("RunInstances", move || { store.lock().unwrap().insert(format!("gone:{gone}"), now.to_string()); });
    assert!(served.webhook(q) < 300);
    assert_eq!(state(gone)["state"], "cancelled", "{}", state(gone));
    ids.push(gone);
    // Its record ended meanwhile (cancelled by its event as the machine was asked for).
    fresh();
    served.aws.fail("RunInstances", 200, &started, None);
    let (ended, q) = served.github.queue("ended meanwhile", "superci", served.now_ms());
    let store = served.store.0.clone();
    served.aws.during("DescribeImages", move || {
        let mut m = store.lock().unwrap();
        let k = format!("job:{ended}");
        if let Some(v) = m.get(&k).cloned() { let mut j: serde_json::Value = serde_json::from_str(&v).unwrap(); j["state"] = "cancelled".into(); j["ended_ms"] = now.into(); j["rev"] = (j["rev"].as_u64().unwrap_or(0) + 1).into(); m.insert(k, j.to_string()); }
    });
    assert!(served.webhook(q) < 300);
    ids.push(ended);
    // A family with its own disk SuperCI does not know (as AWS gave it): its files on the standard disk.
    fresh();
    served.aws.fail("RunInstances", 200, &started.replace("c8a.xlarge", "m5d.xlarge"), None);
    let (odd, q) = served.github.queue("odd family", "superci", served.now_ms());
    assert!(served.webhook(q) < 300);
    assert!(state(odd)["local_disk"] != true, "{}", state(odd));
    ids.push(odd);
    fresh();
    let home = signed_in(&served);
    let (mut child, link) = dashboard(&home);
    b.goto(&format!("{link}&p=jobs"));
    b.wait(&shows("Recent jobs"));
    seen_in_browser(&served);
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

/// A control plane whose own containers are Modal's: a job started there, sized for it, and stopped once over.
#[test]
fn a_control_plane_whose_own_containers_are_modals_starts_and_stops_a_job_there() {
    let Some(mut b) = Browser::open(1300, 900) else { eprintln!("no Chrome here: skipped"); return };
    let served = serve(|c| { c.own_cloud = "modal".into(); });
    served.connect_github();
    let (id, q) = served.github.queue("on modal", "superci/cpu=2", served.now_ms());
    assert!(served.webhook(q) < 300);
    let job = served.store.under(&format!("job:{id}"))[0].1.clone();
    assert_eq!(job["state"], "launched", "{job}");
    served.webhook(served.github.start(id));
    served.webhook(served.github.finish(id, "success", "2026-10-15T10:00:01.0000000Z ok", served.now_ms()));
    let home = signed_in(&served);
    let (mut child, link) = dashboard(&home);
    b.goto(&format!("{link}&p=job&id={id}"));
    b.wait(&format!("{} && !document.querySelector('main').innerText.includes('Reading your control plane')", shows("Status")));
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

/// GitLab's jobs on AWS: one whose spot machine AWS takes back, retried at GitLab and run again off spot machines; an
/// event delivered twice; one taken by another job's runner; one cancelled while its machine starts. Each job's page
/// says how it went.
#[test]
fn gitlab_jobs_on_aws_taken_back_retried_on_anothers_runner_and_cancelled_are_said_on_their_pages() {
    let Some(mut b) = Browser::open(1300, 1000) else { eprintln!("no Chrome here: skipped"); return };
    let served = serve(|c| {
        (c.aws_own, c.aws_own_creds) = (Some(("111111111111".into(), "us-east-1".into())), Some(superci_core::aws::Credentials { access_key_id: "ASIAX".into(), secret_access_key: "secret".into(), session_token: Some("tok".into()), expires_at_ms: u64::MAX }));
    });
    *served.public.lock().unwrap() = Some("https://plane.acme.test".into());
    served.connect_gitlab();
    served.aws.put(&format!("/superci/{PLANE}/ROUTING"), r#"{"default":null,"rules":[],"order":[{"cloud":"aws","max_jobs":100},{"cloud":"aws-on-demand"},{"cloud":"cloudflare","off":true}]}"#);
    served.aws.replaying(include_str!("../fixtures/aws/launch/spot.jsonl"), &[]);
    let started: String = include_str!("../fixtures/aws/launch/on-demand.jsonl").lines().filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .find(|v| v["asked"].as_str().unwrap_or_default().contains("Action=RunInstances") && v["status"] == 200).map(|v| v["answer"].as_str().unwrap_or_default().to_string()).unwrap();
    served.store.0.lock().unwrap().insert("aws:no-fleet".into(), served.now_ms().to_string());
    served.aws.fail("RunInstances", 200, &started, None);
    let state = |id: u64| served.store.under(&format!("job:gl{id}"))[0].1.clone();
    let agent = ureq::Agent::new_with_defaults();
    let say = |q: &str| agent.post(&format!("{}/{q}", served.url)).send("").map(|r| r.status().as_u16()).unwrap_or_else(|e| match e { ureq::Error::StatusCode(c) => c, _ => 0 });
    // Taken back: its machine says so, GitLab fails the job, and SuperCI asks GitLab to run it again.
    let (back, p) = served.gitlab.pending("e2e", &["superci"], served.now_ms());
    assert!(served.gitlab_webhook(p.clone()) < 300);
    assert!(served.gitlab_webhook(p) < 300, "the same event again: known");
    let notice = state(back)["notice"].as_str().expect("its machine's link to say AWS takes it back").to_string();
    served.gitlab_webhook(served.gitlab.running(back, served.now_ms()));
    assert_eq!(say(&format!("interrupted?gl={back}&t={notice}")), 200);
    served.gitlab_webhook(served.gitlab.finish(back, "failed", "ERROR: Job failed: the runner was stopped", served.now_ms()));
    assert!(served.gitlab.asked.lock().unwrap().iter().any(|a| a.contains(&format!("/jobs/{back}/retry"))), "GitLab asked to run it again");
    // Its machine's notice again, its job over: found by its id, nothing more noted.
    let noted = served.store.under("spot:interrupted:").len();
    assert_eq!(say(&format!("interrupted?gl={back}&t={notice}")), 200);
    assert_eq!(served.store.under("spot:interrupted:").len(), noted);
    // Taken by another job's runner: its own machine's job then runs as the other job.
    let (x, p) = served.gitlab.pending("lint", &["superci"], served.now_ms()); served.gitlab_webhook(p);
    let (y, p) = served.gitlab.pending("build", &["superci"], served.now_ms()); served.gitlab_webhook(p);
    let ry = state(y)["runner_id"].as_u64().expect("its runner");
    served.gitlab_webhook(served.gitlab.running_on(x, ry, served.now_ms()));
    // It ends there: that runner's machine is done; its own machine is for the other job now.
    served.gitlab_webhook(served.gitlab.finish(x, "success", "ok", served.now_ms()));
    // One cancelled once it was on another's runner, its own record still starting.
    let (z2, p) = served.gitlab.pending("deploy 2", &["superci"], served.now_ms()); served.gitlab_webhook(p);
    let (h2, p) = served.gitlab.pending("its host", &["superci"], served.now_ms()); served.gitlab_webhook(p);
    let rh2 = state(h2)["runner_id"].as_u64().expect("its runner");
    { let mut m = served.store.0.lock().unwrap(); let k = format!("job:gl{z2}"); let mut j: serde_json::Value = serde_json::from_str(&m[&k]).unwrap(); j["state"] = "launching".into(); m.insert(k, j.to_string()); }
    let _ = served.gitlab.running_on(z2, rh2, served.now_ms());
    served.gitlab_webhook(served.gitlab.finish(z2, "canceled", "", served.now_ms()));
    // Cancelled while its machine starts.
    let (z, p) = served.gitlab.pending("deploy", &["superci"], served.now_ms()); served.gitlab_webhook(p);
    served.gitlab_webhook(served.gitlab.finish(z, "canceled", "", served.now_ms()));
    // The job GitLab made of the one taken back, run again: off spot machines.
    let (retried, p) = served.gitlab.pending("e2e", &["superci"], served.now_ms());
    assert!(served.gitlab_webhook(p) < 300);
    assert!(state(retried)["no_spot"] == true, "{}", state(retried));
    // Cancelled before this control plane knew of it: its pending event, behind, starts nothing.
    let (unknown, p) = served.gitlab.pending("unknown", &["superci"], served.now_ms());
    served.gitlab_webhook(served.gitlab.finish(unknown, "canceled", "", served.now_ms()));
    assert!(served.store.0.lock().unwrap().contains_key(&format!("gone:gl{unknown}")));
    let _ = served.gitlab_webhook(p);
    // Cancelled while it waited for room, while its machine was asked for, once its machine came up (left idle for
    // the sweep), and once over already.
    let edit = |id: u64, f: &dyn Fn(&mut serde_json::Value)| { let mut m = served.store.0.lock().unwrap(); let k = format!("job:gl{id}"); let mut j: serde_json::Value = serde_json::from_str(&m[&k]).unwrap(); f(&mut j); m.insert(k, j.to_string()); };
    let mut cancelled = vec![];
    for (name, how, after) in [("waits", "waiting", "cancelled"), ("asked for", "launching", "cancelled"), ("came up", "", "orphan"), ("over", "over", "launched")] {
        let (id, p) = served.gitlab.pending(name, &["superci"], served.now_ms());
        assert!(served.gitlab_webhook(p) < 300);
        match how { "" => {} "over" => edit(id, &|j| { j["over"] = true.into(); }), s => edit(id, &|j| { j["state"] = s.into(); }) }
        served.gitlab_webhook(served.gitlab.finish(id, "canceled", "", served.now_ms()));
        assert_eq!(state(id)["state"], after, "{name}: {}", state(id));
        cancelled.push(id);
    }
    // Taken back with GitLab refusing to run it again and to remove its runner: said; its end said again finds it
    // already settled.
    let (refused, p) = served.gitlab.pending("refused again", &["superci"], served.now_ms());
    assert!(served.gitlab_webhook(p) < 300);
    let notice = state(refused)["notice"].as_str().unwrap().to_string();
    served.gitlab_webhook(served.gitlab.running(refused, served.now_ms()));
    assert_eq!(say(&format!("interrupted?gl={refused}&t={notice}")), 200);
    served.gitlab.fail("POST", &format!("/jobs/{refused}/retry"), 403);
    served.gitlab.fail("DELETE", "/runners/", 500);
    let ended = served.gitlab.finish(refused, "failed", "ERROR: Job failed", served.now_ms());
    served.gitlab_webhook(ended.clone());
    assert!(state(refused)["error"].as_str().unwrap_or_default().contains("not run again"), "{}", state(refused));
    served.gitlab_webhook(ended);
    served.gitlab.fail_none();
    // Taken back as it passed: done, not run again. (The takings back before forgotten: spot is not paused for it.)
    served.store.0.lock().unwrap().retain(|k, _| !k.starts_with("spot:interrupted:"));
    let (passed, p) = served.gitlab.pending("passed anyway", &["superci"], served.now_ms());
    assert!(served.gitlab_webhook(p) < 300);
    let notice = state(passed)["notice"].as_str().unwrap().to_string();
    served.gitlab_webhook(served.gitlab.running(passed, served.now_ms()));
    assert_eq!(say(&format!("interrupted?gl={passed}&t={notice}")), 200);
    // (Its trace as GitLab writes one: sections, one without a heading, erase-line codes and carriage returns.)
    let trace = "\u{1b}[0KRunning with gitlab-runner 17.5.0\nsection_start:1760000000:prepare_executor\r\u{1b}[0K\u{1b}[0K\u{1b}[36;1mPreparing the executor\u{1b}[0;m\nUsing Docker\nsection_end:1760000001:prepare_executor\r\u{1b}[0K\nsection_start:1760000001:step_script\r\u{1b}[0K\n$ make test\nok\nsection_end:1760000002:step_script\r\u{1b}[0K\n\u{1b}[32;1mJob succeeded\u{1b}[0;m\n";
    served.gitlab_webhook(served.gitlab.finish(passed, "success", trace, served.now_ms()));
    assert_eq!(state(passed)["state"], "done", "{}", state(passed));
    // Run on another job's runner while its own record is over, or its own machine already running it.
    let (host, p) = served.gitlab.pending("its runner", &["superci"], served.now_ms()); served.gitlab_webhook(p);
    let host_runner = state(host)["runner_id"].as_u64().unwrap();
    let (over_own, p) = served.gitlab.pending("over already", &["superci"], served.now_ms()); served.gitlab_webhook(p);
    edit(over_own, &|j| { j["state"] = "cancelled".into(); });
    served.gitlab_webhook(served.gitlab.running_on(over_own, host_runner, served.now_ms()));
    let (host2, p) = served.gitlab.pending("its runner too", &["superci"], served.now_ms()); served.gitlab_webhook(p);
    let host2_runner = state(host2)["runner_id"].as_u64().unwrap();
    let (seen_own, p) = served.gitlab.pending("seen already", &["superci"], served.now_ms()); served.gitlab_webhook(p);
    edit(seen_own, &|j| { j["seen_in_progress"] = true.into(); });
    served.gitlab_webhook(served.gitlab.running_on(seen_own, host2_runner, served.now_ms()));
    // Ended with no runner as other than cancelled (its machine left idle), and cancelled once its machine was seen.
    let (no_runner, p) = served.gitlab.pending("passed elsewhere", &["superci"], served.now_ms()); served.gitlab_webhook(p);
    served.gitlab_webhook(served.gitlab.finish(no_runner, "success", "ok", served.now_ms()));
    assert_eq!(state(no_runner)["state"], "orphan", "{}", state(no_runner));
    let (seen, p) = served.gitlab.pending("seen, then cancelled", &["superci"], served.now_ms()); served.gitlab_webhook(p);
    edit(seen, &|j| { j["seen_in_progress"] = true.into(); });
    served.gitlab_webhook(served.gitlab.finish(seen, "canceled", "", served.now_ms()));
    served.aws.fail_none();
    let home = signed_in(&served);
    let (mut child, link) = dashboard(&home);
    for (id, said) in [(back, "AWS took back its spot machine"), (x, "Status"), (y, "Status"), (z, "Cancelled")] {
        b.goto(&format!("{link}&p=job&id={id}&gl="));
        b.wait(&format!("{} && !document.querySelector('main').innerText.includes('Reading your control plane')", shows("Status")));
        let page = b.eval("document.querySelector('main').innerText").as_str().unwrap_or_default().replace('\n', " ");
        assert!(page.contains(said), "job {id}: no {said:?} ({}):\n{page}", state(id));
    }
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

/// A failing runner (started to fail a job whose label is not understood) that GitHub hands another job instead: that
/// job is said to have been failed by it, keeps its machine, and its machine then takes the first. Each page says so.
#[test]
fn a_failing_runner_that_takes_another_job_is_said_on_its_page_and_that_jobs_machine_takes_the_first() {
    let Some(mut b) = Browser::open(1300, 900) else { eprintln!("no Chrome here: skipped"); return };
    let served = serve(|_| {});
    served.connect_github();
    let state = |id: u64| served.store.under(&format!("job:{id}"))[0].1.clone();
    // A label no machine has: failed at once, with a runner started to say so on GitHub.
    let (first, q) = served.github.queue("first", "superci/cpus=8", served.now_ms()); served.webhook(q);
    let failing = format!("superci-{PLANE}-{first}-x");
    assert!(served.store.0.lock().unwrap().contains_key(&format!("runner:{failing}")), "{:?}", served.store.0.lock().unwrap().keys().filter(|k| k.starts_with("runner:")).collect::<Vec<_>>());
    // Another job with a label of SuperCI's: its machine; the failing runner takes it first, and fails it.
    let (second, q) = served.github.queue("second", "superci", served.now_ms()); served.webhook(q);
    served.webhook(served.github.start_on(second, &failing));
    served.webhook(served.github.finish(second, "failure", "2026-10-15T10:00:01.0000000Z Error: the label is not understood", served.now_ms()));
    assert!(state(second)["error"].as_str().unwrap_or_default().starts_with("failed by the runner started to fail first"), "{}", state(second));
    // Its machine then takes the first job, still queued at GitHub.
    let runner = state(second)["runner"].as_str().unwrap().to_string();
    served.webhook(served.github.start_on(first, &runner));
    let home = signed_in(&served);
    let (mut child, link) = dashboard(&home);
    for (id, said) in [(first, "Running"), (second, "Failed")] {
        b.goto(&format!("{link}&p=job&id={id}"));
        b.wait(&format!("{} && !document.querySelector('main').innerText.includes('Reading your control plane')", shows("Status")));
        let page = b.eval("document.querySelector('main').innerText").as_str().unwrap_or_default().replace('\n', " ");
        assert!(page.contains(said), "job {id}: no {said:?} ({}):\n{page}", state(id));
    }
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

/// What the dashboard finds in the AWS account it is signed in to, by the roles SuperCI made there (as IAM lists them):
/// another CI's control plane, listed apart; a control plane elsewhere whose machines run here, only seen; and the
/// workspace's own one deleted in AWS meanwhile, dropped from the workspace.
#[test]
fn control_planes_found_in_aws_are_listed_apart_seen_or_dropped_when_deleted_there() {
    let Some(mut b) = Browser::open(1300, 1000) else { eprintln!("no Chrome here: skipped"); return };
    let role = |name: &str, description: &str| format!("<member><Path>/</Path><RoleName>{name}</RoleName><RoleId>AROAEXAMPLE{:09}</RoleId><Arn>arn:aws:iam::111111111111:role/{name}</Arn><CreateDate>2026-10-18T10:00:00Z</CreateDate><Description>{description}</Description><MaxSessionDuration>3600</MaxSessionDuration></member>", name.len());
    let roles = |members: &[String]| format!("<ListRolesResponse xmlns=\"https://iam.amazonaws.com/doc/2010-05-08/\"><ListRolesResult><IsTruncated>false</IsTruncated><Roles>{}</Roles></ListRolesResult><ResponseMetadata><RequestId>00000000-0000-4000-8000-000000000001</RequestId></ResponseMetadata></ListRolesResponse>", members.concat());
    // Its own, another CI's beside it, and the role a control plane on Cloudflare runs its AWS machines with.
    let served = serve(|_| {});
    let home = signed_in(&served);
    served.aws.fail("ListRoles", 200, &roles(&[role(&format!("superci-plane-{PLANE}"), &format!("Runs the SuperCI control plane {} (Lambda, us-east-1, runs-on superci).", served.url)),
        role("superci-plane-otherci000001", "Runs the SuperCI control plane https://otherci.example (Lambda, us-east-1, runs-on otherci)."),
        role("superci-plane-elsewhere0001", "Assumed by the SuperCI control plane https://superci.acme.workers.dev (OpenID Connect) to run GitHub Actions jobs.")]), None);
    let (mut child, link) = dashboard(&home);
    b.goto(&format!("{link}&p=planes"));
    b.wait(&format!("{} && !document.querySelector('main').innerText.includes('Reading your control plane')", shows("Control plane")));
    let page = b.eval("document.querySelector('main').innerText").as_str().unwrap_or_default().replace('\n', " ");
    assert!(page.contains("AWS · 111111111111") && page.contains("otherci000001"), "{page}");
    let _ = child.kill();
    // Deleted in AWS meanwhile: no role of its any more. Dropped from the workspace; the page starts anew.
    served.aws.fail_none();
    served.aws.fail("ListRoles", 200, &roles(&[]), None);
    let (mut child, link) = dashboard(&home);
    b.goto(&link);
    b.wait("document.querySelector('main') && !document.querySelector('main').innerText.includes('Reading your control plane')");
    let page = b.eval("document.querySelector('main').innerText").as_str().unwrap_or_default().replace('\n', " ");
    assert!(!page.contains("AWS · 111111111111 · us-east-1"), "{page}");
    let kept = std::fs::read_to_string(home.join("workspaces/default/sign-ins.json")).unwrap_or_default();
    assert!(!kept.contains("\"plane\":{\"Aws\""), "dropped from the workspace: {kept}");
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

/// The Permissions card: what AWS refused the control plane (noted by it) while this computer is not signed in to that
/// account, and a GitHub installation that has not accepted the App's newer permissions, each with what to do.
#[test]
fn the_permissions_card_says_what_aws_refused_and_what_an_installation_has_not_accepted() {
    let Some(mut b) = Browser::open(1300, 1000) else { eprintln!("no Chrome here: skipped"); return };
    let served = serve(|c| {
        (c.aws_own, c.aws_own_creds) = (Some(("111111111111".into(), "us-east-1".into())), Some(superci_core::aws::Credentials { access_key_id: "ASIAX".into(), secret_access_key: "secret".into(), session_token: Some("tok".into()), expires_at_ms: u64::MAX }));
    });
    served.connect_github();
    served.github.set_older_install(true);
    // AWS refused the control plane one of its permissions (as it noted, with what AWS said).
    served.store.0.lock().unwrap().insert("denied:aws:AskSpotPoolsAtOnce".into(), serde_json::json!({ "at_ms": served.now_ms(), "said": "UnauthorizedOperation: You are not authorized to perform: ec2:CreateLaunchTemplate" }).to_string());
    // This computer has the control plane's key only: no AWS sign-in.
    let home = common::home(&served);
    let (mut child, link) = dashboard(&home);
    b.goto(&format!("{link}&p=planes"));
    b.wait(&format!("{} && {}", shows("Permissions"), shows("AWS refused 1 permission")));
    b.wait(&shows("has 1 permission to accept"));
    // Each opens what to do: sign in with AWS, coming back to allow it; accept it on GitHub.
    b.eval("document.querySelector('button[onclick*=\"perm-aws\"]').click()");
    b.wait("document.getElementById('perm-aws').open && document.getElementById('perm-aws').innerText.includes('Sign in with AWS')");
    b.eval("document.getElementById('perm-aws').close()");
    b.eval("document.querySelector('button[onclick*=\"perm-gh-\"]').click()");
    b.wait("[...document.querySelectorAll('dialog')].some(d => d.open && d.innerText.includes('Review request'))");
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

/// What a computer may send of a working tree for a run, as `superci runs create` sends it (with a run key): a tree's
/// id and part said well or refused, a part over 5 MB refused, a base chain too long, a base the bucket no longer has.
/// No run comes of any, as the Runs page says.
#[test]
fn a_working_trees_parts_are_taken_or_refused_and_no_run_comes_of_them() {
    let Some(mut b) = Browser::open(1300, 900) else { eprintln!("no Chrome here: skipped"); return };
    let agent = ureq::Agent::config_builder().http_status_as_error(false).build().new_agent();
    let send = |served: &superci_testkit::Served, q: &str, body: Vec<u8>| { let mut r = agent.post(&format!("{}/runs/tree?{q}", served.url)).header("authorization", &format!("Bearer {}", served.run_key)).send(body).unwrap(); (r.status().as_u16(), r.body_mut().read_to_string().unwrap_or_default()) };
    let tree = "tree-Ab_cd-0123456789xyz";
    // No bucket: kept in pieces through the control plane.
    let served = serve(|_| {});
    for (q, status) in [("id=short&part=0", 400), ("id=tree-has!a-char-it-may-not&part=0", 400), (&format!("id={tree}&part=1000")[..], 400), ("part=0", 400)] {
        assert_eq!(send(&served, q, vec![]).0, status, "{q}");
    }
    assert_eq!(send(&served, &format!("id={tree}&part=0"), vec![b'x'; 5_000_001]).0, 413);
    let (status, said) = send(&served, &format!("id={tree}&part=0"), b"a part".to_vec());
    assert!(status == 200 && said.contains("\"stored\":6"), "{status} {said}");
    let (status, said) = send(&served, &format!("id={tree}&part=0&link=1"), vec![]);
    assert!(status == 200 && said.contains("\"url\":null"), "no bucket, no link: {status} {said}");
    let (status, said) = send(&served, &format!("id={tree}&part=0&sent=1"), vec![]);
    assert!(status == 200 && said.contains("\"sent\":true"), "{status} {said}");
    let home = signed_in(&served);
    let (mut child, link) = dashboard(&home);
    b.goto(&format!("{link}&p=runs"));
    b.wait("document.querySelector('main') && !document.querySelector('main').innerText.includes('Reading your control plane')");
    assert!(served.store.under("run:").is_empty());
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
    // Its own AWS account's bucket: links to put a tree there; a base chain too long, or a base it no longer has.
    let served = serve(|c| {
        (c.aws_own, c.aws_own_creds) = (Some(("111111111111".into(), "us-east-1".into())), Some(superci_core::aws::Credentials { access_key_id: "ASIAX".into(), secret_access_key: "secret".into(), session_token: Some("tok".into()), expires_at_ms: u64::MAX }));
    });
    // (Its bucket ready: its cache program is there.)
    let bucket = superci_core::cache::bucket(PLANE, "us-east-1");
    served.github.put_object(&format!("{bucket}/{}", superci_core::cache::program_key("x86_64")), b"program");
    let (status, said) = send(&served, &format!("id={tree}&part=0&link=1&later=1&keep=b0123456789abcdef"), vec![]);
    assert!(status == 200 && said.contains("\"url\":\"https://") && said.contains("\"later\":true"), "{status} {said}");
    let chain = (0..17).map(|i| format!("layer{i:04}")).collect::<Vec<_>>().join(",");
    assert_eq!(send(&served, &format!("id={tree}&part=0&link=1&layer=l0123456789abcdef&base={chain}"), vec![]).0, 400);
    let (status, said) = send(&served, &format!("id={tree}&part=0&link=1&layer=l0123456789abcdef&base=bgone0123456789"), vec![]);
    assert!(status == 200 && said.contains("\"base\":false"), "a base the bucket no longer has: {status} {said}");
    let (status, said) = send(&served, &format!("id={tree}&part=0&link=1&base=bgone0123456789"), vec![]);
    assert!(status == 200 && said.contains("\"base\":false"), "{status} {said}");
    // A chain with a name that is none; a layer named as none (taken as no layer); bases all there: the layer's link.
    assert_eq!(send(&served, &format!("id={tree}&part=0&link=1&layer=l0123456789abcdef&base=b0123456789abcdef,no!"), vec![]).0, 400);
    let (status, _) = send(&served, &format!("id={tree}&part=0&link=1&layer=short&base=b0123456789abcdef"), vec![]);
    assert!(status < 500, "{status}");
    let bucket = superci_core::cache::bucket(PLANE, "us-east-1");
    for name in ["b0123456789abcdef", "l1111111111111111"] { served.github.put_object(&format!("{bucket}/bases/{name}.tar.gz"), b"tree") }
    let (status, said) = send(&served, &format!("id={tree}&part=0&link=1&layer=l2222222222222222&base=b0123456789abcdef,l1111111111111111"), vec![]);
    assert!(status == 200 && said.contains("\"base\":true") && said.contains("\"parts\":3"), "{status} {said}");
    let (status, said) = send(&served, &format!("id={tree}&part=0&link=1&base=b0123456789abcdef"), vec![]);
    assert!(status == 200 && said.contains("\"parts\":2"), "{status} {said}");
    assert!(served.store.under("run:").is_empty());
}

/// A GitLab job's cache on AWS: its machine keeps GitLab's cache through SuperCI's, refused until its job began (which
/// job a runner takes is GitLab's to say), then kept as its project's by its branch; Settings lists it by the project.
#[test]
fn a_gitlab_jobs_cache_is_kept_as_its_projects_once_its_job_began_and_settings_lists_it() {
    let Some(mut b) = Browser::open(1300, 1000) else { eprintln!("no Chrome here: skipped"); return };
    let served = serve(|c| {
        (c.aws_own, c.aws_own_creds) = (Some(("111111111111".into(), "us-east-1".into())), Some(superci_core::aws::Credentials { access_key_id: "ASIAX".into(), secret_access_key: "secret".into(), session_token: Some("tok".into()), expires_at_ms: u64::MAX }));
    });
    served.connect_gitlab();
    // (Behind its public address: machines reach the cache at it.)
    *served.public.lock().unwrap() = Some("https://plane.acme.test".into());
    served.aws.put(&format!("/superci/{PLANE}/ROUTING"), r#"{"default":null,"rules":[],"order":[{"cloud":"aws"},{"cloud":"aws-on-demand"},{"cloud":"cloudflare","off":true}]}"#);
    served.aws.replaying(include_str!("../fixtures/aws/launch/spot.jsonl"), &[]);
    let started: String = include_str!("../fixtures/aws/launch/on-demand.jsonl").lines().filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .find(|v| v["asked"].as_str().unwrap_or_default().contains("Action=RunInstances") && v["status"] == 200).map(|v| v["answer"].as_str().unwrap_or_default().to_string()).unwrap();
    served.store.0.lock().unwrap().insert("aws:no-fleet".into(), served.now_ms().to_string());
    served.aws.fail("RunInstances", 200, &started, None);
    // Its bucket ready (its cache program there).
    let bucket = superci_core::cache::bucket(PLANE, "us-east-1");
    served.github.put_object(&format!("{bucket}/{}", superci_core::cache::program_key("x86_64")), b"program");
    let (id, p) = served.gitlab.pending("test", &["superci"], served.now_ms());
    assert!(served.gitlab_webhook(p) < 300);
    // Its machine's start script: GitLab's runner keeps its cache at superci-cache, with a link of its own.
    let asked = served.aws.asked.lock().unwrap().iter().rev().find(|a| a["asked"].as_str().unwrap_or_default().contains("Action=RunInstances")).map(|a| a["asked"].as_str().unwrap().to_string()).unwrap();
    let data: String = url::form_urlencoded::parse(asked.as_bytes()).find(|(k, _)| k == "UserData").map(|(_, v)| v.to_string()).unwrap();
    use base64::Engine;
    let script = String::from_utf8(base64::engine::general_purpose::STANDARD.decode(data).unwrap()).unwrap();
    assert!(script.contains("CACHE_TYPE=s3"), "GitLab's runner keeps its cache with SuperCI's: {}", script.lines().filter(|l| l.to_lowercase().contains("cache")).collect::<Vec<_>>().join("\n"));
    let link = script.split("/cache/").nth(1).unwrap().split('\'').next().unwrap().to_string();
    let agent = ureq::Agent::config_builder().http_status_as_error(false).build().new_agent();
    let call = |what: &str, body: serde_json::Value| { let mut r = agent.post(&format!("{}/cache/{link}/{what}", served.url)).send(body.to_string()).unwrap(); (r.status().as_u16(), serde_json::from_str::<serde_json::Value>(&r.body_mut().read_to_string().unwrap_or_default()).unwrap_or_default()) };
    let key = "gl/project/250833/default-non_protected/cache.zip";
    assert_eq!(call("tool-begin", serde_json::json!({ "tool": "gitlab", "hash": key })).0, 403, "before its job began");
    served.gitlab_webhook(served.gitlab.running(id, served.now_ms()));
    let (status, begun) = call("tool-begin", serde_json::json!({ "tool": "gitlab", "hash": key }));
    assert_eq!(status, 200, "{begun}");
    assert_eq!(call("tool-sign", serde_json::json!({ "tool": "gitlab", "hash": key, "names": ["p00001"] })).0, 200);
    assert_eq!(call("tool-finish", serde_json::json!({ "tool": "gitlab", "hash": key, "pieces": [["p00000", 64], ["p00001", 6]] })).1["size"], 70);
    assert_eq!(call("tool-get", serde_json::json!({ "tool": "gitlab", "hash": key })).1["pieces"].as_array().map(Vec::len), Some(2));
    // Another job whose running event never came: GitLab is asked which job its runner runs, and its cache is open.
    let (id2, p) = served.gitlab.pending("test 2", &["superci"], served.now_ms());
    assert!(served.gitlab_webhook(p) < 300);
    let asked = served.aws.asked.lock().unwrap().iter().rev().find(|a| a["asked"].as_str().unwrap_or_default().contains("Action=RunInstances")).map(|a| a["asked"].as_str().unwrap().to_string()).unwrap();
    let data: String = url::form_urlencoded::parse(asked.as_bytes()).find(|(k, _)| k == "UserData").map(|(_, v)| v.to_string()).unwrap();
    let script2 = String::from_utf8(base64::engine::general_purpose::STANDARD.decode(data).unwrap()).unwrap();
    let link2 = script2.split("/cache/").nth(1).unwrap().split('\'').next().unwrap().to_string();
    let runner2 = served.store.under(&format!("job:gl{id2}"))[0].1["runner_id"].as_u64().unwrap();
    let _ = served.gitlab.running_on(id2, runner2, served.now_ms());
    let mut r = agent.post(&format!("{}/cache/{link2}/tool-begin", served.url)).send(serde_json::json!({ "tool": "gitlab", "hash": "gl/project/250833/other/cache.zip" }).to_string()).unwrap();
    assert_eq!(r.status().as_u16(), 200, "{}", r.body_mut().read_to_string().unwrap_or_default());
    // A link that is no machine's: nothing.
    let (status, _) = { let mut r = agent.post(&format!("{}/cache/not-a-machines-link/tool-get", served.url)).send("{}").unwrap(); (r.status().as_u16(), r.body_mut().read_to_string().unwrap_or_default()) };
    assert!(status >= 400, "{status}");
    let home = signed_in(&served);
    let (mut child, link) = dashboard(&home);
    b.goto(&format!("{link}&p=settings"));
    b.wait(&format!("{} && {}", shows("Cache"), shows("acme/app")));
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

/// Each kind of machine's start script with the cache in it, its region's bucket ready: Linux (and Docker's images kept
/// when the store's program is there too), arm64, Windows; none with the cache turned off, and no images kept with
/// them turned off. Each job's page then says how it stands.
#[test]
fn each_kind_of_machine_gets_the_cache_in_its_start_script_unless_it_is_turned_off() {
    let Some(mut b) = Browser::open(1300, 1000) else { eprintln!("no Chrome here: skipped"); return };
    let served = serve(|c| {
        (c.aws_own, c.aws_own_creds) = (Some(("111111111111".into(), "us-east-1".into())), Some(superci_core::aws::Credentials { access_key_id: "ASIAX".into(), secret_access_key: "secret".into(), session_token: Some("tok".into()), expires_at_ms: u64::MAX }));
    });
    served.connect_github();
    *served.public.lock().unwrap() = Some("https://plane.acme.test".into());
    let order = r#"{"order":[{"cloud":"aws","max_jobs":100},{"cloud":"aws-on-demand"},{"cloud":"cloudflare","off":true}]"#;
    served.aws.put(&format!("/superci/{PLANE}/ROUTING"), &format!("{order}}}"));
    served.aws.replaying(include_str!("../fixtures/aws/launch/spot.jsonl"), &[]);
    let started: String = include_str!("../fixtures/aws/launch/on-demand.jsonl").lines().filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .find(|v| v["asked"].as_str().unwrap_or_default().contains("Action=RunInstances") && v["status"] == 200).map(|v| v["answer"].as_str().unwrap_or_default().to_string()).unwrap();
    served.store.0.lock().unwrap().insert("aws:no-fleet".into(), served.now_ms().to_string());
    served.aws.fail("RunInstances", 200, &started, None);
    let bucket = superci_core::cache::bucket(PLANE, "us-east-1");
    for arch in ["x86_64", "aarch64", "x86_64-windows"] { served.github.put_object(&format!("{bucket}/{}", superci_core::cache::program_key(arch)), b"program") }
    served.github.put_object(&format!("{bucket}/{}", superci_core::cache::layers_key("x86_64")), b"program");
    // The start script AWS was last given.
    let script = || {
        let asked = served.aws.asked.lock().unwrap().iter().rev().find(|a| a["asked"].as_str().unwrap_or_default().contains("Action=RunInstances")).map(|a| a["asked"].as_str().unwrap().to_string()).unwrap();
        let data: String = url::form_urlencoded::parse(asked.as_bytes()).find(|(k, _)| k == "UserData").map(|(_, v)| v.to_string()).unwrap();
        use base64::Engine;
        String::from_utf8_lossy(&base64::engine::general_purpose::STANDARD.decode(data).unwrap()).to_string()
    };
    let job = |label: &str| { let (id, q) = served.github.queue(label, label, served.now_ms()); assert!(served.webhook(q) < 300); id };
    let mut ids = vec![];
    let runs = || served.aws.asked.lock().unwrap().iter().filter(|a| a["asked"].as_str().unwrap_or_default().contains("Action=RunInstances")).count();
    for label in ["superci", "superci/arch=arm64", "superci/os=windows", "superci/image=ubuntu26/docker-cache=true", "superci/image=ubuntu26/docker-cache=true/scratch=ramdisk"] {
        let before = runs();
        let id = job(label);
        assert!(runs() > before, "{label}: no machine asked for: {}", served.store.under(&format!("job:{id}"))[0].1);
        let s = script();
        assert!(s.contains("plane.acme.test/cache/"), "{label}: {}", s.lines().filter(|l| l.contains("cache")).collect::<Vec<_>>().join("\n"));
        ids.push(id);
    }
    assert!(served.store.under("job:").iter().any(|(_, j)| j["layers"] == true), "Docker's images kept from the start for one: {:?}", served.store.under("job:").iter().map(|(_, j)| (j["label"].clone(), j["layers"].clone(), j["keeps"].clone())).collect::<Vec<_>>());
    // That machine's own link: the layer store's script, program and index, and what it asks of the store.
    let script_now = script();
    let link = script_now.split("plane.acme.test/cache/").nth(1).unwrap().split(|c: char| c == '\'' || c == '"' || c == '/' || c.is_whitespace()).next().unwrap().to_string();
    let agent = ureq::Agent::config_builder().http_status_as_error(false).build().new_agent();
    for what in ["layers", "layers-program", "layers.json", "cache", "program"] {
        let status = agent.get(&format!("{}/cache/{link}/{what}", served.url)).call().map(|r| r.status().as_u16()).unwrap_or(0);
        assert!(status < 500, "{what}: {status}");
        if what == "cache" { assert_eq!(status, 200, "its link is its machine's") }
    }
    for (what, body) in [("layers-links", r#"{"layers":["sha256:aaaa"]}"#), ("layers-propose", r#"{"layers":[{"digest":"sha256:aaaa","size":10}]}"#), ("layers-uploaded", r#"{"layers":["sha256:aaaa"]}"#),
        ("layers-kept", r#"{"served":1,"served_bytes":10,"pulled":0,"pulled_bytes":0,"uploaded":0,"uploaded_bytes":0}"#), ("tool-get", r#"{"tool":"turbo","hash":"abc"}"#)] {
        let status = agent.post(&format!("{}/cache/{link}/{what}", served.url)).send(body).map(|r| r.status().as_u16()).unwrap_or(0);
        assert!(status < 500, "{what}: {status}");
    }
    // Docker's images not kept: the cache still there.
    served.aws.put(&format!("/superci/{PLANE}/ROUTING"), &format!(r#"{order},"cache":{{"images_off":true}}}}"#));
    ids.push(job("superci"));
    assert!(script().contains("plane.acme.test/cache/"));
    // The cache turned off: none in its script.
    served.aws.put(&format!("/superci/{PLANE}/ROUTING"), &format!(r#"{order},"cache":{{"off":true}}}}"#));
    ids.push(job("superci"));
    assert!(!script().contains("plane.acme.test/cache/"), "turned off");
    served.aws.fail_none();
    let home = signed_in(&served);
    let (mut child, link) = dashboard(&home);
    for id in ids {
        b.goto(&format!("{link}&p=job&id={id}"));
        b.wait(&format!("{} && !document.querySelector('main').innerText.includes('Reading your control plane')", shows("Status")));
    }
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

/// AWS's runners connected to a control plane outside AWS, as the stack the dashboard opens reports back: a forged
/// report, one for another role, account or region refused; the right one taken once. Runners then shows AWS.
#[test]
fn aws_runners_are_connected_once_by_the_stacks_report_and_runners_shows_them() {
    let Some(mut b) = Browser::open(1300, 900) else { eprintln!("no Chrome here: skipped"); return };
    let served = serve(|c| { c.aws_connect_token = Some("connect-token-for-tests-0001".into()); c.aws_region = Some("us-east-1".into()); });
    // AWS's answer to the control plane taking its role (as STS documents it).
    served.aws.fail("AssumeRoleWithWebIdentity", 200, "<AssumeRoleWithWebIdentityResponse xmlns=\"https://sts.amazonaws.com/doc/2011-06-15/\"><AssumeRoleWithWebIdentityResult><Credentials><AccessKeyId>ASIAEXAMPLE000000001</AccessKeyId><SecretAccessKey>secret-for-tests</SecretAccessKey><SessionToken>token-for-tests</SessionToken><Expiration>2099-01-01T00:00:00Z</Expiration></Credentials><SubjectFromWebIdentityToken>plane:testplane0001</SubjectFromWebIdentityToken><AssumedRoleUser><Arn>arn:aws:sts::123456789012:assumed-role/superci-plane-testplane0001/superci</Arn><AssumedRoleId>AROAEXAMPLE:superci</AssumedRoleId></AssumedRoleUser></AssumeRoleWithWebIdentityResult><ResponseMetadata><RequestId>00000000-0000-4000-8000-000000000001</RequestId></ResponseMetadata></AssumeRoleWithWebIdentityResponse>", None);
    let agent = ureq::Agent::config_builder().http_status_as_error(false).build().new_agent();
    let report = |token: &str, account: &str, region: &str, role: &str| agent.post(&format!("{}/aws/callback", served.url)).header("content-type", "application/json")
        .send(serde_json::json!({ "token": token, "accountId": account, "region": region, "roleArn": role }).to_string()).unwrap().status().as_u16();
    let role = |account: &str| format!("arn:aws:iam::{account}:role/superci-plane-{PLANE}");
    assert_eq!(report("forged-token", "123456789012", "us-east-1", &role("123456789012")), 403);
    assert_eq!(report("connect-token-for-tests-0001", "123456789012", "us-east-1", "arn:aws:iam::123456789012:role/someone-elses"), 400);
    assert_eq!(report("connect-token-for-tests-0001", "12345678901", "us-east-1", &role("12345678901")), 400);
    assert_eq!(report("connect-token-for-tests-0001", "123456789012", "eu-west-1", &role("123456789012")), 400);
    assert_eq!(report("connect-token-for-tests-0001", "123456789012", "us-east-1", &role("123456789012")), 200);
    assert_eq!(report("connect-token-for-tests-0001", "123456789012", "us-east-1", &role("123456789012")), 409, "the token works once");
    let home = common::home(&served);
    let (mut child, link) = dashboard(&home);
    b.goto(&format!("{link}&p=runners"));
    b.wait(&format!("{} && {}", shows("Runner providers"), shows("AWS")));
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

/// A finished Cloudflare job's cost as Cloudflare measured it (its usage figures, as its GraphQL API answers them):
/// read by the dashboard once Cloudflare has them, handed to the control plane, and the job settled at that.
#[test]
fn a_finished_cloudflare_jobs_cost_is_settled_at_what_cloudflare_measured() {
    let Some(mut b) = Browser::open(1300, 900) else { eprintln!("no Chrome here: skipped"); return };
    let served = serve(|_| {});
    served.connect_github();
    // A job that ran a minute on Cloudflare's containers and ended five minutes ago.
    let (id, q) = served.github.queue("tests", "superci", served.now_ms()); served.webhook(q);
    served.webhook(served.github.start(id));
    served.webhook(served.github.finish(id, "success", "2026-10-15T10:00:01.0000000Z ok", served.now_ms()));
    let container = "a".repeat(64);
    { let now = served.now_ms(); let mut m = served.store.0.lock().unwrap(); let k = format!("job:{id}"); let mut j: serde_json::Value = serde_json::from_str(&m[&k]).unwrap();
      j["machine_id"] = container.clone().into(); j["started_ms"] = (now - 360_000).into(); j["ended_ms"] = (now - 300_000).into(); m.insert(k, j.to_string()); }
    // Signed in to Cloudflare: its account, no control plane of SuperCI's there, and its usage figures for the job.
    let home = signed_in(&served);
    let file = home.join("workspaces/default/sign-ins.json");
    let mut kept: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    kept["cloudflare"] = serde_json::json!({ "access": "cloudflare-access-for-tests", "expires_at_ms": 4102444800000u64, "refresh": "cloudflare-refresh-for-tests" });
    std::fs::write(&file, kept.to_string()).unwrap();
    served.aws.fail("/client/v4/accounts?", 200, r#"{"result":[{"id":"1111111111111111111111111111111a","name":"Acme"}],"success":true,"errors":[],"messages":[]}"#, None);
    served.aws.fail("/workers/scripts", 200, r#"{"result":[],"success":true,"errors":[],"messages":[]}"#, None);
    served.aws.fail("/workers/subdomain", 200, r#"{"result":{"subdomain":"acme"},"success":true,"errors":[],"messages":[]}"#, None);
    served.aws.fail("/graphql", 200, &serde_json::json!({ "data": { "viewer": { "accounts": [{ "g": [{ "dimensions": { "instanceId": container }, "sum": { "cpuTimeSec": 45.0, "allocatedMemory": 1.0e12, "allocatedDisk": 2.0e12, "txBytes": 1.0e8 } }] }] } }, "errors": null }).to_string(), None);
    let (mut child, link) = dashboard(&home);
    b.goto(&format!("{link}&p=overview"));
    b.wait(&shows("Usage"));
    until("its cost settled at what Cloudflare measured", || served.store.under(&format!("job:{id}"))[0].1["cost_from"] == "measured");
    b.goto(&format!("{link}&p=job&id={id}"));
    b.wait(&format!("{} && {}", shows("Status"), shows("Passed")));
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

/// Control planes of each kind deployed, given runners, moved and deleted from the dashboard, as Cloudflare and Modal
/// answered a real account: a Cloudflare one with its containers and a Modal launcher, everything moved to a Modal
/// one, Cloudflare's launcher taken out there, both deleted. Each step read on its page.
#[test]
fn control_planes_of_each_kind_are_deployed_moved_and_deleted_from_the_dashboard_as_their_clouds_answered() {
    let Some(mut b) = Browser::open(1300, 1000) else { eprintln!("no Chrome here: skipped"); return };
    let served = serve(|_| {});
    served.connect_github();
    let aws = served.aws.clone();
    let cf = superci_testkit::serve_as("cfplane00002", move |c| {
        let settings = aws.worker_settings("superci");
        c.read_settings(&settings);
        if let Some((_, on)) = settings.iter().find(|(k, _)| k == "CONTAINERS") { c.containers = on == "on" }
    });
    cf.aws.params.lock().unwrap().remove("/superci/cfplane00002/CONTAINERS");
    let aws = served.aws.clone();
    let md = superci_testkit::serve_as("mdplane00002", move |c| {
        let settings = aws.modal_plane_settings();
        c.read_settings(&settings);
        c.own_cloud = "modal".into();
        c.containers = settings.iter().any(|(k, v)| k == "CONTAINERS" && v == "on");
    });
    md.aws.params.lock().unwrap().remove("/superci/mdplane00002/CONTAINERS");
    served.aws.workers.lock().unwrap().insert("superci.acme.workers.dev".into(), cf.url.clone());
    served.aws.workers.lock().unwrap().insert("acmeorg--superci-plane-mdplane00002.modal.run".into(), md.url.clone());
    served.aws.workers.lock().unwrap().insert("acmeorg--superci-runners-cfplane00002.modal.run".into(), md.url.clone());
    let home = signed_in(&served);
    let file = home.join("workspaces/default/sign-ins.json");
    let mut kept: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    kept["cloudflare"] = serde_json::json!({ "access": "cloudflare-access-for-tests", "expires_at_ms": 4102444800000u64, "refresh": "cloudflare-refresh-for-tests" });
    kept["modal"] = serde_json::json!({ "token_id": "ak-modal-for-tests", "token_secret": "as-modal-for-tests", "workspace": "acmeorg" });
    std::fs::write(&file, kept.to_string()).unwrap();
    let step = |name: &str| served.aws.replaying(&std::fs::read_to_string(format!("{}/fixtures/move/{name}.jsonl", env!("CARGO_MANIFEST_DIR"))).unwrap(), &[]);
    // A dashboard for each step (a deploy names its new control plane as it starts), its form posted from its page.
    let open = |b: &mut Browser, id: &str| { let (child, link) = common::dashboard_with(&home, &[("SUPERCI_NEW_PLANE_ID", id), ("SUPERCI_VIEWS_S", "0")]); b.goto(&link); b.goto(&format!("{}/job/view?id=0", link.split("/?").next().unwrap())); (child, link) };
    let until_page = |b: &mut Browser, link: &str, page: &str, text: &str| {
        let end = std::time::Instant::now() + std::time::Duration::from_secs(90);
        loop {
            b.goto(&format!("{link}&{page}"));
            b.wait("document.querySelector('main') && !document.querySelector('main').innerText.includes('Reading your control plane')");
            let t = b.eval("document.body.innerText").as_str().unwrap_or_default().to_string();
            if t.contains(text) { return }
            assert!(std::time::Instant::now() < end, "{page}: no {text:?}:\n{t}");
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
    };
    // Deployed: a Cloudflare one in its account, then a Modal one.
    step("01-deploy-cf");
    let (child, link) = open(&mut b, "cfplane00002");
    let (got, text) = post(&mut b, "/plane/cloudflare", &[("account", "1111111111111111111111111111111a")]);
    assert!(got < 400, "{got} {text}");
    until_page(&mut b, &link, "p=planes", "superci.acme.workers.dev");
    drop(child);
    step("02-deploy-modal");
    let (child, link) = open(&mut b, "mdplane00002");
    let (got, text) = post(&mut b, "/plane/modal", &[]);
    assert!(got < 400, "{got} {text}");
    until_page(&mut b, &link, "p=planes", "acmeorg--superci-plane-mdplane00002");
    drop(child);
    // The Cloudflare one: its own containers, and Modal's sandboxes through a launcher deployed in Modal.
    step("03-cf-containers");
    let (child, link) = open(&mut b, "");
    let (got, text) = post(&mut b, "/runners/cloudflare?c=cfplane00002", &[]);
    assert!(got < 400, "{got} {text}");
    step("04-cf-modal-agent");
    let (got, text) = post(&mut b, "/runners/modal?c=cfplane00002", &[]);
    assert!(got < 400, "{got} {text}");
    until_page(&mut b, &link, "c=cfplane00002&p=runners", "Modal");
    drop(child);
    // Everything moved to the Modal one; the Cloudflare one says where it went.
    step("06-dashboard");
    step("07-move");
    let (child, link) = open(&mut b, "");
    let (got, text) = post(&mut b, "/plane/move?c=cfplane00002", &[("plane", "mdplane00002")]);
    assert!(got < 400, "{got} {text}");
    until("moved", || cf.store.under("moved_to").len() == 1);
    step("08-after-move");
    until_page(&mut b, &link, "c=mdplane00002&p=runners", "Cloudflare");
    drop(child);
    // Cloudflare's launcher taken out of the Modal one; then both deleted.
    step("09-md-remove-cf");
    let (child, link) = open(&mut b, "");
    let (got, text) = post(&mut b, "/runners/remove?c=mdplane00002", &[("cloud", "cloudflare")]);
    assert!(got < 400, "{got} {text}");
    step("10-delete-cf");
    let (got, text) = post(&mut b, "/plane/delete?c=mdplane00002", &[("plane", "cfplane00002")]);
    assert!(got < 400, "{got} {text}");
    until_page(&mut b, &link, "c=mdplane00002&p=planes", "Deleted");
    drop(child);
    let _ = std::fs::remove_dir_all(&home);
}

/// A Cloudflare control plane's containers set from the Runners form (where they start, GitHub's full image, how many
/// at once), then it and a Modal one updated from the dashboard, as Cloudflare and Modal answered a real account.
#[test]
fn cloudflare_containers_are_set_and_cloudflare_and_modal_control_planes_updated_from_the_dashboard() {
    let Some(mut b) = Browser::open(1300, 1000) else { eprintln!("no Chrome here: skipped"); return };
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
    let step = |name: &str| served.aws.replaying(&std::fs::read_to_string(format!("{}/fixtures/update/{name}.jsonl", env!("CARGO_MANIFEST_DIR"))).unwrap(), &[]);
    let open = |b: &mut Browser, id: &str| { let (child, link) = common::dashboard_with(&home, &[("SUPERCI_NEW_PLANE_ID", id), ("SUPERCI_VIEWS_S", "0")]); b.goto(&link); b.goto(&format!("{}/job/view?id=0", link.split("/?").next().unwrap())); (child, link) };
    let until_page = |b: &mut Browser, link: &str, page: &str, text: &str| {
        let end = std::time::Instant::now() + std::time::Duration::from_secs(90);
        loop {
            b.goto(&format!("{link}&{page}"));
            b.wait("document.querySelector('main') && !document.querySelector('main').innerText.includes('Reading your control plane')");
            let t = b.eval("document.body.innerText").as_str().unwrap_or_default().to_string();
            if t.contains(text) { return }
            assert!(std::time::Instant::now() < end, "{page}: no {text:?}:\n{t}");
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
    };
    step("01-deploy-cf");
    let (child, link) = open(&mut b, "cfplane00003");
    assert!(post(&mut b, "/plane/cloudflare", &[("account", "1111111111111111111111111111111a")]).0 < 400);
    until_page(&mut b, &link, "p=planes", "superci.acme.workers.dev");
    drop(child);
    step("02-deploy-modal");
    let (child, link) = open(&mut b, "mdplane00003");
    assert!(post(&mut b, "/plane/modal", &[]).0 < 400);
    until_page(&mut b, &link, "p=planes", "acmeorg--superci-plane-mdplane00003");
    drop(child);
    step("03-cf-containers");
    let (child, link) = open(&mut b, "");
    assert!(post(&mut b, "/runners/cloudflare?c=cfplane00003", &[]).0 < 400);
    // Its containers: where they start, GitHub's full image, how many at once (each a secret of its Worker).
    step("04-cf-settings");
    let (got, text) = post(&mut b, "/routing?c=cfplane00003", &[("action", "pool"), ("cloud", "cloudflare"), ("location", "weur"), ("image", "https://images.example.com/full"), ("max", "3")]);
    assert!(got < 400, "{got} {text}");
    let secret = |n: &str| served.aws.worker_settings("superci").into_iter().find(|(k, _)| k == n).map(|(_, v)| v).unwrap_or_default();
    until("its containers' settings", || secret("CF_LOCATION") == "weur" && secret("CF_IMAGE") == "https://images.example.com/full");
    until_page(&mut b, &link, "c=cfplane00003&p=runners", "3 at once");
    // Each updated to this version: the Worker uploaded again; Modal's app deployed again.
    step("05-update-cf");
    assert!(post(&mut b, "/plane/update?c=cfplane00003", &[("plane", "cfplane00003")]).0 < 400);
    until_page(&mut b, &link, "c=cfplane00003&p=planes", "Updated to");
    // Its secrets, kept as its Worker's: 4.8 KB each at most, 12 at most (it is handed them with each run).
    b.goto(&format!("{}/job/view?id=0", link.split("/?").next().unwrap()));
    // (Under a secret's 4 KB, but past Cloudflare's 5 KB once written: each quote is two characters there.)
    let (got, text) = post(&mut b, "/secrets/add?c=cfplane00003", &[("name", "TOO_BIG"), ("value", &"\"".repeat(3000))]);
    assert!(got == 409 && text.contains("about 4.8 KB at most"), "{got} {text}");
    for n in 0..12 { let (got, text) = post(&mut b, "/secrets/add?c=cfplane00003", &[("name", &format!("SECRET_{n}")), ("value", "v")]); assert!(got < 400, "{n}: {got} {text}") }
    let (got, text) = post(&mut b, "/secrets/add?c=cfplane00003", &[("name", "ONE_MORE"), ("value", "v")]);
    assert!(got == 409 && text.contains("12 secrets at most"), "{got} {text}");
    let (got, text) = post(&mut b, "/secrets/remove?c=cfplane00003", &[("name", "SECRET_0")]);
    assert!(got < 400, "{got} {text}");
    drop(child);
    step("06-update-md");
    let (child, link) = open(&mut b, "");
    assert!(post(&mut b, "/plane/update?c=mdplane00003", &[("plane", "mdplane00003")]).0 < 400);
    until_page(&mut b, &link, "c=mdplane00003&p=planes", "Updated to");
    drop(child);
    let _ = std::fs::remove_dir_all(&home);
}

/// A Modal control plane deployed from the dashboard, given its own sandboxes, read, its sandboxes taken out and the
/// control plane deleted, as Modal answered a real workspace. Each step read on its page.
#[test]
fn a_modal_control_plane_is_deployed_given_sandboxes_and_deleted_from_the_dashboard_as_modal_answered() {
    let Some(mut b) = Browser::open(1300, 1000) else { eprintln!("no Chrome here: skipped"); return };
    let served = serve(|_| {});
    served.connect_github();
    let aws = served.aws.clone();
    let modal = superci_testkit::serve_as("mdplane00001", move |c| {
        let settings = aws.modal_plane_settings();
        c.read_settings(&settings);
        c.own_cloud = "modal".into();
        c.containers = settings.iter().any(|(k, v)| k == "CONTAINERS" && v == "on");
    });
    modal.aws.params.lock().unwrap().remove("/superci/mdplane00001/CONTAINERS");
    served.aws.workers.lock().unwrap().insert("acmeorg--superci-plane-mdplane00001.modal.run".into(), modal.url.clone());
    let home = signed_in(&served);
    let file = home.join("workspaces/default/sign-ins.json");
    let mut kept: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    kept["modal"] = serde_json::json!({ "token_id": "ak-modal-for-tests", "token_secret": "as-modal-for-tests", "workspace": "acmeorg" });
    std::fs::write(&file, kept.to_string()).unwrap();
    let step = |name: &str| served.aws.replaying(&std::fs::read_to_string(format!("{}/fixtures/modal/{name}.jsonl", env!("CARGO_MANIFEST_DIR"))).unwrap(), &[]);
    let (mut child, link) = common::dashboard_with(&home, &[("SUPERCI_NEW_PLANE_ID", "mdplane00001"), ("SUPERCI_VIEWS_S", "0")]);
    b.goto(&link);
    let quiet = |b: &mut Browser| b.goto(&format!("{}/job/view?id=0", link.split("/?").next().unwrap()));
    let until_page = |b: &mut Browser, page: &str, text: &str| {
        let end = std::time::Instant::now() + std::time::Duration::from_secs(90);
        loop {
            b.goto(&format!("{link}&{page}"));
            b.wait("document.querySelector('main') && !document.querySelector('main').innerText.includes('Reading your control plane')");
            let t = b.eval("document.body.innerText").as_str().unwrap_or_default().to_string();
            if t.contains(text) { return }
            assert!(std::time::Instant::now() < end, "{page}: no {text:?}:\n{t}");
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
    };
    // Deployed: its images built in the workspace, `modal deploy` run in a sandbox, answering.
    step("02-deploy");
    quiet(&mut b);
    assert!(post(&mut b, "/plane/modal", &[]).0 < 400);
    until_page(&mut b, "p=planes", "acmeorg--superci-plane-mdplane00001");
    step("03-use");
    until_page(&mut b, "c=mdplane00001&p=planes", "Modal · acmeorg");
    // Its own sandboxes, switched on.
    step("04-sandboxes");
    quiet(&mut b);
    assert!(post(&mut b, "/runners/modal-own?c=mdplane00001", &[]).0 < 400);
    step("05-dashboard");
    until_page(&mut b, "c=mdplane00001&p=runners", "Sandboxes");
    // Taken out, then deleted (its app stopped and its Dicts deleted, by Modal's own client in a sandbox).
    step("06-remove");
    quiet(&mut b);
    let (got, text) = post(&mut b, "/runners/remove?c=mdplane00001", &[("cloud", "modal")]);
    assert!(got < 400, "{got} {text}");
    step("07-delete");
    let _ = child.kill();
    let (child2, link2) = common::dashboard_with(&home, &[("SUPERCI_VIEWS_S", "0")]);
    b.goto(&link2);
    b.goto(&format!("{}/job/view?id=0", link2.split("/?").next().unwrap()));
    let (got, text) = post(&mut b, "/plane/delete", &[("plane", "mdplane00001")]);
    assert!(got < 400, "{got} {text}");
    b.goto(&format!("{link2}&p=planes"));
    b.wait(&page_shows("Deleted Modal · acmeorg"));
    drop(child2);
    let _ = std::fs::remove_dir_all(&home);
}

/// Pages opened by the addresses older links have: a control plane chosen by its place in the list (one that is
/// there, one that is not), and the set-up shown or not; each opens a page.
#[test]
fn pages_open_by_older_links_a_control_plane_by_its_place_and_set_up_shown_or_not() {
    let Some(mut b) = Browser::open(1300, 900) else { eprintln!("no Chrome here: skipped"); return };
    let served = serve(|_| {});
    let home = signed_in(&served);
    let (mut child, link) = dashboard(&home);
    for (q, said) in [("p=planes&plane=0", "Control plane"), ("p=planes&plane=7", "Control plane"), ("p=overview&new=1", "Set up"), ("p=overview&new=0", "Usage")] {
        b.goto(&format!("{link}&{q}"));
        b.wait(&format!("document.querySelector('main') && !document.querySelector('main').innerText.includes('Reading your control plane') && {}", page_shows(said)));
    }
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

/// Another control plane in the same AWS account deleted from the dashboard, as AWS answered a real deletion: the one
/// in use is not (that is stopping SuperCI); the other goes, and what was made for it elsewhere is said as left where
/// it could not be deleted: its launchers in Cloudflare and Modal (not signed in to), its budget in AWS.
#[test]
fn another_control_plane_is_deleted_from_the_dashboard_and_what_was_left_elsewhere_is_said() {
    let Some(mut b) = Browser::open(1300, 1000) else { eprintln!("no Chrome here: skipped"); return };
    let served = serve(|_| {});
    // (The other reads its settings where the dashboard writes them: its key, its launchers, its routing.)
    let aws = served.aws.clone();
    let other = superci_testkit::serve_as("testplane0002", move |c| c.read_settings(&aws.settings("testplane0002")));
    let home = signed_in(&served);
    served.aws.replaying(include_str!("../fixtures/aws/planes/delete.jsonl"), &[("http://new-plane.test", &other.url)]);
    for (k, v) in other.aws.settings("testplane0002") { served.aws.put(&format!("/superci/testplane0002/{k}"), &v) }
    // The other one's launchers in Cloudflare and Modal, and a budget in AWS for its limit.
    let launchers = r#"[{"cloud":"cloudflare","url":"https://superci-runners-testplane0002.acme.workers.dev"},{"cloud":"modal","url":"https://acme--superci-runners-testplane0002.modal.run"}]"#;
    served.aws.put("/superci/testplane0002/LAUNCHERS", launchers);
    served.aws.put("/superci/testplane0002/ROUTING", r#"{"aws_budget":{"stop":false},"order":[{"cloud":"aws"}]}"#);
    served.aws.fail("budgets.amazonaws.com", 400, &superci_testkit::json_error("AccessDeniedException", "User: arn:aws:sts::111111111111:assumed-role/Admin/someone is not authorized to perform: budgets:ViewBudget"), None);
    let (mut child, link) = common::dashboard_with(&home, &[("SUPERCI_VIEWS_S", "0")]);
    b.goto(&format!("{link}&p=planes"));
    b.wait(&format!("{} && {}", shows("Control plane"), shows(&other.url)));
    b.goto(&format!("{}/job/view?id=0", link.split("/?").next().unwrap()));
    // The one in use: refused.
    let (got, text) = post(&mut b, "/plane/delete", &[("plane", PLANE)]);
    assert!(got == 409 && text.contains("in use"), "{got} {text}");
    // The other: deleted, with what is left said.
    let (got, text) = post(&mut b, "/plane/delete", &[("plane", "testplane0002")]);
    assert!(got < 400, "{got} {text}");
    b.goto(&format!("{link}&p=planes"));
    b.wait(&format!("{} && {} && {}", page_shows("Deleted"), page_shows("Its Cloudflare launcher (sign in with Cloudflare to remove it)"), page_shows("Its Modal launcher (sign in with Modal to remove it)")));
    assert!(b.eval("document.body.innerText").as_str().unwrap_or_default().contains("Its budget in AWS"), "{}", b.eval("document.body.innerText"));
    assert!(served.aws.settings("testplane0002").is_empty(), "its settings gone: {:?}", served.aws.settings("testplane0002"));
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

/// An update asked for while jobs run on Cloudflare's containers (a new setup would stop them): refused, saying how
/// many, for two as for one.
#[test]
fn an_update_waits_for_the_jobs_running_on_cloudflare_saying_how_many() {
    let Some(mut b) = Browser::open(1300, 900) else { eprintln!("no Chrome here: skipped"); return };
    let served = serve(|_| {});
    served.connect_github();
    for name in ["one", "two"] { let (id, q) = served.github.queue(name, "superci/provider=cloudflare", served.now_ms()); served.webhook(q); served.webhook(served.github.start(id)); }
    let home = signed_in(&served);
    let (mut child, link) = dashboard(&home);
    b.goto(&link);
    b.goto(&format!("{}/job/view?id=0", link.split("/?").next().unwrap()));
    let (got, text) = post(&mut b, "/plane/update", &[]);
    assert!(got == 409 && text.contains("2 jobs are running on Cloudflare"), "{got} {text}");
    // One of them over: the other said alone.
    let first = served.store.under("job:").into_iter().filter_map(|(_, j)| j["job_id"].as_u64()).min().unwrap();
    served.webhook(served.github.finish(first, "success", "2026-10-15T10:00:01.0000000Z ok", served.now_ms()));
    let (got, text) = post(&mut b, "/plane/update", &[]);
    assert!(got == 409 && text.contains("1 job is running on Cloudflare"), "{got} {text}");
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

/// A change in the dashboard that needs a cloud whose sign-in is over: said as that, with what to do. Noted as ended
/// before, refused by the cloud as the change is made (as Cloudflare and Modal answered ended ones), or the cloud not
/// answering. Nothing changes.
#[test]
fn a_change_needing_a_cloud_whose_sign_in_is_over_says_so() {
    let Some(mut b) = Browser::open(1300, 900) else { eprintln!("no Chrome here: skipped"); return };
    let cf = serde_json::json!({ "access": "example-access_token-ended", "expires_at_ms": 4102444800000u64, "refresh": "example-refresh_token-ended" });
    let modal = serde_json::json!({ "token_id": "ak-superciendedrecording00", "token_secret": "as-superciendedrecording00", "workspace": "acmeci" });
    type Case = (&'static str, Vec<(&'static str, serde_json::Value)>, &'static str, &'static str, Vec<(&'static str, &'static str)>, &'static str);
    let cases: Vec<Case> = vec![
        ("Cloudflare noted as ended", vec![("cloudflare_ended", true.into())], "", "/plane/cloudflare", vec![("account", "1111111111111111111111111111111a")], "Cloudflare sign-in has expired"),
        ("Modal noted as ended", vec![("modal_ended", true.into())], "", "/plane/modal", vec![], "Modal no longer accepts"),
        ("Cloudflare refusing it", vec![("cloudflare", cf.clone())], include_str!("../fixtures/cloudflare/sign-in/ended.jsonl"), "/plane/cloudflare", vec![("account", "1111111111111111111111111111111a")], "Cloudflare sign-in has expired"),
        ("Modal refusing it", vec![("modal", modal.clone())], include_str!("../fixtures/modal/sign-in/ended.jsonl"), "/plane/modal", vec![], "Modal no longer accepts"),
        ("Modal not answering", vec![("modal", serde_json::json!({ "token_id": "ak-modal-for-tests", "token_secret": "as-modal-for-tests", "workspace": "acmeorg" }))], "", "/runners/modal", vec![], "Modal"),
    ];
    for (what, kept_extra, recording, path, fields, said) in cases {
        let served = serve(|_| {});
        let home = signed_in(&served);
        let file = home.join("workspaces/default/sign-ins.json");
        let mut kept: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        for (k, v) in kept_extra { kept[k] = v }
        std::fs::write(&file, kept.to_string()).unwrap();
        if !recording.is_empty() { served.aws.replaying(recording, &[]) }
        let (mut child, link) = dashboard(&home);
        b.goto(&link);
        b.goto(&format!("{}/job/view?id=0", link.split("/?").next().unwrap()));
        let (got, text) = post(&mut b, path, &fields);
        assert!(got >= 400 && text.contains(said), "{what}: {got} {text}");
        let _ = child.kill();
        let _ = std::fs::remove_dir_all(&home);
    }
}

/// A control plane asked for in AWS where it could not run: a region the account's organization closes, one without
/// the image jobs run on (both said before anything is made); then one that AWS stops, asked for again meanwhile,
/// and started over, what it made removed (or said as left).
#[test]
fn a_control_plane_asked_for_where_it_cannot_run_is_refused_and_one_stopped_is_started_over() {
    let Some(mut b) = Browser::open(1300, 1000) else { eprintln!("no Chrome here: skipped"); return };
    let served = serve(|_| {});
    // Ireland closed by the organization's policy (as AWS words it); Canada and Stockholm without the image.
    served.aws.fail("DescribeVpcs|ec2.eu-west-1.", 403, &superci_testkit::ec2_error("UnauthorizedOperation", "You are not authorized to perform this operation. User: arn:aws:sts::111111111111:assumed-role/Admin/someone is not authorized to perform: ec2:DescribeVpcs with an explicit deny in a service control policy: arn:aws:organizations::222222222222:policy/o-example001/service_control_policy/p-example1"), None);
    let home = signed_in(&served);
    let (mut child, link) = dashboard(&home);
    b.goto(&format!("{link}&p=add-runners"));
    b.wait("document.getElementById('aws-account') && !document.body.innerText.includes('Asking AWS')");
    b.goto(&format!("{}/job/view?id=0", link.split("/?").next().unwrap()));
    let (got, text) = post(&mut b, "/plane/aws", &[("region", "eu-west-1"), ("account", "111111111111")]);
    assert!(got == 409 && text.contains("cannot use Ireland") && text.contains("organization"), "{got} {text}");
    let (got, text) = post(&mut b, "/plane/aws", &[("region", "ca-central-1"), ("account", "111111111111")]);
    assert!(got == 409 && text.contains("No job could start in Canada"), "{got} {text}");
    // One AWS stops (it answers nothing more), asked for again while it runs; then started over.
    served.aws.fail_none();
    served.aws.fail("CreateRole", 503, &superci_testkit::ec2_error("ServiceUnavailable", "Service is unavailable."), None);
    let (got, text) = post(&mut b, "/plane/aws", &[("region", "us-east-2"), ("account", "111111111111")]);
    assert!(got < 400, "{got} {text}");
    let (got, _) = post(&mut b, "/plane/aws", &[("region", "us-east-2"), ("account", "111111111111")]);
    assert!(got < 400, "a second while one runs: back to its page");
    let end = std::time::Instant::now() + std::time::Duration::from_secs(120);
    loop {
        b.goto(&format!("{link}&p=plane"));
        b.wait("document.querySelector('main') && !document.querySelector('main').innerText.includes('Reading your control plane')");
        if b.eval("document.body.innerText").as_str().unwrap_or_default().contains("Try again") { break }
        assert!(std::time::Instant::now() < end, "it stopped: {}", b.eval("document.body.innerText"));
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    b.goto(&format!("{}/job/view?id=0", link.split("/?").next().unwrap()));
    let (got, text) = post(&mut b, "/plane/start-over", &[]);
    assert!(got < 400, "{got} {text}");
    b.goto(&format!("{link}&p=plane"));
    b.wait("document.querySelector('main') && !document.querySelector('main').innerText.includes('Reading your control plane')");
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

/// The Overview after one job, one run, its machine had in seconds: said in the singular, every job having run, the
/// wait under a minute.
#[test]
fn the_overview_after_a_busy_hour_on_aws_says_its_runs_jobs_waits_and_what_github_would_have_charged() {
    let Some(mut b) = Browser::open(1300, 1000) else { eprintln!("no Chrome here: skipped"); return };
    // From a control plane that counts runs, one from before that does not (0.19.5), and one from before runs were
    // kept with jobs (0.18.1).
    for old in [None, Some("0.19.5"), Some("0.18.1")] {
        let served = serve(|c| {
            (c.aws_own, c.aws_own_creds) = (Some(("111111111111".into(), "us-east-1".into())), Some(superci_core::aws::Credentials { access_key_id: "ASIAX".into(), secret_access_key: "secret".into(), session_token: Some("tok".into()), expires_at_ms: u64::MAX }));
        });
        if let Some(v) = old { *served.aws.old_version.lock().unwrap() = Some(v.into()) }
        served.connect_github();
        served.aws.put(&format!("/superci/{PLANE}/ROUTING"), r#"{"order":[{"cloud":"aws"},{"cloud":"aws-on-demand"},{"cloud":"cloudflare"}]}"#);
        served.aws.replaying(include_str!("../fixtures/aws/launch/spot.jsonl"), &[]);
        let started: String = include_str!("../fixtures/aws/launch/on-demand.jsonl").lines().filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .find(|v| v["asked"].as_str().unwrap_or_default().contains("Action=RunInstances") && v["status"] == 200).map(|v| v["answer"].as_str().unwrap_or_default().to_string()).unwrap();
        served.store.0.lock().unwrap().insert("aws:no-fleet".into(), served.now_ms().to_string());
        served.aws.fail("RunInstances", 200, &started, None);
        // Three runs on AWS and one on Cloudflare, each waiting a minute and a half for its machine and running a few
        // minutes; and one that never ran (a label nothing has).
        let mut ids = vec![];
        for (name, label) in [("build", "superci"), ("test", "superci"), ("lint", "superci"), ("docs", "superci/provider=cloudflare")] {
            let (id, q) = served.github.queue(name, label, served.now_ms());
            assert!(served.webhook(q) < 300);
            served.later(90_000);
            assert!(served.webhook(served.github.start(id)) < 300);
            served.later(4 * 60_000);
            assert!(served.webhook(served.github.finish(id, "success", "2026-10-15T10:00:01.0000000Z ok", served.now_ms())) < 300);
            ids.push(id);
        }
        let (never, q) = served.github.queue("never", "superci/cpus=8", served.now_ms());
        assert!(served.webhook(q) < 300);
        ids.push(never);
        served.aws.fail_none();
        // (Its runs counted at a sweep.)
        served.upkeep();
        // From before runs were kept with jobs: none of them in a run.
        if old == Some("0.18.1") {
            for id in &ids { let mut m = served.store.0.lock().unwrap(); let k = format!("job:{id}"); let mut j: serde_json::Value = serde_json::from_str(&m[&k]).unwrap(); j["run_id"] = 0.into(); m.insert(k, j.to_string()); }
        }
        let home = signed_in(&served);
        let (mut child, link) = dashboard(&home);
        b.goto(&format!("{link}&p=overview"));
        b.wait(&format!("{} && {}", reads("did not"), reads("Latest")));
        let page = b.eval("document.querySelector('main').innerText").as_str().unwrap_or_default().replace('\n', " ");
        assert!(page.contains("Where they ran") && page.contains("min"), "{old:?}: {page}");
        if old.is_none() { assert!(page.contains("5 runs"), "{page}") }
        if old == Some("0.18.1") {
            assert!(page.contains("All jobs"), "{page}");
            // Two days on: no jobs in the last day.
            let _ = child.kill();
            served.later(2 * 86_400_000);
            let (c, link) = dashboard(&home);
            child = c;
            b.goto(&format!("{link}&p=overview"));
            b.wait(&reads("No jobs in the last 24 hours"));
        }
        let _ = child.kill();
        let _ = std::fs::remove_dir_all(&home);
    }
}

#[test]
fn finished_jobs_logs_are_read_whole_kept_for_the_next_opens_and_raw_ones_go_to_github() {
    let Some(mut b) = Browser::open(1300, 1000) else { eprintln!("no Chrome here: skipped"); return };
    let served = serve(|_| {});
    served.connect_github();
    let mut ids = vec![];
    for n in 0..6 {
        let (id, q) = served.github.queue(&format!("job {n}"), "superci", served.now_ms());
        served.webhook(q);
        served.webhook(served.github.start(id));
        served.webhook(served.github.finish(id, "success", &format!("2026-10-15T10:00:01.0000000Z job {n} says ok\n"), served.now_ms()));
        ids.push(id);
    }
    let home = signed_in(&served);
    let (mut child, link) = dashboard(&home);
    b.goto(&link);
    b.wait(&shows("Usage"));
    b.goto(&format!("{}/job/view?id=0", link.split("/?").next().unwrap()));
    let get = |b: &mut Browser, path: &str| -> (u16, String) {
        let got = b.eval(&format!("fetch({path:?}, {{credentials: 'same-origin', redirect: 'manual'}}).then(async r => [r.status, await r.text()])"));
        (got[0].as_u64().unwrap_or(0) as u16, got[1].as_str().unwrap_or_default().to_string())
    };
    // Each finished job's log, whole; more than are kept for the next opens, then the first again.
    for (n, id) in ids.iter().enumerate().chain(std::iter::once((0, &ids[0]))) {
        let (status, text) = get(&mut b, &format!("/job/view?id={id}"));
        assert!(status == 200 && text.contains(&format!("job {n} says ok")), "{id}: {status} {text}");
    }
    // Its raw log: from GitHub's own link.
    let (status, _) = get(&mut b, &format!("/job/raw?id={}", ids[1]));
    assert!(status == 200 || status == 0 || (300..400).contains(&status), "{status}");
    // A GitLab job's address that is no job here, and one with a letter no id has.
    let (status, _) = get(&mut b, "/job/view?id=17000000099&gl=gl-2nd");
    assert!(status < 500, "{status}");
    let (status, _) = get(&mut b, "/job/view?id=1&gl=Bad!");
    assert_eq!(status, 400);
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn two_gitlabs_are_each_listed_on_the_permissions_card_and_repositories() {
    let Some(mut b) = Browser::open(1300, 1000) else { eprintln!("no Chrome here: skipped"); return };
    let served = serve(|_| {});
    served.connect_github();
    served.connect_gitlab();
    served.aws.put(&format!("/superci/{PLANE}/GITLAB_gsecond"), r#"{"url":"https://gitlab.com","token":"gitlab-token-for-tests","hook_secret":"a-hook-secret-for-tests","id":"gsecond"}"#);
    let home = signed_in(&served);
    let (mut child, link) = dashboard(&home);
    for (p, said) in [("planes", "Permissions"), ("repos", "GitLab")] {
        b.goto(&format!("{link}&p={p}"));
        b.wait(&format!("{} && !document.querySelector('main').innerText.includes('Reading your control plane')", reads(said)));
    }
    // Their tokens made with fewer scopes than SuperCI asks for: each said with what it lacks.
    served.gitlab.scopes(&["read_api"]);
    // (Read again once the control plane's ten minutes with the last answer are over.)
    served.later(11 * 60_000);
    let _ = child.kill();
    let (c, link) = dashboard(&home);
    child = c;
    b.goto(&format!("{link}&p=planes"));
    b.wait(&format!("{} && !document.querySelector('main').innerText.includes('Reading your control plane')", reads("Its token lacks")));
    b.goto(&format!("{link}&p=gitlab&g=gsecond"));
    b.wait(&shows("GitLab"));
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn signing_out_with_cloudflare_and_modal_signed_in_says_what_became_of_each_sign_in() {
    let Some(mut b) = Browser::open(1300, 1000) else { eprintln!("no Chrome here: skipped"); return };
    let served = serve(|_| {});
    let home = signed_in(&served);
    let file = home.join("workspaces/default/sign-ins.json");
    let mut kept: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    kept["cloudflare"] = serde_json::json!({ "access": "cloudflare-access-for-tests", "expires_at_ms": 4102444800000u64, "refresh": "cloudflare-refresh-for-tests" });
    kept["modal"] = serde_json::json!({ "token_id": "ak-modal-for-tests", "token_secret": "as-modal-for-tests", "workspace": "acmeorg" });
    std::fs::write(&file, kept.to_string()).unwrap();
    let (mut child, link) = dashboard(&home);
    b.goto(&link);
    b.wait(&page_shows("Usage"));
    b.goto(&format!("{}/job/view?id=0", link.split("/?").next().unwrap()));
    let (got, text) = post(&mut b, "/signout", &[]);
    assert!(got < 400, "{got} {text}");
    assert!(!file.exists(), "its sign-ins removed");
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
    // Signed out while Cloudflare's and Modal's sign-ins are given for the run by name: those are not SuperCI's to end.
    let served = serve(|_| {});
    let home = signed_in(&served);
    let (mut child, link) = common::dashboard_with(&home, &[("SUPERCI_CLOUDFLARE_TOKEN", "cf-token-given"), ("SUPERCI_MODAL_TOKEN_ID", "ak-given"), ("SUPERCI_MODAL_TOKEN_SECRET", "as-given")]);
    b.goto(&link);
    b.wait("document.querySelector('main') !== null");
    b.goto(&format!("{}/job/view?id=0", link.split("/?").next().unwrap()));
    let (got, text) = post(&mut b, "/signout", &[]);
    assert!(got < 400, "{got} {text}");
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn each_address_a_machine_or_computer_calls_without_its_link_or_key_finds_nothing_and_the_pages_stay() {
    let Some(mut b) = Browser::open(1300, 900) else { eprintln!("no Chrome here: skipped"); return };
    let served = serve(|_| {});
    // Settings edited by hand into what they may not hold: read keys whose hash is none or whose rights are unknown,
    // an image address a shell would read as more than a word. Each taken as none.
    let hash = "a".repeat(64);
    for (k, v) in [("READ_KEY_4102444800_short", "abc".to_string()), ("READ_KEY_4102444800_notahash", "z".repeat(64)), ("READ_KEY_4102444800_rights", format!("{hash}:everything")),
        ("READ_KEY_4102444800_good", format!("{hash}:runs")), ("CF_IMAGE", "https://images.example.com/a b".to_string())] {
        served.aws.put(&format!("/superci/{PLANE}/{k}"), &v);
    }
    let agent = ureq::Agent::config_builder().http_status_as_error(false).build().new_agent();
    let token = "not-a-machines-link-000000";
    for (method, path) in [("POST", "/runs/12/cancel"), ("GET", "/runs/12/artifacts"), ("GET", "/runs/12/log"), ("GET", "/runs/12"), ("GET", "/runs/12/artifact"),
        ("GET", &*format!("/cache/{token}/program")), ("GET", &format!("/cache/{token}/cache")), ("GET", &format!("/cache/{token}/layers")), ("GET", &format!("/cache/{token}/layers-program")),
        ("GET", &format!("/cache/{token}/layers.json")), ("GET", &format!("/cache/{token}/other")), ("POST", &format!("/cache/{token}/layers-links")), ("POST", &format!("/cache/{token}/layers-propose")),
        ("POST", &format!("/cache/{token}/layers-uploaded")), ("POST", &format!("/cache/{token}/layers-kept")), ("POST", &format!("/cache/{token}/tool-get")), ("POST", &format!("/cache/{token}/other"))] {
        let url = format!("{}{path}", served.url);
        let status = if method == "GET" { agent.get(&url).call().map(|r| r.status().as_u16()).unwrap_or(0) } else { agent.post(&url).send("{}").map(|r| r.status().as_u16()).unwrap_or(0) };
        assert!((400..500).contains(&status), "{method} {path}: {status}");
    }
    // What only the dashboard (or a reader's key) may ask, asked without a key, or with a run key where the
    // dashboard's is needed: nothing.
    for (method, path) in [("GET", "/caches"), ("GET", "/github/deliveries"), ("GET", "/github/public-repos"), ("GET", "/gitlab/projects"), ("GET", "/job/log?id=1"), ("GET", "/keys"), ("GET", "/secrets"),
        ("POST", "/aws/forget"), ("POST", "/caches/delete"), ("POST", "/costs"), ("POST", "/github/uninstall"), ("POST", "/gitlab/projects"), ("POST", "/keys/hello"), ("POST", "/leave"), ("POST", "/move/away"),
        ("POST", "/move/claim"), ("POST", "/move/export"), ("POST", "/move/import"), ("POST", "/permissions/given"), ("POST", "/runs"), ("POST", "/runs/tree"), ("POST", "/gitlab/webhook")] {
        let url = format!("{}{path}", served.url);
        for key in ["", served.run_key.as_str()] {
            let auth = format!("Bearer {key}");
            let status = if method == "GET" { agent.get(&url).header("authorization", &auth).call().map(|r| r.status().as_u16()).unwrap_or(0) } else { agent.post(&url).header("authorization", &auth).send("{}").map(|r| r.status().as_u16()).unwrap_or(0) };
            if key.is_empty() { assert!((400..500).contains(&status), "{method} {path} without a key: {status}") }
        }
    }
    let home = home(&served);
    let (mut child, link) = dashboard(&home);
    b.goto(&link);
    b.wait(&shows("Usage"));
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn sign_ins_made_or_ended_in_another_terminal_while_the_dashboard_runs_are_taken_in() {
    let Some(mut b) = Browser::open(1300, 1000) else { eprintln!("no Chrome here: skipped"); return };
    let served = serve(|_| {});
    let home = signed_in(&served);
    let file = home.join("workspaces/default/sign-ins.json");
    let (mut child, link) = dashboard(&home);
    b.goto(&link);
    b.wait(&page_shows("Usage"));
    let change = |f: &dyn Fn(&mut serde_json::Value)| { let mut kept: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap(); f(&mut kept); std::fs::write(&file, kept.to_string()).unwrap(); };
    // `superci login` with Cloudflare and Modal in another terminal: this page has them on its next read.
    change(&|k| {
        k["cloudflare"] = serde_json::json!({ "access": "cloudflare-access-for-tests", "expires_at_ms": 4102444800000u64, "refresh": "cloudflare-refresh-for-tests" });
        k["modal"] = serde_json::json!({ "token_id": "ak-modal-for-tests", "token_secret": "as-modal-for-tests", "workspace": "acmeorg" });
    });
    b.goto(&format!("{link}&p=planes"));
    b.wait(&format!("{} && !document.querySelector('main').innerText.includes('Reading your control plane')", shows("Control plane")));
    // Then AWS signed out there, and its device key taken out: taken in as well.
    change(&|k| { k.as_object_mut().unwrap().remove("aws"); k.as_object_mut().unwrap().remove("aws_more"); k["aws_ended"] = true.into(); });
    b.goto(&format!("{link}&p=planes"));
    b.wait(&shows("Control plane"));
    change(&|k| { k.as_object_mut().unwrap().remove("key"); });
    b.goto(&format!("{link}&p=overview"));
    b.wait(&shows("Overview"));
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn control_planes_in_clouds_not_signed_in_or_not_answering_are_listed_as_they_stand() {
    let Some(mut b) = Browser::open(1300, 1000) else { eprintln!("no Chrome here: skipped"); return };
    // The one in use in AWS; one in Modal (not signed in there) that answers, one in Cloudflare that does not.
    let served = serve(|_| {});
    let other = superci_testkit::serve_as("testplane0002", |_| {});
    let home = signed_in(&served);
    let file = home.join("workspaces/default/sign-ins.json");
    let mut kept: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    kept["planes_more"] = serde_json::json!([
        { "Modal": { "workspace": "acmeorg", "url": other.url, "plane_id": "testplane0002", "label": "" } },
        { "Cloudflare": { "account_id": "0123456789abcdef0123456789abcdef", "account_name": "Acme", "script": "superci", "url": "http://127.0.0.1:9", "plane_id": "cfplane00009", "label": "" } }]);
    std::fs::write(&file, kept.to_string()).unwrap();
    let (mut child, link) = dashboard(&home);
    b.goto(&format!("{link}&p=planes"));
    b.wait(&format!("{} && !document.querySelector('main').innerText.includes('Reading your control plane')", shows("Control plane")));
    b.wait(&format!("{} && {}", page_shows("Not answering"), page_shows("Sign in with Modal")));
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn what_the_cli_reads_and_the_keys_it_makes_are_what_the_dashboard_shows() {
    let Some(mut b) = Browser::open(1300, 1000) else { eprintln!("no Chrome here: skipped"); return };
    let served = serve(|_| {});
    served.connect_github();
    let (id, q) = served.github.queue("tests", "superci", served.now_ms()); served.webhook(q);
    served.webhook(served.github.start(id));
    served.webhook(served.github.finish(id, "success", "2026-10-15T10:00:01.0000000Z ok", served.now_ms()));
    let home = signed_in(&served);
    let cli = |args: &[&str]| { let o = common::command(&home).args(args).output().unwrap(); (o.status.code().unwrap_or(-1), String::from_utf8_lossy(&o.stdout).to_string() + &String::from_utf8_lossy(&o.stderr)) };
    // What it reads, each as the dashboard reads it.
    for args in [&["status"][..], &["jobs", "list"], &["jobs", "retrieve", &id.to_string()], &["runs", "list"], &["usage", "retrieve"], &["usage", "retrieve", "--period=week"], &["planes", "list"], &["runners", "list"],
        &["accounts", "list"], &["accounts", "retrieve", "aws"], &["limits", "retrieve"], &["labels", "list"], &["jobs", "retrieve", &id.to_string(), "--tests"], &["name", "retrieve"], &["machine", "retrieve"], &["caches", "retrieve"], &["caches", "list"], &["secrets", "list"], &["keys", "list"], &["images", "list"], &["workspaces", "list"],
        &["status", "--json"], &["jobs", "list", "--json"]] {
        let (code, said) = cli(args);
        assert!(code == 0 || code == 1, "{args:?}: {code} {said}");
        assert!(!said.contains("panicked"), "{args:?}: {said}");
    }
    // Keys: a read key, a run key; names and lengths that are not one, a name kept already.
    let (code, said) = cli(&["keys", "create", "reader", "--days=7"]);
    assert!(code == 0 && said.contains("superci_read_"), "{said}");
    let (code, said) = cli(&["keys", "create", "ci2", "--runs"]);
    assert!(code == 0 && said.contains("SUPERCI_KEY=superci_run_"), "{said}");
    for (args, why) in [(&["keys", "create", "reader"][..], "a key named reader already"), (&["keys", "create", "not-a-name"], "letters and digits"), (&["keys", "create", "averyveryverylongnamethatgoeson"], "letters and digits"),
        (&["keys", "create", "late", "--days=400"], "1 to 366 days"), (&["keys", "create", "late", "--days=0"], "1 to 366 days"), (&["keys", "create", "late", "--days=x"], "number of days"), (&["keys", "create", "capped", "--max-cpu=8"], "no limits of its own")] {
        let (code, said) = cli(args);
        assert!(code != 0 && said.contains(why), "{args:?}: {code} {said}");
    }
    let (mut child, link) = dashboard(&home);
    b.goto(&format!("{link}&p=settings"));
    b.wait(&format!("{} && {}", page_shows("reader"), page_shows("ci2")));
    let _ = child.kill();
    // From control planes too old for them: said to update first.
    for (version, args, why) in [("0.10.0", &["keys", "create", "old"][..], "0.11.0 or newer"), ("0.17.0", &["keys", "create", "old", "--runs"], "0.18.0 or newer")] {
        *served.aws.old_version.lock().unwrap() = Some(version.into());
        let (code, said) = cli(args);
        assert!(code != 0 && said.contains(why), "{version}: {code} {said}");
    }
    *served.aws.old_version.lock().unwrap() = None;
    // What it changes: each first as a dry run (nothing done), then done; and what is no change at all.
    for args in [&["name", "update", "AcmeCI"][..], &["labels", "create", "acmeci"], &["labels", "delete", "acmeci", "--confirm"], &["machine", "update", "--cpu=8", "--ram=32"], &["machine", "update", "--arch=arm64"],
        &["limits", "update", "--max-cpu=32", "--max-hours=3", "--monthly-usd=200", "--daily-usd=20"], &["limits", "update", "--monthly-usd=none", "--daily-usd=none"],
        &["runners", "update", "cloudflare", "--max-jobs=3", "--monthly-usd=50"], &["runners", "update", "cloudflare", "--max-jobs=none", "--monthly-usd=none"],
        &["caches", "update", "--size-gb=20", "--days=5", "--images=false"], &["caches", "update", "--enabled=false"], &["caches", "update", "--enabled=true", "--images=true"],
        &["accounts", "update", "aws", "--monthly-usd=100", "--daily-usd=10"], &["accounts", "update", "aws", "--monthly-usd=none", "--daily-usd=none"]] {
        let (code, said) = cli(&[args, &["--dry-run"]].concat());
        assert!(code == 0 || code == 1, "{args:?} dry: {code} {said}");
        let (code, said) = cli(args);
        assert!(code == 0 || code == 1, "{args:?}: {code} {said}");
        assert!(!said.contains("panicked"), "{args:?}: {said}");
    }
    for args in [&["machine", "update", "--cpu=lots"][..], &["limits", "update", "--monthly-usd=-1"], &["runners", "update", "nowhere", "--max-jobs=1"], &["caches", "update", "--size-gb=0"], &["labels", "delete", "superci"],
        &["name", "update"], &["accounts", "update", "gcp", "--monthly-usd=1"], &["machine", "update", "--arch=sparc"]] {
        let (code, said) = cli(args);
        assert!(code != 0, "{args:?}: {said}");
    }
    // A run started from this computer signed in (its own key, no run key): its working tree and its run taken.
    let dir = common::temp("signed-in-run");
    std::fs::create_dir_all(dir.join(".github/workflows")).unwrap();
    std::fs::write(dir.join(".github/workflows/ci.yml"), "jobs:\n  tests:\n    runs-on: superci\n    steps:\n      - run: echo ok\n").unwrap();
    for args in [&["init", "-q"][..], &["add", "-A"], &["-c", "user.name=t", "-c", "user.email=t@example.com", "commit", "-qm", "t"]] { assert!(std::process::Command::new("git").current_dir(&dir).args(args).status().unwrap().success()) }
    let o = common::command(&home).current_dir(&dir).args(["runs", "create", "--workflow=.github/workflows/ci.yml", "--detach"]).output().unwrap();
    assert!(o.status.success(), "{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
    let _ = std::fs::remove_dir_all(&dir);
    let (mut child, link) = dashboard(&home);
    b.goto(&format!("{link}&p=runners"));
    b.wait(&shows("Runner providers"));
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn sign_ins_in_each_state_are_read_and_kept_as_they_stand_and_the_pages_say_so() {
    let Some(mut b) = Browser::open(1300, 1000) else { eprintln!("no Chrome here: skipped"); return };
    let pages = |b: &mut Browser, link: &str| for p in ["overview", "planes", "runners"] {
        b.goto(&format!("{link}&p={p}"));
        b.wait(&format!("document.querySelector('main') && !document.querySelector('main').innerText.includes('Reading your control plane')"));
    };
    // An AWS sign-in due, whose renewal AWS refuses: ended, said, the rest of the pages as they are.
    let served = serve(|_| {});
    let home = signed_in(&served);
    let file = home.join("workspaces/default/sign-ins.json");
    let change = |f: &dyn Fn(&mut serde_json::Value)| { let mut kept: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap(); f(&mut kept); std::fs::write(&file, kept.to_string()).unwrap(); };
    change(&|k| { k["aws"]["creds"]["expires_at_ms"] = 0.into(); k["aws"]["refresh_token"] = "refresh-token-aws-ended".into(); });
    let (mut child, link) = dashboard(&home);
    pages(&mut b, &link);
    let _ = child.kill();
    // Cloudflare signed in, with no account it may use; Modal's token given for this run, which Modal refuses; and
    // what ended before (AWS's, Cloudflare's, Modal's) kept as ended.
    let served = serve(|_| {});
    let home = signed_in(&served);
    let file = home.join("workspaces/default/sign-ins.json");
    let mut kept: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    kept["cloudflare"] = serde_json::json!({ "access": "cloudflare-access-for-tests", "expires_at_ms": 4102444800000u64, "refresh": "cloudflare-refresh-for-tests" });
    kept["modal_ended"] = true.into();
    std::fs::write(&file, kept.to_string()).unwrap();
    served.aws.fail("api.cloudflare.com/client/v4/accounts", 200, &serde_json::json!({ "success": true, "errors": [], "messages": [], "result": [], "result_info": { "page": 1, "per_page": 50, "count": 0, "total_count": 0 } }).to_string(), None);
    served.aws.replaying(include_str!("../fixtures/modal/sign-in/ended.jsonl"), &[]);
    let (mut child, link) = common::dashboard_with(&home, &[("SUPERCI_MODAL_TOKEN_ID", "ak-modal-given"), ("SUPERCI_MODAL_TOKEN_SECRET", "as-modal-given")]);
    pages(&mut b, &link);
    let _ = child.kill();
    // Signed out of AWS since, its end noted, with Cloudflare's and Modal's: each said where a page needs it.
    let mut kept: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    for k in ["aws", "cloudflare"] { kept.as_object_mut().unwrap().remove(k); }
    kept["aws_ended"] = true.into();
    kept["cloudflare_ended"] = true.into();
    std::fs::write(&file, kept.to_string()).unwrap();
    let (mut child, link) = dashboard(&home);
    pages(&mut b, &link);
    let (got, _) = post(&mut b, "/runners/cloudflare", &[]);
    assert!(got >= 400);
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn control_planes_are_found_again_as_the_clouds_have_them_one_gone_none_found_or_a_second_accounts_sign_in_ended() {
    let Some(mut b) = Browser::open(1300, 1000) else { eprintln!("no Chrome here: skipped"); return };
    let open = |b: &mut Browser, link: &str| { b.goto(&format!("{link}&p=planes")); b.wait("document.querySelector('main') && !document.querySelector('main').innerText.includes('Reading your control plane')"); b.text("main") };
    // The kept control plane deleted from its account meanwhile: gone from the page; the one there offered.
    let served = serve(|_| {});
    let home = signed_in(&served);
    let file = home.join("workspaces/default/sign-ins.json");
    let change = |file: &std::path::Path, f: &dyn Fn(&mut serde_json::Value)| { let mut kept: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(file).unwrap()).unwrap(); f(&mut kept); std::fs::write(file, kept.to_string()).unwrap(); };
    change(&file, &|k| { k["planes_more"] = serde_json::json!([{ "Aws": { "account_id": "111111111111", "region": "us-east-1", "url": "http://127.0.0.1:9", "plane_id": "goneplane001", "label": "" } }]); });
    let (mut child, link) = dashboard(&home);
    let text = open(&mut b, &link);
    assert!(!text.contains("127.0.0.1:9"), "{text}");
    let _ = child.kill();
    // A second AWS account whose sign-in has ended: forgotten, the first's control plane read as before.
    change(&file, &|k| {
        let mut other = k["aws"].clone();
        other["account_id"] = "222222222222".into();
        other["creds"]["expires_at_ms"] = 0.into();
        other["refresh_token"] = "refresh-token-ended".into();
        k["aws_more"] = serde_json::json!([other]);
    });
    let (mut child, link) = dashboard(&home);
    let _ = open(&mut b, &link);
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
    // No control plane kept, and AWS not answering: said as the page.
    let served = serve(|_| {});
    let home = signed_in(&served);
    let file = home.join("workspaces/default/sign-ins.json");
    change(&file, &|k| { for f in ["plane", "planes_more", "key"] { k.as_object_mut().unwrap().remove(f); } });
    served.aws.fail("ListFunctions", 500, &superci_testkit::json_error("ServiceException", "We encountered an internal error. Please try again."), None);
    served.aws.fail("lambda.", 500, &superci_testkit::json_error("ServiceException", "We encountered an internal error. Please try again."), None);
    let (mut child, link) = dashboard(&home);
    b.goto(&link);
    b.wait("document.querySelector('main') && document.querySelector('main').innerText.length > 0");
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn with_several_workspaces_a_page_that_names_none_opens_on_the_one_shown_last() {
    let Some(mut b) = Browser::open(1300, 900) else { eprintln!("no Chrome here: skipped"); return };
    let served = serve(|_| {});
    let home = signed_in(&served);
    let o = common::command(&home).args(["workspaces", "create", "globex"]).output().unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let (mut child, link) = dashboard(&home);
    b.goto(&link);
    b.wait("document.querySelector('main') !== null");
    let base = link.split("/?").next().unwrap().to_string();
    // A page with no workspace in its address: opened on one, its address naming it.
    for path in ["/?p=runners", "/"] {
        b.goto(&format!("{base}{path}"));
        b.wait("document.querySelector('main') !== null");
        let at = b.eval("location.href").as_str().unwrap_or_default().to_string();
        assert!(at.contains("w="), "{path}: {at}");
    }
    // One named that is not kept here: said, nothing changed.
    let (status, text) = { let got = b.eval(&format!("fetch('{base}/?w=nosuch&p=runners', {{credentials: 'same-origin'}}).then(async r => [r.status, await r.text()])")); (got[0].as_u64().unwrap_or(0), got[1].as_str().unwrap_or_default().to_string()) };
    // (A page: the list of workspaces, to open one from.)
    assert!(status >= 400 || text.contains("class=\"spaces\""), "{status}");
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn the_public_repositories_are_read_from_github_once_a_while_and_github_failing_is_no_list() {
    let Some(mut b) = Browser::open(1300, 1000) else { eprintln!("no Chrome here: skipped"); return };
    let served = serve(|_| {});
    served.connect_github();
    served.github.set_public(true);
    let home = signed_in(&served);
    let (mut child, link) = dashboard(&home);
    // Read from GitHub, then within five minutes from what was read.
    for _ in 0..2 {
        b.goto(&format!("{link}&p=settings"));
        b.wait(&format!("{} && !document.querySelector('main').innerText.includes('Reading your control plane')", shows("Settings")));
    }
    let _ = child.kill();
    // GitHub failing to list them (minutes on): no list, the page as it is.
    let served = serve(|_| {});
    served.connect_github();
    served.github.set_public(true);
    served.github.fail("GET", "/installation/repositories", 502, "Server Error");
    let home = signed_in(&served);
    let (mut child, link) = dashboard(&home);
    b.goto(&format!("{link}&p=settings"));
    b.wait(&format!("{} && !document.querySelector('main').innerText.includes('Reading your control plane')", shows("Settings")));
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn sign_ins_not_completed_at_aws_or_started_on_another_port_for_cloudflare_are_said() {
    let Some(mut b) = Browser::open(1300, 900) else { eprintln!("no Chrome here: skipped"); return };
    let served = serve(|_| {});
    let home = signed_in(&served);
    let (mut child, link) = dashboard(&home);
    // AWS's sign-in started, then turned down on AWS's page: said, nothing kept.
    let (status, to, _) = common::post(&link, "/aws/signin", &[]);
    assert!((300..400).contains(&status), "{status} {to}");
    let state = url::Url::parse(&to).ok().and_then(|u| u.query_pairs().find(|(k, _)| k == "state").map(|(_, v)| v.into_owned())).unwrap_or_default();
    let (status, page) = common::part(&link, &format!("/oauth/callback?state={state}&error=access_denied"));
    assert!(status == 400 && page.contains("AWS sign-in was not completed"), "{status} {}", common::words(&page));
    // Cloudflare's, from a dashboard not on the port its sign-in comes back to: said, with what to do.
    let (status, _, page) = common::post(&link, "/cloudflare/signin", &[]);
    assert!(status == 409 && common::words(&page).contains("needs port 8976"), "{status} {}", common::words(&page));
    // A Cloudflare sign-in coming back with no sign-in started, and one turned down: said.
    let (status, page) = common::part(&link, "/oauth/callback?state=nothing-started&error=access_denied");
    assert!(status == 400, "{status} {}", common::words(&page));
    b.goto(&link);
    b.wait(&shows("Usage"));
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_queued_event_delivered_twice_after_a_stuck_start_or_for_a_job_run_again_is_taken_as_it_should() {
    let Some(mut b) = Browser::open(1300, 900) else { eprintln!("no Chrome here: skipped"); return };
    let served = serve(|_| {});
    served.connect_github();
    let state = |id: u64| served.store.under(&format!("job:{id}"))[0].1.clone();
    // Delivered twice: claimed once.
    let (id, q) = served.github.queue("twice", "superci", served.now_ms());
    assert!(served.webhook(q.clone()) < 300);
    let runner = state(id)["runner"].clone();
    assert!(served.webhook(q.clone()) < 300);
    assert_eq!(state(id)["runner"], runner, "the same claim");
    // Its start stuck midway (the runtime stopped), delivered again two minutes on: taken over.
    { let mut m = served.store.0.lock().unwrap(); let k = format!("job:{id}"); let mut j: serde_json::Value = serde_json::from_str(&m[&k]).unwrap(); j["state"] = "launching".into(); m.insert(k, j.to_string()); }
    served.later(120_000);
    assert!(served.webhook(q) < 300);
    // A job of a run asked to run again after AWS took its spot machine back: not on a spot machine this time.
    let (again, q) = served.github.queue("run again", "superci", served.now_ms());
    let run = served.github.job(again)["run_id"].as_u64().unwrap();
    served.store.0.lock().unwrap().insert(format!("rerun:acme/app:{run}:run again"), served.now_ms().to_string());
    assert!(served.webhook(q) < 300);
    assert!(state(again)["no_spot"] == true && state(again)["after_reclaim"] == true, "{}", state(again));
    let home = signed_in(&served);
    let (mut child, link) = dashboard(&home);
    b.goto(&format!("{link}&p=jobs"));
    b.wait(&shows("Recent jobs"));
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn each_commands_arguments_and_refusals_are_taken_or_said_and_the_dashboard_shows_what_changed() {
    let Some(mut b) = Browser::open(1300, 1000) else { eprintln!("no Chrome here: skipped"); return };
    let served = serve(|_| {});
    served.connect_github();
    served.connect_gitlab();
    let (id, q) = served.github.queue("tests", "superci", served.now_ms()); served.webhook(q);
    served.webhook(served.github.start(id));
    served.webhook(served.github.finish(id, "failure", "2026-10-15T10:00:01.0000000Z ##[error]Process completed with exit code 1.", served.now_ms()));
    let run = served.store.under(&format!("job:{id}"))[0].1["run_id"].to_string();
    let home = signed_in(&served);
    let cli = |args: &[&str]| { let o = common::command(&home).args(args).env("GITLAB_TOKEN_FOR_TESTS", "gitlab-token-for-tests").output().unwrap(); (o.status.code().unwrap_or(-1), String::from_utf8_lossy(&o.stdout).to_string() + &String::from_utf8_lossy(&o.stderr)) };
    let (id_s, by_run) = (id.to_string(), format!("--run={run}"));
    let table: Vec<Vec<&str>> = vec![
        // Jobs and runs: by run, by id, with their tests, and what is not an id.
        vec!["jobs", "list", &by_run], vec!["jobs", "list", "--run=abc"], vec!["jobs", "retrieve"], vec!["jobs", "retrieve", "abc"], vec!["jobs", "retrieve", &id_s, "--bytes=200"], vec!["jobs", "retrieve", &id_s, "--tests"],
        vec!["runs", "retrieve"], vec!["runs", "retrieve", "abc"], vec!["runs", "retrieve", &run], vec!["runs", "retrieve", &run, "--tests"], vec!["runs", "retrieve", "123456"], vec!["runs", "cancel"], vec!["runs", "cancel", "abc"],
        vec!["usage", "retrieve", "--period=month"], vec!["usage", "retrieve", "--period=year"],
        // Runners: each setting, those a provider does not have, and what is no provider.
        vec!["runners", "update", "cloudflare", "--regions=us-east-1"], vec!["runners", "update", "aws", "--regions=us-east-1,us-east-2", "--dry-run"], vec!["runners", "update", "cloudflare", "--location=weur"],
        vec!["runners", "update", "cloudflare", "--image=none"], vec!["runners", "update", "cloudflare", "--image=https://images.example.com/full:1"], vec!["runners", "update", "aws", "--networks="],
        vec!["runners", "update", "aws", "--enabled=false", "--dry-run"], vec!["runners", "update", "nowhere"], vec!["runners", "update"], vec!["runners", "delete", "cloudflare", "--only-here", "--stop-jobs", "--dry-run"],
        vec!["runners", "delete", "nowhere", "--confirm"], vec!["runners", "delete", "cloudflare"], vec!["runners", "create", "nowhere"], vec!["runners", "create"],
        // Labels, its name, the default machine, limits: nothing given, defaults, what does not hold.
        vec!["labels", "create"], vec!["labels", "create", "acmeci-big"], vec!["labels", "list"], vec!["labels", "delete", "acmeci-big", "--confirm"], vec!["labels", "delete"],
        vec!["name", "update"], vec!["name", "update", "Acme CI", "--dry-run"], vec!["machine", "update"], vec!["machine", "update", "--cpu=default", "--ram=default"], vec!["machine", "update", "--cpu=0"],
        vec!["limits", "update"], vec!["limits", "update", "--max-cpu=16"], vec!["limits", "update", "--monthly-usd=inf"],
        vec!["accounts", "update", "aws"], vec!["accounts", "update", "aws", "--monthly-usd=100", "--budget=emails", "--email=ops@example.com", "--dry-run"], vec!["accounts", "retrieve", "aws", "--account=222222222222"],
        // Code hosts' connections, projects, control planes, images.
        vec!["connections", "list"], vec!["connections", "create"], vec!["connections", "create", "github", "--dry-run"], vec!["connections", "create", "gitlab", "--dry-run"], vec!["connections", "delete", "gitlab", "--dry-run"],
        vec!["projects", "list"], vec!["projects", "update"], vec!["projects", "update", "acme/app", "--enabled=false", "--dry-run"],
        vec!["planes", "allow"], vec!["planes", "allow", "acme/app", "--dry-run"], vec!["planes", "delete"], vec!["planes", "delete", "nosuchplane1", "--confirm"], vec!["planes", "update", "--dry-run"], vec!["planes", "create"], vec!["planes", "create", "nowhere"],
        vec!["images", "create"], vec!["images", "create", "ca-central-1", "--dry-run"], vec!["images", "update", "ca-central-1", "--dry-run"], vec!["images", "delete", "ca-central-1"], vec!["images", "delete", "mars-1", "--confirm"],
        // Keys and workspaces: what is not there, what is there already, and dry runs.
        vec!["keys", "delete"], vec!["keys", "delete", "nosuchkey"], vec!["keys", "create", "dry", "--dry-run"],
        vec!["workspaces", "update", "--plane=testplane0001"], vec!["workspaces", "update", "--id=other"], vec!["workspaces", "delete", "--confirm"], vec!["workspaces", "list"],
        vec!["workspaces", "create"], vec!["workspaces", "create", "Globex", "--dry-run"], vec!["workspaces", "create", "Globex"], vec!["workspaces", "create", "Globex"], vec!["workspaces", "list"], vec!["status", "--workspace=globex"],
        vec!["workspaces", "update", "globex", "--id=initech", "--dry-run"], vec!["workspaces", "update", "globex", "--id=initech"], vec!["workspaces", "update", "initech", "--plane=testplane0001", "--dry-run"], vec!["workspaces", "update"],
        vec!["workspaces", "delete", "initech"], vec!["workspaces", "delete", "initech", "--confirm", "--dry-run"], vec!["workspaces", "delete", "initech", "--confirm"], vec!["workspaces", "delete", "nosuch", "--confirm"],
        vec!["public_repos", "create"], vec!["public_repos", "create", "acme/app", "--dry-run"], vec!["public_repos", "delete", "acme/app", "--dry-run"],
        vec!["connections", "delete", "github"], vec!["connections", "delete", "github", "acme", "--confirm"], vec!["connections", "delete", "github", "nobody", "--confirm"], vec!["connections", "delete"],
        vec!["connections", "create", "gitlab"], vec!["connections", "create", "gitlab", "--token-env=GITLAB_TOKEN_FOR_TESTS", "--dry-run"], vec!["connections", "list", "--json"],
        vec!["projects", "update", "--enabled=true", "--all", "--dry-run"], vec!["projects", "update", "--enabled=false", "--all"], vec!["projects", "update", "--enabled=true"], vec!["projects", "update", "250833", "--enabled=true", "--dry-run"],
        vec!["artifacts", "list"], vec!["artifacts", "list", "--run=abc"], vec!["artifacts", "retrieve", &by_run], vec!["artifacts", "list", &by_run], vec!["artifacts", "retrieve", "report", &by_run],
        vec!["caches", "update"], vec!["limits", "update", "--max-hours=3"], vec!["limits", "update", "--daily-usd=5"], vec!["machine", "update", "--cpu=3", "--ram=1"], vec!["machine", "update", "--disk=1"],
        vec!["secrets", "create", "LISTED", "--value-env=HOME"], vec!["secrets", "list"], vec!["secrets", "list", "--json"], vec!["secrets", "delete", "LISTED", "--confirm"],
        // Help, a resource alone, what is no command.
        vec!["help"], vec!["help", "runs"], vec!["runs"], vec!["runs", "--help"], vec!["nothing"], vec!["runs", "nothing"], vec!["--version"],
    ];
    for args in &table {
        let (code, said) = cli(args);
        assert!((0..=3).contains(&code) && !said.contains("panicked"), "{args:?}: {code} {said}");
    }
    // (A second workspace looking after the same CI, as a copy of the first: listed as its twin.)
    let twin = home.join("workspaces/twin");
    std::fs::create_dir_all(&twin).unwrap();
    std::fs::copy(home.join("workspaces/default/sign-ins.json"), twin.join("sign-ins.json")).unwrap();
    // (And a computer with none yet.)
    let empty = common::temp("no-workspaces");
    std::fs::create_dir_all(&empty).unwrap();
    let o = std::process::Command::new(env!("CARGO_BIN_EXE_superci")).args(["workspaces", "list"]).env("SUPERCI_HOME", &empty).output().unwrap();
    // (A control plane with no runs yet: said so.)
    let quiet = serve(|_| {});
    let quiet_home = signed_in(&quiet);
    let r = common::command(&quiet_home).args(["runs", "list"]).output().unwrap();
    assert!(String::from_utf8_lossy(&r.stdout).contains("No runs yet"), "{}", String::from_utf8_lossy(&r.stdout));
    let _ = std::fs::remove_dir_all(&quiet_home);
    assert!(String::from_utf8_lossy(&o.stdout).contains("None yet") || o.status.code().is_some_and(|c| c <= 3));
    let _ = std::fs::remove_dir_all(&empty);
    let (code, said) = cli(&["workspaces", "list"]);
    assert!(code <= 3 && said.contains("twin"), "{said}");
    let _ = std::fs::remove_dir_all(&twin);
    // The control plane named by its id, by its address, and by what is neither; and how long each part took.
    for plane in [PLANE.to_string(), served.url.clone(), format!("{}/", served.url), "nosuchplane1".to_string()] {
        let (code, said) = cli(&["status", &format!("--plane={plane}")]);
        assert!((0..=3).contains(&code) && !said.contains("panicked"), "{plane}: {code} {said}");
    }
    let o = common::command(&home).args(["status"]).env("SUPERCI_TIMING", "1").output().unwrap();
    assert!(o.status.code().is_some_and(|c| c <= 3), "{}", String::from_utf8_lossy(&o.stderr));
    let (mut child, link) = dashboard(&home);
    for p in ["runners", "settings", "repos"] {
        b.goto(&format!("{link}&p={p}"));
        b.wait("document.querySelector('main') && !document.querySelector('main').innerText.includes('Reading your control plane')");
    }
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn the_overview_after_one_quick_job_says_it_in_the_singular() {
    let Some(mut b) = Browser::open(1300, 1000) else { eprintln!("no Chrome here: skipped"); return };
    let served = serve(|_| {});
    served.connect_github();
    let (id, q) = served.github.queue("tests", "superci", served.now_ms()); served.webhook(q);
    served.webhook(served.github.start(id));
    served.webhook(served.github.finish(id, "success", "2026-10-15T10:00:01.0000000Z ok", served.now_ms()));
    let home = signed_in(&served);
    let (mut child, link) = dashboard(&home);
    b.goto(&format!("{link}&p=overview"));
    b.wait(&format!("{} && {}", reads("1 job"), shows("Usage")));
    let page = b.eval("document.querySelector('main').innerText").as_str().unwrap_or_default().replace('\n', " ");
    assert!(!page.contains("1 jobs") && !page.contains("did not run"), "{page}");
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}


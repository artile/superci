//! Live checks against real clouds (paid, a few cents each; run one with `cargo test -p superci -- --ignored live_`).
//! Each starts a runner the way a control plane does, with a short time bound, and then nobody stops it, as if the
//! control plane were gone: the runner registers with GitHub, waits for a job that never comes (a label no workflow
//! uses), and must end at its bound on its own. It is cleaned up afterwards whatever happens.
//!
//! Settings (environment): SUPERCI_LIVE_APP (a GitHub App's JSON: id, slug, owner, and pem_path to its key), SUPERCI_LIVE_REPO
//! (owner/repo the App is installed on, for a repository-level runner), SUPERCI_LIVE_AWS_PROFILE (a profile in
//! ~/.aws/credentials). With no App given, a runner's registration is made with the test kit's stand-ins for GitHub
//! and AWS instead: what the checks share runs offline in every test run.
#![cfg(test)]
use std::time::{Duration, Instant};

use futures::executor::block_on;
use superci_core::github::{self, App, Owner};
use superci_core::io::{self, Http, Request, Response};

/// What the checks send through: AWS and GitHub themselves, or (`at`: a stand-in's address) the test kit's stand-ins
/// for them.
pub struct Live { blocking: crate::aws::Blocking, at: Option<String> }

impl Live {
    pub fn new(at: Option<String>) -> Live { Live { blocking: crate::aws::Blocking::new(), at } }
}

#[async_trait::async_trait(?Send)]
impl Http for Live {
    async fn send(&self, mut r: Request) -> io::Result<Response> {
        if let Some(at) = &self.at {
            r.url = io::stand_in_at(&r.url, at);
            // (The host asked is the stand-in's.)
            r.headers.retain(|(k, _)| !k.eq_ignore_ascii_case("host"));
        }
        self.blocking.send(r).await
    }
}

pub fn now_ms() -> u64 { std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64 }

/// The live settings given; `at`, never in the environment, is a stand-in's address a test gives.
#[derive(Default)]
pub struct Settings { pub app: Option<String>, pub repo: Option<String>, pub aws_profile: Option<String>, pub home: Option<String>, pub at: Option<String> }

impl Settings {
    pub fn from_env() -> Settings {
        let var = |n: &str| std::env::var(n).ok();
        Settings { app: var("SUPERCI_LIVE_APP"), repo: var("SUPERCI_LIVE_REPO"), aws_profile: var("SUPERCI_LIVE_AWS_PROFILE"), home: var("HOME"), at: None }
    }
}

/// A GitHub App from its JSON at `path` (its key at `pem_path`, else beside it as `.pem`).
pub fn app_from(path: &str) -> App {
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let pem_path = v["pem_path"].as_str().map(str::to_string).unwrap_or_else(|| std::path::Path::new(path).with_extension("pem").to_string_lossy().into());
    App { id: v["id"].as_u64().unwrap(), slug: v["slug"].as_str().unwrap_or_default().into(), pem: std::fs::read_to_string(pem_path).unwrap(),
        webhook_secret: String::new(), owner: v["owner"].as_str().unwrap_or_default().into(), owner_is_org: true, host: None }
}

/// A just-in-time runner registration nothing will ever run on, with what it takes to watch and remove it.
pub struct Idle { pub jit: String, pub id: u64, token: String, repo: String, http: Live,
    /// The stand-ins it was made with, when no App was given (kept while it is).
    stand_ins: Option<superci_testkit::Served> }

impl Idle {
    pub fn new(name: &str) -> Idle { Idle::with(name, Settings::from_env()) }

    pub fn with(name: &str, settings: Settings) -> Idle {
        let (stand_ins, app, repo) = match settings.app.as_deref() {
            Some(path) => (None, app_from(path), settings.repo.clone().expect("SUPERCI_LIVE_REPO: owner/repo")),
            None => (Some(superci_testkit::serve(|_| {})), serde_json::from_value(superci_testkit::GitHub::app()).unwrap(), superci_testkit::github::REPO.to_string()),
        };
        let http = Live::new(settings.at.or_else(|| stand_ins.as_ref().map(|s| s.aws.url.clone())));
        let installs = block_on(github::installations(&http, &app, now_ms())).unwrap();
        let owner = repo.split('/').next().unwrap();
        let install = installs.iter().find(|i| i.account == owner).expect("the App is installed on the repository's owner");
        let token = block_on(github::installation_token(&http, &app, install.id, now_ms())).unwrap();
        let label = format!("superci-leaktest-{}", superci_core::crypto::random_id(8));
        let (jit, id) = block_on(github::jit_config(&http, &app.api(), &token, &Owner { org: false, login: owner.into() }, &repo, name, &[&label], "/home/runner/work")).unwrap();
        Idle { jit, id, token, repo, http, stand_ins }
    }

    /// GitHub's view of the runner: "online" while it waits for a job, "offline" once its machine is gone, None when removed.
    pub fn status(&self) -> Option<String> {
        let mut r = Request::new("GET", &format!("https://api.github.com/repos/{}/actions/runners/{}", self.repo, self.id));
        r.headers.extend([("authorization".into(), format!("Bearer {}", self.token)), ("accept".into(), "application/vnd.github+json".into()), ("user-agent".into(), "superci-live".into())]);
        let res = block_on(self.http.send(r)).ok()?;
        if res.status == 404 { return None }
        serde_json::from_slice::<serde_json::Value>(&res.body).ok()?["status"].as_str().map(str::to_string)
    }
}

impl Drop for Idle {
    fn drop(&mut self) { let _ = block_on(github::delete_runner(&self.http, &github::api_base(None), &self.token, &Owner { org: false, login: String::new() }, &self.repo, self.id)); }
}

/// Every 30 seconds (one line a minute at most is printed, plus every change) until `ended` says so or `limit` passes.
/// Returns when it ended, from `start`.
pub fn watch(what: &str, start: Instant, limit: Duration, look: impl FnMut() -> (String, bool)) -> Option<Duration> { watch_every(what, start, limit, Duration::from_secs(30), look) }

/// The same, looking `every` so often.
fn watch_every(what: &str, start: Instant, limit: Duration, every: Duration, mut look: impl FnMut() -> (String, bool)) -> Option<Duration> {
    let mut last = (String::new(), Instant::now() - Duration::from_secs(120));
    loop {
        let (state, ended) = look();
        let t = start.elapsed();
        if state != last.0 || last.1.elapsed() >= Duration::from_secs(60) {
            eprintln!("[{:>3}s] {what}: {state}", t.as_secs());
            last = (state, Instant::now());
        }
        if ended { return Some(t) }
        if t > limit { return None }
        std::thread::sleep(every);
    }
}

mod offline {
    use super::*;

    /// With no live settings (as tests run), a runner is registered with the stand-ins: GitHub says it offline until
    /// its machine is up, then online, and it is removed when dropped.
    #[test]
    fn an_idle_runner_is_registered_watched_and_removed_with_the_stand_ins() {
        let idle = Idle::new("superci-leaktest-offline");
        let stand_ins = idle.stand_ins.as_ref().expect("no live settings in tests");
        let github = stand_ins.github.clone();
        // (Its control plane answers too, sent to as it is.)
        assert!(block_on(Live::new(None).send(Request::new("GET", &format!("{}/status", stand_ins.url)))).unwrap().status > 0);
        assert!(!idle.jit.is_empty() && github.runner_names() == ["superci-leaktest-offline"]);
        assert_eq!(idle.status().as_deref(), Some("offline"));
        github.ready("superci-leaktest-offline");
        assert_eq!(idle.status().as_deref(), Some("online"));
        drop(idle);
        assert!(github.runner_names().is_empty());
    }

    /// A runner GitHub has no more is None; so is an answer that is not JSON, and one that never came.
    #[test]
    fn a_runner_removed_an_answer_not_json_and_none_at_all_are_no_state() {
        let mut idle = Idle::with("superci-leaktest-states", Settings::default());
        let github = idle.stand_ins.as_ref().unwrap().github.clone();
        let id = idle.id;
        idle.id = 999;
        assert_eq!(idle.status(), None);
        idle.id = id;
        github.answer_at("GET", &format!("/repos/{}/actions/runners/{id}", superci_testkit::github::REPO), 200, "not json");
        assert_eq!(idle.status(), None);
        let unreachable = std::mem::replace(&mut idle.http, Live::new(Some("http://127.0.0.1:1".into())));
        assert_eq!(idle.status(), None);
        // (Removed through the stand-ins after all.)
        idle.http = unreachable;
    }

    /// An App given by its JSON (its key named in it, or beside it) is the one used, through the stand-in given.
    #[test]
    fn an_app_given_by_its_json_registers_through_the_stand_in_given() {
        let served = superci_testkit::serve(|_| {});
        let dir = std::env::temp_dir().join(format!("superci-live-app-{}-{}", std::process::id(), superci_core::crypto::random_id(6)));
        std::fs::create_dir_all(&dir).unwrap();
        let app = superci_testkit::GitHub::app();
        std::fs::write(dir.join("app.pem"), app["pem"].as_str().unwrap()).unwrap();
        std::fs::write(dir.join("app.json"), serde_json::json!({ "id": app["id"], "slug": app["slug"], "owner": app["owner"] }).to_string()).unwrap();
        std::fs::write(dir.join("named.json"), serde_json::json!({ "id": app["id"], "pem_path": dir.join("app.pem") }).to_string()).unwrap();
        for (file, name) in [("app.json", "superci-leaktest-beside"), ("named.json", "superci-leaktest-named")] {
            let settings = Settings { app: Some(dir.join(file).to_string_lossy().into()), repo: Some(superci_testkit::github::REPO.into()), at: Some(served.aws.url.clone()), ..Default::default() };
            let idle = Idle::with(name, settings);
            assert!(idle.stand_ins.is_none() && served.github.runner_names().contains(&name.to_string()));
        }
        assert!(served.github.runner_names().is_empty());
        assert_eq!(app_from(&dir.join("app.json").to_string_lossy()).slug, app["slug"].as_str().unwrap());
        // Sent through as it is, with no stand-in.
        let res = block_on(Live::new(None).send(Request::new("GET", &format!("{}/api.github.com/app", served.aws.url)))).unwrap();
        assert!(res.status > 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Watching prints each change and, while it stays, a line a minute; it ends when the look says so or at the limit.
    #[test]
    fn watching_ends_when_the_look_says_so_or_at_the_limit() {
        let mut looks = vec![("", false), ("", false), ("up", true)].into_iter();
        assert!(watch_every("machine", Instant::now(), Duration::from_secs(3600), Duration::ZERO, || { let (s, e) = looks.next().unwrap(); (s.to_string(), e) }).is_some());
        assert_eq!(watch("machine", Instant::now() - Duration::from_secs(1), Duration::ZERO, || ("pending".into(), false)), None);
        assert!(watch("machine", Instant::now(), Duration::ZERO, || ("gone".into(), true)).is_some());
    }
}

mod aws_live {
    use super::*;
    use superci_core::aws::{self, Credentials, Launch};

    /// Credentials of a named profile in ~/.aws/credentials (SUPERCI_LIVE_AWS_PROFILE).
    fn creds(settings: &Settings) -> Credentials {
        let profile = settings.aws_profile.as_deref().expect("SUPERCI_LIVE_AWS_PROFILE");
        let file = std::fs::read_to_string(format!("{}/.aws/credentials", settings.home.as_deref().expect("HOME"))).unwrap();
        let mut section = String::new();
        let (mut id, mut secret, mut token) = (None, None, None);
        for line in file.lines().map(str::trim) {
            if line.starts_with('[') { section = line.trim_matches(|c| c == '[' || c == ']').to_string(); continue }
            if section != profile { continue }
            if let Some((k, v)) = line.split_once('=') {
                let v = Some(v.trim().to_string());
                match k.trim() { "aws_access_key_id" => id = v, "aws_secret_access_key" => secret = v, "aws_session_token" => token = v, _ => {} }
            }
        }
        Credentials { access_key_id: id.expect("key id"), secret_access_key: secret.expect("secret"), session_token: token, expires_at_ms: u64::MAX }
    }

    fn state(http: &Live, region: &str, c: &Credentials, id: &str) -> String {
        let xml = block_on(aws::ec2(http, region, c, "DescribeInstances", serde_json::json!({ "InstanceId": [id] }), now_ms())).unwrap_or_default();
        xml.split("<instanceState>").nth(1).and_then(|s| aws::xml_tag(s, "name")).unwrap_or("unknown").to_string()
    }

    /// A profile's credentials are read from its own section only, and a file with none is refused.
    #[test]
    fn a_profiles_credentials_are_read_from_its_section_only() {
        let home = std::env::temp_dir().join(format!("superci-live-home-{}-{}", std::process::id(), superci_core::crypto::random_id(6)));
        std::fs::create_dir_all(home.join(".aws")).unwrap();
        std::fs::write(home.join(".aws/credentials"), "[other]\naws_access_key_id = AKIDOTHER\n\n[superci]\n# a note\naws_access_key_id = AKIDEXAMPLE\naws_secret_access_key = secret\naws_session_token = token\nregion = us-east-1\n").unwrap();
        let settings = Settings { aws_profile: Some("superci".into()), home: Some(home.to_string_lossy().into()), ..Default::default() };
        let c = creds(&settings);
        assert_eq!((c.access_key_id.as_str(), c.secret_access_key.as_str(), c.session_token.as_deref()), ("AKIDEXAMPLE", "secret", Some("token")));
        std::fs::write(home.join(".aws/credentials"), "").unwrap();
        assert!(std::panic::catch_unwind(|| creds(&settings)).is_err());
        let _ = std::fs::remove_dir_all(&home);
    }

    /// A machine's state as AWS says it, and "unknown" when AWS says nothing of it.
    #[test]
    fn a_machines_state_is_as_aws_says_it_or_unknown() {
        let served = superci_testkit::serve(|_| {});
        let http = Live::new(Some(served.aws.url.clone()));
        let c = Credentials { access_key_id: "AKIDEXAMPLE".into(), secret_access_key: "secret".into(), session_token: None, expires_at_ms: u64::MAX };
        served.aws.fail("DescribeInstances", 200, "<DescribeInstancesResponse><instanceState><code>16</code><name>running</name></instanceState></DescribeInstancesResponse>", Some(1));
        assert_eq!(state(&http, "us-east-1", &c, "i-0123456789abcdef0"), "running");
        assert_eq!(state(&http, "us-east-1", &c, "i-0123456789abcdef0"), "unknown");
    }

    /// AWS's prices as the control plane reads them: a zone's spot prices over a past day, the Price List's on-demand
    /// and gp3 prices (needs pricing:GetProducts), against what AWS publishes.
    #[test]
    #[ignore]
    fn live_aws_prices() {
        let (http, region, c, now) = (Live::new(None), "us-east-1", creds(&Settings::from_env()), now_ms());
        let day = block_on(aws::spot_cost(&http, region, "us-east-1a", &c, "c7a.large", "linux", now - 26 * 3_600_000, now - 2 * 3_600_000, now)).unwrap();
        let minute = block_on(aws::spot_cost(&http, region, "us-east-1a", &c, "c7a.large", "linux", now - 3_600_000, now - 3_590_000, now)).unwrap();
        eprintln!("c7a.large spot in us-east-1a: a day ${day:.4} (${:.4}/h on average), ten seconds billed as a minute ${minute:.6}", day / 24.0);
        assert!(day > 0.0 && (minute * 60.0 - day / 24.0).abs() < day / 24.0, "a minute at about the hour's rate");
        match block_on(aws::on_demand_price(&http, &c, region, "c7a.large", "linux", now)) { Ok(p) => eprintln!("c7a.large on-demand ${p}/h (AWS lists $0.10264)"), Err(e) => eprintln!("on-demand: {e}") }
        // What a machine sent (CloudWatch), e.g. the one live_aws_machine_ends_at_its_time_bound started (SUPERCI_LIVE_AWS_INSTANCE).
        if let Ok(id) = std::env::var("SUPERCI_LIVE_AWS_INSTANCE") {
            let bytes = block_on(aws::bytes_sent(&http, region, &c, &id, now - 12 * 3_600_000, now, now)).unwrap();
            eprintln!("{id} sent {:.1} MB", bytes / 1e6);
        }
        match block_on(aws::disk_price(&http, &c, region, now)) { Ok(p) => eprintln!("gp3 ${p}/GB-month (AWS lists $0.08)"), Err(e) => eprintln!("gp3: {e}") }
    }

    /// A spot machine with a control plane's user data and a 3-minute bound terminates itself, nobody asking.
    #[test]
    #[ignore]
    fn live_aws_machine_ends_at_its_time_bound() {
        let (http, region, c) = (Live::new(None), "us-east-1", creds(&Settings::from_env()));
        let idle = Idle::new(&format!("superci-leaktest-aws-{}", superci_core::crypto::random_id(6)));
        let image = block_on(aws::latest_image(&http, region, &c, "135269210855", "runs-on-v2.2-ubuntu24-full-x64-*", now_ms())).unwrap();
        let user_data = aws::runner_user_data(&superci_core::io::Work::GitHub { jit: idle.jit.clone() }, 3).unwrap();
        let types = ["c7a.large".to_string(), "m7a.large".into(), "c7i.large".into()];
        let tags = [("superci".to_string(), "leaktest".to_string())];
        let start = Instant::now();
        let aws::Started { id, kind, .. } = block_on(aws::run_instance(&http, &c, &Launch { region, network: None, image: &image, types: &types, user_data: &user_data, disk_gb: 30, disk_mibs: 125, disk_iops: 3000, clock: None, skip: &[], tags: &tags, spot: true, os: "linux", avoid: &[], timer: None, jitter_ms: 0, fleet: false, store_disk: None }, now_ms(), &mut superci_core::aws::Told::default())).unwrap();
        eprintln!("launched {id} ({kind}) in {region}, bound 3 min");
        let mut online = false;
        let ended = watch("machine", start, Duration::from_secs(10 * 60), || {
            let s = state(&http, region, &c, &id);
            let runner = idle.status().unwrap_or_else(|| "removed".into());
            online |= runner == "online";
            (format!("{s}, runner {runner}"), s == "shutting-down" || s == "terminated")
        });
        if ended.is_none() { let _ = block_on(aws::terminate_instance(&http, region, &c, &id, now_ms())); }
        let ended = ended.expect("the machine was still up after 10 minutes (terminated now)");
        assert!(online, "its runner never came online: the bound was not tested on a waiting runner");
        assert!(ended >= Duration::from_secs(170) && ended <= Duration::from_secs(7 * 60), "ended after {ended:?}");
    }
}

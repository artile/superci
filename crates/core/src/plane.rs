//! A control plane: the part that lives in the user's own cloud account (Cloudflare, AWS or Modal). It is headless: setup
//! and changes happen in the dashboard on the user's machine, which gives the control plane its GitHub App credentials and
//! AWS connect token as the runtime's encrypted secrets. Online, a control plane answers only:
//! - `POST /webhook`: the App's `workflow_job` events (signed with the App's webhook secret);
//! - `GET /.well-known/openid-configuration`, `/.well-known/jwks.json`: its OpenID Connect identity (public keys only),
//!   which the connected AWS account trusts;
//! - `POST /aws/callback`: the dashboard reporting the AWS role it made, with a one-time token (closes once used);
//! - `GET /health`: whether GitHub and AWS are connected (yes/no only), for the setup page.
//! Each job labelled for it runs on a spot machine in the connected AWS account.
use std::cell::RefCell;
use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::aws::{self, Credentials, Launch};
use crate::cache;
use crate::crypto::{safe_eq, sha256_hex, verify_hub_signature, SigningKeyStore};
use crate::github::{self, App, Owner};
use crate::gitlab::{self, GitLab};
use crate::io::{Containers, get_json, put_json, Clock, Http, Request, Response, Result, Store, Timer, Work};
use crate::spec::{aws_instance_types, disk_speed, Capacity, Size, Spec, DISK_IOPS_INCLUDED, DISK_MIBS_INCLUDED};

pub const AUDIENCE: &str = "superci";
/// How long a job's machine lives in a cloud, in minutes: the longest a job may run as set (six hours unless set),
/// never more than that cloud lets a machine live.
pub fn job_minutes(set: Option<u32>, cloud: &str) -> u32 { set.unwrap_or(MAX_JOB_MINUTES).clamp(1, max_minutes(cloud)) }

/// The longest a cloud lets a machine live, in minutes. AWS sets no limit, so there it is the longest GitHub lets a
/// job run on a runner of your own (five days); Cloudflare keeps a container six hours at most; a Modal sandbox
/// lives a day at most. The longest a job may run (`Routing::max_minutes`, six hours unless set) is never more.
pub fn max_minutes(cloud: &str) -> u32 {
    match cloud { "cloudflare" => 6 * 60, "modal" => 24 * 60, _ => 5 * 24 * 60 }
}

/// What the runtime gives a control plane: its id and label, and (once set up on the user's machine) the App and AWS connect
/// token from its secrets.
pub struct Config {
    pub plane_id: String,
    /// The label workflows name (`runs-on: superci`), and the ones it also answers to: a label changed in the
    /// dashboard keeps the one before working until that is removed, so no workflow has to change the same day.
    pub label: String,
    pub more_labels: Vec<String>,
    /// What it is called where people read it (the dashboard's header, the App's name, what it says on a job it
    /// could not run), when that is not SuperCI: a team's own name for its CI (AcmeCI).
    pub name: Option<String>,
    pub app: Option<App>,
    /// GitHub Apps for further organizations (a private App belongs to one account, so each has its own); `app` is
    /// the first one made.
    pub more_apps: Vec<App>,
    /// GitLab, when connected from the dashboard (jobs from its projects' webhooks).
    pub gitlab: Option<GitLab>,
    /// Further GitLab connections (another GitLab, or another account's token); `gitlab` is the first one made.
    pub more_gitlabs: Vec<GitLab>,
    pub aws_region: Option<String>,
    pub aws_connect_token: Option<String>,
    /// The AWS regions machines go to, in order, set from the dashboard: the next one when a region has no spot
    /// capacity (or quota) left. None set: the connection's region.
    pub aws_regions: Vec<String>,
    /// Networks of the account's own, by region, set from the dashboard: machines there start in these subnets and
    /// security groups, not in the control plane's own network.
    pub aws_networks: HashMap<String, aws::GivenNetwork>,
    /// Where Cloudflare's containers start: a Durable Object location hint ("enam", "weur"…), or "auto" (Cloudflare
    /// chooses). Set from the dashboard; eastern North America (near GitHub and GitLab) when unset.
    pub cloudflare_location: String,
    /// Where GitHub's full image is published for Cloudflare's containers (see `docker::start`); none: the
    /// container's own small image.
    pub cloudflare_image: Option<String>,
    /// Set by each dashboard session on your machine (through the cloud's API): keys to read `/status`, each with
    /// the time it expires (ms).
    pub dashboard_keys: Vec<(u64, String)>,
    /// Keys that only read (`superci keys create`, for a coding agent or a script that should look and not change):
    /// each with the time it expires (ms), its name, and the SHA-256 of the key (the key itself is not kept here).
    /// Set through the cloud's API like the dashboard's, as READ_KEY_<expires, unix seconds>_<NAME>.
    pub read_keys: Vec<(u64, String, String)>,
    /// A one-time token for moving to another control plane, set by the dashboard through the cloud's API (so only
    /// someone who controls this cloud account can ask): the old one hands over its settings with it, the new one
    /// takes its history and GitLab's project runners with it.
    pub move_token: Option<String>,
    /// Runner agents in other clouds (Cloudflare containers, Modal sandboxes), set from the dashboard.
    pub agents: Vec<Agent>,
    /// Where jobs run, set from the dashboard (workflows only say `runs-on: <label>`).
    pub routing: Routing,
    /// Machines started by this control plane itself: containers (a Cloudflare control plane with its container
    /// application) or sandboxes (a Modal control plane), in `own_cloud`.
    pub containers: bool,
    pub own_cloud: String,
    /// When the control plane runs in AWS: its own account and the region for machines, and the runtime's credentials
    /// (no role to assume, no OpenID Connect).
    pub aws_own: Option<(String, String)>,
    /// A control plane in AWS whose AWS runners were removed (`AWS_RUNNERS=off`): it starts no machines there.
    pub aws_runners_off: bool,
    pub aws_own_creds: Option<Credentials>,
    pub image_owner: String,
    pub image_name: String,
    pub instance_types: Vec<String>,
    /// The default machine (`runs-on: superci`), set from the dashboard; a job's label may ask for another.
    pub machine: Spec,
}

/// This control plane's version (SuperCI's): the dashboard offers an update when it is older than the dashboard.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

impl Config {
    /// Every label it answers to, its own first.
    pub fn labels(&self) -> Vec<String> { std::iter::once(self.label.clone()).chain(self.more_labels.iter().filter(|l| **l != self.label).cloned()).collect() }

    /// The labels as set in the dashboard (a JSON list, the one workflows are shown first): taken when it holds at
    /// least one that can be a label.
    pub fn set_labels(&mut self, json: &str) {
        let mut set: Vec<String> = serde_json::from_str::<Vec<String>>(json).unwrap_or_default().into_iter().map(|l| l.to_ascii_lowercase()).filter(|l| crate::spec::valid_label(l).is_ok() || l == "superci").collect();
        set.dedup();
        if let Some(first) = set.first().cloned() { self.label = first; self.more_labels = set.split_off(1); }
    }

    /// What it is called: SuperCI, unless given a name of its own.
    pub fn name(&self) -> &str { self.name.as_deref().unwrap_or("SuperCI") }

    /// The name as set in the dashboard: taken when it can be one (see `spec::valid_name`).
    pub fn set_name(&mut self, name: &str) {
        let name = name.trim();
        self.name = (name != "SuperCI" && crate::spec::valid_name(name).is_ok()).then(|| name.to_string());
    }

    /// What a label asks for, by any of its labels.
    pub fn parse(&self, label: &str) -> Option<std::result::Result<Spec, String>> { Spec::parse_among(label, &self.labels()) }

    pub fn new(plane_id: String) -> Self {
        Config {
            plane_id,
            label: "superci".into(),
            more_labels: Vec::new(),
            name: None,
            own_cloud: "cloudflare".into(),
            app: None,
            gitlab: None,
            more_gitlabs: Vec::new(),
            more_apps: Vec::new(),
            aws_region: None,
            aws_connect_token: None,
            aws_regions: Vec::new(),
            aws_networks: HashMap::new(),
            cloudflare_location: "enam".into(),
            cloudflare_image: None,
            dashboard_keys: Vec::new(),
            read_keys: Vec::new(),
            move_token: None,
            agents: Vec::new(),
            routing: Routing::default(),
            containers: false,
            aws_own: None,
            aws_runners_off: false,
            aws_own_creds: None,
            // RunsOn's public GitHub-compatible Ubuntu 24.04 image: its snapshot is warm, so machines start in seconds.
            image_owner: "135269210855".into(),
            image_name: "runs-on-v2.2-ubuntu24-full-x64-*".into(),
            instance_types: vec!["c8a.xlarge".into(), "c7a.xlarge".into(), "m7a.xlarge".into()],
            machine: Spec::default(),
        }
    }

    pub fn owner(&self) -> Option<Owner> {
        self.app.as_ref().map(|a| Owner { org: a.owner_is_org, login: a.owner.clone() })
    }

    /// Every GitHub App of this control plane: the first, then one for each further organization.
    pub fn apps(&self) -> impl Iterator<Item = &App> { self.app.iter().chain(self.more_apps.iter()) }

    /// Every GitLab connection of this control plane: the first, then each further one.
    pub fn gitlabs(&self) -> impl Iterator<Item = &GitLab> { self.gitlab.iter().chain(self.more_gitlabs.iter()) }
}

/// What a runtime keeps between requests while it is alive (credentials, tokens, the image id).
#[derive(Default)]
pub struct Cache {
    /// Whether the jobs' indexes are complete (once they are, they stay so).
    indexed: std::cell::Cell<bool>,
    /// Runners' links known to be a job's (`rjob:<token>`), until when; and a count for posts of lines in one
    /// millisecond (see `ControlPlane::lines_in`).
    runner_links: RefCell<HashMap<String, u64>>,
    lines_seq: std::cell::Cell<u64>,
    installed: RefCell<Option<(bool, u64)>>,
    creds: RefCell<Option<Credentials>>,
    tokens: RefCell<HashMap<u64, (String, u64)>>,
    /// The machine image per region and architecture, and until when it is taken as current.
    image: RefCell<HashMap<String, (String, u64)>>,
    /// Which of a set of machine types a region has, and until when that is taken as current.
    types: RefCell<HashMap<String, (Vec<String>, u64)>>,
    /// What the GitHub App asks for and the GitLab token's scopes, and until when they are taken as current.
    app_permissions: RefCell<HashMap<u64, (serde_json::Value, u64)>>,
    /// Each App's address on GitHub as GitHub last said it (its owner may rename the App there).
    app_slugs: RefCell<HashMap<u64, String>>,
    gitlab_scopes: RefCell<HashMap<String, (Vec<String>, u64)>>,
    /// The public repositories the App is installed on, and until when they are taken as current.
    public_repos: RefCell<Option<(Vec<String>, u64)>>,
    /// The control plane's own network per AWS region (None: not made there), and until when it is taken as current.
    network: RefCell<HashMap<String, (Option<aws::Network>, u64)>>,
    /// Whether a region's cache bucket has this version's `superci-cache`, and until when that is taken as current.
    cache_ready: RefCell<HashMap<String, (bool, u64)>>,
    /// GitHub's keys for its jobs' tokens, and when they were fetched.
    github_keys: RefCell<Option<(serde_json::Value, u64)>>,
}

#[derive(Serialize, Deserialize, Clone)]
struct Identity { signing: SigningKeyStore }

#[derive(Serialize, Deserialize, Clone)]
pub struct AwsConnection { pub account_id: String, pub region: String, pub role_arn: String, pub connected: bool, pub error: Option<String> }

#[derive(Serialize, Deserialize, Clone, Default)]
pub struct Job {
    pub job_id: u64, pub run_id: u64, pub repo: String, pub state: String, pub at_ms: u64,
    pub runner: Option<String>, pub runner_id: Option<u64>, pub installation_id: u64,
    /// Where it ran ("aws", "cloudflare", "modal"), the machine's id there, and its kind.
    pub cloud: String, pub machine_id: Option<String>, pub machine_type: Option<String>,
    pub error: Option<String>, pub seen_in_progress: bool,
    /// The label it asked for (`superci`, `superci/cpu=8/arch=arm64`): what machine, and what its runner registers as.
    #[serde(default)] pub label: String,
    /// What its machine costs per hour (estimated from published prices; AWS: the spot price at launch), and when
    /// the job started running and when its machine ended (ms). The machine is paid from launch (`at_ms`) to end.
    #[serde(default)] pub usd_per_hour: Option<f64>,
    /// Its price while its runner waits for the job, when lower (Cloudflare bills CPU only as it is used).
    #[serde(default, skip_serializing_if = "Option::is_none")] pub waiting_usd_per_hour: Option<f64>,
    #[serde(default)] pub started_ms: Option<u64>,
    #[serde(default)] pub ended_ms: Option<u64>,
    /// When its machine was last asked for (ms), and how many times it has been placed again because one never began it.
    #[serde(default)] pub launched_ms: Option<u64>,
    #[serde(default)] pub retries: u32,
    /// Its machine's CPUs (what GitHub's runner of that size would have cost is compared by it).
    #[serde(default)] pub cpu: Option<u32>,
    /// Where it comes from: "gitlab", or GitHub (empty). A GitLab job's project (`repo` is its path) and tags.
    #[serde(default, skip_serializing_if = "String::is_empty")] pub provider: String,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub project_id: Option<u64>,
    /// Which GitLab connection it came through (see `GitLab::id`); nothing: the first.
    #[serde(default, skip_serializing_if = "String::is_empty")] pub gitlab: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")] pub tags: Vec<String>,
    /// Its name and its workflow's (GitLab: its stage), for people to tell jobs apart.
    #[serde(default, skip_serializing_if = "String::is_empty")] pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")] pub workflow: String,
    /// Added to its runner's name when its first runner went on to run another job (a new one cannot take its name).
    #[serde(default, skip_serializing_if = "String::is_empty")] pub runner_suffix: String,
    /// The AWS region its machine is in (one of the connection's regions), its zone there, and its disk (GB).
    #[serde(default, skip_serializing_if = "Option::is_none")] pub region: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub zone: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub disk_gb: Option<u32>,
    /// Its disk's speed, when above what AWS includes (MiB a second, IOPS): counted in its price.
    #[serde(default, skip_serializing_if = "Option::is_none")] pub disk_mibs: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub disk_iops: Option<u32>,
    /// How its start went, to see where a slow one lost its time. When GitHub made the job (ms; `at_ms` is when this
    /// control plane heard of it). Every request to AWS for its machine, in order (see `Ask`; the last two dozen). The
    /// link its machine says its own part with (`hello`); what it said is kept apart from this record (see
    /// `MachineStart`) and given with it in the status, as `machine`.
    #[serde(default, skip_serializing_if = "Option::is_none")] pub queued_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")] pub asks: Vec<Ask>,
    /// How many times its start stopped with spot pools still to ask and went on with them a moment later (see
    /// `MORE_SPOT`); back to none once it has a machine.
    #[serde(default, skip_serializing_if = "is_zero")] pub spot_rounds: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub hello: Option<String>,
    /// A job of a run from a computer (provider "local"): the token in its runner's address, `/runner/<token>`,
    /// where the runner takes its job from this control plane in place of GitHub (see `runner_service`).
    #[serde(default, skip_serializing_if = "Option::is_none")] pub local: Option<String>,
    /// The key for coding agents that started it (its name), when one did (see `RunKey`).
    #[serde(default, skip_serializing_if = "Option::is_none")] pub by: Option<String>,
    /// What it writes most is in memory, or on its machine's own disk (its label asked: `-ramdisk`, `-localdisk`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")] pub ramdisk: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")] pub local_disk: bool,
    /// Its machine has no public address (a private subnet of the account's own: none is billed).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")] pub private: bool,
    /// What its machine cost, settled once it ended: at the prices the cloud billed for its time ("prices"), or as the
    /// cloud measured it ("measured"). Until then, `usd_per_hour` over its time is the estimate.
    #[serde(default, skip_serializing_if = "Option::is_none")] pub cost_usd: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub cost_from: Option<String>,
    /// What its AWS machine sent (GB, CloudWatch's count, read a little after it ended; -1: AWS would not say); its
    /// cost is in `cost_usd`.
    #[serde(default, skip_serializing_if = "Option::is_none")] pub sent_gb: Option<f64>,
    /// Which attempt of its run it is in, as GitHub counts them (a run, or a job of it, run again is the next).
    #[serde(default, skip_serializing_if = "Option::is_none")] pub attempt: Option<u32>,
    /// Nothing here could run it: a runner was started that failed it at once on GitHub, saying why (`fail_fast`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")] pub failed_fast: bool,
    /// Its job ended on another job's runner while its own machine had not begun anything: the machine is kept a
    /// while for that other job (still queued, its runner taken), and no new one is started for this one.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")] pub over: bool,
    /// The job its machine is for now, when its own job went to another job's runner: that other job, still queued
    /// with its runner taken. If this machine never comes up, a new one is started while that job still waits.
    #[serde(default, skip_serializing_if = "Option::is_none")] pub for_job: Option<u64>,
    /// Its own job began on a runner (its own or another job's), or ended: GitHub no longer has it queued.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")] pub began: bool,
    /// Why it failed, while a failing runner (see `fail_fast`) is still to be started for it.
    #[serde(default, skip_serializing_if = "Option::is_none")] pub fail_pending: Option<String>,
    /// The GitHub App its event came through (one per organization); none: the first.
    #[serde(default, skip_serializing_if = "Option::is_none")] pub app_id: Option<u64>,
    /// Its AWS machine is an on-demand one (the place `AWS_ON_DEMAND`): as its label asks, or as the order has it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")] pub on_demand: bool,
    /// Not on a spot machine this time, if another place in the order can run it: run again after AWS took its spot
    /// machine back, or a try that ran out of time with every region refusing a spot machine.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")] pub no_spot: bool,
    /// Run again after AWS took its spot machine back (GitHub's new attempt of the job, or GitLab's retry).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")] pub after_reclaim: bool,
    /// Kept off spot machines, and the account's on-demand limit did not let its machine start: asked on a spot
    /// machine again, whatever kept it off them (and kept there: it waits for one rather than for that limit).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")] pub spot_again: bool,
    /// The places where starting its machine failed: its next try starts with the others (cleared once one starts).
    #[serde(default, skip_serializing_if = "Vec::is_empty")] pub failed_at: Vec<String>,
    /// What each region said when it had no spot machine for it, and the job went down the order (or waits).
    #[serde(default, skip_serializing_if = "Option::is_none")] pub spot_refused: Option<String>,
    /// What its spot machine says its notice with (see `aws::runner_user_data_for`), and whether AWS took it back.
    #[serde(default, skip_serializing_if = "Option::is_none")] pub notice: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")] pub interrupted: bool,
    /// The job its machine took, when that is not its own: GitHub gives a waiting job to any idle runner with its
    /// label, so jobs queued together run on each other's machines. A list of jobs names the machine's row by it
    /// (what failed on that machine is that job, as GitHub shows it).
    #[serde(default, skip_serializing_if = "Option::is_none")] pub ran: Option<u64>,
    /// How the job itself ended, as GitHub said when it was over (`success`, `failure`, `cancelled`, …): whether its
    /// own steps passed. Apart from `state`, which is whether a machine ran it.
    #[serde(default, skip_serializing_if = "Option::is_none")] pub conclusion: Option<String>,
    /// The job its interrupted machine was running, to be run again once its run has finished.
    #[serde(default, skip_serializing_if = "Option::is_none")] pub rerun: Option<Rerun>,
    /// How often the record was written: one more at each write (see `ControlPlane::keep`). A copy whose count is
    /// not the kept record's was read before another request wrote it.
    #[serde(default, skip_serializing_if = "unwritten")] pub rev: std::cell::Cell<u64>,
    /// GitHub's id of its repository, the branch it runs for, and the repository's default branch: what the cache
    /// lets the tools on its machine that have no GitHub token of their own (Turbo, Nx, Bazel) read and write.
    #[serde(default, skip_serializing_if = "Option::is_none")] pub repo_id: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub default_branch: Option<String>,
    /// What of its cost its day's totals (`Day`) hold: none until it is over, then its cost as counted, so a cost
    /// settled later adds only the difference. Written with the job itself, so it is counted once.
    #[serde(default, skip_serializing_if = "Option::is_none")] pub counted_usd: Option<f64>,
}

fn unwritten(rev: &std::cell::Cell<u64>) -> bool { rev.get() == 0 }

/// Where it is noted that this control plane's role could not ask many spot pools in one request (see `aws::fleet`):
/// for ten minutes they are asked one by one without trying.
const NO_FLEET_KEY: &str = "aws:no-fleet";

/// Where a launch template made for one request is noted until it is removed (`aws:template:<region>:<name>`): one
/// still noted later was not, and a sweep removes it.
fn template_key(region: &str, name: &str) -> String { format!("aws:template:{region}:{name}") }

/// One request to AWS for a job's machine: when (ms after the control plane heard of the job), for which machine type
/// in which zone, spot or on-demand, what AWS said ("started", or its word for no), and how long the answer took (ms).
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Ask { pub at: u64, pub kind: String, #[serde(default, skip_serializing_if = "String::is_empty")] pub zone: String, pub spot: bool, pub said: String, pub ms: u64,
    /// How many pools the request asked at once, when more than the one it names (see `aws::fleet`: the type and
    /// zone are then the pool AWS chose, or none).
    #[serde(default, skip_serializing_if = "is_zero")] pub pools: u32 }

/// What a machine said of its own start (see `aws::with_hello`), by its own clock: how long after its kernel started
/// its start-up script began (seconds), when the script began, when it began reading the runner's files, when it was
/// ready to start the runner, and when the runner listened for a job (ms). Kept under `hello:<job>`, apart from the
/// job's record: a machine says so while its job's events arrive, and neither write may undo the other.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct MachineStart {
    #[serde(default)] pub boot_s: f64, #[serde(default)] pub script_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub read_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub ready_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub listening_ms: Option<u64>,
    /// How many seconds GitHub's address for its runner refused it before the runner started (see
    /// `aws::runner_hosts`): none, when it answered at once.
    #[serde(default, skip_serializing_if = "Option::is_none")] pub github_wait_s: Option<u64>,
    /// What its runner logged while it was slow to listen for a job (more than ten seconds): each wait, with the
    /// line before and after, and its warnings and errors. As the machine sent it, cut to what a page can show.
    #[serde(default, skip_serializing_if = "Option::is_none")] pub log: Option<String>,
}

/// Where a job is kept: GitLab's job ids have their own space.
/// A job to run again after a spot interruption: GitHub's job (the one the machine was running, which may be another
/// with the same label), and how that stands ("pending", "asked", or why not).
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Rerun { pub job_id: u64, pub run_id: u64, pub repo: String, pub name: String, pub state: String, pub at_ms: u64 }

/// How long one request may go on trying AWS's regions for a machine before the rest is left for the job's next try.
const LAUNCH_BUDGET_MS: u64 = 15_000;

/// How a try ends when one request's worth of spot pools was asked and others are left: the job waits a moment and
/// goes on with those (the ones just refused are not asked again), before it goes on down the order to an on-demand
/// machine. Three rounds at most.
const MORE_SPOT: &str = "more spot pools to ask";

/// Where it is noted that a job ended at its code host before any runner took it, while its machine was still being
/// asked for (`gone:<job>`): the start under way stops there, or ends the machine it has just been given.
fn gone_key(j: &Job) -> String { format!("gone:{}", job_key(j).trim_start_matches("job:")) }

/// Where it is noted that AWS does not offer a machine type in a zone (it said "Unsupported"): not asked again for a day.
fn unoffered_key(region: &str, kind: &str, zone: &str) -> String { format!("aws:unoffered:{region}:{kind}:{zone}") }

fn is_zero(n: &u32) -> bool { *n == 0 }

/// Spot is paused for half an hour once AWS took back two machines within a quarter of an hour.
const SPOT_PAUSE: (usize, u64, u64) = (2, 15 * 60_000, 30 * 60_000);

/// Where a job to be run again off spot machines is noted until it arrives: by its repository, run and name (GitHub gives the
/// new attempt's job a new id).
fn rerun_key(repo: &str, run_id: u64, name: &str) -> String { format!("rerun:{repo}:{run_id}:{name}") }

/// How a failing runner's name ends (see `fail_fast`); a job's own runner's never does.
const FAILING: &str = "-x";
fn failing_runner(name: &str) -> bool { name.ends_with(FAILING) }

/// Where the time of a GitLab connection's last event is kept.
fn last_event_key(connection: &str) -> String { if connection.is_empty() { "gitlab:last_event".into() } else { format!("gitlab:last_event:{connection}") } }

/// An image address as the dashboard may set it: https, and nothing a shell would read as more than a word.
pub fn image_address(s: &str) -> Option<String> {
    let s = s.trim().trim_end_matches('/');
    (s.starts_with("https://") && s.len() <= 300 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b":/._-".contains(&b))).then(|| s.to_string())
}

pub fn job_key(j: &Job) -> String { if j.provider == "gitlab" { format!("job:gl{}", gitlab::local(&j.gitlab, j.job_id)) } else { format!("job:{}", j.job_id) } }

/// How long a job's record is kept once it is over (the dashboard's history; spend and usage are kept by the day).
pub const JOB_KEPT_MS: u64 = 90 * 86_400_000;
/// How long a job stays among the live ones once nothing more is done for it (machines counted against an account's
/// limits a little longer than they run).
const LIVE_AFTER_MS: u64 = 30 * 60_000;
/// How long a runner's name is kept with its job: past the longest job.
const RUNNER_KEPT_MS: u64 = MAX_JOB_MINUTES as u64 * 60_000 + 86_400_000;
/// Where it is said that the jobs' indexes are complete.
const INDEXED: &str = "indexed";
/// How long a day's totals are kept.
const DAY_KEPT_MS: u64 = 400 * 86_400_000;

/// Whether a job is over: no machine of its may still be coming or running, and it waits for nothing.
pub fn over(j: &Job) -> bool { !["waiting", "launching", "launched", "running", "orphan", "pending"].contains(&j.state.as_str()) }

/// Whether the sweep still has something to do for a job: one not over; one to be run again, or whose failure is
/// still to be said; an AWS machine whose sent data is still to be added to its cost; a job of GitHub's that ended
/// before any runner took it (its machine may still be coming); or one that ended a moment ago.
pub fn needs_care(j: &Job, now: u64) -> bool {
    let ended = j.ended_ms.unwrap_or(j.at_ms);
    !over(j) || j.rerun.as_ref().is_some_and(|r| r.state == "pending") || j.fail_pending.is_some()
        || (j.cloud == "aws" && j.cost_from.is_some() && j.sent_gb.is_none() && now < ended + 2 * 86_400_000)
        || (!(j.began || j.over || j.failed_fast) && now.saturating_sub(j.at_ms) < 86_400_000)
        || now < ended + LIVE_AFTER_MS
}

/// A day's totals (UTC), from the jobs queued in it: what each place's machines cost (see `place_of`), and its hours.
#[derive(Serialize, Deserialize, Clone, Default, Debug, PartialEq)]
pub struct Day { #[serde(default)] pub spend: std::collections::BTreeMap<String, f64>, #[serde(default)] pub hours: Vec<Hour> }

/// An hour of a day's jobs: how many were queued, ran and did not run; their machines' time and the minutes the code
/// host bills (each job rounded up); what they cost and what the code host's runners would have; each one's wait.
#[derive(Serialize, Deserialize, Clone, Default, Debug, PartialEq)]
pub struct Hour {
    #[serde(default)] pub jobs: u64, #[serde(default)] pub ran: u64, #[serde(default)] pub not_ran: u64, #[serde(default)] pub ms: u64,
    #[serde(default)] pub billed: u64, #[serde(default)] pub usd: f64, #[serde(default)] pub host_usd: f64, #[serde(default)] pub waits: Vec<u64>,
}

impl Hour {
    fn add(&mut self, o: &Hour) {
        (self.jobs, self.ran, self.not_ran, self.ms, self.billed) = (self.jobs + o.jobs, self.ran + o.ran, self.not_ran + o.not_ran, self.ms + o.ms, self.billed + o.billed);
        (self.usd, self.host_usd) = (self.usd + o.usd, self.host_usd + o.host_usd);
        self.waits.extend_from_slice(&o.waits);
    }
}

/// Where a day's totals are kept: by the day's number since the epoch, so they sort by time.
pub fn day_key(day: u64) -> String { format!("day:{day:06}") }

/// What one job adds to its hour (the hour it was queued in, counted from the epoch), as it stands at `now`.
pub fn hour_of(j: &Job, host_price: f64, now: u64) -> (u64, Hour) {
    let mut h = Hour { jobs: 1, usd: job_usd(j, now), ..Default::default() };
    if ["failed", "orphan", "swept"].contains(&j.state.as_str()) && j.started_ms.is_none() { h.not_ran = 1 }
    if let Some(start) = j.started_ms {
        let took = j.ended_ms.unwrap_or(if active(j) { now } else { start }).saturating_sub(start);
        let minutes = took.div_ceil(60_000).max(1);
        (h.ran, h.ms, h.billed, h.host_usd) = (1, took, minutes, minutes as f64 * host_price);
        h.waits.push(start.saturating_sub(j.queued_ms.unwrap_or(j.at_ms)));
    }
    (j.at_ms / 3_600_000, h)
}

/// Published prices, per hour, for the machines other clouds started before jobs were sized (jobs recorded without a
/// price): a Cloudflare container of 2 vCPU, 8 GiB, 16 GB (CPU counted as fully busy, so an upper bound) and a Modal
/// sandbox of 2 CPU cores and 8 GiB (sandbox rates).
pub const CLOUDFLARE_CONTAINER_USD_PER_HOUR: f64 = (2.0 * 0.000020 + 8.0 * 0.0000025 + 16.0 * 0.00000007) * 3600.0;
pub const MODAL_SANDBOX_USD_PER_HOUR: f64 = (2.0 * 0.00003942 + 8.0 * 0.00000667) * 3600.0;

/// A Cloudflare container of this size, per hour, at most: memory and disk are billed as provisioned, CPU only while
/// used (counted here as always busy).
pub fn cloudflare_usd_per_hour(size: Size) -> f64 { (size.cpu as f64 * 0.000020 + size.ram_gb as f64 * 0.0000025 + size.disk_gb as f64 * 0.00000007) * 3600.0 }
/// What Cloudflare bills for a container's measured usage (its usage analytics: CPU seconds used, memory and disk
/// byte-seconds provisioned, bytes sent), at list prices: the same numbers Cloudflare's own usage page shows.
pub fn cloudflare_measured_usd(cpu_secs: f64, memory_byte_secs: f64, disk_byte_secs: f64, tx_bytes: f64) -> f64 {
    cpu_secs * 0.000020 + memory_byte_secs / 1_073_741_824.0 * 0.0000025 + disk_byte_secs / 1e9 * 0.00000007 + tx_bytes / 1e9 * 0.025
}

/// What Cloudflare bills for a month of an account's containers, after the usage Workers Paid includes each month
/// (25 GiB-hours of memory, 375 vCPU-minutes, 200 GB-hours of disk, 1 TB sent from North America and Europe).
pub fn cloudflare_billed_usd(cpu_secs: f64, memory_byte_secs: f64, disk_byte_secs: f64, tx_bytes: f64) -> f64 {
    let over = |used: f64, included: f64| (used - included).max(0.0);
    cloudflare_measured_usd(over(cpu_secs, 375.0 * 60.0), over(memory_byte_secs, 25.0 * 3600.0 * 1_073_741_824.0), over(disk_byte_secs, 200.0 * 3600.0 * 1e9), over(tx_bytes, 1e12))
}

/// A Cloudflare container's price while its runner waits: memory and disk (CPU is billed as used, and it idles).
pub fn cloudflare_waiting_usd_per_hour(size: Size) -> f64 { (size.ram_gb as f64 * 0.0000025 + size.disk_gb as f64 * 0.00000007) * 3600.0 }

/// Cloudflare runs this many jobs at once unless the dashboard says otherwise (its containers have no cap of their own).
pub const CLOUDFLARE_JOBS_AT_ONCE: u32 = 20;

/// A Modal sandbox of this size, per hour (sandbox rates).
/// A Modal sandbox of `cpu` vCPUs (half as many of Modal's physical cores) and `ram_gb`, per hour, at Modal's sandbox
/// rates: $0.00003942 a core-second, $0.00000667 a GiB-second (held to its size, it is billed exactly that).
pub fn modal_usd_per_hour(cpu: u32, ram_gb: u32) -> f64 { (cpu as f64 / 2.0 * 0.00003942 + ram_gb as f64 * 0.00000667) * 3600.0 }

/// A Modal GPU's price per hour (Modal's per-second list prices; A100: the 40 GB one).
pub fn modal_gpu_usd_per_hour(gpu: Option<&str>) -> f64 {
    let per_second = match gpu { Some("t4") => 0.000164, Some("l4") => 0.000222, Some("a10g") => 0.000306, Some("l40s") => 0.000542, Some("a100") => 0.000583,
        Some("h100") => 0.001097, Some("h200") => 0.001261, Some("b200") => 0.001736, _ => 0.0 };
    per_second * 3600.0
}

/// A container or sandbox as the dashboard names it: "4cpu-16gb", "2cpu-8gb-t4".
fn machine_name(size: Size) -> String { format!("{}cpu-{}gb{}", size.cpu, size.ram_gb, size.gpu.map(|g| format!("-{g}")).unwrap_or_default()) }

/// How a try ends when starting the machine failed at more than one place (followed by what each said).
const SEVERAL: &str = "no provider could start its machine: ";

/// A place refusing what a job asks, as it would any job asking the same (no hiccup): no GPU machines allowed in the
/// AWS account, or a runner agent refusing the request (Modal: no GPUs without a payment method).
fn refuses_alike(e: &str) -> bool { e.starts_with(NO_GPU_QUOTA) || e.contains(" runner agent: 400 ") || e.starts_with("Modal refused: ") }

/// How a try ends when AWS has no spot machine for a job and nothing else in the order can run it (followed by what
/// each region said): the job waits, and is tried again, rather than failing.
const WAIT_FOR_SPOT: &str = "waiting: AWS has no spot machine for it now, and nothing else in the order can run it (";

/// How a try ends when every place AWS was asked refused for the account's own limits of running CPUs (followed by
/// what each said): the job waits while machines of this control plane's own are using them, as it would for a place
/// that is full; with none of its own running it is tried again and then fails, saying which limits to raise.
const AT_LIMIT: &str = "at the AWS account's limits: ";

/// How a try ends for a job kept off spot machines whose on-demand machine the account's limit refused (followed by
/// what each region said): it is asked on a spot machine again (see `Job::spot_again`).
const BACK_ON_SPOT: &str = "back on spot: ";

/// As `WAIT_FOR_SPOT`, when an on-demand machine was asked for too and refused only for the account's limit.
const WAIT_FOR_SPOT_AT_LIMIT: &str = "waiting: AWS has no spot machine for it now, and your AWS account's on-demand limit does not let an on-demand one start (";

/// What a job's machine is called in AWS's console (its Name tag): the CI's name, the repository and the job, so that
/// a list of machines says whose each is. Nothing reads it: a machine is known by its `superci-plane` and
/// `superci-job` tags, and GitHub knows the runner by another name, which carries the control plane's id. So a CI
/// that is given another name later just names its next machines by it.
pub fn machine_name_in_aws(ci: &str, repo: &str, job: u64) -> String {
    let ci = name_in_names(ci);
    let repo: String = repo.rsplit('/').next().unwrap_or_default().chars().filter(|c| c.is_ascii_alphanumeric() || "._-".contains(*c)).take(60).collect();
    [ci.as_str(), repo.as_str(), &job.to_string()].into_iter().filter(|part| !part.is_empty()).collect::<Vec<_>>().join("-")
}

/// A CI's name as it stands in the names of things made for it: lowercase letters and digits, a dash for anything
/// else (Acme CI: acme-ci).
fn name_in_names(ci: &str) -> String {
    let named: String = ci.trim().to_lowercase().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
    named.trim_matches('-').to_string()
}

/// How a runner's name begins where people read it (a job's log at GitHub says which runner took it; GitLab lists
/// its runners): the CI's name, then this control plane's id. `superci-<id>` unless the CI is named, as it was for
/// every CI before. Nothing reads a name back by how it begins (a runner is found by the whole name kept with its
/// job), so a CI given another name just names its next runners by it.
fn runner_prefix(ci: &str, plane_id: &str) -> String {
    let named = name_in_names(ci);
    format!("{}-{plane_id}", if named.is_empty() { "superci" } else { named.as_str() })
}

/// What a job that ran on-demand is told of spot when the account's AWS organization allows none (see `aws::refusal`).
pub const SPOT_NOT_ALLOWED: &str = "SpotNotAllowedByOrganization";

/// Where the control plane notes when its organization last refused a spot machine (see where it is read).
const SPOT_NOT_ALLOWED_KEY: &str = "aws:spot-not-allowed";

/// How a job fails when AWS lets the account run no GPU machines (followed by the quota to raise).
const NO_GPU_QUOTA: &str = "AWS lets this account run no GPU machines yet: ask AWS for more in Service Quotas, ";

fn cloud_name(cloud: &str) -> &str {
    match cloud { "aws" => "AWS", AWS_ON_DEMAND => "AWS on-demand", "cloudflare" => "Cloudflare", "modal" => "Modal", c => c }
}

/// AWS's on-demand machines, as a place of their own in the order (`aws` there is its spot machines): when no region
/// has a spot machine for a job, the job goes to the next place in the order that can run it, which may be this one.
pub const AWS_ON_DEMAND: &str = "aws-on-demand";

/// The place in the order a job's machine counts toward (its limits and its spend).
fn place_of(j: &Job) -> &str { if j.cloud == "aws" && j.on_demand { AWS_ON_DEMAND } else { &j.cloud } }

/// A runner agent in another cloud: it starts one runner per request from this control plane, which it trusts the way
/// AWS does, by this control plane's signed tokens (issuer = the control plane's URL, audience = the agent's URL).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Agent { pub cloud: String, pub url: String }

/// Where jobs run. A job naming a cloud in its label (`superci/provider=aws`) runs only there; else the first rule whose
/// repository pattern matches ("owner/repo", "owner/*" or "*") names its only cloud; else the places in `order` are tried
/// top to bottom: the first that can run the job's machine and is within its limits takes it. Without an order: the
/// default cloud, else every connected cloud (AWS, Cloudflare, agents).
///
/// AWS is two places: `aws`, its spot machines, and `AWS_ON_DEMAND`, its on-demand ones (right after `aws` unless the
/// order puts it elsewhere, or turns it off). A job goes to the next place that can run it when the one before cannot
/// start its machine: AWS has no spot machine for it, or starting it failed. With no spot machine and no other place,
/// it waits for one.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Routing {
    #[serde(default)]
    pub default: Option<String>,
    #[serde(default)]
    pub rules: Vec<Rule>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub order: Vec<Pool>,
    /// Public repositories whose jobs may run here (patterns as in rules). Others' jobs are refused: on a public
    /// repository anyone can open a pull request. Even here, runs from forks and `pull_request_target` are refused.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub public_repos: Vec<String>,
    /// The most CPUs a label may ask for (default `MAX_CPU`); memory at most 8 GB per CPU of it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_cpu: Option<u32>,
    /// The longest a job may run, in minutes (default `MAX_JOB_MINUTES`): its machine is ended after it. Never
    /// longer than its cloud lets a machine live (`max_minutes`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_minutes: Option<u32>,
    /// The most this CI may spend on machines in a month, all its providers together (USD, estimated as on the
    /// dashboard). At it no job is started, until the month ends or the limit is raised.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub monthly_usd: Option<f64>,
    /// The most a cloud account may spend on machines in a month, its providers together (by `account_of`: `aws` is
    /// AWS's spot and on-demand machines). At it, jobs go to the providers of other accounts in the order.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub account_usd: std::collections::BTreeMap<String, f64>,
    /// The same two for a day (UTC): a runaway day stops that day, not at the month's limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub daily_usd: Option<f64>,
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub account_daily_usd: std::collections::BTreeMap<String, f64>,
    /// A budget in the AWS account itself for its monthly limit, made by the dashboard with its owner's sign-in (the
    /// control plane has no part in it and no permission for it): AWS emails at 80% and at 100% of the account's
    /// real bill. Kept here so the pages say it without asking AWS.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aws_budget: Option<AwsBudget>,
    /// The cache (see `cache`): on unless turned off, and how much it keeps.
    #[serde(default, skip_serializing_if = "cache::Settings::is_default")]
    pub cache: cache::Settings,
}

/// What the AWS account's own budget does beside its emails.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct AwsBudget {
    /// At 100% AWS puts a rule on this control plane's role that refuses starting machines (see
    /// `aws::stop_policy_name`), and takes it off when the next month begins.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")] pub stop: bool,
}

/// What a job is told when AWS's budget has put its rule on this control plane's role.
pub const BUDGET_STOPPED: &str = "AWS's budget for this account reached its limit and stopped new machines there";

/// Where the control plane notes when AWS last refused a machine by the budget's rule (the dashboard says so).
const BUDGET_STOPPED_KEY: &str = "aws:budget-stopped";

/// Where the control plane notes a spot pool (a machine type in a zone of a region) that had no machine or took one
/// back, with when: the next jobs ask the other pools first.
fn pool_key(region: &str, kind: &str, zone: &str) -> String { format!("aws:pool:{region}:{kind}:{zone}") }

/// The day (UTC) a moment is in, as the moment it began.
pub fn day_start_ms(now_ms: u64) -> u64 { now_ms - now_ms % 86_400_000 }

/// The cloud account a place in the order spends in: AWS's on-demand machines are the same account's as its spot ones.
pub fn account_of(cloud: &str) -> &str { if cloud == AWS_ON_DEMAND { "aws" } else { cloud } }

/// A limit in dollars as it is said: whole dollars without cents ($200), another amount with them ($12.50).
pub fn dollars(usd: f64) -> String { if usd.fract() == 0.0 { format!("${usd:.0}") } else { format!("${usd:.2}") } }

/// What a month's spending by place (`month_spend`) comes to in one cloud account.
pub fn account_spend(spend: &HashMap<String, f64>, account: &str) -> f64 { spend.iter().filter(|(c, _)| account_of(c) == account).map(|(_, usd)| usd).sum() }

/// The longest a job may run unless the dashboard sets another: six hours, as on GitHub's own runners.
pub const MAX_JOB_MINUTES: u32 = 360;
/// The largest machine a label may ask for unless the dashboard sets another: 32 CPUs.
pub const MAX_CPU: u32 = 32;
/// Jobs at once in a cloud unless the dashboard sets another.
pub const JOBS_AT_ONCE: u32 = 20;

fn matches_repo(pattern: &str, repo: &str) -> bool {
    pattern == "*" || pattern.eq_ignore_ascii_case(repo)
        || pattern.strip_suffix("/*").is_some_and(|owner| repo.split('/').next().is_some_and(|o| o.eq_ignore_ascii_case(owner)))
}

/// A place in the order, with its limits: jobs at once, and spend this month (USD, estimated as on the dashboard).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Pool {
    pub cloud: String,
    /// Turned off in the dashboard: it takes no jobs (only `AWS_ON_DEMAND` can be, the others are removed instead).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")] pub off: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub max_jobs: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub monthly_usd: Option<f64>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Rule { pub repo: String, pub cloud: String }

impl Routing {
    /// The cloud this policy names for a repository, if any.
    pub fn cloud_for(&self, repo: &str) -> Option<&str> { self.rule_for(repo).or(self.default.as_deref()) }

    /// The cloud a repository's rule names (an exception to the order), if any.
    pub fn rule_for(&self, repo: &str) -> Option<&str> {
        self.rules.iter().find(|r| matches_repo(&r.repo, repo)).map(|r| r.cloud.as_str())
    }

    /// A public repository allowed to run jobs here.
    pub fn allows_public(&self, repo: &str) -> bool { self.public_repos.iter().any(|p| matches_repo(p, repo)) }

    /// Why a machine is larger than allowed, if it is.
    pub fn too_large(&self, spec: &Spec) -> Option<String> {
        let max = self.max_cpu.unwrap_or(MAX_CPU);
        if spec.cpu.is_some_and(|c| c > max) { return Some(format!("it asks for {} CPUs; the most allowed is {max} (set in the dashboard)", spec.cpu.unwrap_or_default())) }
        if spec.ram_gb.is_some_and(|g| g > max * 8) { return Some(format!("it asks for {} GB of memory; the most allowed is {} GB (set in the dashboard)", spec.ram_gb.unwrap_or_default(), max * 8)) }
        None
    }

    /// The limits for a cloud, from its place in the order.
    pub fn pool(&self, cloud: &str) -> Option<&Pool> { self.order.iter().find(|p| p.cloud == cloud) }
}

enum Runner { Aws(AwsConnection), Agent(Agent), Own }

/// Where a job goes: a place now, later (every place that could run it is full, over its limit or offline), or nowhere
/// (no connected place can run its machine), each with why.
enum Placement { Run(Vec<(String, Runner)>), Wait(String), Fail(String) }

/// The start of this month (UTC), in ms.
pub fn month_start_ms(now_ms: u64) -> u64 {
    let days = now_ms / 86_400_000;
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm), then back to the 1st of that month.
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let day = doy - (153 * ((5 * doy + 2) / 153) + 2) / 5; // 0-based day of month
    (days as i64 - day) as u64 * 86_400_000
}

/// A job's estimated machine cost so far (USD).
pub fn job_usd(j: &Job, now_ms: u64) -> f64 {
    if let Some(settled) = j.cost_usd { return settled }
    let Some(per_hour) = j.usd_per_hour else { return 0.0 };
    // Paid from when the machine started, whether or not a job came: an AWS machine boots billed, a sandbox is billed
    // for what it holds, a container for its memory and disk (and CPU as used) while its runner waits.
    let from = j.launched_ms.or(j.started_ms).unwrap_or(j.at_ms);
    let end = j.ended_ms.unwrap_or(if active(j) { now_ms } else { from });
    let began = j.started_ms.unwrap_or(end).clamp(from, end.max(from));
    (began.saturating_sub(from) as f64 * j.waiting_usd_per_hour.unwrap_or(per_hour) + end.saturating_sub(began) as f64 * per_hour) / 3_600_000.0
}

/// GitHub's price per minute for its hosted Linux runner with this many CPUs (2 and fewer: the standard runner), x64 or
/// arm64 (GitHub's prices from January 2026; each job is rounded up to whole minutes).
pub fn github_usd_per_minute(cpu: u32, arm64: bool) -> f64 {
    if arm64 { return match cpu { 0..=2 => 0.005, 3..=4 => 0.008, 5..=8 => 0.014, 9..=16 => 0.026, 17..=32 => 0.050, _ => 0.098 } }
    match cpu { 0..=2 => 0.006, 3..=4 => 0.012, 5..=8 => 0.022, 9..=16 => 0.042, 17..=32 => 0.082, 33..=64 => 0.162, _ => 0.252 }
}

/// GitHub's price per minute for its hosted Windows runner with this many CPUs (from GitHub's price list).
pub fn github_windows_usd_per_minute(cpu: u32) -> f64 {
    match cpu { 0..=2 => 0.010, 3..=4 => 0.022, 5..=8 => 0.042, 9..=16 => 0.082, 17..=32 => 0.162, _ => 0.322 }
}

/// GitLab.com's price per minute for its hosted runner of this many CPUs: compute minutes by the runner's size (one
/// a minute on the small one of 2 CPUs, two on the medium of 4, three on the large of 8, six on 16, twelve on 32),
/// each $10 for 1,000 bought beyond a plan's.
pub fn gitlab_usd_per_minute(cpu: u32) -> f64 {
    0.01 * match cpu { 0..=2 => 1.0, 3..=4 => 2.0, 5..=8 => 3.0, 9..=16 => 6.0, 17..=32 => 12.0, _ => 24.0 }
}

/// The periods usage is told for: a day (by the hour), a week and 30 days (by the day).
pub const USAGE_PERIODS: [(&str, u64, usize); 3] = [("day", 3_600_000, 24), ("week", 86_400_000, 7), ("month", 86_400_000, 30)];

/// What jobs used in a period up to now, and what the code host's own runners would have charged for them: how many
/// were queued, ran and did not run, their machines' minutes and the billed minutes (each job rounded up to a whole
/// minute, as GitHub and GitLab bill), what they cost and what the code host would have, how long they waited for a
/// machine; and the same for each hour or day of it, the oldest first. From each hour's totals (by the hour since
/// the epoch, see `hour_of`).
pub fn usage_of(hours: &std::collections::BTreeMap<u64, Hour>, now: u64, step_ms: u64, steps: usize) -> serde_json::Value {
    let (first, last) = ((now.saturating_sub(step_ms * steps as u64)) / 3_600_000 + 1, now / 3_600_000);
    let mut buckets = vec![Hour::default(); steps];
    let mut all = Hour::default();
    for (hour, h) in hours.range(first..=last) {
        let i = ((hour - first) * 3_600_000 / step_ms) as usize;
        buckets[i.min(steps - 1)].add(h);
        all.add(h);
    }
    let median = |w: &mut Vec<u64>| { w.sort_unstable(); w.get(w.len() / 2).copied() };
    let mut waits = all.waits.clone();
    waits.sort_unstable();
    serde_json::json!({
        "since_ms": first * 3_600_000, "step_ms": step_ms, "jobs": all.jobs, "ran": all.ran, "not_ran": all.not_ran,
        "machine_minutes": (all.ms as f64 / 60_000.0 * 10.0).round() / 10.0, "billed_minutes": all.billed, "usd": all.usd, "host_usd": all.host_usd,
        "wait_median_ms": waits.get(waits.len() / 2), "wait_fastest_ms": waits.first(), "wait_slowest_ms": waits.last(),
        "steps": buckets.iter_mut().map(|h| serde_json::json!({ "ran": h.ran, "not_ran": h.not_ran, "minutes": (h.ms as f64 / 60_000.0 * 10.0).round() / 10.0, "usd": h.usd, "host_usd": h.host_usd, "wait_median_ms": median(&mut h.waits) })).collect::<Vec<_>>(),
    })
}

/// The same from the jobs themselves.
pub fn usage(jobs: &[Job], host_price: impl Fn(&Job) -> f64, now: u64, step_ms: u64, steps: usize) -> serde_json::Value {
    let mut hours: std::collections::BTreeMap<u64, Hour> = std::collections::BTreeMap::new();
    for j in jobs.iter().filter(|j| j.at_ms <= now) { let (hour, h) = hour_of(j, host_price(j), now); hours.entry(hour).or_default().add(&h) }
    usage_of(&hours, now, step_ms, steps)
}

/// What a cloud's machines can be (the clouds this control plane starts machines in).
fn capacity_of(cloud: &str) -> Capacity {
    match cloud { "aws" => Capacity::aws(), "modal" => Capacity::modal(), _ => Capacity::cloudflare() }
}

fn active(j: &Job) -> bool { ["launching", "launched", "running"].contains(&j.state.as_str()) }

pub struct ControlPlane<'a> {
    pub store: &'a dyn Store,
    pub http: &'a dyn Http,
    pub clock: &'a dyn Clock,
    pub timer: &'a dyn Timer,
    pub config: &'a Config,
    pub cache: &'a Cache,
    /// The runtime's own containers, when it has them.
    pub containers: Option<&'a dyn Containers>,
}

/// A key that only reads, from the name and value it is stored under (`READ_KEY_<expires, unix seconds>_<NAME>`, the
/// key's SHA-256): when it expires (ms), its name, the hash. A removed one (emptied, where a store keeps names) is none.
/// A key that may also start runs from a computer says so after the hash, `<hash>:runs` (see `RunKey`), and is kept
/// whole.
pub fn read_key(name: &str, value: &str) -> Option<(u64, String, String)> {
    let (until, key_name) = name.strip_prefix("READ_KEY_")?.split_once('_')?;
    let (hash, more) = value.split_once(':').unwrap_or((value, ""));
    if hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) { return None }
    if !more.is_empty() && RunKey::of(more).is_none() { return None }
    Some((until.parse::<u64>().ok()? * 1000, key_name.to_string(), value.to_ascii_lowercase()))
}

/// A key for coding agents: it may also start runs from a computer. It has no limits of its own: its jobs are any
/// job of this control plane's, within its limits (the largest machine, what may be spent in a month and a day).
#[derive(Clone, Debug, PartialEq)]
pub struct RunKey { pub name: String }

impl RunKey {
    /// From what is kept after the key's hash: `runs`.
    fn of(more: &str) -> Option<RunKey> { (more == "runs").then(|| RunKey { name: String::new() }) }
}

fn nothing_here() -> Response {
    Response::text(404, "SuperCI plane: nothing here (set up and changed from the SuperCI dashboard on your machine)")
}

fn origin(url: &str) -> String {
    url::Url::parse(url).map(|u| u.origin().ascii_serialization()).unwrap_or_default()
}

/// How a GitLab job ended, in the words GitHub's jobs say it (`success`, `failure`, `cancelled`).
fn gitlab_conclusion(status: &str) -> &'static str { match status { "success" => "success", "canceled" => "cancelled", _ => "failure" } }

/// The role the dashboard makes in an AWS account for a control plane (`connect_runners`).
pub fn role_arn(account_id: &str, plane_id: &str) -> String {
    format!("arn:aws:iam::{account_id}:role/superci-plane-{plane_id}")
}

impl<'a> ControlPlane<'a> {
    /// The control plane's signing key: made once, kept in its storage; only the public half is ever served.
    async fn identity(&self) -> Result<Identity> {
        if let Some(i) = get_json::<Identity>(self.store, "identity").await? { return Ok(i); }
        let i = Identity { signing: SigningKeyStore::generate() };
        put_json(self.store, "identity", &i).await?;
        Ok(i)
    }

    pub async fn handle(&self, req: Request) -> Response {
        match self.route(req).await {
            Ok(r) => r,
            Err(e) => Response::text(500, &e.chars().take(300).collect::<String>()),
        }
    }

    async fn route(&self, req: Request) -> Result<Response> {
        let url = url::Url::parse(&req.url).map_err(|e| e.to_string())?;
        let plane_url = origin(&req.url);
        if get_json::<String>(self.store, "plane_url").await?.as_deref() != Some(plane_url.as_str()) { put_json(self.store, "plane_url", &plane_url).await? }
        match (req.method.as_str(), url.path()) {
            ("GET", "/.well-known/openid-configuration") => Ok(Response::json(&serde_json::json!({
                "issuer": plane_url, "jwks_uri": format!("{plane_url}/.well-known/jwks.json"), "response_types_supported": ["id_token"],
                "subject_types_supported": ["public"], "id_token_signing_alg_values_supported": ["ES256"], "claims_supported": ["sub", "aud", "iss", "exp", "iat"] }))),
            ("GET", "/.well-known/jwks.json") => Ok(Response::json(&serde_json::json!({ "keys": [self.identity().await?.signing.public_jwk()?] }))),
            ("GET", "/health") => {
                let aws = self.aws_connection().await?.is_some_and(|a| a.connected);
                let runners = aws || !self.config.agents.is_empty() || self.own_containers();
                let moved_to = get_json::<String>(self.store, "moved_to").await?;
                let mut health = serde_json::json!({ "plane": self.config.plane_id, "label": self.config.label, "version": VERSION, "github": self.config.app.is_some(), "gitlab": self.config.gitlabs().next().is_some(), "installed": self.installed().await, "aws": aws, "runners": runners });
                // Moved: another control plane took over (the dashboard shows it as not in use). Standby: taking over,
                // not yet in use.
                if let Some(to) = moved_to { health["moved_to"] = to.into() }
                if self.store.get("standby").await?.is_some() { health["standby"] = true.into() }
                Ok(Response::json(&health))
            }
            ("GET", "/status") => self.status(&req).await,
            // Moved: events that still arrive here (sent just before the switch) go on to the new control plane.
            ("POST", "/webhook") | ("POST", "/gitlab/webhook") if self.store.get("moved_to").await?.is_some() => self.forward(&req, url.path()).await,
            ("POST", "/webhook") => self.webhook(&req, &plane_url).await,
            ("POST", "/gitlab/webhook") => self.gitlab_webhook(&req, &plane_url).await,
            ("POST", "/aws/callback") => self.aws_callback(&req, &plane_url).await,
            // A spot machine saying AWS is taking it back (only it has its job's link).
            ("POST", "/interrupted") => {
                let q: HashMap<String, String> = url.query_pairs().into_owned().collect();
                // The job its machine is on now: a GitHub runner may have taken another than the one it was started
                // for (found by the runner's name); a GitLab machine's record is its own.
                let found = match q.get("gl") {
                    Some(id) if !id.is_empty() && id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-') => get_json::<Job>(self.store, &format!("job:gl{id}")).await?,
                    Some(_) => None,
                    None => self.by_runner(q.get("runner").map(String::as_str)).await?,
                };
                let Some(mut j) = found else { return Ok(nothing_here()) };
                if !j.notice.as_deref().is_some_and(|n| n.len() >= 16 && q.get("t").is_some_and(|t| safe_eq(t.as_bytes(), n.as_bytes()))) { return Ok(nothing_here()) }
                if !j.interrupted && active(&j) {
                    j.interrupted = true;
                    self.keep(&job_key(&j), &j).await?;
                    // The pool that took it back is asked last for a while (it is short of machines now).
                    if let (Some(region), Some(kind), Some(zone)) = (&j.region, &j.machine_type, &j.zone) { let _ = self.store.put(&pool_key(region, kind, zone), self.clock.now_ms().to_string()).await; }
                    // One record each (two machines may say so at once), counted by `spot_paused`.
                    let now = self.clock.now_ms();
                    put_json(self.store, &format!("spot:interrupted:{now}:{}", j.job_id), &now).await?;
                }
                Ok(Response::text(200, "ok"))
            }
            // A machine saying how its own start went (see `MachineStart`): with its link, and only what it may say.
            ("POST", "/hello") => {
                let q: HashMap<String, String> = url.query_pairs().into_owned().collect();
                let found = match q.get("gl") {
                    Some(id) if !id.is_empty() && id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-') => get_json::<Job>(self.store, &format!("job:gl{id}")).await?,
                    Some(_) => None,
                    None => self.by_runner(q.get("runner").map(String::as_str)).await?,
                };
                let Some(j) = found else { return Ok(nothing_here()) };
                if !j.hello.as_deref().is_some_and(|n| n.len() >= 16 && q.get("t").is_some_and(|t| safe_eq(t.as_bytes(), n.as_bytes()))) { return Ok(nothing_here()) }
                // (Times near now only: a machine's clock is set by AWS before its script runs.)
                let now = self.clock.now_ms();
                let ms = |k: &str| q.get(k).and_then(|v| v.parse::<u64>().ok()).filter(|t| t.abs_diff(now) < 3_600_000);
                // What its runner wrote of a step so far (a page of its log, from `at` on), kept for its page here.
                if let Some(file) = q.get("live").filter(|f| f.len() <= 120 && f.ends_with(".log") && f.bytes().all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))) {
                    let at = q.get("at").and_then(|a| a.parse::<u64>().ok()).unwrap_or(0);
                    if !req.body.is_empty() && req.body.len() <= 4_000_000 { self.keep_live(job_key(&j).trim_start_matches("job:"), file, at, &req.body, &plane_url, now).await? }
                    return Ok(Response::text(200, "ok"))
                }
                let key = format!("hello:{}", job_key(&j).trim_start_matches("job:"));
                let mut said = get_json::<MachineStart>(self.store, &key).await?.unwrap_or_default();
                if let Some(script) = ms("script") { said.script_ms = script; said.boot_s = q.get("boot").and_then(|b| b.parse::<f64>().ok()).filter(|b| (0.0..3600.0).contains(b)).unwrap_or_default() }
                said.read_ms = ms("read").or(said.read_ms);
                said.ready_ms = ms("ready").or(said.ready_ms);
                said.listening_ms = ms("listening").or(said.listening_ms);
                said.github_wait_s = q.get("gh").and_then(|v| v.parse::<u64>().ok()).filter(|s| (1..=60).contains(s)).or(said.github_wait_s);
                // (Text only, and so much of it: lines of a log, each cut where a page would cut it.)
                if q.get("log").is_some_and(|l| l == "runner") && !req.body.is_empty() {
                    let text: String = String::from_utf8_lossy(&req.body).chars().filter(|c| *c == '\n' || (!c.is_control() && *c != '\u{fffd}')).take(16_000).collect();
                    said.log = Some(text.lines().map(|l| l.chars().take(300).collect::<String>()).collect::<Vec<_>>().join("\n")).filter(|t| !t.trim().is_empty());
                }
                self.store.put_until(&key, serde_json::to_string(&said).map_err(|e| e.to_string())?, now + JOB_KEPT_MS).await?;
                Ok(Response::text(200, "ok"))
            }
            // GitLab's projects, for the dashboard: which send their jobs here; switching one on or off; after the
            // connection changes, every webhook here gets the new secret.
            ("GET", "/job/log") if self.reader(&req) => self.job_log(&url).await,
            // What GitHub says of the events it sent here lately, for whoever may read: whether each arrived, and (one
            // by its id) where it was sent and what was answered. To see why a job never got a machine.
            ("GET", "/github/deliveries") if self.reader(&req) => {
                let id = url.query_pairs().find(|(k, _)| k == "id").and_then(|(_, v)| v.parse::<u64>().ok());
                let mut out = vec![];
                for app in self.config.apps() { out.push(serde_json::json!({ "app": app.slug, "deliveries": github::deliveries(self.http, app, id, self.clock.now_ms()).await.unwrap_or_else(|e| serde_json::json!({ "error": e })) })) }
                Ok(Response::json(&serde_json::Value::Array(out)))
            }
            ("GET", "/gitlab/projects") if self.reader(&req) => {
                // Of one connection (`g`: its name; nothing: the first).
                let which = url.query_pairs().find(|(k, _)| k == "g").map(|(_, v)| v.into_owned()).unwrap_or_default();
                let gl = self.gitlab_by(&which).ok_or("GitLab is not connected")?;
                let list = gitlab::projects(self.http, gl, &format!("{plane_url}/gitlab/webhook")).await?;
                // The last GitLab event that arrived, so the dashboard can say whether webhooks reach here.
                let last = get_json::<u64>(self.store, &last_event_key(&gl.id)).await?;
                Ok(Response::json(&serde_json::json!({ "projects": list, "last_event_ms": last })))
            }
            ("POST", "/gitlab/projects") if self.dashboard(&req) => {
                let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap_or_default();
                let gl = self.gitlab_by(body["gitlab"].as_str().unwrap_or_default()).ok_or("GitLab is not connected")?;
                let hook = format!("{plane_url}/gitlab/webhook");
                let ids: Vec<(u64, bool)> = if body["refresh"] == true {
                    gitlab::projects(self.http, gl, &hook).await?.into_iter().filter(|p| p.enabled).map(|p| (p.id, true)).collect()
                } else if body["all_off"] == true {
                    gitlab::projects(self.http, gl, &hook).await?.into_iter().filter(|p| p.enabled).map(|p| (p.id, false)).collect()
                } else if body["all_on"] == true {
                    gitlab::projects(self.http, gl, &hook).await?.into_iter().filter(|p| !p.enabled).map(|p| (p.id, true)).collect()
                } else { vec![(body["id"].as_u64().ok_or("which project?")?, body["enabled"] == true)] };
                for (id, on) in &ids { gitlab::set_project(self.http, gl, *id, &hook, *on).await? }
                Ok(Response::json(&serde_json::json!({ "changed": ids.len() })))
            }
            // Moving to another control plane (see `move_out`, `move_in`, `claim`).
            ("POST", "/move/export") if self.dashboard(&req) => self.move_out(&req).await,
            ("POST", "/move/import") if self.dashboard(&req) => self.move_in(&req).await,
            ("POST", "/move/claim") if self.dashboard(&req) => self.claim(&req, &plane_url).await,
            ("POST", "/leave") if self.dashboard(&req) => self.leave(&plane_url).await,
            ("POST", "/move/away") if self.dashboard(&req) => {
                let to = serde_json::from_slice::<serde_json::Value>(&req.body).unwrap_or_default()["to"].as_str().unwrap_or_default().to_string();
                if !to.starts_with("https://") { return Ok(Response::text(400, "where to?")) }
                put_json(self.store, "moved_to", &to).await?;
                Ok(Response::json(&serde_json::json!({ "moved_to": to })))
            }
            // The public repositories the App is installed on, for the dashboard to allow them one by one (read again
            // after five minutes).
            ("GET", "/github/public-repos") if self.dashboard(&req) => {
                let now = self.clock.now_ms();
                if let Some((repos, until)) = self.cache.public_repos.borrow().clone() { if now < until { return Ok(Response::json(&serde_json::json!({ "repos": repos }))) } }
                if self.config.app.is_none() { return Err("no GitHub App".into()) }
                // Each App by itself: one that GitHub no longer answers for (deleted there) does not hide the others'.
                let (mut repos, mut failed) = (vec![], vec![]);
                for app in self.config.apps() {
                    let of_app: Result<Vec<String>> = async {
                        let mut out = vec![];
                        for i in github::installations(self.http, app, now).await? {
                            let token = self.installation_token(app, i.id).await?;
                            out.extend(github::public_repositories(self.http, &app.api(), &token).await?);
                        }
                        Ok(out)
                    }.await;
                    match of_app { Ok(r) => repos.extend(r), Err(e) => failed.push(format!("{}: {}", app.owner, e.chars().take(200).collect::<String>())) }
                }
                if repos.is_empty() && !failed.is_empty() { return Err(failed.join("; ")) }
                repos.sort();
                if failed.is_empty() { *self.cache.public_repos.borrow_mut() = Some((repos.clone(), now + 300_000)); }
                Ok(Response::json(&serde_json::json!({ "repos": repos, "failed": failed })))
            }
            // The dashboard gave a cloud the permissions this version asks for: what it refused before is forgotten.
            // An organization removed: its App (one of the further ones) is uninstalled where it is installed. The
            // dashboard then drops its secret; the App itself is deleted on GitHub.
            ("POST", "/github/uninstall") if self.dashboard(&req) => {
                let id = serde_json::from_slice::<serde_json::Value>(&req.body).unwrap_or_default()["app"].as_u64();
                let Some(app) = self.config.more_apps.iter().find(|a| Some(a.id) == id) else { return Ok(Response::text(404, "no such App here")) };
                let now = self.clock.now_ms();
                // An App already deleted on GitHub has nothing left to uninstall: said, not an error (the dashboard
                // drops its secret either way).
                let (mut installations, mut error) = (0, None);
                match github::installations(self.http, app, now).await {
                    Ok(list) => for i in list { match github::delete_installation(self.http, app, i.id, now).await { Ok(()) => installations += 1, Err(e) => error = Some(e) } },
                    Err(e) => error = Some(e),
                }
                Ok(Response::json(&serde_json::json!({ "github_installations": installations, "error": error })))
            }
            // AWS runners removed (the dashboard deletes the role and network with the account's sign-in): the connection
            // is forgotten, so no job goes there; adding AWS again connects anew.
            ("POST", "/aws/forget") if self.dashboard(&req) => {
                self.store.delete("aws").await?;
                self.cache.creds.replace(None);
                self.cache.network.borrow_mut().clear();
                self.cache.image.borrow_mut().clear();
                Ok(Response::json(&serde_json::json!({ "aws": false })))
            }
            ("POST", "/permissions/given") if self.dashboard(&req) => {
                let cloud = serde_json::from_slice::<serde_json::Value>(&req.body).unwrap_or_default()["cloud"].as_str().unwrap_or_default().to_string();
                let mut cleared = 0;
                for (k, _) in self.store.list(&format!("denied:{cloud}:")).await? { self.store.delete(&k).await?; cleared += 1 }
                Ok(Response::json(&serde_json::json!({ "cleared": cleared })))
            }
            // Costs the dashboard read from a cloud's own metering (Cloudflare's usage analytics), kept with the jobs.
            ("POST", "/costs") if self.dashboard(&req) => {
                let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap_or_default();
                let mut kept = 0;
                for (key, usd) in body["measured"].as_object().into_iter().flatten() {
                    let Some(usd) = usd.as_f64().filter(|u| u.is_finite() && (0.0..1000.0).contains(u)) else { continue };
                    if !key.starts_with("job:") { continue }
                    let Some(mut j) = get_json::<Job>(self.store, key).await? else { continue };
                    if j.cloud != "cloudflare" || active(&j) { continue }
                    j.cost_usd = Some(usd);
                    j.cost_from = Some("measured".into());
                    self.keep(key, &j).await?;
                    kept += 1;
                }
                Ok(Response::json(&serde_json::json!({ "kept": kept })))
            }
            // Runs from a computer: the working tree in parts, a run, how it goes; and its runner's calls.
            ("POST", "/runs/tree") if self.dashboard(&req) || self.run_key(&req).is_some() => self.tree_part(&url, &req, &plane_url).await,
            ("POST", "/runs") if self.dashboard(&req) => self.create_run(&req, &plane_url, None).await,
            ("POST", "/runs") if self.run_key(&req).is_some() => { let k = self.run_key(&req); self.create_run(&req, &plane_url, k).await }
            ("GET", p) if p.starts_with("/runs/") && p.ends_with("/artifacts") && self.reader(&req) => self.run_artifacts(p, &url, &plane_url).await,
            ("GET", p) if p.starts_with("/runs/") && p.ends_with("/log") && self.reader(&req) => self.run_log(p, &url).await,
            ("GET", p) if p.starts_with("/runs/") && self.reader(&req) => self.run_status(p, &url).await,
            (_, p) if p.starts_with("/runner/") => self.for_runner(&req, &plane_url, &url).await,
            // A machine's `superci-cache` (with the link only it has), for a job's cache.
            ("POST", p) if p.starts_with("/cache/") => self.cache_route(&req, &plane_url, p).await,
            ("GET", p) if p.starts_with("/cache/") && p.ends_with("/program") => self.cache_program(&plane_url, p).await,
            // The cache's entries (by repository, or one repository's), and taking some out.
            ("GET", "/caches") if self.reader(&req) => self.caches(&url).await,
            ("POST", "/caches/delete") if self.dashboard(&req) => self.caches_delete(&req, &plane_url).await,
            _ => Ok(nothing_here()),
        }
    }

    /// An event for the control plane this one moved to, passed on as it came (its signature still holds there).
    async fn forward(&self, req: &Request, path: &str) -> Result<Response> {
        let to = get_json::<String>(self.store, "moved_to").await?.ok_or("not moved")?;
        // Only events that really come from GitHub or GitLab (as here before the move).
        let genuine = if path == "/webhook" { self.config.apps().any(|a| verify_hub_signature(&a.webhook_secret, &req.body, req.header("x-hub-signature-256"))) }
            else { self.gitlab_signed(req).is_some() };
        if !genuine { return Ok(Response::text(401, "bad signature")) }
        let mut out = Request::new("POST", &format!("{to}{path}")).with_body(req.body.clone());
        for (k, v) in &req.headers {
            if k.starts_with("x-hub-") || k.starts_with("x-github-") || k.starts_with("x-gitlab-") || k == "content-type" || k == "user-agent" { out = out.with_header(k, v) }
        }
        let r = self.http.send(out).await?;
        Ok(Response::text(r.status, &r.body_text()))
    }

    /// Stopping SuperCI: GitLab's projects stop sending jobs here (their webhooks go) and the project runners it
    /// made are removed; GitHub's App is removed from where it is installed. What it did, for the dashboard.
    async fn leave(&self, plane_url: &str) -> Result<Response> {
        let now = self.clock.now_ms();
        let (mut projects, mut runners, mut installations) = (0, 0, 0);
        for gl in self.config.gitlabs() {
            let hook = format!("{plane_url}/gitlab/webhook");
            for p in gitlab::projects(self.http, gl, &hook).await?.into_iter().filter(|p| p.enabled) {
                gitlab::set_project(self.http, gl, p.id, &hook, false).await?;
                projects += 1;
            }
            // Its runners (each record says whose it is; ones from before there could be several: the first's).
            for (k, v) in self.store.list("glrunner:").await? {
                let v = serde_json::from_str::<serde_json::Value>(&v).unwrap_or_default();
                if v["gitlab"].as_str().unwrap_or_default() != gl.id { continue }
                if let Some(id) = v["id"].as_u64() { gitlab::delete_runner(self.http, gl, id).await?; runners += 1 }
                self.store.delete(&k).await?;
            }
        }
        // Each App by itself (one deleted on GitHub already does not keep the others installed).
        let mut failed = vec![];
        for app in self.config.apps() {
            match github::installations(self.http, app, now).await {
                Ok(list) => for i in list { match github::delete_installation(self.http, app, i.id, now).await { Ok(()) => installations += 1, Err(e) => failed.push(format!("{}: {e}", app.owner)) } },
                Err(e) => failed.push(format!("{}: {e}", app.owner)),
            }
        }
        Ok(Response::json(&serde_json::json!({ "gitlab_projects": projects, "gitlab_runners": runners, "github_installations": installations, "github_failed": failed })))
    }

    /// Whether a request carries this control plane's one-time move token (set through the cloud's API), unused.
    async fn move_token_ok(&self, req: &Request) -> Result<bool> {
        let (Some(want), Some(given)) = (&self.config.move_token, req.header("x-move-token")) else { return Ok(false) };
        if want.len() < 16 || !safe_eq(given.as_bytes(), want.as_bytes()) { return Ok(false) }
        // Each token is taken once.
        self.store.put_if_absent(&format!("move_token_used:{}", sha256_hex(want.as_bytes())), "1".into()).await
    }

    /// Moving away, step one: everything another control plane needs to take over, for the dashboard to hand it there
    /// (the App with its key, GitLab, the order and the default machine; GitLab's project runners and history). Only
    /// with the one-time move token.
    async fn move_out(&self, req: &Request) -> Result<Response> {
        if !self.move_token_ok(req).await? { return Ok(Response::text(403, "no move token, or one used already")) }
        let records = |prefix: &'static str| async move { self.store.list(prefix).await };
        Ok(Response::json(&serde_json::json!({
            "app": self.config.app, "more_apps": self.config.more_apps, "gitlab": self.config.gitlab, "more_gitlabs": self.config.more_gitlabs, "routing": self.config.routing, "machine": self.config.machine,
            // GitLab's project runners, and jobs that have finished (ones still running finish here).
            "state": records("glrunner:").await?.into_iter()
                .chain(records("job:").await?.into_iter().filter(|(_, v)| serde_json::from_str::<Job>(v).is_ok_and(|j| ["done", "failed", "cancelled", "swept", "orphan"].contains(&j.state.as_str()))))
                .collect::<Vec<_>>(),
        })))
    }

    /// Moving here: the GitLab project runners and history of the control plane before (what is here already stays).
    async fn move_in(&self, req: &Request) -> Result<Response> {
        if !self.move_token_ok(req).await? { return Ok(Response::text(403, "no move token, or one used already")) }
        let body: serde_json::Value = serde_json::from_slice(&req.body).map_err(|e| e.to_string())?;
        let mut taken = 0;
        for pair in body["state"].as_array().into_iter().flatten() {
            let (Some(k), Some(v)) = (pair[0].as_str(), pair[1].as_str()) else { continue };
            if !(k.starts_with("glrunner:") || k.starts_with("job:")) { continue }
            // A job comes with its indexes and counted in its day's totals here (its count there is not this one's).
            let job = k.starts_with("job:").then(|| serde_json::from_str::<Job>(v).ok()).flatten();
            let Some(mut j) = job else { if self.store.put_if_absent(k, v.to_string()).await? { taken += 1 } continue };
            j.counted_usd = None;
            let add = self.counting(&mut j);
            if self.store.swap_until(k, None, serde_json::to_string(&j).map_err(|e| e.to_string())?, Self::job_end(&j, self.clock.now_ms())).await? {
                self.indexes(k, &j, true, add).await?;
                taken += 1;
            }
        }
        // Not in use until the switch (`claim`).
        self.store.put("standby", "1".into()).await?;
        Ok(Response::json(&serde_json::json!({ "taken": taken })))
    }

    /// Moving here, the switch: GitHub's App sends its jobs here, and GitLab projects that sent theirs to the control
    /// plane before (`from`) send them here instead.
    async fn claim(&self, req: &Request, plane_url: &str) -> Result<Response> {
        let from = serde_json::from_slice::<serde_json::Value>(&req.body).unwrap_or_default()["from"].as_str().unwrap_or_default().trim_end_matches('/').to_string();
        let now = self.clock.now_ms();
        // Every App's webhook, each by itself; one that would not move is said (its organization's jobs still go to
        // the control plane before, which passes them on).
        let mut failed = vec![];
        for app in self.config.apps() {
            if let Err(e) = github::set_webhook_url(self.http, app, &format!("{plane_url}/webhook"), now).await { failed.push(format!("{}: {}", app.owner, e.chars().take(200).collect::<String>())) }
        }
        if !failed.is_empty() && failed.len() == self.config.apps().count() { return Err(failed.join("; ")) }
        let mut projects = 0;
        for gl in self.config.gitlabs().filter(|_| from.starts_with("https://")) {
            let (old, new) = (format!("{from}/gitlab/webhook"), format!("{plane_url}/gitlab/webhook"));
            for p in gitlab::projects(self.http, gl, &old).await?.into_iter().filter(|p| p.enabled) {
                gitlab::set_project(self.http, gl, p.id, &new, true).await?;
                gitlab::set_project(self.http, gl, p.id, &old, false).await?;
                projects += 1;
            }
        }
        // In use (again, when moving back to a control plane moved away from before).
        self.store.delete("moved_to").await?;
        self.store.delete("standby").await?;
        // GitHub can lose an event made in the seconds its App's address changes (seen: a job queued four seconds
        // after, never sent anywhere): for a while, jobs it did not tell of are looked for more often (`untold`).
        put_json(self.store, "claimed_at", &now).await?;
        self.timer.wake_in(60_000).await?;
        Ok(Response::json(&serde_json::json!({ "github": self.config.app.is_some(), "github_apps": self.config.apps().count(), "github_failed": failed, "gitlab_projects": projects })))
    }

    /// Whether a request carries a dashboard session's key that has not expired.
    fn dashboard(&self, req: &Request) -> bool {
        let given = req.header("authorization").and_then(|v| v.strip_prefix("Bearer ")).unwrap_or("");
        let now = self.clock.now_ms();
        self.config.dashboard_keys.iter().any(|(until, key)| *until > now && key.len() >= 16 && safe_eq(given.as_bytes(), key.as_bytes()))
    }

    /// How long a job's machine lives in a cloud, in minutes: the longest a job may run as set (six hours unless set),
    /// and never more than that cloud lets a machine live. Enforced by the machine itself or its cloud, so it holds
    /// with the control plane gone; a job's own time limit (`timeout-minutes`) ends it sooner.
    fn machine_minutes(&self, cloud: &str) -> u32 { job_minutes(self.config.routing.max_minutes, cloud) }

    /// Whether a request may read: a dashboard session's key, or a key that only reads and has not expired.
    fn reader(&self, req: &Request) -> bool {
        if self.dashboard(req) { return true }
        let given = req.header("authorization").and_then(|v| v.strip_prefix("Bearer ")).unwrap_or("");
        if given.len() < 16 { return false }
        let (hash, now) = (sha256_hex(given.as_bytes()), self.clock.now_ms());
        self.config.read_keys.iter().any(|(until, _, kept)| *until > now && safe_eq(hash.as_bytes(), kept.split(':').next().unwrap_or_default().as_bytes()))
    }

    /// A key for coding agents the request comes with, one that may start runs from a computer (see `RunKey`), and
    /// has not expired.
    fn run_key(&self, req: &Request) -> Option<RunKey> {
        let given = req.header("authorization").and_then(|v| v.strip_prefix("Bearer ")).unwrap_or("");
        if given.len() < 16 { return None }
        let (hash, now) = (sha256_hex(given.as_bytes()), self.clock.now_ms());
        self.config.read_keys.iter().filter(|(until, _, _)| *until > now).find_map(|(_, name, kept)| {
            let (h, more) = kept.split_once(':')?;
            if !safe_eq(hash.as_bytes(), h.as_bytes()) { return None }
            RunKey::of(more).map(|_| RunKey { name: name.clone() })
        })
    }

    /// A job's log, for whoever may read: GitHub's for the job (with the App's token for its repository), or GitLab's
    /// trace. Its end (`bytes`: how much, a megabyte at most), since the end says why a job failed.
    async fn job_log(&self, url: &url::Url) -> Result<Response> {
        let q = |n: &str| url.query_pairs().find(|(k, _)| k == n).map(|(_, v)| v.into_owned());
        let id = q("id").filter(|i| !i.is_empty() && i.bytes().all(|b| b.is_ascii_digit())).ok_or("which job?")?;
        // A GitLab job is kept under its connection's name and its id (`gl`: the name; nothing for the first).
        let key = match q("gl") { Some(g) if gitlab::valid_id(&g) || g.is_empty() => format!("job:gl{}", gitlab::local(&g, id.parse().unwrap_or_default())), Some(_) => return Err("which GitLab?".into()), None => format!("job:{id}") };
        let Some(j) = get_json::<Job>(self.store, &key).await? else { return Ok(Response::json(&serde_json::json!({ "error": "This control plane has no such job (it keeps the latest ones)." }))) };
        // With `link`: where GitHub keeps a job's whole log, for it to be fetched from there whatever its size.
        if q("link").is_some() && j.provider.is_empty() {
            let link = match (self.app_of(&j), self.installation_token_for(&j).await) { (Some(app), Some(token)) => github::job_log_link(self.http, &app.api(), &token, &j.repo, j.job_id).await, _ => Err("GitHub gave no token for its repository".into()) };
            return Ok(Response::json(&match link { Ok(url) => serde_json::json!({ "url": url }), Err(e) => serde_json::json!({ "error": e }) }))
        }
        // A run from a computer's: links to each piece of each step's whole log in the bucket, in order (whatever
        // its size, nothing through here); none when a piece is kept in the store instead.
        if q("link").is_some() && j.provider == "local" {
            return Ok(Response::json(&match self.log_links(j.local.as_deref().unwrap_or_default()).await? { Some(urls) => serde_json::json!({ "urls": urls }), None => serde_json::json!({ "urls": null }) }))
        }
        // With `steps`: the job's steps too (GitHub's as it has them now, with their times; a run from a computer's with
        // each one's log, or its lines so far while it runs). GitLab's are in its log, marked as sections.
        let steps = if q("steps").is_none() { serde_json::Value::Null } else if j.provider == "local" {
            serde_json::Value::Array(self.local_steps(j.local.as_deref().unwrap_or_default()).await?)
        } else if j.provider.is_empty() {
            match (self.app_of(&j), self.installation_token_for(&j).await) { (Some(app), Some(token)) => github::job_steps(self.http, &app.api(), &token, &j.repo, j.job_id).await.map(serde_json::Value::Array).unwrap_or_default(), _ => serde_json::Value::Null }
        } else { serde_json::Value::Null };
        // A run from a computer: each step's whole log as its runner sent it here, else the lines it sent as it went.
        let fetched = if j.provider == "local" {
            let token = j.local.clone().unwrap_or_default();
            self.local_log(&token).await
        } else if j.provider == "gitlab" {
            match (self.gitlab_of(&j), j.project_id) { (Some(gl), Some(project)) => gitlab::job_trace(self.http, gl, project, j.job_id).await, _ => Err("its GitLab is no longer connected".into()) }
        } else {
            let from_github = match (self.app_of(&j), self.installation_token_for(&j).await) { (Some(app), Some(token)) => github::job_log(self.http, &app.api(), &token, &j.repo, j.job_id).await, _ => Err("GitHub gave no token for its repository (is the App still installed there?)".into()) };
            // Not yet from GitHub (it gives a job's log once it has ended): what its runner wrote as it went.
            match from_github {
                Ok(l) if !l.is_empty() => Ok(l),
                other => match self.live_log(key.trim_start_matches("job:"), 1_000_000).await { Ok(l) if !l.is_empty() => Ok(l), _ => other },
            }
        };
        let log = match fetched { Ok(l) => l, Err(e) => return Ok(Response::json(&serde_json::json!({ "error": e, "steps": steps }))) };
        // (Four MB at most: what the runtimes answer with, written as JSON, stays within their six.)
        let want = q("bytes").and_then(|b| b.parse::<usize>().ok()).unwrap_or(200_000).clamp(1_000, 4_000_000);
        // From a line's start, when the cut falls inside one.
        let from = log.len().saturating_sub(want);
        let from = if from == 0 { 0 } else { log[from..].iter().position(|b| *b == b'\n').map(|n| from + n + 1).unwrap_or(from) };
        Ok(Response::json(&serde_json::json!({ "log": String::from_utf8_lossy(&log[from..]), "bytes": log.len(), "truncated": from > 0, "steps": steps })))
    }

    /// What the dashboard shows, for a key a dashboard session set and that has not expired (or one that only reads):
    /// the App and where it is installed, the AWS connection, and recent jobs. Without a key it is like any unknown path.
    async fn status(&self, req: &Request) -> Result<Response> {
        if !self.reader(req) { return Ok(nothing_here()); }
        // `light`: asked only to see a setting take (after each change): without what GitHub has to be asked for.
        let light = url::Url::parse(&req.url).is_ok_and(|u| u.query_pairs().any(|(k, v)| k == "light" && v == "1"));
        let mut installations = vec![];
        for app in self.config.apps().filter(|_| !light) {
            installations.extend(github::installations(self.http, app, self.clock.now_ms()).await.unwrap_or_default()
                .into_iter().map(|i| serde_json::json!({ "account": i.account, "repositories": i.selection, "id": i.id, "permissions": i.permissions, "app": app.id })));
        }
        // Each App (one per organization), with what it asks GitHub for.
        let mut apps = vec![];
        for app in self.config.apps() {
            let permissions = self.app_permissions(app).await;
            apps.push(serde_json::json!({ "id": app.id, "slug": self.app_slug(app), "owner": app.owner, "org": app.owner_is_org, "host": app.host, "permissions": permissions }));
        }
        let mut gitlabs = vec![];
        for gl in self.config.gitlabs() { gitlabs.push(serde_json::json!({ "id": gl.id, "url": gl.url, "scopes": self.gitlab_scopes(gl).await })) }
        // The newest fifty jobs; the ones still going; what this month and today cost.
        let jobs = self.newest_jobs(50).await?;
        let live: Vec<Job> = self.live_jobs().await?.into_iter().map(|(_, j)| j).collect();
        let (month_spend, day_spend) = self.spend().await?;
        // What jobs used over a day, a week and 30 days, beside what the code host's runners would have charged.
        let now = self.clock.now_ms();
        let (hours, _) = self.hours(31).await?;
        let usage: serde_json::Map<String, serde_json::Value> = USAGE_PERIODS.iter().map(|(name, step, steps)| (name.to_string(), usage_of(&hours, now, *step, *steps))).collect();
        // Each with what its machine said of its own start, where one did (kept apart: see `MachineStart`).
        let hello_keys: Vec<String> = jobs.iter().map(|j| format!("hello:{}", job_key(j).trim_start_matches("job:"))).collect();
        let hellos: Vec<Option<serde_json::Value>> = self.store.get_many(&hello_keys).await.unwrap_or_default().into_iter().map(|v| v.and_then(|v| serde_json::from_str(&v).ok())).collect();
        let with_machines: Vec<serde_json::Value> = jobs.iter().enumerate().map(|(i, j)| {
            let mut v = serde_json::to_value(j).unwrap_or_default();
            if let Some(Some(m)) = hellos.get(i) { v["machine"] = m.clone() }
            // (Its link is the machine's alone.)
            if let Some(o) = v.as_object_mut() { o.remove("hello"); }
            v
        }).collect();
        Ok(Response::json(&serde_json::json!({
            "plane": self.config.plane_id, "label": self.config.label, "labels": self.config.labels(), "name": self.config.name(),
            "app": self.config.app.as_ref().map(|a| serde_json::json!({ "id": a.id, "slug": self.app_slug(a), "owner": a.owner, "org": a.owner_is_org, "host": a.host })),
            "apps": apps,
            "installations": installations,
            "aws": self.aws_connection().await?,
            "aws_regions": self.aws_connection().await?.map(|a| self.aws_regions(&a)).unwrap_or_default(),
            "aws_networks": self.config.aws_networks,
            "cloudflare_location": self.config.cloudflare_location,
            "cloudflare_image": self.config.cloudflare_image,
            "agents": self.config.agents,
            "containers": self.own_containers(),
            "own_cloud": self.config.own_cloud,
            "gitlab": self.config.gitlab.as_ref().map(|g| serde_json::json!({ "url": g.url })),
            // Every GitLab connection (the first, then each further one, by its name), with its token's scopes.
            "gitlabs": gitlabs,
            "routing": self.config.routing,
            "read_keys": if self.dashboard(req) { serde_json::json!(self.config.read_keys.iter().map(|(until, name, kept)| {
                let mut k = serde_json::json!({ "name": name, "until_ms": until });
                if kept.split_once(':').and_then(|(_, more)| RunKey::of(more)).is_some() { k["runs"] = true.into() }
                k
            }).collect::<Vec<_>>()) } else { serde_json::Value::Null },
            "machine": self.config.machine,
            "spend": month_spend,
            // (It keeps the CI's and a cloud account's limit for the month: one from before would drop them unsaid.)
            "account_limits": true,
            // (And for a day; and what the AWS account's own budget does.)
            "daily_limits": true,
            "spend_today": day_spend,
            "aws_budget_stopped_ms": self.store.get(BUDGET_STOPPED_KEY).await.ok().flatten().and_then(|at| at.parse::<u64>().ok()),
            // Machines up now in each cloud, over every job (`jobs` is the newest fifty).
            "active": live.iter().filter(|j| active(j) && !j.cloud.is_empty()).fold(HashMap::<String, u32>::new(), |mut m, j| { *m.entry(place_of(j).to_string()).or_default() += 1; m }),
            // What the clouds and code hosts allow it (see permissions.rs): what the App asks for (each installation's
            // accepted permissions are in `installations`), the GitLab token's scopes, and permissions refused lately.
            "permissions": {
                "github_app": match &self.config.app { Some(app) => self.app_permissions(app).await, None => None },
                "gitlab_scopes": match &self.config.gitlab { Some(gl) => self.gitlab_scopes(gl).await, None => None },
                "denied": self.store.list("denied:").await?.into_iter().filter_map(|(k, v)| Some((k.strip_prefix("denied:")?.to_string(), serde_json::from_str::<serde_json::Value>(&v).ok()?))).collect::<serde_json::Map<_, _>>(),
            },
            "jobs": with_machines,
            "usage": usage,
            // How its store stands: whether the jobs' indexes are built, and nothing is left in an earlier layout.
            "store": { "indexed": self.indexed().await, "moved": self.store.migrate(0).await.unwrap_or(false) },
        })))
    }

    /// What the code host's own runner of a job's size charges a minute: GitLab.com's for a GitLab job, else GitHub's
    /// (Linux x64 or arm64, Windows; with a GPU, GitHub's 4-core GPU runner).
    fn host_price(&self, j: &Job) -> f64 {
        let spec = self.config.parse(&j.label).and_then(|s| s.ok()).map(|s| s.or(&self.config.machine)).unwrap_or_else(|| self.config.machine.clone());
        let cpu = j.cpu.or(spec.cpu).unwrap_or(2);
        if j.provider == "gitlab" { return gitlab_usd_per_minute(cpu) }
        if spec.gpu.is_some() { return 0.052 }
        if spec.os() == "windows" { return github_windows_usd_per_minute(cpu) }
        github_usd_per_minute(cpu, spec.arch.as_deref() == Some("arm64"))
    }

    /// What the App asks GitHub for (asked again after ten minutes).
    async fn app_permissions(&self, app: &App) -> Option<serde_json::Value> {
        let now = self.clock.now_ms();
        if let Some((p, until)) = self.cache.app_permissions.borrow().get(&app.id).cloned() { if now < until { return Some(p) } }
        let said = github::app_now(self.http, app, now).await.ok()?;
        let p = said["permissions"].clone();
        self.cache.app_permissions.borrow_mut().insert(app.id, (p.clone(), now + 600_000));
        if let Some(slug) = said["slug"].as_str().filter(|s| !s.is_empty()) { self.cache.app_slugs.borrow_mut().insert(app.id, slug.to_string()); }
        Some(p)
    }

    /// The App's address on GitHub: as GitHub last said it (see `app_permissions`), else as it was when the App was
    /// made. An App renamed on GitHub has another address, and the links the pages make to it follow.
    fn app_slug(&self, app: &App) -> String { self.cache.app_slugs.borrow().get(&app.id).cloned().unwrap_or_else(|| app.slug.clone()) }

    /// The GitLab token's scopes (asked again after ten minutes).
    async fn gitlab_scopes(&self, gl: &GitLab) -> Option<Vec<String>> {
        let now = self.clock.now_ms();
        if let Some((s, until)) = self.cache.gitlab_scopes.borrow().get(&gl.id).cloned() { if now < until { return Some(s) } }
        let s = gitlab::token_scopes(self.http, gl).await.ok()?;
        self.cache.gitlab_scopes.borrow_mut().insert(gl.id.clone(), (s.clone(), now + 600_000));
        Some(s)
    }

    /// A GitLab connection by its name (nothing: the first), and the one a job came through.
    fn gitlab_by(&self, id: &str) -> Option<&GitLab> { self.config.gitlabs().find(|g| g.id == id) }
    fn gitlab_of(&self, j: &Job) -> Option<&GitLab> { self.gitlab_by(&j.gitlab) }

    /// The connection whose webhook secret a request carries (each has its own).
    fn gitlab_signed(&self, req: &Request) -> Option<&GitLab> {
        let given = req.header("x-gitlab-token")?;
        self.config.gitlabs().find(|g| g.hook_secret.len() >= 16 && safe_eq(given.as_bytes(), g.hook_secret.as_bytes()))
    }

    /// Notes whether a cloud refused one of its permissions (`permissions.rs`) for a call just made: refused, it is kept
    /// (when, and what the cloud said) for the dashboard; allowed again, it is cleared.
    async fn note_permission<T>(&self, cloud: &str, id: &str, result: &Result<T>) {
        let key = format!("denied:{cloud}:{id}");
        match result {
            // (A refusal by the account's organization is no permission this control plane's role lacks: giving the
            // role more would change nothing.)
            Err(e) if aws::refusal(e).is_some_and(|r| matches!(r.by, aws::By::Organization | aws::By::OrganizationResources)) => {}
            Err(e) if ["AccessDenied", "UnauthorizedOperation", "not authorized"].iter().any(|d| e.contains(d)) => {
                let _ = put_json(self.store, &key, &serde_json::json!({ "at_ms": self.clock.now_ms(), "error": e.chars().take(300).collect::<String>() })).await;
            }
            Ok(_) => { if self.store.get(&key).await.ok().flatten().is_some() { let _ = self.store.delete(&key).await; } }
            Err(_) => {}
        }
    }

    /// A job whose machine never began it: its runner registration is withdrawn and it is placed again (elsewhere, or
    /// waiting for room).
    async fn again(&self, plane_url: &str, key: &str, j: &mut Job, why: &str) -> Result<()> {
        self.withdraw_runner(j).await;
        if let (Some(id), Some(runner)) = (&j.machine_id, &j.runner) { let _ = self.store.delete(&format!("assign:{id}:{runner}")).await; }
        j.runner = None;
        j.runner_id = None;
        j.machine_id = None;
        j.machine_type = None;
        // What its machine said (a spot machine taken back before it began anything) goes with the machine.
        (j.notice, j.interrupted) = (None, false);
        j.cloud = String::new();
        j.state = "waiting".into();
        j.error = Some(format!("waiting: {why}"));
        self.keep(key, &*j).await?;
        self.start(plane_url, j).await
    }

    /// The GitHub App a job's event came through (jobs from before there could be several: the first), and the
    /// account its runners register with.
    fn app_of(&self, j: &Job) -> Option<&App> {
        match j.app_id { Some(id) => self.config.apps().find(|a| a.id == id), None => self.config.app.as_ref() }
    }

    /// Which runners can take a GitHub job: those with its label, in its organization (in its repository, for a person's).
    fn runners_of(&self, j: &Job) -> String {
        format!("{}|{}|{}", j.installation_id, j.label, if self.app_of(j).is_some_and(|a| a.owner_is_org) { "" } else { j.repo.as_str() })
    }

    fn owner_of(app: &App) -> Owner { Owner { org: app.owner_is_org, login: app.owner.clone() } }

    /// An installation token for a job's repository (None without the App, or if GitHub would not give one).
    async fn installation_token_for(&self, j: &Job) -> Option<String> {
        self.installation_token(self.app_of(j)?, j.installation_id).await.ok()
    }

    /// Whether the App is installed anywhere (asked with its own key, at most once a minute).
    async fn installed(&self) -> bool {
        if self.config.app.is_none() { return false }
        let now = self.clock.now_ms();
        if let Some((yes, at)) = *self.cache.installed.borrow() { if now.saturating_sub(at) < 60_000 { return yes; } }
        let mut yes = false;
        for app in self.config.apps() { yes |= github::installations(self.http, app, now).await.map(|i| !i.is_empty()).unwrap_or(false) }
        self.cache.installed.replace(Some((yes, now)));
        yes
    }

    /// Connected when the control plane can assume the role and see the machine image.
    async fn verify_aws(&self, plane_url: &str, mut a: AwsConnection) -> Result<AwsConnection> {
        self.cache.creds.replace(None);
        let check = async {
            let creds = self.aws_creds(plane_url, &a).await?;
            aws::latest_image(self.http, &a.region, &creds, &self.config.image_owner, &self.config.image_name, self.clock.now_ms()).await
        }.await;
        a.connected = check.is_ok();
        a.error = check.err().map(|e| e.chars().take(300).collect());
        put_json(self.store, "aws", &a).await?;
        Ok(a)
    }

    /// The dashboard's report of the AWS role it made: with the connect token it set, for this control plane's own role,
    /// in the region it chose; each token is accepted once.
    async fn aws_callback(&self, req: &Request, plane_url: &str) -> Result<Response> {
        let (Some(token), Some(region)) = (&self.config.aws_connect_token, &self.config.aws_region) else { return Ok(Response::text(409, "no AWS connection in progress")) };
        let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap_or_default();
        let s = |k: &str| body[k].as_str().unwrap_or_default().to_string();
        if token.len() < 16 || !safe_eq(s("token").as_bytes(), token.as_bytes()) { return Ok(Response::text(403, "not this control plane's stack")); }
        let used = sha256_hex(token.as_bytes());
        if get_json::<String>(self.store, "aws_token_used").await?.as_deref() == Some(used.as_str()) { return Ok(Response::text(409, "this connect token was used already")); }
        let account_id = s("accountId");
        if account_id.len() != 12 || !account_id.bytes().all(|b| b.is_ascii_digit()) || s("roleArn") != role_arn(&account_id, &self.config.plane_id) || s("region") != *region {
            return Ok(Response::text(400, "unexpected account, role or region"));
        }
        let a = self.verify_aws(plane_url, AwsConnection { role_arn: role_arn(&account_id, &self.config.plane_id), account_id, region: region.clone(), connected: false, error: None }).await?;
        if a.connected { put_json(self.store, "aws_token_used", &used).await?; }
        Ok(Response::json(&serde_json::json!({ "connected": a.connected, "error": a.error })))
    }

    /// Credentials for the connected role: a token this control plane signs as its own identity provider, exchanged with STS.
    async fn aws_creds(&self, plane_url: &str, a: &AwsConnection) -> Result<Credentials> {
        if a.role_arn.is_empty() { return self.config.aws_own_creds.clone().ok_or_else(|| "no AWS credentials in this runtime".to_string()); }
        let now = self.clock.now_ms();
        if let Some(c) = self.cache.creds.borrow().as_ref() { if c.expires_at_ms > now + 300_000 { return Ok(c.clone()); } }
        let signing = self.identity().await?.signing;
        let secs = now / 1000;
        let token = signing.jwt(&serde_json::json!({ "iss": plane_url, "sub": format!("plane:{}", self.config.plane_id), "aud": AUDIENCE, "iat": secs, "exp": secs + 300 }))?;
        let creds = aws::assume_role_with_web_identity(self.http, &a.region, &a.role_arn, &token, &runner_prefix(self.config.name(), &self.config.plane_id), now).await?;
        self.cache.creds.replace(Some(creds.clone()));
        Ok(creds)
    }

    async fn installation_token(&self, app: &App, installation_id: u64) -> Result<String> {
        let now = self.clock.now_ms();
        if let Some((t, until)) = self.cache.tokens.borrow().get(&installation_id) { if *until > now { return Ok(t.clone()); } }
        let t = github::installation_token(self.http, app, installation_id, now).await?;
        self.cache.tokens.borrow_mut().insert(installation_id, (t.clone(), now + 45 * 60_000));
        Ok(t)
    }

    async fn webhook(&self, req: &Request, plane_url: &str) -> Result<Response> {
        if self.config.app.is_none() { return Ok(Response::text(401, "no app")) }
        // Signed by one of this control plane's Apps (each organization has its own): the event is that one's.
        let Some(app) = self.config.apps().find(|a| verify_hub_signature(&a.webhook_secret, &req.body, req.header("x-hub-signature-256"))) else { return Ok(Response::text(401, "bad signature")) };
        if req.header("x-github-event") != Some("workflow_job") { return Ok(Response::text(202, "ignored")); }
        let payload: serde_json::Value = serde_json::from_slice(&req.body).map_err(|e| e.to_string())?;
        // The sweep keeps running after the first webhook (it asks for failed deliveries again).
        self.timer.wake_in(300_000).await?;
        let Some(ev) = github::our_job_among(&payload, &self.config.labels(), &app.owner) else { return Ok(Response::text(202, "not ours")) };
        match ev.action.as_str() {
            "queued" => self.launch(plane_url, &ev, app).await?,
            "in_progress" => self.started(plane_url, ev.job_id, ev.runner_name.as_deref()).await?,
            "completed" => self.completed(plane_url, &ev).await?,
            _ => {}
        }
        Ok(Response::text(202, "ok"))
    }

    /// A GitLab project's job event: a pending job with this control plane's label in its tags gets a machine; later
    /// events say when it began and ended (the machine ends by itself after one job).
    async fn gitlab_webhook(&self, req: &Request, plane_url: &str) -> Result<Response> {
        if self.config.gitlabs().next().is_none() { return Ok(nothing_here()) }
        // Which connection it is from: the one whose secret it carries.
        let Some(gl) = self.gitlab_signed(req) else { return Ok(Response::text(401, "bad token")) };
        put_json(self.store, &last_event_key(&gl.id), &self.clock.now_ms()).await?;
        let Some(ev) = gitlab::job_event(&req.body) else { return Ok(Response::text(200, "ignored")) };
        self.gitlab_job_event(gl, ev, plane_url).await
    }

    /// A GitLab job's event: as GitLab sent it, or a waiting job GitLab never told of, found by asking (`untold_gitlab`).
    async fn gitlab_job_event(&self, gl: &GitLab, ev: gitlab::JobEvent, plane_url: &str) -> Result<Response> {
        self.timer.wake_in(300_000).await?;
        let key = format!("job:gl{}", gitlab::local(&gl.id, ev.job_id));
        // A project's path, told apart across connections (for the note that a job is run again).
        let project = if gl.id.is_empty() { ev.project.clone() } else { format!("{}:{}", gl.id, ev.project) };
        match ev.status.as_str() {
            "pending" => {
                if self.store.get(&key).await?.is_some() { return Ok(Response::text(200, "known")) }
                let (tags, status) = gitlab::job(self.http, gl, ev.project_id, ev.job_id).await?;
                if status != "pending" { return Ok(Response::text(200, "not pending")) }
                let ours: Vec<&String> = tags.iter().filter(|t| self.config.parse(t).is_some()).collect();
                let [label] = ours.as_slice() else { return Ok(Response::text(200, "not ours")) };
                let mut job = Job { job_id: ev.job_id, run_id: ev.pipeline_id, repo: ev.project.clone(), state: "launching".into(), at_ms: self.clock.now_ms(), label: label.to_string(),
                    provider: "gitlab".into(), gitlab: gl.id.clone(), project_id: Some(ev.project_id), tags: tags.clone(), name: ev.name.clone(), workflow: ev.stage.clone(), ..Default::default() };
                // Claimed once, however many deliveries arrive.
                if !self.store.put_if_absent(&key, serde_json::to_string(&job).map_err(|e| e.to_string())?).await? { return Ok(Response::text(200, "known")) }
                self.indexes(&key, &job, true, None).await?;
                // Run again after AWS took its spot machine back: not on a spot machine this time.
                let again = rerun_key(&project, ev.pipeline_id, &ev.name);
                if self.store.get(&again).await?.is_some() {
                    job.no_spot = true;
                    job.after_reclaim = true;
                    self.store.delete(&again).await?;
                }
                self.start_soon(plane_url, &mut job).await?;
            }
            "running" | "success" | "failed" | "canceled" | "skipped" => {
                // The machine it runs on is its runner's, and a runner takes any job with its tags: the record that
                // follows the machine is the one of the job the runner was made for (as with GitHub's runners).
                let made_for = match ev.runner_id { Some(r) => get_json::<serde_json::Value>(self.store, &format!("glrunner:{}", gitlab::local(&gl.id, r))).await?.map(|v| (r, v["job"].as_u64())), None => None };
                let machine_key = made_for.and_then(|(_, job)| job).map(|id| format!("job:gl{}", gitlab::local(&gl.id, id))).unwrap_or(key.clone());
                let now = self.clock.now_ms();
                if ev.status == "running" {
                    // The runner that took it takes no other (a job that read its token gets nothing more with it).
                    let Some((r, _)) = made_for else { return Ok(Response::text(200, "not on a runner of ours")) };
                    let _ = gitlab::pause_runner(self.http, gl, r).await;
                    // What the job on the runner's machine runs for, for the cache there (see `machine_access`).
                    let at = format!("glrunner:{}", gitlab::local(&gl.id, r));
                    if let Some(mut v) = get_json::<serde_json::Value>(self.store, &at).await? {
                        v["running"] = serde_json::json!({ "job": ev.job_id, "project_id": ev.project_id, "project": ev.project, "ref": ev.git_ref, "tag": ev.tag, "default_branch": ev.default_branch });
                        put_json(self.store, &at, &v).await?;
                    }
                    if let Some(mut j) = get_json::<Job>(self.store, &machine_key).await? {
                        // Another job than the runner's own: this job's machine, still coming, is for the runner's
                        // job now (or for whichever job the runner's machine was for).
                        if machine_key != key {
                            if let Some(mut own) = get_json::<Job>(self.store, &key).await?.filter(|o| active(o) && !o.seen_in_progress) {
                                own.for_job = Some(j.for_job.unwrap_or(j.job_id));
                                self.keep(&key, &own).await?;
                            }
                        }
                        j.for_job = None;
                        j.state = "running".into();
                        j.seen_in_progress = true;
                        j.started_ms.get_or_insert(now);
                        self.keep(&machine_key, &j).await?;
                    }
                    return Ok(Response::text(200, "ok"));
                }
                if let Some((r, _)) = made_for {
                    // Its machine ends by itself after one job; its runner is removed.
                    if gitlab::delete_runner(self.http, gl, r).await.is_ok() { self.store.delete(&format!("glrunner:{}", gitlab::local(&gl.id, r))).await?; }
                    if let Some(mut j) = get_json::<Job>(self.store, &machine_key).await? {
                        // Its machine ran it to its end: how the job itself ended is its own (as a GitHub job's). One whose
                        // spot machine AWS took back failed for that, not for itself.
                        j.state = if ev.status == "canceled" { "cancelled" } else if j.interrupted && ev.status == "failed" { "failed" } else { "done" }.into();
                        j.conclusion = Some(gitlab_conclusion(&ev.status).into());
                        j.ended_ms.get_or_insert(now);
                        // AWS took its spot machine back: the job it was running is run again (GitLab makes a new
                        // job of it at once), at the next place in the order below AWS's spot machines.
                        if j.interrupted && ev.status == "failed" && j.rerun.is_none() {
                            put_json(self.store, &rerun_key(&project, ev.pipeline_id, &ev.name), &now).await?;
                            let asked = gitlab::retry_job(self.http, gl, ev.project_id, ev.job_id).await;
                            if asked.is_err() { self.store.delete(&rerun_key(&project, ev.pipeline_id, &ev.name)).await?; }
                            j.error = Some(match &asked { Ok(()) => "AWS took back its spot machine; run again, off spot machines".to_string(), Err(e) => format!("AWS took back its spot machine; not run again: {e}").chars().take(300).collect() });
                            j.rerun = Some(Rerun { job_id: ev.job_id, run_id: ev.pipeline_id, repo: ev.project.clone(), name: ev.name.clone(), state: if asked.is_ok() { "asked".into() } else { "not run again".into() }, at_ms: now });
                        }
                        self.settle(plane_url, &mut j).await;
                        self.keep(&machine_key, &j).await?;
                    }
                }
                // Its own record, when it ran elsewhere or nowhere: cancelled while it waited for room, or its machine
                // left idle (ended by the sweep, unless it takes another job first).
                if machine_key != key || made_for.is_none() {
                    // Cancelled before any runner took it and before it was known here (its events arrive side by
                    // side, and this one can be first): noted, so that its start, a moment behind, starts nothing.
                    if ev.runner_id.is_none() && ["canceled", "skipped"].contains(&ev.status.as_str()) && self.store.get(&key).await?.is_none() {
                        let _ = self.store.put(&format!("gone:{}", key.trim_start_matches("job:")), now.to_string()).await;
                    }
                    if let Some(mut own) = get_json::<Job>(self.store, &key).await? {
                        if own.state == "waiting" {
                            own.state = "cancelled".into();
                            own.ended_ms = Some(now);
                            self.keep(&key, &own).await?;
                        } else if own.state == "launching" && made_for.is_none() {
                            // Over while its machine is still being asked for: cancelled now. The start under way
                            // finds its record ended, asks for nothing more, and ends what it was given (see `keep`).
                            own.state = "cancelled".into();
                            own.began = true;
                            own.ended_ms = Some(now);
                            self.keep(&key, &own).await?;
                        } else if own.state == "launched" && !own.seen_in_progress {
                            match made_for.and_then(|(_, job)| job) {
                                Some(runners) => { own.over = true; own.for_job.get_or_insert(runners); }
                                None if own.over => {}
                                None => own.state = "orphan".into(),
                            }
                            self.keep(&key, &own).await?;
                            self.timer.wake_in(60_000).await?;
                        }
                    }
                }
            }
            _ => {}
        }
        Ok(Response::text(200, "ok"))
    }

    /// A GitLab runner of the job's own (GitLab has none for exactly one job): its token goes to the machine only; the
    /// runner is kept by id until it is removed, after its job or once its machine's time is up.
    async fn gitlab_runner(&self, gl: &GitLab, job: &mut Job) -> Result<String> {
        let mut tags = job.tags.clone();
        tags.sort();
        let (id, token) = gitlab::create_runner(self.http, gl, job.project_id.ok_or("a GitLab job without its project")?, &tags, self.config.name(), &self.config.plane_id, job.job_id).await?;
        put_json(self.store, &format!("glrunner:{}", gitlab::local(&gl.id, id)), &serde_json::json!({ "id": id, "job": job.job_id, "gitlab": gl.id, "at_ms": self.clock.now_ms() })).await?;
        job.runner_id = Some(id);
        Ok(token)
    }

    /// Withdraws a job's runner registration, on GitHub or GitLab.
    async fn withdraw_runner(&self, j: &Job) {
        let Some(rid) = j.runner_id else { return };
        if j.provider == "gitlab" {
            if let Some(gl) = self.gitlab_of(j) { if gitlab::delete_runner(self.http, gl, rid).await.is_ok() { let _ = self.store.delete(&format!("glrunner:{}", gitlab::local(&gl.id, rid))).await; } }
        } else if let Some(app) = self.app_of(j) {
            if let Ok(t) = self.installation_token(app, j.installation_id).await { let _ = github::delete_runner(self.http, &app.api(), &t, &Self::owner_of(app), &j.repo, rid).await; }
            if let Some(name) = &j.runner { let _ = self.store.delete(&format!("runner:{name}")).await; }
        }
    }

    /// GitLab runners past their machine's time (and ones from before each job had its own, kept per tag set): removed.
    async fn sweep_gitlab_runners(&self, now: u64) -> Result<()> {
        if self.config.gitlabs().next().is_none() { return Ok(()) }
        for (key, v) in self.store.list("glrunner:").await? {
            let Ok(v) = serde_json::from_str::<serde_json::Value>(&v) else { continue };
            if v["at_ms"].as_u64().is_some_and(|at| now.saturating_sub(at) < (max_minutes("aws") as u64 + 10) * 60_000) { continue }
            // Removed at the connection it was made through; one whose connection is gone is only forgotten here.
            if let (Some(id), Some(gl)) = (v["id"].as_u64(), self.gitlab_by(v["gitlab"].as_str().unwrap_or_default())) { if gitlab::delete_runner(self.http, gl, id).await.is_err() { continue } }
            self.store.delete(&key).await?;
        }
        Ok(())
    }

    /// The AWS account machines run in: this control plane's own when it runs in AWS, else the one connected by role.
    async fn aws_connection(&self) -> Result<Option<AwsConnection>> {
        if let Some((account_id, region)) = &self.config.aws_own {
            if self.config.aws_runners_off { return Ok(None) }
            return Ok(Some(AwsConnection { account_id: account_id.clone(), region: region.clone(), role_arn: String::new(), connected: true, error: None }));
        }
        get_json::<AwsConnection>(self.store, "aws").await
    }

    /// The regions machines go to in AWS, in order (the dashboard's list, else the connection's region).
    fn aws_regions(&self, a: &AwsConnection) -> Vec<String> {
        if self.config.aws_regions.is_empty() { vec![a.region.clone()] } else { self.config.aws_regions.clone() }
    }

    /// The current machine image in a region for a machine (Linux x64 or arm64, Linux with NVIDIA's drivers for GPUs,
    /// Windows), with its disk's size (looked up at most every six hours).
    async fn image(&self, region: &str, spec: &Spec, creds: &Credentials, now: u64) -> Result<(String, u32)> {
        let base = &self.config.image_name;
        let pattern = if spec.os() == "windows" { base.replace("-ubuntu24-full-", "-windows25-full-") }
            else if spec.gpu.is_some() { base.replace("-ubuntu24-full-", "-ubuntu24-gpu-") }
            else if spec.arch() == "arm64" { base.replace("-x64-", "-arm64-") } else { base.clone() };
        let key = format!("{region}/{pattern}");
        if let Some((id, _)) = self.cache.image.borrow().get(&key).filter(|(_, until)| *until > now) { if let Some((id, gb)) = id.split_once(' ') { return Ok((id.to_string(), gb.parse().unwrap_or(0))) } }
        let (id, gb) = aws::latest_image_sized(self.http, region, creds, &self.config.image_owner, &pattern, now).await?;
        self.cache.image.borrow_mut().insert(key, (format!("{id} {gb}"), now + 6 * 3_600_000));
        Ok((id, gb))
    }

    /// The types among these that a region has, in their order (asked at most every six hours). One a region does
    /// not have is then not asked for first with every job: AWS refuses it each time, and a spot machine's zones are
    /// chosen by the prices of the first type. Not known (AWS did not say): all of them, as given.
    async fn offered(&self, region: &str, types: &[String], creds: &Credentials, now: u64) -> Vec<String> {
        let key = format!("{region}/{}", types.join(","));
        if let Some((have, _)) = self.cache.types.borrow().get(&key).filter(|(_, until)| *until > now) { return have.clone() }
        let have = match aws::offered_types(self.http, region, creds, types, now).await { Ok(have) if !have.is_empty() => have, _ => types.to_vec() };
        self.cache.types.borrow_mut().insert(key, (have.clone(), now + 6 * 3_600_000));
        have
    }

    /// The spot pools of a region (a type in a zone) noted in the last quarter of an hour as having no machine, or as
    /// having taken one back (see `pool_key`). Older notes are dropped.
    /// Pools not to ask at all now: a spot pool that had no machine, or took one back, within the last minute (the
    /// answer would be the same: said to this job a round ago, or to another queued with it), and a zone AWS does
    /// not offer a type in (noted for a day).
    async fn pools_to_skip(&self, region: &str, spot: bool, now: u64) -> Vec<(String, String)> {
        let mut pools = vec![];
        let pool = |key: &str| { let mut parts = key.rsplitn(3, ':'); match (parts.next(), parts.next()) { (Some(zone), Some(kind)) => Some((kind.to_string(), zone.to_string())), _ => None } };
        if spot { for (key, at) in self.store.list(&format!("aws:pool:{region}:")).await.unwrap_or_default() { if at.parse::<u64>().is_ok_and(|at| now < at + 60_000) { pools.extend(pool(&key)) } } }
        for (key, at) in self.store.list(&format!("aws:unoffered:{region}:")).await.unwrap_or_default() {
            if at.parse::<u64>().is_ok_and(|at| now < at + 86_400_000) { pools.extend(pool(&key)) } else { let _ = self.store.delete(&key).await; }
        }
        pools
    }

    async fn pools_to_avoid(&self, region: &str, now: u64) -> Vec<(String, String)> {
        let mut pools = vec![];
        for (key, at) in self.store.list(&format!("aws:pool:{region}:")).await.unwrap_or_default() {
            if at.parse::<u64>().is_ok_and(|at| now < at + 900_000) {
                let mut parts = key.rsplitn(3, ':');
                if let (Some(zone), Some(kind)) = (parts.next(), parts.next()) { pools.push((kind.to_string(), zone.to_string())) }
            } else { let _ = self.store.delete(&key).await; }
        }
        pools
    }

    /// Whether spot is paused now: AWS took back two machines within a quarter of an hour, less than half an hour ago.
    async fn spot_paused(&self, now: u64) -> bool {
        let mut times: Vec<u64> = self.store.list("spot:interrupted:").await.unwrap_or_default().into_iter().filter_map(|(_, v)| v.parse().ok()).collect();
        times.sort();
        let (count, within, pause) = SPOT_PAUSE;
        times.windows(count).any(|w| w[count - 1].saturating_sub(w[0]) <= within && now.saturating_sub(w[count - 1]) < pause)
    }

    /// A job whose spot machine AWS took back, its run finished: run again (GitHub makes a new attempt of that job and
    /// of those that need it), noted so the new attempt is not on a spot machine again. Whether it is settled.
    async fn run_again(&self, j: &mut Job, now: u64) -> bool {
        let Some(mut r) = j.rerun.clone().filter(|r| r.state == "pending") else { return true };
        let settled = async {
            if now.saturating_sub(r.at_ms) > 6 * 3_600_000 { return Some("not run again: its run was still going six hours later".to_string()) }
            // GitHub not answering now (a token, the run's status): asked again the next minute.
            let token = self.installation_token_for(j).await?;
            let api = self.app_of(j)?.api();
            match github::run_status(self.http, &api, &token, &r.repo, r.run_id).await {
                Ok(s) if s == "completed" => {}
                Err(e) if e.contains(" 404 ") => return Some("not run again: its run is gone".into()),
                _ => return None,
            }
            // The other jobs of this run that wait to be run again: GitHub takes one request per attempt, so they
            // are asked for together (the run's failed jobs). Each is asked for once, whoever gets here first.
            let others: Vec<Rerun> = self.live_jobs().await.unwrap_or_default().into_iter().filter_map(|(_, o)| o.rerun)
                .filter(|o| o.state == "pending" && o.repo == r.repo && o.run_id == r.run_id && o.job_id != r.job_id).collect();
            let claim = |id: u64| format!("rerun-asked:{id}");
            match self.store.put_if_absent(&claim(r.job_id), now.to_string()).await { Ok(true) => {} Ok(false) => return Some("asked".into()), Err(_) => return None }
            for o in &others { let _ = self.store.put_if_absent(&claim(o.job_id), now.to_string()).await; }
            for name in std::iter::once(&r.name).chain(others.iter().map(|o| &o.name)) { let _ = put_json(self.store, &rerun_key(&r.repo, r.run_id, name), &now).await; }
            let asked = if others.is_empty() { github::rerun_job(self.http, &api, &token, &r.repo, r.job_id).await } else { github::rerun_failed_jobs(self.http, &api, &token, &r.repo, r.run_id).await };
            let Err(e) = asked else { return Some("asked".into()) };
            for name in std::iter::once(&r.name).chain(others.iter().map(|o| &o.name)) { let _ = self.store.delete(&rerun_key(&r.repo, r.run_id, name)).await; }
            // Refused for good (the App may not, or GitHub will not run it again): said. Anything else: tried again.
            if e.contains("Resource not accessible") { return Some("not run again: GitHub's App may not (Actions: write; see Control plane → Permissions)".into()) }
            if [" 403 ", " 404 ", " 409 ", " 422 "].iter().any(|c| e.contains(c)) { return Some(format!("not run again: {e}")) }
            for id in std::iter::once(r.job_id).chain(others.iter().map(|o| o.job_id)) { let _ = self.store.delete(&claim(id)).await; }
            None
        }.await;
        let Some(state) = settled else { return false };
        j.error = Some(if state == "asked" { "AWS took back its spot machine; run again, off spot machines".into() } else { format!("AWS took back its spot machine; {state}") });
        r.state = state;
        j.rerun = Some(r);
        true
    }

    /// Whether other jobs' GPU machines are up in AWS now (then a quota refusal means it is full, not that it is 0).
    /// How many other jobs of this control plane have a machine in AWS now, and how many had one until two minutes
    /// ago (AWS counts a machine against the account's limits a little longer than it runs).
    async fn aws_machines_up(&self, job_id: u64) -> (usize, usize) {
        let now = self.clock.now_ms();
        let jobs: Vec<Job> = self.live_jobs().await.unwrap_or_default().into_iter().map(|(_, j)| j).collect();
        let theirs = |j: &&Job| j.job_id != job_id && j.cloud == "aws" && j.machine_id.is_some();
        (jobs.iter().filter(theirs).filter(|j| active(j)).count(), jobs.iter().filter(theirs).filter(|j| !active(j) && j.ended_ms.is_some_and(|e| now.saturating_sub(e) < 120_000)).count())
    }

    async fn gpu_machines_up(&self, job_id: u64) -> bool {
        let jobs: Vec<Job> = self.live_jobs().await.unwrap_or_default().into_iter().map(|(_, j)| j).collect();
        jobs.iter().any(|j| j.job_id != job_id && j.cloud == "aws" && active(j) && self.config.parse(&j.label).and_then(|s| s.ok()).is_some_and(|s| s.gpu.is_some()))
    }

    /// Its own network in an AWS region (made by the dashboard), looked up at most every ten minutes; without one
    /// (not made yet, or a role from before it could read networks), machines start in the region's default network.
    /// A region with a network of the account's own set in the dashboard uses that one (looked up the same way), or
    /// fails saying what is wrong with it: its machines never start anywhere else. AWS not answering is an error too
    /// (the job is tried again), not a reason to use the default network.
    async fn network(&self, region: &str, creds: &Credentials, now: u64) -> Result<Option<aws::Network>> {
        // Kept by region and by what is set for it (a setting changed in the dashboard is used at once).
        let key = match self.config.aws_networks.get(region) { Some(g) => format!("{region} {}", serde_json::to_string(g).unwrap_or_default()), None => region.to_string() };
        if let Some((n, _)) = self.cache.network.borrow().get(&key).filter(|(_, until)| *until > now) { return Ok(n.clone()) }
        let found = match self.config.aws_networks.get(region) {
            Some(given) => aws::given_network(self.http, region, creds, given, now).await.map(Some),
            None => aws::find_network(self.http, region, creds, &self.config.plane_id, now).await,
        };
        self.note_permission("aws", "ReadNetwork", &found).await;
        let n = match found {
            Ok(n) => n,
            Err(e) if !self.config.aws_networks.contains_key(region) && ["AccessDenied", "UnauthorizedOperation", "not authorized"].iter().any(|d| e.contains(d)) => None,
            Err(e) => return Err(e),
        };
        self.cache.network.borrow_mut().insert(key, (n.clone(), now + if n.is_some() { 600_000 } else { 120_000 }));
        Ok(n)
    }

    /// Whether this control plane starts Cloudflare containers itself.
    fn own_containers(&self) -> bool { self.config.containers && self.containers.is_some() }

    /// Where a job's machine may go, by the order and limits set in the dashboard (see `Routing`): the places that can
    /// run it now, first to last. The job goes to the first; to the next when that one cannot start its machine.
    /// `no_spot`: AWS's spot machines are passed over when another place can run it (see `Job::no_spot`, `spot_paused`).
    async fn place(&self, repo: &str, spec: &Spec, gitlab: bool, no_spot: bool, failed_at: &[String]) -> Result<Placement> {
        let aws = self.aws_connection().await?.filter(|a| a.connected);
        let routing = &self.config.routing;
        let mut connected = vec![];
        if aws.is_some() { connected.extend(["aws".to_string(), AWS_ON_DEMAND.to_string()]) }
        if self.own_containers() { connected.push(self.config.own_cloud.clone()) }
        for a in &self.config.agents { if !connected.contains(&a.cloud) { connected.push(a.cloud.clone()) } }
        // The order as set; places connected since then come after it (AWS's on-demand machines: right after its spot ones).
        let mut order: Vec<String> = routing.order.iter().map(|p| p.cloud.clone()).collect();
        for c in &connected {
            if order.contains(c) { continue }
            match order.iter().position(|o| o == "aws") { Some(at) if c == AWS_ON_DEMAND => order.insert(at + 1, c.clone()), _ => order.push(c.clone()) }
        }
        // A label or a rule naming AWS means both of its places, as the order has them.
        let named = |c: &str| -> Vec<String> { if c == "aws" { order.iter().filter(|o| *o == "aws" || *o == AWS_ON_DEMAND).cloned().collect() } else { vec![c.to_string()] } };
        let candidates: Vec<String> = if let Some(c) = &spec.cloud { let v = named(c); if v.is_empty() { vec![c.clone()] } else { v } }
            else if let Some(c) = routing.rule_for(repo) { let v = named(c); if v.is_empty() { vec![c.to_string()] } else { v } }
            else if !routing.order.is_empty() { order.clone() }
            else if let Some(d) = &routing.default { let v = named(d); if v.is_empty() { vec![d.clone()] } else { v } }
            else { connected };
        if candidates.is_empty() { return Ok(Placement::Fail(format!("no cloud connected for jobs: connect one in the {} dashboard", self.config.name()))) }
        let (spend, today) = self.spend().await?;
        let jobs: Vec<Job> = self.live_jobs().await?.into_iter().map(|(_, j)| j).collect();
        // The whole CI's limit for the month: at it nothing is started, wherever.
        if let Some(cap) = routing.monthly_usd {
            let spent: f64 = spend.values().sum();
            if spent >= cap { return Ok(Placement::Fail(format!("{} reached its limit of {} a month (${spent:.2} spent on machines): no job starts until next month, or until the limit is raised (Runners in the dashboard, or `superci limits update --monthly-usd=N`)", self.config.name(), dollars(cap)))) }
        }
        // And for the day.
        if let Some(cap) = routing.daily_usd {
            let spent: f64 = today.values().sum();
            if spent >= cap { return Ok(Placement::Fail(format!("{} reached its limit of {} a day (${spent:.2} spent on machines today): no job starts until tomorrow (UTC), or until the limit is raised (Runners in the dashboard, or `superci limits update --daily-usd=N`)", self.config.name(), dollars(cap)))) }
        }
        let (mut later, mut never, mut places) = (vec![], vec![], vec![]);
        for cloud in &candidates {
            let name = cloud_name(cloud);
            let runner = match cloud.as_str() {
                "aws" | AWS_ON_DEMAND => aws.clone().map(|a| (Capacity::aws(), Runner::Aws(a))),
                c if c == self.config.own_cloud && self.own_containers() => Some((capacity_of(c), Runner::Own)),
                c => self.config.agents.iter().find(|a| a.cloud == c).map(|a| (if c == "cloudflare" { Capacity::cloudflare() } else { Capacity::modal() }, Runner::Agent(a.clone()))),
            };
            // Limits from the order: turned off, jobs at once, spend this month.
            let pool = routing.pool(cloud);
            if pool.is_some_and(|p| p.off) { never.push(format!("{name} is turned off")); continue }
            // Its cloud account's limit for the month, its providers together.
            if let Some(cap) = routing.account_usd.get(account_of(cloud)) {
                if account_spend(&spend, account_of(cloud)) >= *cap {
                    let said = format!("the {} account reached its {} a month", cloud_name(account_of(cloud)).split(' ').next().unwrap_or_default(), dollars(*cap));
                    if !never.contains(&said) { never.push(said) }
                    continue
                }
            }
            if let Some(cap) = routing.account_daily_usd.get(account_of(cloud)) {
                if account_spend(&today, account_of(cloud)) >= *cap {
                    let said = format!("the {} account reached its {} a day", cloud_name(account_of(cloud)).split(' ').next().unwrap_or_default(), dollars(*cap));
                    if !never.contains(&said) { never.push(said) }
                    continue
                }
            }
            if let Some(cap) = pool.and_then(|p| p.monthly_usd) {
                if spend.get(cloud).copied().unwrap_or(0.0) >= cap { never.push(format!("{name} reached its {} a month", dollars(cap))); continue }
            }
            if let Some(max) = pool.and_then(|p| p.max_jobs).or(Some(if cloud == "cloudflare" { CLOUDFLARE_JOBS_AT_ONCE } else { JOBS_AT_ONCE })) {
                if jobs.iter().filter(|j| place_of(j) == cloud && active(j)).count() as u32 >= max { later.push(format!("{name} runs {max} at once")); continue }
            }
            // A label that names this provider, and it is not added here: said as it is.
            let Some((capacity, runner)) = runner else {
                if spec.cloud.is_some() { return Ok(Placement::Fail(format!("its label needs {} runners, which this control plane has not added", cloud_name(spec.cloud.as_deref().unwrap_or(cloud))))) }
                never.push(format!("{name} is not connected"));
                continue
            };
            if gitlab && cloud == "modal" { never.push("Modal runs no Docker (GitLab jobs run in Docker)".into()); continue }
            if gitlab && spec.os() == "windows" { never.push(format!("{name} runs no GitLab jobs on Windows")); continue }
            if let Some(why) = capacity.refuses(spec) { never.push(format!("{name} runs {why}")); continue }
            // Its label asks for an on-demand machine: not AWS's spot ones.
            if cloud == "aws" && spec.on_demand() { never.push("AWS spot machines are not asked for (spot=false)".into()); continue }
            places.push((cloud.clone(), runner));
        }
        // Spot is passed over only when something else can take the job: with nothing else, a spot machine it is.
        if no_spot && places.iter().any(|(c, _)| c != "aws") { places.retain(|(c, _)| c != "aws") }
        // Places that failed this job before come after those that have not (still in order among themselves).
        places.sort_by_key(|(c, _)| failed_at.contains(c));
        Ok(if !places.is_empty() { Placement::Run(places) }
            else if !later.is_empty() { Placement::Wait(format!("waiting: {}", later.join("; "))) }
            else { Placement::Fail(format!("nowhere to run {}: {}", spec.describe(), never.join("; "))) })
    }

    /// One call to a runner agent, with a token this control plane signs for it alone.
    async fn agent_call(&self, plane_url: &str, a: &Agent, path: &str, body: serde_json::Value) -> Result<serde_json::Value> {
        let secs = self.clock.now_ms() / 1000;
        let token = self.identity().await?.signing.jwt(&serde_json::json!({ "iss": plane_url, "sub": format!("plane:{}", self.config.plane_id), "aud": a.url, "iat": secs, "exp": secs + 120 }))?;
        let req = Request::new("POST", &format!("{}{path}", a.url)).with_header("authorization", &format!("Bearer {token}"))
            .with_header("content-type", "application/json").with_body(body.to_string());
        let r = self.http.send(req).await?;
        if r.status >= 300 {
            // What it said (FastAPI's `detail`), else its answer as it came.
            let said = serde_json::from_slice::<serde_json::Value>(&r.body).ok().and_then(|v| v["detail"].as_str().map(str::to_string)).unwrap_or_else(|| r.body_text());
            return Err(format!("{} runner agent: {} {}", a.cloud, r.status, said.chars().take(300).collect::<String>()));
        }
        serde_json::from_slice(&r.body).map_err(|e| format!("{} runner agent: {e}", a.cloud))
    }

    /// Ends the machine that ran (or would have run) a job, wherever it is.
    /// An AWS machine's cost once it ended, at the prices AWS billed: its zone's spot prices over its time (or its
    /// on-demand price), its disk and its public address. Left as an estimate if AWS cannot say.
    async fn settle(&self, plane_url: &str, j: &mut Job) {
        if j.cloud != "aws" || j.cost_usd.is_some() { return }
        let (Some(region), Some(kind), Some(end)) = (j.region.clone(), j.machine_type.clone(), j.ended_ms) else { return };
        let Ok(Some(a)) = self.aws_connection().await else { return };
        let Ok(creds) = self.aws_creds(plane_url, &a).await else { return };
        let (from, now) = (j.launched_ms.unwrap_or(j.at_ms), self.clock.now_ms());
        let hours = end.saturating_sub(from).max(60_000) as f64 / 3_600_000.0;
        let spec = self.config.parse(&j.label).and_then(|s| s.ok()).map(|s| s.or(&self.config.machine)).unwrap_or_default();
        let machine = match j.zone.as_deref() {
            Some(zone) if !(spec.on_demand() || j.on_demand) => aws::spot_cost(self.http, &region, zone, &creds, &kind, spec.os(), from, end, now).await,
            _ => { let p = aws::on_demand_price(self.http, &creds, &region, &kind, spec.os(), now).await; self.note_permission("aws", "Prices", &p).await; p.map(|p| p * hours) }
        };
        let disk = aws::disk_price(self.http, &creds, &region, now).await;
        self.note_permission("aws", "Prices", &disk).await;
        let gb_month = disk.unwrap_or_else(|_| aws::disk_gb_month(&region));
        if let Ok(machine) = machine {
            j.cost_usd = Some(machine + aws::extras_usd_per_hour(gb_month, j.disk_gb.unwrap_or(60), j.disk_mibs.unwrap_or(DISK_MIBS_INCLUDED), j.disk_iops.unwrap_or(DISK_IOPS_INCLUDED), !j.private) * hours);
            j.cost_from = Some("prices".into());
        }
    }

    /// Adds what an AWS machine sent to its cost, at AWS's price for data sent out (an upper bound: CloudWatch counts
    /// traffic within the region too). Whether it could.
    async fn settle_sent(&self, plane_url: &str, j: &mut Job) -> bool {
        let (Some(region), Some(id), Some(end)) = (j.region.clone(), j.machine_id.clone(), j.ended_ms) else { return false };
        let Ok(Some(a)) = self.aws_connection().await else { return false };
        let Ok(creds) = self.aws_creds(plane_url, &a).await else { return false };
        let sent = aws::bytes_sent(self.http, &region, &creds, &id, j.launched_ms.unwrap_or(j.at_ms), end, self.clock.now_ms()).await;
        self.note_permission("aws", "MachineTraffic", &sent).await;
        let bytes = match sent {
            Ok(b) => b,
            // A role from before it could read CloudWatch: not known (-1), not asked again.
            Err(e) if e.contains("AccessDenied") => { j.sent_gb = Some(-1.0); return true }
            Err(_) => return false,
        };
        let gb = bytes / 1e9;
        j.sent_gb = Some(gb);
        j.cost_usd = Some(j.cost_usd.unwrap_or(0.0) + gb * aws::DATA_OUT_USD_PER_GB);
        true
    }

    async fn stop_machine(&self, plane_url: &str, j: &Job) {
        let Some(id) = &j.machine_id else { return };
        if j.cloud == "aws" {
            if let Ok(Some(a)) = self.aws_connection().await {
                let region = j.region.clone().unwrap_or(a.region.clone());
                if let Ok(c) = self.aws_creds(plane_url, &a).await { let _ = aws::terminate_instance(self.http, &region, &c, id, self.clock.now_ms()).await; }
            }
        } else if j.cloud == self.config.own_cloud && self.own_containers() {
            if let Some(c) = self.containers { let _ = c.stop(id).await; }
        } else if let Some(agent) = self.config.agents.iter().find(|a| a.cloud == j.cloud) {
            let _ = self.agent_call(plane_url, agent, "/stop", serde_json::json!({ "id": id })).await;
        }
    }

    /// A queued job: claimed once, then started where the order says (or left waiting for room).
    async fn launch(&self, plane_url: &str, ev: &github::JobEvent, app: &App) -> Result<()> {
        let key = format!("job:{}", ev.job_id);
        let mut job = Job { job_id: ev.job_id, run_id: ev.run_id, repo: ev.repo.clone(), state: "launching".into(), at_ms: self.clock.now_ms(), installation_id: ev.installation_id, label: ev.label.clone(),
            name: ev.name.clone(), workflow: ev.workflow.clone(), app_id: Some(app.id), attempt: Some(ev.attempt).filter(|a| *a > 0), queued_ms: ev.created_ms,
            repo_id: ev.repo_id, branch: ev.branch.clone(), default_branch: ev.default_branch.clone(), ..Default::default() };
        // Claimed once, however many deliveries of the event arrive at once. A claim that never got past launching (the
        // runtime restarted mid-way) is taken over by a later delivery.
        if self.store.put_if_absent(&key, serde_json::to_string(&job).map_err(|e| e.to_string())?).await? { self.indexes(&key, &job, true, None).await? } else {
            let stuck = get_json::<Job>(self.store, &key).await?.is_some_and(|j| j.state == "launching" && job.at_ms.saturating_sub(j.at_ms) > 90_000);
            if !stuck { return Ok(()); }
            self.keep(&key, &job).await?;
        }
        // A job run again after AWS took its spot machine back: not on a spot machine this time.
        let again = rerun_key(&ev.repo, ev.run_id, &ev.name);
        if self.store.get(&again).await?.is_some() {
            job.no_spot = true;
            job.after_reclaim = true;
            self.store.delete(&again).await?;
            self.keep(&key, &job).await?;
        }
        // A public repository: anyone can open a pull request there. Its jobs run only if the dashboard allows the
        // repository, and never for a run from outside it (a fork's pull request, pull_request_target). In doubt, not.
        if ev.public {
            let refused = if !self.config.routing.allows_public(&ev.repo) {
                Some("a public repository: allow it in the dashboard (Settings) for its jobs to run on your clouds".to_string())
            } else {
                let outside = match self.installation_token(app, ev.installation_id).await {
                    Ok(token) => github::run_from_outside(self.http, &app.api(), &token, &ev.repo, ev.run_id).await,
                    Err(e) => Err(e),
                };
                match outside {
                    Ok(None) => None,
                    Ok(Some(what)) => Some(format!("{what} in a public repository: not run on your clouds")),
                    Err(e) => Some(format!("could not check where its run came from ({e}): not run on your clouds")),
                }
            };
            if let Some(why) = refused {
                job.state = "failed".into();
                job.error = Some(why);
                job.ended_ms = Some(job.at_ms);
                return self.keep(&key, &job).await;
            }
        }
        self.start_soon(plane_url, &mut job).await
    }

    /// Writes a job's record. Requests run side by side (an event's, a sweep's, a machine's), and one may hold a
    /// copy for seconds while it waits on a cloud: written back as it is, that copy would undo what another wrote
    /// meanwhile. So the kept record is read first. Changed since this copy was read (its count of writes is
    /// another), and ended: it stays as it ended, nothing is written, and a machine this copy got meanwhile, which
    /// the record knows nothing of, is ended with its runner (a job cancelled while its machine was asked for).
    /// Changed and not ended: what only ever becomes true is kept from it (a runner took the job, the job began,
    /// how it ended, AWS's notice), the rest is this copy's.
    /// Whether the jobs' indexes are complete: `live:` (the jobs the sweep still cares for), `recent:` (every job by
    /// when it was queued) and `day:` (each day's totals), built once from the jobs kept before them (`reindex`).
    /// Until then what reads them reads every job instead.
    async fn indexed(&self) -> bool {
        if self.cache.indexed.get() { return true }
        let done = self.store.get(INDEXED).await.ok().flatten().is_some();
        self.cache.indexed.set(done);
        done
    }

    /// The jobs the sweep still cares for (see `needs_care`), each with its key, as they are kept now.
    async fn live_jobs(&self) -> Result<Vec<(String, Job)>> {
        let now = self.clock.now_ms();
        if !self.indexed().await {
            return Ok(self.store.list("job:").await?.into_iter().filter_map(|(k, v)| Some((k, serde_json::from_str::<Job>(&v).ok()?))).filter(|(_, j)| needs_care(j, now)).collect())
        }
        // (The jobs themselves, read at once: an index says only which.)
        let keys: Vec<String> = self.store.list("live:").await?.into_iter().filter_map(|(k, _)| Some(format!("job:{}", k.strip_prefix("live:")?))).collect();
        Ok(keys.iter().cloned().zip(self.store.get_many(&keys).await?).filter_map(|(k, v)| Some((k, serde_json::from_str::<Job>(&v?).ok()?))).collect())
    }

    /// The newest `n` jobs, the newest first.
    async fn newest_jobs(&self, n: usize) -> Result<Vec<Job>> {
        if !self.indexed().await {
            let mut all: Vec<Job> = self.store.list("job:").await?.into_iter().filter_map(|(_, v)| serde_json::from_str(&v).ok()).collect();
            all.sort_by(|a, b| b.at_ms.cmp(&a.at_ms));
            all.truncate(n);
            return Ok(all)
        }
        let keys: Vec<String> = self.store.list_last("recent:", n).await?.into_iter().rev().filter_map(|(k, _)| Some(format!("job:{}", k.splitn(3, ':').nth(2)?))).collect();
        Ok(self.store.get_many(&keys).await?.into_iter().flatten().filter_map(|v| serde_json::from_str(&v).ok()).collect())
    }

    /// Each hour's totals over the last `days` days and today, from each day's (jobs still going counted as they
    /// stand now), by the hour since the epoch; and each day's spend by place.
    async fn hours(&self, days: u64) -> Result<(std::collections::BTreeMap<u64, Hour>, std::collections::BTreeMap<u64, std::collections::BTreeMap<String, f64>>)> {
        let now = self.clock.now_ms();
        let today = now / 86_400_000;
        let (mut hours, mut spend) = (std::collections::BTreeMap::new(), std::collections::BTreeMap::<u64, std::collections::BTreeMap<String, f64>>::new());
        let add = |j: &Job, hours: &mut std::collections::BTreeMap<u64, Hour>, spend: &mut std::collections::BTreeMap<u64, std::collections::BTreeMap<String, f64>>| {
            let (hour, h) = hour_of(j, self.host_price(j), now);
            *spend.entry(j.at_ms / 86_400_000).or_default().entry(place_of(j).to_string()).or_insert(0.0) += h.usd;
            hours.entry(hour).or_insert_with(Hour::default).add(&h);
        };
        if !self.indexed().await {
            let since = (today - days) * 86_400_000;
            for (_, v) in self.store.list("job:").await? { if let Ok(j) = serde_json::from_str::<Job>(&v) { if j.at_ms >= since { add(&j, &mut hours, &mut spend) } } }
            return Ok((hours, spend))
        }
        let keys: Vec<String> = (today - days..=today).map(day_key).collect();
        for (day, v) in (today - days..=today).zip(self.store.get_many(&keys).await?) {
            let Some(d) = v.and_then(|v| serde_json::from_str::<Day>(&v).ok()) else { continue };
            for (i, h) in d.hours.iter().enumerate() { if h.jobs > 0 || h.usd != 0.0 { hours.entry(day * 24 + i as u64).or_insert_with(Hour::default).add(h) } }
            let s = spend.entry(day).or_default();
            for (place, usd) in d.spend { *s.entry(place).or_insert(0.0) += usd }
        }
        for (_, j) in self.live_jobs().await? { if j.counted_usd.is_none() && j.at_ms >= (today - days) * 86_400_000 { add(&j, &mut hours, &mut spend) } }
        Ok((hours, spend))
    }

    /// What machines cost this month and today, by place (see `place_of`).
    async fn spend(&self) -> Result<(HashMap<String, f64>, HashMap<String, f64>)> {
        let now = self.clock.now_ms();
        let (month_day, today) = (month_start_ms(now) / 86_400_000, now / 86_400_000);
        let (_, spend) = self.hours(today - month_day).await?;
        let (mut month, mut day) = (HashMap::new(), HashMap::new());
        for (d, by) in spend {
            for (place, usd) in by {
                if d >= month_day { *month.entry(place.clone()).or_insert(0.0) += usd }
                if d == today { *day.entry(place).or_insert(0.0) += usd }
            }
        }
        Ok((month, day))
    }

    /// Whether AWS refused a machine by the rule its budget puts on this control plane's role: the refusal names
    /// that rule, or (AWS does not always name the policy) it is an explicit deny of the role's own policies while
    /// such a budget is on.
    fn stopped_by_budget(&self, e: &str) -> bool {
        e.contains(&aws::stop_policy_name(&self.config.plane_id))
            || (self.config.routing.aws_budget.as_ref().is_some_and(|b| b.stop) && e.contains("explicit deny") && aws::refusal(e).is_some_and(|r| r.by == aws::By::Identity))
    }

    /// Adds what a job's cost changed by (all of it when it is first counted) to its day's totals.
    async fn count(&self, j: &Job, h: &Hour) -> Result<()> {
        let (day, hour) = (j.at_ms / 86_400_000, (j.at_ms % 86_400_000 / 3_600_000) as usize);
        let key = day_key(day);
        for _ in 0..8 {
            let was = self.store.get(&key).await?;
            let mut d: Day = was.as_deref().and_then(|v| serde_json::from_str(v).ok()).unwrap_or_default();
            d.hours.resize(24, Hour::default());
            d.hours[hour].add(h);
            *d.spend.entry(place_of(j).to_string()).or_insert(0.0) += h.usd;
            if self.store.swap_until(&key, was.as_deref(), serde_json::to_string(&d).map_err(|e| e.to_string())?, Some((day + 1) * 86_400_000 + DAY_KEPT_MS)).await? { return Ok(()) }
        }
        Err(format!("{key} kept changing while it was written"))
    }

    /// What a job's record is to carry before it is written: what of its cost its day's totals will hold, and what to
    /// add to them once it is written (all it adds, when it is first counted; what its cost changed by, after).
    fn counting(&self, j: &mut Job) -> Option<Hour> {
        if !over(j) { return None }
        let now = self.clock.now_ms();
        let (_, h) = hour_of(j, self.host_price(j), now);
        let add = match j.counted_usd {
            None => h.clone(),
            Some(c) if (h.usd - c).abs() > 1e-9 => Hour { usd: h.usd - c, ..Default::default() },
            Some(_) => return None,
        };
        j.counted_usd = Some(h.usd);
        Some(add)
    }

    /// When a job's record goes: a while after it is over.
    fn job_end(j: &Job, now: u64) -> Option<u64> { over(j).then(|| j.ended_ms.unwrap_or(now).max(j.at_ms) + JOB_KEPT_MS) }

    /// A job's indexes, once its record is written: among the live ones while the sweep cares for it (and a little
    /// after), among the recent ones from its first write, and its day's totals.
    async fn indexes(&self, key: &str, j: &Job, new: bool, add: Option<Hour>) -> Result<()> {
        let now = self.clock.now_ms();
        let id = key.trim_start_matches("job:");
        if needs_care(j, now) { self.store.put(&format!("live:{id}"), String::new()).await? }
        else { self.store.put_until(&format!("live:{id}"), String::new(), now + LIVE_AFTER_MS).await? }
        if new {
            self.store.put_until(&format!("recent:{:013}:{id}", j.at_ms), String::new(), j.at_ms + JOB_KEPT_MS).await?;
            // A repository's newest job of GitHub's, for the look for jobs GitHub never told of (two weeks of them).
            if j.provider.is_empty() {
                let at = format!("repo:{}", j.repo);
                let newer = self.store.get(&at).await?.and_then(|v| serde_json::from_str::<Job>(&v).ok()).is_none_or(|kept| kept.at_ms <= j.at_ms);
                if newer { self.store.put_until(&at, serde_json::to_string(j).map_err(|e| e.to_string())?, j.at_ms + 14 * 86_400_000).await? }
            }
        }
        if let Some(h) = add { self.count(j, &h).await? }
        Ok(())
    }

    /// Builds the jobs' indexes from the jobs kept before them, `budget` jobs at a time (where it got to is kept);
    /// whether they are complete.
    async fn reindex(&self, budget: usize) -> Result<bool> {
        if self.indexed().await { return Ok(true) }
        let now = self.clock.now_ms();
        let after = self.store.get("reindex-at").await?.unwrap_or_default();
        let mut done = 0;
        for (key, v) in self.store.list("job:").await?.into_iter().filter(|(k, _)| *k > after) {
            if done == budget { self.store.put("reindex-at", key).await?; return Ok(false) }
            let Ok(mut j) = serde_json::from_str::<Job>(&v) else { continue };
            let add = self.counting(&mut j);
            if self.store.swap_until(&key, Some(&v), serde_json::to_string(&j).map_err(|e| e.to_string())?, Self::job_end(&j, now)).await? {
                self.indexes(&key, &j, true, add).await?;
            }
            done += 1;
        }
        self.store.put(INDEXED, now.to_string()).await?;
        self.store.delete("reindex-at").await?;
        self.cache.indexed.set(true);
        Ok(true)
    }

    async fn keep(&self, key: &str, job: &Job) -> Result<()> {
        // (Written over exactly what was read, see `Store::swap`: changed in between, it is read and weighed again.)
        for _ in 0..4 {
            let was = self.store.get(key).await.ok().flatten();
            let kept = was.as_deref().and_then(|v| serde_json::from_str::<Job>(v).ok()).filter(|kept| kept.rev != job.rev);
            let counted = was.as_deref().and_then(|v| serde_json::from_str::<Job>(v).ok()).and_then(|k| k.counted_usd);
            let mut write = match kept {
                None => { let fresh = job.clone(); fresh.rev.set(job.rev.get() + 1); fresh }
                Some(kept) if kept.ended_ms.is_some() => {
                    if job.machine_id.is_some() && job.machine_id != kept.machine_id {
                        let plane_url = get_json::<String>(self.store, "plane_url").await.ok().flatten().unwrap_or_default();
                        self.stop_machine(&plane_url, job).await;
                    }
                    if job.machine_id != kept.machine_id || job.runner != kept.runner || job.runner_id != kept.runner_id { self.withdraw_runner(job).await }
                    return Ok(())
                }
                Some(kept) => {
                    let mut merged = job.clone();
                    merged.seen_in_progress |= kept.seen_in_progress;
                    merged.began |= kept.began;
                    merged.interrupted |= kept.interrupted;
                    merged.started_ms = merged.started_ms.or(kept.started_ms);
                    merged.conclusion = merged.conclusion.take().or(kept.conclusion);
                    merged.rev.set(kept.rev.get() + 1);
                    merged
                }
            };
            // (What its day's totals hold is the kept record's, whoever read it before.)
            write.counted_usd = counted;
            let add = self.counting(&mut write);
            if self.store.swap_until(key, was.as_deref(), serde_json::to_string(&write).map_err(|e| e.to_string())?, Self::job_end(&write, self.clock.now_ms())).await? {
                job.rev.set(write.rev.get());
                self.indexes(key, &write, was.is_none(), add).await?;
                return Ok(())
            }
        }
        Err(format!("{key} kept changing while it was written"))
    }

    /// Whether a job's record ended since this copy of it was read (cancelled at its code host, in another request).
    async fn ended_meanwhile(&self, key: &str, job: &Job) -> bool {
        get_json::<Job>(self.store, key).await.ok().flatten().is_some_and(|kept| kept.rev != job.rev && kept.ended_ms.is_some())
    }

    /// A job just claimed gets its machine once the event that brought it is answered, where the runtime can do
    /// that (see `Timer::later`): GitHub and GitLab wait ten seconds for the answer and count the event as failed
    /// after, and AWS can take longer to give a machine. Where it cannot, here and now, as before.
    async fn start_soon(&self, plane_url: &str, job: &mut Job) -> Result<()> {
        let key = job_key(job);
        self.keep(&key, &*job).await?;
        if self.timer.later(&format!("start {key}")) { return Ok(()) }
        self.start(plane_url, job).await
    }

    /// Removes a launch template made for one request, and the note of it (kept when AWS did not remove it: a sweep
    /// tries again).
    async fn remove_template(&self, creds: &Credentials, region: &str, name: &str) {
        if aws::delete_template(self.http, region, creds, name, self.clock.now_ms()).await.is_ok() { let _ = self.store.delete(&template_key(region, name)).await; }
    }

    /// What was left for after an answer (see `Timer::later`), given back by the runtime with the address the
    /// request came to. A job to start: only as it was left, claimed and nothing of a machine yet (one cancelled
    /// meanwhile gets none, see `gone_key`; one taken up by a sweep is that sweep's).
    pub async fn later(&self, url: &str, what: &str) -> Result<()> {
        // (After a sweep there was no request: the control plane's own address, as it last saw it.)
        let base = match origin(url) { o if o.starts_with("http") => o, _ => get_json::<String>(self.store, "plane_url").await?.unwrap_or_default() };
        // A launch template whose one use is over (see `aws::fleet`).
        if let Some((region, name)) = what.strip_prefix("template ").and_then(|t| t.split_once(' ')) {
            if let Some(a) = self.aws_connection().await? { let creds = self.aws_creds(&base, &a).await?; self.remove_template(&creds, region, name).await }
            return Ok(())
        }
        let Some(key) = what.strip_prefix("start ") else { return Ok(()) };
        let Some(mut job) = get_json::<Job>(self.store, key).await? else { return Ok(()) };
        if job.state != "launching" || job.machine_id.is_some() || job.runner.is_some() { return Ok(()) }
        self.start(&base, &mut job).await
    }

    /// One machine for a job, of the size the label asks for, in the first place in the order that can run it now, with
    /// its work: a just-in-time GitHub runner registration (under the job's own label), or a GitLab project runner for
    /// the job's tags.
    async fn start(&self, plane_url: &str, job: &mut Job) -> Result<()> {
        let key = job_key(job);
        let gitlab = job.provider == "gitlab";
        let label = if job.label.is_empty() { self.config.label.clone() } else { job.label.clone() };
        let spec = match self.config.parse(&label) {
            Some(Ok(s)) => Ok(s.or(&self.config.machine)),
            Some(Err(e)) => Err(e),
            None => Err(format!("{label} is not this control plane's label")),
        };
        // Where it goes is decided again each time.
        job.cloud = String::new();
        // Whether it failed for what its label asks (not for its repository, nor for a cloud's hiccup): then any job with
        // that label fails here alike, and a failing runner can take it (see `fail_fast`).
        let mut intrinsic = false;
        let result: Result<()> = async {
            let spec = match spec { Ok(s) => s, Err(e) => { intrinsic = true; return Err(e) } };
            if let Some(why) = self.config.routing.too_large(&spec) { intrinsic = true; return Err(why) }
            let now = self.clock.now_ms();
            let places = match self.place(&job.repo, &spec, gitlab, !job.spot_again && (job.no_spot || self.spot_paused(now).await), &job.failed_at).await? {
                Placement::Run(p) => p,
                Placement::Wait(why) => {
                    job.state = "waiting".into();
                    job.error = Some(why);
                    self.keep(&key, &*job).await?;
                    return self.timer.wake_in(20_000).await;
                }
                Placement::Fail(why) => { intrinsic = spec.cloud.is_some() || self.config.routing.rule_for(&job.repo).is_none(); return Err(why) }
            };
            let cloud_of = |r: &Runner| match r { Runner::Aws(_) => "aws".to_string(), Runner::Agent(a) => a.cloud.clone(), Runner::Own => self.config.own_cloud.clone() };
            job.state = "launching".into();
            job.error = None;
            job.cloud = cloud_of(&places[0].1);
            job.on_demand = places[0].0 == AWS_ON_DEMAND;
            let name = format!("{}-{}{}{}", runner_prefix(self.config.name(), &self.config.plane_id), if gitlab { "gl" } else { "" }, if gitlab { gitlab::local(&job.gitlab, job.job_id) } else { job.job_id.to_string() }, job.runner_suffix);
            let work = if job.provider == "local" {
                // A run from a computer: its runner takes the job from this control plane, at its own address.
                let token = job.local.clone().ok_or("a run from a computer without its address")?;
                let jit = crate::runner_service::jit_config(&format!("{plane_url}/runner/{token}"), &name)?;
                job.runner = Some(name.clone());
                self.keep(&key, &*job).await?;
                self.store.put_until(&format!("runner:{name}"), job.job_id.to_string(), self.clock.now_ms() + RUNNER_KEPT_MS).await?;
                if let Some(mut handed) = get_json::<crate::runner_service::HandedJob>(self.store, &format!("rjob:{token}")).await? { handed.runner = name.clone(); put_json(self.store, &format!("rjob:{token}"), &handed).await? }
                Work::GitHub { jit }
            } else if gitlab {
                let gl = self.gitlab_of(job).ok_or("its GitLab is not connected any more")?;
                let token = self.gitlab_runner(gl, job).await?;
                job.runner = Some(name.clone());
                self.keep(&key, &*job).await?;
                Work::GitLab { url: gl.url.clone(), token }
            } else {
                let app = self.app_of(job).ok_or("no GitHub App")?;
                let owner = Self::owner_of(app);
                let token = self.installation_token(app, job.installation_id).await?;
                // Where GitHub's own runners check out code (Windows: the runner's own folder).
                let folder = if spec.os() == "windows" { "_work" } else { "/home/runner/work" };
                let (jit, runner_id) = github::jit_config(self.http, &app.api(), &token, &owner, &job.repo, &name, &[&label], folder).await?;
                job.runner = Some(name.clone());
                job.runner_id = Some(runner_id);
                self.keep(&key, &*job).await?;
                self.store.put_until(&format!("runner:{name}"), job.job_id.to_string(), self.clock.now_ms() + RUNNER_KEPT_MS).await?;
                Work::GitHub { jit }
            };
            // The places that can run it, in order. The job goes to the next when this one cannot start its machine now:
            // AWS has no spot machine for it, or starting it failed (a provider's hiccup, or its refusal).
            enum Tried { Started(String, String), NoRoom, OutOfTime, MoreSpot }
            let (mut started, began) = (None, self.clock.now_ms());
            // What each region said when it had no room, and whether only for the account's limits (quota).
            let (mut last, mut quota_only, mut answers) = (String::from("no AWS region"), true, Vec::<String>::new());
            let (mut tried_spot, mut tried_on_demand, mut out_of_time) = (false, false, false);
            // The same, of on-demand machines alone: whether a region refused one, and only for the account's limits.
            let (mut on_demand_refused, mut on_demand_quota_only) = (false, true);
            // What each place said where starting the machine failed.
            let mut failures: Vec<(String, String)> = vec![];
            let (mut more_spot, mut gone) = (false, false);
            for (at, (place, runner)) in places.into_iter().enumerate() {
                // Its job ended at its code host meanwhile, before any runner took it (cancelled): nothing more is asked for.
                if self.store.get(&gone_key(job)).await.ok().flatten().is_some() { gone = true; break }
                if self.ended_meanwhile(&key, job).await { gone = true; break }
                job.cloud = cloud_of(&runner);
                job.on_demand = place == AWS_ON_DEMAND;
                // What only the place before had (a spot machine's notice, Cloudflare's price while waiting).
                job.notice = None;
                job.waiting_usd_per_hour = None;
                // A runtime answers one request only so long (AWS Lambda: 30 seconds): the rest is left for the job's
                // next try.
                if at > 0 && self.clock.now_ms().saturating_sub(began) > LAUNCH_BUDGET_MS { out_of_time = true; break }
                let tried: Result<Tried> = match runner {
                    Runner::Aws(a) => async {
                        let on_demand = place == AWS_ON_DEMAND;
                        if on_demand { tried_on_demand = true } else { tried_spot = true }
                        // An organization that allowed no spot machine within the last ten minutes is not asked again
                        // for each job (it would refuse each one, a second lost every time): the job goes on down the
                        // order. Not longer: once its owner changes that rule, jobs are on spot within ten minutes.
                        if !on_demand && self.store.get(SPOT_NOT_ALLOWED_KEY).await.ok().flatten().and_then(|at| at.parse::<u64>().ok()).is_some_and(|at| now < at + 600_000) {
                            answers.push(format!("{}: {SPOT_NOT_ALLOWED}", self.aws_regions(&a).first().cloned().unwrap_or_default()));
                            quota_only = false;
                            job.spot_refused = Some(answers.join("; ").chars().take(300).collect());
                            return Ok(Tried::NoRoom)
                        }
                        let creds = self.aws_creds(plane_url, &a).await?;
                        let tags = vec![("Name".to_string(), machine_name_in_aws(self.config.name(), &job.repo, job.job_id)), ("superci-plane".into(), self.config.plane_id.clone()), ("superci-job".into(), job.job_id.to_string()), ("superci-repo".into(), job.repo.clone())];
                        // The standard machine keeps the configured types; a sized one gets the types that fit it.
                        let types = if spec.cpu.is_none() && spec.ram_gb.is_none() && spec.arch() == "x64" && spec.gpu.is_none() { self.config.instance_types.clone() } else { aws_instance_types(&spec) };
                        let mut disk_gb = spec.disk_gb.unwrap_or(60);
                        let (mut disk_mibs, mut disk_iops) = (DISK_MIBS_INCLUDED, DISK_IOPS_INCLUDED);
                        job.cpu = Some(Capacity::aws().fit(&spec).map(|s| s.cpu).unwrap_or(4));
                        // A spot machine says when AWS takes it back, with a link only it has.
                        job.notice = (!on_demand).then(|| crate::crypto::random_token(18));
                        let notice = job.notice.as_ref().map(|t| if gitlab { format!("{plane_url}/interrupted?gl={}&t={t}", gitlab::local(&job.gitlab, job.job_id)) } else { format!("{plane_url}/interrupted?runner={name}&t={t}") });
                        let user_data = aws::runner_user_data_for(&work, self.machine_minutes("aws"), spec.os(), notice.as_deref())?;
                        // What a job writes most goes on the machine's own NVMe disk when it has one, or in memory
                        // when its label asks (`scratch=ramdisk`).
                        let user_data = aws::with_scratch(user_data, spec.ramdisk());
                        // A Linux machine says how its own start went, with a link only it has.
                        // (Never a reason for a machine not to start: without a link it can use, it starts as before.)
                        let user_data = if spec.os() == "linux" && plane_url.starts_with("https://") {
                            let token = job.hello.get_or_insert_with(|| crate::crypto::random_token(18)).clone();
                            let link = if gitlab { format!("{plane_url}/hello?gl={}&t={token}", gitlab::local(&job.gitlab, job.job_id)) } else { format!("{plane_url}/hello?runner={name}&t={token}") };
                            aws::with_hello(user_data.clone(), &link).unwrap_or(user_data)
                        } else if spec.os() == "windows" && plane_url.starts_with("https://") {
                            // (A Windows machine says the same, and sends its runner's lines as they are written.)
                            let token = job.hello.get_or_insert_with(|| crate::crypto::random_token(18)).clone();
                            aws::with_hello_windows(user_data.clone(), &format!("{plane_url}/hello?runner={name}&t={token}")).unwrap_or(user_data)
                        } else { user_data };
                        let mut launched = None;
                        // The regions in order: the next when this one has no room (capacity or quota) for any of the types.
                        for region in &self.aws_regions(&a) {
                            if self.clock.now_ms().saturating_sub(began) > LAUNCH_BUDGET_MS { return Ok(Tried::OutOfTime) }
                            let said = |what: &str| format!("{region}{}: {what}", if on_demand { " on-demand" } else { "" });
                            let (image, image_gb) = match self.image(region, &spec, &creds, now).await { Ok(i) => i, Err(e) => { last = said(&e); continue } };
                            // Never smaller than its image's disk (Windows' is 100 GB).
                            disk_gb = disk_gb.max(image_gb);
                            let network = self.network(region, &creds, now).await?;
                            let types = self.offered(region, &types, &creds, now).await;
                            // Spot pools that had no machine, or took one back, in the last quarter of an hour: asked last.
                            let avoid = if on_demand { vec![] } else { self.pools_to_avoid(region, now).await };
                            // Many jobs queued at once: AWS lets a few requests through and refuses the rest for a
                            // moment ("RequestLimitExceeded"). The launch waits and asks again by itself, each job
                            // after a wait of its own (see `aws::run_instance`).
                            // The disk's speed: by the machine's size unless the label or the default machine says.
                            (disk_mibs, disk_iops) = disk_speed(job.cpu.unwrap_or(4), disk_gb, &spec);
                            let skip = self.pools_to_skip(region, !on_demand, self.clock.now_ms()).await;
                            // The spot pools left after the best has no machine are asked in one request, unless
                            // this role lately could not.
                            let fleet = !on_demand && !self.store.get(NO_FLEET_KEY).await.ok().flatten().and_then(|at| at.parse::<u64>().ok()).is_some_and(|at| now < at + 600_000);
                            // GitHub's cache, kept in this region's bucket, for a Linux machine of a GitHub job.
                            // (A GitLab job's machine is known by its runner's id: the job that runner takes is its own.)
                            let cache_runner = if gitlab { format!("gl:{}", gitlab::local(&job.gitlab, job.runner_id.unwrap_or_default())) } else { name.clone() };
                            let user_data = self.with_cache(user_data.clone(), region, spec.os(), spec.arch(), &creds, plane_url, &cache_runner, now).await;
                            let launch = Launch { region, network: network.as_ref(), image: &image, types: &types, user_data: &user_data, disk_gb, disk_mibs, disk_iops, tags: &tags, spot: !on_demand, os: spec.os(),
                                clock: Some(self.clock), skip: &skip, avoid: &avoid, timer: Some(self.timer), jitter_ms: (job.job_id % 8) * 120, fleet };
                            let mut told = aws::Told::default();
                            let machine = aws::run_instance(self.http, &creds, &launch, now, &mut told).await;
                            if let Some(e) = told.fleet_refused.take() {
                                let _ = self.store.put(NO_FLEET_KEY, now.to_string()).await;
                                self.note_permission::<()>("aws", "AskSpotPoolsAtOnce", &Err(e)).await;
                            } else if fleet && told.asked.iter().any(|a| a.4 > 0) { self.note_permission("aws", "AskSpotPoolsAtOnce", &Ok(())).await }
                            // The launch template of a request for many pools is removed: after the request at hand
                            // is answered where the runtime can do that (it is in the way of nothing, and the job's
                            // machine is the sooner known), else now. Noted first, so that one not removed for any
                            // reason is removed by a sweep.
                            if let Some(name) = told.template_left.take() {
                                let _ = self.store.put(&template_key(region, &name), now.to_string()).await;
                                if !self.timer.later(&format!("template {region} {name}")) { self.remove_template(&creds, region, &name).await }
                            }
                            // Each request and its answer is kept with the job (the last two dozen).
                            let mut at = self.clock.now_ms().saturating_sub(job.at_ms).saturating_sub(told.asked.iter().map(|a| a.3).sum());
                            for (kind, zone, said, ms, pools) in told.asked.drain(..) { job.asks.push(Ask { at, kind, zone, spot: !on_demand, said, ms, pools }); at += ms }
                            if job.asks.len() > 24 { let over = job.asks.len() - 24; job.asks.drain(..over); }
                            // A zone AWS does not offer a type in is not asked for it again (for a day).
                            for (kind, zone, said) in &told.refused { if said == "Unsupported" && !zone.is_empty() { let _ = self.store.put(&unoffered_key(region, kind, zone), now.to_string()).await; } }
                            // One request's worth of spot pools asked, none with a machine, and others left: the job
                            // goes on with those in a moment (see `MORE_SPOT`), before any on-demand machine.
                            let more = told.more && !on_demand && machine.as_ref().is_err_and(|e| aws::out_of_room(e)) && job.spot_rounds < 2;
                            // A spot pool with no machine now is remembered, for the jobs that come next.
                            if !on_demand { for (kind, zone, said) in &told.refused { if said == "InsufficientInstanceCapacity" && !zone.is_empty() { let _ = self.store.put(&pool_key(region, kind, zone), now.to_string()).await; } } }
                            // Still refused for the rate of requests after its waits: for a spot machine that is no
                            // room for it just now, said as that with the job (it goes on down the order, to an
                            // on-demand machine unless the order says otherwise, rather than wait a minute more).
                            if !on_demand && machine.as_ref().is_err_and(|e| e.starts_with("RequestLimitExceeded")) {
                                quota_only = false;
                                answers.push(said("RequestLimitExceeded"));
                                last = said(machine.as_ref().err().map(String::as_str).unwrap_or_default());
                                continue
                            }
                            // The account's AWS organization allows no spot machines (a policy of the organization, which
                            // nothing in the account lifts: no permission of the control plane's is missing). As with no
                            // room for one, the job goes on down the order, and says why it ran on-demand.
                            if !on_demand && machine.as_ref().is_err_and(|e| aws::refusal(e).is_some_and(|r| r.by == aws::By::Organization)) {
                                quota_only = false;
                                let _ = self.store.put(SPOT_NOT_ALLOWED_KEY, now.to_string()).await;
                                answers.push(said(SPOT_NOT_ALLOWED));
                                last = said(machine.as_ref().err().map(String::as_str).unwrap_or_default());
                                break
                            }
                            // AWS's budget for the account reached its limit and put its rule on this control plane's
                            // role (a budget its owner asked for: see `Routing::aws_budget`). No permission is
                            // missing and no other region or kind of machine would start: the job says what it is.
                            if machine.as_ref().is_err_and(|e| self.stopped_by_budget(e)) {
                                let _ = self.store.put(BUDGET_STOPPED_KEY, now.to_string()).await;
                                return Err(format!("{BUDGET_STOPPED}: raise the account's limit on Runners in the {} dashboard (that lifts it), or wait for next month", self.config.name()))
                            }
                            self.note_permission("aws", "LaunchOnlyTaggedMachines", &machine).await;
                            match machine {
                                Ok(m) => {
                                    // (A machine started: the budget's rule, if it was on the role, is off it.)
                                    if self.store.get(BUDGET_STOPPED_KEY).await.ok().flatten().is_some() { let _ = self.store.delete(BUDGET_STOPPED_KEY).await; }
                                    launched = Some((region.clone(), m, network.is_some_and(|n| n.private)));
                                    break
                                }
                                Err(e) if aws::out_of_room(&e) => {
                                    if more { return Ok(Tried::MoreSpot) }
                                    let quota = ["VcpuLimitExceeded", "MaxSpotInstanceCountExceeded"].iter().any(|c| e.starts_with(c));
                                    quota_only &= quota;
                                    if on_demand { on_demand_refused = true; on_demand_quota_only &= quota }
                                    answers.push(said(e.split(':').next().unwrap_or(&e)));
                                    last = said(&e);
                                }
                                // A network that is no longer there (removed, made again): looked up anew next try.
                                Err(e) => {
                                    if e.starts_with("InvalidSubnet") || e.starts_with("InvalidGroup") || e.starts_with("InvalidSecurityGroup") { self.cache.network.borrow_mut().clear(); }
                                    // An image that is no longer there (the account's copy was replaced by a newer build): looked up anew next try.
                                    if e.starts_with("InvalidAMIID") { self.cache.image.borrow_mut().clear(); }
                                    return Err(e)
                                }
                            }
                        }
                        let Some((region, machine, private)) = launched else {
                            // No region has room: what the regions said of spot goes with the job, down the order.
                            if !answers.is_empty() && tried_spot { job.spot_refused = Some(answers.join("; ").chars().take(300).collect()) }
                            return Ok(Tried::NoRoom)
                        };
                        if !on_demand { job.spot_refused = None }
                        job.private = private;
                        // Spot: its zone's price now. On-demand: AWS's list price. Settled at the prices in effect when it ends.
                        let price = if on_demand { let p = aws::on_demand_price(self.http, &creds, &region, &machine.kind, spec.os(), now).await; self.note_permission("aws", "Prices", &p).await; p.ok() }
                            else { aws::spot_price(self.http, &region, Some(&machine.zone), &creds, &machine.kind, spec.os(), now).await.ok() };
                        let cpu = job.cpu.unwrap_or(4);
                        job.usd_per_hour = Some(price.unwrap_or_else(|| aws::on_demand_usd_per_hour(cpu, spec.ram_gb.unwrap_or(cpu * 4))) + aws::extras_usd_per_hour(aws::disk_gb_month(&region), disk_gb, disk_mibs, disk_iops, !private));
                        job.region = Some(region);
                        job.zone = Some(machine.zone).filter(|z| !z.is_empty());
                        job.disk_gb = Some(disk_gb);
                        job.disk_mibs = Some(disk_mibs).filter(|m| *m > DISK_MIBS_INCLUDED);
                        job.disk_iops = Some(disk_iops).filter(|i| *i > DISK_IOPS_INCLUDED);
                        // (In memory as its label asked, or on its machine's own disk: a machine type that has one uses it.)
                        job.ramdisk = spec.ramdisk() && spec.os() == "linux";
                        job.local_disk = !job.ramdisk && spec.os() == "linux" && machine.kind.split('.').next().is_some_and(|family| family.ends_with('d') && crate::spec::FAMILIES.iter().any(|(f, _)| *f == family));
                        Ok(Tried::Started(machine.id, machine.kind))
                    }.await,
                    Runner::Own => async {
                        // Sized for the job (placement already checked it fits).
                        let size = capacity_of(&self.config.own_cloud).fit(&spec)?;
                        job.cpu = Some(size.cpu);
                        job.usd_per_hour = Some(if self.config.own_cloud == "modal" { modal_usd_per_hour(size.cpu, size.ram_gb) + modal_gpu_usd_per_hour(size.gpu) } else { cloudflare_usd_per_hour(size) });
                        if self.config.own_cloud == "cloudflare" { job.waiting_usd_per_hour = Some(cloudflare_waiting_usd_per_hour(size)) }
                        Ok(Tried::Started(self.containers.ok_or("no containers here")?.start(&name, &work, self.machine_minutes(&self.config.own_cloud), size).await?, machine_name(size)))
                    }.await,
                    Runner::Agent(a) => async {
                        let size = if a.cloud == "cloudflare" { Capacity::cloudflare() } else { Capacity::modal() }.fit(&spec)?;
                        job.cpu = Some(size.cpu);
                        job.usd_per_hour = match a.cloud.as_str() { "cloudflare" => Some(cloudflare_usd_per_hour(size)), "modal" => Some(modal_usd_per_hour(size.cpu, size.ram_gb) + modal_gpu_usd_per_hour(size.gpu)), _ => None };
                        if a.cloud == "cloudflare" { job.waiting_usd_per_hour = Some(cloudflare_waiting_usd_per_hour(size)) }
                        let mut body = serde_json::json!({ "name": name, "job": job.job_id, "repo": job.repo, "max_minutes": self.machine_minutes(&a.cloud), "cpu": size.cpu, "ram_gb": size.ram_gb, "disk_gb": size.disk_gb, "location": self.config.cloudflare_location });
                        if a.cloud == "cloudflare" { if let Some(i) = &self.config.cloudflare_image { body["image_url"] = i.clone().into() } }
                        if let Some(g) = size.gpu { body["gpu"] = g.to_uppercase().into() }
                        for (k, v) in work.json().as_object().into_iter().flatten() { body[k] = v.clone() }
                        let v = self.agent_call(plane_url, &a, "/launch", body).await?;
                        Ok(Tried::Started(v["id"].as_str().ok_or("runner agent answered without an id")?.to_string(), v["kind"].as_str().unwrap_or_default().to_string()))
                    }.await,
                };
                match tried {
                    Ok(Tried::Started(id, kind)) => { started = Some((id, kind)); break }
                    Ok(Tried::NoRoom) => {}
                    Ok(Tried::OutOfTime) => { out_of_time = true; break }
                    Ok(Tried::MoreSpot) => { more_spot = true; break }
                    // Remembered with the job: its next try starts with the places that have not failed it.
                    Err(e) => {
                        if !job.failed_at.contains(&place) { job.failed_at.push(place.clone()) }
                        // AWS failing alike for its spot and its on-demand machines (its network, its permissions) is said once.
                        if !(place == AWS_ON_DEMAND && failures.iter().any(|(p, said)| p == "aws" && *said == e)) { failures.push((place, e)) }
                    }
                }
            }
            if gone {
                // (Its runner goes with it; its record says cancelled, with no machine and no cost.)
                self.withdraw_runner(job).await;
                let _ = self.store.delete(&gone_key(job)).await;
                (job.state, job.began, job.runner_id, job.error) = ("cancelled".into(), true, None, None);
                job.ended_ms.get_or_insert(self.clock.now_ms());
                self.keep(&key, &*job).await?;
                return Ok(())
            }
            if more_spot && started.is_none() { job.spot_rounds += 1; return Err(MORE_SPOT.into()) }
            // Out of time with spot machines refused: the next try starts below them.
            if out_of_time {
                if !answers.is_empty() && !job.spot_again { job.no_spot = true; job.spot_refused = Some(answers.join("; ").chars().take(300).collect()) }
                last = format!("{last} (out of time for more tries)");
            }
            let (machine_id, machine_type) = match started {
                Some(m) => { job.failed_at.clear(); job.spot_rounds = 0; m }
                None => {
                    // An account allowed no GPU machines (AWS's quota for them starts at 0): no use waiting. Seen when
                    // every refusal was for the account's limits, or every on-demand one was (spot may have said
                    // something else: a type a zone does not have).
                    let no_quota = quota_only || (on_demand_refused && on_demand_quota_only);
                    if spec.gpu.is_some() && no_quota && !answers.is_empty() && !self.gpu_machines_up(job.job_id).await {
                        let (spot, od) = ("“All G and VT Spot Instance Requests” (https://console.aws.amazon.com/servicequotas/home/services/ec2/quotas/L-3819A6DF)", "“Running On-Demand G and VT instances” (https://console.aws.amazon.com/servicequotas/home/services/ec2/quotas/L-DB2E81BA)");
                        let why = format!("{NO_GPU_QUOTA}{}{}", match (tried_spot && quota_only, tried_on_demand) { (true, true) => format!("{spot} or {od}"), (true, false) => spot.to_string(), _ => od.to_string() }, " in each region it uses");
                        if failures.is_empty() { return Err(why) }
                        failures.push(("aws".into(), why));
                        answers.clear();
                    }
                    // Every refusal was for the account's limits, on-demand's among them, for a machine that is no GPU
                    // one (those are said above). (With only spot asked, the job waits for a spot machine as before.)
                    let at_limit = failures.is_empty() && !out_of_time && quota_only && tried_on_demand && !answers.is_empty() && spec.gpu.is_none();
                    // Kept off spot machines (run again after AWS took one back, or spot paused), with on-demand's
                    // limit not letting its machine start: a job larger than that limit would wait for room that
                    // never comes, when the spot limit it ran under a moment ago is free. A spot machine is asked
                    // for again (the pool that took one back is asked last, see `pools_to_avoid`).
                    if at_limit && !tried_spot && !job.spot_again && matches!(self.place(&job.repo, &spec, gitlab, false, &job.failed_at).await, Ok(Placement::Run(p)) if p.iter().any(|(c, _)| c == "aws")) {
                        return Err(format!("{BACK_ON_SPOT}{}", answers.join("; ")))
                    }
                    if at_limit { return Err(format!("{AT_LIMIT}{}", answers.join("; "))) }
                    return Err(match failures.len() {
                        0 if out_of_time => last,
                        // Only spot machines were asked for, no region has one, and nothing else in the order can run
                        // it: it waits for one (see `WAIT_FOR_SPOT`).
                        0 if tried_spot && !tried_on_demand && !answers.is_empty() => format!("{WAIT_FOR_SPOT}{}", answers.join("; ")),
                        // No spot machine now, and on-demand refused only for the account's limit: it waits for a
                        // spot machine likewise (an on-demand one is asked for each time too, should the limit have room).
                        0 if tried_spot && on_demand_refused && on_demand_quota_only && !answers.is_empty() && spec.gpu.is_none() => format!("{WAIT_FOR_SPOT_AT_LIMIT}{}", answers.join("; ")),
                        0 if answers.len() > 1 => format!("AWS had no room for it: {}", answers.join("; ")),
                        0 => last,
                        // One place, and it failed: what it said, as it said it.
                        1 if answers.is_empty() && !out_of_time => failures.remove(0).1,
                        _ => {
                            // Every place refused what the job asks (none a hiccup): it fails at once (see below).
                            if answers.is_empty() && !out_of_time && failures.iter().all(|(_, e)| refuses_alike(e)) { intrinsic = true; job.cloud = String::new() }
                            let mut said = vec![];
                            if !answers.is_empty() { said.push(format!("AWS had no room ({})", answers.join("; "))) }
                            said.extend(failures.iter().map(|(place, e)| format!("{}: {e}", cloud_name(place))));
                            if out_of_time { said.push("out of time for more tries".into()) }
                            format!("{SEVERAL}{}", said.join("; "))
                        }
                    })
                }
            };
            job.state = "launched".into();
            job.launched_ms = Some(self.clock.now_ms());
            job.machine_id = Some(machine_id);
            job.machine_type = Some(machine_type);
            // Its job ended at its code host while the machine was being asked for (cancelled before any runner took
            // it): the machine it has just been given is ended at once, not left for a sweep to find idle.
            if self.store.get(&gone_key(job)).await.ok().flatten().is_some() {
                self.stop_machine(plane_url, job).await;
                self.withdraw_runner(job).await;
                let _ = self.store.delete(&gone_key(job)).await;
                (job.state, job.began, job.runner_id) = ("cancelled".into(), true, None);
                job.ended_ms = Some(self.clock.now_ms());
                self.settle(plane_url, job).await;
                return self.keep(&key, &*job).await
            }
            self.keep(&key, &*job).await?;
            self.timer.wake_in(60_000).await
        }.await;
        if let Err(e) = result {
            self.withdraw_runner(job).await;
            job.runner_id = None;
            // Spot pools left to ask: gone on with in a moment, and not counted as a start that failed.
            if e == MORE_SPOT {
                job.state = "waiting".into();
                job.error = Some("waiting: asking AWS's other spot pools for a machine".into());
                self.keep(&key, &*job).await?;
                return self.timer.wake_in(1_000).await;
            }
            // The account's limits of running CPUs. While machines of this control plane's own are up there (or
            // ended a moment ago: AWS counts one a little longer than it runs), the job waits for one to end, as on
            // GitHub's own runners, and the sweep tries it again. With none of its own, the limit is below what the
            // job asks or used by something else in the account: tried again twice like any failed start, then it
            // fails saying which limits to raise.
            if let Some(said) = e.strip_prefix(BACK_ON_SPOT) {
                job.no_spot = false;
                job.spot_again = true;
                job.state = "waiting".into();
                job.error = Some(format!("waiting: an on-demand machine for it does not fit your AWS account's on-demand limit; asking for a spot machine again ({})", said.chars().take(160).collect::<String>()));
                self.keep(&key, &*job).await?;
                return self.timer.wake_in(2_000).await;
            }
            let e = match e.strip_prefix(AT_LIMIT) {
                None => e,
                Some(said) => {
                    let (ours, ended) = self.aws_machines_up(job.job_id).await;
                    if ours > 0 {
                        job.state = "waiting".into();
                        job.error = Some(format!("waiting: your AWS account's limit of running CPUs is in use by {ours} of your job{} there; it starts when one ends ({})", if ours == 1 { "" } else { "s" }, said.chars().take(160).collect::<String>()));
                        self.keep(&key, &*job).await?;
                        return self.timer.wake_in(20_000).await;
                    }
                    // None running, one ended a moment ago: AWS counts it a little longer. Nothing is waited for but that.
                    if ended > 0 {
                        job.state = "waiting".into();
                        job.error = Some(format!("waiting: AWS still counts a machine of yours that ended a moment ago against your account's limit of running CPUs; trying again ({})", said.chars().take(160).collect::<String>()));
                        self.keep(&key, &*job).await?;
                        return self.timer.wake_in(20_000).await;
                    }
                    format!("Your AWS account's limits do not let this machine start: it asks for {} CPUs ({said}). Ask AWS for more in Service Quotas: “All Standard (A, C, D, H, I, M, R, T, Z) Spot Instance Requests” (https://console.aws.amazon.com/servicequotas/home/services/ec2/quotas/{}) or “Running On-Demand Standard (A, C, D, H, I, M, R, T, Z) instances” (https://console.aws.amazon.com/servicequotas/home/services/ec2/quotas/L-1216C47A).",
                        job.cpu.unwrap_or(4), aws::SPOT_CPU_QUOTA)
                }
            };
            // No GPU machines allowed in the AWS account, or a runner agent refusing what was asked (Modal: no GPUs
            // without a payment method): every job asking for it fails alike, at once.
            if !e.starts_with(SEVERAL) && refuses_alike(&e) { intrinsic = true; job.cloud = String::new() }
            // No spot machine anywhere and no other place for it: it waits for one, however long (as for a place that is full).
            if e.starts_with(WAIT_FOR_SPOT) || e.starts_with(WAIT_FOR_SPOT_AT_LIMIT) {
                job.state = "waiting".into();
                job.error = Some(format!("{})", e.chars().take(280).collect::<String>()));
                self.keep(&key, &*job).await?;
                return self.timer.wake_in(30_000).await;
            }
            // A place was picked and starting the machine failed (Cloudflare "temporarily unavailable", a spot shortage):
            // tried again, twice; GitHub would otherwise keep the job queued for a day. No place for it: failed at once.
            // (Not what AWS's budget stopped: trying again changes nothing until the rule is lifted. Said at once.)
            if !job.cloud.is_empty() && job.retries < 2 && !e.contains(BUDGET_STOPPED) {
                job.retries += 1;
                job.state = "waiting".into();
                job.error = Some(format!("waiting: starting its machine failed, trying again ({})", e.chars().take(200).collect::<String>()));
                self.keep(&key, &*job).await?;
                return self.timer.wake_in(20_000).await;
            }
            job.state = "failed".into();
            job.error = Some(e.chars().take(700).collect());
            job.ended_ms.get_or_insert(self.clock.now_ms());
            self.keep(&key, &*job).await?;
            // It fails now at GitHub too, saying why, instead of waiting there a day: what its label asks that nothing
            // here can run, or a machine that would not start, three times. GitHub gives the failing runner any job with
            // the label, so not for what only its repository is refused (another repository's job could take it).
            if intrinsic || !job.cloud.is_empty() { self.fail_fast(plane_url, job, &e).await; }
        }
        Ok(())
    }

    /// A job nothing here can run (what its label asks: a machine no place has, like macOS, or a place not added): a
    /// small runner takes it and fails it before any step, saying why, rather than GitHub keeping it queued for
    /// a day. Only for what a label asks: GitHub gives a runner any job with its label, and every job with that label
    /// fails here alike. It runs where it costs least and starts soonest (Cloudflare, Modal, AWS), for ten minutes at
    /// most. A GitLab job is cancelled instead (GitLab can cancel one job).
    async fn fail_fast(&self, plane_url: &str, job: &mut Job, why: &str) {
        if job.provider == "gitlab" {
            if let (Some(gl), Some(project)) = (self.gitlab_of(job), job.project_id) { let _ = gitlab::cancel_job(self.http, gl, project, job.job_id).await; }
            return;
        }
        // Where it may run, cheapest and soonest first (no runner is registered at GitHub when there is no place for it).
        enum Place { Own, Agent(Agent), Aws(AwsConnection) }
        let mut places = vec![];
        if self.own_containers() { places.push(Place::Own) }
        for c in ["cloudflare", "modal"] { if let Some(a) = self.config.agents.iter().find(|a| a.cloud == c) { places.push(Place::Agent(a.clone())) } }
        if let Ok(Some(a)) = self.aws_connection().await.map(|a| a.filter(|a| a.connected)) { places.push(Place::Aws(a)) }
        if places.is_empty() { return }
        let Some(app) = self.app_of(job) else { return };
        let owner = Self::owner_of(app);
        let Ok(token) = self.installation_token(app, job.installation_id).await else { return };
        let label = if job.label.is_empty() { self.config.label.clone() } else { job.label.clone() };
        let name = format!("{}-{}{FAILING}", runner_prefix(self.config.name(), &self.config.plane_id), job.job_id);
        let Ok((jit, runner_id)) = github::jit_config(self.http, &app.api(), &token, &owner, &job.repo, &name, &[&label], "/home/runner/work").await else { return self.fail_later(job, why).await };
        // Known by its name, as a job's runner is: it may take another job with the label (see `failed_by_runner`).
        let _ = self.store.put_until(&format!("runner:{name}"), job.job_id.to_string(), self.clock.now_ms() + RUNNER_KEPT_MS).await;
        // macOS (no provider here runs it yet): what to do, in so many words.
        let mac = self.config.parse(&label).and_then(|s| s.ok()).is_some_and(|s| s.os() == "macos") && why.starts_with("nowhere to run");
        let called = self.config.name();
        let line = if mac { format!("::error title={called} could not run this job::{called} runs no macOS jobs yet. Use runs-on: macos-latest for GitHub's own.") }
            // A limit for the month: what to do is raising it or waiting (the whole CI's says so itself).
            else if why.contains(" reached its ") { format!("::error title={called} could not run this job::{}.{}", why.replace(['\r', '\n'], " ").trim_end_matches('.'), if why.contains("until the limit is raised") || why.contains(BUDGET_STOPPED) { String::new() } else { format!(" Raise that limit on Runners in the {called} dashboard, or wait for {}.", if why.contains(" a day") { "tomorrow" } else { "next month" }) }) }
            // A label that is not understood says itself what to write: nothing to add in the dashboard.
            else if why.contains("runs-on: ") { format!("::error title={called} could not run this job::{}.", why.replace(['\r', '\n'], " ").trim_end_matches('.')) }
            else { format!("::error title={called} could not run this job::{}. Change its runs-on, or add a place that can run it in the {called} dashboard.", why.replace(['\r', '\n'], " ").trim_end_matches('.')) };
        let work = Work::Fail { jit, why: crate::crypto::b64(line.as_bytes()) };
        let size = Size { cpu: 1, ram_gb: 4, disk_gb: 8, gpu: None };
        // The first place that starts it (a container that would not start, a cloud's hiccup: the next).
        for place in places {
            let started: Result<()> = async {
                match place {
                    Place::Own => self.containers.ok_or("no containers")?.start(&name, &work, 10, size).await.map(|_| ()),
                    Place::Agent(a) => {
                        let mut body = serde_json::json!({ "name": name, "job": job.job_id, "repo": job.repo, "max_minutes": 10, "cpu": 1, "ram_gb": 4, "disk_gb": 8, "location": self.config.cloudflare_location });
                        for (k, v) in work.json().as_object().into_iter().flatten() { body[k] = v.clone() }
                        self.agent_call(plane_url, &a, "/launch", body).await.map(|_| ())
                    }
                    Place::Aws(a) => {
                        let creds = self.aws_creds(plane_url, &a).await?;
                        let tags = vec![("Name".to_string(), machine_name_in_aws(self.config.name(), &job.repo, job.job_id)), ("superci-plane".into(), self.config.plane_id.clone()), ("superci-job".into(), job.job_id.to_string())];
                        // (The first two are the kind an AWS budget's rule lets through: see `aws::SAYS_WHY_TYPES`.)
                        let types = [aws::SAYS_WHY_TYPES[0].to_string(), aws::SAYS_WHY_TYPES[1].to_string(), "m7a.large".into()];
                        let user_data = aws::runner_user_data(&work, 10)?;
                        // A spot machine in the first region that has one; where spot is refused (its limit may be
                        // the very reason the job fails), an on-demand one, unless on-demand is turned off in the order.
                        let markets: &[bool] = if self.config.routing.pool(AWS_ON_DEMAND).is_some_and(|p| p.off) { &[true] } else { &[true, false] };
                        let mut started: Result<()> = Err("no AWS region".into());
                        'regions: for region in self.aws_regions(&a) {
                            let (image, image_gb) = match self.image(&region, &Spec::default(), &creds, self.clock.now_ms()).await { Ok(i) => i, Err(e) => { started = Err(e); continue } };
                            let network = match self.network(&region, &creds, self.clock.now_ms()).await { Ok(n) => n, Err(e) => { started = Err(e); continue } };
                            for spot in markets {
                                started = aws::run_instance(self.http, &creds, &aws::Launch { region: &region, network: network.as_ref(), image: &image, types: &types, user_data: &user_data, disk_gb: image_gb.max(30), disk_mibs: DISK_MIBS_INCLUDED, disk_iops: DISK_IOPS_INCLUDED, clock: None, skip: &[], tags: &tags, spot: *spot, os: "linux",
                                    avoid: &[], timer: Some(self.timer), jitter_ms: (job.job_id % 8) * 120, fleet: false }, self.clock.now_ms(), &mut aws::Told::default()).await.map(|_| ());
                                if started.is_ok() { break 'regions }
                            }
                        }
                        started
                    }
                }
            }.await;
            if started.is_ok() {
                job.failed_fast = true;
                job.fail_pending = None;
                let _ = self.keep(&job_key(job), &*job).await;
                return;
            }
        }
        // Nowhere would start it: its registration withdrawn, and tried again from the sweep.
        let _ = github::delete_runner(self.http, &app.api(), &token, &owner, &job.repo, runner_id).await;
        let _ = self.store.delete(&format!("runner:{name}")).await;
        self.fail_later(job, why).await
    }

    /// A failing runner that could not be started now: the sweep tries again, each minute for half an hour, while
    /// GitHub still has the job queued.
    async fn fail_later(&self, job: &mut Job, why: &str) {
        job.fail_pending = Some(why.to_string());
        let _ = self.keep(&job_key(job), &*job).await;
        let _ = self.timer.wake_in(60_000).await;
    }

    /// A failing runner ended a job. Its own: nothing more to do. Another with the same label (GitHub gives a runner
    /// any job with its label): that one failed for the first one's reason, said on it. Its machine, if one is coming,
    /// takes the first job, still queued (see `started`); if none is, the first job gets a failing runner again.
    async fn failed_by_runner(&self, runner: &str, job_id: u64) -> Result<()> {
        let first = self.by_runner(Some(runner)).await?;
        self.store.delete(&format!("runner:{runner}")).await?;
        let Some(mut first) = first.filter(|f| f.job_id != job_id) else { return Ok(()) };
        let Some(mut taken) = get_json::<Job>(self.store, &format!("job:{job_id}")).await? else { return Ok(()) };
        // One that was failing here too (its own failing runner takes the first), or one whose machine is busy with
        // another job (its record is that machine's).
        if !(active(&taken) || taken.state == "waiting") || taken.seen_in_progress { return Ok(()) }
        let why = first.error.clone().unwrap_or_default();
        taken.error = Some(format!("failed by the runner started to fail {} ({why})", if first.name.is_empty() { "another job" } else { first.name.as_str() }).chars().take(300).collect());
        taken.failed_fast = true;
        taken.for_job = Some(first.job_id);
        if taken.state == "waiting" {
            taken.for_job = None;
            taken.state = "failed".into();
            taken.ended_ms = Some(self.clock.now_ms());
            first.failed_fast = false;
            first.fail_pending = Some(why);
            self.keep(&job_key(&first), &first).await?;
        }
        self.keep(&format!("job:{job_id}"), &taken).await?;
        self.timer.wake_in(60_000).await
    }

    async fn by_runner(&self, runner: Option<&str>) -> Result<Option<Job>> {
        let Some(name) = runner else { return Ok(None) };
        let Some(job_id) = get_json::<u64>(self.store, &format!("runner:{name}")).await? else { return Ok(None) };
        get_json::<Job>(self.store, &format!("job:{job_id}")).await
    }

    /// A job began on a runner. GitHub gives a waiting job to any idle runner with its label, so a runner may begin a
    /// job other than the one it was started for. When that other job has no machine of its own coming any more (its
    /// machine failed or was swept), this runner becomes its machine, and the job it was started for gets a new one;
    /// otherwise the two simply swap (the other job's machine takes this one's job).
    async fn started(&self, plane_url: &str, job_id: u64, runner: Option<&str>) -> Result<()> {
        // A failing runner (see `fail_fast`) is no machine of a job's: what it took is settled when it ends.
        if runner.is_some_and(failing_runner) { return Ok(()) }
        if let (Some(mut j), Some(name)) = (self.by_runner(runner).await?, runner) {
            if j.job_id != job_id {
                if let Some(mut other) = get_json::<Job>(self.store, &format!("job:{job_id}")).await? {
                    if !["launching", "launched", "running"].contains(&other.state.as_str()) {
                        let now = self.clock.now_ms();
                        // What that job's record holds of a machine that has ended (it ran another job's work and
                        // was ended with it, while this job still waited): kept below, not written over.
                        let ended = (other.machine_id.is_some() && other.ended_ms.is_some()).then(|| other.clone());
                        other.runner = j.runner.clone();
                        other.runner_id = j.runner_id;
                        other.machine_id = j.machine_id.clone();
                        other.machine_type = j.machine_type.clone();
                        other.cloud = j.cloud.clone();
                        other.region = j.region.clone();
                        other.cpu = j.cpu;
                        other.usd_per_hour = j.usd_per_hour;
                        // The machine as it is: where it is, what it costs and since when, and what it says if AWS
                        // takes it back.
                        (other.zone, other.disk_gb, other.waiting_usd_per_hour, other.launched_ms) = (j.zone.take(), j.disk_gb.take(), j.waiting_usd_per_hour.take(), j.launched_ms);
                        (other.on_demand, other.notice, other.interrupted, other.private, other.for_job) = (j.on_demand, j.notice.take(), j.interrupted, j.private, None);
                        other.spot_refused = j.spot_refused.take();
                        (other.cost_usd, other.cost_from, other.sent_gb, other.fail_pending, other.failed_fast) = (None, None, None, None, false);
                        other.state = "running".into();
                        (other.seen_in_progress, other.began) = (true, true);
                        other.error = None;
                        other.started_ms = Some(now);
                        other.ended_ms = None;
                        self.keep(&format!("job:{}", other.job_id), &other).await?;
                        self.store.put_until(&format!("runner:{name}"), other.job_id.to_string(), self.clock.now_ms() + RUNNER_KEPT_MS).await?;
                        // The job this runner was for: a machine of its own, again.
                        let key = job_key(&j);
                        (j.runner, j.runner_id, j.machine_id, j.machine_type) = (None, None, None, None);
                        j.interrupted = false;
                        j.cloud = String::new();
                        // A job that is over (cancelled before a runner took it, or failed by another's failing
                        // runner) needs no machine again. Nor does one whose own job began on another job's runner
                        // (`for_job`: its machine was for that other job) once that other job has got this machine.
                        if j.state == "orphan" || j.failed_fast || j.over || j.for_job == Some(job_id) {
                            j.state = if j.state == "orphan" { "cancelled" } else if j.failed_fast { "failed" } else { "done" }.into();
                            j.for_job = None;
                            j.ended_ms.get_or_insert(now);
                            // The machine that ended with another job's work stays in the books, here.
                            if let Some(e) = ended.filter(|_| j.state == "done") {
                                (j.cloud, j.machine_id, j.machine_type, j.region, j.zone, j.disk_gb, j.cpu) = (e.cloud, e.machine_id, e.machine_type, e.region, e.zone, e.disk_gb, e.cpu);
                                (j.usd_per_hour, j.waiting_usd_per_hour, j.launched_ms, j.started_ms, j.ended_ms) = (e.usd_per_hour, e.waiting_usd_per_hour, e.launched_ms, e.started_ms, e.ended_ms);
                                (j.cost_usd, j.cost_from, j.sent_gb, j.on_demand, j.private, j.seen_in_progress) = (e.cost_usd, e.cost_from, e.sent_gb, e.on_demand, e.private, true);
                            }
                            return self.keep(&key, &j).await;
                        }
                        // (Kept for another job, `for_job`, it stays so: the new machine is for that job.)
                        j.state = "launching".into();
                        j.error = None;
                        j.launched_ms = Some(now);
                        j.runner_suffix = format!("-b{}", now % 100_000);
                        self.keep(&key, &j).await?;
                        return self.start(plane_url, &mut j).await;
                    }
                }
            }
        }
        if let Some(mut j) = self.by_runner(runner).await? {
            // It began another job than its own, whose machine is still coming: that machine is for this runner's
            // job now (or for whichever job this machine was for).
            if j.job_id != job_id {
                if let Some(mut other) = get_json::<Job>(self.store, &format!("job:{job_id}")).await? {
                    let coming = active(&other) && !other.seen_in_progress;
                    if coming { other.for_job = Some(j.for_job.unwrap_or(j.job_id)) }
                    if coming || !other.began {
                        other.began = true;
                        self.keep(&format!("job:{job_id}"), &other).await?;
                    }
                }
                j.ran = Some(job_id);
            } else {
                j.began = true;
                j.ran = None;
            }
            j.for_job = None;
            j.seen_in_progress = true;
            // A machine left idle by a cancelled job (an orphan) that took a job before the sweep came is busy too.
            if j.state == "launched" || j.state == "orphan" { j.state = "running".into(); }
            j.started_ms.get_or_insert(self.clock.now_ms());
            self.keep(&format!("job:{}", j.job_id), &j).await?;
        } else if let Some(mut own) = get_json::<Job>(self.store, &format!("job:{job_id}")).await?.filter(|o| !o.began) {
            // It began on a runner that is none of this control plane's.
            own.began = true;
            self.keep(&format!("job:{job_id}"), &own).await?;
        }
        Ok(())
    }

    /// A machine nobody waits for any more (its job was cancelled before any runner took it), ended at once: its
    /// runner is taken back from GitHub first, which GitHub refuses while the runner is running a job (it may have
    /// been given another one a moment ago, the event still on its way). Only then is the machine stopped. Whether it was.
    async fn take_back(&self, plane_url: &str, j: &mut Job) -> bool {
        let (Some(app), Some(runner_id)) = (self.app_of(j), j.runner_id) else { return false };
        if !j.provider.is_empty() { return false }
        let Ok(token) = self.installation_token(app, j.installation_id).await else { return false };
        if github::delete_runner(self.http, &app.api(), &token, &Self::owner_of(app), &j.repo, runner_id).await.is_err() { return false }
        if let Some(name) = &j.runner { let _ = self.store.delete(&format!("runner:{name}")).await; }
        self.stop_machine(plane_url, j).await;
        (j.state, j.runner_id) = ("cancelled".into(), None);
        j.ended_ms.get_or_insert(self.clock.now_ms());
        self.settle(plane_url, j).await;
        true
    }

    /// A part of the working tree a run from a computer is started with (a gzipped tar, cut in parts of a few MB),
    /// for its checkout to fetch (`for_runner`). With `link`: a link to put it in the control plane's bucket, where
    /// there is one (`rtree-s3:<tree>:<part>` says where), so a tree of many MB never goes through the store (a
    /// DynamoDB table writes only so much a second); else (`url` null), or without `link`, it is kept in pieces the
    /// store takes (`rtree:<tree>:<part>:<piece>`).
    async fn tree_part(&self, url: &url::Url, req: &Request, plane_url: &str) -> Result<Response> {
        let q: HashMap<String, String> = url.query_pairs().into_owned().collect();
        let id = q.get("id").filter(|i| (16..=64).contains(&i.len()) && i.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'));
        let (Some(id), Some(part)) = (id, q.get("part").and_then(|p| p.parse::<u32>().ok()).filter(|p| *p < 1000)) else { return Ok(Response::text(400, "say which tree (id) and which part")) };
        if q.contains_key("link") {
            let now = self.clock.now_ms();
            let Some((region, creds)) = self.tree_bucket(plane_url, now).await? else { return Ok(Response::json(&serde_json::json!({ "url": null }))) };
            let bucket = cache::bucket(&self.config.plane_id, &region);
            let named = |n: &String| (8..=80).contains(&n.len()) && n.bytes().all(|b| b.is_ascii_alphanumeric());
            let _ = self.store.put_if_absent(&format!("rtree-at:{id}"), now.to_string()).await;
            // A tree the computer keeps as a base for later runs' (`keep`): in the bucket under its name, kept there
            // as long as the bucket keeps anything; or only what changed since one kept before (`base`), on top of it.
            let object = match (q.get("keep").filter(|n| named(n)), q.get("base").filter(|n| named(n))) {
                (Some(name), _) => format!("bases/{name}.tar.gz"),
                (None, Some(name)) => {
                    let base = format!("bases/{name}.tar.gz");
                    let there = aws::s3(self.http, "HEAD", &aws::s3_url(&bucket, &region, &base), &region, &creds, vec![], &[], now).await.is_ok_and(|r| r.status == 200);
                    if !there { return Ok(Response::json(&serde_json::json!({ "url": null, "base": false }))) }
                    self.store.put(&format!("rtree-s3:{id}:00000"), format!("{region}|{base}")).await?;
                    let changed = tree_object(id, 1);
                    self.store.put(&format!("rtree-s3:{id}:00001"), format!("{region}|{changed}")).await?;
                    return Ok(Response::json(&serde_json::json!({ "url": aws::presign("PUT", &aws::s3_url(&bucket, &region, &changed), &creds, &region, cache::LINK_SECONDS, now)?, "base": true, "parts": 2 })))
                }
                _ => tree_object(id, part),
            };
            let link = aws::presign("PUT", &aws::s3_url(&bucket, &region, &object), &creds, &region, cache::LINK_SECONDS, now)?;
            self.store.put(&format!("rtree-s3:{id}:{part:05}"), format!("{region}|{object}")).await?;
            return Ok(Response::json(&serde_json::json!({ "url": link })))
        }
        if req.body.len() > 5_000_000 { return Ok(Response::text(413, "a part is 5 MB at most")) }
        for (i, piece) in req.body.chunks(300_000).enumerate() { self.store.put(&format!("rtree:{id}:{part:05}:{i:03}"), crate::crypto::b64(piece)).await? }
        let _ = self.store.put_if_absent(&format!("rtree-at:{id}"), self.clock.now_ms().to_string()).await;
        Ok(Response::json(&serde_json::json!({ "stored": req.body.len() })))
    }

    /// The bucket a working tree goes to: the first of the AWS regions whose bucket is ready, with the sign-in to it.
    async fn tree_bucket(&self, plane_url: &str, now: u64) -> Result<Option<(String, Credentials)>> {
        let Some(a) = self.aws_connection().await?.filter(|a| a.connected) else { return Ok(None) };
        let creds = self.aws_creds(plane_url, &a).await?;
        for region in self.aws_regions(&a) {
            if self.cache_ready(&region, "x86_64", &creds, now).await { return Ok(Some((region, creds))) }
        }
        Ok(None)
    }

    /// A run's working tree, gone: its pieces in the store, and its parts in the bucket.
    async fn forget_tree(&self, plane_url: &str, tree: &str) -> Result<()> {
        for (k, _) in self.store.list(&format!("rtree:{tree}:")).await? { self.store.delete(&k).await? }
        let in_bucket = self.store.list(&format!("rtree-s3:{tree}:")).await?;
        if !in_bucket.is_empty() {
            let now = self.clock.now_ms();
            let creds = match self.aws_connection().await?.filter(|a| a.connected) { Some(a) => Some(self.aws_creds(plane_url, &a).await?), None => None };
            for (k, kept) in in_bucket {
                let (region, object) = tree_place(kept.trim_matches('"'), tree, k.rsplit(':').next().and_then(|p| p.parse::<u32>().ok()).unwrap_or(0));
                // (A base stays for later runs' trees; the bucket removes it in time.)
                if let (Some(creds), true) = (&creds, object.starts_with("runs/")) { let _ = aws::s3(self.http, "DELETE", &aws::s3_url(&cache::bucket(&self.config.plane_id, &region), &region, &object), &region, creds, vec![], &[], now).await; }
                self.store.delete(&k).await?;
            }
        }
        self.store.delete(&format!("rtree-at:{tree}")).await?;
        Ok(())
    }

    /// A log a run's job sent (`n`, the runner's number for it), whole: its pieces in order, from the store or the
    /// bucket.
    async fn log_text(&self, token: &str, n: u64) -> Result<Vec<u8>> {
        use base64::Engine;
        let mut out = vec![];
        let mut creds = None;
        for (k, v) in self.store.list(&format!("rlog:{token}:{n:04}:")).await? {
            match v.trim_matches('"').strip_prefix("s3:") {
                Some(region) => {
                    if creds.is_none() {
                        let plane_url = get_json::<String>(self.store, "plane_url").await?.unwrap_or_default();
                        if let Some(a) = self.aws_connection().await?.filter(|a| a.connected) { creds = Some(self.aws_creds(&plane_url, &a).await?) }
                    }
                    let Some(c) = &creds else { continue };
                    let seq = k.rsplit(':').next().unwrap_or("0000");
                    let got = aws::s3(self.http, "GET", &aws::s3_url(&cache::bucket(&self.config.plane_id, region), region, &format!("runs/{token}/log-{n:04}-{seq}")), region, c, vec![], &[], self.clock.now_ms()).await?;
                    if got.status < 300 { out.extend(got.body) }
                }
                None => out.extend(base64::engine::general_purpose::STANDARD.decode(v.trim_matches('"')).map_err(|e| e.to_string())?),
            }
        }
        Ok(out)
    }

    /// A post of a job's lines from its runner (see `runner_service::feed`), kept under when it came (to the
    /// millisecond, then a count): its step and where in that step's log it begins. Only for a runner's link known
    /// to be a job's (asked of the store once per runtime, then remembered).
    async fn lines_in(&self, token: &str, sub: &str, req: &Request) -> Result<Response> {
        let now = self.clock.now_ms();
        let known = self.cache.runner_links.borrow().get(token).is_some_and(|until| *until > now);
        if !known {
            if self.store.get(&format!("rjob:{token}")).await?.is_none() { return Ok(nothing_here()) }
            self.cache.runner_links.borrow_mut().insert(token.to_string(), now + 3_600_000);
        }
        let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap_or_default();
        let lines: Vec<String> = body["value"].as_array().into_iter().flatten().filter_map(|l| l.as_str().map(str::to_string)).collect();
        if !lines.is_empty() {
            let seq = self.cache.lines_seq.get().wrapping_add(1) % 1000;
            self.cache.lines_seq.set(seq);
            let step = body["stepId"].as_str().map(str::to_string).or_else(|| sub.split('?').next().unwrap_or(sub).trim_end_matches('/').rsplit('/').next().map(str::to_string));
            self.store.put_until(&format!("rline:{token}:{now:013}{seq:03}"), serde_json::json!({ "step": step, "start": body["startLine"], "lines": lines }).to_string(), now + crate::runner_service::KEPT_MS).await?;
        }
        Ok(Response::new(200, "application/json", ""))
    }

    /// A piece of what a GitHub job's runner wrote as it went (`file`, a page of its log; `at`, where in it): in the
    /// bucket where there is one (the store keeps where), else in the store. Gone after two days.
    async fn keep_live(&self, id: &str, file: &str, at: u64, body: &[u8], plane_url: &str, now: u64) -> Result<()> {
        let (mark, until) = (format!("glive:{id}:{file}:{at:013}"), now + 2 * 86_400_000);
        if let Some((region, creds)) = self.tree_bucket(plane_url, now).await? {
            let object = format!("live/{id}/{file}-{at:013}");
            let put = aws::s3(self.http, "PUT", &aws::s3_url(&cache::bucket(&self.config.plane_id, &region), &region, &object), &region, &creds, body.to_vec(), &[], now).await?;
            if put.status < 300 { return self.store.put_until(&mark, format!("s3:{region}:{}", body.len()), until).await }
        }
        for (i, piece) in body.chunks(crate::runner_service::LOG_PIECE).enumerate() {
            self.store.put_until(&format!("{mark}:{i:03}"), crate::crypto::b64(piece), until).await?;
        }
        Ok(())
    }

    /// What a job's runner has written so far of one step (its pages: `<timeline>_<step>_<n>.log`), as the step's log
    /// will be: none, when its machine sent none.
    async fn step_pages(&self, id: &str, step: &str) -> Result<Option<String>> {
        use base64::Engine;
        if step.is_empty() || !step.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-') { return Ok(None) }
        let marks: Vec<(String, String)> = self.store.list(&format!("glive:{id}:")).await?.into_iter().filter(|(k, _)| k.contains(&format!("_{step}_"))).collect();
        if marks.is_empty() { return Ok(None) }
        let plane_url = get_json::<String>(self.store, "plane_url").await?.unwrap_or_default();
        let mut creds = None;
        let mut out = vec![];
        for (k, v) in &marks {
            let rest = k.strip_prefix(&format!("glive:{id}:")).unwrap_or_default();
            match v.trim_matches('"').strip_prefix("s3:") {
                Some(r) => {
                    let region = r.split(':').next().unwrap_or_default();
                    if creds.is_none() { if let Some(a) = self.aws_connection().await?.filter(|a| a.connected) { creds = Some(self.aws_creds(&plane_url, &a).await?) } }
                    let Some(c) = &creds else { continue };
                    let (file, at) = (rest.split(':').next().unwrap_or_default(), rest.split(':').nth(1).unwrap_or_default());
                    let got = aws::s3(self.http, "GET", &aws::s3_url(&cache::bucket(&self.config.plane_id, region), region, &format!("live/{id}/{file}-{at}")), region, c, vec![], &[], self.clock.now_ms()).await?;
                    if got.status < 300 { out.extend(got.body) }
                }
                None => out.extend(base64::engine::general_purpose::STANDARD.decode(v.trim_matches('"')).map_err(|e| e.to_string())?),
            }
        }
        // (Only whole lines: the last may still be being written.)
        let text = String::from_utf8_lossy(&out).into_owned();
        Ok(Some(match text.rfind('\n') { Some(i) => text[..=i].to_string(), None => String::new() }))
    }

    /// What a GitHub job's runner wrote as it went, its last `want` bytes or so: each page in order (by the time its
    /// first line was written), its pieces one after another.
    async fn live_log(&self, id: &str, want: usize) -> Result<Vec<u8>> {
        use base64::Engine;
        let marks = self.store.list(&format!("glive:{id}:")).await?;
        let size = |v: &str| v.trim_matches('"').strip_prefix("s3:").and_then(|r| r.rsplit(':').next()).and_then(|n| n.parse::<usize>().ok()).unwrap_or(v.len() * 3 / 4);
        // From the end, as much as is wanted.
        let (mut total, mut from) = (0usize, marks.len());
        while from > 0 && total < want { from -= 1; total += size(&marks[from].1) }
        let plane_url = get_json::<String>(self.store, "plane_url").await?.unwrap_or_default();
        let mut creds = None;
        let mut pages: Vec<(String, Vec<u8>)> = vec![];
        for (k, v) in &marks[from..] {
            let rest = k.strip_prefix(&format!("glive:{id}:")).unwrap_or_default();
            let file = rest.split(':').next().unwrap_or_default().to_string();
            let bytes = match v.trim_matches('"').strip_prefix("s3:") {
                Some(r) => {
                    let region = r.split(':').next().unwrap_or_default();
                    if creds.is_none() { if let Some(a) = self.aws_connection().await?.filter(|a| a.connected) { creds = Some(self.aws_creds(&plane_url, &a).await?) } }
                    let Some(c) = &creds else { continue };
                    let at = rest.split(':').nth(1).unwrap_or_default();
                    let got = aws::s3(self.http, "GET", &aws::s3_url(&cache::bucket(&self.config.plane_id, region), region, &format!("live/{id}/{file}-{at}")), region, c, vec![], &[], self.clock.now_ms()).await?;
                    if got.status < 300 { got.body } else { continue }
                }
                None => base64::engine::general_purpose::STANDARD.decode(v.trim_matches('"')).map_err(|e| e.to_string())?,
            };
            match pages.iter_mut().find(|(f, _)| *f == file) { Some((_, b)) => b.extend(bytes), None => pages.push((file, bytes)) }
        }
        // Every line in the order it was written (each begins with its time), once: the runner writes a step's lines to
        // the step's pages and again, a moment later, to the job's. A line also in a smaller page within two seconds
        // is that one's (the job's page is the largest); a line said twice in one page stays twice.
        // (A message of several lines has its time on its first only: they stay together.)
        let timed = |l: &str| { let b = l.trim_start_matches('\u{feff}').as_bytes(); b.len() > 28 && b[4] == b'-' && b[10] == b'T' && b[..4].iter().all(u8::is_ascii_digit) };
        let secs = |e: &str| e.trim_start_matches('\u{feff}').get(..19).map(|t| t.to_string());
        let text_of = |e: &str| e.trim_start_matches('\u{feff}').get(29..).unwrap_or_default().to_string();
        let mut files: Vec<Vec<String>> = pages.iter().map(|(_, b)| {
            let mut entries: Vec<String> = vec![];
            // (A page's last line, while it is still being written, waits until it is whole.)
            let text = String::from_utf8_lossy(b);
            let whole = if text.ends_with('\n') { &text[..] } else { text.rsplit_once('\n').map(|(w, _)| w).unwrap_or("") };
            for l in whole.lines().filter(|l| !l.is_empty()) {
                match entries.last_mut() { Some(e) if !timed(l) => { e.push('\n'); e.push_str(l) } _ => entries.push(l.to_string()) }
            }
            entries
        }).collect();
        files.sort_by_key(|f| std::cmp::Reverse(f.len()));
        let near = |a: &Option<String>, b: &Option<String>| match (a.as_deref().and_then(|t| aws::parse_iso_ms(&format!("{t}Z"))), b.as_deref().and_then(|t| aws::parse_iso_ms(&format!("{t}Z")))) { (Some(x), Some(y)) => x.abs_diff(y) <= 2_000, _ => false };
        let mut entries: Vec<String> = vec![];
        for (i, f) in files.iter().enumerate() {
            let smaller: std::collections::HashMap<String, Vec<Option<String>>> = files[i + 1..].iter().flatten().fold(std::collections::HashMap::new(), |mut m, e| { m.entry(text_of(e)).or_insert_with(Vec::new).push(secs(e)); m });
            entries.extend(f.iter().filter(|e| !smaller.get(&text_of(e)).is_some_and(|at| at.iter().any(|t| near(t, &secs(e))))).cloned());
        }
        entries.sort_by(|a, b| a.trim_start_matches('\u{feff}').get(..28).cmp(&b.trim_start_matches('\u{feff}').get(..28)));
        Ok(entries.into_iter().flat_map(|l| format!("{l}\n").into_bytes()).collect())
    }

    /// Links to the pieces of a run's job's whole log, step after step: none when any is kept in the store.
    async fn log_links(&self, token: &str) -> Result<Option<Vec<String>>> {
        let handed = get_json::<crate::runner_service::HandedJob>(self.store, &format!("rjob:{token}")).await?.unwrap_or_default();
        let mut steps: Vec<&serde_json::Value> = handed.steps.iter().filter(|s| s["type"] != "Job" && s["log"].as_u64().is_some()).collect();
        steps.sort_by_key(|s| s["order"].as_u64().unwrap_or(0));
        let Some(a) = self.aws_connection().await?.filter(|a| a.connected) else { return Ok(None) };
        let plane_url = get_json::<String>(self.store, "plane_url").await?.unwrap_or_default();
        let (creds, now) = (self.aws_creds(&plane_url, &a).await?, self.clock.now_ms());
        let mut urls = vec![];
        for s in steps {
            let n = s["log"].as_u64().unwrap_or(0);
            for (k, v) in self.store.list(&format!("rlog:{token}:{n:04}:")).await? {
                let Some(region) = v.trim_matches('"').strip_prefix("s3:") else { return Ok(None) };
                let seq = k.rsplit(':').next().unwrap_or("0000");
                urls.push(aws::presign("GET", &aws::s3_url(&cache::bucket(&self.config.plane_id, region), region, &format!("runs/{token}/log-{n:04}-{seq}")), &creds, region, cache::LINK_SECONDS, now)?);
            }
        }
        Ok(Some(urls))
    }

    /// A run's job's steps in order, each with its log: the whole one once the runner sent it, else the lines it sent
    /// of the step as it went.
    async fn local_steps(&self, token: &str) -> Result<Vec<serde_json::Value>> {
        let handed = get_json::<crate::runner_service::HandedJob>(self.store, &format!("rjob:{token}")).await?.unwrap_or_default();
        let mut steps: Vec<&serde_json::Value> = handed.steps.iter().filter(|s| s["type"] != "Job").collect();
        steps.sort_by_key(|s| s["order"].as_u64().unwrap_or(0));
        let posts: Vec<(Option<String>, Vec<String>)> = self.store.list(&format!("rline:{token}:")).await?.into_iter().map(|(_, v)| { let (step, _, lines) = crate::runner_service::feed(&v); (step, lines) }).collect();
        let mut out = vec![];
        let job_id = get_json::<crate::runner_service::HandedJob>(self.store, &format!("rjob:{token}")).await?.map(|h| h.job_id).unwrap_or(0);
        for s in steps {
            let text = match s["log"].as_u64() {
                Some(n) => String::from_utf8_lossy(&self.log_text(token, n).await?).into_owned(),
                // Still going: what its machine sent of its pages (whole), else the lines the runner sent as it went
                // (it drops some when they come fast).
                None => match self.step_pages(&job_id.to_string(), s["id"].as_str().unwrap_or_default()).await? {
                    Some(text) if !text.is_empty() => text,
                    _ => posts.iter().filter(|(step, _)| step.as_deref() == s["id"].as_str()).flat_map(|(_, l)| l.iter().cloned()).collect::<Vec<_>>().join("\n"),
                },
            };
            out.push(serde_json::json!({ "name": s["name"], "state": s["state"], "result": s["result"], "start": s["start"], "finish": s["finish"], "text": text }));
        }
        Ok(out)
    }

    /// A run's job's log: each step's whole log in order, where the runner sent them; else the lines it sent as it went.
    async fn local_log(&self, token: &str) -> Result<Vec<u8>> {
        let handed = get_json::<crate::runner_service::HandedJob>(self.store, &format!("rjob:{token}")).await?.unwrap_or_default();
        // Each step's: its whole log once sent, else what its machine sent of it so far.
        let mut out = vec![];
        if !handed.steps.is_empty() {
            for s in self.local_steps(token).await? { out.extend(s["text"].as_str().unwrap_or_default().as_bytes()); if !out.ends_with(b"\n") && !out.is_empty() { out.push(b'\n') } }
        }
        if !out.is_empty() { return Ok(out) }
        let mut lines = vec![];
        for (_, v) in self.store.list(&format!("rline:{token}:")).await? { lines.extend(crate::runner_service::feed(&v).2) }
        Ok(lines.join("\n").into_bytes())
    }

    /// A run from a computer: a workflow file's jobs, run as GitHub would run them, each on a machine as any job of
    /// this control plane's, with the working tree uploaded (`tree_part`) in place of a checkout, and what a run gives
    /// beyond GitHub's (`secrets`, `vars`). Each combination of a matrix is a job of its own; a job starts once the
    /// jobs it needs have ended, and runs if its `if` holds (by default, if they all succeeded), knowing their
    /// results and outputs (see `advance_run`). Each job's runner takes the job from here (see `runner_service`).
    /// With `--job`, that job and the jobs it needs.
    async fn create_run(&self, req: &Request, plane_url: &str, key: Option<RunKey>) -> Result<Response> {
        #[derive(Deserialize)]
        struct NewRun { workflow: String, path: String, #[serde(default)] job: Option<String>, repository: String, #[serde(default, rename = "ref")] reference: String,
            #[serde(default)] sha: String, #[serde(default)] actor: String, #[serde(default)] event_name: String, #[serde(default)] secrets: Vec<(String, String)>,
            #[serde(default)] vars: Vec<(String, String)>, #[serde(default)] tree: Option<String>, #[serde(default)] parts: u32, #[serde(default)] layered: bool }
        let r: NewRun = match serde_json::from_slice(&req.body) { Ok(r) => r, Err(e) => return Ok(Response::text(400, &format!("not a run: {e}"))) };
        let wf = match crate::workflow::parse(&r.workflow) { Ok(w) => w, Err(e) => return Ok(Response::text(400, &e)) };
        let jobs = wf.get("jobs").map(|j| j.pairs().to_vec()).unwrap_or_default();
        let names: Vec<String> = jobs.iter().filter_map(|(k, _)| k.text().map(str::to_string)).collect();
        if names.is_empty() { return Ok(Response::text(400, &format!("{} has no jobs", r.path))) }
        // Which jobs: all of them, or the one asked for and those it needs (and those they need).
        let mut wanted: Vec<String> = match &r.job {
            Some(j) if names.contains(j) => vec![j.clone()],
            Some(j) => return Ok(Response::text(400, &format!("{} has no job named {j}: {}", r.path, names.join(", ")))),
            None => names.clone(),
        };
        let mut i = 0;
        while i < wanted.len() {
            let job = wf.get("jobs").and_then(|j| j.get(&wanted[i])).cloned();
            for n in job.as_ref().map(crate::workflow::needs).unwrap_or_default() {
                if !names.contains(&n) { return Ok(Response::text(400, &format!("the job {} needs {n}, which {} does not have", wanted[i], r.path))) }
                if !wanted.contains(&n) { wanted.push(n) }
            }
            i += 1;
        }
        // Each job, each combination of its matrix: one job of the run, in the order of the file.
        let now = self.clock.now_ms();
        let mut instances = vec![];
        for name in names.iter().filter(|n| wanted.contains(n)) {
            let job = wf.get("jobs").and_then(|j| j.get(name)).cloned().unwrap_or(crate::workflow::Node::Scalar { text: String::new(), plain: true, line: 0, col: 0 });
            if job.get("uses").is_some() { return Ok(Response::text(400, &format!("the job {name} calls another workflow; a run from a computer runs jobs with steps of their own yet"))) }
            let combos = match crate::workflow::matrix(&job) { Ok(Some(c)) => c.into_iter().map(serde_json::Value::Object).collect::<Vec<_>>(), Ok(None) => vec![serde_json::Value::Null], Err(e) => return Ok(Response::text(400, &format!("the job {name}: {e}"))) };
            if combos.is_empty() { continue }
            let total = combos.len();
            for (index, matrix) in combos.into_iter().enumerate() {
                instances.push(serde_json::json!({ "key": name, "index": index, "total": total, "matrix": matrix, "needs": crate::workflow::needs(&job), "state": "pending" }));
            }
        }
        if instances.len() > 64 { return Ok(Response::text(400, &format!("{} jobs: a run from a computer runs 64 at most", instances.len()))) }
        let run_id = local_id();
        // Its jobs' cache: the repository's entries, as GitHub's jobs keep them when GitHub knows it here.
        let (ns, default_branch) = match self.repository_of(&r.repository).await { Some((id, default)) => (format!("gh{id}"), default), None => (cache::local_ns(&r.repository), "main".to_string()) };
        let run = serde_json::json!({ "at_ms": now, "workflow": r.workflow, "path": r.path, "repository": r.repository, "ref": r.reference, "sha": r.sha, "actor": r.actor,
            "event_name": r.event_name, "secrets": r.secrets, "vars": r.vars, "tree": r.tree, "parts": r.parts, "layered": r.layered, "by": key.as_ref().map(|k| k.name.clone()), "jobs": instances,
            "cache": { "ns": ns, "default_branch": default_branch } });
        put_json(self.store, &format!("run:{run_id}"), &run).await?;
        if let Err(e) = self.advance_run(plane_url, run_id).await { return Ok(Response::text(400, &e)) }
        let run = get_json::<serde_json::Value>(self.store, &format!("run:{run_id}")).await?.unwrap_or_default();
        Ok(Response::json(&serde_json::json!({ "run_id": run_id.to_string(), "jobs": run["jobs"].as_array().map(|j| j.iter().map(|j| j["name"].clone()).collect::<Vec<_>>()).unwrap_or_default() })))
    }

    /// Moves a run from a computer on: notes how its started jobs ended, then starts each job whose needs have all
    /// ended and whose `if` holds (one that does not is skipped), as long as anything changes. The run's record is
    /// written over exactly what was read of it (`Store::swap`), so that two requests that move it at once (two
    /// jobs ending together) never start a job twice: the jobs are started only once that write has been made.
    async fn advance_run(&self, plane_url: &str, run_id: u64) -> std::result::Result<(), String> {
        let key = format!("run:{run_id}");
        for _ in 0..16 {
            let Some(was) = self.store.get(&key).await? else { return Ok(()) };
            let mut run: serde_json::Value = serde_json::from_str(&was).map_err(|e| e.to_string())?;
            let wf = crate::workflow::parse(run["workflow"].as_str().unwrap_or_default())?;
            let mut changed = false;
            // How each started job ended: its runner said, or its machine never came (a start that failed).
            let jobs = run["jobs"].as_array().cloned().unwrap_or_default();
            let mut jobs = jobs;
            for j in jobs.iter_mut().filter(|j| j["state"] == "started") {
                let (Some(id), Some(token)) = (j["job_id"].as_str().and_then(|i| i.parse::<u64>().ok()), j["token"].as_str()) else { continue };
                let handed = get_json::<crate::runner_service::HandedJob>(self.store, &format!("rjob:{token}")).await?;
                let record = get_json::<Job>(self.store, &format!("job:{id}")).await?;
                let said = handed.as_ref().and_then(|h| h.result.clone());
                let over = record.as_ref().is_some_and(|r| r.ended_ms.is_some() || ["failed", "swept", "cancelled", "orphan"].contains(&r.state.as_str()));
                if said.is_none() && !over { continue }
                let result = match said.as_deref() { Some("succeeded") => "success", Some("cancelled" | "canceled") => "cancelled", Some("skipped") => "skipped", _ => "failure" };
                let outputs: serde_json::Map<String, serde_json::Value> = handed.as_ref().and_then(|h| h.outputs.clone()).and_then(|o| o.as_object().cloned()).unwrap_or_default()
                    .into_iter().map(|(k, v)| (k, v.get("value").cloned().unwrap_or(v))).collect();
                j["state"] = "done".into(); j["result"] = result.into(); j["outputs"] = outputs.into();
                changed = true;
            }
            // Each job whose needs have ended: started, or skipped.
            let mut start = vec![];
            loop {
                let mut moved = false;
                for i in 0..jobs.len() {
                    if jobs[i]["state"] != "pending" { continue }
                    let needed: Vec<String> = jobs[i]["needs"].as_array().into_iter().flatten().filter_map(|n| n.as_str().map(str::to_string)).collect();
                    let of = |n: &str| jobs.iter().filter(|j| j["key"] == n).collect::<Vec<_>>();
                    if needed.iter().any(|n| of(n).iter().any(|j| j["state"] != "done")) { continue }
                    let mut needs = serde_json::Map::new();
                    let mut status = crate::expression::Status::Success;
                    let mut all_good = true;
                    for n in &needed {
                        let results: Vec<&str> = of(n).iter().map(|j| j["result"].as_str().unwrap_or("skipped")).collect();
                        let result = if results.contains(&"failure") { "failure" } else if results.contains(&"cancelled") { "cancelled" } else if results.iter().all(|r| *r == "skipped") { "skipped" } else { "success" };
                        if result == "failure" { status = crate::expression::Status::Failure }
                        if result == "cancelled" && status == crate::expression::Status::Success { status = crate::expression::Status::Cancelled }
                        all_good &= result == "success";
                        let mut outputs = serde_json::Map::new();
                        for j in of(n) { for (k, v) in j["outputs"].as_object().into_iter().flatten() { outputs.insert(k.clone(), v.clone()); } }
                        needs.insert(n.clone(), serde_json::json!({ "result": result, "outputs": outputs }));
                    }
                    let key_name = jobs[i]["key"].as_str().unwrap_or_default().to_string();
                    let job = wf.get("jobs").and_then(|j| j.get(&key_name));
                    let values = self.run_contexts(&run, &jobs[i], &needs);
                    let c = crate::expression::Contexts { values: &values, status };
                    let runs = match job.and_then(|j| j.get("if")).and_then(|n| n.text()) {
                        Some(cond) => crate::expression::holds(cond, &c).map_err(|e| format!("the job {key_name}'s if: {e}"))?,
                        None => all_good,
                    };
                    let name = self.instance_name(job, &jobs[i], &values)?;
                    jobs[i]["name"] = name.into();
                    jobs[i]["needs_said"] = serde_json::Value::Object(needs);
                    if runs {
                        // (Reserved here, started once the record says so: see above.)
                        jobs[i]["state"] = "started".into();
                        jobs[i]["job_id"] = local_id().to_string().into();
                        jobs[i]["token"] = crate::crypto::random_token(24).into();
                        start.push(i);
                    } else {
                        jobs[i]["state"] = "done".into(); jobs[i]["result"] = "skipped".into();
                    }
                    moved = true;
                }
                if !moved { break }
                changed = true;
            }
            if !changed { return Ok(()) }
            // A run all over keeps no secret.
            if jobs.iter().all(|j| j["state"] == "done") { run["secrets"] = serde_json::json!([]) }
            let secrets: Vec<(String, String)> = serde_json::from_value(run["secrets"].clone()).unwrap_or_default();
            run["jobs"] = serde_json::Value::Array(jobs.clone());
            if !self.store.swap(&key, Some(&was), run.to_string()).await? { continue }
            for i in start { self.start_instance(plane_url, run_id, &run, &jobs[i], &secrets, &wf).await? }
            // (All over: its working tree goes now, not after a day.)
            if let Some(tree) = run["tree"].as_str().filter(|_| jobs.iter().all(|j| j["state"] == "done")) { self.forget_tree(plane_url, tree).await? }
            return Ok(())
        }
        Err("the run kept changing while it was moved on".into())
    }

    /// The contexts a job of a run from a computer is read with: `github`, `vars`, `inputs`, its `matrix`, its
    /// `strategy`, and what it `needs`.
    fn run_contexts(&self, run: &serde_json::Value, job: &serde_json::Value, needs: &serde_json::Map<String, serde_json::Value>) -> serde_json::Map<String, serde_json::Value> {
        let vars: serde_json::Map<String, serde_json::Value> = serde_json::from_value::<Vec<(String, String)>>(run["vars"].clone()).unwrap_or_default().into_iter().map(|(k, v)| (k, v.into())).collect();
        let reference = run["ref"].as_str().filter(|r| !r.is_empty()).unwrap_or("refs/heads/main");
        let mut c = serde_json::Map::new();
        c.insert("github".into(), serde_json::json!({ "event_name": run["event_name"].as_str().filter(|e| !e.is_empty()).unwrap_or("push"), "repository": run["repository"], "ref": reference, "sha": run["sha"], "actor": run["actor"] }));
        c.insert("vars".into(), vars.into());
        c.insert("inputs".into(), serde_json::json!({}));
        c.insert("matrix".into(), job["matrix"].clone());
        c.insert("strategy".into(), serde_json::json!({ "fail-fast": true, "job-index": job["index"], "job-total": job["total"], "max-parallel": job["total"] }));
        c.insert("needs".into(), serde_json::Value::Object(needs.clone()));
        c
    }

    /// A job's name as shown: its `name:` (with its expressions read), else its key, with its matrix's values.
    fn instance_name(&self, job: Option<&crate::workflow::Node>, inst: &serde_json::Value, values: &serde_json::Map<String, serde_json::Value>) -> std::result::Result<String, String> {
        let key = inst["key"].as_str().unwrap_or_default();
        let c = crate::expression::Contexts { values, status: crate::expression::Status::Success };
        if let Some(n) = job.and_then(|j| j.get("name")).and_then(|n| n.text()) { return crate::expression::interpolate(n, &c) }
        Ok(match inst["matrix"].as_object().filter(|m| !m.is_empty()) {
            Some(m) => format!("{key} ({})", m.values().map(crate::expression::text).collect::<Vec<_>>().join(", ")),
            None => key.to_string(),
        })
    }

    /// A repository's id and default branch as GitHub has them, where an App of this control plane is installed on it.
    async fn repository_of(&self, repository: &str) -> Option<(u64, String)> {
        let owner = repository.split('/').next()?;
        let now = self.clock.now_ms();
        for app in self.config.apps() {
            let Ok(found) = github::installations(self.http, app, now).await else { continue };
            let Some(i) = found.iter().find(|i| i.account.eq_ignore_ascii_case(owner)) else { continue };
            let Ok(token) = self.installation_token(app, i.id).await else { continue };
            let Ok(r) = github::repository(self.http, &app.api(), &token, repository).await else { continue };
            return Some((r["id"].as_u64()?, r["default_branch"].as_str().unwrap_or("main").to_string()))
        }
        None
    }

    /// Starts one job of a run from a computer: its message (with its matrix, strategy and needs), its record, and
    /// its machine, as any job's.
    async fn start_instance(&self, plane_url: &str, run_id: u64, run: &serde_json::Value, inst: &serde_json::Value, secrets: &[(String, String)], wf: &crate::workflow::Node) -> std::result::Result<(), String> {
        let (key_name, token) = (inst["key"].as_str().unwrap_or_default(), inst["token"].as_str().unwrap_or_default());
        let job_id: u64 = inst["job_id"].as_str().and_then(|i| i.parse().ok()).ok_or("a job without its id")?;
        let job = wf.get("jobs").and_then(|j| j.get(key_name));
        let needs = inst["needs_said"].as_object().cloned().unwrap_or_default();
        let values = self.run_contexts(run, inst, &needs);
        let c = crate::expression::Contexts { values: &values, status: crate::expression::Status::Success };
        // Its machine: the one its label says when it is this control plane's, else the default machine.
        let runs_on = job.and_then(|j| j.get("runs-on")).and_then(|n| n.text()).map(|t| crate::expression::interpolate(t, &c)).transpose()?.unwrap_or_default();
        let label = if self.config.parse(&runs_on).is_some() { runs_on } else { self.config.label.clone() };
        let now = self.clock.now_ms();
        let repository = run["repository"].as_str().unwrap_or_default().to_string();
        let reference = run["ref"].as_str().filter(|r| !r.is_empty()).unwrap_or("refs/heads/main").to_string();
        let actor = run["actor"].as_str().unwrap_or_default();
        let path = run["path"].as_str().unwrap_or_default().to_string();
        let github = serde_json::json!({ "event_name": run["event_name"].as_str().filter(|e| !e.is_empty()).unwrap_or("push"), "event": {}, "repository": repository,
            "repository_owner": repository.split('/').next().unwrap_or_default(), "ref": reference, "ref_name": reference.rsplit('/').next().unwrap_or_default(),
            "ref_type": if reference.starts_with("refs/tags/") { "tag" } else { "branch" }, "sha": run["sha"], "actor": actor, "triggering_actor": actor,
            "workflow": wf.get("name").and_then(|n| n.text()).unwrap_or(&path), "run_id": run_id.to_string(), "run_number": "1", "run_attempt": "1",
            "server_url": "https://github.com", "api_url": "https://api.github.com", "graphql_url": "https://api.github.com/graphql", "job": key_name, "retention_days": "1" });
        let service = format!("{plane_url}/runner/{token}/");
        let vars: Vec<(String, String)> = serde_json::from_value(run["vars"].clone()).unwrap_or_default();
        let display = inst["name"].as_str().map(str::to_string);
        // Its token, which its actions use for the cache as a GitHub job's own (see `cache::local_claims`).
        let who = run["by"].as_str().filter(|b| !b.is_empty() && b.bytes().all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c))).unwrap_or("computer");
        let mut claims = cache::local_claims(run["cache"]["ns"].as_str().unwrap_or_default(), who, &reference, run["cache"]["default_branch"].as_str().unwrap_or("main"), &run_id.to_string(), &job_id.to_string());
        let secs = now / 1000;
        for (k, v) in [("iss", serde_json::json!(plane_url)), ("aud", "superci-cache".into()), ("sub", format!("run:{run_id}").into()), ("iat", secs.into()), ("exp", (secs + (self.machine_minutes("aws") as u64 + 60) * 60).into())] { claims[k] = v }
        let access_token = self.identity().await?.signing.jwt(&claims)?;
        let given = crate::runner_service::Given { workflow: run["workflow"].as_str().unwrap_or_default(), path: &path, job: key_name, secrets, vars: &vars, github, service: &service,
            access_token: &access_token, ids: crate::runner_service::Ids::new(1), matrix: inst["matrix"].clone(),
            strategy: values["strategy"].clone(), needs: serde_json::Value::Object(needs), display: display.clone() };
        let message = crate::runner_service::job_message(&given)?;
        put_json(self.store, &format!("rjob:{token}"), &crate::runner_service::HandedJob { message, tree: run["tree"].as_str().map(str::to_string), parts: run["parts"].as_u64().unwrap_or(0) as u32, layered: run["layered"] == true, at_ms: now, job_id, ..Default::default() }).await?;
        let mut job = Job { job_id, run_id, repo: repository, state: "launching".into(), at_ms: now, label, name: display.unwrap_or_else(|| key_name.to_string()), workflow: path, provider: "local".into(),
            local: Some(token.to_string()), queued_ms: Some(now), by: run["by"].as_str().map(str::to_string), ..Default::default() };
        if !self.store.put_if_absent(&job_key(&job), serde_json::to_string(&job).map_err(|e| e.to_string())?).await? { return Ok(()) }
        self.indexes(&job_key(&job), &job, true, None).await?;
        self.start_soon(plane_url, &mut job).await
    }

    /// `GET /runs/<id>/artifacts`: a run from a computer's artifacts (each one's name and size), or with `name`, links
    /// to read that one's pieces, in order. Whoever may look may fetch them, as at GitHub.
    async fn run_artifacts(&self, path: &str, url: &url::Url, plane_url: &str) -> Result<Response> {
        let Some(id) = path.trim_start_matches("/runs/").trim_end_matches("/artifacts").parse::<u64>().ok() else { return Ok(nothing_here()) };
        if get_json::<serde_json::Value>(self.store, &format!("run:{id}")).await?.is_none() { return Ok(nothing_here()) }
        let run = id.to_string();
        let Some(name) = url.query_pairs().find(|(k, _)| k == "name").map(|(_, v)| v.into_owned()) else {
            let all = cache::artifacts(self.store, &run).await?;
            return Ok(Response::json(&serde_json::json!({ "artifacts": all.iter().map(|(n, e)| serde_json::json!({ "name": n, "size": e.size, "made": e.made })).collect::<Vec<_>>() })));
        };
        let Some((_, entry)) = cache::artifact(self.store, &run, &name, true).await? else { return Ok(Response::text(404, &format!("the run has no artifact {name}"))) };
        let Some(a) = self.aws_connection().await?.filter(|a| a.connected) else { return Ok(Response::text(409, "this control plane has no AWS account")) };
        let (creds, now) = (self.aws_creds(plane_url, &a).await?, self.clock.now_ms());
        let (bucket, prefix) = (cache::bucket(&self.config.plane_id, &entry.region), entry.prefix(&cache::artifact_ns(&run)));
        let pieces = entry.pieces.iter().map(|(p, size)| Ok(serde_json::json!({ "url": aws::presign("GET", &aws::s3_url(&bucket, &entry.region, &format!("{prefix}/{p}")), &creds, &entry.region, cache::LINK_SECONDS, now)?, "size": size }))).collect::<Result<Vec<_>>>()?;
        Ok(Response::json(&serde_json::json!({ "name": name, "size": entry.size, "pieces": pieces })))
    }

    /// `GET /runs/<id>/log?job=<i>&log=<n>`: a whole log the run's `i`th job's runner sent (a step's), as text.
    async fn run_log(&self, path: &str, url: &url::Url) -> Result<Response> {
        let Some(id) = path.trim_start_matches("/runs/").trim_end_matches("/log").parse::<u64>().ok() else { return Ok(nothing_here()) };
        let Some(run) = get_json::<serde_json::Value>(self.store, &format!("run:{id}")).await? else { return Ok(nothing_here()) };
        let q = |k: &str| url.query_pairs().find(|(n, _)| n == k).and_then(|(_, v)| v.parse::<u64>().ok());
        // A step still going (`step`): what its machine sent of its pages so far, whole lines only.
        let step = url.query_pairs().find(|(k, _)| k == "step").map(|(_, v)| v.into_owned());
        if let (Some(i), Some(step)) = (q("job"), step) {
            let Some(job) = run["jobs"][i as usize]["job_id"].as_str() else { return Ok(nothing_here()) };
            return Ok(match self.step_pages(job, &step).await? { Some(t) => Response::new(200, "text/plain; charset=utf-8", t.into_bytes()), None => nothing_here() })
        }
        let (Some(i), Some(n)) = (q("job"), q("log")) else { return Ok(Response::text(400, "say which job and which log")) };
        let Some(token) = run["jobs"][i as usize]["token"].as_str() else { return Ok(nothing_here()) };
        Ok(Response::new(200, "text/plain; charset=utf-8", self.log_text(token, n).await?))
    }

    /// How a run from a computer goes: each job's name, state, how it ended, its machine, and the lines its runner
    /// wrote after the count given for it in `after` (one count for each job, in order, comma between; the answer
    /// gives them back as `next`); and the run's own state and result once all its jobs are over.
    async fn run_status(&self, path: &str, url: &url::Url) -> Result<Response> {
        let Some(id) = path.trim_start_matches("/runs/").parse::<u64>().ok() else { return Ok(nothing_here()) };
        let Some(run) = get_json::<serde_json::Value>(self.store, &format!("run:{id}")).await? else { return Ok(nothing_here()) };
        let after: Vec<u64> = url.query_pairs().find(|(k, _)| k == "after").map(|(_, v)| v.split(',').map(|n| n.parse().unwrap_or(0)).collect()).unwrap_or_default();
        let mut out = vec![];
        for (i, j) in run["jobs"].as_array().into_iter().flatten().enumerate() {
            let from = after.get(i).copied().unwrap_or(0);
            let (mut lines, mut next) = (vec![], from);
            let record = match j["job_id"].as_str().and_then(|i| i.parse::<u64>().ok()) { Some(jid) => get_json::<Job>(self.store, &format!("job:{jid}")).await?, None => None };
            // Its lines since then, each post with its step and where in that step's log it begins; and its steps,
            // with the number of each one's whole log (for what the runner did not send as it went).
            let (mut posts, mut steps) = (vec![], vec![]);
            if let Some(token) = j["token"].as_str() {
                for (k, v) in self.store.list(&format!("rline:{token}:")).await? {
                    let n = k.rsplit(':').next().and_then(|n| n.parse::<u64>().ok()).unwrap_or(0);
                    if n <= from { continue }
                    let (step, start, those) = crate::runner_service::feed(&v);
                    lines.extend(those.iter().cloned());
                    posts.push(serde_json::json!({ "step": step, "start": start, "lines": those }));
                    next = next.max(n);
                }
                let handed = get_json::<crate::runner_service::HandedJob>(self.store, &format!("rjob:{token}")).await?.unwrap_or_default();
                steps = handed.steps.iter().filter(|s| s["type"] != "Job").map(|s| serde_json::json!({ "id": s["id"], "name": s["name"], "state": s["state"], "result": s["result"], "order": s["order"], "log": s["log"], "start": s["start"], "finish": s["finish"] })).collect();
            }
            out.push(serde_json::json!({ "name": j["name"].as_str().or(j["key"].as_str()), "state": record.as_ref().map(|r| r.state.clone()).unwrap_or_else(|| j["state"].as_str().unwrap_or_default().to_string()),
                "result": j["result"], "error": record.as_ref().and_then(|r| r.error.clone()), "machine": record.as_ref().and_then(|r| r.machine_type.clone()), "lines": lines, "next": next, "posts": posts, "steps": steps,
                // What its machine cost, and how long the job ran.
                "usd": record.as_ref().map(|r| job_usd(r, self.clock.now_ms())), "took_ms": record.as_ref().and_then(|r| Some(r.ended_ms.unwrap_or(self.clock.now_ms()).saturating_sub(r.started_ms?))) }));
        }
        let jobs = run["jobs"].as_array().cloned().unwrap_or_default();
        let over = jobs.iter().all(|j| j["state"] == "done");
        let result = over.then(|| if jobs.iter().any(|j| j["result"] == "failure") { "failure" } else if jobs.iter().any(|j| j["result"] == "cancelled") { "cancelled" } else { "success" });
        Ok(Response::json(&serde_json::json!({ "run_id": id.to_string(), "over": over, "result": result, "jobs": out })))
    }

    /// A call of the runner of a run from a computer, at `/runner/<token>/…`: its checkout and working tree, or
    /// one of GitHub's runner's own calls (see `runner_service::answer`). When it takes its job, the job has begun;
    /// when it says how the job ended, the job has ended (as GitHub's events say of other jobs).
    async fn for_runner(&self, req: &Request, plane_url: &str, url: &url::Url) -> Result<Response> {
        let rest = url.path().trim_start_matches("/runner/");
        let (token, sub) = rest.split_once('/').unwrap_or((rest, ""));
        if token.len() < 16 || !token.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') { return Ok(nothing_here()) }
        // A job's lines as its runner writes them: kept at once, nothing else read or written (a run that prints
        // fast sends a hundred lines at a time, one post after another, and waits for each).
        if req.method == "POST" && sub.contains("TimeLineWebConsoleLog/") { return self.lines_in(token, sub, req).await }
        let key = format!("rjob:{token}");
        let Some(before) = get_json::<crate::runner_service::HandedJob>(self.store, &key).await? else { return Ok(nothing_here()) };
        if sub == "superci/checkout.tar.gz" {
            return Ok(Response::new(200, "application/gzip", crate::runner_service::checkout_tarball(&format!("{plane_url}/runner/{token}/superci/tree"), before.parts, before.layered)))
        }
        if sub == "superci/tree" {
            let part = url.query_pairs().find(|(k, _)| k == "part").and_then(|(_, v)| v.parse::<u32>().ok()).unwrap_or(0);
            let Some(tree) = &before.tree else { return Ok(nothing_here()) };
            // (In the bucket: a link there, for a while.)
            if let Some(kept) = self.store.get(&format!("rtree-s3:{tree}:{part:05}")).await? {
                let now = self.clock.now_ms();
                let (region, object) = tree_place(kept.trim_matches('"'), tree, part);
                let Some(a) = self.aws_connection().await?.filter(|a| a.connected) else { return Ok(Response::text(409, "this control plane has no AWS account")) };
                let creds = self.aws_creds(plane_url, &a).await?;
                let link = aws::presign("GET", &aws::s3_url(&cache::bucket(&self.config.plane_id, &region), &region, &object), &creds, &region, cache::LINK_SECONDS, now)?;
                return Ok(Response::new(302, "text/plain", Vec::new()).with_header("location", &link))
            }
            let mut bytes = vec![];
            for (_, piece) in self.store.list(&format!("rtree:{tree}:{part:05}:")).await? {
                use base64::Engine;
                bytes.extend(base64::engine::general_purpose::STANDARD.decode(piece.trim_matches('"')).map_err(|e| e.to_string())?);
            }
            return Ok(Response::new(200, "application/octet-stream", bytes))
        }
        // A log's content: into the bucket where there is one (a log of many MB never goes through the store).
        if let Some(n) = Some(sub).filter(|s| s.contains("Logfiles/")).and_then(|s| s.split('?').next()).and_then(|s| s.trim_end_matches('/').rsplit('/').next()).and_then(|n| n.parse::<u64>().ok()).filter(|_| ["POST", "PUT"].contains(&req.method.as_str())) {
            let now = self.clock.now_ms();
            if let Some((region, creds)) = self.tree_bucket(plane_url, now).await? {
                let seq = self.store.list(&format!("rlog:{token}:{n:04}:")).await?.len();
                let object = format!("runs/{token}/log-{n:04}-{seq:04}");
                let put = aws::s3(self.http, "PUT", &aws::s3_url(&cache::bucket(&self.config.plane_id, &region), &region, &object), &region, &creds, req.body.clone(), &[], now).await?;
                if put.status < 300 {
                    self.store.put_until(&format!("rlog:{token}:{n:04}:{seq:04}"), format!("s3:{region}"), now + crate::runner_service::KEPT_MS).await?;
                    return Ok(Response::new(200, "application/json", ""))
                }
            }
        }
        let full = url[url::Position::BeforePath..].to_string();
        let path = full.trim_start_matches(&format!("/runner/{token}")).to_string();
        let res = crate::runner_service::answer(self.store, token, req, &path).await?;
        let after = get_json::<crate::runner_service::HandedJob>(self.store, &key).await?.unwrap_or_default();
        if !before.taken && after.taken { self.started(plane_url, after.job_id, Some(&after.runner)).await? }
        if before.result.is_none() {
            if let Some(result) = &after.result {
                let conclusion = match result.as_str() { "succeeded" => "success", "failed" => "failure", "canceled" | "cancelled" => "cancelled", other => other }.to_string();
                if let Some(job) = get_json::<Job>(self.store, &format!("job:{}", after.job_id)).await? {
                    let ev = github::JobEvent { action: "completed".into(), job_id: job.job_id, run_id: job.run_id, attempt: 1, created_ms: job.queued_ms, repo: job.repo.clone(), installation_id: 0,
                        runner_name: Some(after.runner.clone()), conclusion: Some(conclusion), name: job.name.clone(), workflow: job.workflow.clone(), label: job.label.clone(), spec: Ok(Spec::default()), public: false, repo_id: None, branch: None, default_branch: None };
                    self.completed(plane_url, &ev).await?;
                    // (The run it is a job of moves on: the jobs that needed it may start.)
                    if let Err(e) = self.advance_run(plane_url, job.run_id).await { let _ = self.store.put_until(&format!("run-error:{}", job.run_id), e, self.clock.now_ms() + 2 * 86_400_000).await; }
                }
                // (Its secrets go with the job: nothing of them is kept once it has ended.)
                let mut done = after.clone();
                if let Some(vars) = done.message.get_mut("variables").and_then(|v| v.as_object_mut()) { vars.retain(|_, v| v["isSecret"] != true) }
                done.message["mask"] = serde_json::json!([]);
                put_json(self.store, &key, &done).await?;
            }
        }
        if res.header("x-superci-hold").is_some() { self.timer.pause(crate::runner_service::HOLD_MS).await }
        Ok(res)
    }

    /// A finished job ends the machine that ran it, found by the runner's name (GitHub may hand a job to any idle runner
    /// with the label). A job cancelled before any runner took it leaves its own machine idle: marked for the sweep.
    async fn completed(&self, plane_url: &str, ev: &github::JobEvent) -> Result<()> {
        let (job_id, runner) = (ev.job_id, ev.runner_name.as_deref());
        if let Some(name) = runner.filter(|r| failing_runner(r)) { return self.failed_by_runner(name, job_id).await }
        let mut on_anothers = None;
        if let Some(mut j) = self.by_runner(runner).await? {
            if j.job_id != job_id { on_anothers = Some(j.for_job.unwrap_or(j.job_id)) }
            self.stop_machine(plane_url, &j).await;
            if let Some(name) = runner { let _ = self.store.delete(&format!("runner:{name}")).await; }
            j.state = "done".into();
            if j.job_id == job_id { j.began = true; j.conclusion = ev.conclusion.clone() }
            j.ended_ms.get_or_insert(self.clock.now_ms());
            // AWS took its spot machine back: the job it was running is run again once its run has finished (GitHub
            // runs nothing again before), on an on-demand machine. Not one that passed anyway (the notice came as
            // it ended).
            if j.interrupted && j.rerun.is_none() && !matches!(ev.conclusion.as_deref(), Some("success" | "skipped" | "neutral")) {
                j.state = "failed".into();
                j.error = Some("AWS took back its spot machine; the job is run again, off spot machines once its run has finished".into());
                j.rerun = Some(Rerun { job_id, run_id: ev.run_id, repo: ev.repo.clone(), name: ev.name.clone(), state: "pending".into(), at_ms: self.clock.now_ms() });
                self.timer.wake_in(60_000).await?;
            }
            self.settle(plane_url, &mut j).await;
            self.keep(&format!("job:{}", j.job_id), &j).await?;
            if j.job_id == job_id { return Ok(()); }
        }
        if let Some(mut own) = get_json::<Job>(self.store, &format!("job:{job_id}")).await? {
            // How it ended is the job's own, whichever machine ran it.
            let said_before = own.conclusion.clone();
            own.conclusion = ev.conclusion.clone().or(said_before.clone());
            // Cancelled while waiting for room: nothing was started. (Noted too, for a sweep that listed it a moment
            // ago as waiting and is about to start it from that: it stops there, see `gone_key`.)
            if own.state == "waiting" {
                if runner.is_none() && on_anothers.is_none() { let _ = self.store.put(&gone_key(&own), self.clock.now_ms().to_string()).await; }
                own.state = "cancelled".into();
                own.began = true;
                own.ended_ms = Some(self.clock.now_ms());
                self.keep(&format!("job:{job_id}"), &own).await?;
                return Ok(());
            }
            // Ended before any runner took it (cancelled), with its machine still being asked for: noted for the start
            // that is under way, which stops there or ends the machine it has just been given (see `gone_key`).
            if own.state == "launching" && runner.is_none() && on_anothers.is_none() { let _ = self.store.put(&gone_key(&own), self.clock.now_ms().to_string()).await; }
            if own.state == "launched" && !own.seen_in_progress {
                // It ran on another job's runner: that job, still queued, is what its own machine is for now. It
                // ran nowhere (cancelled before a runner took it): its machine is ended now if its runner can be
                // taken back from GitHub (which refuses while a runner is running a job), else by the sweep.
                // (The same event delivered again finds the runner's name forgotten: what was noted stays.)
                match on_anothers {
                    Some(waiting) => { own.over = true; own.for_job.get_or_insert(waiting); }
                    None if own.over => {}
                    None if self.take_back(plane_url, &mut own).await => {}
                    None => own.state = "orphan".into(),
                }
                own.began = true;
                self.keep(&format!("job:{job_id}"), &own).await?;
                self.timer.wake_in(60_000).await?;
            } else if !own.began {
                // Its record holds a machine busy with another job's work: its own job is over all the same.
                own.began = true;
                self.keep(&format!("job:{job_id}"), &own).await?;
            } else if own.conclusion != said_before {
                self.keep(&format!("job:{job_id}"), &own).await?;
            }
        } else if runner.is_none() && on_anothers.is_none() {
            // Ended before this control plane knew of it: GitHub sends a job's events side by side, and the end of
            // one cancelled as it was queued (a newer push took its place) can arrive first. Noted, so that its
            // queued event, a moment behind, starts nothing (see `gone_key`).
            let _ = self.store.put(&gone_key(&Job { job_id, ..Default::default() }), self.clock.now_ms().to_string()).await;
        }
        Ok(())
    }

    /// Every minute while machines are up: ends machines whose runner never got a job (10 min), orphans, and machines
    /// past the job time bound (they also power themselves off).
    pub async fn alarm(&self) -> Result<()> {
        let now = self.clock.now_ms();
        // The store's own upkeep: what an earlier layout left is moved, a little each minute; what has passed its end
        // goes, once an hour (where the store does not remove it by itself).
        let _ = self.store.migrate(500).await;
        if self.store.put_if_absent(&format!("upkeep:{}", now / 3_600_000), now.to_string()).await.unwrap_or(false) {
            let _ = self.store.put_until(&format!("upkeep:{}", now / 3_600_000), now.to_string(), now + 2 * 3_600_000).await;
            let _ = self.store.expire(now).await;
        }
        let plane_url = get_json::<String>(self.store, "plane_url").await?.unwrap_or_default();
        let (mut active, mut waiting) = (0, 0);
        // The jobs still cared for, once the indexes are built from the jobs kept before them (a batch at a time).
        let _ = self.reindex(150).await;
        let kept: Vec<(String, String)> = self.live_jobs().await?.into_iter().map(|(k, j)| (k, serde_json::to_string(&j).unwrap_or_default())).collect();
        // GitHub's jobs that still wait as far as it has said (with the runners that can take them), and the jobs
        // that machines still coming are for.
        let (mut queued, mut aimed): (Vec<(String, u64, String)>, Vec<u64>) = (vec![], vec![]);
        for (_, v) in &kept {
            let Ok(j) = serde_json::from_str::<Job>(v) else { continue };
            if !j.provider.is_empty() { continue }
            if ["launching", "launched"].contains(&j.state.as_str()) && !j.seen_in_progress { aimed.push(j.for_job.unwrap_or(j.job_id)) }
            let waits = !(j.began || j.over || j.failed_fast) && j.fail_pending.is_none() && j.rerun.is_none() && now.saturating_sub(j.at_ms) < 86_400_000;
            if waits && ["launching", "launched", "running", "done"].contains(&j.state.as_str()) { queued.push((self.runners_of(&j), j.job_id, j.repo.clone())) }
        }
        for (key, v) in kept {
            let Ok(mut j) = serde_json::from_str::<Job>(&v) else { continue };
            // Nothing more to do for it (time has passed since it was written): no longer among the live ones.
            if self.indexed().await && !needs_care(&j, now) { self.store.delete(&format!("live:{}", key.trim_start_matches("job:"))).await?; continue }
            // Waiting for room: try again (GitHub keeps a job queued for up to a day). As it stands now, not as it
            // was listed a moment ago: it may have been cancelled, or started by another request, since.
            if j.state == "waiting" {
                match get_json::<Job>(self.store, &key).await? { Some(fresh) if fresh.state == "waiting" => j = fresh, _ => continue }
                if now.saturating_sub(j.at_ms) > 24 * 3_600_000 { j.state = "failed".into(); j.error = Some("waited a day for room".into()); self.keep(&key, &j).await?; continue }
                self.start(&plane_url, &mut j).await?;
                if j.state == "waiting" { waiting += 1 }
                continue;
            }
            // A start cut off midway (the runtime stopped while it waited on a cloud): placed again, twice at most.
            if j.state == "launching" {
                if now.saturating_sub(j.launched_ms.unwrap_or(j.at_ms)) < 120_000 { active += 1; continue }
                self.stop_machine(&plane_url, &j).await;
                if j.retries >= 2 {
                    j.state = "failed".into();
                    j.error = Some("its machine's start was cut off, three times".into());
                    j.ended_ms.get_or_insert(now);
                    self.keep(&key, &j).await?;
                } else {
                    j.retries += 1;
                    j.launched_ms = Some(now);
                    self.again(&plane_url, &key, &mut j, "its machine's start was cut off; trying again").await?;
                    active += 1;
                }
                continue;
            }
            // A job whose spot machine AWS took back: run again once its run has finished.
            if j.rerun.as_ref().is_some_and(|r| r.state == "pending") {
                if self.run_again(&mut j, now).await { self.keep(&key, &j).await? } else { active += 1 }
                continue;
            }
            // A failed job GitHub still has queued, its failing runner not started yet: tried again, for half an hour.
            if let (Some(why), false) = (j.fail_pending.clone(), j.provider == "gitlab") {
                if now.saturating_sub(j.ended_ms.unwrap_or(j.at_ms)) > 30 * 60_000 { j.fail_pending = None; self.keep(&key, &j).await?; continue }
                let queued = match (self.app_of(&j), self.installation_token_for(&j).await) {
                    (Some(app), Some(t)) => github::job_status(self.http, &app.api(), &t, &j.repo, j.job_id).await.map(|s| s == "queued").unwrap_or(true),
                    _ => false,
                };
                if queued { self.fail_fast(&plane_url, &mut j, &why).await; active += 1 } else { j.fail_pending = None; self.keep(&key, &j).await? }
                continue;
            }
            // An AWS machine that ended a while ago: what it sent, once CloudWatch has counted it, added to its cost.
            if j.cloud == "aws" && j.cost_from.is_some() && j.sent_gb.is_none() && j.ended_ms.is_some_and(|e| now > e + 12 * 60_000 && now < e + 2 * 86_400_000) {
                if self.settle_sent(&plane_url, &mut j).await { self.keep(&key, &j).await?; }
                continue;
            }
            if !["launched", "running", "orphan"].contains(&j.state.as_str()) { continue; }
            // A machine that never began its job (a container that did not start, a spot machine that did not boot):
            // placed again, twice at most, then the job fails with why (GitHub would otherwise keep it queued for a day).
            if j.state == "launched" && !j.seen_in_progress {
                let age = now.saturating_sub(j.launched_ms.unwrap_or(j.at_ms));
                let (wait, up_by) = if j.cloud == "aws" { (8 * 60_000, 90_000) } else { (3 * 60_000, 60_000) };
                let late = age > wait;
                // Once it has had time to come up and has begun nothing, GitHub is asked at every sweep whether the
                // job it is for still waits (one request): a machine nobody waits for is ended then, not minutes later.
                // (At once, when its job is known to have ended before any runner took it and the start missed the note.)
                let told_gone = self.store.get(&gone_key(&j)).await.ok().flatten().is_some();
                // (Its runner is taken back from GitHub, which refuses while it runs a job, and the machine ended: the
                // job is cancelled, as when the note is seen in time. Refused: as any machine nobody waits for, below.)
                if told_gone && j.for_job.is_none() && self.take_back(&plane_url, &mut j).await {
                    let _ = self.store.delete(&gone_key(&j)).await;
                    j.began = true;
                    self.keep(&key, &j).await?;
                    continue;
                }
                if late || told_gone || (age > up_by && j.provider.is_empty()) {
                    let place = cloud_name(&j.cloud).to_string();
                    // The job its machine is for: its own, or the one whose runner took its own (still queued, then, with
                    // no machine coming but this one).
                    let wanted = j.for_job.unwrap_or(j.job_id);
                    // Its own job ran on another job's runner, or is over: nothing of it failed here.
                    let elsewhere = j.over || j.for_job.is_some() || j.began;
                    // Only while GitHub still has the job queued (another runner may have taken it, or it was
                    // cancelled, and the event never came).
                    let (mut gone, mut on_runner) = ((j.failed_fast || j.over) && j.for_job.is_none(), None);
                    if !gone && j.provider == "gitlab" {
                        // GitLab has it on this machine's runner (the events that said so never came): as if told.
                        if self.gitlab_settle(&key, &mut j, now).await { if j.state == "running" { active += 1 } continue }
                        gone = match (self.gitlab_of(&j), j.project_id) { (Some(gl), Some(p)) => gitlab::job(self.http, gl, p, wanted).await.is_ok_and(|(_, s)| s != "pending"), _ => false };
                    } else if !gone {
                        if let (Some(app), Some(t)) = (self.app_of(&j), self.installation_token_for(&j).await) {
                            if let Ok((status, runner)) = github::job_state(self.http, &app.api(), &t, &j.repo, wanted).await { (gone, on_runner) = (status != "queued", runner) }
                        }
                    }
                    if gone && j.provider.is_empty() {
                        // GitHub has that job on this very machine (the event that said so never came): begun, as if told.
                        if on_runner.is_some() && on_runner == j.runner {
                            self.started(&plane_url, wanted, j.runner.as_deref()).await?;
                            active += 1;
                            continue;
                        }
                        // Its runner is busy all the same (what said so was lost): the machine is running a job.
                        let busy = match (self.app_of(&j), self.installation_token_for(&j).await, j.runner_id) {
                            (Some(app), Some(t), Some(id)) => github::runner_busy(self.http, &app.api(), &t, &Self::owner_of(app), &j.repo, id).await,
                            _ => Ok(false),
                        };
                        if busy.is_err() && !late { active += 1; continue }
                        if busy == Ok(true) {
                            (j.seen_in_progress, j.for_job) = (true, None);
                            j.state = "running".into();
                            j.started_ms.get_or_insert(now);
                            self.keep(&key, &j).await?;
                            active += 1;
                            continue;
                        }
                        // The job it was for waits no more. Another that still waits with no machine coming for it
                        // (GitHub gives a job to any runner with its label, and three jobs can pass theirs around)
                        // gets this one.
                        queued.retain(|(_, id, _)| *id != wanted);
                        if wanted == j.job_id { j.began = true }
                        let mine = self.runners_of(&j);
                        // The newest first, and GitHub is asked of each (three at most): what was told can be stale.
                        let mut others: Vec<(u64, String)> = queued.iter().filter(|(runners, id, _)| *runners == mine && !aimed.contains(id)).map(|(_, id, repo)| (*id, repo.clone())).collect();
                        others.sort_unstable_by(|a, b| b.0.cmp(&a.0));
                        let mut next = None;
                        for (id, repo) in others.into_iter().take(3) {
                            let (Some(app), Some(t)) = (self.app_of(&j), self.installation_token_for(&j).await) else { break };
                            match github::job_status(self.http, &app.api(), &t, &repo, id).await {
                                Ok(status) if status == "queued" => { next = Some(id); break }
                                Ok(_) => {
                                    queued.retain(|(_, q, _)| *q != id);
                                    if let Some(mut other) = get_json::<Job>(self.store, &format!("job:{id}")).await? { other.began = true; self.keep(&format!("job:{id}"), &other).await? }
                                }
                                Err(_) => {}
                            }
                        }
                        if let Some(next) = next {
                            aimed.push(next);
                            j.for_job = Some(next).filter(|n| *n != j.job_id);
                            gone = false;
                            if !late {
                                self.keep(&key, &j).await?;
                                active += 1;
                                continue;
                            }
                        }
                    }
                    if gone {
                        self.stop_machine(&plane_url, &j).await;
                        self.withdraw_runner(&j).await;
                        // Else its machine began nothing and its job waits no more (taken elsewhere or cancelled, untold).
                        j.state = if elsewhere { "done" } else { "swept" }.into();
                        j.for_job = None;
                        j.ended_ms.get_or_insert(now);
                        self.settle(&plane_url, &mut j).await;
                        self.keep(&key, &j).await?;
                        continue;
                    }
                    if late {
                        self.stop_machine(&plane_url, &j).await;
                        if j.retries >= 2 {
                            self.withdraw_runner(&j).await;
                            j.state = "failed".into();
                            j.error = Some(format!("{place} did not start a machine for it, three times"));
                            j.ended_ms.get_or_insert(now);
                            self.keep(&key, &j).await?;
                        } else {
                            j.retries += 1;
                            self.again(&plane_url, &key, &mut j, &format!("{place} did not start its machine; trying again")).await?;
                            if j.state == "waiting" { waiting += 1 } else { active += 1 }
                        }
                        continue;
                    }
                }
            }
            // Its machine's age (a job may have waited long for room before it got one).
            let age = now.saturating_sub(j.launched_ms.unwrap_or(j.at_ms));
            let stale = j.state == "orphan" || (!j.seen_in_progress && age > 10 * 60_000) || age > (self.machine_minutes(&j.cloud) as u64 + 5) * 60_000;
            if !stale { active += 1; continue; }
            self.stop_machine(&plane_url, &j).await;
            self.withdraw_runner(&j).await;
            j.state = "swept".into();
            j.ended_ms.get_or_insert(now);
            self.settle(&plane_url, &mut j).await;
            self.keep(&key, &j).await?;
        }
        self.sweep_gitlab_runners(now).await?;
        // Notes that only matter for a while: spot interruptions (counted for the pause), jobs asked to run again and
        // not arrived.
        for (prefix, keep) in [("spot:interrupted:", SPOT_PAUSE.1 + SPOT_PAUSE.2), ("rerun:", 86_400_000), ("rerun-asked:", 7 * 86_400_000), ("gone:", 3_600_000)] {
            for (k, v) in self.store.list(prefix).await? { if v.trim_matches('"').parse::<u64>().is_ok_and(|at| now.saturating_sub(at) > keep) { self.store.delete(&k).await? } }
        }
        // A launch template left behind by a request for many spot pools (it could not be removed at the time):
        // removed now; given up on after a day.
        for (k, at) in self.store.list("aws:template:").await? {
            let mut parts = k.trim_start_matches("aws:template:").splitn(2, ':');
            let (Some(region), Some(name)) = (parts.next(), parts.next()) else { continue };
            // (Not one of a moment ago: its removal may be on its way.)
            if at.parse::<u64>().is_ok_and(|at| now.saturating_sub(at) < 30_000) { continue }
            let removed = match self.aws_connection().await { Ok(Some(a)) => match self.aws_creds(&plane_url, &a).await { Ok(c) => aws::delete_template(self.http, region, &c, name, now).await, Err(e) => Err(e) }, _ => Err("no AWS".into()) };
            if removed.is_ok() || at.parse::<u64>().is_ok_and(|at| now.saturating_sub(at) > 86_400_000) { self.store.delete(&k).await? }
        }
        // Runs from a computer with jobs still going: moved on (a job whose machine never came has ended).
        for (k, v) in self.store.list("run:").await? {
            let run: serde_json::Value = serde_json::from_str(&v).unwrap_or_default();
            if run["jobs"].as_array().is_some_and(|j| j.iter().any(|j| j["state"] != "done")) {
                if let Ok(id) = k.trim_start_matches("run:").parse::<u64>() { let _ = self.advance_run(&plane_url, id).await; }
            }
        }
        // What a run from a computer kept (its jobs' messages, lines, logs, working tree): gone after a day.
        for (k, v) in self.store.list("run:").await? {
            let run: serde_json::Value = serde_json::from_str(&v).unwrap_or_default();
            if run["at_ms"].as_u64().is_some_and(|at| now.saturating_sub(at) < 86_400_000) { continue }
            for token in run["jobs"].as_array().into_iter().flatten().filter_map(|j| j["token"].as_str()) {
                for prefix in [format!("rline:{token}:"), format!("rlog:{token}:")] {
                    for (k, v) in self.store.list(&prefix).await? {
                        // (A log kept in the bucket goes from there too.)
                        if let Some(region) = v.trim_matches('"').strip_prefix("s3:") {
                            if let Some((_, creds)) = self.tree_bucket(&plane_url, now).await.ok().flatten() {
                                let (n, seq) = { let mut p = k.rsplit(':'); let seq = p.next().unwrap_or_default().to_string(); (p.next().unwrap_or_default().to_string(), seq) };
                                let _ = aws::s3(self.http, "DELETE", &aws::s3_url(&cache::bucket(&self.config.plane_id, region), region, &format!("runs/{token}/log-{n}-{seq}")), region, &creds, vec![], &[], now).await;
                            }
                        }
                        self.store.delete(&k).await?
                    }
                }
                self.store.delete(&format!("rjob:{token}")).await?;
            }
            self.store.delete(&k).await?;
        }
        for (k, at) in self.store.list("rtree-at:").await? {
            if at.trim_matches('"').parse::<u64>().is_ok_and(|at| now.saturating_sub(at) < 86_400_000) { continue }
            self.forget_tree(&plane_url, k.trim_start_matches("rtree-at:")).await?;
        }
        // The cache: what goes (over a repository's limit, unused for a week, never finished), once an hour.
        if let Err(e) = self.sweep_cache(&plane_url, now).await { let _ = self.store.put("cache-sweep-error", e).await; }
        self.redeliver_failed().await;
        self.untold_gitlab(&plane_url, now).await;
        let moved_lately = self.untold(&plane_url, now).await;
        // Every 20 seconds while jobs wait for room; every minute while machines are up, or just after a move here;
        // every five otherwise (redeliveries, jobs not told of).
        self.timer.wake_in(if waiting > 0 { 20_000 } else if active > 0 || moved_lately { 60_000 } else { 300_000 }).await?;
        Ok(())
    }

    /// Jobs that wait for a runner of this control plane and that GitHub never told it of: found by asking, and given
    /// a machine as if the event had come. GitHub sends each event once; one it loses (seen when its App's address
    /// changes in a move) is in no list of failed deliveries, so nothing else would ever start that job. Asked for
    /// the repositories that had a job here in the last day (ten at most) and, in turns, for those of the last two
    /// weeks (five at a time), every five minutes; in the quarter of an
    /// hour after a move here, at every sweep, and also for runs of which another job has started. Whether a move
    /// was that lately.
    async fn untold(&self, plane_url: &str, now: u64) -> bool {
        let moved_lately = get_json::<u64>(self.store, "claimed_at").await.ok().flatten().is_some_and(|at| now.saturating_sub(at) < 15 * 60_000);
        if self.config.app.is_none() { return moved_lately }
        if !moved_lately && get_json::<u64>(self.store, "untold_at").await.ok().flatten().is_some_and(|at| now.saturating_sub(at) < 5 * 60_000 - 5_000) { return false }
        let _ = put_json(self.store, "untold_at", &now).await;
        let kept = if self.indexed().await { self.store.list("repo:").await } else { self.store.list("job:").await };
        let Ok(kept) = kept else { return moved_lately };
        let mut jobs: Vec<Job> = kept.into_iter().filter_map(|(_, v)| serde_json::from_str::<Job>(&v).ok()).filter(|j| j.provider.is_empty() && now.saturating_sub(j.at_ms) < 14 * 86_400_000).collect();
        jobs.sort_by(|a, b| b.at_ms.cmp(&a.at_ms));
        // The repositories that had a job in the last day (ten at most), and five of the quieter ones of the last
        // two weeks, a different five each time: the first push after a weekend must not rest on one event alone.
        let (mut repos, mut quiet): (Vec<&Job>, Vec<&Job>) = (vec![], vec![]);
        for j in &jobs {
            if repos.iter().chain(&quiet).any(|r| r.repo == j.repo) { continue }
            if now.saturating_sub(j.at_ms) < 86_400_000 && repos.len() < 10 { repos.push(j) } else { quiet.push(j) }
        }
        let turn = (now / 300_000) as usize;
        for i in 0..quiet.len().min(5) { repos.push(quiet[(turn * 5 + i) % quiet.len()]) }
        for known in repos {
            let (Some(app), Some(token)) = (self.app_of(known), self.installation_token_for(known).await) else { continue };
            let Ok(waiting) = github::waiting_jobs(self.http, &app.api(), &token, &known.repo, moved_lately).await else { continue };
            for (job, private) in waiting {
                // Told of already, or made a moment ago (its event may still be on its way).
                let Some(id) = job["id"].as_u64() else { continue };
                if matches!(self.store.get(&format!("job:{id}")).await, Ok(Some(_)) | Err(_)) { continue }
                if job["created_at"].as_str().and_then(aws::parse_iso_ms).is_none_or(|at| now.saturating_sub(at) < 45_000) { continue }
                let event = serde_json::json!({ "action": "queued", "workflow_job": job, "repository": { "full_name": known.repo, "private": private }, "installation": { "id": known.installation_id } });
                if let Some(ev) = github::our_job_among(&event, &self.config.labels(), &app.owner) { let _ = self.launch(plane_url, &ev, app).await; }
            }
        }
        moved_lately
    }

    /// GitLab's jobs that wait for a runner of this control plane and that GitLab never told it of (a job event GitLab
    /// did not send, or that did not arrive): found by asking each project whose jobs come here for its pending jobs,
    /// every few minutes, and given a machine as if the event had come. Which projects those are is asked less often.
    async fn untold_gitlab(&self, plane_url: &str, now: u64) {
        if self.config.gitlabs().next().is_none() { return }
        let asked = self.store.get("gitlab:untold").await.ok().flatten().and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
        if now.saturating_sub(asked) < 180_000 { return }
        let _ = self.store.put("gitlab:untold", now.to_string()).await;
        for gl in self.config.gitlabs() {
            let known = format!("gitlab:enabled:{}", gl.id);
            let kept = get_json::<(Vec<(u64, String)>, u64)>(self.store, &known).await.ok().flatten();
            let projects = match kept {
                Some((p, at)) if now.saturating_sub(at) < 15 * 60_000 => p,
                _ => {
                    let Ok(all) = gitlab::projects(self.http, gl, &format!("{plane_url}/gitlab/webhook")).await else { continue };
                    let on: Vec<(u64, String)> = all.into_iter().filter(|p| p.enabled).map(|p| (p.id, p.path)).collect();
                    let _ = put_json(self.store, &known, &(on.clone(), now)).await;
                    on
                }
            };
            // The jobs it has machines for, as GitLab has them now (their events may not have come either).
            for (key, mut j) in self.live_jobs().await.unwrap_or_default() {
                if j.provider != "gitlab" || j.gitlab != gl.id || !active(&j) { continue }
                let _ = self.gitlab_settle(&key, &mut j, now).await;
            }
            for (project, path) in projects {
                let Ok(waiting) = gitlab::pending_jobs(self.http, gl, project).await else { continue };
                for job in waiting {
                    let Some(id) = job["id"].as_u64() else { continue };
                    if matches!(self.store.get(&format!("job:gl{}", gitlab::local(&gl.id, id))).await, Ok(Some(_)) | Err(_)) { continue }
                    // Made a moment ago: its event may still be on its way.
                    if job["created_at"].as_str().and_then(aws::parse_iso_ms).is_none_or(|at| now.saturating_sub(at) < 45_000) { continue }
                    let ev = gitlab::JobEvent { job_id: id, status: "pending".into(), project_id: project, project: path.clone(), pipeline_id: job["pipeline"]["id"].as_u64().unwrap_or(0),
                        name: job["name"].as_str().unwrap_or_default().to_string(), stage: job["stage"].as_str().unwrap_or_default().to_string(), runner_id: None,
                        git_ref: job["ref"].as_str().map(str::to_string), tag: job["tag"] == true, default_branch: None };
                    let _ = self.gitlab_job_event(gl, ev, plane_url).await;
                }
            }
        }
    }

    /// A GitLab job its machine's runner took, as GitLab has it now: begun (and what it runs for, kept with its runner
    /// for the cache there), or over. Whether it changed anything (GitLab's own events would have said so).
    async fn gitlab_settle(&self, key: &str, j: &mut Job, now: u64) -> bool {
        let (Some(gl), Some(project), Some(runner)) = (self.gitlab_of(j), j.project_id, j.runner_id) else { return false };
        let Ok(v) = gitlab::job_json(self.http, gl, project, j.job_id).await else { return false };
        if v["runner"]["id"].as_u64() != Some(runner) { return false }
        let status = v["status"].as_str().unwrap_or_default();
        let mut changed = false;
        // (Its times as GitLab kept them: when it began and ended, not when this was asked or a sweep first saw it.)
        let at = |k: &str| v[k].as_str().and_then(aws::parse_iso_ms);
        if let Some(t) = at("started_at") { changed |= j.started_ms.replace(t) != Some(t) }
        if !j.seen_in_progress && status != "pending" {
            (j.seen_in_progress, j.state) = (true, "running".into());
            j.started_ms.get_or_insert(at("started_at").unwrap_or(now));
            let at = format!("glrunner:{}", gitlab::local(&gl.id, runner));
            if let Ok(Some(mut r)) = get_json::<serde_json::Value>(self.store, &at).await {
                if r["running"].is_null() {
                    r["running"] = serde_json::json!({ "job": j.job_id, "project_id": project, "project": j.repo, "ref": v["ref"], "tag": v["tag"], "default_branch": null });
                    let _ = put_json(self.store, &at, &r).await;
                }
            }
            changed = true;
        }
        if let Some(state) = match status { "success" | "failed" => Some("done"), "canceled" => Some("cancelled"), _ => None } {
            j.state = state.into();
            j.conclusion = Some(gitlab_conclusion(status).into());
            j.ended_ms = at("finished_at").or(j.ended_ms).or(Some(now));
            if gitlab::delete_runner(self.http, gl, runner).await.is_ok() { let _ = self.store.delete(&format!("glrunner:{}", gitlab::local(&gl.id, runner))).await; }
            changed = true;
        }
        if changed { let _ = self.keep(key, &*j).await; }
        changed
    }

    /// A webhook that failed while the runtime restarted (a secret changed, a new version) would leave its job waiting
    /// forever: GitHub does not retry. So failed deliveries from the last 30 minutes are asked for again, once each.
    async fn redeliver_failed(&self) {
        let now = self.clock.now_ms();
        for app in self.config.apps() {
            let Ok(failed) = github::failed_deliveries(self.http, app, now.saturating_sub(30 * 60_000), now).await else { continue };
            for (id, guid) in failed {
                let key = format!("redelivered:{guid}");
                if matches!(self.store.put_if_absent(&key, now.to_string()).await, Ok(true)) { let _ = self.store.put_until(&key, now.to_string(), now + 86_400_000).await; let _ = github::redeliver(self.http, app, id, now).await; }
            }
        }
    }
}

/// The cache (see `cache`): its routes for machines, its sweep, and `superci-cache` on each machine.
impl<'a> ControlPlane<'a> {
    /// Whether a region's bucket is set up and has this version's `superci-cache` for an architecture (looked at every
    /// ten minutes; the dashboard makes the bucket and puts the program there).
    async fn cache_ready(&self, region: &str, arch: &str, creds: &Credentials, now: u64) -> bool {
        let at = format!("{region} {arch}");
        if let Some((ready, _)) = self.cache.cache_ready.borrow().get(&at).filter(|(_, until)| *until > now) { return *ready }
        let url = aws::s3_url(&cache::bucket(&self.config.plane_id, region), region, &cache::program_key(arch));
        let ready = matches!(aws::s3(self.http, "HEAD", &url, region, creds, vec![], &[], now).await, Ok(r) if r.status == 200);
        self.cache.cache_ready.borrow_mut().insert(at, (ready, now + 600_000));
        ready
    }

    /// A machine's start-up script with `superci-cache` in it, when its region's bucket is ready, with the machine's
    /// own link to this control plane (until its longest job ends; its `program` sends it on to the bucket).
    async fn with_cache(&self, user_data: String, region: &str, os: &str, arch: &str, creds: &Credentials, plane_url: &str, runner: &str, now: u64) -> String {
        // Which build of the program: by architecture, and Windows' of its own.
        let arch = match (os, arch) { ("windows", _) => "x86_64-windows", (_, "arm64") => "aarch64", _ => "x86_64" };
        if !["linux", "windows"].contains(&os) { return user_data }
        // (Turned off in the dashboard: jobs use GitHub's cache.)
        if self.config.routing.cache.off || !plane_url.starts_with("https://") || !self.cache_ready(region, arch, creds, now).await { return user_data }
        let token = crate::crypto::random_token(18);
        let until = now + (self.machine_minutes("aws") as u64 + 30) * 60_000;
        if cache::add_machine(self.store, &token, &cache::Machine { region: region.to_string(), arch: arch.to_string(), until, runner: runner.to_string() }).await.is_err() { return user_data }
        let link = format!("{plane_url}/cache/{token}");
        if os == "windows" { aws::with_cache_windows(user_data.clone(), &link) } else if runner.starts_with("gl:") { aws::with_cache_gitlab(user_data.clone(), &link) } else { aws::with_cache(user_data.clone(), &link) }.unwrap_or(user_data)
    }

    /// What the job on a machine may do in the cache, asked without a token (see `cache::branch_access`): a GitHub
    /// job's, by what GitHub said of it; a run from a computer's, as its token would say it.
    async fn machine_access(&self, machine: &cache::Machine) -> Result<cache::Access> {
        // A GitLab machine: the job its runner took, as GitLab said when it began (none yet: nothing to write).
        if let Some(local) = machine.runner.strip_prefix("gl:") {
            let at = format!("glrunner:{local}");
            let mut runner = get_json::<serde_json::Value>(self.store, &at).await?.ok_or("its runner is not kept")?;
            // GitLab's word of the job it took has not come (or not yet): GitLab is asked which job the runner runs.
            if runner["running"].is_null() {
                let id = runner["id"].as_u64().unwrap_or_default();
                if let Some(gl) = self.gitlab_by(runner["gitlab"].as_str().unwrap_or_default()) {
                    if let Ok(Some(job)) = gitlab::runner_job(self.http, gl, id).await {
                        runner["running"] = serde_json::json!({ "job": job["id"], "project_id": job["project"]["id"], "project": job["project"]["path_with_namespace"], "ref": job["ref"], "tag": job["tag"], "default_branch": job["project"]["default_branch"] });
                        put_json(self.store, &at, &runner).await?;
                    }
                }
            }
            let r = &runner["running"];
            let (Some(project), Some(git_ref)) = (r["project_id"].as_u64(), r["ref"].as_str()) else { return Err("its job has not begun".into()) };
            let connection = runner["gitlab"].as_str().unwrap_or_default();
            let ns = cache::gitlab_ns(connection, project);
            if let Some(path) = r["project"].as_str().filter(|p| !p.is_empty()) { let _ = self.store.put_if_absent(&format!("cachename:{ns}"), path.to_string()).await; }
            return cache::ref_access(&ns, &if r["tag"] == true { format!("refs/tags/{git_ref}") } else { format!("refs/heads/{git_ref}") }, r["default_branch"].as_str().unwrap_or_default())
                .ok_or_else(|| "a branch SuperCI does not keep".to_string())
        }
        let job = self.by_runner(Some(&machine.runner)).await?.ok_or("no job on this machine")?;
        if job.provider == "local" {
            let run = get_json::<serde_json::Value>(self.store, &format!("run:{}", job.run_id)).await?.ok_or("its run is not kept")?;
            let who = run["by"].as_str().filter(|b| !b.is_empty() && b.bytes().all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c))).unwrap_or("computer");
            let claims = cache::local_claims(run["cache"]["ns"].as_str().unwrap_or_default(), who, run["ref"].as_str().unwrap_or("refs/heads/main"), run["cache"]["default_branch"].as_str().unwrap_or("main"), &job.run_id.to_string(), &job.job_id.to_string());
            return cache::local_access(&claims)
        }
        if !job.provider.is_empty() { return Err("not a GitHub job".into()) }
        let (Some(repo), Some(branch)) = (job.repo_id, job.branch.as_deref()) else { return Err("GitHub did not say the job's repository and branch".into()) };
        cache::branch_access(repo, branch, job.default_branch.as_deref().unwrap_or_default()).ok_or_else(|| "a branch SuperCI does not keep".to_string())
    }

    /// GitHub's keys for its jobs' tokens: kept a day, and fetched again for a key not among them (once a minute at most).
    async fn github_keys(&self, kid: &str, now: u64) -> Result<serde_json::Value> {
        let kept = self.cache.github_keys.borrow().clone();
        let kept = match kept { Some(k) => Some(k), None => get_json::<(serde_json::Value, u64)>(self.store, "github-keys").await.ok().flatten() };
        let has = |keys: &serde_json::Value| keys["keys"].as_array().is_some_and(|l| l.iter().any(|k| k["kid"] == kid));
        if let Some((keys, at)) = &kept {
            if (has(keys) && now.saturating_sub(*at) < 86_400_000) || now.saturating_sub(*at) < 60_000 { self.cache.github_keys.replace(Some((keys.clone(), *at))); return Ok(keys.clone()) }
        }
        let r = self.http.send(Request::new("GET", "https://token.actions.githubusercontent.com/.well-known/jwks")).await?;
        if r.status != 200 { return kept.map(|(k, _)| k).ok_or_else(|| format!("GitHub's keys: {}", r.status)) }
        let keys: serde_json::Value = serde_json::from_slice(&r.body).map_err(|e| format!("GitHub's keys: {e}"))?;
        let _ = put_json(self.store, "github-keys", &(keys.clone(), now)).await;
        self.cache.github_keys.replace(Some((keys.clone(), now)));
        Ok(keys)
    }

    /// What a job's token lets it do in the cache: GitHub's own token (signed with GitHub's keys), or one this control
    /// plane gave a run from a computer (signed with its own).
    async fn cache_access(&self, token: &str, plane_url: &str, now: u64) -> Result<cache::Access> {
        use base64::Engine;
        let header: serde_json::Value = token.split('.').next().and_then(|h| base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(h.trim_end_matches('=')).ok())
            .and_then(|h| serde_json::from_slice(&h).ok()).ok_or("not a job's token")?;
        if header["alg"] == "ES256" {
            let claims = crate::crypto::verify_es256_jwt(token, &self.identity().await?.signing.public_jwk()?, now / 1000)?;
            if claims["iss"] != plane_url || claims["aud"] != "superci-cache" { return Err("not this control plane's token".into()) }
            return cache::local_access(&claims)
        }
        let keys = self.github_keys(header["kid"].as_str().unwrap_or_default(), now).await?;
        let claims = crate::crypto::verify_rs256_jwt(token, &keys, now / 1000)?;
        if claims["iss"] != "https://token.actions.githubusercontent.com" { return Err("not a GitHub job's token".into()) }
        cache::github_access(&claims)
    }

    /// `GET /cache/<machine's link>/program`: on to this version's `superci-cache` for the machine, in its region's
    /// bucket (a link that does only that, for a few minutes).
    async fn cache_program(&self, plane_url: &str, path: &str) -> Result<Response> {
        let now = self.clock.now_ms();
        let token = path.trim_start_matches("/cache/").trim_end_matches("/program");
        let Some(machine) = cache::machine(self.store, token, now).await? else { return Ok(nothing_here()) };
        let Some(a) = self.aws_connection().await?.filter(|a| a.connected) else { return Ok(nothing_here()) };
        let creds = self.aws_creds(plane_url, &a).await?;
        let url = aws::s3_url(&cache::bucket(&self.config.plane_id, &machine.region), &machine.region, &cache::program_key(&machine.arch));
        Ok(Response::redirect(&aws::presign("GET", &url, &creds, &machine.region, 600, now)?).with_status(302))
    }

    /// `POST /cache/<machine's link>/<what>`, from `superci-cache` with the job's token: `lookup` (a key, restore keys
    /// and version: links to read the hit's pieces), `begin` (an entry to save), `sign` (links to write its pieces),
    /// `finish` (its pieces, in order). A machine's link works for its region until its longest job is over.
    async fn cache_route(&self, req: &Request, plane_url: &str, path: &str) -> Result<Response> {
        let now = self.clock.now_ms();
        let mut parts = path.trim_start_matches("/cache/").splitn(2, '/');
        let (token, what) = (parts.next().unwrap_or_default(), parts.next().unwrap_or_default());
        let Some(machine) = cache::machine(self.store, token, now).await? else { return Ok(nothing_here()) };
        let body: serde_json::Value = serde_json::from_slice(&req.body).map_err(|e| format!("not JSON: {e}"))?;
        let refuse = |why: &str| Ok(Response::json(&serde_json::json!({ "ok": false, "why": why })).with_status(403));
        // The tools on the machine with no token of GitHub's (Turbo, Nx, Bazel): what the job on it may (see
        // `machine_access`); everything else, with the job's own token.
        let access = if what.starts_with("tool-") { self.machine_access(&machine).await } else { self.cache_access(body["token"].as_str().unwrap_or_default(), plane_url, now).await };
        let access = match access { Ok(a) => a, Err(e) => return refuse(&e) };
        let Some(a) = self.aws_connection().await?.filter(|a| a.connected) else { return refuse("this control plane has no AWS account") };
        let creds = self.aws_creds(plane_url, &a).await?;
        let (key, version) = (body["key"].as_str().unwrap_or_default(), body["version"].as_str().unwrap_or_default());
        let link = |method: &str, region: &str, object: &str| aws::presign(method, &aws::s3_url(&cache::bucket(&self.config.plane_id, region), region, object), &creds, region, cache::LINK_SECONDS, now);
        match what {
            "lookup" => {
                let restore: Vec<String> = body["restore_keys"].as_array().into_iter().flatten().filter_map(|k| k.as_str().map(str::to_string)).take(10).collect();
                // (A key or version GitHub would not take is found nowhere.)
                let Ok(Some(hit)) = cache::lookup(self.store, &access, key, &restore, version, now).await else { return Ok(Response::json(&serde_json::json!({ "hit": false }))) };
                let _ = cache::used(self.store, &hit, now).await;
                let prefix = hit.entry.prefix(&access.ns);
                let pieces = hit.entry.pieces.iter().map(|(p, size)| Ok(serde_json::json!({ "url": link("GET", &hit.entry.region, &format!("{prefix}/{p}"))?, "size": size }))).collect::<Result<Vec<_>>>()?;
                Ok(Response::json(&serde_json::json!({ "hit": true, "key": hit.key, "size": hit.entry.size, "pieces": pieces })))
            }
            "begin" => match cache::begin(self.store, &access, key, version, &machine.region, now).await {
                Ok(_) => Ok(Response::json(&serde_json::json!({ "ok": true }))),
                Err(cache::Refused::Exists) => Ok(Response::json(&serde_json::json!({ "ok": false, "why": "exists" })).with_status(409)),
                Err(cache::Refused::NoWrite) => refuse("this job may only read the cache"),
                Err(cache::Refused::Bad(e)) => Ok(Response::json(&serde_json::json!({ "ok": false, "why": e })).with_status(400)),
            },
            "sign" | "finish" => {
                let Some((at, entry)) = cache::begun(self.store, &access, key, version).await? else { return Ok(Response::json(&serde_json::json!({ "ok": false, "why": "no entry being saved" })).with_status(404)) };
                if what == "sign" {
                    let names: Vec<&str> = body["names"].as_array().into_iter().flatten().filter_map(|n| n.as_str()).collect();
                    if names.is_empty() || names.len() > 64 || !names.iter().all(|n| cache::piece_name(n)) { return Ok(Response::json(&serde_json::json!({ "ok": false, "why": "piece names" })).with_status(400)) }
                    let prefix = entry.prefix(&access.ns);
                    let urls = names.iter().map(|n| link("PUT", &entry.region, &format!("{prefix}/{n}"))).collect::<Result<Vec<_>>>()?;
                    return Ok(Response::json(&serde_json::json!({ "ok": true, "urls": urls })))
                }
                let pieces: Vec<(String, u64)> = body["pieces"].as_array().into_iter().flatten().filter_map(|p| Some((p[0].as_str()?.to_string(), p[1].as_u64()?))).collect();
                let done = cache::finish(self.store, &at, &entry, pieces).await?;
                Ok(Response::json(&serde_json::json!({ "ok": true, "size": done.size })))
            }
            // A tool's artifact (Turbo's, Nx's, Bazel's): one piece, under its hash, kept as a cache entry of its kind.
            "tool-get" | "tool-begin" | "tool-sign" | "tool-finish" => {
                let tool = body["tool"].as_str().unwrap_or_default();
                if !["turbo", "nx", "bazelac", "bazelcas", "gitlab"].contains(&tool) { return Ok(Response::json(&serde_json::json!({ "ok": false, "why": "not a tool SuperCI keeps" })).with_status(400)) }
                let hash = body["hash"].as_str().unwrap_or_default();
                match what {
                    "tool-get" => {
                        let Ok(Some(hit)) = cache::lookup(self.store, &access, hash, &[], tool, now).await else { return Ok(Response::json(&serde_json::json!({ "hit": false }))) };
                        let _ = cache::used(self.store, &hit, now).await;
                        let prefix = hit.entry.prefix(&access.ns);
                        let pieces = hit.entry.pieces.iter().map(|(p, size)| Ok(serde_json::json!({ "url": link("GET", &hit.entry.region, &format!("{prefix}/{p}"))?, "size": size }))).collect::<Result<Vec<_>>>()?;
                        Ok(Response::json(&serde_json::json!({ "hit": true, "size": hit.entry.size, "pieces": pieces, "meta": hit.entry.meta })))
                    }
                    "tool-begin" => match cache::begin(self.store, &access, hash, tool, &machine.region, now).await {
                        Ok((_, entry)) => Ok(Response::json(&serde_json::json!({ "ok": true, "url": link("PUT", &entry.region, &format!("{}/p00000", entry.prefix(&access.ns)))? }))),
                        Err(cache::Refused::Exists) => Ok(Response::json(&serde_json::json!({ "ok": false, "why": "exists" })).with_status(409)),
                        Err(cache::Refused::NoWrite) => refuse("this job may only read the cache"),
                        Err(cache::Refused::Bad(e)) => Ok(Response::json(&serde_json::json!({ "ok": false, "why": e })).with_status(400)),
                    },
                    _ => {
                        let Some((at, entry)) = cache::begun(self.store, &access, hash, tool).await? else { return Ok(Response::json(&serde_json::json!({ "ok": false, "why": "no entry being saved" })).with_status(404)) };
                        // More pieces of a large one.
                        if what == "tool-sign" {
                            let names: Vec<&str> = body["names"].as_array().into_iter().flatten().filter_map(|n| n.as_str()).collect();
                            if names.is_empty() || names.len() > 64 || !names.iter().all(|n| cache::piece_name(n)) { return Ok(Response::json(&serde_json::json!({ "ok": false, "why": "piece names" })).with_status(400)) }
                            let urls = names.iter().map(|n| link("PUT", &entry.region, &format!("{}/{n}", entry.prefix(&access.ns)))).collect::<Result<Vec<_>>>()?;
                            return Ok(Response::json(&serde_json::json!({ "ok": true, "urls": urls })))
                        }
                        let pieces: Vec<(String, u64)> = match body["pieces"].as_array() {
                            Some(list) => list.iter().filter_map(|p| Some((p[0].as_str()?.to_string(), p[1].as_u64()?))).collect(),
                            None => vec![("p00000".into(), body["size"].as_u64().unwrap_or(0))],
                        };
                        let done = cache::finish_with(self.store, &at, &entry, pieces, body["meta"].as_str().map(str::to_string)).await?;
                        Ok(Response::json(&serde_json::json!({ "ok": true, "size": done.size })))
                    }
                }
            }
            // A run from a computer's artifacts (a job of GitHub's keeps them at GitHub).
            "artifact-begin" | "artifact-sign" | "artifact-finish" | "artifacts" | "artifact-get" | "artifact-delete" => {
                let Some(run) = access.run.clone() else { return refuse("a job of GitHub's keeps its artifacts at GitHub") };
                let (name, ns) = (body["name"].as_str().unwrap_or_default(), cache::artifact_ns(&run));
                let missing = || Ok(Response::json(&serde_json::json!({ "ok": false, "why": "no such artifact" })).with_status(404));
                match what {
                    "artifact-begin" => match cache::begin_artifact(self.store, &run, name, &machine.region, now).await {
                        Ok(_) => Ok(Response::json(&serde_json::json!({ "ok": true }))),
                        Err(cache::Refused::Exists) => Ok(Response::json(&serde_json::json!({ "ok": false, "why": "an artifact of this name exists in the run" })).with_status(409)),
                        Err(cache::Refused::Bad(e)) => Ok(Response::json(&serde_json::json!({ "ok": false, "why": e })).with_status(400)),
                        Err(cache::Refused::NoWrite) => refuse("this job may not keep artifacts"),
                    },
                    "artifact-sign" => {
                        let Some((_, entry)) = cache::artifact(self.store, &run, name, false).await? else { return missing() };
                        let names: Vec<&str> = body["names"].as_array().into_iter().flatten().filter_map(|n| n.as_str()).collect();
                        if names.is_empty() || names.len() > 64 || !names.iter().all(|n| cache::piece_name(n)) { return Ok(Response::json(&serde_json::json!({ "ok": false, "why": "piece names" })).with_status(400)) }
                        let urls = names.iter().map(|n| link("PUT", &entry.region, &format!("{}/{n}", entry.prefix(&ns)))).collect::<Result<Vec<_>>>()?;
                        Ok(Response::json(&serde_json::json!({ "ok": true, "urls": urls })))
                    }
                    "artifact-finish" => {
                        let Some((at, entry)) = cache::artifact(self.store, &run, name, false).await? else { return missing() };
                        let pieces: Vec<(String, u64)> = body["pieces"].as_array().into_iter().flatten().filter_map(|p| Some((p[0].as_str()?.to_string(), p[1].as_u64()?))).collect();
                        let done = cache::finish(self.store, &at, &entry, pieces).await?;
                        Ok(Response::json(&serde_json::json!({ "ok": true, "id": cache::artifact_id(&done), "size": done.size })))
                    }
                    "artifacts" => {
                        let all = cache::artifacts(self.store, &run).await?;
                        Ok(Response::json(&serde_json::json!({ "ok": true, "artifacts": all.iter().map(|(n, e)| serde_json::json!({ "name": n, "id": cache::artifact_id(e), "size": e.size, "made": e.made })).collect::<Vec<_>>() })))
                    }
                    "artifact-get" => {
                        let Some((_, entry)) = cache::artifact(self.store, &run, name, true).await? else { return missing() };
                        let pieces = entry.pieces.iter().map(|(p, size)| Ok(serde_json::json!({ "url": link("GET", &entry.region, &format!("{}/{p}", entry.prefix(&ns)))?, "size": size }))).collect::<Result<Vec<_>>>()?;
                        Ok(Response::json(&serde_json::json!({ "ok": true, "id": cache::artifact_id(&entry), "size": entry.size, "pieces": pieces })))
                    }
                    _ => {
                        let Some((at, entry)) = cache::artifact(self.store, &run, name, true).await? else { return missing() };
                        self.store.delete(&at).await?;
                        for (p, _) in &entry.pieces { let _ = aws::s3(self.http, "DELETE", &aws::s3_url(&cache::bucket(&self.config.plane_id, &entry.region), &entry.region, &format!("{}/{p}", entry.prefix(&ns))), &entry.region, &creds, vec![], &[], now).await; }
                        Ok(Response::json(&serde_json::json!({ "ok": true, "id": cache::artifact_id(&entry) })))
                    }
                }
            }
            _ => Ok(nothing_here()),
        }
    }

    /// The cache as it stands: its settings, each repository with what it keeps (its name, as GitHub has it, found
    /// once), and with `?repo=` (`owner/name`, or its id `gh…`) that repository's entries.
    async fn caches(&self, url: &url::Url) -> Result<Response> {
        let now = self.clock.now_ms();
        let all = cache::list(self.store, None).await?;
        let mut repos: std::collections::BTreeMap<String, (usize, u64, u64)> = Default::default();
        for l in &all { let r = repos.entry(l.ns.clone()).or_default(); r.0 += 1; r.1 += l.entry.size; r.2 = r.2.max(l.entry.used.max(l.entry.made)) }
        let mut named = vec![];
        for (ns, (entries, bytes, used)) in &repos { named.push(serde_json::json!({ "id": ns, "name": self.cache_repo_name(ns).await, "entries": entries, "bytes": bytes, "used_ms": used })) }
        named.sort_by_key(|r| std::cmp::Reverse(r["bytes"].as_u64().unwrap_or(0)));
        let settings = &self.config.routing.cache;
        let mut out = serde_json::json!({ "on": !settings.off, "size_gb": settings.size_gb(), "days": settings.days(), "max_age_days": cache::MAX_AGE_DAYS, "repos": named, "now_ms": now });
        if let Some(want) = url.query_pairs().find(|(k, _)| k == "repo").map(|(_, v)| v.to_string()) {
            let ns = named.iter().find(|r| r["id"] == want.as_str() || r["name"].as_str().is_some_and(|n| n.eq_ignore_ascii_case(&want))).and_then(|r| r["id"].as_str().map(str::to_string));
            let entries: Vec<serde_json::Value> = all.iter().filter(|l| Some(&l.ns) == ns.as_ref()).map(|l| serde_json::json!({ "key": l.key, "scope": l.scope, "version": l.version,
                "bytes": l.entry.size, "made_ms": l.entry.made, "used_ms": l.entry.used.max(l.entry.made), "region": l.entry.region })).collect();
            out["entries"] = entries.into();
            out["repo"] = ns.map(serde_json::Value::from).unwrap_or(serde_json::Value::Null);
        }
        Ok(Response::json(&out))
    }

    /// A repository's name for the cache's id of it (`gh<GitHub's id>`), asked of GitHub once and kept.
    async fn cache_repo_name(&self, ns: &str) -> Option<String> {
        let key = format!("cachename:{ns}");
        if let Ok(Some(name)) = self.store.get(&key).await { return Some(name) }
        let id = ns.strip_prefix("gh")?;
        let now = self.clock.now_ms();
        for app in self.config.apps() {
            for i in github::installations(self.http, app, now).await.unwrap_or_default() {
                let Ok(token) = self.installation_token(app, i.id).await else { continue };
                if let Ok(r) = github::repository_by_id(self.http, &app.api(), &token, id).await {
                    if let Some(name) = r["full_name"].as_str() { let _ = self.store.put(&key, name.to_string()).await; return Some(name.to_string()) }
                }
            }
        }
        None
    }

    /// `POST /caches/delete` (the dashboard's): a repository's entries taken out, all of them or those with `key`
    /// (in `scope` only, when given), and their pieces deleted from the bucket.
    async fn caches_delete(&self, req: &Request, plane_url: &str) -> Result<Response> {
        let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap_or_default();
        let repo = body["repo"].as_str().unwrap_or_default();
        let ns = if repo.starts_with("gh") || repo.starts_with("gl") || repo.starts_with("local-") { repo.to_string() } else {
            let mut found = None;
            for l in cache::list(self.store, None).await? { if self.cache_repo_name(&l.ns).await.is_some_and(|n| n.eq_ignore_ascii_case(repo)) { found = Some(l.ns); break } }
            match found { Some(n) => n, None => return Ok(Response::json(&serde_json::json!({ "removed": 0 }))) }
        };
        let (removed, gone) = cache::remove(self.store, &ns, body["key"].as_str(), body["scope"].as_str()).await?;
        if let (false, Some(a)) = (gone.is_empty(), self.aws_connection().await?.filter(|a| a.connected)) {
            let creds = self.aws_creds(plane_url, &a).await?;
            let now = self.clock.now_ms();
            for (region, object) in gone.iter().flat_map(|(r, o)| o.iter().map(move |o| (r, o))).take(2000) {
                let _ = aws::s3(self.http, "DELETE", &aws::s3_url(&cache::bucket(&self.config.plane_id, region), region, object), region, &creds, vec![], &[], now).await;
            }
        }
        Ok(Response::json(&serde_json::json!({ "removed": removed, "repo": ns })))
    }

    /// Once an hour: entries that go (see `cache::evict`) taken out, their pieces deleted from the bucket (what is not,
    /// the bucket's own rule removes within `cache::BUCKET_DAYS`), and machines' links past their time forgotten.
    async fn sweep_cache(&self, plane_url: &str, now: u64) -> Result<()> {
        let last = self.store.get("cache-swept").await?.and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
        if now.saturating_sub(last) < 3_600_000 { return Ok(()) }
        self.store.put("cache-swept", now.to_string()).await?;
        for (k, v) in self.store.list("cachem:").await? {
            if serde_json::from_str::<cache::Machine>(&v).map(|m| m.until < now).unwrap_or(true) { self.store.delete(&k).await? }
        }
        let mut gone = cache::evict(self.store, now, &self.config.routing.cache).await?;
        // The artifacts of runs from a computer that are no longer kept.
        for (k, v) in self.store.list("artifact:").await? {
            let run = k.split(':').nth(1).unwrap_or_default().to_string();
            if self.store.get(&format!("run:{run}")).await?.is_some() { continue }
            self.store.delete(&k).await?;
            if let Ok(e) = serde_json::from_str::<cache::Entry>(&v) { gone.push((e.region.clone(), e.pieces.iter().map(|(p, _)| format!("{}/{p}", e.prefix(&cache::artifact_ns(&run)))).collect())) }
        }
        if gone.is_empty() { return Ok(()) }
        let Some(a) = self.aws_connection().await?.filter(|a| a.connected) else { return Ok(()) };
        let creds = self.aws_creds(plane_url, &a).await?;
        for (region, objects) in gone.iter().flat_map(|(r, o)| o.iter().map(move |o| (r, o))).take(500) {
            let _ = aws::s3(self.http, "DELETE", &aws::s3_url(&cache::bucket(&self.config.plane_id, region), region, objects), region, &creds, vec![], &[], now).await;
        }
        Ok(())
    }
}

/// An id for a run from a computer or one of its jobs: above every id GitHub or GitLab gives (2^52 and up), and
/// below 2^53, so a number in JavaScript holds it whole (GitHub's actions print a run's id from one); one of 2^52 at
/// random, so runs started together never share one.
fn local_id() -> u64 {
    let r = crate::crypto::random_bytes(8);
    (1u64 << 52) | (u64::from_le_bytes([r[0], r[1], r[2], r[3], r[4], r[5], r[6], r[7]]) & ((1u64 << 52) - 1))
}

/// Where a part of a run's working tree is kept in the bucket.
fn tree_object(tree: &str, part: u32) -> String { format!("runs/{tree}/{part:05}") }

/// A tree's part as kept (`region|object`; the region alone before bases were kept).
fn tree_place(kept: &str, tree: &str, part: u32) -> (String, String) {
    match kept.split_once('|') { Some((region, object)) => (region.to_string(), object.to_string()), None => (kept.to_string(), tree_object(tree, part)) }
}

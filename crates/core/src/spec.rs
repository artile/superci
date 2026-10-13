//! What a job asks for. Workflows say `runs-on: superci` for the default machine (set in the dashboard), or name
//! the machine in the label itself, in any order: `superci-8cpu`, `superci-8cpu-32gb`, `superci-arm64`,
//! `superci-100disk`, `superci-windows`, `superci-gpu` (or a GPU by name: `superci-l4`),
//! `superci-aws`, `superci-ondemand`; and of its disk on AWS: `superci-500mbs`, `superci-6000iops`,
//! `superci-ramdisk`, `superci-localdisk`. Nothing has to be created first;
//! the machine registers with GitHub under the exact label the job used, so only jobs asking for the same can take it.
use serde::{Deserialize, Serialize};

/// A machine as asked for. Unset parts are the default machine's, then each cloud's standard machine.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Spec {
    #[serde(default, skip_serializing_if = "Option::is_none")] pub cpu: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub ram_gb: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub disk_gb: Option<u32>,
    /// "x64" or "arm64".
    #[serde(default, skip_serializing_if = "Option::is_none")] pub arch: Option<String>,
    /// "linux", "windows" or "macos".
    #[serde(default, skip_serializing_if = "Option::is_none")] pub os: Option<String>,
    /// One GPU: by name (one of `GPUS`), or "any" (each place's least costly).
    #[serde(default, skip_serializing_if = "Option::is_none")] pub gpu: Option<String>,
    /// Only this cloud ("aws", "cloudflare", "modal").
    #[serde(default, skip_serializing_if = "Option::is_none")] pub cloud: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")] pub on_demand: bool,
    /// Its disk's speed on AWS, when not left to the machine's size (see `disk_speed`): MiB a second, and IOPS.
    #[serde(default, skip_serializing_if = "Option::is_none")] pub disk_mibs: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub disk_iops: Option<u32>,
    /// What a job writes most (Docker's files, its work folder, /tmp) in memory (`ramdisk`), or on a disk in the
    /// machine itself (`localdisk`: a machine type with an NVMe disk of its own). AWS, Linux.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")] pub ramdisk: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")] pub nvme: bool,
}

/// What AWS includes with a disk at no extra price: MiB a second, and IOPS.
pub const DISK_MIBS_INCLUDED: u32 = 125;
pub const DISK_IOPS_INCLUDED: u32 = 3000;

/// How fast a machine's disk is on AWS (MiB a second, IOPS). Left to the machine's size: 31.25 MiB a second for each
/// CPU, no less than what AWS includes and no more than 1,000. So the standard 4-CPU machine has what AWS includes,
/// at no extra price, and the extra on a larger one is a twentieth to a tenth of a spot machine's price (a machine of
/// any size could use more, for its first half hour: its size sets what is worth paying for, not what it can do). IOPS: what AWS includes, or what
/// the speed needs. As asked, when a label or the default machine says: within what a gp3 volume takes (125 to 2,000
/// MiB a second, 3,000 to 80,000 IOPS, 4 IOPS for each MiB a second, 500 IOPS for each GB).
pub fn disk_speed(cpu: u32, disk_gb: u32, s: &Spec) -> (u32, u32) {
    let by_size = (cpu * 125 / 4).clamp(DISK_MIBS_INCLUDED, 1000);
    let most_iops = (disk_gb.saturating_mul(500)).clamp(DISK_IOPS_INCLUDED, 80_000);
    let mibs = s.disk_mibs.unwrap_or(by_size).clamp(DISK_MIBS_INCLUDED, 2000).min(most_iops / 4);
    let iops = s.disk_iops.unwrap_or(DISK_IOPS_INCLUDED).clamp(DISK_IOPS_INCLUDED, 80_000).max(mibs * 4).min(most_iops);
    (mibs, iops)
}

/// The clouds a label may name.
pub const CLOUDS: [&str; 3] = ["aws", "cloudflare", "modal"];

/// The GPUs a label may name, least costly first.
pub const GPUS: [&str; 8] = ["t4", "l4", "a10g", "l40s", "a100", "h100", "h200", "b200"];

/// Whether a word can be a control plane's label (what workflows name in `runs-on`, before any part): two to twenty-four
/// lowercase letters and digits, a letter first, and not a word GitHub or a label's parts use (`ubuntu`, `gpu`…): a
/// label is told from its parts by the dashes between them.
pub fn valid_label(label: &str) -> Result<(), String> {
    let ok = (2..=24).contains(&label.len()) && label.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit()) && label.as_bytes()[0].is_ascii_lowercase();
    if !ok { return Err("A label is 2 to 24 lowercase letters and digits, starting with a letter (acmeci, ci2).".into()) }
    const TAKEN: [&str; 25] = ["localdisk", "ubuntu", "windows", "macos", "linux", "self", "hosted", "selfhosted", "x64", "arm64", "arm", "amd64", "x86", "mac", "osx", "win", "gpu", "aws", "cloudflare", "modal", "ondemand", "spot", "latest", "ramdisk", "nvme"];
    if TAKEN.contains(&label) || GPUS.contains(&label) { return Err(format!("{label} means something else in a label already: choose another word.")) }
    Ok(())
}

/// Whether a control plane can be called this where people read it (AcmeCI, Build Farm): two to twenty-four letters,
/// digits, spaces and dashes, a letter first and no space last. Nothing else, since it goes into an App's name and
/// into what a job's page says, where other marks mean something.
pub fn valid_name(name: &str) -> Result<(), String> {
    let ok = (2..=24).contains(&name.len()) && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b' ' || b == b'-')
        && name.as_bytes()[0].is_ascii_alphabetic() && !name.ends_with([' ', '-']) && !name.contains("  ");
    if ok { Ok(()) } else { Err("A name is 2 to 24 letters, digits, spaces and dashes, starting with a letter (AcmeCI, Build Farm).".into()) }
}

impl Spec {
    /// What a label asks for among several bases a control plane answers to: the longest that fits (so `ci` does not
    /// take `cibig-8cpu`).
    pub fn parse_among(label: &str, bases: &[String]) -> Option<Result<Spec, String>> {
        let mut bases: Vec<&String> = bases.iter().collect();
        bases.sort_by_key(|b| std::cmp::Reverse(b.len()));
        bases.into_iter().find_map(|b| Spec::parse(label, b))
    }

    /// What a label asks for: `base` alone is the default machine; `base-part-part…` names one. `None`: not ours;
    /// `Some(Err)`: ours, but a part is not understood (the job is told why instead of waiting forever).
    pub fn parse(label: &str, base: &str) -> Option<Result<Spec, String>> {
        let label = label.to_ascii_lowercase();
        let base = base.to_ascii_lowercase();
        if label == base { return Some(Ok(Spec::default())) }
        let rest = label.strip_prefix(&base)?.strip_prefix('-')?;
        let mut s = Spec::default();
        for part in rest.split('-') {
            let num = |suffix: &str| part.strip_suffix(suffix).and_then(|n| n.parse::<u32>().ok()).filter(|n| *n > 0);
            if part == "ramdisk" { s.ramdisk = true }
            else if part == "localdisk" { s.nvme = true }
            else if let Some(n) = num("mbs") { if !(125..=2000).contains(&n) { return Some(Err(format!("a disk's speed is 125 to 2000 MiB a second, not {n}"))) } s.disk_mibs = Some(n) }
            else if let Some(n) = num("iops") { if !(3000..=80_000).contains(&n) { return Some(Err(format!("a disk has 3000 to 80000 IOPS, not {n}"))) } s.disk_iops = Some(n) }
            else if let Some(n) = num("cpu") { if n > 192 { return Some(Err(format!("{n} CPUs is more than any machine has"))) } s.cpu = Some(n) }
            else if let Some(n) = num("disk") { if n > 16_000 { return Some(Err(format!("{n} GB of disk is too much"))) } s.disk_gb = Some(n) }
            else if let Some(n) = num("gb") { if n > 1536 { return Some(Err(format!("{n} GB of memory is more than any machine has"))) } s.ram_gb = Some(n) }
            else if ["arm64", "arm"].contains(&part) { s.arch = Some("arm64".into()) }
            else if ["x64", "amd64", "x86"].contains(&part) { s.arch = Some("x64".into()) }
            else if ["macos", "mac", "osx"].contains(&part) { s.os = Some("macos".into()) }
            else if ["windows", "win"].contains(&part) { s.os = Some("windows".into()) }
            else if part == "gpu" { s.gpu = Some("any".into()) }
            else if let Some(g) = GPUS.iter().find(|g| **g == part || (**g == "a10g" && part == "a10")) { s.gpu = Some(g.to_string()) }
            else if part == "linux" { s.os = Some("linux".into()) }
            else if part == "ondemand" { s.on_demand = true }
            else if CLOUDS.contains(&part) { s.cloud = Some(part.into()) }
            else { return Some(Err(format!("“{part}” in runs-on: {label} is not understood (sizes are like 8cpu, 32gb, 100disk; also arm64, windows, gpu or a GPU like l4, ondemand, a disk's speed like 500mbs, ramdisk, localdisk, or a cloud: aws, cloudflare, modal)"))) }
        }
        Some(Ok(s))
    }

    /// This, with unset parts taken from `default`.
    pub fn or(&self, default: &Spec) -> Spec {
        Spec {
            // CPU asked for without memory: memory follows the CPU (4 GB each), not the default's.
            cpu: self.cpu.or(default.cpu), ram_gb: self.ram_gb.or(if self.cpu.is_some() { None } else { default.ram_gb }), disk_gb: self.disk_gb.or(default.disk_gb),
            // A label that names another kind of machine (Windows, a GPU, arm64) does not take the default's
            // architecture or system with it (a default of arm64 does not make `-windows` Windows on arm64).
            arch: self.arch.clone().or_else(|| default.arch.clone().filter(|_| self.os.is_none() && self.gpu.is_none())),
            os: self.os.clone().or_else(|| default.os.clone().filter(|_| self.arch.is_none() && self.gpu.is_none())),
            gpu: self.gpu.clone().or_else(|| default.gpu.clone()),
            cloud: self.cloud.clone().or_else(|| default.cloud.clone()), on_demand: self.on_demand || default.on_demand,
            disk_mibs: self.disk_mibs.or(default.disk_mibs), disk_iops: self.disk_iops.or(default.disk_iops),
            ramdisk: self.ramdisk || default.ramdisk, nvme: self.nvme || default.nvme,
        }
    }

    /// The architecture: as asked, else arm64 for macOS (Apple silicon, as GitHub's macos-latest) and x64 otherwise.
    pub fn arch(&self) -> &str { self.arch.as_deref().unwrap_or(if self.os() == "macos" { "arm64" } else { "x64" }) }
    pub fn os(&self) -> &str { self.os.as_deref().unwrap_or("linux") }
    /// Memory: as asked, else 4 GB per CPU.
    pub fn ram(&self) -> Option<u32> { self.ram_gb.or(self.cpu.map(|c| c * 4)) }

    /// In a few words, for the dashboard and errors ("8 CPU, 32 GB, arm64").
    pub fn describe(&self) -> String {
        let mut parts = vec![];
        if let Some(c) = self.cpu { parts.push(format!("{c} CPU")) }
        if let Some(r) = self.ram_gb { parts.push(format!("{r} GB")) }
        if let Some(d) = self.disk_gb { parts.push(format!("{d} GB disk")) }
        if self.arch.is_some() { parts.push(self.arch().to_string()) }
        if self.os.is_some() { parts.push(self.os().to_string()) }
        match self.gpu.as_deref() { Some("any") => parts.push("a GPU".into()), Some(g) => parts.push(format!("{} GPU", g.to_uppercase())), None => {} }
        if self.on_demand { parts.push("on-demand".into()) }
        match self.disk_mibs { Some(DISK_MIBS_INCLUDED) => parts.push("standard disk speed".into()), Some(m) => parts.push(format!("{m} MiB/s disk")), None => {} }
        if let Some(i) = self.disk_iops { parts.push(format!("{i} IOPS")) }
        if self.ramdisk { parts.push("RAM disk".into()) }
        if self.nvme { parts.push("local disk".into()) }
        if let Some(c) = &self.cloud { parts.push(format!("only {c}")) }
        if parts.is_empty() { "the default machine".into() } else { parts.join(", ") }
    }
}

/// What a place can run: the largest machine, which systems, its standard machine (for what a label leaves out), and
/// its memory: per CPU when only CPUs are asked for, and the least it gives per CPU.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Capacity {
    pub max_cpu: u32, pub max_ram_gb: u32, pub max_disk_gb: u32, pub arch: Vec<String>, pub os: Vec<String>, pub on_demand: bool,
    pub std_cpu: u32, pub std_ram_gb: u32, pub std_disk_gb: u32,
    /// Memory for each CPU asked for when the label names no memory (up to the most it has); 0: none promised.
    pub ram_per_cpu: u32,
    /// The least memory it gives per CPU (memory asked for below it is rounded up).
    pub min_ram_per_cpu: u32,
    /// The GPUs it has (one per machine), least costly first; none: no GPUs.
    #[serde(default)] pub gpus: Vec<String>,
    /// Whether it can put a job's files in memory, or on a machine's own NVMe disk, when a label asks (AWS).
    #[serde(default)] pub scratch: bool,
}

/// A machine as a place starts it.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub struct Size { pub cpu: u32, pub ram_gb: u32, pub disk_gb: u32,
    /// Its GPU (one of `GPUS`), if it has one.
    #[serde(default, skip_deserializing, skip_serializing_if = "Option::is_none")] pub gpu: Option<&'static str> }

impl Capacity {
    /// The machine this place starts for `s`, or why it cannot.
    pub fn fit(&self, s: &Spec) -> Result<Size, String> {
        let cpu = s.cpu.unwrap_or(self.std_cpu);
        let ram = match (s.ram_gb, s.cpu) {
            (Some(r), _) => r,
            (None, Some(c)) => (c * self.ram_per_cpu).min(self.max_ram_gb),
            (None, None) => self.std_ram_gb,
        }.max(cpu * self.min_ram_per_cpu);
        let disk = s.disk_gb.unwrap_or(self.std_disk_gb);
        if cpu > self.max_cpu { return Err(format!("at most {} CPU", self.max_cpu)) }
        if ram > self.max_ram_gb { return Err(format!("at most {} GB memory", self.max_ram_gb)) }
        if disk > self.max_disk_gb { return Err(format!("at most {} GB disk", self.max_disk_gb)) }
        if !self.arch.iter().any(|a| a == s.arch()) { return Err(format!("{} only", self.arch.join(", "))) }
        if !self.os.iter().any(|o| o == s.os()) { return Err(format!("{} only", self.os.join(", "))) }
        if s.on_demand && !self.on_demand { return Err("no on-demand machines".into()) }
        if s.ramdisk && !self.scratch { return Err("no RAM disk".into()) }
        if s.nvme && !self.scratch { return Err("no machines with a local disk to ask for".into()) }
        if (s.ramdisk || s.nvme) && s.os() != "linux" { return Err("a RAM disk or a local disk on Linux only".into()) }
        if s.nvme && s.gpu.is_some() { return Err("no GPU machines with a local disk to ask for".into()) }
        if s.os() == "windows" && s.arch() != "x64" { return Err("Windows on x64 only".into()) }
        let gpu = match s.gpu.as_deref() {
            None => None,
            Some(_) if self.gpus.is_empty() => return Err("no GPUs".into()),
            Some(_) if s.os() == "windows" => return Err("no Windows machines with GPUs".into()),
            Some(_) if s.arch() != "x64" => return Err("GPUs on x64 only".into()),
            Some(g) => match self.gpus.iter().find(|m| g == "any" || *m == g) {
                Some(m) => GPUS.iter().copied().find(|k| k == m),
                None => return Err(format!("GPUs {} only", self.gpus.join(", ").to_uppercase())),
            },
        };
        Ok(Size { cpu, ram_gb: ram, disk_gb: disk, gpu })
    }

    /// Why this place cannot run `s`, if it cannot.
    pub fn refuses(&self, s: &Spec) -> Option<String> { self.fit(s).err() }

    /// AWS: any size EC2 has, x64 and arm64, Linux and Windows, spot or on-demand; standard 4 CPU, 8 GB, 60 GB disk.
    /// GPUs: NVIDIA T4 (g4dn), L4 (g6), A10G (g5), L40S (g6e), one per machine.
    pub fn aws() -> Self { Capacity { max_cpu: 192, max_ram_gb: 1536, max_disk_gb: 16_000, arch: vec!["x64".into(), "arm64".into()], os: vec!["linux".into(), "windows".into()], on_demand: true,
        std_cpu: 4, std_ram_gb: 8, std_disk_gb: 60, ram_per_cpu: 4, min_ram_per_cpu: 0, gpus: ["t4", "l4", "a10g", "l40s"].map(String::from).to_vec(), scratch: true } }
    /// Cloudflare containers, sized per job: 1 to 4 CPU, at least 3 GB per CPU, up to 12 GB and 20 GB disk, x64. CPU is
    /// billed only while used, so the standard machine is the largest: 4 CPU, 12 GB, 20 GB.
    pub fn cloudflare() -> Self { Capacity { max_cpu: 4, max_ram_gb: 12, max_disk_gb: 20, arch: vec!["x64".into()], os: vec!["linux".into()], on_demand: true,
        std_cpu: 4, std_ram_gb: 12, std_disk_gb: 20, ram_per_cpu: 4, min_ram_per_cpu: 3, gpus: vec![], scratch: false } }
    /// Modal sandboxes (non-preemptible, x64); standard 2 CPU, 8 GB; any of Modal's GPUs, one per sandbox.
    pub fn modal() -> Self { Capacity { max_cpu: 64, max_ram_gb: 336, max_disk_gb: 512, arch: vec!["x64".into()], os: vec!["linux".into()], on_demand: true,
        std_cpu: 2, std_ram_gb: 8, std_disk_gb: 64, ram_per_cpu: 4, min_ram_per_cpu: 0, gpus: GPUS.map(String::from).to_vec(), scratch: false } }
}

/// EC2 instance types for a machine, best first (fallbacks for when one has no spot capacity): the smallest size with
/// enough CPU in the family with enough memory per CPU (c: 2 GB, m: 4 GB, r: 8 GB), current generation first.
pub fn aws_instance_types(s: &Spec) -> Vec<String> {
    if let Some(gpu) = s.gpu.as_deref() { return aws_gpu_types(gpu, s) }
    const SIZES: [(u32, &str); 9] = [(2, "large"), (4, "xlarge"), (8, "2xlarge"), (16, "4xlarge"), (32, "8xlarge"), (48, "12xlarge"), (64, "16xlarge"), (96, "24xlarge"), (192, "48xlarge")];
    let arm = s.arch() == "arm64";
    let cpu = s.cpu.unwrap_or(4);
    let ram = s.ram().unwrap_or(if s.cpu.is_none() { 8 } else { cpu * 4 });
    let mut out = vec![];
    // (With a local NVMe disk, when asked for: the families that have one.)
    let families = match (s.nvme, arm) {
        (false, false) => [(2, ["c8a", "c7a"]), (4, ["m8a", "m7a"]), (8, ["r8a", "r7a"])],
        (false, true) => [(2, ["c8g", "c7g"]), (4, ["m8g", "m7g"]), (8, ["r8g", "r7g"])],
        (true, false) => [(2, ["c8id", "c6id"]), (4, ["m8id", "m6id"]), (8, ["r8id", "r6id"])],
        (true, true) => [(2, ["c8gd", "c7gd"]), (4, ["m8gd", "m7gd"]), (8, ["r8gd", "r7gd"])],
    };
    for (per_cpu, families) in families {
        let Some((c, size)) = SIZES.iter().find(|(c, _)| *c >= cpu && c * per_cpu >= ram) else { continue };
        for f in families { out.push((*c, per_cpu, format!("{f}.{size}"))) }
    }
    // The fewest CPUs first (what is paid for), then the least memory.
    out.sort_by_key(|(c, per_cpu, _)| (*c, *per_cpu));
    out.into_iter().map(|(_, _, t)| t).collect()
}

/// GPU machines (x64, one NVIDIA GPU) for a machine: the smallest size with enough CPU and memory, in each family
/// with the GPU; for any GPU, the least costly family first (T4, then L4, then A10G).
fn aws_gpu_types(gpu: &str, s: &Spec) -> Vec<String> {
    // (GPU, family, memory per CPU); sizes xlarge (4 CPU) to 16xlarge (64 CPU).
    const FAMILIES: [(&str, &str, u32); 4] = [("t4", "g4dn", 4), ("l4", "g6", 4), ("a10g", "g5", 4), ("l40s", "g6e", 8)];
    const SIZES: [(u32, &str); 5] = [(4, "xlarge"), (8, "2xlarge"), (16, "4xlarge"), (32, "8xlarge"), (64, "16xlarge")];
    let cpu = s.cpu.unwrap_or(4);
    let ram = s.ram().unwrap_or(16);
    FAMILIES.iter().filter(|(g, _, _)| gpu == *g || (gpu == "any" && *g != "l40s"))
        .filter_map(|(_, family, per_cpu)| SIZES.iter().find(|(c, _)| *c >= cpu && c * per_cpu >= ram).map(|(_, size)| format!("{family}.{size}"))).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_label_naming_another_kind_of_machine_does_not_take_the_defaults_kind() {
        let parse = |l: &str| Spec::parse(l, "superci").unwrap().unwrap();
        let arm = Spec { arch: Some("arm64".into()), cpu: Some(8), ..Default::default() };
        assert_eq!((parse("superci").or(&arm).arch(), parse("superci-windows").or(&arm).arch(), parse("superci-gpu").or(&arm).arch()), ("arm64", "x64", "x64"));
        assert_eq!(parse("superci-windows").or(&arm).cpu, Some(8), "its size still comes along");
        let windows = Spec { os: Some("windows".into()), ..Default::default() };
        assert_eq!((parse("superci").or(&windows).os(), parse("superci-arm64").or(&windows).os(), parse("superci-t4").or(&windows).os()), ("windows", "linux", "linux"));
    }

    #[test]
    fn labels() {
        assert_eq!(Spec::parse("superci", "superci"), Some(Ok(Spec::default())));
        assert_eq!(Spec::parse("ubuntu-latest", "superci"), None);
        assert_eq!(Spec::parse("supercix", "superci"), None);
        let s = Spec::parse("SuperCI-arm64-8cpu-32gb-100disk", "superci").unwrap().unwrap();
        assert_eq!((s.cpu, s.ram_gb, s.disk_gb, s.arch()), (Some(8), Some(32), Some(100), "arm64"));
        assert_eq!(Spec::parse("superci-8cpu-arm64", "superci"), Spec::parse("superci-arm64-8cpu", "superci"));
        let s = Spec::parse("superci-macos-aws-ondemand", "superci").unwrap().unwrap();
        assert_eq!((s.os(), s.cloud.as_deref(), s.on_demand), ("macos", Some("aws"), true));
        assert!(Spec::parse("superci-8cores", "superci").unwrap().unwrap_err().contains("8cores"));
        assert!(Spec::parse("superci-500cpu", "superci").unwrap().is_err());
        assert_eq!(Spec::parse("superci-8cpu", "superci").unwrap().unwrap().ram(), Some(32));
        let s = Spec::parse("superci-windows-8cpu", "superci").unwrap().unwrap();
        assert_eq!((s.os(), s.cpu, s.gpu.as_deref()), ("windows", Some(8), None));
        assert_eq!(Spec::parse("superci-gpu", "superci").unwrap().unwrap().gpu.as_deref(), Some("any"));
        assert_eq!(Spec::parse("superci-a10-16cpu", "superci").unwrap().unwrap().gpu.as_deref(), Some("a10g"));
        assert_eq!(Spec::parse("superci-H100", "superci").unwrap().unwrap().describe(), "H100 GPU");
        // A disk's speed, and where a job's files go.
        let s = Spec::parse("superci-8cpu-500mbs-6000iops-ramdisk", "superci").unwrap().unwrap();
        assert_eq!((s.disk_mibs, s.disk_iops, s.ramdisk, s.nvme, s.disk_gb), (Some(500), Some(6000), true, false, None));
        assert_eq!(s.describe(), "8 CPU, 500 MiB/s disk, 6000 IOPS, RAM disk");
        assert!(Spec::parse("superci-50mbs", "superci").unwrap().unwrap_err().contains("125 to 2000") && Spec::parse("superci-100iops", "superci").unwrap().is_err());
        let nvme = Spec::parse("superci-16cpu-localdisk", "superci").unwrap().unwrap();
        assert!(Spec::parse("superci-16cpu-nvme", "superci").unwrap().is_err() && Spec::parse("superci-tmpfs", "superci").unwrap().is_err(), "one word for each");
        assert_eq!(nvme.describe(), "16 CPU, local disk");
        assert_eq!(aws_instance_types(&nvme)[..2], ["m8id.4xlarge".to_string(), "m6id.4xlarge".to_string()]);
        assert_eq!(aws_instance_types(&Spec { arch: Some("arm64".into()), ..nvme.clone() })[0], "m8gd.4xlarge");
        assert!(Capacity::cloudflare().refuses(&nvme).is_some() && Capacity::modal().refuses(&s).is_some() && Capacity::aws().refuses(&s).is_none());
        assert!(Capacity::aws().refuses(&Spec::parse("superci-windows-ramdisk", "superci").unwrap().unwrap()).unwrap().contains("Linux only"));
        // The disk's speed follows the machine's size unless said; what is said stays within what a gp3 volume takes.
        let d = Spec::default();
        assert_eq!([2, 4, 8, 16, 32, 96].map(|cpu| disk_speed(cpu, 60, &d)), [(125, 3000), (125, 3000), (250, 3000), (500, 3000), (1000, 4000), (1000, 4000)]);
        assert_eq!(disk_speed(4, 60, &Spec { disk_mibs: Some(125), ..Default::default() }), (125, 3000), "the standard speed, when the default machine says so");
        assert_eq!(disk_speed(4, 60, &Spec { disk_mibs: Some(2000), ..Default::default() }), (2000, 8000));
        assert_eq!(disk_speed(8, 60, &Spec { disk_iops: Some(80_000), ..Default::default() }), (250, 30_000), "500 IOPS for each GB at most");
        assert_eq!(disk_speed(32, 8, &d), (1000, 4000));
    }

    #[test]
    fn defaults_fill_in() {
        let default = Spec { cpu: Some(2), disk_gb: Some(30), arch: Some("arm64".into()), ..Default::default() };
        let s = Spec { cpu: Some(8), ..Default::default() }.or(&default);
        assert_eq!((s.cpu, s.disk_gb, s.arch()), (Some(8), Some(30), "arm64"));
        assert_eq!(Spec::default().describe(), "the default machine");
        assert_eq!(s.describe(), "8 CPU, 30 GB disk, arm64");
        let with_memory = Spec { cpu: Some(2), ram_gb: Some(8), ..Default::default() };
        assert_eq!(Spec { cpu: Some(8), ..Default::default() }.or(&with_memory).ram(), Some(32), "memory follows the CPU asked for");
        assert_eq!(Spec { disk_gb: Some(100), ..Default::default() }.or(&with_memory).ram(), Some(8));
    }

    #[test]
    fn each_place_sizes_the_machine_itself() {
        let cf = Capacity::cloudflare();
        let size = |label: &str| cf.fit(&Spec::parse(label, "superci").unwrap().unwrap());
        assert_eq!(size("superci"), Ok(Size { cpu: 4, ram_gb: 12, disk_gb: 20, gpu: None }), "the standard is the largest");
        assert_eq!(size("superci-4cpu"), Ok(Size { cpu: 4, ram_gb: 12, disk_gb: 20, gpu: None }), "4 GB per CPU, up to the most it has");
        assert_eq!(size("superci-2cpu"), Ok(Size { cpu: 2, ram_gb: 8, disk_gb: 20, gpu: None }));
        assert_eq!(size("superci-1cpu-2gb"), Ok(Size { cpu: 1, ram_gb: 3, disk_gb: 20, gpu: None }), "at least 3 GB per CPU");
        assert!(size("superci-4cpu-16gb").unwrap_err().contains("12 GB"));
        assert!(size("superci-8cpu").unwrap_err().contains("4 CPU"));
        assert!(size("superci-40disk").unwrap_err().contains("20 GB disk"));
        assert_eq!(Capacity::aws().fit(&Spec { cpu: Some(8), ..Default::default() }), Ok(Size { cpu: 8, ram_gb: 32, disk_gb: 60, gpu: None }));
    }

    #[test]
    fn gpus_and_windows_go_where_they_are() {
        let spec = |label: &str| Spec::parse(label, "superci").unwrap().unwrap();
        // Any GPU: each place's least costly; one by name only where it is.
        assert_eq!(Capacity::modal().fit(&spec("superci-gpu")).unwrap().gpu, Some("t4"));
        assert_eq!(Capacity::modal().fit(&spec("superci-h100")).unwrap().gpu, Some("h100"));
        assert_eq!(Capacity::aws().fit(&spec("superci-l4")).unwrap().gpu, Some("l4"));
        assert_eq!(Capacity::aws().refuses(&spec("superci-h100")), Some("GPUs T4, L4, A10G, L40S only".into()));
        assert_eq!(Capacity::cloudflare().refuses(&spec("superci-gpu")), Some("no GPUs".into()));
        assert_eq!(Capacity::aws().refuses(&spec("superci-gpu-arm64")), Some("GPUs on x64 only".into()));
        // Windows: AWS only, x64, no GPUs.
        assert_eq!(Capacity::aws().refuses(&spec("superci-windows")), None);
        assert!(Capacity::cloudflare().refuses(&spec("superci-windows")).unwrap().contains("linux only"));
        assert!(Capacity::modal().refuses(&spec("superci-windows")).is_some());
        assert_eq!(Capacity::aws().refuses(&spec("superci-windows-arm64")), Some("Windows on x64 only".into()));
        assert_eq!(Capacity::aws().refuses(&spec("superci-windows-gpu")), Some("no Windows machines with GPUs".into()));
        // GPU machines on AWS: the smallest that fits, T4 first for any.
        assert_eq!(aws_instance_types(&spec("superci-gpu")), ["g4dn.xlarge", "g6.xlarge", "g5.xlarge"]);
        assert_eq!(aws_instance_types(&spec("superci-l40s-8cpu")), ["g6e.2xlarge"]);
        assert_eq!(aws_instance_types(&spec("superci-t4-4cpu-32gb")), ["g4dn.2xlarge"]);
    }

    #[test]
    fn places_say_what_they_cannot_run() {
        assert_eq!(Capacity::cloudflare().refuses(&Spec::default()), None);
        assert!(Capacity::cloudflare().refuses(&Spec { cpu: Some(8), ..Default::default() }).unwrap().contains("4 CPU"));
        assert!(Capacity::cloudflare().refuses(&Spec { arch: Some("arm64".into()), ..Default::default() }).is_some());
        assert_eq!(Capacity::aws().refuses(&Spec { cpu: Some(64), arch: Some("arm64".into()), on_demand: true, ..Default::default() }), None);
        assert!(Capacity::aws().refuses(&Spec { os: Some("macos".into()), ..Default::default() }).is_some());
    }

    #[test]
    fn instance_types_fit() {
        // The standard machine (unset size): 4 CPU, 8 GB, as before.
        assert_eq!(aws_instance_types(&Spec::default())[0], "c8a.xlarge");
        let t = aws_instance_types(&Spec { cpu: Some(8), ..Default::default() });
        assert_eq!(t[0], "m8a.2xlarge"); // 32 GB: 4 per CPU
        let t = aws_instance_types(&Spec { cpu: Some(2), ram_gb: Some(16), arch: Some("arm64".into()), ..Default::default() });
        assert_eq!(t[0], "r8g.large");
        let t = aws_instance_types(&Spec { cpu: Some(16), ram_gb: Some(16), ..Default::default() });
        assert_eq!(t[0], "c8a.4xlarge");
        assert!(t.contains(&"m7a.4xlarge".to_string()));
    }
}

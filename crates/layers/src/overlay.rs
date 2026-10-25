//! containerd's overlay snapshotter (`plugins/snapshots/overlay`): each snapshot a folder `snapshots/<number>` with
//! `fs` (its files, an overlay's upper folder) and, while active, `work` (overlay's own); a snapshot with no parent
//! is a bind mount of its `fs`, one with parents an overlay of theirs. Folder for folder and mount for mount as
//! containerd's, so the kernel and Docker see what they see with it.

use crate::error::{Error, Result};
use crate::meta::{self, Info, Kind, Meta, Snapshot, Usage};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// A mount as containerd's API says one: what the caller mounts.
#[derive(Debug, Clone, PartialEq)]
pub struct Mount { pub kind: String, pub source: String, pub options: Vec<String> }

/// containerd's labels for a snapshot made for a user namespace: the container's ids mapped to the machine's.
pub const UID_MAPPING: &str = "containerd.io/snapshot/uidmapping";
pub const GID_MAPPING: &str = "containerd.io/snapshot/gidmapping";

pub struct Overlay {
    pub root: PathBuf,
    pub meta: Meta,
    /// Folders of removed snapshots are taken away by `cleanup`, not at once (containerd's default, as Docker's
    /// containerd runs it).
    pub async_remove: bool,
    /// Options every overlay mount has (`index=off` where the kernel has that switch).
    pub options: Vec<String>,
}

impl Overlay {
    pub fn open(root: &Path, async_remove: bool) -> Result<Overlay> {
        // (containerd checks that the file system says each entry's type, which xfs made with ftype=0 does not: the
        // store's disk is ext4, as is a machine's own.)
        mkdir_all(root, 0o700).and_then(crate::faults::pass)?;
        // (containerd adds "userxattr" for an overlay made in a user namespace; this runs as the machine's root only.)
        if in_user_namespace() { return Err(Error::Other("not supported in a user namespace".into())) }
        let meta = Meta::open(&root.join("metadata.db")).and_then(crate::faults::pass)?;
        match crate::faults::check(std::fs::DirBuilder::new().mode(0o700).create(root.join("snapshots"))) {
            Err(e) if e.kind() != std::io::ErrorKind::AlreadyExists => return Err(e.into()),
            _ => {}
        }
        // (containerd adds `index=off` where the kernel has that switch, which every kernel SuperCI's machines run has.)
        let options = vec!["index=off".to_string()];
        Ok(Overlay { root: root.to_path_buf(), meta, async_remove, options })
    }

    pub fn upper(&self, id: &str) -> PathBuf { self.root.join("snapshots").join(id).join("fs") }
    fn work(&self, id: &str) -> PathBuf { self.root.join("snapshots").join(id).join("work") }

    pub fn stat(&self, key: &str) -> Result<Info> { self.meta.read(|r| r.info(key)).map(|(_, i, _)| i) }

    pub fn update(&self, info: &Info, paths: &[String]) -> Result<Info> { self.meta.write(|w| w.update(info, paths)) }

    /// What a snapshot takes: an active one's upper folder as it is now (read from disk), a committed one's as kept.
    pub fn usage(&self, key: &str) -> Result<Usage> {
        let (id, info, usage) = self.meta.read(|r| r.info(key)).and_then(crate::faults::pass)?;
        if info.kind == Some(Kind::Active) { return disk_usage(&self.upper(&id)) }
        Ok(usage)
    }

    pub fn prepare(&self, key: &str, parent: &str, labels: BTreeMap<String, String>) -> Result<Vec<Mount>> {
        self.create(Kind::Active, key, parent, labels)
    }

    pub fn view(&self, key: &str, parent: &str, labels: BTreeMap<String, String>) -> Result<Vec<Mount>> {
        self.create(Kind::View, key, parent, labels)
    }

    pub fn mounts(&self, key: &str) -> Result<Vec<Mount>> {
        let s = self.meta.read(|r| r.snapshot(key).and_then(crate::faults::pass).map_err(|e| e.context("failed to get active mount"))).and_then(crate::faults::pass)?;
        Ok(self.mounts_of(&s))
    }

    /// An active snapshot made committed as `name`, its usage read from its upper folder.
    pub fn commit(&self, name: &str, key: &str, labels: BTreeMap<String, String>, parent: &str) -> Result<()> {
        self.commit_with(name, key, labels, parent, None)
    }

    /// The same, with its usage as given (a layer mounted from SuperCI's store says its own) or read from disk.
    pub fn commit_with(&self, name: &str, key: &str, labels: BTreeMap<String, String>, parent: &str, usage: Option<Usage>) -> Result<()> {
        self.meta.write(|w| {
            let (id, _, _) = w.info(key).and_then(crate::faults::pass)?;
            let usage = match usage { Some(u) => u, None => disk_usage(&self.upper(&id)).and_then(crate::faults::pass)? };
            w.commit(key, name, usage, labels, parent).and_then(crate::faults::pass).map_err(|e| e.context(format!("failed to commit snapshot {key}")))?;
            Ok(())
        })
    }

    /// Takes a snapshot out; its folder now, or at the next `cleanup` (`async_remove`).
    pub fn remove(&self, key: &str) -> Result<()> {
        let removals = self.meta.write(|w| {
            w.remove(key).and_then(crate::faults::pass).map_err(|e| e.context(format!("failed to remove snapshot {key}")))?;
            if self.async_remove { return Ok(vec![]) }
            self.cleanup_dirs(&w.ids().and_then(crate::faults::pass)?).map_err(|e| e.context("unable to get directories for removal"))
        }).and_then(crate::faults::pass)?;
        // (The snapshot is gone from the store already: a folder that cannot be taken away is said, not failed.)
        for dir in removals { if let Err(e) = remove_snapshot_dir(&dir) { eprintln!("failed to remove directory {}: {e}", dir.display()) } }
        Ok(())
    }

    pub fn walk(&self, filters: &[String], f: impl FnMut(Info) -> Result<()>) -> Result<()> { self.meta.read(|r| r.walk(filters, f)) }

    /// The folders of snapshots no longer in the store, taken away.
    pub fn cleanup(&self) -> Result<()> {
        // (In a change, so no snapshot is made while the folders are read.)
        let dirs = self.meta.write(|w| self.cleanup_dirs(&w.ids().and_then(crate::faults::pass)?)).and_then(crate::faults::pass)?;
        for dir in dirs { if let Err(e) = remove_snapshot_dir(&dir) { eprintln!("failed to remove directory {}: {e}", dir.display()) } }
        Ok(())
    }

    fn cleanup_dirs(&self, ids: &std::collections::HashMap<String, String>) -> Result<Vec<PathBuf>> {
        let dir = self.root.join("snapshots");
        let mut out = vec![];
        for e in std::fs::read_dir(&dir).and_then(crate::faults::pass)? {
            let name = e.and_then(crate::faults::pass)?.file_name().to_string_lossy().into_owned();
            if !ids.contains_key(&name) { out.push(dir.join(name)) }
        }
        Ok(out)
    }

    fn create(&self, kind: Kind, key: &str, parent: &str, labels: BTreeMap<String, String>) -> Result<Vec<Mount>> {
        let snapshots = self.root.join("snapshots");
        let mut td: Option<PathBuf> = None;
        let out = self.meta.write(|w| {
            let t = prepare_directory(&snapshots, kind).map_err(|(t, e)| (t, e.context("failed to create prepare snapshot dir")));
            let t = match t { Ok(t) => t, Err((t, e)) => { td = t; return Err(e) } };
            td = Some(t.clone());
            let s = w.create(kind, key, parent, labels).and_then(crate::faults::pass).map_err(|e| e.context("failed to create snapshot"))?;
            let (_, info, _) = w.info(key).and_then(crate::faults::pass).map_err(|e| e.context("failed to get snapshot info"))?;
            // The owner of its files' root: the user namespace's root when mapped, else its parent's root's.
            let mut owner: Option<(u32, u32)> = None;
            if info.labels.contains_key(UID_MAPPING) || info.labels.contains_key(GID_MAPPING) {
                let unmarshal = |key| mappings(info.labels.get(key).map(String::as_str).unwrap_or("")).map_err(|e| Error::Other(format!("failed to unmarshal snapshot ID mapped labels: {e}")));
                let (uids, gids) = (unmarshal(UID_MAPPING).and_then(crate::faults::pass)?, unmarshal(GID_MAPPING).and_then(crate::faults::pass)?);
                let (uid, gid) = (root_on_host(&uids).and_then(crate::faults::said), root_on_host(&gids).and_then(crate::faults::said));
                owner = Some((uid.map_err(|e| Error::Other(format!("failed to find root pair: {e}")))?, gid.map_err(|e| Error::Other(format!("failed to find root pair: {e}")))?));
            }
            if owner.is_none() {
                if let Some(p) = s.parent_ids.first() {
                    use std::os::unix::fs::MetadataExt;
                    let st = std::fs::metadata(self.upper(p)).and_then(crate::faults::pass).map_err(|e| Error::Other(format!("failed to stat parent: {e}")))?;
                    owner = Some((st.uid(), st.gid()));
                }
            }
            if let Some((uid, gid)) = owner {
                std::os::unix::fs::lchown(t.join("fs"), Some(uid), Some(gid)).and_then(crate::faults::pass).map_err(|e| Error::Other(format!("failed to chown: {e}")))?;
            }
            let p = snapshots.join(&s.id);
            std::fs::rename(&t, &p).and_then(crate::faults::pass).map_err(|e| Error::Other(format!("failed to rename: {e}")))?;
            td = None;
            Ok(s)
        });
        match out {
            Ok(s) => Ok(self.mounts_of(&s)),
            Err(e) => {
                // (The folder under its own name is made last, by the rename: on any failure there is none.)
                if let Some(t) = td { if let Err(e1) = crate::faults::check(std::fs::remove_dir_all(&t)) { eprintln!("failed to cleanup temp snapshot directory: {e1}") } }
                Err(e)
            }
        }
    }

    fn mounts_of(&self, s: &Snapshot) -> Vec<Mount> {
        // (Ids are mapped by owning the files, as containerd's overlay does without `remap_ids`.)
        let mut options = vec![];
        let bind = |source: PathBuf, ro: bool, mut options: Vec<String>| {
            options.extend([if ro { "ro" } else { "rw" }.to_string(), "rbind".to_string()]);
            vec![Mount { kind: "bind".into(), source: source.to_string_lossy().into_owned(), options }]
        };
        if s.parent_ids.is_empty() { return bind(self.upper(&s.id), s.kind == Kind::View, options) }
        if s.kind == Kind::Active {
            options.push(format!("workdir={}", self.work(&s.id).display()));
            options.push(format!("upperdir={}", self.upper(&s.id).display()));
        } else if s.parent_ids.len() == 1 {
            return bind(self.upper(&s.parent_ids[0]), true, options);
        }
        let lower: Vec<String> = s.parent_ids.iter().map(|p| self.upper(p).to_string_lossy().into_owned()).collect();
        options.push(format!("lowerdir={}", lower.join(":")));
        options.extend(self.options.iter().cloned());
        vec![Mount { kind: "overlay".into(), source: "overlay".into(), options }]
    }
}

use std::os::unix::fs::DirBuilderExt;

fn mkdir_all(p: &Path, mode: u32) -> Result<()> { std::fs::DirBuilder::new().recursive(true).mode(mode).create(p).map_err(Into::into) }

/// A new snapshot's folder, under a temporary name until it is in the store (`new-…`, as Go's `MkdirTemp` makes:
/// only its owner may enter); on failure, whatever of it was made, to be taken away.
fn prepare_directory(snapshots: &Path, kind: Kind) -> std::result::Result<PathBuf, (Option<PathBuf>, Error)> {
    // (Its name of the time, this program and a count: no other is ever given it.)
    let td = snapshots.join(format!("new-{}", random_suffix()));
    crate::faults::check(std::fs::DirBuilder::new().mode(0o700).create(&td)).map_err(|e| (None, Error::Other(format!("failed to create temp dir: {e}"))))?;
    if let Err(e) = crate::faults::check(std::fs::DirBuilder::new().mode(0o755).create(td.join("fs"))) { return Err((Some(td), e.into())) }
    if kind == Kind::Active {
        if let Err(e) = crate::faults::check(std::fs::DirBuilder::new().mode(0o711).create(td.join("work"))) { return Err((Some(td), e.into())) }
    }
    Ok(td)
}

fn random_suffix() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let x = (meta::now() as u64) ^ (std::process::id() as u64).rotate_left(32) ^ N.fetch_add(0x9e37_79b9_7f4a_7c15, Ordering::Relaxed);
    format!("{}", x % 10_000_000_000)
}

/// A snapshot's folder taken away; a store layer mounted on its `fs` is unmounted first.
pub fn remove_snapshot_dir(dir: &Path) -> std::io::Result<()> {
    let fs = dir.join("fs");
    if is_mount_point(&fs) { crate::mount::unmount(&fs).and_then(crate::faults::pass)? }
    match crate::faults::check(std::fs::remove_dir_all(dir)) { Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()), r => r }
}

/// Whether a snapshot's folder is where something is mounted: on another device than its parent folder.
pub fn is_mount_point(p: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (std::fs::symlink_metadata(p), p.parent().map(std::fs::symlink_metadata)) {
        (Ok(a), Some(Ok(b))) => a.dev() != b.dev(),
        _ => false,
    }
}

/// What a folder's files take on disk, as containerd counts it (continuity's `DiskUsage`): each inode once
/// (hard links), its blocks of 512 bytes; links not followed; the folder itself counted.
pub fn disk_usage(root: &Path) -> Result<Usage> {
    use std::os::unix::fs::MetadataExt;
    let mut seen = std::collections::HashSet::new();
    let mut size: i64 = 0;
    let mut stack = vec![root.to_path_buf()];
    loop {
        let Some(p) = stack.pop() else { break };
        let m = std::fs::symlink_metadata(&p).and_then(crate::faults::pass)?;
        if seen.insert((m.dev(), m.ino())) { size += m.blocks() as i64 * 512 }
        if m.is_dir() { for e in std::fs::read_dir(&p).and_then(crate::faults::pass)? { stack.push(e.and_then(crate::faults::pass)?.path()) } }
    }
    Ok(Usage { size, inodes: seen.len() as i64 })
}

/// containerd's id mapping label ("0:100000:65536,…": container id, machine id, how many; containerd's
/// `IDMap.Unmarshal`), each mapping as Go's `Sscanf("%d:%d:%d")` reads it.
fn mappings(label: &str) -> std::result::Result<Option<Vec<(u32, u32, u32)>>, String> {
    if label.is_empty() { return Ok(None) }
    label.split(',').map(|m| {
        let bad = |why: &str| format!("input value {m} unparsable: {why}");
        // (A number as Sscanf's %d reads one: spaces before it, a sign, digits; and where it ended.)
        let number = |t: &str| -> std::result::Result<(i64, usize), String> {
            let t2 = t.trim_start();
            let end = t2.char_indices().find(|(i, c)| !(c.is_ascii_digit() || (*i == 0 && (*c == '-' || *c == '+')))).map(|(i, _)| i).unwrap_or(t2.len());
            Ok((t2[..end].parse().map_err(|_| bad("expected integer"))?, t.len() - t2.len() + end))
        };
        let (c, at) = number(m)?;
        let rest = m[at..].strip_prefix(':').ok_or_else(|| bad("input does not match format"))?;
        let (h, at) = number(rest)?;
        let rest = rest[at..].strip_prefix(':').ok_or_else(|| bad("input does not match format"))?;
        let (l, _) = number(rest)?;
        let invalid = u32::MAX as i64;
        if [c, h, l].iter().any(|n| !(0..invalid).contains(n)) { return Err(format!("invalid mapping \"{m}\"")) }
        Ok((c as u32, h as u32, l as u32))
    }).collect::<std::result::Result<Vec<_>, _>>().map(Some)
}

/// The machine's id that a container's root is (containerd's `toHost` of 0): itself with no mapping.
fn root_on_host(map: &Option<Vec<(u32, u32, u32)>>) -> std::result::Result<u32, String> {
    let Some(map) = map else { return Ok(0) };
    map.iter().find(|(c, _, n)| *c == 0 && *n > 0).map(|(_, h, _)| *h).ok_or_else(|| "container ID 0 cannot be mapped to a host ID".to_string())
}

/// Whether this runs in a user namespace (containerd's `RunningInUserNS`): its ids map other than one to one.
fn in_user_namespace() -> bool {
    match crate::faults::check(std::fs::read_to_string("/proc/self/uid_map")) {
        Ok(m) => m.split_whitespace().collect::<Vec<_>>() != ["0", "0", "4294967295"],
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn overlay(async_remove: bool) -> (Overlay, PathBuf) {
        let dir = std::env::temp_dir().join(format!("superci-layers-ov-{}-{}", std::process::id(), random_suffix()));
        (Overlay::open(&dir, async_remove).unwrap(), dir)
    }
    fn none() -> BTreeMap<String, String> { BTreeMap::new() }

    #[test]
    fn mounts_are_containerds_bind_with_no_parent_and_overlay_with_parents() {
        let (o, dir) = overlay(false);
        let r = |s: &str| dir.join("snapshots").join(s).join("fs").to_string_lossy().into_owned();
        let w = |s: &str| dir.join("snapshots").join(s).join("work").to_string_lossy().into_owned();
        assert_eq!(o.prepare("a-key", "", none()).unwrap(), [Mount { kind: "bind".into(), source: r("1"), options: vec!["rw".into(), "rbind".into()] }]);
        assert!(dir.join("snapshots/1/fs").is_dir() && dir.join("snapshots/1/work").is_dir());
        std::fs::write(dir.join("snapshots/1/fs/f"), b"hello").unwrap();
        o.commit("a", "a-key", none(), "").unwrap();
        assert!(o.usage("a").unwrap().inodes == 2, "the folder and its file");
        assert_eq!(o.view("v1", "a", none()).unwrap(), [Mount { kind: "bind".into(), source: r("1"), options: vec!["ro".into(), "rbind".into()] }], "a view of one parent: that parent, read-only");
        let mut opts = vec![format!("workdir={}", w("3")), format!("upperdir={}", r("3")), format!("lowerdir={}", r("1"))];
        opts.extend(o.options.clone());
        assert_eq!(o.prepare("b-key", "a", none()).unwrap(), [Mount { kind: "overlay".into(), source: "overlay".into(), options: opts }]);
        o.commit("b", "b-key", none(), "").unwrap();
        let mut opts = vec![format!("lowerdir={}:{}", r("3"), r("1"))];
        opts.extend(o.options.clone());
        assert_eq!(o.view("v2", "b", none()).unwrap(), [Mount { kind: "overlay".into(), source: "overlay".into(), options: opts.clone() }], "a view of several: their overlay, no upper");
        assert_eq!(o.mounts("v2").unwrap()[0].options, opts);
        assert!(matches!(o.mounts("b"), Err(Error::FailedPrecondition(_))));
        // Removed now (not async): the store and the folder; a parent with children stays.
        assert!(matches!(o.remove("a"), Err(Error::FailedPrecondition(_))));
        o.remove("v2").unwrap();
        assert!(!dir.join("snapshots/4").exists() && dir.join("snapshots/2").exists(), "v2's folder gone at once, v1's kept");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn removal_waits_for_cleanup_when_async_and_temporary_folders_go_too() {
        let (o, dir) = overlay(true);
        o.prepare("a", "", none()).unwrap();
        o.remove("a").unwrap();
        assert!(dir.join("snapshots/1").exists(), "still there until cleanup");
        std::fs::create_dir(dir.join("snapshots/new-123")).unwrap();
        o.cleanup().unwrap();
        assert_eq!(std::fs::read_dir(dir.join("snapshots")).unwrap().count(), 0);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_failed_create_leaves_no_folder_and_owners_come_from_the_parent_or_the_mapping() {
        let (o, dir) = overlay(false);
        assert!(o.prepare("x", "missing", none()).unwrap_err().is_not_found());
        assert_eq!(std::fs::read_dir(dir.join("snapshots")).unwrap().count(), 0, "the temporary folder is gone");
        let root = |m: &str| mappings(m).and_then(|m| root_on_host(&m));
        assert_eq!((root("0:100000:65536"), root("1:5:10,0:7:1"), root("")), (Ok(100000), Ok(7), Ok(0)), "no mapping: one to one");
        assert_eq!(root("1:5:10").unwrap_err(), "container ID 0 cannot be mapped to a host ID");
        assert!(root("a:b").unwrap_err().starts_with("input value a:b unparsable") && root("0:-1:5").unwrap_err() == "invalid mapping \"0:-1:5\"");
        let _ = std::fs::remove_dir_all(dir);
    }
}

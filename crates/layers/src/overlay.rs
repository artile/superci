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
    /// Ids mapped by the mount itself (`uidmap=`, `gidmap=`), as containerd's `remap_ids`; otherwise by owner.
    pub remap_ids: bool,
    /// Options every overlay mount has (`index=off` where the kernel has that switch).
    pub options: Vec<String>,
}

impl Overlay {
    pub fn open(root: &Path, async_remove: bool) -> Result<Overlay> {
        mkdir_all(root, 0o700).and_then(crate::faults::pass)?;
        if !supports_dtype(root).and_then(crate::faults::pass)? {
            return Err(Error::Other(format!("{} does not support d_type. If the backing filesystem is xfs, please reformat with ftype=1 to enable d_type support", root.display())));
        }
        // (containerd adds "userxattr" for an overlay made in a user namespace; this runs as the machine's root only.)
        if in_user_namespace() { return Err(Error::Other("not supported in a user namespace".into())) }
        let meta = Meta::open(&root.join("metadata.db")).and_then(crate::faults::pass)?;
        match std::fs::DirBuilder::new().mode(0o700).create(root.join("snapshots")) {
            Err(e) if e.kind() != std::io::ErrorKind::AlreadyExists => return Err(e.into()),
            _ => {}
        }
        let options = if Path::new("/sys/module/overlay/parameters/index").exists() { vec!["index=off".to_string()] } else { vec![] };
        Ok(Overlay { root: root.to_path_buf(), meta, async_remove, remap_ids: false, options })
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
        let (s, info) = self.meta.read(|r| {
            let s = r.snapshot(key).map_err(|e| e.context("failed to get active mount")).and_then(crate::faults::pass)?;
            let (_, info, _) = r.info(key).map_err(|e| e.context("failed to get snapshot info")).and_then(crate::faults::pass)?;
            Ok((s, info))
        }).and_then(crate::faults::pass)?;
        Ok(self.mounts_of(&s, &info))
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
            w.commit(key, name, usage, labels, parent).map_err(|e| e.context(format!("failed to commit snapshot {key}"))).and_then(crate::faults::pass)?;
            Ok(())
        })
    }

    /// Takes a snapshot out; its folder now, or at the next `cleanup` (`async_remove`).
    pub fn remove(&self, key: &str) -> Result<()> {
        let removals = self.meta.write(|w| {
            w.remove(key).map_err(|e| e.context(format!("failed to remove snapshot {key}"))).and_then(crate::faults::pass)?;
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
        let mut path: Option<PathBuf> = None;
        let out = self.meta.write(|w| {
            let t = prepare_directory(&snapshots, kind).map_err(|(t, e)| (t, e.context("failed to create prepare snapshot dir")));
            let t = match t { Ok(t) => t, Err((t, e)) => { td = t; return Err(e) } };
            td = Some(t.clone());
            let s = w.create(kind, key, parent, labels).map_err(|e| e.context("failed to create snapshot")).and_then(crate::faults::pass)?;
            let (_, info, _) = w.info(key).map_err(|e| e.context("failed to get snapshot info")).and_then(crate::faults::pass)?;
            // The owner of its files' root: the user namespace's root when mapped, else its parent's root's.
            let mut owner: Option<(u32, u32)> = None;
            if info.labels.contains_key(UID_MAPPING) || info.labels.contains_key(GID_MAPPING) {
                let unmarshal = |key| mappings(info.labels.get(key).map(String::as_str).unwrap_or("")).map_err(|e| Error::Other(format!("failed to unmarshal snapshot ID mapped labels: {e}")));
                let (uids, gids) = (unmarshal(UID_MAPPING).and_then(crate::faults::pass)?, unmarshal(GID_MAPPING).and_then(crate::faults::pass)?);
                let root = to_host(0, &uids).and_then(|u| Ok((u, to_host(0, &gids)?))).map_err(|e| Error::Other(format!("failed to find root pair: {e}")))?;
                owner = Some(root);
            }
            if owner.is_none() {
                if let Some(p) = s.parent_ids.first() {
                    use std::os::unix::fs::MetadataExt;
                    let st = std::fs::metadata(self.upper(p)).map_err(|e| Error::Other(format!("failed to stat parent: {e}"))).and_then(crate::faults::pass)?;
                    owner = Some((st.uid(), st.gid()));
                }
            }
            if let Some((uid, gid)) = owner {
                std::os::unix::fs::lchown(t.join("fs"), Some(uid), Some(gid)).map_err(|e| Error::Other(format!("failed to chown: {e}"))).and_then(crate::faults::pass)?;
            }
            let p = snapshots.join(&s.id);
            path = Some(p.clone());
            std::fs::rename(&t, &p).map_err(|e| Error::Other(format!("failed to rename: {e}"))).and_then(crate::faults::pass)?;
            td = None;
            Ok((s, info))
        });
        match out {
            Ok((s, info)) => Ok(self.mounts_of(&s, &info)),
            Err(e) => {
                if let Some(t) = td { if let Err(e1) = std::fs::remove_dir_all(&t) { eprintln!("failed to cleanup temp snapshot directory: {e1}") } }
                if let Some(p) = path {
                    if let Err(e1) = std::fs::remove_dir_all(&p) {
                        return Err(Error::Other(format!("failed to remove path: {e1}: {e}")));
                    }
                }
                Err(e)
            }
        }
    }

    fn mounts_of(&self, s: &Snapshot, info: &Info) -> Vec<Mount> {
        let mut options = vec![];
        if self.remap_ids {
            if let Some(v) = info.labels.get(UID_MAPPING) { options.push(format!("uidmap={v}")) }
            if let Some(v) = info.labels.get(GID_MAPPING) { options.push(format!("gidmap={v}")) }
        }
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
    let mut td = None;
    for _ in 0..10_000 {
        let t = snapshots.join(format!("new-{}", random_suffix()));
        match std::fs::DirBuilder::new().mode(0o700).create(&t) {
            Ok(()) => { td = Some(t); break }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err((None, Error::Other(format!("failed to create temp dir: {e}")))),
        }
    }
    let Some(td) = td else { return Err((None, Error::Other("failed to create temp dir: too many tries".into()))) };
    if let Err(e) = std::fs::DirBuilder::new().mode(0o755).create(td.join("fs")) { return Err((Some(td), e.into())) }
    if kind == Kind::Active {
        if let Err(e) = std::fs::DirBuilder::new().mode(0o711).create(td.join("work")) { return Err((Some(td), e.into())) }
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
    match std::fs::remove_dir_all(dir) { Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()), r => r }
}

/// Whether a folder is where something is mounted: on another device than its parent folder.
pub fn is_mount_point(p: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (std::fs::symlink_metadata(p), p.parent().map(std::fs::symlink_metadata)) {
        (Ok(a), Some(Ok(b))) => a.dev() != b.dev() || a.ino() == b.ino(),
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
    while let Some(p) = stack.pop() {
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
    let mut out = vec![];
    for m in label.split(',') {
        let bad = |why: &str| format!("input value {m} unparsable: {why}");
        let mut rest = m;
        let mut nums = [0i64; 3];
        for (k, n) in nums.iter_mut().enumerate() {
            if k > 0 { rest = rest.strip_prefix(':').ok_or_else(|| bad("input does not match format"))? }
            let t = rest.trim_start();
            let end = t.char_indices().find(|(i, c)| !(c.is_ascii_digit() || (*i == 0 && (*c == '-' || *c == '+')))).map(|(i, _)| i).unwrap_or(t.len());
            *n = t[..end].parse().map_err(|_| bad("expected integer"))?;
            rest = &t[end..];
        }
        let [c, h, l] = nums;
        let invalid = u32::MAX as i64;
        if c < 0 || c >= invalid || h < 0 || h >= invalid || l < 0 || l >= invalid { return Err(format!("invalid mapping \"{m}\"")) }
        out.push((c as u32, h as u32, l as u32));
    }
    Ok(Some(out))
}

/// The machine's id that a container's id is (containerd's `toHost`): one to one with no mapping.
fn to_host(id: u32, map: &Option<Vec<(u32, u32, u32)>>) -> std::result::Result<u32, String> {
    let Some(map) = map else { return Ok(id) };
    for &(c, h, n) in map {
        let Some(high) = c.checked_add(n) else { break };
        if id >= c && id < high {
            match h.checked_add(id - c) { Some(host) if host != u32::MAX => return Ok(host), _ => break }
        }
    }
    Err(format!("container ID {id} cannot be mapped to a host ID"))
}

/// Whether the folder's file system says each entry's type in a listing (overlay needs it; xfs made with ftype=0
/// does not).
fn supports_dtype(path: &Path) -> Result<bool> {
    use std::os::unix::ffi::OsStrExt;
    // (A folder with nothing in it: a file of its own while it is looked at.)
    let empty = std::fs::read_dir(path).and_then(crate::faults::pass)?.next().is_none();
    let dummy = path.join(format!(".superci-dtype-{}", random_suffix()));
    if empty { std::fs::write(&dummy, b"").and_then(crate::faults::pass)?; }
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).map_err(|e| Error::Other(e.to_string())).and_then(crate::faults::pass)?;
    let mut ok = true;
    unsafe {
        let d = libc::opendir(c.as_ptr());
        if d.is_null() { if empty { let _ = std::fs::remove_file(&dummy); } return Err(std::io::Error::last_os_error().into()) }
        loop {
            let e = libc::readdir(d);
            if e.is_null() { break }
            if (*e).d_type == libc::DT_UNKNOWN { ok = false; break }
        }
        libc::closedir(d);
    }
    if empty { let _ = std::fs::remove_file(&dummy); }
    Ok(ok)
}

/// Whether this runs in a user namespace (containerd's `RunningInUserNS`): its ids map other than one to one.
fn in_user_namespace() -> bool {
    match std::fs::read_to_string("/proc/self/uid_map") {
        Ok(m) => {
            let f: Vec<&str> = m.split_whitespace().collect();
            !(f.len() == 3 && f[0] == "0" && f[1] == "0" && f[2] == "4294967295")
        }
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
        let root = |m: &str| mappings(m).and_then(|m| to_host(0, &m));
        assert_eq!((root("0:100000:65536"), root("1:5:10,0:7:1"), root("")), (Ok(100000), Ok(7), Ok(0)), "no mapping: one to one");
        assert_eq!(root("1:5:10").unwrap_err(), "container ID 0 cannot be mapped to a host ID");
        assert!(root("a:b").unwrap_err().starts_with("input value a:b unparsable") && root("0:-1:5").unwrap_err() == "invalid mapping \"0:-1:5\"");
        let _ = std::fs::remove_dir_all(dir);
    }
}

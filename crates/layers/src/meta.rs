//! What snapshots there are, as containerd's snapshot metadata store keeps it (`core/snapshots/storage`): each
//! snapshot by its name or key, with a number of its own (its folder's name), its kind, its parent, labels and times,
//! and a committed one's disk usage; and each parent's children, so a snapshot with children is not removed. In a
//! redb file, each change in one transaction, as containerd's bolt store has them.

use crate::error::{db, Error, Result};
use crate::filters::Filters;
use redb::{ReadableDatabase, ReadableTable, TableDefinition};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};

const SNAPSHOTS: TableDefinition<&str, &[u8]> = TableDefinition::new("snapshots");
/// (parent's number, child's number) → the child's name.
const PARENTS: TableDefinition<(u64, u64), &str> = TableDefinition::new("parents");
const SEQUENCE: TableDefinition<&str, u64> = TableDefinition::new("sequence");

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Kind { View, Active, Committed }

impl Kind {
    /// The kind as containerd's API numbers it.
    pub fn proto(self) -> i32 { match self { Kind::View => 1, Kind::Active => 2, Kind::Committed => 3 } }
    fn word(self) -> &'static str { match self { Kind::View => "view", Kind::Active => "active", Kind::Committed => "committed" } }
}

/// A time as containerd keeps one: nanoseconds since 1970, UTC.
pub type Nanos = i64;

pub fn now() -> Nanos { std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as i64).unwrap_or(0) }

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Record {
    id: u64,
    kind: Kind,
    #[serde(default)] parent: String,
    #[serde(default)] labels: BTreeMap<String, String>,
    created: Nanos,
    updated: Nanos,
    #[serde(default)] inodes: i64,
    #[serde(default)] size: i64,
}

/// A snapshot as containerd's API says it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Info {
    pub name: String,
    pub parent: String,
    pub kind: Option<Kind>,
    pub labels: BTreeMap<String, String>,
    pub created: Nanos,
    pub updated: Nanos,
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Usage { pub size: i64, pub inodes: i64 }

/// An active or view snapshot, for its mounts: its number and its parents' numbers, nearest first.
#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot { pub id: String, pub kind: Kind, pub parent_ids: Vec<String> }

pub struct Meta { db: redb::Database }

impl Meta {
    pub fn open(path: &std::path::Path) -> Result<Meta> {
        let base = redb::Database::create(path).map_err(db)?;
        let tx = base.begin_write().map_err(db)?;
        {
            tx.open_table(SNAPSHOTS).map_err(db)?;
            tx.open_table(PARENTS).map_err(db)?;
            tx.open_table(SEQUENCE).map_err(db)?;
        }
        tx.commit().map_err(db)?;
        Ok(Meta { db: base })
    }

    /// A read of the store as it stands.
    pub fn read<T>(&self, f: impl FnOnce(&Read) -> Result<T>) -> Result<T> {
        let tx = self.db.begin_read().map_err(db)?;
        let r = Read { snapshots: tx.open_table(SNAPSHOTS).map_err(db)?, parents: tx.open_table(PARENTS).map_err(db)? };
        f(&r)
    }

    /// A change, kept only if `f` succeeds; one at a time.
    pub fn write<T>(&self, f: impl FnOnce(&mut Write) -> Result<T>) -> Result<T> {
        let tx = self.db.begin_write().map_err(db)?;
        let out = {
            let mut w = Write { snapshots: tx.open_table(SNAPSHOTS).map_err(db)?, parents: tx.open_table(PARENTS).map_err(db)?, sequence: tx.open_table(SEQUENCE).map_err(db)? };
            f(&mut w)
        };
        match out {
            Ok(v) => { tx.commit().map_err(db)?; Ok(v) }
            Err(e) => { let _ = tx.abort(); Err(e) }
        }
    }
}

pub struct Read { snapshots: redb::ReadOnlyTable<&'static str, &'static [u8]>, parents: redb::ReadOnlyTable<(u64, u64), &'static str> }
pub struct Write<'t> { snapshots: redb::Table<'t, &'static str, &'static [u8]>, parents: redb::Table<'t, (u64, u64), &'static str>, sequence: redb::Table<'t, &'static str, u64> }

fn get(t: &impl ReadableTable<&'static str, &'static [u8]>, key: &str) -> Result<Option<Record>> {
    match t.get(key).map_err(db)? {
        Some(v) => serde_json::from_slice(v.value()).map(Some).map_err(db),
        None => Ok(None),
    }
}

fn info_of(name: &str, r: &Record) -> Info {
    Info { name: name.to_string(), parent: r.parent.clone(), kind: Some(r.kind), labels: r.labels.clone(), created: r.created, updated: r.updated }
}

/// The parents' numbers, from `parent` (a record's parent name) up, nearest first.
fn parents(t: &impl ReadableTable<&'static str, &'static [u8]>, parent: &str) -> Result<Vec<String>> {
    let mut ids = vec![];
    let mut name = parent.to_string();
    loop {
        let Some(r) = get(t, &name)? else { return Err(Error::NotFound("missing parent".into())) };
        ids.push(r.id.to_string());
        if r.parent.is_empty() { return Ok(ids) }
        name = r.parent;
    }
}

/// What a filter reads of a snapshot (containerd's `adaptSnapshot`).
fn adapt(i: &Info) -> impl Fn(&[String]) -> Option<String> + '_ {
    move |path: &[String]| match path.first().map(String::as_str) {
        Some("kind") => i.kind.map(|k| k.word().to_string()),
        Some("name") => Some(i.name.clone()),
        Some("parent") => Some(i.parent.clone()),
        Some("labels") => if i.labels.is_empty() { None } else { i.labels.get(&path[1..].join(".")).cloned() },
        _ => None,
    }
}

macro_rules! reads {
    ($t:ty) => {
        impl $t {
            /// A snapshot's number, what it is and its usage (containerd's `GetInfo`).
            pub fn info(&self, key: &str) -> Result<(String, Info, Usage)> {
                let Some(r) = get(&self.snapshots, key)? else { return Err(Error::NotFound("snapshot does not exist".into())) };
                Ok((r.id.to_string(), info_of(key, &r), Usage { size: r.size, inodes: r.inodes }))
            }

            /// An active or view snapshot with its parents (containerd's `GetSnapshot`).
            pub fn snapshot(&self, key: &str) -> Result<Snapshot> {
                let Some(r) = get(&self.snapshots, key)? else { return Err(Error::NotFound("snapshot does not exist".into())) };
                if r.kind == Kind::Committed { return Err(Error::FailedPrecondition(format!("requested snapshot {key} not active or view"))) }
                let parent_ids = if r.parent.is_empty() { vec![] } else {
                    if get(&self.snapshots, &r.parent)?.is_none() { return Err(Error::NotFound("parent does not exist".into())) }
                    parents(&self.snapshots, &r.parent).map_err(|e| e.context("failed to get parent chain"))?
                };
                Ok(Snapshot { id: r.id.to_string(), kind: r.kind, parent_ids })
            }

            /// Each snapshot the filters match, by name (containerd's `WalkInfo`).
            pub fn walk(&self, filters: &[String], mut f: impl FnMut(Info) -> Result<()>) -> Result<()> {
                let filters = Filters::parse_all(filters).map_err(Error::InvalidArgument)?;
                for item in self.snapshots.iter().map_err(db)? {
                    let (k, v) = item.map_err(db)?;
                    let r: Record = serde_json::from_slice(v.value()).map_err(db)?;
                    let i = info_of(k.value(), &r);
                    if filters.matches(&adapt(&i)) { f(i)? }
                }
                Ok(())
            }

            /// Each snapshot's number, with its name (containerd's `IDMap`).
            pub fn ids(&self) -> Result<HashMap<String, String>> {
                let mut m = HashMap::new();
                for item in self.snapshots.iter().map_err(db)? {
                    let (k, v) = item.map_err(db)?;
                    let r: Record = serde_json::from_slice(v.value()).map_err(db)?;
                    m.insert(r.id.to_string(), k.value().to_string());
                }
                Ok(m)
            }

            #[allow(dead_code)]
            fn has_children(&self, id: u64) -> Result<bool> {
                Ok(self.parents.range((id, 0)..=(id, u64::MAX)).map_err(db)?.next().is_some())
            }
        }
    };
}
reads!(Read);
reads!(Write<'_>);

impl Write<'_> {
    fn put(&mut self, key: &str, r: &Record) -> Result<()> {
        let v = serde_json::to_vec(r).map_err(db)?;
        self.snapshots.insert(key, v.as_slice()).map_err(db)?;
        Ok(())
    }

    /// The next number (from 1, never given twice), as bolt's `NextSequence`.
    fn next_id(&mut self) -> Result<u64> {
        let n = self.sequence.get("snapshots").map_err(db)?.map(|v| v.value()).unwrap_or(0) + 1;
        self.sequence.insert("snapshots", n).map_err(db)?;
        Ok(n)
    }

    /// A new active or view snapshot on `parent` (committed), or none (containerd's `CreateSnapshot`).
    pub fn create(&mut self, kind: Kind, key: &str, parent: &str, mut labels: BTreeMap<String, String>) -> Result<Snapshot> {
        labels.retain(|_, v| !v.is_empty());
        if kind == Kind::Committed {
            return Err(Error::InvalidArgument(format!("snapshot type {} invalid; only snapshots of type Active or View can be created", kind.word())));
        }
        let parent_record = if parent.is_empty() { None } else {
            let Some(p) = get(&self.snapshots, parent)? else { return Err(Error::NotFound(format!("missing parent {parent:?} bucket"))) };
            if p.kind != Kind::Committed { return Err(Error::InvalidArgument(format!("parent {parent:?} is not committed snapshot"))) }
            Some(p)
        };
        if get(&self.snapshots, key)?.is_some() { return Err(Error::AlreadyExists(format!("snapshot {key}"))) }
        let id = self.next_id().map_err(|e| e.context(format!("unable to get identifier for snapshot {key:?}")))?;
        let t = now();
        self.put(key, &Record { id, kind, parent: parent.to_string(), labels, created: t, updated: t, inodes: 0, size: 0 })?;
        let mut parent_ids = vec![];
        if let Some(p) = parent_record {
            self.parents.insert((p.id, id), key).map_err(db)?;
            parent_ids = parents(&self.snapshots, parent).map_err(|e| e.context(format!("failed to get parent chain for snapshot {key:?}")))?;
        }
        Ok(Snapshot { id: id.to_string(), kind, parent_ids })
    }

    /// Takes a snapshot out, if it has no children: its number and kind (containerd's `Remove`).
    pub fn remove(&mut self, key: &str) -> Result<(String, Kind)> {
        let Some(r) = get(&self.snapshots, key)? else { return Err(Error::NotFound(format!("snapshot {key}"))) };
        if self.has_children(r.id)? { return Err(Error::FailedPrecondition("cannot remove snapshot with child".into())) }
        if !r.parent.is_empty() {
            let Some(p) = get(&self.snapshots, &r.parent)? else { return Err(Error::NotFound(format!("snapshot {key}"))) };
            self.parents.remove((p.id, r.id)).map_err(db)?;
        }
        self.snapshots.remove(key).map_err(db)?;
        Ok((r.id.to_string(), r.kind))
    }

    /// An active snapshot made committed under `name`, with these labels (not its own) and its usage; given a
    /// parent, one created with none takes it (containerd's `CommitActive`). Its number stays.
    pub fn commit(&mut self, key: &str, name: &str, usage: Usage, mut labels: BTreeMap<String, String>, parent: &str) -> Result<String> {
        labels.retain(|_, v| !v.is_empty());
        if get(&self.snapshots, name)?.is_some() { return Err(Error::AlreadyExists(String::new()).context(format!("committed snapshot {name}"))) }
        let Some(mut r) = get(&self.snapshots, key)? else { return Err(Error::NotFound(format!("failed to get active snapshot {key:?}"))) };
        if r.kind != Kind::Active { return Err(Error::FailedPrecondition(format!("snapshot {key:?} is not active"))) }
        r.kind = Kind::Committed;
        r.created = now();
        r.updated = r.created;
        r.labels = labels;
        if r.parent != parent {
            if r.parent.is_empty() { r.parent = parent.to_string() } else if !parent.is_empty() {
                return Err(Error::InvalidArgument(format!("cannot change parent of active snapshot {key:?} on commit")));
            }
        }
        (r.size, r.inodes) = (usage.size, usage.inodes);
        self.put(name, &r)?;
        self.snapshots.remove(key).map_err(db)?;
        if !r.parent.is_empty() {
            let Some(p) = get(&self.snapshots, &r.parent)? else { return Err(Error::NotFound(format!("missing parent {:?} of snapshot {key:?}", r.parent))) };
            if p.kind != Kind::Committed { return Err(Error::FailedPrecondition(format!("parent {:?} is not committed", r.parent))) }
            self.parents.insert((p.id, r.id), name).map_err(db)?;
        }
        Ok(r.id.to_string())
    }

    /// A snapshot's labels changed: all of them, or the `labels` and `labels.KEY` paths given; nothing else can be
    /// (containerd's `UpdateInfo`).
    pub fn update(&mut self, info: &Info, paths: &[String]) -> Result<Info> {
        let Some(mut r) = get(&self.snapshots, &info.name)? else { return Err(Error::NotFound("snapshot does not exist".into())) };
        if paths.is_empty() {
            r.labels = info.labels.clone();
        } else {
            for path in paths {
                if let Some(key) = path.strip_prefix("labels.") {
                    match info.labels.get(key) { Some(v) => { r.labels.insert(key.to_string(), v.clone()); } None => { r.labels.insert(key.to_string(), String::new()); } }
                    continue;
                }
                match path.as_str() {
                    "labels" => r.labels = info.labels.clone(),
                    _ => return Err(Error::InvalidArgument(format!("cannot update {path:?} field on snapshot {:?}", info.name))),
                }
            }
        }
        // (A label set to nothing is no label: containerd keeps no empty value.)
        r.labels.retain(|_, v| !v.is_empty());
        r.updated = now();
        self.put(&info.name, &r)?;
        Ok(info_of(&info.name, &r))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta() -> (Meta, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("superci-layers-meta-{}-{}", std::process::id(), now()));
        std::fs::create_dir_all(&dir).unwrap();
        (Meta::open(&dir.join("metadata.db")).unwrap(), dir)
    }
    fn labels(kv: &[(&str, &str)]) -> BTreeMap<String, String> { kv.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect() }

    #[test]
    fn snapshots_chain_commit_and_remove_as_containerds_store_does() {
        let (m, dir) = meta();
        // A base, committed; a child on it; the child's parents nearest first.
        let a = m.write(|w| w.create(Kind::Active, "a-key", "", labels(&[("x", "1")]))).unwrap();
        assert_eq!((a.id.as_str(), a.parent_ids.len()), ("1", 0));
        assert!(m.write(|w| w.create(Kind::Active, "b", "a-key", BTreeMap::new())).unwrap_err().to_string().contains("is not committed snapshot: invalid argument"));
        assert_eq!(m.write(|w| w.commit("a-key", "a", Usage { size: 10, inodes: 2 }, labels(&[("y", "2")]), "")).unwrap(), "1");
        let (id, info, usage) = m.read(|r| r.info("a")).unwrap();
        assert_eq!((id.as_str(), info.kind, info.labels, usage), ("1", Some(Kind::Committed), labels(&[("y", "2")]), Usage { size: 10, inodes: 2 }), "labels replaced on commit, usage kept");
        assert!(m.read(|r| r.info("a-key")).unwrap_err().is_not_found());
        m.write(|w| w.create(Kind::Active, "b-key", "a", BTreeMap::new())).unwrap();
        m.write(|w| w.commit("b-key", "b", Usage::default(), BTreeMap::new(), "")).unwrap();
        let c = m.write(|w| w.create(Kind::View, "c", "b", BTreeMap::new())).unwrap();
        assert_eq!(c.parent_ids, ["2", "1"], "the parents' numbers, nearest first (a create refused takes none)");
        assert_eq!(m.read(|r| r.snapshot("c")).unwrap(), c);
        assert!(matches!(m.read(|r| r.snapshot("a")), Err(Error::FailedPrecondition(_))));
        // Taken twice; a parent with children stays; children first.
        assert!(m.write(|w| w.create(Kind::Active, "c", "", BTreeMap::new())).unwrap_err().is_already_exists());
        assert_eq!(m.write(|w| w.remove("a")).unwrap_err(), Error::FailedPrecondition("cannot remove snapshot with child".into()));
        assert_eq!(m.write(|w| w.remove("c")).unwrap(), ("3".to_string(), Kind::View));
        m.write(|w| w.remove("b")).unwrap();
        m.write(|w| w.remove("a")).unwrap();
        assert!(m.read(|r| r.ids()).unwrap().is_empty());
        // A failed change leaves nothing.
        let _ = m.write(|w| { w.create(Kind::Active, "d", "", BTreeMap::new())?; Err::<(), _>(Error::Other("no".into())) });
        assert!(m.read(|r| r.info("d")).unwrap_err().is_not_found());
        assert_eq!(m.write(|w| w.create(Kind::Active, "e", "", BTreeMap::new())).unwrap().id, "4", "a failed change gives its number back, as bolt's sequence does");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn labels_update_by_path_and_walk_filters() {
        let (m, dir) = meta();
        m.write(|w| w.create(Kind::Active, "a", "", labels(&[("keep", "1"), ("drop", "2")]))).unwrap();
        m.write(|w| w.create(Kind::View, "b", "", BTreeMap::new())).unwrap();
        let mut i = m.read(|r| r.info("a")).unwrap().1;
        i.labels = labels(&[("new", "3")]);
        let got = m.write(|w| w.update(&i, &["labels.new".into(), "labels.drop".into()])).unwrap();
        assert_eq!(got.labels, labels(&[("keep", "1"), ("new", "3")]), "a path sets its label, an absent one takes it away");
        assert!(m.write(|w| w.update(&i, &["parent".into()])).unwrap_err().to_string().ends_with("invalid argument"));
        let names = |f: &[&str]| { let mut n = vec![]; m.read(|r| r.walk(&f.iter().map(|s| s.to_string()).collect::<Vec<_>>(), |i| { n.push(i.name); Ok(()) })).unwrap(); n };
        assert_eq!(names(&[]), ["a", "b"]);
        assert_eq!(names(&["kind==view"]), ["b"]);
        assert_eq!(names(&["labels.keep==1", "name==b"]), ["a", "b"]);
        assert_eq!(names(&["labels.keep"]), ["a"]);
        assert!(m.read(|r| r.walk(&["name==".into()], |_| Ok(()))).unwrap_err().to_string().ends_with("invalid argument"));
        let _ = std::fs::remove_dir_all(dir);
    }
}

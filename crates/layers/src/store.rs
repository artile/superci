//! A machine's layers: containerd's overlay snapshotter and content store (`overlay`, `content`), and the layers
//! SuperCI's store has served as they are instead of pulled and unpacked.
//!
//! When Docker unpacks a layer the store's index has, the layer's EROFS file is fetched (with the image's other
//! layers, at once), its snapshot made, the file mounted read-only on the snapshot's folder and the snapshot
//! committed as containerd names it, and containerd told it exists already: Docker neither downloads nor unpacks
//! it. Anything on the way that fails (a fetch, a mount) leaves the layer to be pulled as Docker pulls it. Its blob
//! is then known in the namespace that pulled it (a commit with nothing written, as containerd does with content it
//! has) and read from the store only if someone reads it (`docker save`, `docker push`), whole, checked against its
//! digest. A layer is present in the content store only once served, so a blob is never promised that Docker might
//! need before the store has given it.

use crate::client::Containerd;
use crate::content::{Digest, Store};
use crate::error::{db, Error, Result};
use crate::index::{Index, Layer};
use crate::meta::{Nanos, Usage};
use crate::overlay::{self, Mount, Overlay};
use redb::{ReadableDatabase, ReadableTable, TableDefinition};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

/// containerd's labels on a snapshot an image's layer is unpacked into: what it will be committed as, the layer's
/// blob, and the image's layers (as remote snapshotters are told them).
pub const REF: &str = "containerd.io/snapshot.ref";
pub const LAYER_DIGEST: &str = "containerd.io/snapshot/cri.layer-digest";
pub const IMAGE_LAYERS: &str = "containerd.io/snapshot/cri.image-layers";

/// Each snapshot served from the store, by its number: the layer and the file mounted on its folder.
const MOUNTS: TableDefinition<&str, &[u8]> = TableDefinition::new("mounts");

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Served { digest: String, file: String, size: u64 }

pub struct Layers {
    pub overlay: Overlay,
    pub content: Store,
    pub store: StoreSide,
}

/// What serving from the store takes.
pub struct StoreSide {
    pub index: RwLock<Index>,
    /// Where the EROFS files are kept (the store's own disk when it has one).
    pub files: PathBuf,
    db: redb::Database,
    served: Mutex<HashMap<String, Served>>,
    erofs: Mutex<HashMap<String, Arc<tokio::sync::OnceCell<std::result::Result<PathBuf, String>>>>>,
    blobs: Mutex<HashMap<String, Arc<tokio::sync::OnceCell<std::result::Result<(), String>>>>>,
    targets: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    at_once: tokio::sync::Semaphore,
    /// Ranges of one file fetched at once.
    pub parallel: usize,
    agent: ureq::Agent,
    containerd: Containerd,
    started: Nanos,
    pub stats: Stats,
}

#[derive(Default)]
pub struct Stats { pub served: AtomicU64, pub pulled: AtomicU64, pub fell_back: AtomicU64, pub bytes: AtomicU64, pub blobs_read: AtomicU64 }

pub struct Options { pub files: PathBuf, pub db: PathBuf, pub index: Index, pub containerd: PathBuf, pub parallel: usize, pub layers_at_once: usize }

/// Work on files and the database off the threads that answer calls: what it gives (a panic in it is passed on as
/// one: only a step's own bug makes it).
pub async fn off_thread<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T { tokio::task::spawn_blocking(f).await.expect("a step off the answering threads") }

impl Layers {
    /// The layers as they were left: each snapshot served from the store mounted again if it is not (a restart
    /// of this service leaves them mounted; a restart of the machine does not), files no snapshot uses taken away.
    pub fn open(overlay: Overlay, content: Store, o: Options) -> Result<Layers> {
        std::fs::create_dir_all(&o.files).and_then(crate::faults::pass)?;
        let database = redb::Database::create(&o.db).and_then(crate::faults::pass).map_err(db)?;
        let tx = database.begin_write().map_err(db).and_then(crate::faults::pass)?;
        tx.open_table(MOUNTS).map_err(db).and_then(crate::faults::pass)?;
        tx.commit().map_err(db).and_then(crate::faults::pass)?;
        let side = StoreSide {
            index: RwLock::new(o.index), files: o.files, db: database, served: Mutex::new(HashMap::new()),
            erofs: Mutex::new(HashMap::new()), blobs: Mutex::new(HashMap::new()), targets: Mutex::new(HashMap::new()),
            at_once: tokio::sync::Semaphore::new(o.layers_at_once.max(1)), parallel: o.parallel.max(1), agent: crate::fetch::agent(),
            containerd: Containerd::new(o.containerd), started: crate::meta::now(), stats: Stats::default(),

        };
        let l = Layers { overlay, content, store: side };
        l.recover().and_then(crate::faults::pass)?;
        Ok(l)
    }

    fn recover(&self) -> Result<()> {
        let st = &self.store;
        let ids = self.overlay.meta.read(|r| r.ids()).and_then(crate::faults::pass)?;
        let tx = st.db.begin_read().map_err(db).and_then(crate::faults::pass)?;
        let t = tx.open_table(MOUNTS).map_err(db).and_then(crate::faults::pass)?;
        let mut all = vec![];
        for item in t.iter().and_then(crate::faults::pass).map_err(db)? {
            let (k, v) = item.and_then(crate::faults::pass).map_err(db)?;
            all.push((k.value().to_string(), serde_json::from_slice::<Served>(v.value()).map_err(db).and_then(crate::faults::pass)?));
        }
        drop(t);
        drop(tx);
        for (id, s) in all {
            if !ids.contains_key(&id) { st.forget(&id).and_then(crate::faults::pass)?; continue }
            let upper = self.overlay.upper(&id);
            if !overlay::is_mount_point(&upper) {
                let file = PathBuf::from(&s.file);
                if !file.exists() {
                    // (The file is gone: fetched again, if the index still has it.)
                    let layer = st.index.read().unwrap().get(&s.digest).cloned();
                    match layer {
                        Some(layer) => if let Err(e) = st.fetch_erofs_blocking(&s.digest, &layer, &file) { eprintln!("superci-layers: snapshot {id} (layer {}) could not be mounted again: {e}", s.digest); continue },
                        None => { eprintln!("superci-layers: snapshot {id} (layer {}) could not be mounted again: its file is gone and the index has no such layer", s.digest); continue }
                    }
                }
                if let Err(e) = crate::mount::mount_erofs(&file, &upper) { eprintln!("superci-layers: snapshot {id} (layer {}) could not be mounted again: {e}", s.digest); continue }
            }
            st.served.lock().unwrap().insert(id, s);
        }
        st.prune_files();
        Ok(())
    }

    /// A new active snapshot; for a layer the store has, the layer served, and containerd told it exists.
    pub async fn prepare(self: Arc<Self>, key: String, parent: String, labels: BTreeMap<String, String>) -> Result<Vec<Mount>> {
        if let Some(served) = self.clone().serve(&key, &parent, &labels).await { return Err(served) }
        let l = self.clone();
        crate::faults::check(off_thread(move || l.overlay.prepare(&key, &parent, labels)).await)
    }

    /// The layer served from the store, or `None` to make the snapshot as containerd's own does.
    async fn serve(self: Arc<Self>, key: &str, parent: &str, labels: &BTreeMap<String, String>) -> Option<Error> {
        let st = &self.store;
        let (target, digest) = (labels.get(REF)?, labels.get(LAYER_DIGEST)?);
        let Some(layer) = st.index.read().unwrap().get(digest).cloned() else {
            st.stats.pulled.fetch_add(1, Ordering::Relaxed);
            return None
        };
        // (Two pulls of one layer at once: one serves it, the other finds it served.)
        let lock = st.targets.lock().unwrap().entry(format!("{parent}\n{target}")).or_default().clone();
        let _one = lock.lock().await;
        let (l, t, p) = (self.clone(), target.clone(), parent.to_string());
        let found = off_thread(move || {
            let mut found = false;
            let filter = format!("labels.{REF:?}=={t:?},parent=={p:?},kind==committed");
            l.overlay.walk(&[filter], |_| { found = true; Ok(()) }).map(|_| found)
        }).await.ok()?;
        if found { return Some(Error::AlreadyExists(format!("target snapshot {target:?}"))) }
        // (Links for the image's layers the index has, asked for at once: the first of its layers to come.)
        let image: Vec<String> = labels.get(IMAGE_LAYERS).map(|all| all.split(',').filter(|d| !d.is_empty()).map(str::to_string).collect()).unwrap_or_default();
        let unlinked: Vec<String> = { let i = st.index.read().unwrap(); image.iter().chain([digest]).filter(|d| i.get(d).is_some_and(|l| l.erofs.url.is_empty())).cloned().collect() };
        if !unlinked.is_empty() { let l = self.clone(); off_thread(move || l.store.refresh_many(&unlinked)).await; }
        let layer = st.index.read().unwrap().get(digest).cloned().unwrap_or(layer);
        if let Some(all) = labels.get(IMAGE_LAYERS) {
            for d in all.split(',').filter(|d| !d.is_empty() && *d != digest) {
                let layer = st.index.read().unwrap().get(d).cloned();
                if let Some(layer) = layer { let (l, d) = (self.clone(), d.to_string()); tokio::spawn(async move { let _ = l.erofs(&d, &layer).await; }); }
            }
        }
        let began = std::time::Instant::now();
        let file = match self.clone().erofs(digest, &layer).await {
            Ok(f) => f,
            Err(e) => { st.stats.fell_back.fetch_add(1, Ordering::Relaxed); eprintln!("superci-layers: {digest} pulled as usual: {e}"); return None }
        };
        let (l, k, p, lb, d, f, size) = (self.clone(), key.to_string(), parent.to_string(), labels.clone(), digest.clone(), file.clone(), layer.size);
        let usage = layer.usage.map(|u| Usage { size: u.size, inodes: u.inodes });
        let made = off_thread(move || l.serve_snapshot(&k, &p, lb, &d, &f, size, usage)).await;
        match made {
            Ok(()) => {
                st.stats.served.fetch_add(1, Ordering::Relaxed);
                eprintln!("superci-layers: {digest} from the store in {:.2} s", began.elapsed().as_secs_f64());
                // (Made known in its namespace beside this answer, never before it: containerd may be waiting on
                // this answer while holding what that needs.)
                if let Some(ns) = key.split('/').next().filter(|n| !n.is_empty()) {
                    let (l, ns, d, size) = (self.clone(), ns.to_string(), digest.clone(), layer.size as i64);
                    tokio::spawn(async move {
                        if let Err(e) = l.store.containerd.register(&ns, &d, size).await { eprintln!("superci-layers: {d} not made known in {ns}: {e}") }
                    });
                }
                Some(Error::AlreadyExists(format!("target snapshot {target:?}")))
            }
            Err(e) => { st.stats.fell_back.fetch_add(1, Ordering::Relaxed); eprintln!("superci-layers: {digest} pulled as usual: {e}"); None }
        }
    }

    /// The snapshot of a served layer: made active, its file mounted on its folder, committed; undone on failure.
    #[allow(clippy::too_many_arguments)]
    fn serve_snapshot(&self, key: &str, parent: &str, labels: BTreeMap<String, String>, digest: &str, file: &std::path::Path, size: u64, usage: Option<Usage>) -> Result<()> {
        let st = &self.store;
        self.overlay.prepare(key, parent, labels.clone()).and_then(crate::faults::pass)?;
        let (id, _, _) = self.overlay.meta.read(|r| r.info(key)).and_then(crate::faults::pass)?;
        let upper = self.overlay.upper(&id);
        let undo = |mounted: bool| {
            if mounted { let _ = crate::mount::unmount(&upper); }
            let _ = st.forget(&id);
            let _ = self.overlay.remove(key);
            let _ = overlay::remove_snapshot_dir(&self.overlay.root.join("snapshots").join(&id));
        };
        if let Err(e) = crate::mount::mount_erofs(file, &upper) { undo(false); return Err(Error::Other(format!("mounting {}: {e}", file.display()))) }
        let served = Served { digest: digest.to_string(), file: file.to_string_lossy().into_owned(), size };
        if let Err(e) = st.remember(&id, &served) { undo(true); return Err(e) }
        let usage = match usage { Some(u) => Some(u), None => overlay::disk_usage(&upper).ok() };
        if let Err(e) = self.overlay.commit_with(&format!("{key}/superci-layers"), key, labels, parent, usage) { undo(true); return Err(e) }
        Ok(())
    }

    pub fn remove(&self, key: &str) -> Result<()> {
        self.overlay.remove(key).and_then(crate::faults::pass)?;
        self.forget_removed()
    }

    pub fn cleanup(&self) -> Result<()> {
        self.overlay.cleanup().and_then(crate::faults::pass)?;
        self.forget_removed()
    }

    /// Served snapshots no longer in the store forgotten (their blobs no longer present), their files taken away
    /// (a file still mounted stays readable to the kernel until its folder is unmounted).
    fn forget_removed(&self) -> Result<()> {
        let st = &self.store;
        let ids = self.overlay.meta.read(|r| r.ids()).and_then(crate::faults::pass)?;
        let gone: Vec<String> = st.served.lock().unwrap().keys().filter(|id| !ids.contains_key(*id)).cloned().collect();
        for id in gone { st.forget(&id).and_then(crate::faults::pass)? }
        st.prune_files();
        Ok(())
    }

    /// A blob's info: the content store's, or a served layer's.
    pub fn content_info(&self, d: &Digest) -> Result<crate::content::Info> {
        match self.content.info(d) {
            Err(e) if e.is_not_found() => match self.store.served_size(&d.to_string()) {
                Some(size) => { let t = self.store.started; Ok(crate::content::Info { digest: d.to_string(), size: size as i64, created: t, updated: t, labels: BTreeMap::new() }) }
                None => Err(e),
            },
            r => r,
        }
    }

    /// A blob to read, and its size: a served layer's blob fetched whole from the store first.
    pub async fn open_blob(self: Arc<Self>, d: Digest) -> Result<(std::fs::File, u64)> {
        let l = self.clone();
        let dd = d.clone();
        match crate::faults::check(off_thread(move || l.content.open_blob(&dd)).await) {
            Err(e) if e.is_not_found() => {
                let st = &self.store;
                if st.served_size(&d.to_string()).is_none() { return Err(e) }
                self.clone().fetch_blob(&d).await.map_err(Error::Other).and_then(crate::faults::pass)?;
                let l = self.clone();
                crate::faults::check(off_thread(move || l.content.open_blob(&d)).await)
            }
            r => r,
        }
    }

    async fn fetch_blob(self: Arc<Self>, d: &Digest) -> std::result::Result<(), String> {
        let st = &self.store;
        let key = d.to_string();
        let cell = st.blobs.lock().unwrap().entry(key.clone()).or_default().clone();
        let l = self.clone();
        let r = cell.get_or_init(|| async move {
            let st = &l.store;
            let size = st.served_size(&key).ok_or("not served")?;
            // (The semaphore is never closed.)
            let _turn = st.at_once.acquire().await.expect("a turn");
            let (l2, key2, d) = (l.clone(), key.clone(), d.clone());
            off_thread(move || -> std::result::Result<(), String> {
                let st = &l2.store;
                let tmp = l2.content.fetch_dir().map_err(|e| e.to_string())?.join(&d.hex);
                let link = IndexLink { st, digest: key2.clone(), erofs: false };
                let began = std::time::Instant::now();
                let n = crate::fetch::fetch(&st.agent, &link, size, &tmp, st.parallel, None)?;
                l2.content.put_file(&d, &tmp).map_err(|e| e.to_string())?;
                st.stats.blobs_read.fetch_add(1, Ordering::Relaxed);
                st.stats.bytes.fetch_add(n, Ordering::Relaxed);
                eprintln!("superci-layers: blob {key2} read from the store in {:.2} s", began.elapsed().as_secs_f64());
                Ok(())
            }).await
        }).await.clone();
        // (A failed read may be asked again: the next reader tries afresh.)
        if r.is_err() { st.blobs.lock().unwrap().remove(&d.to_string()); }
        r
    }

    /// A layer's EROFS file on this machine's disk, fetched once (each layer once, however many ask, a few at once).
    async fn erofs(self: Arc<Self>, digest: &str, layer: &Layer) -> std::result::Result<PathBuf, String> {
        let st = &self.store;
        let cell = st.erofs.lock().unwrap().entry(digest.to_string()).or_default().clone();
        let l = self.clone();
        cell.get_or_init(|| async move {
            let st = &l.store;
            let file = st.files.join(format!("{}.erofs", digest.split_once(':').map(|(_, h)| h).unwrap_or(digest)));
            if std::fs::metadata(&file).is_ok_and(|m| m.len() == layer.erofs.size) { return Ok(file) }
            // (The semaphore is never closed.)
            let _turn = st.at_once.acquire().await.expect("a turn");
            let (l2, d, layer, f) = (l.clone(), digest.to_string(), layer.clone(), file.clone());
            off_thread(move || l2.store.fetch_erofs_blocking(&d, &layer, &f)).await?;
            Ok(file)
        }).await.clone()
    }

    /// The files of the layers the job's last run used (`ahead`), fetched now, a few at once, before Docker asks for
    /// them: its pull then finds them here (one that asks meanwhile waits for the same fetch).
    pub fn fetch_ahead(self: &Arc<Self>) {
        let st = &self.store;
        let ahead: Vec<(String, Layer)> = st.index.read().unwrap().layers.iter().filter(|(_, l)| l.ahead).map(|(d, l)| (d.clone(), l.clone())).collect();
        if ahead.is_empty() { return }
        let (n, bytes) = (ahead.len(), ahead.iter().map(|(_, l)| l.erofs.size).sum::<u64>());
        let began = std::time::Instant::now();
        let left = Arc::new(std::sync::atomic::AtomicUsize::new(n));
        for (digest, layer) in ahead {
            let (me, left) = (self.clone(), left.clone());
            tokio::spawn(async move {
                if let Err(e) = me.erofs(&digest, &layer).await { eprintln!("superci-layers: {digest} not fetched ahead: {e}") }
                if left.fetch_sub(1, Ordering::Relaxed) == 1 { eprintln!("superci-layers: {n} layers' files ({:.0} MB) fetched ahead in {:.2} s", bytes as f64 / 1e6, began.elapsed().as_secs_f64()) }
            });
        }
    }

    /// The layers SuperCI's store has, read again (the machine's start writes it, then says so with SIGHUP).
    pub fn reload(&self, index: Index) {
        *self.store.index.write().unwrap() = index
    }
}

impl StoreSide {
    fn served_size(&self, digest: &str) -> Option<u64> { self.served.lock().unwrap().values().find(|s| s.digest == digest).map(|s| s.size) }

    fn remember(&self, id: &str, s: &Served) -> Result<()> {
        let tx = self.db.begin_write().map_err(db).and_then(crate::faults::pass)?;
        { tx.open_table(MOUNTS).map_err(db).and_then(crate::faults::pass)?.insert(id, serde_json::to_vec(s).map_err(db).and_then(crate::faults::pass)?.as_slice()).map_err(db).and_then(crate::faults::pass)?; }
        tx.commit().map_err(db).and_then(crate::faults::pass)?;
        self.served.lock().unwrap().insert(id.to_string(), s.clone());
        Ok(())
    }

    fn forget(&self, id: &str) -> Result<()> {
        let tx = self.db.begin_write().map_err(db).and_then(crate::faults::pass)?;
        { tx.open_table(MOUNTS).map_err(db).and_then(crate::faults::pass)?.remove(id).map_err(db).and_then(crate::faults::pass)?; }
        tx.commit().map_err(db).and_then(crate::faults::pass)?;
        let mut served = self.served.lock().unwrap();
        if let Some(s) = served.remove(id) {
            // (Its file kept no longer once no snapshot uses its layer.)
            if !served.values().any(|o| o.digest == s.digest) { self.erofs.lock().unwrap().remove(&s.digest); }
        }
        Ok(())
    }

    /// Files of layers neither served nor fetched in this run (being fetched, or fetched ahead for an image being
    /// pulled), taken away: what a removed image left, and what a run before left half-fetched.
    fn prune_files(&self) {
        let used: std::collections::HashSet<String> = self.served.lock().unwrap().values().map(|s| s.digest.clone()).collect();
        let kept: std::collections::HashSet<String> = self.erofs.lock().unwrap().keys().cloned().collect();
        let Ok(entries) = std::fs::read_dir(&self.files) else { return };
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let hex = name.split('.').next().unwrap_or_default();
            let d = format!("sha256:{hex}");
            if !used.contains(&d) && !kept.contains(&d) { let _ = std::fs::remove_file(e.path()); }
        }
    }

    fn fetch_erofs_blocking(&self, digest: &str, layer: &Layer, file: &std::path::Path) -> std::result::Result<(), String> {
        let tmp = file.with_extension("erofs.part");
        let link = IndexLink { st: self, digest: digest.to_string(), erofs: true };
        let began = std::time::Instant::now();
        let n = crate::fetch::fetch(&self.agent, &link, layer.erofs.size, &tmp, self.parallel, layer.erofs.digest.as_deref())?;
        std::fs::rename(&tmp, file).map_err(|e| e.to_string())?;
        self.stats.bytes.fetch_add(n, Ordering::Relaxed);
        eprintln!("superci-layers: {digest}'s file ({:.1} MB) fetched in {:.2} s", n as f64 / 1e6, began.elapsed().as_secs_f64());
        Ok(())
    }

    /// New links for a layer, from the control plane (`index.refresh`).
    fn refresh(&self, digest: &str) -> bool { self.refresh_many(&[digest.to_string()]) }

    /// The same for several at once: whether the first came.
    fn refresh_many(&self, digests: &[String]) -> bool {
        let Some(r) = self.index.read().unwrap().refresh.clone() else { return false };
        let began = std::time::Instant::now();
        let _said = Said(began, digests.len());
        let body = serde_json::json!({ "digests": digests }).to_string();
        let res = self.agent.post(&r.url).header("authorization", format!("Bearer {}", r.token)).header("content-type", "application/json").send(body.as_bytes());
        let Ok(res) = res else { return false };
        if res.status().as_u16() != 200 { return false }
        let Ok(text) = res.into_body().read_to_string() else { return false };
        let Ok(fresh) = serde_json::from_str::<Index>(&text) else { return false };
        let mut index = self.index.write().unwrap();
        for (d, layer) in fresh.layers { if digests.contains(&d) { index.layers.insert(d, layer); } }
        digests.first().is_some_and(|d| index.layers.get(d).is_some_and(|l| !l.erofs.url.is_empty()))
    }

}

/// Says how long asking for links took, however the asking ends.
struct Said(std::time::Instant, usize);
impl Drop for Said { fn drop(&mut self) { eprintln!("superci-layers: links for {} layer(s) asked for in {:.2} s", self.1, self.0.elapsed().as_secs_f64()) } }

/// A layer's file (its EROFS file or its blob) as the index links it, signed again when refused.
struct IndexLink<'a> { st: &'a StoreSide, digest: String, erofs: bool }

impl crate::fetch::Link for IndexLink<'_> {
    fn url(&self) -> String {
        let read = || { let i = self.st.index.read().unwrap(); i.get(&self.digest).map(|l| if self.erofs { l.erofs.url.clone() } else { l.blob.url.clone() }).unwrap_or_default() };
        // (No link yet: asked for.)
        match read() { u if u.is_empty() && self.st.refresh(&self.digest) => read(), u => u }
    }
    fn refresh(&self) -> bool { self.st.refresh(&self.digest) }
}


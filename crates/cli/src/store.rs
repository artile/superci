//! What SuperCI keeps on this computer: its own sign-ins to your clouds, in a folder of its own (`~/.superci`, or
//! `SUPERCI_HOME`), so that the dashboard opens signed in and commands run without a browser. They are kept by
//! workspace (`workspaces/<name>/sign-ins.json`): each workspace is a world of its own (one company's sign-ins,
//! and through them its control plane and everything of it), and nothing of one is seen from another. Nothing of another
//! tool's is read (no AWS profile, no wrangler or Modal login) and nothing is written anywhere else. `superci logout`
//! removes it.
//!
//! A sign-in here is as strong as the sign-in itself: the file is the owner's alone (0600 in a 0700 folder).
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::plane::Plane;
use crate::{aws, cloudflare, modal};

/// What is kept: each cloud's sign-in, the key the control planes were handed (with the name it is stored under
/// there, which says when it expires) and which of them have it, and the control plane in use.
#[derive(Serialize, Deserialize, Default)]
pub struct Kept {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cloudflare: Option<cloudflare::Session>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aws: Option<aws::Session>,
    /// Further AWS accounts signed in to (a second sign-in to AWS adds an account; it replaces none).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aws_more: Vec<aws::Session>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modal: Option<modal::Session>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<Key>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plane: Option<Plane>,
    /// The other control planes known, so that each is listed at once, also one whose account's sign-in has ended.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub planes_more: Vec<Plane>,
    /// The AWS sign-in kept here was ended by AWS (it ends one after twelve hours at most) and has not been made
    /// again: said where it matters, and the key and the control plane stay, so looking goes on working.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub aws_ended: bool,
    /// What the workspace's CI was last seen to be called and whose jobs it runs: what a list of workspaces says of
    /// each without asking its control plane (and without using its sign-ins from another workspace's page).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seen: Option<Seen>,
}

#[derive(Serialize, Deserialize, Clone, Default, PartialEq, Debug)]
pub struct Seen {
    pub called: String,
    /// Each code host connected: its kind (`github`, `gitlab`) and its name (an organization, a GitLab's address).
    pub hosts: Vec<(String, String)>,
}

#[derive(Serialize, Deserialize, Clone, PartialEq)]
pub struct Key { pub name: String, pub value: String, pub planes: Vec<String> }

impl Kept {
    pub fn signed_in(&self) -> bool { self.cloudflare.is_some() || self.aws.is_some() || !self.aws_more.is_empty() || self.modal.is_some() }
    /// Worth keeping: a sign-in, or what is left of one AWS ended (the control plane and the key it is read with).
    pub fn worth_keeping(&self) -> bool { self.signed_in() || (self.aws_ended && self.key.is_some() && self.plane.is_some()) }
}

#[derive(Clone)]
pub struct Store { dir: PathBuf }

/// What the first workspace is called until it is given a name.
pub const FIRST: &str = "default";

/// SuperCI's folder: `SUPERCI_HOME`, or `.superci` in the home folder. None when there is no home folder.
pub fn root() -> Option<PathBuf> {
    let var = |n: &str| std::env::var_os(n).filter(|v| !v.is_empty()).map(PathBuf::from);
    var("SUPERCI_HOME").or_else(|| var("HOME").or_else(|| var("USERPROFILE")).map(|h| h.join(".superci")))
}

/// Whether a workspace can be called this: 2 to 24 lowercase letters, digits and dashes, starting with a letter (it
/// is a folder's name, and what `--workspace` takes).
pub fn valid_name(name: &str) -> crate::Result<()> {
    let ok = (2..=24).contains(&name.len()) && name.starts_with(|c: char| c.is_ascii_lowercase()) && name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    if ok { Ok(()) } else { Err("A workspace's name is 2 to 24 lowercase letters, digits and dashes, starting with a letter (acme, acme-staging).".into()) }
}

/// The workspaces kept under a folder, by name, in order.
pub fn workspaces_in(root: &std::path::Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(root.join("workspaces")).into_iter().flatten().flatten()
        .filter(|e| e.path().is_dir()).filter_map(|e| e.file_name().into_string().ok()).filter(|n| valid_name(n).is_ok()).collect();
    names.sort();
    names
}

/// A workspace in a line of a list: read from what it keeps, nothing asked of any cloud.
pub struct Listed { pub name: String, pub called: String, pub hosts: Vec<(String, String)>, pub place: String, pub signed_in: bool }

/// The workspaces kept under a folder, with `also` among them (the one a page is of, which has no folder until it
/// keeps something).
pub fn listed(root: &std::path::Path, also: Option<&str>) -> Vec<Listed> {
    let mut names = workspaces_in(root);
    if let Some(a) = also.filter(|a| !names.iter().any(|n| n == a)) { names.push(a.to_string()); names.sort() }
    names.into_iter().map(|name| {
        let kept = Store::under(root, &name).read();
        let seen = kept.seen.clone().unwrap_or_default();
        Listed { called: seen.called, hosts: seen.hosts, place: kept.plane.as_ref().map(|p| p.place()).unwrap_or_default(), signed_in: kept.signed_in(), name }
    }).collect()
}

/// A workspace under a new name: its folder is renamed. The new name must be free.
pub fn rename(root: &std::path::Path, from: &str, to: &str) -> crate::Result<()> {
    valid_name(to)?;
    if from == to { return Ok(()) }
    let (old, new) = (root.join("workspaces").join(from), root.join("workspaces").join(to));
    if new.exists() { return Err(format!("There is a workspace named {to} already.")) }
    if !old.is_dir() { return Store::under(root, to).make() }
    std::fs::rename(&old, &new).map_err(|e| format!("could not rename {}: {e}", old.display()))
}

/// Which workspace is meant, of those kept under `root`: the one named; with none named, the only one, or the first
/// one when there is none yet. With several and none named, nothing picks: the names are given back, for whoever
/// asked to say which.
pub fn which(root: &std::path::Path, named: Option<&str>) -> std::result::Result<String, Vec<String>> {
    let all = workspaces_in(root);
    match named.map(|n| n.trim().to_lowercase()).filter(|n| !n.is_empty()) {
        Some(n) => Ok(n),
        None => match all.as_slice() { [] => Ok(FIRST.to_string()), [only] => Ok(only.clone()), _ => Err(all) },
    }
}

/// Whether a workspace is kept under `root` (the first one is there before it keeps anything, when no other is).
pub fn kept_here(root: &std::path::Path, name: &str) -> bool {
    let all = workspaces_in(root);
    all.iter().any(|w| w == name) || (all.is_empty() && name == FIRST)
}

impl Store {
    /// Where one workspace's sign-ins are kept under SuperCI's folder. None when there is no home folder.
    pub fn of(workspace: &str) -> Option<Store> { Some(Store::under(&root()?, workspace)) }

    pub fn under(root: &std::path::Path, workspace: &str) -> Store { Store { dir: root.join("workspaces").join(workspace) } }

    /// The workspace's name: its folder's.
    pub fn workspace(&self) -> String { self.dir.file_name().and_then(|n| n.to_str()).unwrap_or(FIRST).to_string() }

    /// Makes the workspace's folder (a new workspace is listed before anything is signed in to).
    pub fn make(&self) -> crate::Result<()> {
        std::fs::create_dir_all(&self.dir).map_err(|e| format!("could not make {}: {e}", self.dir.display()))?;
        self.owners_alone();
        Ok(())
    }

    /// The workspace's folder and the two above it (SuperCI's, and the one that holds the workspaces) are the
    /// owner's alone.
    fn owners_alone(&self) {
        only_the_owner(&self.dir, 0o700);
        let Some(all) = self.dir.parent().filter(|p| p.file_name().is_some_and(|n| n == "workspaces")) else { return };
        only_the_owner(all, 0o700);
        if let Some(root) = all.parent() { only_the_owner(root, 0o700) }
    }

    /// Forgets the workspace on this computer: its sign-ins and its folder. Nothing in any cloud is touched.
    pub fn forget(&self) -> crate::Result<()> {
        match std::fs::remove_dir_all(&self.dir) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(format!("could not remove {}: {e}", self.dir.display())),
        }
    }

    #[cfg(test)]
    pub fn at(dir: impl Into<PathBuf>) -> Store { Store { dir: dir.into() } }

    /// SuperCI's folder, which this workspace's is in.
    pub fn root(&self) -> Option<PathBuf> { Some(self.dir.parent()?.parent()?.to_path_buf()) }

    pub fn path(&self) -> PathBuf { self.dir.join("sign-ins.json") }

    /// What is kept (nothing, when there is no file or it cannot be read as this version writes it).
    pub fn read(&self) -> Kept {
        std::fs::read(self.path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    /// Writes what is kept: to a file beside it first, then moved over, so a stop midway leaves the last one whole.
    pub fn write(&self, kept: &Kept) -> crate::Result<()> {
        let say = |e: std::io::Error| format!("could not keep the sign-in in {}: {e}", self.dir.display());
        std::fs::create_dir_all(&self.dir).map_err(say)?;
        self.owners_alone();
        let tmp = self.dir.join(format!("sign-ins.json.{}", std::process::id()));
        let text = serde_json::to_vec_pretty(kept).map_err(|e| e.to_string())?;
        write_private(&tmp, &text).map_err(say)?;
        std::fs::rename(&tmp, self.path()).map_err(say)
    }

    /// Removes what is kept. Whether there was anything.
    pub fn remove(&self) -> crate::Result<bool> {
        match std::fs::remove_file(self.path()) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(format!("could not remove {}: {e}", self.path().display())),
        }
    }
}

#[cfg(unix)]
fn write_private(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(path)?;
    f.write_all(bytes)
}

/// On Windows a file in the user's own folder is the user's (and the administrators') by what it inherits.
#[cfg(not(unix))]
fn write_private(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> { std::fs::write(path, bytes) }

#[cfg(unix)]
fn only_the_owner(path: &std::path::Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode));
}

#[cfg(not(unix))]
fn only_the_owner(_: &std::path::Path, _: u32) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_and_removes() {
        let dir = std::env::temp_dir().join(format!("superci-store-{}-{}", std::process::id(), superci_core::crypto::random_id(6)));
        let store = Store::at(&dir);
        assert!(!store.read().signed_in() && !store.remove().unwrap());
        let kept = Kept { modal: Some(modal::Session { token_id: "ak-test".into(), token_secret: "as-test".into(), workspace: "acme".into() }),
            aws: Some(aws::Session::for_test("123456789012")),
            key: Some(Key { name: "DASHBOARD_KEY_1_AB".into(), value: "k".into(), planes: vec!["p1".into()] }),
            plane: Some(Plane::Aws { account_id: "123456789012".into(), region: "us-east-1".into(), url: "https://x.lambda-url.us-east-1.on.aws".into(), plane_id: "p1".into(), label: "superci".into() }), ..Default::default() };
        store.write(&kept).unwrap();
        let back = store.read();
        assert!(back.signed_in() && back.cloudflare.is_none());
        assert_eq!(back.modal.unwrap().workspace, "acme");
        assert_eq!(back.aws.unwrap().account_id, "123456789012");
        assert!(back.key == kept.key && back.plane == kept.plane);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(store.path()).unwrap().permissions().mode() & 0o777, 0o600);
            assert_eq!(std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700);
        }
        // A file this version cannot read is nothing kept, not an error.
        std::fs::write(store.path(), b"{ not json").unwrap();
        assert!(!store.read().signed_in() && !store.read().worth_keeping());
        assert!(store.remove().unwrap());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn workspaces_are_folders_and_none_is_picked_among_several() {
        let root = std::env::temp_dir().join(format!("superci-workspaces-{}-{}", std::process::id(), superci_core::crypto::random_id(6)));
        // None yet: the first one, which is there before it keeps anything.
        assert!(workspaces_in(&root).is_empty() && which(&root, None) == Ok("default".into()) && kept_here(&root, "default") && !kept_here(&root, "acme"));
        // One: it, whatever it is called. A name is taken as typed (in lowercase), and is not looked up here.
        Store::under(&root, "acme").make().unwrap();
        assert_eq!(which(&root, None), Ok("acme".into()));
        assert!(kept_here(&root, "acme") && !kept_here(&root, "default"));
        assert_eq!(which(&root, Some(" Globex ")), Ok("globex".into()));
        // Two: nothing picks.
        let globex = Store::under(&root, "globex");
        globex.write(&Kept { modal: Some(modal::Session { token_id: "ak".into(), token_secret: "as".into(), workspace: "globex-team".into() }),
            seen: Some(Seen { called: "GlobexCI".into(), hosts: vec![("github".into(), "globex".into())] }), ..Default::default() }).unwrap();
        assert_eq!(which(&root, None), Err(vec!["acme".to_string(), "globex".to_string()]));
        assert_eq!(globex.workspace(), "globex");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for dir in [root.clone(), root.join("workspaces"), root.join("workspaces/globex"), root.join("workspaces/acme")] { assert_eq!(std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700, "{}", dir.display()) }
            assert_eq!(std::fs::metadata(globex.path()).unwrap().permissions().mode() & 0o777, 0o600);
        }
        assert_eq!(globex.root().unwrap(), root);
        // Each one's sign-ins are its own: nothing of one is in another.
        assert!(!Store::under(&root, "acme").read().signed_in() && globex.read().signed_in());
        let all = listed(&root, Some("default"));
        assert_eq!(all.iter().map(|w| w.name.as_str()).collect::<Vec<_>>(), ["acme", "default", "globex"]);
        assert!(all[2].signed_in && all[2].called == "GlobexCI" && all[2].hosts == [("github".to_string(), "globex".to_string())] && !all[0].signed_in);
        // A new name: the folder moves, with what it keeps; a taken name and one that is no name are refused.
        assert_eq!(rename(&root, "globex", "acme").unwrap_err(), "There is a workspace named acme already.");
        assert!(rename(&root, "globex", "Globex Inc").unwrap_err().starts_with("A workspace's name is 2 to 24 lowercase letters"));
        rename(&root, "globex", "initech").unwrap();
        assert!(Store::under(&root, "initech").read().signed_in() && workspaces_in(&root) == ["acme", "initech"]);
        // Forgotten: its folder goes; the other stays.
        Store::under(&root, "initech").forget().unwrap();
        assert_eq!(workspaces_in(&root), ["acme"]);
        for no in ["a", "Acme", "9lives", "acme corp", "acme_corp", "../acme", "a-name-that-is-much-too-long"] { assert!(valid_name(no).is_err(), "{no}") }
        let _ = std::fs::remove_dir_all(&root);
    }
}

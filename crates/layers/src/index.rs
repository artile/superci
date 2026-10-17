//! What SuperCI's store has for this machine: each layer by its blob's digest (the compressed tar a registry
//! serves), with its EROFS file and its blob (their links asked for when a layer is used, see `refresh`). Written by
//! the machine's start from what the control plane answered; a layer the index lacks is pulled as Docker pulls it,
//! and offered to the store after the job (`keep`).

use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct File {
    /// Its link (none until asked for).
    #[serde(default)]
    pub url: String,
    pub size: u64,
    /// The file's own digest (`sha256:…`), checked before it is used.
    #[serde(default)]
    pub digest: Option<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Layer {
    /// The blob's size (as its manifest says).
    pub size: u64,
    /// The layer read-only as EROFS, mounted instead of unpacking the blob.
    pub erofs: File,
    /// The blob itself, for whoever reads it (`docker save`, `docker push`).
    pub blob: Link,
    /// What the unpacked layer takes on disk, as containerd counts it (`docker system df`); counted from the
    /// mounted file when not given.
    #[serde(default)]
    pub usage: Option<Usage>,
    /// The job's last run used it: its file fetched as the store starts, before Docker asks for it.
    #[serde(default)]
    pub ahead: bool,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Link { #[serde(default)] pub url: String }

#[derive(Debug, Clone, Copy, Deserialize, PartialEq)]
pub struct Usage { pub size: i64, pub inodes: i64 }

/// Where links that have run out are signed again: `POST url` with `{"digests": [...]}` and the token, answered
/// with the layers as the index has them.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Refresh { pub url: String, pub token: String }

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
pub struct Index {
    #[serde(default)]
    pub layers: HashMap<String, Layer>,
    #[serde(default)]
    pub refresh: Option<Refresh>,
}

impl Index {
    /// The index at `path`; none there is an empty one (nothing from the store).
    pub fn load(path: &Path) -> Result<Index, String> {
        match std::fs::read(path) {
            Ok(b) => serde_json::from_slice(&b).map_err(|e| format!("{}: {e}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Index::default()),
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    }

    pub fn get(&self, digest: &str) -> Option<&Layer> { self.layers.get(digest) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_index_reads_its_layers_and_none_is_empty() {
        let dir = std::env::temp_dir().join(format!("superci-layers-index-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(Index::load(&dir.join("none.json")).unwrap(), Index::default());
        let p = dir.join("index.json");
        std::fs::write(&p, r#"{"layers": {"sha256:aa": {"size": 3, "erofs": {"url": "https://b/e", "size": 9, "digest": "sha256:bb"}, "blob": {"url": "https://b/b"}, "usage": {"size": 4096, "inodes": 2}}}, "refresh": {"url": "https://plane/layers/links", "token": "t"}}"#).unwrap();
        let i = Index::load(&p).unwrap();
        let l = i.get("sha256:aa").unwrap();
        assert_eq!((l.size, l.erofs.size, l.erofs.digest.as_deref(), l.usage), (3, 9, Some("sha256:bb"), Some(Usage { size: 4096, inodes: 2 })));
        assert_eq!(i.refresh.unwrap().token, "t");
        std::fs::write(&p, "{").unwrap();
        assert!(Index::load(&p).unwrap_err().contains("index.json"));
        let _ = std::fs::remove_dir_all(dir);
    }
}

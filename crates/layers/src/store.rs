//! A machine's layers: containerd's overlay snapshotter and content store (`overlay`, `content`), answering for
//! the layers SuperCI's store has too.

use crate::content::{Digest, Store};
use crate::error::Result;
use crate::overlay::{Mount, Overlay};
use std::collections::BTreeMap;
use std::sync::Arc;

pub struct Layers {
    pub overlay: Overlay,
    pub content: Store,
}

impl Layers {
    /// A new active snapshot.
    pub async fn prepare(self: Arc<Self>, key: String, parent: String, labels: BTreeMap<String, String>) -> Result<Vec<Mount>> {
        let l = self.clone();
        tokio::task::spawn_blocking(move || l.overlay.prepare(&key, &parent, labels)).await.map_err(|e| crate::error::Error::Other(e.to_string()))?
    }

    /// A blob's info.
    pub fn content_info(&self, d: &Digest) -> Result<crate::content::Info> { self.content.info(d) }

    /// A blob to read, and its size.
    pub async fn open_blob(self: Arc<Self>, d: Digest) -> Result<(std::fs::File, u64)> {
        let l = self.clone();
        tokio::task::spawn_blocking(move || l.content.open_blob(&d)).await.map_err(|e| crate::error::Error::Other(e.to_string()))?
    }
}

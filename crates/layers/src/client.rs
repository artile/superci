//! containerd itself, as a client of it (its socket): a layer served from the store made known in the namespace
//! that pulled it, as containerd does with content another namespace already has: a write that commits nothing,
//! its digest and size said. Under a lease of SuperCI's own that runs out after an hour, so containerd's garbage
//! collection keeps it until the image that names it is recorded, and no longer than that image.

use crate::containerd::services::content::v1 as capi;
use crate::containerd::services::leases::v1 as lapi;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tonic::transport::{Channel, Endpoint};

const LEASE_FOR: Duration = Duration::from_secs(3600);
const LEASE_USED_FOR: Duration = Duration::from_secs(45 * 60);

/// A gRPC channel to a Unix socket (containerd's).
pub async fn channel(socket: &std::path::Path) -> Result<Channel, String> {
    let path = socket.to_path_buf();
    Endpoint::try_from("http://containerd").map_err(|e| e.to_string()).and_then(crate::faults::said)?
        .connect_with_connector(tower::service_fn(move |_: tonic::transport::Uri| {
            let p = path.clone();
            async move { Ok::<_, std::io::Error>(hyper_util::rt::TokioIo::new(tokio::net::UnixStream::connect(p).await.and_then(crate::faults::pass)?)) }
        }))
        .await.map_err(|e| format!("containerd at {}: {e}", socket.display()))
}

pub struct Containerd { socket: PathBuf, leases: Mutex<HashMap<String, (String, Instant)>> }

impl Containerd {
    pub fn new(socket: PathBuf) -> Self { Containerd { socket, leases: Mutex::new(HashMap::new()) } }

    async fn channel(&self) -> Result<Channel, String> { channel(&self.socket).await }

    fn with_namespace<T>(msg: T, namespace: &str, lease: Option<&str>) -> Result<tonic::Request<T>, String> {
        let mut r = tonic::Request::new(msg);
        r.metadata_mut().insert("containerd-namespace", namespace.parse().map_err(|_| format!("namespace {namespace:?}")).and_then(crate::faults::said)?);
        if let Some(l) = lease { r.metadata_mut().insert("containerd-lease", l.parse().map_err(|_| format!("lease {l:?}")).and_then(crate::faults::said)?); }
        Ok(r)
    }

    /// The lease this namespace's registrations go under: one per namespace, a new one before it runs out.
    async fn lease(&self, ch: Channel, namespace: &str) -> Result<String, String> {
        if let Some((id, made)) = self.leases.lock().unwrap().get(namespace) { if made.elapsed() < LEASE_USED_FOR { return Ok(id.clone()) } }
        let now = crate::meta::now();
        let id = format!("superci-layers-{now}");
        let expire = crate::content::rfc3339(now + LEASE_FOR.as_nanos() as i64);
        let req = lapi::CreateRequest { id: id.clone(), labels: [("containerd.io/gc.expire".to_string(), expire)].into_iter().collect() };
        lapi::leases_client::LeasesClient::new(ch).create(Self::with_namespace(req, namespace, None).and_then(crate::faults::said)?).await.map_err(|e| format!("a lease: {}", e.message())).and_then(crate::faults::said)?;
        self.leases.lock().unwrap().insert(namespace.to_string(), (id.clone(), Instant::now()));
        Ok(id)
    }

    /// A blob's labels in a namespace (where it was pulled from: `containerd.io/distribution.source.<registry>`).
    pub async fn labels(&self, namespace: &str, digest: &str) -> Result<HashMap<String, String>, String> {
        let ch = self.channel().await.and_then(crate::faults::said)?;
        let req = Self::with_namespace(capi::InfoRequest { digest: digest.to_string() }, namespace, None).and_then(crate::faults::said)?;
        let info = capi::content_client::ContentClient::new(ch).info(req).await.map_err(|e| e.message().to_string()).and_then(crate::faults::said)?.into_inner();
        Ok(info.info.map(|i| i.labels.into_iter().collect()).unwrap_or_default())
    }

    /// `digest` (of `size` bytes) made known in `namespace`; known already is no error. As containerd's own client
    /// writes content it may have: the write's status first, then a commit at the offset that says (containerd
    /// counts shared content as written whole; a commit at offset 0 would start it over).
    pub async fn register(&self, namespace: &str, digest: &str, size: i64) -> Result<(), String> {
        let ch = self.channel().await.and_then(crate::faults::said)?;
        let lease = self.lease(ch.clone(), namespace).await.and_then(crate::faults::said)?;
        let (tx, rx) = tokio::sync::mpsc::channel(2);
        let r#ref = format!("superci-layers-{digest}");
        let stat = capi::WriteContentRequest { action: capi::WriteAction::Stat as i32, r#ref: r#ref.clone(), total: size, expected: digest.to_string(), ..Default::default() };
        tx.send(stat).await.map_err(|e| e.to_string()).and_then(crate::faults::said)?;
        let req = Self::with_namespace(tokio_stream::wrappers::ReceiverStream::new(rx), namespace, Some(&lease)).and_then(crate::faults::said)?;
        let answer = |e: tonic::Status| if e.code() == tonic::Code::AlreadyExists { Ok(()) } else { Err(e.message().to_string()) };
        let mut s = match capi::content_client::ContentClient::new(ch).write(req).await { Ok(r) => r.into_inner(), Err(e) => return answer(e) };
        let status = match s.message().await { Ok(Some(m)) => m, Ok(None) => return Err("containerd ended the write".into()), Err(e) => return answer(e) };
        let commit = capi::WriteContentRequest { action: capi::WriteAction::Commit as i32, r#ref, total: size, expected: digest.to_string(), offset: status.offset, ..Default::default() };
        tx.send(commit).await.map_err(|e| e.to_string()).and_then(crate::faults::said)?;
        drop(tx);
        loop {
            match s.message().await {
                Ok(Some(_)) => continue,
                Ok(None) => return Ok(()),
                Err(e) => return answer(e),
            }
        }
    }
}

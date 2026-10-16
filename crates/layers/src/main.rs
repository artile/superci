/// containerd's own API, as its .proto files say it (see build.rs).
pub mod containerd {
    pub mod types { tonic::include_proto!("containerd.types"); }
    pub mod services {
        pub mod snapshots { pub mod v1 { tonic::include_proto!("containerd.services.snapshots.v1"); } }
        pub mod content { pub mod v1 { tonic::include_proto!("containerd.services.content.v1"); } }
    }
}
pub mod content;
pub mod error;
pub mod filters;
pub mod meta;
pub mod mount;
pub mod overlay;
pub mod server;
pub mod store;

use std::path::PathBuf;
use std::sync::Arc;

const HELP: &str = "superci-layers: containerd's snapshotter and content store for a SuperCI machine, on one socket.

  --root=DIR      its own files: snapshots, labels (/var/lib/superci-layers)
  --content=DIR   containerd's content folder (/var/lib/containerd/io.containerd.content.v1.content)
  --socket=PATH   where containerd reaches it (/run/superci-layers.sock)
  --sync-remove   a removed snapshot's folder goes at once, not at the next cleanup";

struct Args { root: PathBuf, content: PathBuf, socket: PathBuf, sync_remove: bool }

fn args() -> Result<Args, String> {
    let mut a = Args { root: "/var/lib/superci-layers".into(), content: "/var/lib/containerd/io.containerd.content.v1.content".into(), socket: "/run/superci-layers.sock".into(), sync_remove: false };
    for arg in std::env::args().skip(1) {
        let (k, v) = arg.split_once('=').map(|(k, v)| (k.to_string(), Some(v.to_string()))).unwrap_or((arg.clone(), None));
        match (k.as_str(), v) {
            ("--root", Some(v)) => a.root = v.into(),
            ("--content", Some(v)) => a.content = v.into(),
            ("--socket", Some(v)) => a.socket = v.into(),
            ("--sync-remove", None) => a.sync_remove = true,
            ("--help" | "-h", None) => return Err(HELP.into()),
            _ => return Err(format!("{arg:?} is not understood\n\n{HELP}")),
        }
    }
    Ok(a)
}

/// Tells systemd the service is ready (a unit of `Type=notify`), when it runs under one.
fn notify_ready() {
    #[cfg(target_os = "linux")]
    if let Some(path) = std::env::var_os("NOTIFY_SOCKET") {
        use std::os::linux::net::SocketAddrExt;
        let p = path.to_string_lossy().into_owned();
        let addr = match p.strip_prefix('@') {
            Some(name) => std::os::unix::net::SocketAddr::from_abstract_name(name.as_bytes()),
            None => std::os::unix::net::SocketAddr::from_pathname(&p),
        };
        if let (Ok(sock), Ok(addr)) = (std::os::unix::net::UnixDatagram::unbound(), addr) { let _ = sock.send_to_addr(b"READY=1", &addr); }
    }
}

fn main() {
    let a = match args() { Ok(a) => a, Err(e) => { eprintln!("{e}"); std::process::exit(2) } };
    let open = || -> error::Result<store::Layers> {
        let overlay = overlay::Overlay::open(&a.root.join("overlay"), !a.sync_remove)?;
        let content = content::Store::open(&a.content, &a.root.join("content-labels.db"))?;
        Ok(store::Layers { overlay, content })
    };
    let layers = match open() { Ok(l) => Arc::new(l), Err(e) => { eprintln!("superci-layers: {e}"); std::process::exit(1) } };
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("a runtime");
    rt.block_on(async move {
        let _ = std::fs::remove_file(&a.socket);
        if let Some(dir) = a.socket.parent() { let _ = std::fs::create_dir_all(dir); }
        let listener = match tokio::net::UnixListener::bind(&a.socket) { Ok(l) => l, Err(e) => { eprintln!("superci-layers: {}: {e}", a.socket.display()); std::process::exit(1) } };
        use containerd::services::{content::v1::content_server::ContentServer, snapshots::v1::snapshots_server::SnapshotsServer};
        let server = tonic::transport::Server::builder()
            .add_service(SnapshotsServer::new(server::Snapshots { layers: layers.clone() }))
            .add_service(ContentServer::new(server::Content { layers: layers.clone() }).max_decoding_message_size(16 << 20))
            .serve_with_incoming_shutdown(tokio_stream::wrappers::UnixListenerStream::new(listener), async {
                let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).expect("SIGTERM");
                tokio::select! { _ = term.recv() => {}, _ = tokio::signal::ctrl_c() => {} }
            });
        eprintln!("superci-layers: answering on {}", a.socket.display());
        notify_ready();
        if let Err(e) = server.await { eprintln!("superci-layers: {e}"); std::process::exit(1) }
    });
}

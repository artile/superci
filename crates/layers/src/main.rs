use superci_layers::{containerd, content, error, index, keep, overlay, server, setup, store};

use std::path::PathBuf;
use std::sync::Arc;

const HELP: &str = "superci-layers: containerd's snapshotter and content store for a SuperCI machine, on one socket.

  superci-layers setup --index-url=URL [--disk=auto|none|DEVICE] [--root=DIR]
                  at a machine's start: checks, then Docker on this service; on anything that fails, Docker as it
                  comes (it never fails the start)
  superci-layers off
                  Docker as it comes again
  superci-layers keep --link=URL [--most=SECONDS] [--state=FILE]
                  after the job: the layers Docker pulled that the store lacks offered to the control plane at URL
                  and uploaded where it asks (for SECONDS at most, 180 unless said), and what came from the store said
  superci-layers [--root=…] …  the service itself:

  --root=DIR          its own files: snapshots, labels (/var/lib/superci-layers)
  --content=DIR       containerd's content folder (/var/lib/containerd/io.containerd.content.v1.content)
  --socket=PATH       where containerd reaches it (/run/superci-layers.sock)
  --index=PATH        the layers SuperCI's store has (/etc/superci/layers.json; none there: nothing from the store;
                      read again on SIGHUP)
  --files=DIR         where the store's layer files are kept (ROOT/files; the store's own disk when it has one)
  --containerd=PATH   containerd's socket, to make served layers known in their namespace
                      (/run/containerd/containerd.sock)
  --parallel=N        ranges of one file fetched at once (16)
  --layers=N          layers fetched at once (8)
  --sync-remove       a removed snapshot's folder goes at once, not at the next cleanup";

struct Args { root: PathBuf, content: PathBuf, socket: PathBuf, index: PathBuf, files: Option<PathBuf>, containerd: PathBuf, parallel: usize, layers: usize, sync_remove: bool }

fn args() -> Result<Args, String> {
    let mut a = Args { root: "/var/lib/superci-layers".into(), content: "/var/lib/containerd/io.containerd.content.v1.content".into(), socket: "/run/superci-layers.sock".into(),
        index: "/etc/superci/layers.json".into(), files: None, containerd: "/run/containerd/containerd.sock".into(), parallel: 16, layers: 8, sync_remove: false };
    let number = |v: &str, arg: &str| v.parse::<usize>().ok().filter(|n| *n > 0).ok_or(format!("{arg} takes a number above 0"));
    for arg in std::env::args().skip(1) {
        let (k, v) = arg.split_once('=').map(|(k, v)| (k.to_string(), Some(v.to_string()))).unwrap_or((arg.clone(), None));
        match (k.as_str(), v) {
            ("--root", Some(v)) => a.root = v.into(),
            ("--content", Some(v)) => a.content = v.into(),
            ("--socket", Some(v)) => a.socket = v.into(),
            ("--index", Some(v)) => a.index = v.into(),
            ("--files", Some(v)) => a.files = Some(v.into()),
            ("--containerd", Some(v)) => a.containerd = v.into(),
            ("--parallel", Some(v)) => a.parallel = number(&v, "--parallel")?,
            ("--layers", Some(v)) => a.layers = number(&v, "--layers")?,
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
    let argv: Vec<String> = std::env::args().collect();
    match argv.get(1).map(String::as_str) {
        Some("off") => std::process::exit(setup::off()),
        Some("setup-finish") => std::process::exit(setup::finish()),
        Some("keep") => {
            let mut o = keep::Options { link: String::new(), socket: "/run/containerd/containerd.sock".into(), content: "/var/lib/containerd/io.containerd.content.v1.content".into(),
                index: "/etc/superci/layers.json".into(), most: std::time::Duration::from_secs(180), boot: "/run/superci-boot-blobs".into(), state: superci_layers::setup::STATE.into() };
            for arg in &argv[2..] {
                match arg.split_once('=') {
                    Some(("--link", v)) => o.link = v.trim_end_matches('/').into(),
                    Some(("--most", v)) => match v.parse::<u64>() { Ok(n) => o.most = std::time::Duration::from_secs(n), Err(_) => { eprintln!("--most takes seconds"); std::process::exit(2) } },
                    Some(("--socket", v)) => o.socket = v.into(),
                    Some(("--content", v)) => o.content = v.into(),
                    Some(("--index", v)) => o.index = v.into(),
                    Some(("--boot", v)) => o.boot = v.into(),
                    Some(("--state", v)) => o.state = v.into(),
                    _ => { eprintln!("{arg:?} is not understood\n\n{HELP}"); std::process::exit(2) }
                }
            }
            if o.link.is_empty() { eprintln!("keep takes --link\n\n{HELP}"); std::process::exit(2) }
            std::process::exit(keep::keep(&o));
        }
        Some("setup") => {
            let mut o = setup::Options { index_url: String::new(), disk: "auto".into(), root: "/var/lib/superci-layers".into() };
            for arg in &argv[2..] {
                match arg.split_once('=') {
                    Some(("--index-url", v)) => o.index_url = v.into(),
                    Some(("--disk", v)) => o.disk = v.into(),
                    Some(("--root", v)) => o.root = v.into(),
                    _ => { eprintln!("{arg:?} is not understood\n\n{HELP}"); std::process::exit(2) }
                }
            }
            if o.index_url.is_empty() { eprintln!("setup takes --index-url\n\n{HELP}"); std::process::exit(2) }
            std::process::exit(setup::setup(&o));
        }
        _ => {}
    }
    let a = match args() { Ok(a) => a, Err(e) => { eprintln!("{e}"); std::process::exit(2) } };
    let open = || -> error::Result<store::Layers> {
        let overlay = overlay::Overlay::open(&a.root.join("overlay"), !a.sync_remove)?;
        let content = content::Store::open(&a.content, &a.root.join("content-labels.db"))?;
        // (An index that cannot be read leaves every layer to be pulled as usual: never a reason not to answer.)
        let index = index::Index::load(&a.index).unwrap_or_else(|e| { eprintln!("superci-layers: the index is not read, nothing comes from the store: {e}"); index::Index::default() });
        eprintln!("superci-layers: {} layers in the store's index", index.layers.len());
        let files = a.files.clone().unwrap_or_else(|| a.root.join("files"));
        let options = store::Options { files, db: a.root.join("store.db"), index, containerd: a.containerd.clone(), parallel: a.parallel, layers_at_once: a.layers };
        store::Layers::open(overlay, content, options)
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
        // (The index written again: read again.)
        let (l, index_path) = (layers.clone(), a.index.clone());
        let mut hup = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup()).expect("SIGHUP");
        tokio::spawn(async move {
            // (A signal's stream goes on as long as the program does.)
            loop {
                hup.recv().await;
                match index::Index::load(&index_path) {
                    Ok(i) => { eprintln!("superci-layers: {} layers in the store's index", i.layers.len()); l.reload(i); l.fetch_ahead() }
                    Err(e) => eprintln!("superci-layers: the index was not read again: {e}"),
                }
            }
        });
        eprintln!("superci-layers: answering on {}", a.socket.display());
        notify_ready();
        layers.fetch_ahead();
        let failed = server.await.as_ref().err().map(ToString::to_string).or_else(|| superci_layers::faults::said(()).err());
        if let Some(e) = failed { eprintln!("superci-layers: {e}"); std::process::exit(1) }
    });
}

//! containerd's snapshot and content services over gRPC, as containerd's own servers answer them
//! (`contrib/snapshotservice`, `plugins/services/content/contentserver`): the same fields read, the same errors with
//! the same codes, lists in blocks of 100, reads in pieces of 1 MiB, a write session message for message.

use crate::containerd::services::content::v1 as capi;
use crate::containerd::services::snapshots::v1 as sapi;
use crate::containerd::types;
use crate::content::{Digest, Store};
use crate::error::{Error, Result};
use crate::meta::{Info, Kind, Nanos};
use crate::overlay::Mount;
use crate::store::Layers;
use std::collections::BTreeMap;
use std::sync::Arc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status};

fn ts(n: Nanos) -> Option<prost_types::Timestamp> { Some(prost_types::Timestamp { seconds: n.div_euclid(1_000_000_000), nanos: n.rem_euclid(1_000_000_000) as i32 }) }
/// A time a caller sent (Go's zero time is year 1, past what nanoseconds since 1970 hold: kept at the nearest).
fn from_ts(t: Option<prost_types::Timestamp>) -> Nanos { t.map(|t| t.seconds.saturating_mul(1_000_000_000).saturating_add(t.nanos as i64)).unwrap_or(0) }
fn sorted(m: std::collections::HashMap<String, String>) -> BTreeMap<String, String> { m.into_iter().collect() }

fn info_proto(i: Info) -> sapi::Info {
    sapi::Info { name: i.name, parent: i.parent, kind: i.kind.map(Kind::proto).unwrap_or(0), created_at: ts(i.created), updated_at: ts(i.updated), labels: i.labels.into_iter().collect() }
}

fn mounts_proto(m: Vec<Mount>) -> Vec<types::Mount> {
    m.into_iter().map(|m| types::Mount { r#type: m.kind, source: m.source, target: String::new(), options: m.options }).collect()
}

/// Work on files and the database, off the threads that answer calls.
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T> + Send + 'static) -> std::result::Result<T, Status> {
    match tokio::task::spawn_blocking(f).await {
        Ok(r) => r.map_err(Status::from),
        Err(e) => Err(Status::internal(e.to_string())),
    }
}

pub struct Snapshots { pub layers: Arc<Layers> }

#[tonic::async_trait]
impl sapi::snapshots_server::Snapshots for Snapshots {
    async fn prepare(&self, r: Request<sapi::PrepareSnapshotRequest>) -> std::result::Result<Response<sapi::PrepareSnapshotResponse>, Status> {
        let r = r.into_inner();
        let l = self.layers.clone();
        let mounts = l.prepare(r.key, r.parent, sorted(r.labels)).await.map_err(Status::from)?;
        Ok(Response::new(sapi::PrepareSnapshotResponse { mounts: mounts_proto(mounts) }))
    }

    async fn view(&self, r: Request<sapi::ViewSnapshotRequest>) -> std::result::Result<Response<sapi::ViewSnapshotResponse>, Status> {
        let r = r.into_inner();
        let l = self.layers.clone();
        let mounts = blocking(move || l.overlay.view(&r.key, &r.parent, sorted(r.labels))).await?;
        Ok(Response::new(sapi::ViewSnapshotResponse { mounts: mounts_proto(mounts) }))
    }

    async fn mounts(&self, r: Request<sapi::MountsRequest>) -> std::result::Result<Response<sapi::MountsResponse>, Status> {
        let key = r.into_inner().key;
        let l = self.layers.clone();
        let mounts = blocking(move || l.overlay.mounts(&key)).await?;
        Ok(Response::new(sapi::MountsResponse { mounts: mounts_proto(mounts) }))
    }

    async fn commit(&self, r: Request<sapi::CommitSnapshotRequest>) -> std::result::Result<Response<()>, Status> {
        let r = r.into_inner();
        let l = self.layers.clone();
        blocking(move || l.overlay.commit(&r.name, &r.key, sorted(r.labels), &r.parent)).await?;
        Ok(Response::new(()))
    }

    async fn remove(&self, r: Request<sapi::RemoveSnapshotRequest>) -> std::result::Result<Response<()>, Status> {
        let key = r.into_inner().key;
        let l = self.layers.clone();
        blocking(move || l.overlay.remove(&key)).await?;
        Ok(Response::new(()))
    }

    async fn stat(&self, r: Request<sapi::StatSnapshotRequest>) -> std::result::Result<Response<sapi::StatSnapshotResponse>, Status> {
        let key = r.into_inner().key;
        let l = self.layers.clone();
        let info = blocking(move || l.overlay.stat(&key)).await?;
        Ok(Response::new(sapi::StatSnapshotResponse { info: Some(info_proto(info)) }))
    }

    async fn update(&self, r: Request<sapi::UpdateSnapshotRequest>) -> std::result::Result<Response<sapi::UpdateSnapshotResponse>, Status> {
        let r = r.into_inner();
        let i = r.info.unwrap_or_default();
        let info = Info { name: i.name, parent: i.parent, kind: None, labels: sorted(i.labels), created: from_ts(i.created_at), updated: from_ts(i.updated_at) };
        let paths = r.update_mask.map(|m| m.paths).unwrap_or_default();
        let l = self.layers.clone();
        let info = blocking(move || l.overlay.update(&info, &paths)).await?;
        Ok(Response::new(sapi::UpdateSnapshotResponse { info: Some(info_proto(info)) }))
    }

    type ListStream = ReceiverStream<std::result::Result<sapi::ListSnapshotsResponse, Status>>;

    async fn list(&self, r: Request<sapi::ListSnapshotsRequest>) -> std::result::Result<Response<Self::ListStream>, Status> {
        let filters = r.into_inner().filters;
        let l = self.layers.clone();
        let (tx, rx) = tokio::sync::mpsc::channel(4);
        tokio::task::spawn_blocking(move || {
            let mut block = vec![];
            let walked = l.overlay.walk(&filters, |i| {
                block.push(info_proto(i));
                if block.len() >= 100 { tx.blocking_send(Ok(sapi::ListSnapshotsResponse { info: std::mem::take(&mut block) })).map_err(|_| Error::Other("canceled".into()))? }
                Ok(())
            });
            match walked {
                Ok(()) => { if !block.is_empty() { let _ = tx.blocking_send(Ok(sapi::ListSnapshotsResponse { info: block })); } }
                Err(e) => { let _ = tx.blocking_send(Err(e.into())); }
            }
        });
        Ok(Response::new(ReceiverStream::new(rx)))
    }

    async fn usage(&self, r: Request<sapi::UsageRequest>) -> std::result::Result<Response<sapi::UsageResponse>, Status> {
        let key = r.into_inner().key;
        let l = self.layers.clone();
        let u = blocking(move || l.overlay.usage(&key)).await?;
        Ok(Response::new(sapi::UsageResponse { size: u.size, inodes: u.inodes }))
    }

    async fn cleanup(&self, _r: Request<sapi::CleanupRequest>) -> std::result::Result<Response<()>, Status> {
        let l = self.layers.clone();
        blocking(move || l.overlay.cleanup()).await?;
        Ok(Response::new(()))
    }
}

pub struct Content { pub layers: Arc<Layers> }

fn content_info(i: crate::content::Info) -> capi::Info {
    capi::Info { digest: i.digest, size: i.size, created_at: ts(i.created), updated_at: ts(i.updated), labels: i.labels.into_iter().collect() }
}

fn status_proto(s: crate::content::Status) -> capi::Status {
    capi::Status { started_at: ts(s.started), updated_at: ts(s.updated), r#ref: s.ref_, offset: s.offset, total: s.total, expected: s.expected }
}

fn digest_arg(d: &str) -> std::result::Result<Digest, Status> { Digest::parse(d).map_err(|_| Status::invalid_argument(format!("{d:?} failed validation"))) }

#[tonic::async_trait]
impl capi::content_server::Content for Content {
    async fn info(&self, r: Request<capi::InfoRequest>) -> std::result::Result<Response<capi::InfoResponse>, Status> {
        let d = digest_arg(&r.into_inner().digest)?;
        let l = self.layers.clone();
        let i = blocking(move || l.content_info(&d)).await?;
        Ok(Response::new(capi::InfoResponse { info: Some(content_info(i)) }))
    }

    async fn update(&self, r: Request<capi::UpdateRequest>) -> std::result::Result<Response<capi::UpdateResponse>, Status> {
        let r = r.into_inner();
        let i = r.info.unwrap_or_default();
        digest_arg(&i.digest)?;
        let info = crate::content::Info { digest: i.digest, size: i.size, created: from_ts(i.created_at), updated: from_ts(i.updated_at), labels: sorted(i.labels) };
        let paths = r.update_mask.map(|m| m.paths).unwrap_or_default();
        let l = self.layers.clone();
        let out = blocking(move || l.content.update(&info, &paths)).await?;
        Ok(Response::new(capi::UpdateResponse { info: Some(content_info(out)) }))
    }

    type ListStream = ReceiverStream<std::result::Result<capi::ListContentResponse, Status>>;

    async fn list(&self, r: Request<capi::ListContentRequest>) -> std::result::Result<Response<Self::ListStream>, Status> {
        let filters = r.into_inner().filters;
        let l = self.layers.clone();
        let (tx, rx) = tokio::sync::mpsc::channel(4);
        tokio::task::spawn_blocking(move || {
            let mut block = vec![];
            let walked = l.content.walk(&filters, |i| {
                block.push(content_info(i));
                if block.len() >= 100 { tx.blocking_send(Ok(capi::ListContentResponse { info: std::mem::take(&mut block) })).map_err(|_| Error::Other("canceled".into()))? }
                Ok(())
            });
            match walked {
                Ok(()) => { if !block.is_empty() { let _ = tx.blocking_send(Ok(capi::ListContentResponse { info: block })); } }
                Err(e) => { let _ = tx.blocking_send(Err(e.into())); }
            }
        });
        Ok(Response::new(ReceiverStream::new(rx)))
    }

    async fn delete(&self, r: Request<capi::DeleteContentRequest>) -> std::result::Result<Response<()>, Status> {
        let d = r.into_inner().digest;
        let d = Digest::parse(&d).map_err(|e| Status::invalid_argument(e.to_string()))?;
        let l = self.layers.clone();
        blocking(move || l.content.delete(&d)).await?;
        Ok(Response::new(()))
    }

    type ReadStream = ReceiverStream<std::result::Result<capi::ReadContentResponse, Status>>;

    async fn read(&self, r: Request<capi::ReadContentRequest>) -> std::result::Result<Response<Self::ReadStream>, Status> {
        let r = r.into_inner();
        let d = Digest::parse(&r.digest).map_err(|e| Status::invalid_argument(format!("{}: {e}", r.digest)))?;
        let l = self.layers.clone();
        let size = blocking({ let (l, d) = (l.clone(), d.clone()); move || l.content_info(&d) }).await?.size;
        // (A blob SuperCI's store has but this machine does not yet: fetched whole first.)
        let (file, _) = l.clone().open_blob(d.clone()).await.map_err(Status::from)?;
        let mut offset = r.offset.max(0);
        if offset > size { return Err(Status::out_of_range(format!("read past object length {size} bytes"))) }
        let mut want = r.size;
        if want <= 0 || offset + want > size { want = size - offset }
        let (tx, rx) = tokio::sync::mpsc::channel(4);
        tokio::task::spawn_blocking(move || {
            use std::os::unix::fs::FileExt;
            let mut buf = vec![0u8; 1 << 20];
            let end = offset + want;
            let mut sent = 0i64;
            while offset < end {
                let n = ((end - offset) as usize).min(buf.len());
                match file.read_at(&mut buf[..n], offset as u64) {
                    Ok(0) => break,
                    Ok(k) => {
                        if tx.blocking_send(Ok(capi::ReadContentResponse { offset: sent, data: buf[..k].to_vec() })).is_err() { return }
                        offset += k as i64;
                        sent += k as i64;
                    }
                    Err(e) => { let _ = tx.blocking_send(Err(Status::from(Error::from(e)))); return }
                }
            }
        });
        Ok(Response::new(ReceiverStream::new(rx)))
    }

    async fn status(&self, r: Request<capi::StatusRequest>) -> std::result::Result<Response<capi::StatusResponse>, Status> {
        let reference = r.into_inner().r#ref;
        let l = self.layers.clone();
        let rr = reference.clone();
        let s = blocking(move || l.content.status(&rr).map_err(|e| e.context(format!("could not get status for ref {reference:?}")))).await?;
        Ok(Response::new(capi::StatusResponse { status: Some(status_proto(s)) }))
    }

    async fn list_statuses(&self, r: Request<capi::ListStatusesRequest>) -> std::result::Result<Response<capi::ListStatusesResponse>, Status> {
        let filters = r.into_inner().filters;
        let l = self.layers.clone();
        let s = blocking(move || l.content.list_statuses(&filters)).await?;
        Ok(Response::new(capi::ListStatusesResponse { statuses: s.into_iter().map(status_proto).collect() }))
    }

    type WriteStream = ReceiverStream<std::result::Result<capi::WriteContentResponse, Status>>;

    async fn write(&self, r: Request<tonic::Streaming<capi::WriteContentRequest>>) -> std::result::Result<Response<Self::WriteStream>, Status> {
        let stream = r.into_inner();
        let l = self.layers.clone();
        let (tx, rx) = tokio::sync::mpsc::channel(4);
        let handle = tokio::runtime::Handle::current();
        tokio::task::spawn_blocking(move || {
            let mut msg = capi::WriteContentResponse::default();
            match write_session(&l, stream, &handle, &tx, &mut msg) {
                // (A session that ends well sends its last answer once more, as containerd's does.)
                Ok(()) => { let _ = tx.blocking_send(Ok(msg)); }
                Err(e) => {
                    if e.code() != tonic::Code::AlreadyExists { eprintln!("write failed: {e}") }
                    let _ = tx.blocking_send(Err(e));
                }
            }
        });
        Ok(Response::new(ReceiverStream::new(rx)))
    }

    async fn abort(&self, r: Request<capi::AbortRequest>) -> std::result::Result<Response<()>, Status> {
        let reference = r.into_inner().r#ref;
        let l = self.layers.clone();
        blocking(move || l.content.abort(&reference)).await?;
        Ok(Response::new(()))
    }
}

type Tx = tokio::sync::mpsc::Sender<std::result::Result<capi::WriteContentResponse, Status>>;

/// One write session (containerd's `(*service).Write`): the first message names the ref and locks its writer; each
/// message stats, writes at the current offset, or commits.
fn write_session(l: &Layers, mut stream: tonic::Streaming<capi::WriteContentRequest>, handle: &tokio::runtime::Handle, tx: &Tx, msg: &mut capi::WriteContentResponse) -> std::result::Result<(), Status> {
    let store: &Store = &l.content;
    let mut req = match handle.block_on(stream.message())? { Some(m) => m, None => return Err(Status::cancelled("no message")) };
    let reference = req.r#ref.clone();
    if reference.is_empty() { return Err(Status::invalid_argument("first message must have a reference")) }
    let mut total = req.total;
    let mut expected = req.expected.clone();
    let first = if expected.is_empty() { None } else {
        Some(Digest::parse(&expected).map_err(|e| Status::from(Error::InvalidArgument(format!("calculating expected blob path for writer: cannot calculate blob path from invalid digest: {e}"))))?)
    };
    let mut wr = store.writer(&reference, total, first.as_ref()).map_err(Status::from)?;
    loop {
        msg.action = req.action;
        let ws = wr.status();
        msg.offset = ws.offset;
        if !req.expected.is_empty() {
            expected = req.expected.clone();
            if let Ok(d) = Digest::parse(&expected) {
                if l.content_info(&d).is_ok() {
                    wr.close();
                    if let Err(e) = store.abort(&reference) { eprintln!("failed to abort write: {e}") }
                    return Err(Status::already_exists(format!("blob with expected digest {expected} exists")));
                }
            }
        }
        if req.total > 0 { total = req.total }
        match capi::WriteAction::try_from(req.action) {
            Ok(capi::WriteAction::Stat) => {
                msg.digest = wr.digest().to_string();
                msg.started_at = ts(ws.started);
                msg.updated_at = ts(ws.updated);
                msg.total = total;
            }
            Ok(action @ (capi::WriteAction::Write | capi::WriteAction::Commit)) => {
                if req.offset > 0 && req.offset != ws.offset {
                    return Err(Status::out_of_range(format!("write @{} must occur at current offset {}", req.offset, ws.offset)));
                }
                if req.offset == 0 && ws.offset > 0 {
                    wr.truncate(0).map_err(|e| Status::unknown(format!("truncate failed: {e}")))?;
                    msg.offset = 0;
                }
                if !req.data.is_empty() {
                    let n = wr.write(&req.data).map_err(Status::from)?;
                    if n != req.data.len() { return Err(Status::data_loss(format!("wrote {n} of {} bytes", req.data.len()))) }
                    msg.offset += n as i64;
                }
                if action == capi::WriteAction::Commit {
                    let labels = (!req.labels.is_empty()).then(|| sorted(std::mem::take(&mut req.labels)));
                    wr.commit_str(total, &expected, labels.as_ref()).map_err(Status::from)?;
                }
                msg.digest = wr.digest().to_string();
            }
            Err(_) => {}
        }
        if tx.blocking_send(Ok(msg.clone())).is_err() { return Err(Status::cancelled("the caller went away")) }
        if req.action == capi::WriteAction::Commit as i32 { return Ok(()) }
        req = match handle.block_on(stream.message()) {
            Ok(Some(m)) => m,
            Ok(None) => return Ok(()),
            Err(e) => return Err(e),
        };
    }
}

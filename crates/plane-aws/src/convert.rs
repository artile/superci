//! The layer store's converter: the same program as a second function (`SUPERCI_CONVERTER`), with a role that may
//! only read and write the layer store's part of the buckets. Given a layer (a registry's link to its blob, or the
//! blob a machine put in the bucket's staging), it streams the blob in once: hashed (it must be the digest it was
//! asked for), kept as it is when it came from a registry, unpacked (gzip, zstd or a plain tar, as its first bytes
//! say) and hashed again (the layer's diff ID), and the tar handed to `mkfs.erofs` as it comes. The EROFS file and
//! the blob go to the bucket, and what was made (or why nothing was) is written beside them for the control plane,
//! which the sweep reads (`layers/done/`).
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use futures::executor::block_on;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use superci_core::aws::{self, Credentials};
use superci_core::crypto::hex;
use superci_core::io::Http;

/// The options every EROFS file is made with (as the layer store's machines expect: lz4hc in 128 KiB clusters, no
/// inline data, every time and the UUID fixed, so the same layer always makes the same file).
pub const MKFS: [&str; 9] = ["--tar=f", "--aufs", "-zlz4hc", "-C131072", "-Enoinline_data", "-T0", "--mkfs-time", "-U", "39bb3f52-5f38-4d6c-a25a-5ea4cc1d8b11"];

/// A blob larger than this is not converted (the function's disk keeps the blob and the file at once).
pub const MOST_BYTES: u64 = 4 << 30;

/// An upload's part (S3 takes 10,000 at most: 640 GB).
const PART: usize = 64 << 20;

/// What to convert, as the control plane asks.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct Job {
    pub digest: String,
    pub size: u64,
    pub region: String,
    pub bucket: String,
    /// Where the blob is: `{"url", "headers"}` (a registry, with its token) or `{"key"}` (the bucket's staging).
    pub source: Value,
    pub erofs_key: String,
    pub blob_key: String,
    pub done_key: String,
}

/// The blob as it streams in: hashed, and kept on disk when asked.
struct Taken<R> { inner: R, hash: Sha256, bytes: u64, keep: Option<std::fs::File> }

impl<R: Read> Read for Taken<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.hash.update(&buf[..n]);
        self.bytes += n as u64;
        if let Some(f) = &mut self.keep { f.write_all(&buf[..n])? }
        Ok(n)
    }
}

/// The tar as it is unpacked: hashed (the diff ID).
struct Hashed<R> { inner: R, hash: Sha256 }

impl<R: Read> Read for Hashed<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.hash.update(&buf[..n]);
        Ok(n)
    }
}

/// How a blob is packed, by its first bytes.
#[derive(Debug, PartialEq)]
pub enum Packing { Gzip, Zstd, Tar }

pub fn packing(first: &[u8]) -> Packing {
    if first.starts_with(&[0x1f, 0x8b]) { Packing::Gzip } else if first.starts_with(&[0x28, 0xb5, 0x2f, 0xfd]) { Packing::Zstd } else { Packing::Tar }
}

/// What came of a blob streamed through `mkfs`: its digest and size, the diff ID.
#[derive(Debug, PartialEq)]
pub struct Made { pub digest: String, pub size: u64, pub diff_id: String }

/// Streams `blob` through `mkfs` (a command taking a tar on its input and writing `out`), keeping the blob at `keep`
/// when said. Nothing of a failed run is left at `out`.
pub fn make(blob: impl Read, mkfs: &str, out: &std::path::Path, keep: Option<&std::path::Path>) -> Result<Made, String> {
    let _ = std::fs::remove_file(out);
    let keep = match keep { Some(p) => Some(std::fs::File::create(p).map_err(|e| format!("{}: {e}", p.display()))?), None => None };
    let taken = Taken { inner: blob, hash: Sha256::new(), bytes: 0, keep };
    let mut buffered = BufReader::with_capacity(1 << 20, taken);
    let first = buffered.fill_buf().map_err(|e| format!("reading the blob: {e}"))?.to_vec();
    let mut child = Command::new(mkfs).args(MKFS).arg(out).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::piped()).spawn().map_err(|e| format!("{mkfs}: {e}"))?;
    let mut stdin = child.stdin.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let said = std::thread::spawn(move || { let mut s = String::new(); let _ = BufReader::new(stderr).take(64 << 10).read_to_string(&mut s); s });
    // (Unpacked, then what is left of the blob read too: its digest is of all of it.)
    let streamed = (|| -> Result<(String, BufReader<Taken<_>>), String> {
        let copy = |r: &mut dyn Read, w: &mut dyn Write| std::io::copy(r, w).map_err(|e| format!("unpacking the blob into mkfs.erofs: {e}"));
        Ok(match packing(&first) {
            Packing::Gzip => {
                let mut tar = Hashed { inner: flate2::bufread::MultiGzDecoder::new(buffered), hash: Sha256::new() };
                copy(&mut tar, &mut stdin)?;
                (hex(&tar.hash.finalize()), tar.inner.into_inner())
            }
            Packing::Zstd => {
                let mut tar = Hashed { inner: zstd::stream::read::Decoder::with_buffer(buffered).map_err(|e| format!("zstd: {e}"))?, hash: Sha256::new() };
                copy(&mut tar, &mut stdin)?;
                (hex(&tar.hash.finalize()), tar.inner.finish())
            }
            Packing::Tar => {
                let mut tar = Hashed { inner: buffered, hash: Sha256::new() };
                copy(&mut tar, &mut stdin)?;
                (hex(&tar.hash.finalize()), tar.inner)
            }
        })
    })();
    drop(stdin);
    let status = child.wait().map_err(|e| format!("{mkfs}: {e}"))?;
    let said = said.join().unwrap_or_default();
    let fail = |e: String| { let _ = std::fs::remove_file(out); Err(e) };
    let (diff_id, mut rest) = match streamed { Ok(s) => s, Err(e) => return fail(e) };
    if let Err(e) = std::io::copy(&mut rest, &mut std::io::sink()) { return fail(format!("reading the blob: {e}")) }
    if !status.success() { return fail(format!("mkfs.erofs refused the layer ({status}): {}", said.trim().lines().last().unwrap_or_default())) }
    let taken = rest.into_inner();
    if let Some(mut f) = taken.keep { if let Err(e) = f.flush() { return fail(e.to_string()) } }
    Ok(Made { digest: format!("sha256:{}", hex(&taken.hash.finalize())), size: taken.bytes, diff_id: format!("sha256:{diff_id}") })
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder().http_status_as_error(false).max_redirects(10).timeout_connect(Some(Duration::from_secs(20))).timeout_recv_body(Some(Duration::from_secs(600))).build().into()
}

/// A file's SHA-256, read from disk.
fn file_sha256(path: &std::path::Path) -> Result<String, String> {
    let mut f = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut h = Sha256::new();
    std::io::copy(&mut f, &mut h).map_err(|e| e.to_string())?;
    Ok(format!("sha256:{}", hex(&h.finalize())))
}

fn said(r: superci_core::io::Response, what: &str) -> Result<superci_core::io::Response, String> {
    if r.status < 300 { Ok(r) } else { Err(format!("{what}: S3 answered {} {}", r.status, r.body_text().chars().take(300).collect::<String>())) }
}

/// A file put in the bucket: in one request when small, else in parts, four at once.
fn upload(http: &(dyn Http + Sync), creds: &Credentials, region: &str, url: &str, path: &std::path::Path) -> Result<(), String> {
    let size = std::fs::metadata(path).map_err(|e| e.to_string())?.len();
    let now = super::now_ms;
    if size <= PART as u64 {
        let body = std::fs::read(path).map_err(|e| e.to_string())?;
        said(block_on(aws::s3(http, "PUT", url, region, creds, body, &[], now()))?, "putting the file")?;
        return Ok(())
    }
    let made = said(block_on(aws::s3(http, "POST", &format!("{url}?uploads"), region, creds, vec![], &[], now()))?, "beginning the upload")?;
    let text = made.body_text();
    let id = text.split("<UploadId>").nth(1).and_then(|s| s.split("</UploadId>").next()).ok_or("S3 gave no upload id")?.to_string();
    let parts = size.div_ceil(PART as u64) as usize;
    let tags: std::sync::Mutex<Vec<(usize, String)>> = std::sync::Mutex::new(vec![]);
    let next = std::sync::atomic::AtomicUsize::new(0);
    let failed: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);
    std::thread::scope(|s| {
        for _ in 0..4 {
            s.spawn(|| loop {
                let n = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                if n >= parts || failed.lock().unwrap().is_some() { break }
                let one = (|| -> Result<String, String> {
                    use std::io::{Seek, SeekFrom};
                    let mut f = std::fs::File::open(path).map_err(|e| e.to_string())?;
                    f.seek(SeekFrom::Start((n * PART) as u64)).map_err(|e| e.to_string())?;
                    let mut body = Vec::with_capacity(PART);
                    f.take(PART as u64).read_to_end(&mut body).map_err(|e| e.to_string())?;
                    let mut last = String::new();
                    for _ in 0..3 {
                        match block_on(aws::s3(http, "PUT", &format!("{url}?partNumber={}&uploadId={}", n + 1, aws::rfc3986(&id)), region, creds, body.clone(), &[], now())) {
                            Ok(r) if r.status < 300 => return r.header("etag").map(str::to_string).ok_or_else(|| "S3 gave no ETag".to_string()),
                            Ok(r) => last = format!("S3 answered {}", r.status),
                            Err(e) => last = e,
                        }
                    }
                    Err(format!("part {}: {last}", n + 1))
                })();
                match one { Ok(tag) => tags.lock().unwrap().push((n + 1, tag)), Err(e) => { *failed.lock().unwrap() = Some(e); break } }
            });
        }
    });
    if let Some(e) = failed.into_inner().unwrap() {
        let _ = block_on(aws::s3(http, "DELETE", &format!("{url}?uploadId={}", aws::rfc3986(&id)), region, creds, vec![], &[], now()));
        return Err(e)
    }
    let mut tags = tags.into_inner().unwrap();
    tags.sort();
    let xml = format!("<CompleteMultipartUpload>{}</CompleteMultipartUpload>", tags.iter().map(|(n, t)| format!("<Part><PartNumber>{n}</PartNumber><ETag>{t}</ETag></Part>")).collect::<String>());
    let done = said(block_on(aws::s3(http, "POST", &format!("{url}?uploadId={}", aws::rfc3986(&id)), region, creds, xml.into_bytes(), &[], now()))?, "finishing the upload")?;
    // (S3 may answer 200 and say in the body that it failed.)
    if done.body_text().contains("<Error>") { return Err(format!("finishing the upload: {}", done.body_text().chars().take(300).collect::<String>())) }
    Ok(())
}

/// Converts one layer (see the module): what was made, as written to `done_key`.
pub fn convert(http: &(dyn Http + Sync), creds: &Credentials, job: &Job, mkfs: &str, tmp: &std::path::Path) -> Value {
    let began = Instant::now();
    let bucket_url = |key: &str| aws::s3_url(&job.bucket, &job.region, key);
    let result = (|| -> Result<Value, String> {
        if job.size > MOST_BYTES { return Err(format!("a blob of {} bytes (converted up to {MOST_BYTES})", job.size)) }
        if !job.digest.starts_with("sha256:") || job.digest.len() != 71 || !job.digest[7..].bytes().all(|b| b.is_ascii_hexdigit()) { return Err("not a sha256 digest".into()) }
        let (url, headers, staged) = match job.source["key"].as_str() {
            Some(key) => (aws::presign("GET", &bucket_url(key), creds, &job.region, 3600, super::now_ms())?, vec![], Some(key.to_string())),
            None => {
                let url = job.source["url"].as_str().ok_or("no source")?.to_string();
                let headers: Vec<(String, String)> = job.source["headers"].as_object().into_iter().flatten().filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string()))).collect();
                (url, headers, None)
            }
        };
        let mut request = agent().get(&superci_core::io::stand_in(&url));
        for (k, v) in &headers { request = request.header(k.as_str(), v.as_str()) }
        let response = request.call().map_err(|e| format!("fetching the blob: {e}"))?;
        if response.status() != 200 { return Err(format!("fetching the blob: HTTP {}", response.status())) }
        let (out, kept) = (tmp.join("layer.erofs"), tmp.join("layer.blob"));
        let _ = std::fs::remove_file(&kept);
        let reader = response.into_body().into_reader().take(job.size + 1);
        let made = make(reader, mkfs, &out, staged.is_none().then_some(kept.as_path()))?;
        if made.digest != job.digest || made.size != job.size { let _ = std::fs::remove_file(&out); return Err(format!("the blob is {} ({} bytes), not {} ({} bytes)", made.digest, made.size, job.digest, job.size)) }
        let erofs_size = std::fs::metadata(&out).map_err(|e| e.to_string())?.len();
        let erofs_digest = file_sha256(&out)?;
        upload(http, creds, &job.region, &bucket_url(&job.erofs_key), &out)?;
        let _ = std::fs::remove_file(&out);
        match &staged {
            // (Within the bucket: S3 copies it, up to 5 GB, larger than any blob converted.)
            Some(key) => {
                let source = format!("/{}/{key}", job.bucket);
                let copied = said(block_on(aws::s3(http, "PUT", &bucket_url(&job.blob_key), &job.region, creds, vec![], &[("x-amz-copy-source", &source)], super::now_ms()))?, "copying the blob")?;
                if copied.body_text().contains("<Error>") { return Err("copying the blob: S3 failed".into()) }
                let _ = block_on(aws::s3(http, "DELETE", &bucket_url(key), &job.region, creds, vec![], &[], super::now_ms()));
            }
            None => { upload(http, creds, &job.region, &bucket_url(&job.blob_key), &kept)?; let _ = std::fs::remove_file(&kept); }
        }
        Ok(json!({ "digest": job.digest, "size": job.size, "diff_id": made.diff_id, "erofs": { "key": job.erofs_key, "size": erofs_size, "digest": erofs_digest }, "blob": { "key": job.blob_key, "size": job.size } }))
    })();
    let _ = std::fs::remove_file(tmp.join("layer.erofs"));
    let _ = std::fs::remove_file(tmp.join("layer.blob"));
    let mut done = result.unwrap_or_else(|e| json!({ "digest": job.digest, "error": e }));
    done["seconds"] = json!((began.elapsed().as_secs_f64() * 10.0).round() / 10.0);
    if let Err(e) = block_on(aws::s3(http, "PUT", &bucket_url(&job.done_key), &job.region, creds, done.to_string().into_bytes(), &[("content-type", "application/json")], super::now_ms())).map_err(|e| e.to_string()).and_then(|r| said(r, "writing what was made").map(|_| ())) {
        done["unsaid"] = json!(e);
    }
    done
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stand-in for `mkfs.erofs`: what it is given, written where it is told (the last argument), and its options
    /// checked; it refuses a tar that says "refuse".
    fn fake_mkfs(dir: &std::path::Path) -> String {
        let p = dir.join("mkfs.erofs");
        std::fs::write(&p, format!("#!/bin/sh\n[ \"$1 $2 $3 $4 $5 $6 $7 $8 $9\" = \"{}\" ] || exit 9\nfor a; do out=$a; done\ncat > \"$out\"\nif grep -q refuse \"$out\"; then echo 'unusual entry' >&2; exit 1; fi\n", MKFS.join(" "))).unwrap();
        #[cfg(unix)] { use std::os::unix::fs::PermissionsExt; std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap() }
        p.to_string_lossy().into_owned()
    }

    fn tmp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("superci-convert-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn sha(b: &[u8]) -> String { format!("sha256:{}", hex(&Sha256::digest(b))) }

    #[test]
    fn a_blob_is_unpacked_as_its_first_bytes_say_and_hashed_before_and_after() {
        let d = tmp("packing");
        let mkfs = fake_mkfs(&d);
        let tar = b"a tar's bytes, as a layer has them".repeat(1000);
        let gz = { let mut e = flate2::write::GzEncoder::new(vec![], flate2::Compression::fast()); e.write_all(&tar).unwrap(); e.finish().unwrap() };
        // Two gzip members one after the other (as some tools write them), and zstd frames with a skippable one.
        let two = [gz.clone(), { let mut e = flate2::write::GzEncoder::new(vec![], flate2::Compression::fast()); e.write_all(b"more").unwrap(); e.finish().unwrap() }].concat();
        let zst = [zstd::encode_all(&tar[..5000], 3).unwrap(), vec![0x50, 0x2a, 0x4d, 0x18, 4, 0, 0, 0, 1, 2, 3, 4], zstd::encode_all(&tar[5000..], 3).unwrap()].concat();
        for (blob, whole) in [(gz.clone(), tar.clone()), (two.clone(), [tar.clone(), b"more".to_vec()].concat()), (zst.clone(), tar.clone()), (tar.clone(), tar.clone())] {
            let (out, keep) = (d.join("out"), d.join("blob"));
            let made = make(&blob[..], &mkfs, &out, Some(&keep)).unwrap();
            assert_eq!(made, Made { digest: sha(&blob), size: blob.len() as u64, diff_id: sha(&whole) });
            assert_eq!((std::fs::read(&out).unwrap() == whole, std::fs::read(&keep).unwrap() == blob), (true, true));
        }
        assert_eq!((packing(&gz), packing(&zst), packing(&tar)), (Packing::Gzip, Packing::Zstd, Packing::Tar));
    }

    #[test]
    fn a_layer_mkfs_refuses_or_a_broken_blob_leaves_nothing() {
        let d = tmp("refused");
        let mkfs = fake_mkfs(&d);
        let out = d.join("out");
        let e = make(&b"a tar to refuse"[..], &mkfs, &out, None).unwrap_err();
        assert!(e.contains("mkfs.erofs refused the layer") && e.contains("unusual entry") && !out.exists(), "{e}");
        let gz = { let mut e = flate2::write::GzEncoder::new(vec![], flate2::Compression::fast()); e.write_all(&[7u8; 100_000]).unwrap(); e.finish().unwrap() };
        let e = make(&gz[..gz.len() / 2], &mkfs, &out, None).unwrap_err();
        assert!(e.contains("unpacking") && !out.exists(), "{e}");
        assert!(make(&b"x"[..], "/no/such/mkfs", &out, None).unwrap_err().contains("/no/such/mkfs"));
    }

    /// With the real `mkfs.erofs` when there is one (`SUPERCI_MKFS_EROFS`, as the layers job in CI sets): a tar with a
    /// whiteout and an opaque folder makes a file that `fsck.erofs` takes, the same one every time.
    #[test]
    fn the_real_mkfs_makes_the_same_file_from_the_same_layer() {
        let Ok(mkfs) = std::env::var("SUPERCI_MKFS_EROFS") else { return };
        let d = tmp("real");
        let src = d.join("src");
        std::fs::create_dir_all(src.join("etc/opaque")).unwrap();
        std::fs::write(src.join("etc/hello"), "hello\n").unwrap();
        std::fs::write(src.join("etc/.wh.gone"), "").unwrap();
        std::fs::write(src.join("etc/opaque/.wh..wh..opq"), "").unwrap();
        let tar = Command::new("tar").args(["-C", src.to_str().unwrap(), "--owner=0", "--group=0", "-czf", "-", "etc"]).output().unwrap().stdout;
        let (a, b) = (d.join("a.erofs"), d.join("b.erofs"));
        let first = make(&tar[..], &mkfs, &a, None).unwrap();
        make(&tar[..], &mkfs, &b, None).unwrap();
        assert_eq!(first.digest, sha(&tar));
        assert_eq!(std::fs::read(&a).unwrap(), std::fs::read(&b).unwrap(), "the same layer makes the same file");
        if let Ok(fsck) = std::env::var("SUPERCI_FSCK_EROFS") { assert!(Command::new(fsck).arg(&a).status().unwrap().success()) }
    }
}

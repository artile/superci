//! A file fetched from the store's bucket onto this machine's disk: ranges of 8 MiB at once, each written where it
//! goes as it arrives (nothing big held in memory), each tried again if it fails or stalls, the link signed again
//! when it has run out; then the whole file checked against its digest.

use crate::content::{Algorithm, Digest};
use std::io::Read;
use std::os::unix::fs::FileExt;
use std::path::Path;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Duration;

pub const PART: u64 = 8 << 20;
const TRIES: u32 = 5;

/// Where a file is, and how to get a fresh link to it when the one held is refused (expired).
pub trait Link: Sync {
    fn url(&self) -> String;
    /// A new link, if one can be had; false when not.
    fn refresh(&self) -> bool;
}

/// A fixed link (tests; a link that never runs out).
pub struct Fixed(pub String);
impl Link for Fixed {
    fn url(&self) -> String { self.0.clone() }
    fn refresh(&self) -> bool { false }
}

pub fn agent() -> ureq::Agent {
    ureq::Agent::config_builder().http_status_as_error(false).timeout_connect(Some(Duration::from_secs(10)))
        // (A range that sends nothing for this long is asked for again.)
        .timeout_recv_body(Some(Duration::from_secs(20))).timeout_recv_response(Some(Duration::from_secs(20)))
        .build().into()
}

/// `size` bytes from `link` into `dest` (made or replaced), `parallel` ranges at once; `digest` (`sha256:…`), when
/// given, checked over the whole file, which is removed if it does not match. What was read, for counting.
pub fn fetch(agent: &ureq::Agent, link: &dyn Link, size: u64, dest: &Path, parallel: usize, digest: Option<&str>) -> Result<u64, String> {
    let r = fetch_inner(agent, link, size, dest, parallel, digest);
    if r.is_err() { let _ = std::fs::remove_file(dest); }
    r
}

fn fetch_inner(agent: &ureq::Agent, link: &dyn Link, size: u64, dest: &Path, parallel: usize, digest: Option<&str>) -> Result<u64, String> {
    let want = digest.map(Digest::parse).transpose().map_err(|e| format!("the store's digest: {e}"))?;
    let file = std::fs::File::create(dest).map_err(|e| format!("{}: {e}", dest.display()))?;
    file.set_len(size).map_err(|e| format!("{}: {e}", dest.display()))?;
    let parts = size.div_ceil(PART) as usize;
    let next = AtomicUsize::new(0);
    let read = AtomicU64::new(0);
    let failed: Mutex<Option<String>> = Mutex::new(None);
    // (One refresh at a time: the first range refused gets the new link, the others use it.)
    let refreshing = Mutex::new(());
    std::thread::scope(|s| {
        for _ in 0..parallel.clamp(1, parts.max(1)) {
            s.spawn(|| loop {
                if failed.lock().unwrap().is_some() { return }
                let i = next.fetch_add(1, Ordering::Relaxed);
                if i >= parts { return }
                let (from, to) = (i as u64 * PART, ((i as u64 + 1) * PART).min(size));
                match range(agent, link, &file, from, to, &refreshing) {
                    Ok(n) => { read.fetch_add(n, Ordering::Relaxed); }
                    Err(e) => { failed.lock().unwrap().get_or_insert(e); return }
                }
            });
        }
    });
    if let Some(e) = failed.into_inner().unwrap() { return Err(e) }
    file.sync_all().map_err(|e| format!("{}: {e}", dest.display()))?;
    if let Some(want) = want {
        let got = hash_file(dest, want.algorithm).map_err(|e| format!("{}: {e}", dest.display()))?;
        if got != want { return Err(format!("the store's file is not what its digest says ({got}, not {want})")) }
    }
    Ok(read.into_inner())
}

/// One range, tried again until it comes whole.
fn range(agent: &ureq::Agent, link: &dyn Link, file: &std::fs::File, from: u64, to: u64, refreshing: &Mutex<()>) -> Result<u64, String> {
    let mut last = String::new();
    let mut refreshed = 0;
    for attempt in 0..TRIES {
        if attempt > 0 { std::thread::sleep(Duration::from_millis(200 * attempt as u64)) }
        let url = link.url();
        let res = agent.get(&url).header("range", format!("bytes={from}-{}", to - 1)).call();
        let r = match res { Ok(r) => r, Err(e) => { last = e.to_string(); continue } };
        let status = r.status().as_u16();
        // A link that has run out (S3 answers 403 or 400): a new one, once per range and once for all at a time.
        if matches!(status, 400 | 401 | 403) {
            last = format!("the store refused its link ({status})");
            if refreshed < 2 {
                refreshed += 1;
                let _one = refreshing.lock().unwrap();
                if link.url() == url && !link.refresh() { return Err(last) }
            }
            continue;
        }
        if !(status == 206 || (status == 200 && from == 0)) { last = format!("the store answered {status}"); continue }
        let mut body = r.into_body().into_reader();
        let mut buf = vec![0u8; 1 << 20];
        let mut at = from;
        let ok = loop {
            if at >= to { break true }
            match body.read(&mut buf[..((to - at) as usize).min(1 << 20)]) {
                Ok(0) => break false,
                Ok(n) => { if let Err(e) = file.write_all_at(&buf[..n], at) { return Err(e.to_string()) } at += n as u64 }
                Err(e) => { last = e.to_string(); break false }
            }
        };
        if ok { return Ok(to - from) }
        if last.is_empty() { last = format!("the store sent {} of {} bytes", at - from, to - from) }
    }
    Err(format!("bytes {from}-{} from the store: {last}", to - 1))
}

pub fn hash_file(p: &Path, algorithm: Algorithm) -> std::io::Result<Digest> {
    let mut f = std::fs::File::open(p)?;
    let mut h = algorithm.hasher();
    let mut buf = vec![0u8; 1 << 20];
    loop { let n = f.read(&mut buf)?; if n == 0 { break } h.update(&buf[..n]) }
    Ok(h.digest())
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::sync::Arc;

    /// A bucket for tests: serves `data` by ranges on a local port; the paths it refuses (403) or cuts short.
    pub struct Bucket { pub url: String, pub hits: Arc<AtomicUsize> }

    pub fn bucket(files: Vec<(&'static str, Vec<u8>)>, refuse: Option<&'static str>, cut_first: bool) -> Bucket {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", l.local_addr().unwrap());
        let hits = Arc::new(AtomicUsize::new(0));
        let h = hits.clone();
        let files = Arc::new(files);
        let cut = Arc::new(std::sync::atomic::AtomicBool::new(cut_first));
        std::thread::spawn(move || {
            for c in l.incoming() {
                let Ok(mut c) = c else { continue };
                let (files, h, cut) = (files.clone(), h.clone(), cut.clone());
                std::thread::spawn(move || {
                    let mut r = BufReader::new(c.try_clone().unwrap());
                    loop {
                        let mut line = String::new();
                        if r.read_line(&mut line).unwrap_or(0) == 0 { return }
                        let path = line.split_whitespace().nth(1).unwrap_or("").to_string();
                        let mut range = None;
                        loop {
                            let mut hl = String::new();
                            r.read_line(&mut hl).unwrap();
                            if hl.trim().is_empty() { break }
                            if let Some(v) = hl.to_ascii_lowercase().strip_prefix("range: bytes=") {
                                let (a, b) = v.trim().split_once('-').unwrap();
                                range = Some((a.parse::<usize>().unwrap(), b.parse::<usize>().unwrap()));
                            }
                        }
                        h.fetch_add(1, Ordering::Relaxed);
                        if refuse.is_some_and(|p| path == p) { let _ = c.write_all(b"HTTP/1.1 403 Forbidden\r\ncontent-length: 0\r\n\r\n"); continue }
                        let Some((_, data)) = files.iter().find(|(p, _)| *p == path) else { let _ = c.write_all(b"HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\n\r\n"); continue };
                        let (a, b) = range.unwrap_or((0, data.len() - 1));
                        let body = &data[a..=b];
                        let _ = write!(c, "HTTP/1.1 206 Partial Content\r\ncontent-length: {}\r\ncontent-range: bytes {a}-{b}/{}\r\n\r\n", body.len(), data.len());
                        if cut.swap(false, Ordering::Relaxed) { let _ = c.write_all(&body[..body.len() / 2]); return }
                        let _ = c.write_all(body);
                    }
                });
            }
        });
        Bucket { url, hits }
    }

    fn data(n: usize) -> Vec<u8> { (0..n).map(|i| (i * 7 % 251) as u8).collect() }
    fn tmp(name: &str) -> std::path::PathBuf { std::env::temp_dir().join(format!("superci-layers-fetch-{}-{name}", std::process::id())) }

    #[test]
    fn a_file_comes_in_ranges_at_once_checked_against_its_digest() {
        let d = data((PART * 2 + 12345) as usize);
        let digest = Digest::of(Algorithm::Sha256, &d).to_string();
        let b = bucket(vec![("/f", d.clone())], None, true);
        let p = tmp("f");
        let n = fetch(&agent(), &Fixed(format!("{}/f", b.url)), d.len() as u64, &p, 4, Some(&digest)).unwrap();
        assert_eq!((n, std::fs::read(&p).unwrap() == d), (d.len() as u64, true), "a range cut short is asked again");
        assert!(b.hits.load(Ordering::Relaxed) >= 4, "three ranges and the one asked again");
        // Another digest: the file is not kept.
        let wrong = Digest::of(Algorithm::Sha256, b"x").to_string();
        assert!(fetch(&agent(), &Fixed(format!("{}/f", b.url)), d.len() as u64, &p, 4, Some(&wrong)).unwrap_err().contains("is not what its digest says"));
        assert!(!p.exists());
        // Nothing there: said, and no file left.
        assert!(fetch(&agent(), &Fixed(format!("{}/none", b.url)), 10, &p, 4, None).unwrap_err().contains("404"));
        assert!(!p.exists());
        // An empty file needs no request.
        fetch(&agent(), &Fixed(format!("{}/none", b.url)), 0, &p, 4, None).unwrap();
        assert_eq!(std::fs::read(&p).unwrap().len(), 0);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn a_refused_link_is_signed_again_once_for_all_ranges() {
        struct Signed { url: Mutex<String>, fresh: String, refreshes: AtomicUsize }
        impl Link for Signed {
            fn url(&self) -> String { self.url.lock().unwrap().clone() }
            fn refresh(&self) -> bool { self.refreshes.fetch_add(1, Ordering::Relaxed); *self.url.lock().unwrap() = self.fresh.clone(); true }
        }
        let d = data((PART * 3) as usize);
        let b = bucket(vec![("/new", d.clone())], Some("/old"), false);
        let link = Signed { url: Mutex::new(format!("{}/old", b.url)), fresh: format!("{}/new", b.url), refreshes: AtomicUsize::new(0) };
        let p = tmp("signed");
        fetch(&agent(), &link, d.len() as u64, &p, 3, None).unwrap();
        assert_eq!((std::fs::read(&p).unwrap() == d, link.refreshes.load(Ordering::Relaxed)), (true, 1));
        // A link that cannot be signed again: refused, said.
        assert!(fetch(&agent(), &Fixed(format!("{}/old", b.url)), 5, &p, 1, None).unwrap_err().contains("refused its link (403)"));
        let _ = std::fs::remove_file(&p);
    }
}

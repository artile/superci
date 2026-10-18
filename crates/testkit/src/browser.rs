//! Chrome, headless, driven as a person would use a page: it goes to an address, clicks, types and submits, and the
//! page's own scripts run as they do for anyone. Spoken to by Chrome's DevTools protocol over a pipe
//! (`--remote-debugging-pipe`: its descriptors 3 and 4, a message each ending in a NUL), so nothing more is needed
//! than Chrome itself. `None` where no Chrome is installed.
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::os::unix::io::FromRawFd;
use std::os::unix::process::CommandExt;
use std::sync::mpsc::{channel, Receiver};
use std::time::{Duration, Instant};

pub struct Browser { child: std::process::Child, to: std::fs::File, from: Receiver<Value>, id: u64, session: String, _profile: TempDir }

struct TempDir(std::path::PathBuf);
impl Drop for TempDir { fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); } }

fn chrome() -> Option<&'static str> {
    ["/Applications/Google Chrome.app/Contents/MacOS/Google Chrome", "/usr/bin/google-chrome", "/usr/bin/google-chrome-stable", "/usr/bin/chromium", "/usr/bin/chromium-browser"]
        .into_iter().find(|p| std::path::Path::new(p).exists())
}

fn pipe() -> (i32, i32) {
    let mut fds = [0; 2];
    assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
    (fds[0], fds[1])
}

impl Browser {
    /// A fresh Chrome (a profile of its own) with one tab, `width` × `height`.
    pub fn open(width: u32, height: u32) -> Option<Browser> {
        let chrome = chrome()?;
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let profile = TempDir(std::env::temp_dir().join(format!("superci-chrome-{}-{}", std::process::id(), N.fetch_add(1, std::sync::atomic::Ordering::SeqCst))));
        let ((their_in, our_out), (our_in, their_out)) = (pipe(), pipe());
        // (Ours are not handed on to Chrome; its two become its 3 and 4.)
        unsafe { libc::fcntl(our_out, libc::F_SETFD, libc::FD_CLOEXEC); libc::fcntl(our_in, libc::F_SETFD, libc::FD_CLOEXEC); }
        let mut cmd = std::process::Command::new(chrome);
        cmd.args(["--headless=new", "--remote-debugging-pipe", "--disable-gpu", "--no-first-run", "--no-default-browser-check", "--hide-scrollbars", "--mute-audio",
            &format!("--window-size={width},{height}"), &format!("--user-data-dir={}", profile.0.display()), "about:blank"])
            .stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
        unsafe { cmd.pre_exec(move || { if libc::dup2(their_in, 3) < 0 || libc::dup2(their_out, 4) < 0 { return Err(std::io::Error::last_os_error()) } Ok(()) }); }
        let child = cmd.spawn().ok()?;
        unsafe { libc::close(their_in); libc::close(their_out); }
        let (to, mut from_chrome) = unsafe { (std::fs::File::from_raw_fd(our_out), std::fs::File::from_raw_fd(our_in)) };
        let (tx, from) = channel();
        std::thread::spawn(move || {
            let (mut buf, mut chunk) = (Vec::new(), [0u8; 65536]);
            while let Ok(n) = from_chrome.read(&mut chunk) {
                if n == 0 { break }
                buf.extend_from_slice(&chunk[..n]);
                while let Some(end) = buf.iter().position(|b| *b == 0) {
                    let msg: Vec<u8> = buf.drain(..=end).collect();
                    if let Ok(v) = serde_json::from_slice::<Value>(&msg[..msg.len() - 1]) { if tx.send(v).is_err() { return } }
                }
            }
        });
        let mut b = Browser { child, to, from, id: 0, session: String::new(), _profile: profile };
        let target = b.call("Target.createTarget", json!({ "url": "about:blank" })).ok()?["targetId"].clone();
        b.session = b.call("Target.attachToTarget", json!({ "targetId": target, "flatten": true })).ok()?["sessionId"].as_str()?.to_string();
        b.call("Page.enable", json!({})).ok()?;
        b.call("Runtime.enable", json!({})).ok()?;
        Some(b)
    }

    /// One command, to the tab (once attached), and its result.
    pub fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
        self.id += 1;
        let mut msg = json!({ "id": self.id, "method": method, "params": params });
        if !self.session.is_empty() { msg["sessionId"] = json!(self.session) }
        self.to.write_all(format!("{msg}\0").as_bytes()).map_err(|e| e.to_string())?;
        let until = Instant::now() + Duration::from_secs(30);
        loop {
            let v = self.from.recv_timeout(until.saturating_duration_since(Instant::now())).map_err(|_| format!("Chrome did not answer {method}"))?;
            if v["id"] == self.id {
                if let Some(e) = v.get("error") { return Err(format!("{method}: {e}")) }
                return Ok(v["result"].clone());
            }
        }
    }

    /// Goes to `url` and waits for its page to have loaded.
    pub fn goto(&mut self, url: &str) {
        self.call("Page.navigate", json!({ "url": url })).unwrap();
        std::thread::sleep(Duration::from_millis(100));
        self.wait("document.readyState === 'complete'");
    }

    /// What `js` comes to in the page (a promise waited for).
    pub fn eval(&mut self, js: &str) -> Value {
        let r = self.call("Runtime.evaluate", json!({ "expression": js, "returnByValue": true, "awaitPromise": true })).unwrap();
        if let Some(e) = r.get("exceptionDetails") { panic!("{js}: {}", e["exception"]["description"].as_str().unwrap_or(&e.to_string())) }
        r["result"]["value"].clone()
    }

    /// Waits, up to 30 seconds (a page among many tests at once, on a busy machine), for `js` to hold in the page.
    pub fn wait(&mut self, js: &str) {
        let until = Instant::now() + Duration::from_secs(30);
        while Instant::now() < until {
            if self.call("Runtime.evaluate", json!({ "expression": format!("!!({js})"), "returnByValue": true })).is_ok_and(|r| r["result"]["value"] == true) { return }
            std::thread::sleep(Duration::from_millis(100));
        }
        let shown = self.text("main");
        panic!("waited for {js}; the page shows: {}", shown.chars().take(2000).collect::<String>());
    }

    /// The words an element shows (the first that `selector` names; nothing when there is none).
    pub fn text(&mut self, selector: &str) -> String {
        self.eval(&format!("(document.querySelector({0}) || {{innerText: ''}}).innerText", json!(selector))).as_str().unwrap_or_default().split_whitespace().collect::<Vec<_>>().join(" ")
    }

    /// Clicks the first element `selector` names whose words contain `words` (any, when empty), as a person does: at
    /// its middle, with the mouse.
    /// (Once it is there, holds still and is what is at that point: a page still laying itself out, a card coming in
    /// above, would take the click somewhere else.)
    pub fn click(&mut self, selector: &str, words: &str) {
        let find = format!("(() => {{ const e = [...document.querySelectorAll({})].find(e => e.innerText.includes({}) || e.value === {1}); if (!e) return null;
            e.scrollIntoView({{block: 'center'}}); const r = e.getBoundingClientRect(); const x = r.x + r.width / 2, y = r.y + r.height / 2; const at = document.elementFromPoint(x, y);
            return [x, y, !!at && (at === e || e.contains(at) || at.contains(e))]; }})()", json!(selector), json!(words));
        let mut last: Option<[f64; 2]> = None;
        // (Up to 15 seconds, as `wait`: a page loading under a slow, measured build comes late.)
        let until = Instant::now() + Duration::from_secs(30);
        let (x, y) = loop {
            let at = self.eval(&find);
            let Some(a) = at.as_array() else {
                if Instant::now() > until { panic!("nothing to click: {selector} {words:?} on {}", self.text("main")) }
                std::thread::sleep(Duration::from_millis(100));
                continue
            };
            let here = [a[0].as_f64().unwrap(), a[1].as_f64().unwrap()];
            if last == Some(here) && a[2] == true { break (here[0], here[1]) }
            if Instant::now() > until { panic!("not clickable (moving, or under something else): {selector} {words:?} on {}", self.text("main")) }
            last = Some(here);
            std::thread::sleep(Duration::from_millis(100));
        };
        for kind in ["mousePressed", "mouseReleased"] { self.call("Input.dispatchMouseEvent", json!({ "type": kind, "x": x, "y": y, "button": "left", "clickCount": 1 })).unwrap(); }
    }

    /// Types `text` into the field `selector` names, what was there before taken out first.
    pub fn fill(&mut self, selector: &str, text: &str) {
        self.eval(&format!("(() => {{ const e = document.querySelector({}); e.focus(); e.select && e.select(); }})()", json!(selector)));
        self.call("Input.dispatchKeyEvent", json!({ "type": "keyDown", "key": "Backspace", "code": "Backspace", "windowsVirtualKeyCode": 8 })).unwrap();
        self.call("Input.dispatchKeyEvent", json!({ "type": "keyUp", "key": "Backspace", "code": "Backspace", "windowsVirtualKeyCode": 8 })).unwrap();
        self.call("Input.insertText", json!({ "text": text })).unwrap();
    }

    /// The page as a PNG, at full size (for a person to look at).
    pub fn screenshot(&mut self, path: &std::path::Path) {
        let shot = self.call("Page.captureScreenshot", json!({ "format": "png", "captureBeyondViewport": false })).unwrap();
        std::fs::write(path, base64_decode(shot["data"].as_str().unwrap_or_default())).unwrap();
    }
}

impl Drop for Browser {
    fn drop(&mut self) { let _ = self.child.kill(); let _ = self.child.wait(); }
}

fn base64_decode(s: &str) -> Vec<u8> {
    let val = |c: u8| match c { b'A'..=b'Z' => c - b'A', b'a'..=b'z' => c - b'a' + 26, b'0'..=b'9' => c - b'0' + 52, b'+' => 62, _ => 63 };
    let bytes: Vec<u8> = s.bytes().filter(|c| *c != b'=' && !c.is_ascii_whitespace()).collect();
    let mut out = Vec::with_capacity(bytes.len() * 3 / 4);
    for chunk in bytes.chunks(4) {
        let n = chunk.iter().enumerate().fold(0u32, |n, (i, c)| n | (val(*c) as u32) << (18 - 6 * i));
        for i in 0..chunk.len().saturating_sub(1) { out.push((n >> (16 - 8 * i)) as u8) }
    }
    out
}

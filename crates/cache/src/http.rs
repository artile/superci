//! A small HTTP/1.1 server for `superci-cache`: a thread a connection, each request's body read whole (by its length,
//! or chunked) before it is handled, each answer by its length (a download as a stream), and the connection kept for
//! the next request. (What the clients it serves send: GitHub's cache and artifact clients, Azure's blob clients in
//! Node and Go, BuildKit.)
use std::io::{BufRead, BufReader, BufWriter, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;

/// A request's body read whole at most (GitHub's client sends 128 MB at most in one); a larger one, with its length,
/// is the handler's to read as it comes (GitLab's runner sends a whole cache in one).
const WHOLE: u64 = 256 << 20;
/// A chunked body at most (no client of these sends a large one chunked).
const MOST: u64 = 1 << 30;

/// A request; `streamed`: its body (`length` bytes) is not in `body` but the reader the handler is given.
pub struct Request { pub method: String, pub target: String, pub headers: Vec<(String, String)>, pub body: Vec<u8>, pub streamed: bool, pub length: u64 }

impl Request {
    /// A header's value (names compared without case).
    pub fn header(&self, name: &str) -> Option<&str> { self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str()) }
}

pub enum Body { Bytes(Vec<u8>), Stream(Box<dyn Read + Send>) }

pub struct Response { pub status: u16, pub headers: Vec<(String, String)>, pub body: Body, pub length: u64 }

impl Response {
    pub fn bytes(status: u16, kind: &str, body: Vec<u8>) -> Response { Response { status, headers: vec![("content-type".into(), kind.into())], length: body.len() as u64, body: Body::Bytes(body) } }
    /// `length` bytes from `reader` (a `HEAD` gets only the length).
    pub fn stream(status: u16, reader: Box<dyn Read + Send>, length: u64) -> Response { Response { status, headers: vec![], body: Body::Stream(reader), length } }
    pub fn with_headers(mut self, more: Vec<(String, String)>) -> Response { self.headers.extend(more); self }
}

/// Serves for as long as the program runs (a connection that cannot be taken is passed over).
pub fn serve(listener: TcpListener, handle: impl Fn(Request, &mut dyn Read) -> Response + Send + Sync + 'static) -> ! {
    let handle = Arc::new(handle);
    // (`incoming` never ends: the loop is only for the type.)
    loop {
        listener.incoming().flatten().for_each(|stream| {
            let handle = handle.clone();
            std::thread::spawn(move || { let _ = connection(stream, &*handle); });
        })
    }
}

fn connection(stream: TcpStream, handle: &dyn Fn(Request, &mut dyn Read) -> Response) -> std::io::Result<()> {
    let _ = stream.set_nodelay(true);
    // (A socket's second handle: the system makes it as long as it has descriptors to give.)
    let mut reader = BufReader::with_capacity(1 << 16, stream.try_clone().expect("a socket's second handle"));
    let mut writer = BufWriter::with_capacity(1 << 16, stream);
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 { return Ok(()) }
        let line = line.trim_end();
        if line.is_empty() { continue }
        let mut parts = line.split(' ');
        let (method, target, version) = (parts.next().unwrap_or_default().to_string(), parts.next().unwrap_or("/").to_string(), parts.next().unwrap_or("HTTP/1.1").to_string());
        let mut headers = vec![];
        loop {
            let mut h = String::new();
            if reader.read_line(&mut h)? == 0 { return Ok(()) }
            let h = h.trim_end();
            if h.is_empty() { break }
            if let Some((k, v)) = h.split_once(':') { headers.push((k.trim().to_ascii_lowercase(), v.trim().to_string())) }
        }
        let get = |n: &str| headers.iter().find(|(k, _)| k == n).map(|(_, v)| v.clone());
        // (Not said, the connection is broken: reading the body says so.)
        if get("expect").is_some_and(|e| e.eq_ignore_ascii_case("100-continue")) { let _ = writer.write_all(b"HTTP/1.1 100 Continue\r\n\r\n").and_then(|_| writer.flush()); }
        let chunks = get("transfer-encoding").is_some_and(|t| t.to_ascii_lowercase().contains("chunked"));
        let n: u64 = if chunks { 0 } else { get("content-length").and_then(|v| v.parse().ok()).unwrap_or(0) };
        let streamed = n > WHOLE;
        let body = if chunks { chunked(&mut reader)? } else if streamed { vec![] } else {
            let mut b = vec![0u8; n as usize];
            reader.read_exact(&mut b)?;
            b
        };
        let length = if chunks { body.len() as u64 } else { n };
        let mut close = version == "HTTP/1.0" || get("connection").is_some_and(|c| c.eq_ignore_ascii_case("close"));
        let head = method == "HEAD";
        let mut rest = (&mut reader).take(if streamed { n } else { 0 });
        let res = handle(Request { method, target, headers, body, streamed, length }, &mut rest);
        // A streamed body the handler did not read to its end: the connection cannot go on to another request.
        if rest.limit() > 0 { close = true }
        write(&mut writer, res, head, close)?;
        if close { return Ok(()) }
    }
}

fn chunked(reader: &mut impl BufRead) -> std::io::Result<Vec<u8>> {
    let mut body = vec![];
    loop {
        let mut size = String::new();
        if reader.read_line(&mut size)? == 0 { return Err(std::io::Error::other("a chunked body that ended early")) }
        let n = u64::from_str_radix(size.trim().split(';').next().unwrap_or_default().trim(), 16).map_err(std::io::Error::other)?;
        if n == 0 {
            loop { let mut t = String::new(); if reader.read_line(&mut t)? == 0 || t.trim().is_empty() { break } }
            return Ok(body)
        }
        if body.len() as u64 + n > MOST { return Err(std::io::Error::other("a body larger than superci-cache takes")) }
        let at = body.len();
        body.resize(at + n as usize, 0);
        reader.read_exact(&mut body[at..])?;
        let mut end = [0u8; 2];
        reader.read_exact(&mut end)?;
    }
}

/// The answer's head and (to all but a `HEAD`) its body, as one stream: what is not given whole breaks the connection.
fn write(w: &mut impl Write, res: Response, head: bool, close: bool) -> std::io::Result<()> {
    let reason = match res.status { 200 => "OK", 201 => "Created", 206 => "Partial Content", 400 => "Bad Request", 403 => "Forbidden", 404 => "Not Found", 405 => "Method Not Allowed",
        409 => "Conflict", 416 => "Range Not Satisfiable", 500 => "Internal Server Error", 501 => "Not Implemented", 503 => "Service Unavailable", _ => "" };
    let lines: String = res.headers.iter().map(|(k, v)| format!("{k}: {v}\r\n")).collect();
    let top = format!("HTTP/1.1 {} {reason}\r\n{lines}content-length: {}\r\nconnection: {}\r\n\r\n", res.status, res.length, if close { "close" } else { "keep-alive" });
    let length = if head { 0 } else { res.length };
    let body: Box<dyn Read> = match res.body { _ if head => Box::new(std::io::empty()), Body::Bytes(b) => Box::new(std::io::Cursor::new(b)), Body::Stream(r) => Box::new(r.take(length)) };
    if std::io::copy(&mut top.as_bytes().chain(body), w)? < top.len() as u64 + length { return Err(std::io::Error::other("a download broken off")) }
    w.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_by_length_or_chunked_on_one_kept_connection() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let at = listener.local_addr().unwrap();
        std::thread::spawn(move || serve(listener, |r: Request, _: &mut dyn Read| Response::bytes(200, "text/plain", format!("{} {} {}", r.method, r.target, r.body.len()).into_bytes())));
        let mut s = TcpStream::connect(at).unwrap();
        s.write_all(b"PUT /a?x=1 HTTP/1.1\r\nContent-Length: 5\r\nExpect: 100-continue\r\n\r\nhelloPOST /b HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n2\r\nde\r\n0\r\n\r\nHEAD /c HTTP/1.1\r\nConnection: close\r\n\r\n").unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).unwrap();
        assert!(out.starts_with("HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 200 OK\r\n"), "{out}");
        assert!(out.contains("PUT /a?x=1 5") && out.contains("POST /b 5") && out.ends_with("content-length: 9\r\nconnection: close\r\n\r\n"), "{out}");
    }
}

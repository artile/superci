# A bucket for the test: files from a folder by ranges, as S3 serves a signed link; a path with "broken" is refused.
import http.server, os, re, sys

root = sys.argv[1]

class H(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    def log_message(self, *a): pass
    def do_GET(self):
        p = os.path.join(root, self.path.lstrip("/").split("?")[0])
        if "broken" in self.path or not os.path.isfile(p):
            self.send_response(403 if "broken" in self.path else 404); self.send_header("content-length", "0"); self.end_headers(); return
        size = os.path.getsize(p)
        m = re.match(r"bytes=(\d+)-(\d+)", self.headers.get("range", ""))
        a, b = (int(m.group(1)), min(int(m.group(2)), size - 1)) if m else (0, size - 1)
        self.send_response(206 if m else 200)
        self.send_header("content-length", str(b - a + 1))
        if m: self.send_header("content-range", f"bytes {a}-{b}/{size}")
        self.end_headers()
        with open(p, "rb") as f:
            f.seek(a); left = b - a + 1
            while left > 0:
                chunk = f.read(min(left, 1 << 20)); self.wfile.write(chunk); left -= len(chunk)

http.server.ThreadingHTTPServer(("127.0.0.1", 8000), H).serve_forever()

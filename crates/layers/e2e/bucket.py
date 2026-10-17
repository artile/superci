# A bucket and a control plane for the test, on one port.
# The bucket: files from a folder by ranges, as S3 serves a signed link (a path with "broken" is refused); uploads to
# staging/ taken only with a body of the SHA-256 the x-amz-checksum-sha256 header says, as a signed upload link does.
# The control plane: links to layers' files (POST /links, from the index with links at ROOT/index.links.json),
# proposals (POST /propose: every layer asked to be uploaded) and uploads said done (POST /uploaded), noted in
# ROOT/uploaded.txt as "digest size".
import base64, hashlib, http.server, json, os, re, sys

root = sys.argv[1]
asked = {}

class H(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    def log_message(self, *a): pass
    def answer(self, status, body=b"", kind="application/json"):
        self.send_response(status); self.send_header("content-type", kind); self.send_header("content-length", str(len(body))); self.end_headers(); self.wfile.write(body)
    def body(self):
        return self.rfile.read(int(self.headers.get("content-length", "0")))
    def do_POST(self):
        b = json.loads(self.body() or b"{}")
        if self.path == "/links":
            full = json.load(open(os.path.join(root, "index.links.json")))["layers"]
            self.answer(200, json.dumps({"layers": {d: full[d] for d in b["digests"] if d in full}}).encode()); return
        if self.path == "/propose":
            out = {}
            for l in b["layers"]:
                h = l["digest"].split(":")[1]
                sources = [f'{s["registry"]}/{s["repository"]}' for s in l.get("sources", [])]
                asked[h] = (l["digest"], l["size"])
                out[l["digest"]] = {"do": "upload", "url": f"http://127.0.0.1:8000/staging/{h}", "id": h, "headers": {"x-amz-checksum-sha256": base64.b64encode(bytes.fromhex(h)).decode()}}
                with open(os.path.join(root, "proposed.txt"), "a") as f: f.write(f'{l["digest"]} {l["size"]} {",".join(sources)}\n')
            self.answer(200, json.dumps({"layers": out, "uploaded": "http://127.0.0.1:8000/uploaded"}).encode()); return
        if self.path == "/uploaded":
            h = b["id"]; p = os.path.join(root, "staging", h)
            if h not in asked or not os.path.isfile(p) or os.path.getsize(p) != asked[h][1]: self.answer(409, b"nothing there"); return
            with open(os.path.join(root, "uploaded.txt"), "a") as f: f.write(f"{asked[h][0]} {asked[h][1]}\n")
            self.answer(200, b'{"do": "converting"}'); return
        self.answer(404)
    def do_PUT(self):
        m = re.fullmatch(r"/staging/([0-9a-f]{64})", self.path)
        data = self.body()
        if not m or len(data) != int(self.headers.get("content-length", "-1")): self.answer(400); return
        if base64.b64encode(hashlib.sha256(data).digest()).decode() != self.headers.get("x-amz-checksum-sha256"): self.answer(400, b"BadDigest"); return
        os.makedirs(os.path.join(root, "staging"), exist_ok=True)
        with open(os.path.join(root, "staging", m.group(1)), "wb") as f: f.write(data)
        self.answer(200)
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

#!/usr/bin/env python3
"""Makes test fixtures of recordings of what Cloudflare's API answered (a debug build with SUPERCI_RECORD=FILE writes one).

    scripts/sanitize-cloudflare.py crates/cli/fixtures/cloudflare/SITUATION RECORDING:NAME [...] -r REAL=STAND-IN [...]

As `sanitize-recording.py`, and for Cloudflare: the account's id and name, its workers.dev subdomain (each given with
-r), every other 32-hex id (namespaces, tags, etags) by a stand-in, the secrets written to a Worker and the container
registry's credentials blanked, and Workers that are not a control plane's or a runner agent's left out of a listing.
AWS's calls in the same recording are sanitized as `sanitize-recording.py` does, SSM's left out. Modal's calls (protobuf, in base64) get only the replacements of the same length (as in
`sanitize-modal.py`; one of another length found there stops it), and Modal's session token is blanked.
Look through the result before keeping it.
"""
import json, re, sys, base64, urllib.parse, importlib.util

spec = importlib.util.spec_from_file_location("aws_sanitize", __file__.replace("sanitize-cloudflare.py", "sanitize-recording.py"))
aws_sanitize = importlib.util.module_from_spec(spec)
spec.loader.exec_module(aws_sanitize)

def ours(name):
    return name == "superci" or name.startswith("superci-plane-") or name.startswith("superci-runners-")

def tidy(x):
    """What is not to be kept, taken out before ids are replaced."""
    url = x["url"]
    try:
        answer = json.loads(x["answer"])
    except ValueError:
        answer = None
    if isinstance(answer, dict):
        if url.endswith("/workers/scripts") and isinstance(answer.get("result"), list):
            answer["result"] = [s for s in answer["result"] if ours(s.get("id", ""))]
        if "/durable_objects/namespaces" in url and isinstance(answer.get("result"), list):
            answer["result"] = [n for n in answer["result"] if ours(n.get("script") or "")]
        if url.split("?")[0].endswith("/containers/applications") and isinstance(answer.get("result"), list):
            answer["result"] = [a for a in answer["result"] if (a.get("name") or "").startswith("superci")]
        # (A registry's token for its realm.)
        if isinstance(answer, dict):
            for k in ("token", "access_token"):
                if k in answer:
                    answer[k] = "(removed)"
        if "/containers/registries/" in url and isinstance(answer.get("result"), dict):
            for k in list(answer["result"]):
                if k in ("username", "password", "token", "registry_token"):
                    answer["result"][k] = "(removed)"
        x["answer"] = json.dumps(answer, separators=(",", ":"))
    if url.endswith("/secrets") and x["asked"].startswith("{"):
        asked = json.loads(x["asked"])
        if "text" in asked:
            asked["text"] = "(removed)"
        x["asked"] = json.dumps(asked, separators=(",", ":"))
    return x

def main():
    args = sys.argv[1:]
    more = []
    while "-r" in args:
        i = args.index("-r")
        more.append(args[i + 1].split("=", 1))
        del args[i:i + 2]
    out, recordings = args[0], args[1:]
    of = aws_sanitize.stand_ins()
    for recording in recordings:
        path, name = recording.rsplit(":", 1)
        with open(f"{out}/{name}.jsonl", "w") as to:
            for line in open(path):
                x = json.loads(line)
                # (AWS in the same recording: SSM is the test kit's own, and an answer read with decryption is never
                # kept; an upload, a function's code say, by its size only.)
                if x.get("target", "").startswith("AmazonSSM."):
                    continue
                # (A listing of the account's roles: only SuperCI's and AWS's own service roles stay.)
                if "Action=ListRoles" in x.get("asked", "") + x["url"]:
                    x["answer"] = re.sub(r"<member>(?:(?!</member>).)*</member>", lambda m: m.group(0) if re.search(r"<RoleName>(superci-plane-|AWSServiceRoleFor)", m.group(0)) else "", x["answer"], flags=re.S)
                if len(x.get("asked", "")) > 100_000:
                    x["asked"] = f"({len(x['asked'])} bytes)"
                if "api.modal.com" in x["url"]:
                    answer = json.loads(x["answer"])
                    if x["url"].endswith("/AuthTokenGet") and "answers" in answer:
                        answer["answers"] = [base64.b64encode(b"\x0a\x09(removed)").decode()]
                    out_answers = []
                    for a in answer.get("answers", []):
                        b = base64.b64decode(a)
                        for real, stand_in in more:
                            if real.encode() in b and len(real) != len(stand_in):
                                sys.exit(f"{real} is in Modal's answer, and {stand_in} is not of its length")
                            b = b.replace(real.encode(), stand_in.encode())
                        out_answers.append(base64.b64encode(b).decode())
                    if "answers" in answer:
                        answer["answers"] = out_answers
                    if "error" in answer:
                        for real, stand_in in more:
                            answer["error"] = answer["error"].replace(real, stand_in)
                    x["answer"] = json.dumps(answer, separators=(",", ":"))
                    to.write(json.dumps(x, separators=(",", ":"), sort_keys=True) + "\n")
                    continue
                x = tidy(x)
                for k in ("url", "target", "asked", "answer"):
                    for real, stand_in in more:
                        # (As it is, and URL-encoded, as a form or a query carries it.)
                        x[k] = x[k].replace(real, stand_in).replace(urllib.parse.quote(real, safe=""), urllib.parse.quote(stand_in, safe="")).replace(urllib.parse.quote_plus(real), urllib.parse.quote_plus(stand_in))
                    x[k] = aws_sanitize.sanitize(x[k], of)
                    x[k] = re.sub(r"(?<![0-9a-f])[0-9a-f]{32}(?![0-9a-f])", lambda m: m.group(0) if set(m.group(0)) <= set("1a") else of("hex32", m.group(0), lambda n: f"{n:032x}"), x[k])
                for h in x.get("headers", []):
                    for real, stand_in in more:
                        h[1] = h[1].replace(real, stand_in)
                    h[1] = aws_sanitize.sanitize(h[1], of)
                to.write(json.dumps(x, separators=(",", ":"), sort_keys=True) + "\n")

if __name__ == "__main__":
    main()

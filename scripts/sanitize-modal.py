#!/usr/bin/env python3
"""Makes test fixtures of recordings of what Modal's API answered (a debug build with SUPERCI_RECORD=FILE writes one).

    scripts/sanitize-modal.py crates/cli/fixtures/modal RECORDING:NAME [...] -r REAL=STAND-IN [...]

Modal answers in protobuf (kept in base64): each REAL is replaced inside it by a STAND-IN of the same length, so every
length the protobuf says stays true (the workspace's name, control planes' ids). Modal's session token is blanked, and
what other clouds answered in the same recording is left out. Printable text left in the answers is listed at the end:
look through it before keeping the result.
"""
import json, re, sys, base64

def main():
    args = sys.argv[1:]
    more = []
    while "-r" in args:
        i = args.index("-r")
        real, stand_in = args[i + 1].split("=", 1)
        if len(real) != len(stand_in):
            sys.exit(f"{real} and {stand_in} are not of one length")
        more.append((real.encode(), stand_in.encode()))
        del args[i:i + 2]
    out, recordings = args[0], args[1:]
    seen = set()
    for recording in recordings:
        path, name = recording.rsplit(":", 1)
        with open(f"{out}/{name}.jsonl", "w") as to:
            for line in open(path):
                x = json.loads(line)
                if "api.modal.com" not in x["url"]:
                    continue
                answer = json.loads(x["answer"])
                if x["url"].endswith("/AuthTokenGet") and "answers" in answer:
                    # (An AuthTokenGetResponse whose token says it was removed.)
                    answer["answers"] = [base64.b64encode(b"\x0a\x09(removed)").decode()]
                answers = []
                for a in answer.get("answers", []):
                    b = base64.b64decode(a)
                    for real, stand_in in more:
                        b = b.replace(real, stand_in)
                    seen.update(s.decode("latin1") for s in re.findall(rb"[ -~]{6,}", b))
                    answers.append(base64.b64encode(b).decode())
                if "answers" in answer:
                    answer["answers"] = answers
                if "error" in answer:
                    for real, stand_in in more:
                        answer["error"] = answer["error"].replace(real.decode(), stand_in.decode())
                x["answer"] = json.dumps(answer, separators=(",", ":"))
                to.write(json.dumps(x, separators=(",", ":"), sort_keys=True) + "\n")
    for s in sorted(seen):
        print(s)

if __name__ == "__main__":
    main()

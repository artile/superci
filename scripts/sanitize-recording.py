#!/usr/bin/env python3
"""Makes test fixtures of recordings of what AWS answered (a debug build with SUPERCI_RECORD=FILE writes one).

    scripts/sanitize-recording.py crates/cli/fixtures/aws/SITUATION RECORDING:NAME [RECORDING:NAME ...] [-r REAL=STAND-IN]

Each RECORDING is written to SITUATION/NAME.jsonl. Everything that names the real account is replaced, the same thing
by the same stand-in in every file of the situation: account numbers, organization and policy ids, request and session
ids, resource ids, email addresses, and the sealed message AWS adds to a refusal. What is public and what the code asks for by value stays (the runner image's owner,
public image ids, quota codes). Anything else that is the account's own (a control plane's id in a tag) is given with
-r REAL=STAND-IN. Look through the result before keeping it.
"""
import json, re, sys

KEEP_ACCOUNTS = {"135269210855"}  # RunsOn's, whose public image jobs run on

def stand_ins():
    seen = {}
    def of(kind, real, make):
        key = (kind, real)
        if key not in seen:
            seen[key] = make(sum(1 for k in seen if k[0] == kind) + 1)
        return seen[key]
    return of

def sanitize(text, of):
    text = re.sub(r"(?<![0-9.])\d{12}(?![0-9])", lambda m: m.group(0) if m.group(0) in KEEP_ACCOUNTS else of("account", m.group(0), lambda n: str(n) * 12), text)
    text = re.sub(r"\bo-[a-z0-9]{10,32}\b", lambda m: of("org", m.group(0), lambda n: f"o-example{n:03d}"), text)
    text = re.sub(r"\bp-[a-z0-9]{8}\b", lambda m: of("policy", m.group(0), lambda n: f"p-example{n}"), text)
    text = re.sub(r"\b[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\b", lambda m: of("uuid", m.group(0), lambda n: f"00000000-0000-4000-8000-{n:012d}"), text)
    text = re.sub(r"\b((?:vpc-cidr-assoc|vpc|subnet|sg|rtb|igw|acl|dopt|eni|eipalloc|i|vol|snap|lt)-)([0-9a-f]{8,17})\b", lambda m: m.group(1) + of(m.group(1), m.group(2), lambda n: f"{n:017x}"), text)
    text = re.sub(r"[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}", "someone@example.com", text)
    # (AWS's sealed account of a refusal: only the account itself can open it, and it is the account's.)
    text = re.sub(r"(Encoded authorization failure message: )[A-Za-z0-9_+/=-]+", r"\1(removed)", text)
    return text

def main():
    args = sys.argv[1:]
    more = []
    while "-r" in args:
        i = args.index("-r")
        more.append(args[i + 1].split("=", 1))
        del args[i:i + 2]
    out, recordings = args[0], args[1:]
    of = stand_ins()
    for recording in recordings:
        path, name = recording.rsplit(":", 1)
        with open(f"{out}/{name}.jsonl", "w") as to:
            for line in open(path):
                x = json.loads(line)
                for k in ("url", "target", "asked", "answer"):
                    x[k] = sanitize(x[k], of)
                    for real, stand_in in more:
                        x[k] = x[k].replace(real, stand_in)
                to.write(json.dumps(x, separators=(",", ":"), sort_keys=True) + "\n")

if __name__ == "__main__":
    main()

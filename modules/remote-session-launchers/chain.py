"""Run each generated desktop entry against the fakes and compare argv logs.

Each case lists the records expected in $FAKE_LOG, in order. A matcher is
{"exact": argv} or any of {"prefix": argv, "suffix": argv, "absent": word}.
"""

import json
import os
import subprocess
import sys
import tempfile

spec = json.load(open(sys.argv[1]))
failures = 0


def matches(m, record):
    if "exact" in m:
        return record == m["exact"]
    prefix, suffix = m.get("prefix", []), m.get("suffix", [])
    return (
        record[: len(prefix)] == prefix
        and (not suffix or record[-len(suffix) :] == suffix)
        and m.get("absent") not in record
    )


for case in spec["cases"]:
    with open(case["desktop"]) as f:
        execs = [l[5:].rstrip("\n") for l in f if l.startswith("Exec=")]
    for shell in case.get("shells", ["sh"]):
        name = f"{case['name']} ({shell})"
        with tempfile.TemporaryDirectory() as tmp:
            log = os.path.join(tmp, "log")
            open(log, "w").close()
            env = {
                "PATH": spec["path"],
                "HOME": "/local-home",
                "FAKE_LOG": log,
                "FAKE_REMOTE_SHELL": spec["shells"][shell],
                "FAKE_REMOTE_PATH": spec["remotePath"],
            }
            env.update(case.get("env", {}))
            result = subprocess.run(execs, env=env, cwd=tmp, capture_output=True, text=True)
            records = [json.loads(l) for l in open(log)]
        expect = case["expect"]
        ok = (
            len(execs) == 1
            and (case.get("ignoreStatus") or result.returncode == 0)
            and len(records) == len(expect)
            and all(matches(m, r) for m, r in zip(expect, records))
        )
        print(("ok   " if ok else "FAIL ") + name)
        if not ok:
            failures += 1
            print(f"  exec={execs} status={result.returncode}\n  stderr={result.stderr!r}")
            print("  expected:\n" + "".join(f"    {json.dumps(m)}\n" for m in expect))
            print("  got:\n" + "".join(f"    {json.dumps(r)}\n" for r in records))

sys.exit(1 if failures else 0)

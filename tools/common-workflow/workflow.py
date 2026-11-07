"""Common consumer workflow commands; no forge writes or automatic publication."""
import argparse
import json
import os
from pathlib import Path
import re
import shutil
import signal
import subprocess
import sys
import tempfile
import time
from urllib.parse import quote

CANONICAL = "github:binarin/swan-lake-common/swan-lake-current"
TEMP = "git+https://forgejo.lynx-lizard.ts.net/binarin/swan-lake-common?ref="
ORIGINAL = dict(type="github", owner="binarin", repo="swan-lake-common", ref="swan-lake-current")


def run(args, **kwargs):
    return subprocess.check_output(args, text=True, **kwargs).strip()


def declaration(root):
    # Evaluate only the input declaration, without fetching any flake inputs.
    expr = f'(import {json.dumps(str(root / "flake.nix"))}).inputs.common'
    value = json.loads(run(["nix", "eval", "--impure", "--json", "--expr", expr]))
    if not isinstance(value, dict):
        raise ValueError("common input declaration must be an attribute set")
    return value


def common_node(lock):
    key = lock["nodes"][lock["root"]]["inputs"]["common"]
    if not isinstance(key, str):
        raise ValueError("common must be a direct input, not follows")
    return key, lock["nodes"][key]


def check(root):
    decl = declaration(root)
    url = decl.get("url", "")
    if url.startswith(TEMP):
        raise ValueError("temporary pin present; merge/promote common, then run finalize-common")
    if decl != {"url": CANONICAL}:
        raise ValueError(f"common declaration must be exactly {CANONICAL}")
    _, node = common_node(json.loads((root / "flake.lock").read_text()))
    if node.get("flake", True) is not True:
        raise ValueError("common must be a flake")
    if node.get("original") != ORIGINAL:
        raise ValueError("common lock original must be canonical GitHub owner/repo/ref/type")
    locked = node.get("locked", {})
    expected = {k: v for k, v in ORIGINAL.items() if k != "ref"}
    if any(locked.get(k) != v for k, v in expected.items()):
        raise ValueError("common locked source must be canonical GitHub owner/repo/type")
    if set(locked) - {"type", "owner", "repo", "rev", "narHash", "lastModified", "ref"}:
        raise ValueError("unexpected common locked source options")
    if "ref" in locked and locked["ref"] != ORIGINAL["ref"]:
        raise ValueError("noncanonical locked ref")
    if not re.fullmatch(r"[0-9a-f]{40}", locked.get("rev", "")):
        raise ValueError("common requires a full locked revision")
    if not re.fullmatch(r"sha256-[A-Za-z0-9+/]{43}=", locked.get("narHash", "")):
        raise ValueError("common requires a SHA256 NAR hash")


def override_argv(command, url):
    flags = ["--override-input", "common", url, "--no-write-lock-file"]
    if command[:2] == ["nix", "build"]:
        return command[:2] + flags + command[2:]
    if command[:3] == ["nix", "flake", "check"]:
        return command[:3] + flags + command[3:]
    if command[:1] == ["nixos-rebuild"]:
        if "--flake" not in command or "--" in command:
            raise ValueError("nixos-rebuild requires explicit --flake and no -- separator")
        return command[:1] + flags + command[1:]
    if command[:1] == ["deploy"]:
        # deploy-rs forwards the tail after -- to nix build.
        if "--" in command:
            i = command.index("--")
            return command[:i + 1] + flags + command[i + 1:]
        return command + ["--"] + flags
    raise ValueError("supported: nix build, nix flake check, nixos-rebuild --flake, deploy")


def with_common(args):
    root = Path(run(["git", "rev-parse", "--show-toplevel"]))
    common = Path(args.common).resolve() if args.common else root.parent / "swan-lake-common"
    if not (common / "flake.nix").is_file():
        raise ValueError(f"missing common checkout {common}; use --common /path/to/swan-lake-common")
    url = ("path:" if args.dirty else "git+file:") + quote(str(common), safe="/")
    cmd = args.command
    if cmd[:1] == ["--"]:
        cmd = cmd[1:]
    os.execvp(cmd[0] if cmd else "", override_argv(cmd, url))


def resolve(lock, edge, seen=()):
    if isinstance(edge, str):
        return edge
    if not isinstance(edge, list) or tuple(edge) in seen:
        raise ValueError("malformed/cyclic follows")
    key = lock["root"]
    for name in edge:
        key = resolve(lock, lock["nodes"][key]["inputs"][name], seen + (tuple(edge),))
    return key


def graph(lock, key, active=()):
    if key in active:
        raise ValueError("cyclic dependency graph")
    node = lock["nodes"][key]
    return {"locked": node.get("locked"), "flake": node.get("flake", True),
            "inputs": {name: graph(lock, resolve(lock, edge), active + (key,))
                       for name, edge in node.get("inputs", {}).items()}}


def validate_conversion(old, new):
    oldkey, oldnode = common_node(old)
    newkey, newnode = common_node(new)
    before, after = oldnode["locked"], newnode["locked"]
    same = before["rev"] == after["rev"]
    if same and before["narHash"] != after["narHash"]:
        raise ValueError("same revision has mismatched NAR hash")
    # All nodes outside the old common dependency closure must survive unchanged.
    reachable = set()

    def visit(key):
        if key not in reachable:
            reachable.add(key)
            for edge in old["nodes"][key].get("inputs", {}).values():
                visit(resolve(old, edge))

    visit(oldkey)
    for key, node in old["nodes"].items():
        if key not in reachable and node != new["nodes"].get(key):
            raise ValueError(f"unexpected unrelated lock churn: {key}")
    allowed = set(old["nodes"]) | {newkey}
    if same and set(new["nodes"]) - allowed:
        raise ValueError("unexpected new lock nodes")
    if same:
        a, b = graph(old, oldkey), graph(new, newkey)
        a["locked"] = b["locked"] = None
        if a != b:
            raise ValueError("same revision changed resolved dependency graph")
    return same


def promoted(timeout, interval, expected):
    if timeout < 0 or interval < 0:
        raise ValueError("timeout and interval must be nonnegative")
    deadline = time.monotonic() + timeout
    while True:
        info = json.loads(run(["nix", "flake", "prefetch", "--json", "--refresh", CANONICAL],
                              timeout=max(1, deadline - time.monotonic())))
        rev = info["locked"]["rev"]
        if expected is None or rev == expected:
            return rev
        if time.monotonic() >= deadline:
            raise ValueError("timed out waiting for GitHub promotion; originals unchanged")
        time.sleep(min(interval, max(0, deadline - time.monotonic())))


def finalize(args):
    root = Path(args.root).resolve()
    if root != Path(run(["git", "-C", str(root), "rev-parse", "--show-toplevel"])):
        raise ValueError("--root must name the consumer Git root")
    paths = [root / name for name in ("flake.nix", "flake.lock")]
    if run(["git", "-C", str(root), "status", "--porcelain", "--", "flake.nix", "flake.lock"]):
        raise ValueError("commit consumer input files first; unrelated dirty files are allowed")
    snapshot = [p.read_bytes() for p in paths]
    decl = declaration(root)
    url = decl.get("url", "")
    if decl != {"url": url} or not url.startswith(TEMP) or not url[len(TEMP):]:
        raise ValueError("expected an explicit temporary Forgejo common branch URL")
    old = json.loads(snapshot[1])
    _, oldnode = common_node(old)
    original = oldnode["original"]
    if original != dict(type="git", url=TEMP[4:].split("?", 1)[0], ref=url[len(TEMP):]):
        raise ValueError("temporary declaration/lock mismatch")
    tested = oldnode["locked"]["rev"]
    # Default waits for the tested revision. --allow-newer explicitly accepts another promotion.
    rev = promoted(args.timeout, args.interval, None if args.allow_newer else tested)
    text = snapshot[0].decode()
    literal = json.dumps(url)
    if text.count(literal) != 1:
        raise ValueError("temporary URL must appear once as a literal in flake.nix")
    with tempfile.TemporaryDirectory(prefix="finalize-common-") as tmp:
        stage = Path(tmp) / "consumer"
        shutil.copytree(root, stage, symlinks=True, ignore=shutil.ignore_patterns(".git", ".direnv", "result", "result-*"))
        (stage / "flake.nix").write_text(text.replace(literal, json.dumps(CANONICAL)))
        run(["nix", "flake", "lock", str(stage), "--override-input", "common",
             f"github:binarin/swan-lake-common/{rev}"])
        new = json.loads((stage / "flake.lock").read_text())
        _, node = common_node(new)
        if node["locked"]["rev"] != rev:
            raise ValueError("locking did not preserve the selected promoted revision")
        node["original"] = ORIGINAL.copy()
        same = validate_conversion(old, new)
        (stage / "flake.lock").write_text(json.dumps(new, indent=2) + "\n")
        check(stage)
        replacements = [(stage / p.name).read_bytes() for p in paths]
    install(paths, snapshot, replacements)
    print("source-equivalent finalization" if same else "changed revision; new builds required")
    print("Run required canonical-pin and build CI again; no commit/push performed.")


def install(paths, snapshot, replacements):
    # Prepare both files before touching originals. Catch interruption and roll back
    # a partial replacement; no claim of crash-atomic multi-file transactions.
    prepared = []
    replaced = []
    def interrupt(*_):
        raise KeyboardInterrupt()

    previous = signal.signal(signal.SIGTERM, interrupt)
    try:
        for path, content in zip(paths, replacements):
            fd, name = tempfile.mkstemp(prefix=f".{path.name}.", dir=path.parent)
            prepared.append(Path(name))
            with os.fdopen(fd, "wb") as f:
                f.write(content)
            os.chmod(name, path.stat().st_mode)
        if [p.read_bytes() for p in paths] != snapshot:
            raise ValueError("originals changed during finalization; refusing replacement")
        for path, temp, before in zip(paths, prepared, snapshot):
            os.replace(temp, path)
            replaced.append((path, before))
    except BaseException:
        for path, before in replaced:
            path.write_bytes(before)
        raise
    finally:
        for temp in prepared:
            temp.unlink(missing_ok=True)
        signal.signal(signal.SIGTERM, previous)


def main():
    name = Path(sys.argv[0]).name
    parser = argparse.ArgumentParser(prog=name, description=__doc__)
    if name == "with-common":
        parser.add_argument("--common")
        parser.add_argument("--dirty", action="store_true")
        parser.add_argument("command", nargs=argparse.REMAINDER)
        action = with_common
    elif name == "check-common-pin":
        parser.add_argument("--root", default=".")
        action = lambda args: check(Path(args.root).resolve())
    elif name == "finalize-common":
        parser.add_argument("--root", required=True)
        parser.add_argument("--timeout", type=float, default=300)
        parser.add_argument("--interval", type=float, default=5)
        parser.add_argument("--allow-newer", action="store_true")
        action = finalize
    else:
        parser.error("invoke as with-common, check-common-pin or finalize-common")
    try:
        action(parser.parse_args())
    except (ValueError, KeyError, TypeError, OSError, subprocess.SubprocessError) as exc:
        parser.exit(1, f"{name}: {exc}\n")


if __name__ == "__main__":
    main()

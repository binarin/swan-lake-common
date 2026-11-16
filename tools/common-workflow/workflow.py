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
# Inputs the workflow manages: the canonical pin and the optional experiment pin.
INPUTS = ("common", "common-next")
# Git URLs of the swan-lake-common repository (Forgejo origin and GitHub mirror).
NEXT_GIT_URLS = {base + suffix
                 for base in ("https://forgejo.lynx-lizard.ts.net/binarin/swan-lake-common",
                              "https://github.com/binarin/swan-lake-common")
                 for suffix in ("", ".git")}
REV = r"[0-9a-f]{40}"
NAR_HASH = r"sha256-[A-Za-z0-9+/]{43}="


def run(args, **kwargs):
    return subprocess.check_output(args, text=True, **kwargs).strip()


def declaration(root, name="common"):
    # Evaluate only the input declaration, without fetching any flake inputs.
    # A missing input evaluates to None.
    expr = f'(import {json.dumps(str(root / "flake.nix"))}).inputs.{json.dumps(name)} or null'
    value = json.loads(run(["nix", "eval", "--impure", "--json", "--expr", expr]))
    if value is not None and not isinstance(value, dict):
        raise ValueError(f"{name} input declaration must be an attribute set")
    return value


def parse_ref(url):
    # Nix's own parser yields the attribute form that `nix flake lock` records
    # as the node's "original".
    expr = f"builtins.parseFlakeRef {json.dumps(url)}"
    return json.loads(run(["nix", "eval", "--json", "--expr", expr]))


def common_node(lock, name="common"):
    key = lock["nodes"][lock["root"]]["inputs"][name]
    if not isinstance(key, str):
        raise ValueError(f"{name} must be a direct input, not follows")
    return key, lock["nodes"][key]


def check_next_original(original):
    """Accept any ref or rev of the swan-lake-common repository, nothing else."""
    if not isinstance(original, dict):
        raise ValueError("common-next original must be an attribute set")
    kind = original.get("type")
    if kind == "github":
        fixed = {"owner": "binarin", "repo": "swan-lake-common"}
        allowed = {"type", "owner", "repo", "ref", "rev"}
    elif kind == "git":
        fixed = {}
        allowed = {"type", "url", "ref", "rev"}
        if original.get("url") not in NEXT_GIT_URLS:
            raise ValueError("common-next must point at the swan-lake-common repository")
    else:
        raise ValueError("common-next must be a github: or git+https: reference to swan-lake-common")
    if any(original.get(k) != v for k, v in fixed.items()):
        raise ValueError("common-next must point at the swan-lake-common repository")
    if set(original) - allowed:
        raise ValueError(f"unexpected common-next source options: {sorted(set(original) - allowed)}")
    if "ref" in original and not (isinstance(original["ref"], str) and original["ref"]):
        raise ValueError("common-next ref must be a nonempty string")
    if "rev" in original and not re.fullmatch(REV, str(original["rev"])):
        raise ValueError("common-next rev must be a full revision")


def check_next_locked(original, locked):
    kind = original["type"]
    if kind == "github":
        same = ("type", "owner", "repo")
        allowed = {"type", "owner", "repo", "rev", "narHash", "lastModified", "ref"}
    else:
        same = ("type", "url")
        allowed = {"type", "url", "ref", "rev", "narHash", "lastModified", "revCount"}
    if any(locked.get(k) != original[k] for k in same):
        raise ValueError("common-next locked source must match its original repository")
    if set(locked) - allowed:
        raise ValueError("unexpected common-next locked source options")
    for k in ("ref", "rev"):
        if k in original and k in locked and locked[k] != original[k]:
            raise ValueError(f"common-next locked {k} differs from its original")
    if not re.fullmatch(REV, locked.get("rev", "")):
        raise ValueError("common-next requires a full locked revision")
    if not re.fullmatch(NAR_HASH, locked.get("narHash", "")):
        raise ValueError("common-next requires a SHA256 NAR hash")


def check(root, ancestry=True):
    decl = declaration(root)
    if decl is None:
        raise ValueError("common input declaration must be an attribute set")
    url = decl.get("url", "")
    if url.startswith(TEMP):
        raise ValueError("temporary pin present; merge/promote common, then run finalize-common")
    if decl != {"url": CANONICAL}:
        raise ValueError(f"common declaration must be exactly {CANONICAL}")
    lock = json.loads((root / "flake.lock").read_text())
    _, node = common_node(lock)
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
    if not re.fullmatch(REV, locked.get("rev", "")):
        raise ValueError("common requires a full locked revision")
    if not re.fullmatch(NAR_HASH, locked.get("narHash", "")):
        raise ValueError("common requires a SHA256 NAR hash")
    next_locked = check_next(root, lock)
    if next_locked is not None and ancestry:
        print(f"check-common-pin: {next_ancestry(next_locked, locked['rev'])}", file=sys.stderr)


def check_next(root, lock):
    """Validate the optional common-next input; return its locked source or None."""
    decl = declaration(root, "common-next")
    in_lock = "common-next" in lock["nodes"][lock["root"]].get("inputs", {})
    if decl is None and not in_lock:
        return None
    if decl is None or not in_lock:
        raise ValueError("common-next declaration/lock mismatch; run nix flake lock")
    url = decl.get("url")
    if decl != {"url": url} or not isinstance(url, str):
        raise ValueError("common-next declaration must be exactly a url (no follows or options)")
    original = parse_ref(url)
    check_next_original(original)
    _, node = common_node(lock, "common-next")
    if node.get("flake", True) is not True:
        raise ValueError("common-next must be a flake")
    if node.get("original") != original:
        raise ValueError("common-next declaration/lock mismatch; run nix flake lock")
    check_next_locked(original, node.get("locked", {}))
    return node["locked"]


def next_ancestry(locked, common_rev):
    """Describe whether common-next's locked revision descends from common's.

    Fetches only commit history (no trees or blobs) of both revisions from
    common-next's repository into a throwaway bare repository.
    """
    rev = locked["rev"]
    subject = f"common-next {rev[:12]}"
    if rev == common_rev:
        return f"{subject} equals the locked common revision"
    if locked["type"] == "github":
        url = f"https://github.com/{locked['owner']}/{locked['repo']}.git"
    else:
        url = locked["url"]
    env = dict(os.environ, GIT_TERMINAL_PROMPT="0", GIT_NO_LAZY_FETCH="1")
    try:
        with tempfile.TemporaryDirectory(prefix="check-common-pin-") as tmp:
            run(["git", "init", "-q", "--bare", tmp], env=env)
            run(["git", "-C", tmp, "fetch", "-q", "--no-tags", "--filter=tree:0", url, rev, common_rev],
                env=env, timeout=60, stderr=subprocess.PIPE)
            result = subprocess.run(["git", "-C", tmp, "merge-base", "--is-ancestor", common_rev, rev],
                                    env=env, timeout=60, capture_output=True)
    except subprocess.CalledProcessError as exc:
        lines = (exc.stderr or "").strip().splitlines()
        reason = lines[-1] if lines else f"git exited with {exc.returncode}"
    except subprocess.TimeoutExpired:
        reason = "git fetch timed out"
    except OSError as exc:
        reason = str(exc)
    else:
        if result.returncode == 0:
            return f"{subject} is based on common {common_rev[:12]}"
        if result.returncode == 1:
            return f"{subject} is NOT based on common {common_rev[:12]} (not a descendant)"
        lines = result.stderr.decode(errors="replace").strip().splitlines()
        reason = lines[-1] if lines else "git merge-base failed"
    return f"cannot determine whether {subject} is based on common {common_rev[:12]}: {reason}"


def override_argv(command, url, name="common"):
    flags = ["--override-input", name, url, "--no-write-lock-file"]
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
    os.execvp(cmd[0] if cmd else "", override_argv(cmd, url, args.input))


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
    return {"locked": node.get("locked"), "original": node.get("original"),
            "flake": node.get("flake", True),
            "inputs": {name: graph(lock, resolve(lock, edge), active + (key,))
                       for name, edge in node.get("inputs", {}).items()}}


def validate_conversion(old, new, name="common"):
    oldkey, oldnode = common_node(old, name)
    newkey, newnode = common_node(new, name)
    before, after = oldnode["locked"], newnode["locked"]
    same = before["rev"] == after["rev"]
    if same and before["narHash"] != after["narHash"]:
        raise ValueError("same revision has mismatched NAR hash")
    # All nodes outside the old dependency closure of the input must survive unchanged.
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
        a["original"] = b["original"] = None
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
    name = args.input
    root = Path(args.root).resolve()
    if root != Path(run(["git", "-C", str(root), "rev-parse", "--show-toplevel"])):
        raise ValueError("--root must name the consumer Git root")
    paths = [root / f for f in ("flake.nix", "flake.lock")]
    if run(["git", "-C", str(root), "status", "--porcelain", "--", "flake.nix", "flake.lock"]):
        raise ValueError("commit consumer input files first; unrelated dirty files are allowed")
    snapshot = [p.read_bytes() for p in paths]
    decl = declaration(root, name)
    if decl is None:
        raise ValueError(f"no {name} input declared in flake.nix")
    url = decl.get("url", "")
    old = json.loads(snapshot[1])
    _, oldnode = common_node(old, name)
    original = oldnode["original"]
    locked = oldnode["locked"]
    if name == "common":
        if decl != {"url": url} or not url.startswith(TEMP) or not url[len(TEMP):]:
            raise ValueError("expected an explicit temporary Forgejo common branch URL")
        if original != dict(type="git", url=TEMP[4:].split("?", 1)[0], ref=url[len(TEMP):]):
            raise ValueError("temporary declaration/lock mismatch")
        if (locked.get("type") != "git" or locked.get("url") != original["url"]
                or locked.get("ref") != original["ref"]
                or not re.fullmatch(REV, locked.get("rev", ""))
                or not re.fullmatch(NAR_HASH, locked.get("narHash", ""))):
            raise ValueError("malformed temporary locked source")
    else:
        if decl != {"url": url} or not isinstance(url, str):
            raise ValueError(f"{name} declaration must be exactly a url (no follows or options)")
        if url == CANONICAL:
            raise ValueError(f"{name} already declares {CANONICAL}; nothing to finalize")
        check_next_original(original)
        if parse_ref(url) != original:
            raise ValueError(f"{name} declaration/lock mismatch")
        check_next_locked(original, locked)
    tested = locked["rev"]
    # Default waits for the tested revision. --allow-newer explicitly accepts another promotion.
    rev = promoted(args.timeout, args.interval, None if args.allow_newer else tested)
    text = snapshot[0].decode()
    literal = json.dumps(url)
    if text.count(literal) != 1:
        raise ValueError(f"{name} URL must appear once as a literal in flake.nix")
    with tempfile.TemporaryDirectory(prefix="finalize-common-") as tmp:
        stage = Path(tmp) / "consumer"
        shutil.copytree(root, stage, symlinks=True, ignore=shutil.ignore_patterns(".git", ".direnv", "result", "result-*"))
        (stage / "flake.nix").write_text(text.replace(literal, json.dumps(CANONICAL)))
        run(["nix", "flake", "lock", str(stage), "--override-input", name,
             f"github:binarin/swan-lake-common/{rev}"])
        new = json.loads((stage / "flake.lock").read_text())
        _, node = common_node(new, name)
        if node["locked"]["rev"] != rev:
            raise ValueError("locking did not preserve the selected promoted revision")
        node["original"] = ORIGINAL.copy()
        same = validate_conversion(old, new, name)
        (stage / "flake.lock").write_text(json.dumps(new, indent=2) + "\n")
        check(stage, ancestry=False)
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
            replaced.append((path, before))
            os.replace(temp, path)
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
        parser.add_argument("--input", choices=INPUTS, default="common")
        parser.add_argument("--dirty", action="store_true")
        parser.add_argument("command", nargs=argparse.REMAINDER)
        action = with_common
    elif name == "check-common-pin":
        parser.add_argument("--root", default=".")
        action = lambda args: check(Path(args.root).resolve())
    elif name == "finalize-common":
        parser.add_argument("--root", required=True)
        parser.add_argument("--input", choices=INPUTS, default="common")
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

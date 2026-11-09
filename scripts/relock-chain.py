#!/usr/bin/env python3
"""Relock the swan-lake chain and push each downstream pin.

Chain: zwanenzang -> swan-lake-common -> swan-lake / nixos-config.

For every repository the script:

1. fetches origin/master, fast-forwards the local master and refuses to
   continue if the checkout is dirty, on another branch, or ahead of remote;
2. relocks the upstream pin with ``nix flake update <input>`` (zwanenzang is
   the chain source, so it is only refreshed);
3. verifies the result with ``nix flake check --no-build`` (``--no-check``
   skips this);
4. commits flake.lock, pushes master, and fast-forwards/freezes the
   promotion branch (common's ``swan-lake-current``);
5. waits for the GitHub push mirror to serve the promoted revision, then
   continues with the downstream repository.

The script never force-pushes, never edits flake.nix and never touches
untracked files. Downstream consumers must declare the canonical
``github:binarin/swan-lake-common/swan-lake-current`` input; a temporary
paired pin aborts the run instead of being pushed further.

Run it from anywhere inside a checkout, or point it at a workspace with
``--base``. A clean re-run is a no-op: each step is guarded by the current
lock and remote refs.
"""

from __future__ import annotations

import argparse
from dataclasses import dataclass
import json
from pathlib import Path
import subprocess
import sys
import time

MASTER = "master"

# The canonical declaration of the `common` input in downstream flakes.
CANONICAL_COMMON = {
    "type": "github",
    "owner": "binarin",
    "repo": "swan-lake-common",
    "ref": "swan-lake-current",
}


@dataclass(frozen=True)
class Repo:
    name: str
    directory: str
    input: str | None = None
    message: str = ""
    promote: str | None = None
    mirror: str | None = None
    canonical: dict | None = None


CHAIN = (
    # The chain source: nothing to relock, but its master must be current
    # before common pins it.
    Repo(name="zwanenzang", directory="zwanenzang"),
    # common pins zwanenzang's default branch (Forgejo master) and promotes
    # swan-lake-current, which downstream consumers pin through GitHub.
    Repo(
        name="common",
        directory="swan-lake-common",
        input="zwanenzang",
        message="zwanenzang: bump pin",
        promote="swan-lake-current",
        mirror="https://github.com/binarin/swan-lake-common",
        canonical={
            "type": "git",
            "url": "https://forgejo.lynx-lizard.ts.net/binarin/zwanenzang",
        },
    ),
    Repo(
        name="swan-lake",
        directory="swan-lake",
        input="common",
        message="bump common",
        canonical=CANONICAL_COMMON,
    ),
    Repo(
        name="nixos-config",
        directory="nixos-config",
        input="common",
        message="bump common",
        canonical=CANONICAL_COMMON,
    ),
)


@dataclass
class Row:
    name: str
    before: str
    after: str
    result: str


class Failure(Exception):
    """An expected operational failure; the message is shown to the user."""


def short(rev: str | None) -> str:
    return rev[:12] if rev else "?"


def say(name: str, message: str) -> None:
    print(f"[{name}] {message}", flush=True)


def capture(argv: list[str], cwd: Path | None = None) -> str:
    result = subprocess.run(argv, cwd=cwd, text=True, capture_output=True)
    if result.returncode != 0:
        detail = (result.stderr or result.stdout).strip()
        raise Failure(f"command failed: {' '.join(argv)}\n{detail}")
    return result.stdout.strip()


def stream(argv: list[str], cwd: Path | None = None) -> None:
    print(f"$ {' '.join(argv)}", flush=True)
    result = subprocess.run(argv, cwd=cwd)
    if result.returncode != 0:
        raise Failure(f"command failed ({result.returncode}): {' '.join(argv)}")


def git(path: Path, *args: str) -> str:
    return capture(["git", "-C", str(path), *args])


def head(path: Path) -> str:
    return git(path, "rev-parse", "HEAD")


def ls_remote(path: Path, remote: str, branch: str) -> str | None:
    value = capture(["git", "-C", str(path), "ls-remote", remote, f"refs/heads/{branch}"])
    parts = value.split()
    return parts[0] if parts else None


def ls_remote_url(url: str, branch: str) -> str | None:
    value = capture(["git", "ls-remote", "--heads", url, f"refs/heads/{branch}"])
    parts = value.split()
    return parts[0] if parts else None


def is_ancestor(path: Path, older: str, newer: str) -> bool:
    result = subprocess.run(
        ["git", "-C", str(path), "merge-base", "--is-ancestor", older, newer],
        capture_output=True,
        text=True,
    )
    return result.returncode == 0


def check_layout(spec: Repo, path: Path) -> None:
    if not (path / "flake.nix").is_file():
        raise Failure(f"{spec.name}: {path} is not a flake repository")
    if not (path / ".git").exists():
        raise Failure(f"{spec.name}: {path} is not a git checkout")


def ensure_clean(spec: Repo, path: Path) -> None:
    dirty = git(path, "status", "--porcelain", "--untracked-files=no")
    if dirty:
        raise Failure(f"{spec.name}: uncommitted tracked changes:\n{dirty}")


def ensure_branch(spec: Repo, path: Path) -> None:
    result = subprocess.run(
        ["git", "-C", str(path), "symbolic-ref", "--quiet", "--short", "HEAD"],
        capture_output=True,
        text=True,
    )
    branch = result.stdout.strip() if result.returncode == 0 else None
    if branch != MASTER:
        raise Failure(f"{spec.name}: expected branch {MASTER!r}, found {branch or 'detached HEAD'}")


def refresh(spec: Repo, path: Path) -> None:
    say(spec.name, "fetching origin")
    stream(["git", "-C", str(path), "fetch", "--prune", "origin"])
    stream(["git", "-C", str(path), "merge", "--ff-only", f"origin/{MASTER}"])
    local = head(path)
    remote = git(path, "rev-parse", f"origin/{MASTER}")
    if local != remote:
        raise Failure(
            f"{spec.name}: local {MASTER} is {short(local)}, origin/{MASTER} is "
            f"{short(remote)}; push or reset the local branch first"
        )


def direct_input(lock: dict, name: str) -> dict:
    inputs = lock["nodes"][lock["root"]]["inputs"]
    if name not in inputs:
        raise Failure(f"flake.lock has no direct input {name!r}")
    key = inputs[name]
    if not isinstance(key, str):
        raise Failure(f"input {name!r} is a follows edge, not a pinned source")
    return lock["nodes"][key]


def ensure_identity(spec: Repo, path: Path) -> None:
    for key in ("user.name", "user.email"):
        result = subprocess.run(
            ["git", "-C", str(path), "config", key],
            capture_output=True,
            text=True,
        )
        if result.returncode != 0 or not result.stdout.strip():
            raise Failure(f"{spec.name}: git {key} is not configured; refusing to commit")


def verify(spec: Repo, path: Path) -> None:
    say(spec.name, "verifying with nix flake check --no-build")
    stream(["nix", "flake", "check", "--no-build", "--no-write-lock-file"], cwd=path)


def relock(spec: Repo, path: Path, upstream_rev: str, check: bool) -> bool:
    assert spec.input is not None
    lock_file = path / "flake.lock"
    original = lock_file.read_text()
    committed = False
    try:
        say(spec.name, f"relocking {spec.input} (upstream {short(upstream_rev)})")
        stream(["nix", "flake", "update", spec.input], cwd=path)

        changed = git(path, "diff", "--name-only")
        if changed and changed != "flake.lock":
            raise Failure(f"{spec.name}: nix flake update touched unexpected files: {changed}")

        node = direct_input(json.loads(lock_file.read_text()), spec.input)
        locked_rev = node.get("locked", {}).get("rev")
        if locked_rev != upstream_rev:
            raise Failure(
                f"{spec.name}: {spec.input} resolved to {short(locked_rev)}, expected "
                f"{short(upstream_rev)}; the upstream moved during the run, rerun the script"
            )
        if spec.canonical is not None:
            original_decl = node.get("original")
            if original_decl != spec.canonical:
                raise Failure(
                    f"{spec.name}: {spec.input} declaration is not canonical: {original_decl}"
                )

        if not changed:
            say(spec.name, f"{spec.input} already locked at the upstream revision")
            return False

        if check:
            verify(spec, path)
        ensure_identity(spec, path)

        stream(["git", "-C", str(path), "add", "--", "flake.lock"])
        stream(["git", "-C", str(path), "commit", "-m", spec.message, "--", "flake.lock"])
        committed = True
        return True
    finally:
        if not committed and lock_file.read_text() != original:
            subprocess.run(
                ["git", "-C", str(path), "checkout", "HEAD", "--", "flake.lock"],
                capture_output=True,
            )
            say(spec.name, "restored flake.lock after abort")


def push_master(spec: Repo, path: Path) -> None:
    say(spec.name, f"pushing {MASTER}")
    stream(["git", "-C", str(path), "push", "origin", f"HEAD:refs/heads/{MASTER}"])
    remote = ls_remote(path, "origin", MASTER)
    local = head(path)
    if remote != local:
        raise Failure(f"{spec.name}: origin/{MASTER} is {short(remote)} after push, expected {short(local)}")


def wait_for_mirror(spec: Repo, rev: str, timeout: float, interval: float) -> None:
    assert spec.promote is not None and spec.mirror is not None
    say(
        spec.name,
        f"waiting for {spec.mirror} {spec.promote} to reach {short(rev)}",
    )
    deadline = time.monotonic() + timeout
    current = None
    while True:
        current = ls_remote_url(spec.mirror, spec.promote)
        if current == rev:
            say(spec.name, "mirror is up to date")
            return
        if time.monotonic() >= deadline:
            raise Failure(
                f"{spec.name}: {spec.mirror} {spec.promote} is {short(current)}, "
                f"expected {short(rev)} after {timeout:g}s"
            )
        time.sleep(interval)


def promote(spec: Repo, path: Path, timeout: float, interval: float) -> None:
    if spec.promote is None:
        return
    local = head(path)
    stream(["git", "-C", str(path), "fetch", "--prune", "origin"])
    previous = ls_remote(path, "origin", spec.promote)
    if previous is not None and previous != local and not is_ancestor(path, previous, local):
        raise Failure(
            f"{spec.name}: {spec.promote} is {short(previous)}, which is not an ancestor "
            f"of {MASTER} {short(local)}; refusing non-fast-forward promotion"
        )
    say(spec.name, f"promoting {spec.promote} to {short(local)}")
    stream(["git", "-C", str(path), "push", "origin", f"HEAD:refs/heads/{spec.promote}"])
    if spec.mirror is None:
        promoted = ls_remote(path, "origin", spec.promote)
        if promoted != local:
            raise Failure(
                f"{spec.name}: origin/{spec.promote} is {short(promoted)} after push, "
                f"expected {short(local)}"
            )
    result = subprocess.run(
        ["git", "-C", str(path), "branch", "-f", spec.promote, local],
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        say(spec.name, f"local {spec.promote} left untouched ({result.stderr.strip()})")
    if spec.mirror is not None:
        wait_for_mirror(spec, local, timeout, interval)


def process(
    spec: Repo,
    path: Path,
    upstream_rev: str | None,
    check: bool,
    timeout: float,
    interval: float,
) -> Row:
    say(spec.name, f"repository {path}")
    check_layout(spec, path)
    ensure_clean(spec, path)
    ensure_branch(spec, path)

    before = head(path)
    refresh(spec, path)

    if spec.input is None:
        after = head(path)
        say(spec.name, f"source refreshed at {short(after)}")
        return Row(spec.name, before, after, "refreshed (source)")

    if upstream_rev is None:
        raise Failure(f"{spec.name}: no upstream revision recorded")

    relocked = relock(spec, path, upstream_rev, check)
    promoted = spec.promote is not None
    if relocked:
        push_master(spec, path)
    if promoted:
        promote(spec, path, timeout, interval)

    after = head(path)
    result = "relocked" if relocked else "already up to date"
    if relocked:
        result += ", pushed"
    if promoted:
        result += ", promoted"
    return Row(spec.name, before, after, result)


def resolve_chain(base: Path, overrides: dict[str, Path]) -> list[tuple[Repo, Path]]:
    known = {spec.name for spec in CHAIN}
    unknown = set(overrides) - known
    if unknown:
        raise Failure(f"unknown --repo names: {', '.join(sorted(unknown))}")

    script_repo = Path(__file__).resolve().parents[1]
    chain = []
    for spec in CHAIN:
        if spec.name in overrides:
            path = overrides[spec.name]
        elif spec.name == "common":
            path = script_repo
        else:
            path = base / spec.directory
        chain.append((spec, path))
    return chain


def print_summary(rows: list[Row]) -> None:
    print()
    print(f"{'repo':<14} {'before':<14} {'after':<14} result")
    for row in rows:
        print(f"{row.name:<14} {short(row.before):<14} {short(row.after):<14} {row.result}")


def dry_run(chain: list[tuple[Repo, Path]], check: bool) -> int:
    for spec, path in chain:
        check_layout(spec, path)
        ensure_clean(spec, path)
        ensure_branch(spec, path)
        steps = [f"fetch origin/{MASTER}"]
        if spec.input is not None:
            steps.append(f"nix flake update {spec.input}")
            if check:
                steps.append("nix flake check --no-build")
            steps.append(f'commit flake.lock as "{spec.message}"')
            steps.append(f"push {MASTER}")
        if spec.promote is not None:
            steps.append(f"push HEAD -> {spec.promote}")
            if spec.mirror is not None:
                steps.append(f"wait for {spec.mirror}")
        say(spec.name, f"dry-run at {short(head(path))}: " + "; ".join(steps))
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        prog="relock-chain",
        description=__doc__,
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    parser.add_argument(
        "--base",
        help="workspace containing zwanenzang, swan-lake, nixos-config "
        "(default: parent of this checkout)",
    )
    parser.add_argument(
        "--repo",
        action="append",
        default=[],
        metavar="NAME=PATH",
        help="override a checkout path (repeatable: zwanenzang, common, swan-lake, nixos-config)",
    )
    parser.add_argument(
        "--no-check",
        dest="check",
        action="store_false",
        help="skip nix flake check --no-build",
    )
    parser.add_argument(
        "--timeout",
        type=float,
        default=300.0,
        help="seconds to wait for the GitHub push mirror; 0 checks once (default: 300)",
    )
    parser.add_argument(
        "--interval",
        type=float,
        default=5.0,
        help="mirror poll interval in seconds (default: 5)",
    )
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="print the plan without fetching or changing anything",
    )
    parser.set_defaults(check=True)
    opts = parser.parse_args(argv)

    base = Path(opts.base).expanduser().resolve() if opts.base else Path(__file__).resolve().parents[1].parent
    overrides = {}
    for value in opts.repo:
        name, separator, path = value.partition("=")
        if not separator or not name or not path:
            print(f"error: --repo expects NAME=PATH, got {value!r}", file=sys.stderr)
            return 2
        overrides[name] = Path(path).expanduser().resolve()

    try:
        chain = resolve_chain(base, overrides)
        if opts.dry_run:
            return dry_run(chain, opts.check)

        rows = []
        revisions: dict[str, str] = {}
        for spec, path in chain:
            upstream_rev = revisions.get(spec.input) if spec.input is not None else None
            row = process(spec, path, upstream_rev, opts.check, opts.timeout, opts.interval)
            rows.append(row)
            revisions[spec.name] = row.after
        print_summary(rows)
        return 0
    except Failure as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    except KeyboardInterrupt:
        print("interrupted", file=sys.stderr)
        return 130


if __name__ == "__main__":
    sys.exit(main())

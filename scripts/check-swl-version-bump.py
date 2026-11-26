#!/usr/bin/env python3
"""Fail an MR that changes swl's sources without bumping its version.

Usage: check-swl-version-bump.py TARGET

TARGET is the MR's target branch, e.g. origin/master; HEAD is the MR. The
sources are the paths below tools/swl that tools/swl/src-fileset.json lists
(the fileset of packages.swl in modules/swl.nix), and that list itself. When
the MR changes any of them since its merge base with TARGET, the version in
tools/swl/Cargo.toml at HEAD must be greater, as semver, than at the merge
base and at TARGET. Exit status: 0 when it passes, 1 when it fails, 2 on
errors.
"""
import json
import os
import re
import subprocess
import sys
import tomllib

SWL = "tools/swl"
FILESET = f"{SWL}/src-fileset.json"
CARGO_TOML = f"{SWL}/Cargo.toml"
SEMVER = re.compile(r"(\d+)\.(\d+)\.(\d+)(?:-([0-9A-Za-z.-]+))?(?:\+[0-9A-Za-z.-]+)?")


def git(*args):
    return subprocess.run(["git", *args], check=True, text=True,
                          stdout=subprocess.PIPE).stdout


def show(rev, path):
    """The file at `rev`, or None when it has none."""
    out = subprocess.run(["git", "show", f"{rev}:{path}"], text=True,
                         stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
    return out.stdout if out.returncode == 0 else None


def source_paths(*revs):
    paths = {FILESET}
    for rev in revs:
        text = show(rev, FILESET)
        if text is not None:
            paths.update(f"{SWL}/{p}" for p in json.loads(text))
    return sorted(paths)


def version(rev):
    text = show(rev, CARGO_TOML)
    return None if text is None else tomllib.loads(text)["package"]["version"]


def semver_key(v):
    """A key that orders versions by semver precedence; build metadata is
    ignored."""
    m = SEMVER.fullmatch(v)
    if not m:
        raise ValueError(f"{v!r} is not a semver version")
    major, minor, patch, pre = m.groups()
    # A release is greater than its pre-releases; pre-release identifiers
    # compare numerically when numeric, numeric ones lower than others.
    pre_key = (1,) if pre is None else (0, tuple(
        (0, int(i), "") if i.isdigit() else (1, 0, i) for i in pre.split(".")))
    return (int(major), int(minor), int(patch), pre_key)


def main(argv):
    if len(argv) != 2:
        print(__doc__.strip().splitlines()[2], file=sys.stderr)
        return 2
    target = argv[1]
    try:
        os.chdir(git("rev-parse", "--show-toplevel").strip())
        base = git("merge-base", target, "HEAD").strip()
        paths = source_paths("HEAD", base)
        changed = git("diff", "--name-only", "--no-renames", base, "HEAD",
                      "--", *paths).split()
        if not changed:
            print(f"swl is unchanged since the merge base with {target}: no version bump needed")
            return 0
        head = version("HEAD")
        if head is None:
            print(f"error: no {CARGO_TOML} at HEAD", file=sys.stderr)
            return 2
        failed = False
        for name, rev in (("the merge base", base), (target, target)):
            old = version(rev)
            if old is not None and semver_key(head) <= semver_key(old):
                print(f"swl changed ({', '.join(changed)}), but its version {head} "
                      f"is not greater than {old} at {name}: bump the version in "
                      f"{CARGO_TOML} (pre-1.0: minor for a breaking change or a new "
                      "feature, patch for a fix)", file=sys.stderr)
                failed = True
        if not failed:
            print(f"swl changed, and its version {head} is bumped")
        return 1 if failed else 0
    except (subprocess.CalledProcessError, ValueError, KeyError,
            tomllib.TOMLDecodeError, json.JSONDecodeError) as e:
        print(f"error: {e}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv))

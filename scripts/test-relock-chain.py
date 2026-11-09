#!/usr/bin/env python3
"""Disposable end-to-end test for scripts/relock-chain.py.

Creates four throwaway repositories (zwanenzang, common, swan-lake,
nixos-config) with local bare origins and git+file flake inputs, advances
zwanenzang, runs the relock chain, and asserts that every pin, commit,
push and promotion landed. Then verifies that a clean rerun is a no-op and
that a dirty checkout aborts before touching anything.

Run with: python3 scripts/test-relock-chain.py
"""

import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile

SCRIPT = Path(__file__).with_name("relock-chain.py").resolve()


def run(argv, cwd=None):
    result = subprocess.run(argv, cwd=cwd, text=True, capture_output=True)
    if result.returncode != 0:
        sys.exit(
            f"command failed: {' '.join(map(str, argv))}\n{result.stdout}\n{result.stderr}"
        )
    return result.stdout


def git(repo, *args):
    return run(["git", "-C", str(repo), *args]).strip()


def make_repo(base, name):
    work = base / name
    bare = base / f"{name}.git"
    run(["git", "init", "-q", "-b", "master", str(work)])
    run(["git", "init", "-q", "--bare", "-b", "master", str(bare)])
    git(work, "config", "user.name", "Relock Fixture")
    git(work, "config", "user.email", "relock@example.invalid")
    git(work, "remote", "add", "origin", str(bare))
    return work, bare


def commit_flake(repo, body, message):
    (repo / "flake.nix").write_text(body)
    git(repo, "add", "flake.nix")
    git(repo, "commit", "-qm", message)


def lock(repo):
    run(["nix", "flake", "lock"], cwd=repo)
    git(repo, "add", "flake.lock")
    git(repo, "commit", "-qm", "lock inputs")


def locked_rev(repo, name):
    lock_file = json.loads((repo / "flake.lock").read_text())
    key = lock_file["nodes"][lock_file["root"]]["inputs"][name]
    return lock_file["nodes"][key]["locked"]["rev"]


def clean(repo):
    return git(repo, "status", "--porcelain", "--untracked-files=no") == ""


def load_module():
    spec = importlib.util.spec_from_file_location("relock_chain", SCRIPT)
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


def main():
    with tempfile.TemporaryDirectory(prefix="relock-chain-test-") as tmp:
        base = Path(tmp)
        zz, zz_bare = make_repo(base, "zwanenzang")
        common, common_bare = make_repo(base, "swan-lake-common")
        swan, _ = make_repo(base, "swan-lake")
        nico, _ = make_repo(base, "nixos-config")

        commit_flake(zz, '{ outputs = _: { marker = "zz"; }; }\n', "zz")
        git(zz, "push", "-q", "origin", "master")

        commit_flake(
            common,
            f"""{{ inputs.zwanenzang.url = "git+file://{zz_bare}";\n  outputs = {{ self, zwanenzang }}: {{ marker = "common"; }};\n}}\n""",
            "common",
        )
        lock(common)
        git(common, "push", "-q", "origin", "master", "master:swan-lake-current")

        for name, repo in (("swan-lake", swan), ("nixos-config", nico)):
            commit_flake(
                repo,
                f"""{{ inputs.common.url = "git+file://{common_bare}?ref=swan-lake-current";\n  outputs = {{ self, common }}: {{ marker = "{name}"; }};\n}}\n""",
                name,
            )
            lock(repo)
            git(repo, "push", "-q", "origin", "master")

        module = load_module()
        module.CHAIN = (
            module.Repo(name="zwanenzang", directory="zwanenzang"),
            module.Repo(
                name="common",
                directory="swan-lake-common",
                input="zwanenzang",
                message="zwanenzang: bump pin",
                promote="swan-lake-current",
                mirror=str(common_bare),
                canonical={"type": "git", "url": zz_bare.as_uri()},
            ),
            module.Repo(
                name="swan-lake",
                directory="swan-lake",
                input="common",
                message="bump common",
                canonical={
                    "type": "git",
                    "url": common_bare.as_uri(),
                    "ref": "swan-lake-current",
                },
            ),
            module.Repo(
                name="nixos-config",
                directory="nixos-config",
                input="common",
                message="bump common",
                canonical={
                    "type": "git",
                    "url": common_bare.as_uri(),
                    "ref": "swan-lake-current",
                },
            ),
        )
        argv = [
            "--base",
            str(base),
            "--no-check",
            "--timeout",
            "10",
            "--interval",
            "0.1",
            "--repo",
            f"zwanenzang={zz}",
            "--repo",
            f"common={common}",
            "--repo",
            f"swan-lake={swan}",
            "--repo",
            f"nixos-config={nico}",
        ]

        before = {
            "zwanenzang": git(zz, "rev-parse", "HEAD"),
            "common": git(common, "rev-parse", "HEAD"),
            "swan-lake": git(swan, "rev-parse", "HEAD"),
            "nixos-config": git(nico, "rev-parse", "HEAD"),
        }
        (zz / "marker.txt").write_text("advanced\n")
        git(zz, "add", "marker.txt")
        git(zz, "commit", "-qm", "advance zwanenzang")
        git(zz, "push", "-q", "origin", "master")
        advanced = git(zz, "rev-parse", "HEAD")

        assert module.main(argv) == 0, "first run must succeed"
        for name, repo in (("common", common), ("swan-lake", swan), ("nixos-config", nico)):
            assert clean(repo), f"{name} must be clean after the run"
        assert locked_rev(common, "zwanenzang") == advanced
        common_head = git(common, "rev-parse", "HEAD")
        assert common_head != before["common"], "common must commit the bumped pin"
        assert git(common, "rev-parse", "origin/master") == common_head
        assert git(common, "rev-parse", "origin/swan-lake-current") == common_head
        for repo in (swan, nico):
            assert locked_rev(repo, "common") == common_head
            assert git(repo, "rev-parse", "HEAD") != before[repo.name]
            assert git(repo, "rev-parse", "origin/master") == git(repo, "rev-parse", "HEAD")

        rerun = {
            "zwanenzang": git(zz, "rev-parse", "HEAD"),
            "common": git(common, "rev-parse", "HEAD"),
            "swan-lake": git(swan, "rev-parse", "HEAD"),
            "nixos-config": git(nico, "rev-parse", "HEAD"),
        }
        assert module.main(argv) == 0, "second run must succeed"
        for name, repo in (("zwanenzang", zz), ("common", common), ("swan-lake", swan), ("nixos-config", nico)):
            assert git(repo, "rev-parse", "HEAD") == rerun[name], "rerun must not commit"

        (common / "flake.nix").write_text((common / "flake.nix").read_text() + "# dirty\n")
        assert module.main(argv) == 1, "dirty checkout must abort"
        assert git(common, "rev-parse", "HEAD") == common_head
        git(common, "checkout", "--", "flake.nix")

        print("\nrelock-chain test passed")


if __name__ == "__main__":
    main()

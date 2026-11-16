import json
from pathlib import Path
import tempfile
import argparse
import subprocess
import unittest
from unittest.mock import patch

import workflow as w

REV = "a" * 40
NEXT_REV = "c" * 40
HASH = "sha256-" + "A" * 43 + "="
FORGEJO = "https://forgejo.lynx-lizard.ts.net/binarin/swan-lake-common"


def lock():
    return {"version": 7, "root": "root", "nodes": {
        "root": {"inputs": {"common": "renamed", "dep": ["common", "dep"], "other": "other"}},
        "renamed": {"original": w.ORIGINAL.copy(), "locked": {
            "type": "github", "owner": "binarin", "repo": "swan-lake-common",
            "rev": REV, "narHash": HASH}, "inputs": {"dep": "dep"}},
        "dep": {"locked": {"rev": "dependency"}},
        "other": {"locked": {"rev": "unrelated"}},
    }}


def add_next(lock, original, locked=None):
    """Add a common-next node (and a root follows edge into it) to a lock."""
    lock["nodes"]["root"]["inputs"]["common-next"] = "next"
    lock["nodes"]["root"]["inputs"]["next-dep"] = ["common-next", "dep"]
    lock["nodes"]["next"] = {"original": original, "inputs": {"dep": "next-dep"}, "locked": locked or {
        **{k: v for k, v in original.items() if k != "ref"}, "rev": NEXT_REV, "narHash": HASH}}
    lock["nodes"]["next-dep"] = {"locked": {"rev": "next dependency"}}
    return lock


# Declarations and the attribute form builtins.parseFlakeRef gives for them.
NEXT_ACCEPTED = {
    w.CANONICAL: w.ORIGINAL.copy(),
    "github:binarin/swan-lake-common/sl-0034-x": dict(
        type="github", owner="binarin", repo="swan-lake-common", ref="sl-0034-x"),
    "github:binarin/swan-lake-common/" + NEXT_REV: dict(
        type="github", owner="binarin", repo="swan-lake-common", rev=NEXT_REV),
    w.TEMP + "sl-0034-x": dict(type="git", url=FORGEJO, ref="sl-0034-x"),
    "git+" + FORGEJO + ".git?ref=a/b": dict(type="git", url=FORGEJO + ".git", ref="a/b"),
    "git+" + FORGEJO + "?rev=" + NEXT_REV: dict(type="git", url=FORGEJO, rev=NEXT_REV),
    "git+https://github.com/binarin/swan-lake-common?ref=x": dict(
        type="git", url="https://github.com/binarin/swan-lake-common", ref="x"),
}
NEXT_REJECTED = [
    dict(type="github", owner="other", repo="swan-lake-common", ref="x"),
    dict(type="github", owner="binarin", repo="swan-lake", ref="x"),
    dict(type="github", owner="binarin", repo="swan-lake-common", host="example.org"),
    dict(type="github", owner="binarin", repo="swan-lake-common", dir="sub"),
    dict(type="github", owner="binarin", repo="swan-lake-common", rev="abc"),
    dict(type="github", owner="Binarin", repo="swan-lake-common"),
    dict(type="git", url="https://forgejo.lynx-lizard.ts.net/binarin/swan-lake", ref="x"),
    dict(type="git", url="https://example.org/binarin/swan-lake-common", ref="x"),
    dict(type="git", url="http://forgejo.lynx-lizard.ts.net/binarin/swan-lake-common", ref="x"),
    dict(type="git", url="file:///tmp/swan-lake-common"),
    dict(type="git", url="ssh://git@forgejo.lynx-lizard.ts.net/binarin/swan-lake-common"),
    dict(type="git", url=FORGEJO, ref="x", shallow=True),
    dict(type="git", url=FORGEJO, ref="x", submodules=True),
    dict(type="git", url=FORGEJO, ref=""),
    dict(type="path", path="/tmp/swan-lake-common"),
    dict(type="indirect", id="swan-lake-common"),
    dict(type="tarball", url=FORGEJO + "/archive/master.tar.gz"),
]


class Guard(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.lock = lock()

    def check(self, decl=None, next_decl=None, parsed=None, ancestry="REPORT"):
        (self.root / "flake.lock").write_text(json.dumps(self.lock))
        decls = {"common": decl or {"url": w.CANONICAL}, "common-next": next_decl}
        with patch.object(w, "declaration", side_effect=lambda _, name="common": decls[name]), \
                patch.object(w, "parse_ref", side_effect=lambda url: parsed if parsed is not None else NEXT_ACCEPTED[url]), \
                patch.object(w, "next_ancestry", return_value=ancestry) as report, \
                patch("sys.stderr") as stderr:
            w.check(self.root)
        return report, stderr

    def test_canonical_renamed_and_stale(self):
        self.check()
        self.lock["nodes"]["renamed"]["locked"]["rev"] = "b" * 40
        self.check()

    def test_declarations(self):
        for url in [w.TEMP + "feature", "path:/tmp/common", "git+file:/tmp/common", "github:other/repo/ref"]:
            with self.subTest(url=url), self.assertRaises(ValueError):
                self.check({"url": url})
        with self.assertRaisesRegex(ValueError, "temporary pin present"):
            self.check({"url": w.TEMP + "feature"})
        with self.assertRaises(ValueError):
            self.check({"url": w.CANONICAL, "inputs": {"dep": {"follows": "other"}}})

    def test_bad_fields(self):
        for section, fields in [("original", ["type", "owner", "repo", "ref"]),
                                ("locked", ["type", "owner", "repo", "rev", "narHash"])]:
            for field in fields:
                for value in [None, "bad"]:
                    with self.subTest(section=section, field=field, value=value):
                        self.lock = lock()
                        if value is None:
                            del self.lock["nodes"]["renamed"][section][field]
                        else:
                            self.lock["nodes"]["renamed"][section][field] = value
                        with self.assertRaises(ValueError):
                            self.check()

    def test_follows_and_missing(self):
        for edge in [["other"], "missing", None]:
            self.lock["nodes"]["root"]["inputs"]["common"] = edge
            with self.assertRaises((ValueError, KeyError)):
                self.check()


class NextGuard(Guard):
    def test_accepted_refs(self):
        for url, original in NEXT_ACCEPTED.items():
            with self.subTest(url=url):
                self.lock = add_next(lock(), original)
                report, stderr = self.check(next_decl={"url": url})
                report.assert_called_once()
                self.assertEqual(report.call_args.args[1], REV)
                written = "".join(c.args[0] for c in stderr.write.call_args_list)
                self.assertIn("check-common-pin: REPORT", written)

    def test_git_locked_with_rev_count(self):
        original = NEXT_ACCEPTED[w.TEMP + "sl-0034-x"]
        self.lock = add_next(lock(), original, dict(
            type="git", url=FORGEJO, ref="sl-0034-x", rev=NEXT_REV, narHash=HASH,
            lastModified=1, revCount=3))
        self.check(next_decl={"url": w.TEMP + "sl-0034-x"})

    def test_rejected_repositories_and_options(self):
        for original in NEXT_REJECTED:
            with self.subTest(original=original):
                with self.assertRaises(ValueError):
                    w.check_next_original(original)
                self.lock = add_next(lock(), original)
                with self.assertRaises(ValueError):
                    self.check(next_decl={"url": "x"}, parsed=original)
        for original in NEXT_ACCEPTED.values():
            w.check_next_original(original)

    def test_declaration_lock_mismatch(self):
        url = w.TEMP + "sl-0034-x"
        self.lock = lock()
        with self.assertRaisesRegex(ValueError, "mismatch"):
            self.check(next_decl={"url": url})
        self.lock = add_next(lock(), NEXT_ACCEPTED[url])
        with self.assertRaisesRegex(ValueError, "mismatch"):
            self.check()
        stale = dict(NEXT_ACCEPTED[url], ref="other")
        self.lock = add_next(lock(), stale)
        with self.assertRaisesRegex(ValueError, "mismatch"):
            self.check(next_decl={"url": url})
        self.lock = add_next(lock(), NEXT_ACCEPTED[url])
        for decl in [{"url": url, "inputs": {"nixpkgs": {"follows": "nixpkgs"}}},
                     {"url": url, "flake": False}, {"follows": "common"}]:
            with self.subTest(decl=decl), self.assertRaisesRegex(ValueError, "exactly a url"):
                self.check(next_decl=decl)

    def test_follows_and_not_flake(self):
        url = w.TEMP + "sl-0034-x"
        self.lock = add_next(lock(), NEXT_ACCEPTED[url])
        self.lock["nodes"]["root"]["inputs"]["common-next"] = ["common"]
        with self.assertRaisesRegex(ValueError, "direct input"):
            self.check(next_decl={"url": url})
        self.lock = add_next(lock(), NEXT_ACCEPTED[url])
        self.lock["nodes"]["next"]["flake"] = False
        with self.assertRaisesRegex(ValueError, "must be a flake"):
            self.check(next_decl={"url": url})

    def test_bad_locked(self):
        url = w.TEMP + "sl-0034-x"
        good = dict(type="git", url=FORGEJO, ref="sl-0034-x", rev=NEXT_REV, narHash=HASH)
        for field, value in [("url", FORGEJO + ".git"), ("url", "https://example.org/x"),
                             ("type", "github"), ("ref", "other"), ("rev", "abc"), ("rev", None),
                             ("narHash", None), ("narHash", "sha1-x"), ("dir", "sub"), ("shallow", True)]:
            with self.subTest(field=field, value=value), self.assertRaises(ValueError):
                locked = dict(good)
                if value is None:
                    del locked[field]
                else:
                    locked[field] = value
                self.lock = add_next(lock(), NEXT_ACCEPTED[url], locked)
                self.check(next_decl={"url": url})
        pinned = "git+" + FORGEJO + "?rev=" + NEXT_REV
        locked = dict(type="git", url=FORGEJO, rev="d" * 40, narHash=HASH)
        self.lock = add_next(lock(), NEXT_ACCEPTED[pinned], locked)
        with self.assertRaisesRegex(ValueError, "locked rev differs"):
            self.check(next_decl={"url": pinned})

    def test_common_stays_canonical(self):
        url = w.TEMP + "sl-0034-x"
        self.lock = add_next(lock(), NEXT_ACCEPTED[url])
        with self.assertRaisesRegex(ValueError, "temporary pin present"):
            self.check({"url": w.TEMP + "feature"}, next_decl={"url": url})
        with self.assertRaises(ValueError):
            self.check({"url": "github:binarin/swan-lake-common/other"}, next_decl={"url": url})


def git(*args, cwd):
    return subprocess.run(["git", "-c", "user.name=Fixture", "-c", "user.email=fixture@example.org",
                           *args], cwd=cwd, check=True, text=True, capture_output=True).stdout.strip()


class Ancestry(unittest.TestCase):
    def test_report(self):
        with tempfile.TemporaryDirectory() as tmp:
            repo = Path(tmp) / "repo"
            repo.mkdir()
            git("init", "-q", "-b", "master", cwd=repo)
            git("commit", "-q", "--allow-empty", "-m", "base", cwd=repo)
            base = git("rev-parse", "HEAD", cwd=repo)
            git("commit", "-q", "--allow-empty", "-m", "next", cwd=repo)
            child = git("rev-parse", "HEAD", cwd=repo)
            git("checkout", "-q", "-b", "side", base, cwd=repo)
            git("commit", "-q", "--allow-empty", "-m", "side", cwd=repo)
            side = git("rev-parse", "HEAD", cwd=repo)
            source = {"type": "git", "url": repo.as_uri()}
            self.assertIn("equals", w.next_ancestry(dict(source, rev=base), base))
            self.assertRegex(w.next_ancestry(dict(source, rev=child), base),
                             f"common-next {child[:12]} is based on common {base[:12]}$")
            self.assertIn("is NOT based on common", w.next_ancestry(dict(source, rev=side), child))
            self.assertIn("is NOT based on common", w.next_ancestry(dict(source, rev=base), child))
            missing = {"type": "git", "url": (Path(tmp) / "missing").as_uri(), "rev": child}
            self.assertIn("cannot determine", w.next_ancestry(missing, base))
            self.assertIn("cannot determine", w.next_ancestry(dict(source, rev="f" * 40), base))


class Overrides(unittest.TestCase):
    def test_adapters(self):
        url = "path:/directory%20with%20spaces"
        flags = ["--override-input", "common", url, "--no-write-lock-file"]
        for prefix in [["nix", "build"], ["nix", "flake", "check"]]:
            self.assertEqual(w.override_argv(prefix + [".", "--", "a b"], url),
                             prefix + flags + [".", "--", "a b"])
        command = ["nixos-rebuild", "build", "--flake", ".#host"]
        self.assertEqual(w.override_argv(command, url), command[:1] + flags + command[1:])
        self.assertEqual(w.override_argv(["deploy", ".#host"], url),
                         ["deploy", ".#host", "--"] + flags)
        self.assertEqual(w.override_argv(["deploy", ".#host", "--", "--offline"], url),
                         ["deploy", ".#host", "--"] + flags + ["--offline"])

    def test_checkout_selection(self):
        with tempfile.TemporaryDirectory(prefix="checkout space ") as tmp:
            root = Path(tmp) / "consumer"
            root.mkdir()
            common = root.parent / "swan-lake-common"
            cwd = Path.cwd()
            args = argparse.Namespace(common=None, dirty=False, input="common",
                                      command=["--", "nix", "build", "a b"])
            with patch.object(w, "run", return_value=str(root)):
                with self.assertRaisesRegex(ValueError, "use --common"):
                    w.with_common(args)
                common.mkdir()
                (common / "flake.nix").write_text("{}")
                for dirty in [False, True]:
                    args.dirty = dirty
                    with patch.object(w.os, "execvp") as execute:
                        w.with_common(args)
                    cmd = execute.call_args.args[1]
                    self.assertEqual(cmd[-1], "a b")
                    self.assertIn("%20", cmd[4])
                    self.assertTrue(cmd[4].startswith("path:" if dirty else "git+file:"))
                    self.assertEqual(Path.cwd(), cwd)

    def test_input_name(self):
        flags = ["--override-input", "common-next", "path:/tmp", "--no-write-lock-file"]
        self.assertEqual(w.override_argv(["nix", "build", "."], "path:/tmp", "common-next"),
                         ["nix", "build"] + flags + ["."])

    def test_reject(self):
        for command in [[], ["nix", "run"], ["sh", "-c", "nix build"], ["nixos-rebuild", "build"]]:
            with self.assertRaises(ValueError):
                w.override_argv(command, "path:/tmp")


class Finalization(unittest.TestCase):
    def test_equivalent_and_newer(self):
        old, new = lock(), lock()
        old["nodes"]["renamed"]["locked"]["type"] = "git"
        self.assertTrue(w.validate_conversion(old, new))
        new["nodes"]["renamed"]["locked"]["rev"] = "b" * 40
        new["nodes"]["dep"]["locked"]["rev"] = "new dependency"
        self.assertFalse(w.validate_conversion(old, new))

    def test_bad_conversion(self):
        for node, field, value in [("renamed", "narHash", "bad"),
                                   ("dep", "rev", "changed"), ("other", "rev", "changed")]:
            old, new = lock(), lock()
            new["nodes"][node]["locked"][field] = value
            with self.subTest(node=node), self.assertRaises(ValueError):
                w.validate_conversion(old, new)

    def test_shared_dependency_of_other_input(self):
        def shared():
            lock_ = add_next(lock(), w.ORIGINAL.copy())
            lock_["nodes"]["next"]["inputs"]["dep"] = "dep"
            del lock_["nodes"]["next-dep"]
            return lock_
        for name, key in [("common", "renamed"), ("common-next", "next")]:
            with self.subTest(name=name):
                old, new = shared(), shared()
                new["nodes"][key]["locked"]["rev"] = "b" * 40
                self.assertFalse(w.validate_conversion(old, new, name))
                new["nodes"]["dep"]["locked"]["rev"] = "changed"
                other = "common-next" if name == "common" else "common"
                with self.assertRaisesRegex(ValueError, f"resolved graph of {other} changed"):
                    w.validate_conversion(old, new, name)

    def test_dependency_declaration_churn(self):
        old, new = lock(), lock()
        new["nodes"]["dep"]["original"] = {"type": "path", "path": "/tmp/other"}
        with self.assertRaisesRegex(ValueError, "dependency graph"):
            w.validate_conversion(old, new)

    def test_resolved_follows_graph(self):
        old, new = lock(), lock()
        old["nodes"]["root"]["inputs"]["dep"] = "dep"
        new["nodes"]["root"]["inputs"]["dep"] = "dep"
        new["nodes"]["renamed"]["inputs"]["dep"] = ["dep"]
        self.assertTrue(w.validate_conversion(old, new))
        new["nodes"]["root"]["inputs"]["dep"] = ["dep"]
        with self.assertRaises(ValueError):
            w.graph(new, "renamed")

    def test_poll_timeout_and_moving_ref(self):
        def info(rev):
            return json.dumps({"locked": {"rev": rev}})
        with patch.object(w, "run", side_effect=[info("old"), info(REV), info("later")]):
            self.assertEqual(w.promoted(1, 0, REV), REV)
        with patch.object(w, "run", return_value=info("old")):
            with self.assertRaisesRegex(ValueError, "timed out"):
                w.promoted(0, 0, REV)
        with patch.object(w, "run", return_value=info("later")):
            self.assertEqual(w.promoted(0, 0, None), "later")

    def staging(self, name, url, newer=False, churn=None, next_original=None):
        """Run finalize on a committed fixture with `nix flake lock` simulated."""
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            old = lock()
            temp_original = {"type": "git", "url": FORGEJO, "ref": "feature"}
            temp_locked = dict(temp_original, rev=REV, narHash=HASH)
            urls = {"common": w.CANONICAL, "common-next": w.CANONICAL}
            if name == "common":
                old["nodes"]["renamed"].update(original=temp_original, locked=temp_locked)
            add_next(old, next_original or w.ORIGINAL.copy())
            urls[name] = url
            (root / "flake.nix").write_text(
                "{ inputs.common.url = " + json.dumps(urls["common"]) + "; inputs.common-next.url = "
                + json.dumps(urls["common-next"]) + "; outputs = _: {}; }")
            (root / "flake.lock").write_text(json.dumps(old))
            subprocess.run(["git", "init", "-q", str(root)], check=True)
            git("add", ".", cwd=root)
            git("commit", "-qm", "fixture", cwd=root)
            (root / "unrelated").write_text("preserve me")
            snapshot = [(root / f).read_bytes() for f in ["flake.nix", "flake.lock"]]
            tested = old["nodes"]["renamed" if name == "common" else "next"]["locked"]["rev"]
            selected = "b" * 40 if newer else tested
            real_run = w.run

            def staged_lock(cmd, **kwargs):
                if cmd[:3] != ["nix", "flake", "lock"]:
                    return real_run(cmd, **kwargs)
                self.assertEqual(cmd[4:], ["--override-input", name, f"github:binarin/swan-lake-common/{selected}"])
                new = add_next(lock(), w.ORIGINAL.copy())
                node = new["nodes"]["renamed" if name == "common" else "next"]
                node["locked"]["rev"] = selected
                if churn:
                    new["nodes"][churn]["locked"]["rev"] = "bad"
                (Path(cmd[3]) / "flake.lock").write_text(json.dumps(new))
                return ""

            def decl(path, input_name="common"):
                staged = urls if path == root else {"common": w.CANONICAL, "common-next": w.CANONICAL}
                return {"url": staged[input_name]}

            parsed = {w.CANONICAL: w.ORIGINAL.copy(), url: next_original}
            args = argparse.Namespace(root=str(root), input=name, timeout=0, interval=0, allow_newer=newer)
            with patch.object(w, "run", side_effect=staged_lock), \
                    patch.object(w, "promoted", return_value=selected) as promoted, \
                    patch.object(w, "declaration", side_effect=decl), \
                    patch.object(w, "parse_ref", side_effect=parsed.__getitem__), \
                    patch("sys.stdout"):
                try:
                    w.finalize(args)
                except ValueError:
                    self.assertEqual(snapshot, [(root / f).read_bytes() for f in ["flake.nix", "flake.lock"]])
                    raise
                finally:
                    self.assertEqual((root / "unrelated").read_text(), "preserve me")
            promoted.assert_called_once_with(0, 0, None if newer else tested)
            text = (root / "flake.nix").read_text()
            self.assertNotIn(json.dumps(url), text)
            self.assertEqual(text.count(json.dumps(w.CANONICAL)), 2)
            return json.loads((root / "flake.lock").read_text())

    def test_full_staging(self):
        url = w.TEMP + "feature"
        for newer in [False, True]:
            with self.subTest(newer=newer):
                new = self.staging("common", url, newer=newer)
                self.assertEqual(new["nodes"]["renamed"]["original"], w.ORIGINAL)
        with self.assertRaisesRegex(ValueError, "unrelated lock churn: other"):
            self.staging("common", url, churn="other")
        with self.assertRaisesRegex(ValueError, "unrelated lock churn: next"):
            self.staging("common", url, churn="next")

    def test_full_staging_common_next(self):
        for url, original in NEXT_ACCEPTED.items():
            if url == w.CANONICAL:
                continue
            for newer in [False, True]:
                with self.subTest(url=url, newer=newer):
                    new = self.staging("common-next", url, newer=newer, next_original=original)
                    self.assertEqual(new["nodes"]["next"]["original"], w.ORIGINAL)
                    self.assertEqual(new["nodes"]["root"]["inputs"]["next-dep"], ["common-next", "dep"])
        url = w.TEMP + "feature"
        original = {"type": "git", "url": FORGEJO, "ref": "feature"}
        for node in ["renamed", "dep", "other"]:
            with self.subTest(churn=node), self.assertRaisesRegex(ValueError, f"unrelated lock churn: {node}"):
                self.staging("common-next", url, churn=node, next_original=original)
        with self.assertRaisesRegex(ValueError, "same revision changed resolved dependency graph"):
            self.staging("common-next", url, churn="next-dep", next_original=original)
        new = self.staging("common-next", url, newer=True, churn="next-dep", next_original=original)
        self.assertEqual(new["nodes"]["next-dep"]["locked"]["rev"], "bad")

    def test_common_next_refusals(self):
        with self.assertRaisesRegex(ValueError, "nothing to finalize"):
            self.staging("common-next", w.CANONICAL, next_original=w.ORIGINAL.copy())
        other = {"type": "github", "owner": "other", "repo": "swan-lake-common", "ref": "x"}
        with self.assertRaisesRegex(ValueError, "swan-lake-common repository"):
            self.staging("common-next", "github:other/swan-lake-common/x", next_original=other)

    def test_install_and_failure(self):
        with tempfile.TemporaryDirectory() as tmp:
            paths = [Path(tmp) / name for name in ["flake.nix", "flake.lock"]]
            for p in paths:
                p.write_bytes(b"original")
            snapshot = [b"original"] * 2
            replace = w.os.replace
            for error in [OSError("write failed"), KeyboardInterrupt()]:
                calls = []

                def fail_second(src, dst):
                    calls.append(dst)
                    if len(calls) == 2:
                        raise error
                    replace(src, dst)

                with patch.object(w.os, "replace", side_effect=fail_second):
                    with self.assertRaises(type(error)):
                        w.install(paths, snapshot, [b"new"] * 2)
                self.assertEqual([p.read_bytes() for p in paths], snapshot)
                self.assertEqual(len(list(Path(tmp).iterdir())), 2)
            paths[0].write_bytes(b"concurrent edit")
            with self.assertRaisesRegex(ValueError, "originals changed"):
                w.install(paths, snapshot, [b"new"] * 2)
            self.assertEqual(paths[0].read_bytes(), b"concurrent edit")
            self.assertEqual(paths[1].read_bytes(), b"original")
            w.install(paths, [p.read_bytes() for p in paths], [b"new"] * 2)
            self.assertEqual([p.read_bytes() for p in paths], [b"new"] * 2)


if __name__ == "__main__":
    unittest.main()

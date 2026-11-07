import json
from pathlib import Path
import tempfile
import argparse
import subprocess
import unittest
from unittest.mock import patch

import workflow as w

REV = "a" * 40
HASH = "sha256-" + "A" * 43 + "="


def lock():
    return {"version": 7, "root": "root", "nodes": {
        "root": {"inputs": {"common": "renamed", "dep": ["common", "dep"], "other": "other"}},
        "renamed": {"original": w.ORIGINAL.copy(), "locked": {
            "type": "github", "owner": "binarin", "repo": "swan-lake-common",
            "rev": REV, "narHash": HASH}, "inputs": {"dep": "dep"}},
        "dep": {"locked": {"rev": "dependency"}},
        "other": {"locked": {"rev": "unrelated"}},
    }}


class Guard(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.lock = lock()

    def check(self, decl=None):
        (self.root / "flake.lock").write_text(json.dumps(self.lock))
        with patch.object(w, "declaration", return_value=decl or {"url": w.CANONICAL}):
            w.check(self.root)

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

    def test_full_staging(self):
        for newer, churn in [(False, False), (True, False), (False, True)]:
            with self.subTest(newer=newer, churn=churn), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                url = w.TEMP + "feature"
                old = lock()
                old["nodes"]["renamed"]["original"] = {
                    "type": "git", "url": w.TEMP[4:].split("?", 1)[0], "ref": "feature"}
                old["nodes"]["renamed"]["locked"]["type"] = "git"
                (root / "flake.nix").write_text('{ inputs.common.url = ' + json.dumps(url) + '; outputs = _: {}; }')
                (root / "flake.lock").write_text(json.dumps(old))
                subprocess.run(["git", "init", "-q", str(root)], check=True)
                subprocess.run(["git", "-C", str(root), "add", "."], check=True)
                subprocess.run(["git", "-C", str(root), "-c", "user.name=Fixture", "-c",
                                "user.email=fixture@example.org", "commit", "-qm", "fixture"], check=True)
                (root / "unrelated").write_text("preserve me")
                snapshot = [(root / name).read_bytes() for name in ["flake.nix", "flake.lock"]]
                selected = "b" * 40 if newer else REV
                real_run = w.run

                def staged_lock(cmd, **kwargs):
                    if cmd[:3] != ["nix", "flake", "lock"]:
                        return real_run(cmd, **kwargs)
                    new = lock()
                    new["nodes"]["renamed"]["locked"]["rev"] = selected
                    if churn:
                        new["nodes"]["other"]["locked"]["rev"] = "bad"
                    (Path(cmd[3]) / "flake.lock").write_text(json.dumps(new))
                    self.assertEqual(cmd[-1], f"github:binarin/swan-lake-common/{selected}")
                    return ""

                def decl(path):
                    return {"url": url if path == root else w.CANONICAL}

                args = argparse.Namespace(root=str(root), timeout=0, interval=0, allow_newer=newer)
                with patch.object(w, "run", side_effect=staged_lock), patch.object(w, "promoted", return_value=selected), patch.object(w, "declaration", side_effect=decl):
                    if churn:
                        with self.assertRaisesRegex(ValueError, "unrelated lock churn"):
                            w.finalize(args)
                        self.assertEqual(snapshot, [(root / name).read_bytes() for name in ["flake.nix", "flake.lock"]])
                    else:
                        w.finalize(args)
                        self.assertIn(w.CANONICAL, (root / "flake.nix").read_text())
                self.assertEqual((root / "unrelated").read_text(), "preserve me")

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

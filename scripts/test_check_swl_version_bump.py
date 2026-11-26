"""check-swl-version-bump.py against scratch git repos: an MR branch off
master, checked with master as the target."""
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

SCRIPT = Path(__file__).resolve().parent / "check-swl-version-bump.py"
FILESET = '["Cargo.toml", "Cargo.lock", "src", "tests"]\n'


def cargo_toml(version):
    return f'[package]\nname = "swl"\nversion = "{version}"\nedition = "2024"\n'


class CheckSwlVersionBump(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.repo = Path(self.tmp.name)
        self.env = dict(os.environ, HOME=self.tmp.name, GIT_CONFIG_NOSYSTEM="1",
                        GIT_AUTHOR_NAME="t", GIT_AUTHOR_EMAIL="t@example.org",
                        GIT_COMMITTER_NAME="t", GIT_COMMITTER_EMAIL="t@example.org")
        self.git("init", "-q", "-b", "master")
        self.commit({"tools/swl/src-fileset.json": FILESET,
                     "tools/swl/Cargo.toml": cargo_toml("0.2.0"),
                     "tools/swl/Cargo.lock": "lock\n",
                     "tools/swl/src/main.rs": "fn main() {}\n",
                     "tools/swl/README.md": "swl\n",
                     "modules/swl.nix": "{ }\n"})
        self.git("checkout", "-q", "-b", "mr")

    def tearDown(self):
        self.tmp.cleanup()

    def git(self, *args):
        return subprocess.run(["git", *args], cwd=self.repo, env=self.env, check=True,
                              text=True, stdout=subprocess.PIPE).stdout

    def commit(self, files, branch=None):
        if branch:
            self.git("checkout", "-q", branch)
        for path, text in files.items():
            p = self.repo / path
            p.parent.mkdir(parents=True, exist_ok=True)
            p.write_text(text)
        self.git("add", "-A")
        self.git("commit", "-qm", "change")
        if branch:
            self.git("checkout", "-q", "mr")

    def check(self, target="master"):
        out = subprocess.run([sys.executable, str(SCRIPT), target], cwd=self.repo / "tools",
                             env=self.env, text=True, capture_output=True)
        return out.returncode, out.stdout + out.stderr

    def assert_passes(self):
        code, out = self.check()
        self.assertEqual(code, 0, out)

    def assert_fails(self, needle):
        code, out = self.check()
        self.assertEqual(code, 1, out)
        self.assertIn(needle, out)

    def test_no_change_passes(self):
        self.assert_passes()

    def test_unrelated_change_passes(self):
        self.commit({"tools/swl/README.md": "more\n", "modules/swl.nix": "{ x }\n"})
        self.assert_passes()

    def test_change_without_bump_fails(self):
        self.commit({"tools/swl/src/main.rs": "fn main() { }\n"})
        self.assert_fails("swl changed (tools/swl/src/main.rs), but its version 0.2.0 "
                          "is not greater than 0.2.0 at the merge base")

    def test_change_with_bump_passes(self):
        self.commit({"tools/swl/src/main.rs": "fn main() { }\n",
                     "tools/swl/Cargo.toml": cargo_toml("0.2.1")})
        self.assert_passes()

    def test_equal_version_fails(self):
        # Only the build metadata differs: the same version.
        self.commit({"tools/swl/src/main.rs": "fn main() { }\n",
                     "tools/swl/Cargo.toml": cargo_toml("0.2.0+other")})
        self.assert_fails("is not greater than 0.2.0")

    def test_lower_version_fails(self):
        self.commit({"tools/swl/Cargo.toml": cargo_toml("0.1.9")})
        self.assert_fails("its version 0.1.9 is not greater than 0.2.0")

    def test_semver_order_is_not_string_order(self):
        self.commit({"tools/swl/src/new.rs": "\n",
                     "tools/swl/Cargo.toml": cargo_toml("0.10.0")})
        self.assert_passes()

    def test_a_pre_release_is_lower_than_its_release(self):
        self.commit({"tools/swl/Cargo.toml": cargo_toml("0.2.0-rc.1")})
        self.assert_fails("its version 0.2.0-rc.1 is not greater than 0.2.0")

    def test_a_bump_the_target_already_made_fails(self):
        # Master moved to 0.3.0 meanwhile; the MR's 0.3.0 off the old master
        # is no bump over it.
        self.commit({"tools/swl/Cargo.toml": cargo_toml("0.3.0")}, branch="master")
        self.commit({"tools/swl/tests/t.rs": "\n",
                     "tools/swl/Cargo.toml": cargo_toml("0.3.0")})
        self.assert_fails("is not greater than 0.3.0 at master")

    def test_a_removed_or_renamed_source_counts(self):
        self.git("mv", "tools/swl/src/main.rs", "tools/swl/main.rs")
        self.git("commit", "-qm", "move")
        self.assert_fails("tools/swl/src/main.rs")

    def test_a_path_added_to_the_fileset_counts(self):
        self.commit({"tools/swl/build.rs": "fn main() {}\n"})
        self.assert_passes()
        self.commit({"tools/swl/src-fileset.json":
                     '["Cargo.toml", "Cargo.lock", "build.rs", "src", "tests"]\n'})
        self.assert_fails("tools/swl/build.rs")

    def test_bad_arguments_and_target_are_errors(self):
        code, _ = self.check(target="no-such-branch")
        self.assertEqual(code, 2)
        out = subprocess.run([sys.executable, str(SCRIPT)], cwd=self.repo,
                             text=True, capture_output=True)
        self.assertEqual(out.returncode, 2)
        self.assertIn("Usage:", out.stderr)


if __name__ == "__main__":
    unittest.main()

"""Opt-in real-Nix smoke test: python3 tools/common-workflow/integration.py.

Uses only disposable local flakes; no network, activation or deployment.
"""
import json
from pathlib import Path
import subprocess
import tempfile
from urllib.parse import quote

SOURCE = Path(__file__).with_name("workflow.py").resolve()


def run(args, cwd, **kwargs):
    return subprocess.run(args, cwd=cwd, check=True, text=True, **kwargs)


with tempfile.TemporaryDirectory(prefix="common workflow ") as tmp:
    base = Path(tmp)
    common, consumer = base / "swan-lake-common", base / "consumer"
    common.mkdir()
    consumer.mkdir()
    (consumer / "subdir").mkdir()
    for root in [common, consumer]:
        run(["git", "init", "-q", str(root)], base)
    (common / "flake.nix").write_text('{ outputs = _: { marker = "committed"; }; }')
    run(["git", "add", "."], common)
    run(["git", "-c", "user.name=Fixture", "-c", "user.email=fixture@example.org",
         "commit", "-qm", "fixture"], common)
    (consumer / "flake.nix").write_text('''{
      inputs.common.url = PLACEHOLDER;
      outputs = { self, common }: let
        build = builtins.derivation {
          name = common.marker; system = "x86_64-linux";
          builder = "/bin/sh"; args = [ "-c" "exit 0" ];
        };
      in { packages.x86_64-linux.default = build; };
    }'''.replace("PLACEHOLDER", json.dumps("git+file:" + quote(str(common), safe="/"))))
    run(["git", "add", "."], consumer)
    run(["nix", "flake", "lock"], consumer)
    links = base / "bin"
    links.mkdir()
    for name in ["with-common", "check-common-pin"]:
        (links / name).symlink_to(SOURCE)
    command = ["python3", str(links / "with-common")]
    before = [(consumer / name).read_bytes() for name in ["flake.nix", "flake.lock"]]
    for dirty in [False, True]:
        options = ["--dirty"] if dirty else []
        run(command + options + ["--", "nix", "build", "..", "--dry-run", "--no-link"], consumer / "subdir")
        run(command + ["--common", str(common)] + options + ["--", "nix", "flake", "check", "..", "--no-build"], consumer / "subdir")
    assert before == [(consumer / name).read_bytes() for name in ["flake.nix", "flake.lock"]]
    failed = subprocess.run(command + ["--", "nix", "build", ".#missing", "--no-link"], cwd=consumer)
    assert failed.returncode != 0
    # Real Nix evaluation of a canonical declaration and renamed locked node.
    (consumer / "flake.nix").write_text('{ inputs.common.url = "github:binarin/swan-lake-common/swan-lake-current"; outputs = _: {}; }')
    (consumer / "flake.lock").write_text(json.dumps({"root": "root", "nodes": {
        "root": {"inputs": {"common": "renamed"}},
        "renamed": {"original": {"type": "github", "owner": "binarin", "repo": "swan-lake-common", "ref": "swan-lake-current"},
                    "locked": {"type": "github", "owner": "binarin", "repo": "swan-lake-common", "rev": "a" * 40, "narHash": "sha256-" + "A" * 43 + "="}}}}))
    run(["python3", str(links / "check-common-pin"), "--root", str(consumer)], base)
    print("Real Nix override/guard integration passed; consumer inputs unchanged during overrides.")

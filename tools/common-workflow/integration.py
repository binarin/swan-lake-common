"""Opt-in real-Nix smoke test: python3 tools/common-workflow/integration.py.

Uses only disposable local flakes; no network, activation or deployment.
"""
import json
from pathlib import Path
import subprocess
import sys
import tempfile
from urllib.parse import quote

SOURCE = Path(__file__).with_name("workflow.py").resolve()
sys.path.insert(0, str(SOURCE.parent))
import workflow  # noqa: E402
from test_workflow import NEXT_ACCEPTED  # noqa: E402


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

    # The fixture forms of common-next declarations match what Nix parses.
    for url, original in NEXT_ACCEPTED.items():
        assert workflow.parse_ref(url) == original, (url, workflow.parse_ref(url))

    # common-next at a Forgejo branch, with a root input following into it.
    # The locked revision equals common's, so the ancestry report needs no network.
    next_url = workflow.TEMP + "sl-0034-x"
    forgejo = workflow.TEMP[4:].split("?", 1)[0]
    (consumer / "flake.nix").write_text(
        '{ inputs.common.url = "github:binarin/swan-lake-common/swan-lake-current"; '
        'inputs.common-next.url = ' + json.dumps(next_url) + '; '
        'inputs.nixpkgs-next.follows = "common-next/nixpkgs"; outputs = _: {}; }')
    fixture = json.loads((consumer / "flake.lock").read_text())
    fixture["nodes"]["root"]["inputs"].update({"common-next": "common-next",
                                               "nixpkgs-next": ["common-next", "nixpkgs"]})
    fixture["nodes"]["common-next"] = {
        "original": {"type": "git", "url": forgejo, "ref": "sl-0034-x"},
        "locked": {"type": "git", "url": forgejo, "ref": "sl-0034-x", "rev": "a" * 40,
                   "narHash": "sha256-" + "A" * 43 + "=", "lastModified": 1, "revCount": 1}}
    (consumer / "flake.lock").write_text(json.dumps(fixture))
    checked = run(["python3", str(links / "check-common-pin"), "--root", str(consumer)], base,
                  capture_output=True)
    assert "common-next aaaaaaaaaaaa equals the locked common revision" in checked.stderr, checked.stderr
    (consumer / "flake.nix").write_text((consumer / "flake.nix").read_text().replace(
        json.dumps(next_url), json.dumps("github:other/swan-lake-common/x")))
    rejected = subprocess.run(["python3", str(links / "check-common-pin"), "--root", str(consumer)],
                              cwd=base, text=True, capture_output=True)
    assert rejected.returncode == 1 and "swan-lake-common repository" in rejected.stderr, rejected.stderr

    # with-common --input common-next overrides only common-next.
    pair = base / "pair"
    pair.mkdir()
    run(["git", "init", "-q", str(pair)], base)
    (pair / "flake.nix").write_text("""{
      inputs.common.url = COMMON;
      inputs.common-next.url = COMMON;
      outputs = { self, common, common-next }: {
        packages.x86_64-linux.default = builtins.derivation {
          name = "${common.marker}-${common-next.marker}"; system = "x86_64-linux";
          builder = "/bin/sh"; args = [ "-c" "exit 0" ];
        };
      };
    }""".replace("COMMON", json.dumps("git+file:" + quote(str(common), safe="/"))))
    run(["git", "add", "."], pair)
    run(["nix", "flake", "lock"], pair)
    experiment = base / "experiment"
    experiment.mkdir()
    (experiment / "flake.nix").write_text('{ outputs = _: { marker = "experiment"; }; }')
    before = [(pair / name).read_bytes() for name in ["flake.nix", "flake.lock"]]
    built = run(command + ["--common", str(experiment), "--dirty", "--input", "common-next", "--",
                           "nix", "build", ".", "--dry-run", "--no-link", "--json"], pair,
                capture_output=True)
    assert "committed-experiment" in built.stdout, built.stdout
    assert before == [(pair / name).read_bytes() for name in ["flake.nix", "flake.lock"]]
    print("Real Nix override/guard integration passed; consumer inputs unchanged during overrides.")

# Common upgrades across repositories

## Overview

Common stages changes on Forgejo master; the human promotes a master revision
by advancing `swan-lake-current`. Forgejo push-mirrors to GitHub. Consumers
commit `github:binarin/swan-lake-common/swan-lake-current` and a revision lock.
A paired consumer MR may temporarily reference the common Forgejo feature
branch, but must restore the canonical declaration before merge. A consumer may
also carry one experiment input, `common-next` (see below).

These tools support local iteration, CI policy and finalization. They never
merge, promote, trigger mirror writes, commit or push. Inventory, extraction and
reverse-dependency CI are outside this workflow.

## Local iteration

Enter the consumer shell (`nix develop`), then:

```sh
with-common -- nix build .#some-package
with-common --common /path/to/common -- nix flake check
with-common --common /path/to/common --dirty -- nix build .#some-package
with-common -- nixos-rebuild build --flake .#host
with-common -- deploy .#host -- --show-trace
with-common --common /path/to/common --input common-next -- nixos-rebuild build --flake .#host
```

The default common checkout is `swan-lake-common` beside the consumer **Git
root**, even when called from a subdirectory. Worktrees usually need `--common`.
Normal mode uses `git+file:` (Git-tracked files); `--dirty` uses `path:` (including
untracked files). Both can include modified tracked files. Neither mode changes
consumer flake files. Nix flags include `--no-write-lock-file`. Root inputs that
follow `common/*` follow the override too. `--input common-next` overrides the
experiment input instead of `common`. The wrapper replaces itself with the
command, preserving cwd, argument boundaries, exit status and signals.

Supported command forms are exactly those above (not arbitrary `nix run`, sudo
wrappers or shell command strings). deploy-rs forwards flags after its `--`;
nixos-rebuild requires an explicit `--flake`. Deployment/activation still has
its normal effects: the helper does not make those commands safe dry runs.

## Paired MRs and CI

1. Publish a common feature branch/MR. In the consumer MR use
   `git+https://forgejo.lynx-lizard.ts.net/binarin/swan-lake-common?ref=BRANCH`.
2. Run `nix flake update common`, commit the pin and test the consumer. Refresh
   the consumer lock whenever the common feature head changes.
3. Common fast-forward merges into master. **The human** promotes it to
   `swan-lake-current`; wait for the push mirror.
4. Finalize the consumer as below, commit, push and rerun required CI.

The independent `common-pin-policy` job fails with `temporary pin present`;
build jobs do not depend on it. Administrators must require the actual emitted
`common-pin-policy` status on both protected consumer master branches. A workflow
file alone is **not** a merge barrier. Check the status name in a live MR before
configuring it, and disallow agent merge/protected-push bypass. Protection
settings require administrative visibility; unavailable evidence is not success.

Consumers also include `.pre-commit-config.yaml`. Run `pre-commit install` in
the consumer shell to opt in. Do **not** use `--overwrite`: pre-commit's migration
mode preserves an existing hook. Inspect any existing custom hook/hooksPath and
combine it deliberately rather than deleting it. The hook checks every commit;
use `SKIP=common-pin-policy git commit ...` deliberately for temporary paired
pins. CI policy cannot be skipped that way.

## Experiments: `common-next`

`common-next` is an optional second common input of a consumer flake. It may
point at any ref or revision of the swan-lake-common repository, typically an
unmerged Forgejo MR branch:

```nix
common-next.url = "git+https://forgejo.lynx-lizard.ts.net/binarin/swan-lake-common?ref=BRANCH";
```

Each consumer has at most one `common-next`, so it runs one experiment at a
time. Machines opt in explicitly by building from `inputs.common-next` instead of
`inputs.common`; which machines opt in is not policed. When no experiment is
running, `common-next` declares the canonical
`github:binarin/swan-lake-common/swan-lake-current`, and opted-in machines act as
a canary of current.

Unlike a temporary paired pin, `common-next` may stay on consumer master. Its
policy (`check-common-pin`):

- `common` must still be exactly canonical.
- `common-next`, if declared, must be a plain `url` (no `follows` or other
  attributes) that names the swan-lake-common repository:
  `github:binarin/swan-lake-common[/REF-OR-REV]` (optionally `?ref=`/`?rev=`), or
  `git+https://` on `forgejo.lynx-lizard.ts.net/binarin/swan-lake-common` or
  `github.com/binarin/swan-lake-common`, with or without `.git`, optionally
  `?ref=`/`?rev=`. Other hosts, owners, repositories, schemes (`git+ssh`,
  `git+file`, `path`, tarballs, registry ids) and options (`dir`, `shallow`,
  `submodules`, `host`, ...) are rejected. The declaration is parsed by Nix and
  must equal the lock's `original`; the locked source must be the same
  repository with a full revision and SHA256 NAR hash.
- Whether `common-next`'s locked revision is based on (equal to or a descendant
  of) the locked `common` revision is reported on stderr, never enforced:
  `check-common-pin: common-next REV is based on common REV`, `... is NOT based
  on common ...`, or `cannot determine whether ...: REASON` when the history
  cannot be fetched (offline, unknown revision). The check fetches only the
  commit history of both revisions from `common-next`'s repository.

Root inputs may follow `common-next/...`; they are refreshed together with it.
To end an experiment after the common MR is merged and promoted, finalize it
like a temporary pin with `--input common-next` (below). Abandoning an
experiment is the same rewrite without merging:
`finalize-common --input common-next --allow-newer` locks the currently promoted
revision instead of waiting for the tested one.

## Finalization

Commit flake.nix/flake.lock first; unrelated dirty files may remain. Then:

```sh
finalize-common --root /absolute/consumer/root --timeout 300
# Only if the human promoted a later revision than the tested common head:
finalize-common --root /absolute/consumer/root --allow-newer
# Return the experiment input to canonical current after promotion:
finalize-common --root /absolute/consumer/root --input common-next
```

The same commands are available as flake apps in consumers that import
common's `flakeModules.default`, e.g.
`nix run .#finalize-common -- --root "$PWD" --input common-next`.

`--input common` (the default) requires the temporary Forgejo branch URL;
`--input common-next` accepts any non-canonical `common-next` declaration that
passes the policy above. In both cases the tested revision is the input's
locked revision, and the input is rewritten to the canonical URL.

The default polls GitHub for the tested revision; `--allow-newer` explicitly
accepts the currently promoted revision. The selected revision is frozen before
locking so subsequent ref movement cannot change the target. Original input
files must be clean and unchanged during the operation. A disposable copy is
locked first; unexpected unrelated lock churn (any node outside the finalized
input's dependency closure, or any change to the resolved graph of the other
common input, including dependencies the two share) or same-revision
dependency/hash changes abort before replacement. No network/write failure before replacement
changes originals. Replacement prepares both files first and rolls back a
partial write on caught failure/interruption. It is **not** a crash-atomic
multi-file transaction: do not run concurrent writers, and inspect files after
a power loss/SIGKILL or a rollback filesystem failure.

`source-equivalent finalization` means common rev, NAR hash and resolved
input graph match; it does not mean the entire consumer source identity is
unchanged. `changed revision; new builds required` is never called a no-op.
Always inspect the full diff and rerun required policy/build CI. The helper
never bypasses required statuses. A later canonical lock remains valid after
another promotion; the guard does not require today's promoted head.

Routine upgrades use `swl bump` (see `tools/swl/README.md`): it moves common's
inputs forward, promotes `swan-lake-current`, and opens a bump MR moving `common`
in each consumer. nixos-config's existing scheduled full update already covers
common.

## Validation

`nix flake check` builds the package and runs stdlib fixture tests. Run
`nix develop -c python3 tools/common-workflow/integration.py` for disposable,
real-Nix override and declaration tests (including `common-next` parsing,
policy and `with-common --input common-next`) without network or activation.

## Consumer flake apps

Common's `flakeModules.default` adds `apps.check-common-pin` and
`apps.finalize-common` to the importing consumer. Their attributes are module
defaults, so a consumer that defines an app of the same name keeps its own.
`with-common` is a devshell command and is not exported as a consumer app.

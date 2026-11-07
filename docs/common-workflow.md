# Common upgrades across repositories

## Overview

Common stages changes on Forgejo master; the human promotes a master revision
by advancing `swan-lake-current`. Forgejo push-mirrors to GitHub. Consumers
commit `github:binarin/swan-lake-common/swan-lake-current` and a revision lock.
A paired consumer MR may temporarily reference the common Forgejo feature
branch, but must restore the canonical declaration before merge.

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
```

The default common checkout is `swan-lake-common` beside the consumer **Git
root**, even when called from a subdirectory. Worktrees usually need `--common`.
Normal mode uses `git+file:` (Git-tracked files); `--dirty` uses `path:` (including
untracked files). Both can include modified tracked files. Neither mode changes
consumer flake files. Nix flags include `--no-write-lock-file`. Root inputs that
follow `common/*` follow the override too. The wrapper replaces itself with the
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

## Finalization

Commit flake.nix/flake.lock first; unrelated dirty files may remain. Then:

```sh
finalize-common --root /absolute/consumer/root --timeout 300
# Only if the human promoted a later revision than the tested common head:
finalize-common --root /absolute/consumer/root --allow-newer
```

The default polls GitHub for the tested revision; `--allow-newer` explicitly
accepts the currently promoted revision. The selected revision is frozen before
locking so subsequent ref movement cannot change the target. Original input
files must be clean and unchanged during the operation. A disposable copy is
locked first; unexpected unrelated lock churn or same-revision dependency/hash
changes abort before replacement. No network/write failure before replacement
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

Routine upgrades use `nix flake update common`. nixos-config's existing scheduled
full update already covers common; swan-lake uses manual updates for now.

## Validation

`nix flake check` builds the package and runs stdlib fixture tests. Run
`nix develop -c python3 tools/common-workflow/integration.py` for disposable,
real-Nix override and declaration tests without network or activation.

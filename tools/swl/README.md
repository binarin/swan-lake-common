# swl

Cross-repo workspaces for Swan Lake (SL-0036), version 0. `swl` creates
checkouts side by side, points the consumers' flake inputs at them with
`--override-input … git+file://…`, and wraps `nix`, `nh` and `deploy` so that
builds and deploys from the dev shell use the same overrides. It never writes a
`flake.lock`.

Manifests, `plan`, `land`, the CI lock check and MR state are not part of v0.

## Workspace layout

```
<ws>/
  swan-lake/                 consumer checkout: a flake whose directory name has no @
  nixos-config/              another consumer
  swan-lake-common@common/   shared checkout <repo>@<input>
  zwanenzang@zwanenzang/     <input> is the input name in the flake that consumes it directly
  .swl/bin/{nix,nh,deploy}   generated wrappers
```

Every direct subdirectory with a `flake.nix` is a checkout; dot directories are
skipped. Each input name can be overridden by only one checkout per workspace.

## Commands

```sh
swl ws add swan-lake                      # worktree of the consumer, detached at the base
swl ws add swan-lake-common@common --branch sl-0036-x
swl env                                   # .envrc.local per consumer, and the wrappers
swl status                                # local state of the shared checkouts
swl shim nix --workspace <ws> -- ARGS…    # what the wrappers run
```

`ws add`, `env` and `status` use the current directory as the workspace unless
`--workspace DIR` is given. The global `--config FILE` selects the repos config.

- `ws add NAME [--branch B]` runs `git worktree add` in the repo's configured
  source, which can be a bare repository or a main checkout. Without
  `--branch`, the worktree is detached at the repo's `base`. With `--branch`,
  an existing branch is checked out, or a new branch is created at `base`
  without an upstream. `ws add` does not fetch: fetch the source first.
- `env` writes each consumer's `.envrc.local` and `<ws>/.swl/bin/{nix,nh,deploy}`.
  It writes only files whose content changes, so that direnv does not reload
  needlessly. It refuses to replace a `.envrc.local` that it did not write
  (without the generated marker line) unless `--force` is given. It prints the
  overrides per consumer, and warns when a consumer's `.envrc` does not source
  `.envrc.local` into `use_flake_args`. Rerun it after adding or removing
  checkouts, or after a checkout's `flake.lock` changes.
- `status` prints, for each shared checkout, its input, HEAD, branch, and
  whether it is dirty (tracked changes only, as `git+file:` sees them). It then
  prints one line for each flake in the workspace that it overrides: the input
  path, the rev that flake's own `flake.lock` locks, and ahead/behind of HEAD
  against that rev. It uses local git only.

## Config

The repos config is `--config FILE`, else `$SWL_CONFIG`, else
`$XDG_CONFIG_HOME/swl/repos.toml` (`~/.config/swl/repos.toml`). A missing file
at the default location means an empty config: the shim and `env` still work,
but `ws add` needs the repos listed.

```toml
# Optional. Flake URLs that name the repo {repo}; these are the defaults.
url_patterns = [
  "github:binarin/{repo}",
  "git+https://forgejo.lynx-lizard.ts.net/binarin/{repo}",
  "git+ssh://forgejo.lynx-lizard.ts.net/binarin/{repo}",
]

[repos.swan-lake-common]
source = "/home/binarin/personal-workspace/bare-repos/swan-lake-common.git"
base = "origin/master"        # optional; the default

[repos.zwanenzang]
source = "/home/binarin/personal-workspace/swan-lake-umbrella/zwanenzang"
urls = ["git+https://example.org/mirror/zwanenzang"]   # optional extra forms
```

URLs are compared after lower-casing and after dropping the query, a trailing
`/` or `.git`, and the user in `scheme://user@host`. So, for example,
`git+ssh://forgejo@forgejo.lynx-lizard.ts.net/binarin/zwanenzang.git` matches
the default ssh pattern.

## Overrides: hop derivation

To find the overrides for a flake in the workspace, swl reads the flake's
`flake.lock`. A root input gets an override when:
- it is not a `follows`;
- it is named like a shared checkout's `<input>`;
- it is locked (`original` or `locked`) to that checkout's repo.

swl then continues in the checkout's own `flake.lock`, which Nix uses below an
override. Its inputs get the prefix `<input>/`. For example, `zwanenzang@zwanenzang`
becomes `common/zwanenzang` for swan-lake. Root inputs that follow an
overridden input, such as swan-lake's `zwanenzang` (follows
`common/zwanenzang`), are covered by the follows. So are inputs made to
follow by the outer flake. The walk goes only through checkouts in the
workspace: to work on zwanenzang for a consumer, check out common too. Other
inputs of the same repo, such as `common-next`, are left alone unless a
`swan-lake-common@common-next` checkout exists.

## Generated files

The consumer's `.envrc.local` follows swan-lake's `.envrc` contract: it is
sourced before `use flake`, appends to the bash array `use_flake_args`, and
may call `watch_file` and `PATH_add`.

```sh
# Generated by `swl env` for the workspace /ws; rerun it rather than editing.
use_flake_args+=(
  --override-input common git+file:///ws/swan-lake-common@common
  --override-input common/zwanenzang git+file:///ws/zwanenzang@zwanenzang
  --no-write-lock-file
)
shopt -s globstar
watch_file /ws/swan-lake-common@common/flake.nix /ws/swan-lake-common@common/flake.lock /ws/swan-lake-common@common/**/*.nix
watch_file /ws/zwanenzang@zwanenzang/flake.nix /ws/zwanenzang@zwanenzang/flake.lock /ws/zwanenzang@zwanenzang/**/*.nix
PATH_add /ws/.swl/bin
export SWAN_LAKE_OVERRIDES='common←swan-lake-common@common common/zwanenzang←zwanenzang@zwanenzang'
```

Each wrapper is one `exec` of this `swl` by its absolute path, so the
consumers' dev shells need no change:

```sh
#!/bin/sh
# Generated by `swl env`; rerun it rather than editing.
exec /nix/store/…-swl-0.1.0/bin/swl shim nix --workspace /ws -- "$@"
```

## Shim rules

The shim finds the real tool on `PATH`. It skips the workspace's `.swl/bin`
and any other file that carries the generated marker. It prints nothing on
stderr except when it refuses a call. Set `SWL_SHIM_LOG=FILE` to append one
line per call: the tool, the action and the arguments.

`nix`:
- Subcommands that take installables and lock flags get the overrides,
  inserted right after the subcommand words:
  - `build`, `eval`, `develop`, `print-dev-env`, `run`, `shell`, `path-info`,
    `log`, `why-depends`, `copy`, `repl`, `bundle`, `search`, `edit`;
  - `env shell` and `derivation show`;
  - `flake check|show|archive|metadata|info|prefetch-inputs`.

  Everything else passes through unchanged.
- Calls whose installables are all store paths pass through untouched. This
  covers `/nix/store/…`, `…^out`, and `result` links into the store. So do
  `--expr` and `--file` calls.
- Overrides apply only when an installable is a local flake reference (`.`,
  a path, `path:` or `git+file:`) whose flake directory is a checkout of the
  workspace. With no installable, the flake is the one at or above the
  current directory. If several installables name workspace checkouts, the
  overrides are joined.
- With overrides, `--no-write-lock-file` is added. Overrides the caller
  already passes are not repeated, so nix-direnv's own calls stay as they
  are.
- While overrides apply, the shim refuses `nix flake lock`, `nix flake update`,
  `--commit-lock-file`, `--recreate-lock-file`, `--update-input` and
  `--output-lock-file`. Outside the workspace these pass through.

`nh` passes through, except that nh 4.4.2's lock-update flags are refused:
`-u/--update`, `-U/--update-input`, `--recreate-lock-file` and
`--commit-lock-file`. nh's own nix calls go through the `nix` wrapper.

`deploy` passes through. Its nix calls go through the `nix` wrapper. A policy
for deploying overrides to a real host is for later.

## Limits

- Only checkouts listed directly in the workspace count. An input that is in
  `flake.nix` but not yet in the lock cannot be derived.
- `git+file:` sees tracked files only. Use `git add -N` for new files.
- Only `.nix` files and the lock of overridden checkouts are watched. After
  changing other sources (for example a Rust tool in common), run
  `nix-direnv-reload`.
- The wrappers name the `swl` store path that ran `swl env`. Rerun `swl env`
  after updating swl, and keep that path alive against garbage collection,
  for example by having swl in a profile.
- The nix option table is finite. The value of an unknown option can be taken
  for an installable, so the shim may then add overrides to a store-path call
  (harmless) or miss the default `.`. Before the subcommand, only words that
  are nix subcommands are taken as one.
- `nix profile`, `nix fmt` and the legacy `nix-*` commands pass through.
- nh's lock-flag refusal applies to any flake, not only to workspace ones.
- Not verified: `nh os switch/boot/test` activation, `--build-host`,
  `--target-host`, and deploy-rs `--remote-build`.

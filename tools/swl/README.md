# swl

Cross-repo workspaces for Swan Lake. `swl` creates checkouts side by side,
points the consumers' flake inputs at them with
`--override-input … git+file://…`, and wraps `nix`, `nh` and `deploy` so that
builds and deploys from the dev shell use the same overrides. In a workspace
it never writes a `flake.lock`.

A consumer branch that depends on unmerged shared-repo changes names their
MRs in a manifest, `swl.toml`. `swl publish` pushes the work and writes the
manifest, CI builds against the MRs' heads (`swl ci env`) and gates MRs and
master (`swl check`), and `swl finalize` turns landed entries into a lock
bump. `plan` and `land` (the lander) are not part of swl yet.

## Workspace layout

```
<ws>/
  swan-lake/                 consumer checkout: a flake whose directory name has no @
  nixos-config/              another consumer
  swan-lake-common@common/   shared checkout <repo>@<input>
  zwanenzang@zwanenzang/     <input> is the input name in the flake that consumes it directly
  .swl/bin/{nix,nh,deploy}   generated wrappers
  .swl/gcroots/swl           GC root of the swl the wrappers exec
```

Every direct subdirectory with a `flake.nix` is a checkout; dot directories are
skipped. Each input name can be overridden by only one checkout per workspace.

## Commands

```sh
swl start SL-0060-foo swan-lake swan-lake-common@common   # a workspace for one change
swl start --from swan-lake#12             # join a published consumer MR
swl ws add swan-lake                      # fetch, worktree detached at the base, then env
swl ws add swan-lake-common@common --branch sl-0036-x
swl env                                   # .envrc.local per consumer, and the wrappers
swl status                                # local state of the shared checkouts
swl shim nix --workspace <ws> -- ARGS…    # what the wrappers run
swl publish swan-lake                     # push, open MRs, write swl.toml
swl check                                 # CI: the MR or master gate
swl ci env                                # CI: nix wrapper with the MR overrides
swl finalize [--wait]                     # landed entries -> one lock commit
```

`ws add`, `env` and `status` use the current directory as the workspace unless
`--workspace DIR` is given; `start` creates one below it. The global
`--config FILE` selects the repos config.

- `start NAME CHECKOUT... [--workspace DIR] [--branch B] [--no-fetch]` sets up
  the workspace for one change: `DIR` (default `./NAME`) with each checkout
  (at least one consumer, plus `<repo>@<input>` shared checkouts) on branch
  `B` (default `NAME` lower-cased: workspace `SL-0060-foo`, branches
  `sl-0060-foo`), added as `ws add --branch B` does, fetching each source
  once. Then `env` once, and `direnv allow` for each consumer with an
  `.envrc` when `direnv` is on PATH (else a note to run it). It ends with the
  checkouts and a `cd` hint. Rerunnable: an existing checkout directory is
  kept, and reported when it is on another branch (never changed); missing
  ones are added. An interrupted `start` is finished by rerunning it.
- `start --from CONSUMER#MR [--workspace DIR] [--no-fetch]` joins a
  published change, for example on another machine. With git (consumer repos
  are private), it reads `refs/pull/MR/head` from the consumer source's
  remote and the one branch at that head; with none (the MR was merged and
  its branch deleted, or the branch moved on) or several, it refuses. It
  fetches that head and reads its `swl.toml`. The workspace is `DIR`
  (default `./<consumer branch>`): the consumer on its branch, tracking the
  remote's, and for each entry `[inputs.<path>] repo, mr` the checkout
  `<repo>@<last segment of path>` (`common/zwanenzang` gives
  `zwanenzang@zwanenzang`): on an open MR's head branch, or detached at a
  merged MR's merge commit. A closed or unknown MR is an error, before
  anything is added. Then `env` and `direnv allow` as above; rerunnable
  likewise.

- `ws add NAME [--branch B] [--no-fetch] [--no-env]` runs `git worktree add`
  in the repo's configured source, which can be a bare repository or a main
  checkout.
  - First it fetches the source's remote that the `base` names (`origin` for
    `origin/master`), so that the new worktree starts at the current base. A
    base without a remote prefix is not fetched. `--no-fetch` skips the fetch
    for offline work; a failed fetch is an error, not a stale base.
  - Without `--branch`, the worktree is detached at `base`. With `--branch B`:
    an existing local B is checked out, fast-forwarded first when it is
    strictly behind `<remote>/B` (a diverged one is left with a warning;
    swl never resets); else B is created at `<remote>/B` with that upstream,
    to join a published branch; else B is created at `base` without an
    upstream.
  - Then it runs `env` for the workspace, unless `--no-env`, or while the
    workspace has no consumer checkout yet. When `env` refuses (a
    hand-written `.envrc.local`), the checkout is still added; fix it and
    rerun `swl env` (`--force` replaces the file).
- `env` writes each consumer's `.envrc.local` and `<ws>/.swl/bin/{nix,nh,deploy}`,
  and roots the swl they exec (see [Generated files](#generated-files)).
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

`publish`, `check`, `ci env` and `finalize` are described under
[Manifest and landing](#manifest-and-landing).

## Config

The repos config is `--config FILE`, else `$SWL_CONFIG`, else
`$XDG_CONFIG_HOME/swl/repos.toml` (`~/.config/swl/repos.toml`). A missing file
at the default location means an empty config: the shim and `env` still work,
but `ws add` needs the repos listed. A file named by `--config` or
`$SWL_CONFIG` must exist. When `env` runs with one of them, the wrappers pass
it on as `--config <absolute path>`, so the shim uses the same config as the
`.envrc.local` files.

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
consumers' dev shells need no change (with `--config FILE` before `shim` when
the config was chosen explicitly):

```sh
#!/bin/sh
# Generated by `swl env`; rerun it rather than editing.
exec /nix/store/…-swl-0.1.0/bin/swl shim nix --workspace /ws -- "$@"
```

When that `swl` is in the Nix store (`$NIX_STORE_DIR`, default
`/nix/store`), `env` makes `<ws>/.swl/gcroots/swl` an indirect GC root for
its store path, with the real `nix` (`nix build --out-link`), so that a
garbage collection does not remove what the wrappers exec. It skips the call
when the link already points there, and does nothing for a swl outside the
store (a cargo build). Deleting the workspace drops the root. A failure is a
warning: the files are written anyway.

## Shim rules

The shim finds the real tool on `PATH`. It skips the workspace's `.swl/bin`
and any other file that carries the generated marker. It prints nothing on
stderr except when it refuses a call (exit 1) or fails (`swl: …`, exit 2). Set `SWL_SHIM_LOG=FILE` to append one
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
  covers `/nix/store/…`, `…^out`, and `./result` links into the store. So do
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
  `--output-lock-file`. Outside the workspace these pass through. Options are
  read up to `--`, also after the installable of `nix run`, as Nix does;
  only the positional words after it belong to the program.

`nh` passes through, except that nh 4.4.2's lock-update flags are refused:
`-u/--update`, `-U/--update-input`, `--recreate-lock-file` and
`--commit-lock-file`. nh's own nix calls go through the `nix` wrapper.

`deploy` passes through. Its nix calls go through the `nix` wrapper. A policy
for deploying overrides to a real host is for later.

## Manifest and landing

### `swl.toml`

A repo's branch lists the shared-repo MRs it depends on in `swl.toml` at its
root, one table per input path:

```toml
[inputs.common]
repo = "swan-lake-common"
mr = 24

[inputs."common/zwanenzang"]   # nested input paths are quoted
repo = "zwanenzang"
mr = 7
```

- An entry has exactly `repo` (the shared repo's name on the forge, owner
  `binarin`) and `mr` (its pull request number). Unknown keys, a missing key,
  a non-positive `mr` or a malformed input path are errors.
- No branch names: an entry follows the MR, so it survives rebases and the
  deletion of the branch on merge.
- An absent file means nothing is pending. Master never has one.
- `publish` and `finalize` rewrite the file in a fixed form: a header
  comment, then the tables sorted by input path. Hand-added comments are
  not kept.
- The format and code are not specific to consumers; a shared repo such as
  common can have a manifest for its own inputs (zwanenzang, the forks).

Rules shared by all commands: an MR must not change `flake.lock` (CI
enforces it); lock changes reach master only through `swl finalize` (for
manifest entries) or by hand (routine bumps). A branch with entries is
landed, not merged through the forge.

### `swl publish [--workspace DIR] <consumer>`

From a workspace, for the consumer checkout `<consumer>`:

1. For each shared checkout that overrides one of the consumer's inputs and
   whose HEAD differs from the rev the consumer locks: push its branch to
   `origin`, fast-forward only. A HEAD already in the repo's base (config
   `base`, default `origin/master`, fetched first) is a lock bump or merged
   work, not an MR, and is skipped.
2. Find the open MR whose head is that branch (`fj pr search --all` and
   `fj pr view`), or open one as a draft (`fj pr create`, title `WIP: ` and
   the first commit's subject, body the commit list), into the base branch.
3. Write `mr = N` entries for its input paths into the consumer's
   `swl.toml` and commit only that file on the consumer branch
   (`swl: manifest for swan-lake-common#24`). With nothing published, an
   existing `swl.toml` is left as it is.
4. Push the consumer branch and find or open its MR (not a draft; the body
   lists the dependencies).

Everything is checked before anything is pushed. Refused: a detached HEAD,
the base branch itself (publish from a topic branch), a consumer branch that
changes `flake.lock`, uncommitted changes to tracked files, and a branch on
`origin` that HEAD does not contain (swl never force-pushes; fetch and rebase or merge). So a
manifest never names unpushed commits. Rerunning finds the existing MRs,
leaves a current manifest alone and pushes only new commits. Entries are
added or updated, never removed by `publish`.

Forge access is only through `fj`, which must be on PATH and logged in
(`fj auth`); swl has no token configuration. swl parses fj 0.7's English
output, and passes `-H forgejo.lynx-lizard.ts.net`. `publish` fetches each
checkout's base branch (an `origin/<branch>` base) and reads the remote
branch state with `ls-remote`; it fetches nothing else. A squash-merged MR is
not recognised as merged: its commits are not in the base.

### `swl check [DIR] [--event mr|master] [--base REV]`

The CI gate, in the repo at or above `DIR` (default: the current directory).
Exit status 1 with one line per problem.

- `--event` defaults from the Forgejo Actions environment:
  `GITHUB_EVENT_NAME=pull_request` is `mr`; `push` with
  `GITHUB_REF=refs/heads/master` is `master`; anything else needs `--event`.
- On an MR:
  - `flake.lock` at HEAD equals the one at the merge base of HEAD and the
    base: `--base REV`, else `origin/$GITHUB_BASE_REF`, which `check`
    fetches (`--unshallow` in a shallow checkout; `fetch-depth: 0` is
    better).
  - `swl.toml`, if present, parses, and for each entry: the input is that
    repo (its lock node's URL by the config's URL patterns, or for a root
    input not yet in the lock, its `flake.nix` URL), and the MR exists and is
    open or merged (anonymous API).
- On master:
  - there is no `swl.toml`;
  - the common pin policy, ported from `check-common-pin`: `common` is
    exactly `github:binarin/swan-lake-common/swan-lake-current` in
    `flake.nix` and canonical in the lock (full rev, SHA256 NAR hash, no extra
    source options); every `common-next` or `common-next-<label>` input is a
    plain URL at any ref or rev of swan-lake-common (GitHub or Forgejo),
    matching its lock node. For each, an informational line says whether it
    is based on the locked `common`; offline that is "cannot determine", not
    a failure. A flake with neither `common` nor an experiment input skips
    the policy.

`check` reads `flake.nix` with `nix eval --impure` (no input is fetched) and
uses the real `nix` on PATH, never a swl wrapper.

### `swl ci env [DIR]`

For a CI job, in the repo it checked out. For each manifest entry it asks
the anonymous API for the MR's state:

- open: override the input with the MR's head;
- merged: the same, and a note that the entry can land;
- closed without merging, no such MR (a 404, which a private repo gives
  too), or the API unreachable: exit 1, and nothing is written.

It then writes a `nix` wrapper into a new directory under `$RUNNER_TEMP`
(else the temp directory) and appends that directory to `$GITHUB_PATH`, so
the job's later steps call it, whatever tool calls nix. Without
`$GITHUB_PATH` it prints the `export PATH=…` line to use. The wrapper adds

```
--override-input common 'git+https://forgejo.lynx-lizard.ts.net/binarin/swan-lake-common?ref=refs/pull/24/head&rev=<head sha>'
```

pinned to the head the API reported, so the whole job builds one revision. It
applies the shim's rules: only to the checked-out flake (`.`, a path, or
`git+file:` of it), store-path calls untouched, `--no-write-lock-file`
added, and the lock-writing commands and flags refused. Run `swl check`
before `swl ci env`, or in another job: after it, `nix run .#swl` would
evaluate the MR's common.

A consumer's CI, sketched (the switch-over is a separate MR per consumer):

```yaml
jobs:
  swl-check:
    runs-on: native
    steps:
      - uses: actions/checkout@v4
        with:
          ref: ${{ github.event.pull_request.head.sha || github.sha }}
          fetch-depth: 0
      - run: nix run .#swl -- check
  flake-check:
    runs-on: native
    steps:
      - uses: actions/checkout@v4
        with:
          ref: ${{ github.event.pull_request.head.sha || github.sha }}
      - run: nix run .#swl -- ci env
      - run: nix flake check
```

`nix run .#swl` takes swl from the consumer's locked common, through
common's `flakeModules.default` (or `flakeModules.swl` alone). A common MR
that changes swl is therefore tested with the previous swl.

### `swl finalize [DIR] [--wait [--timeout S] [--interval S]]`

On the consumer's MR branch, after shared MRs have merged:

1. For each entry, the MR state (anonymous API). Open, closed or unknown
   entries stay.
2. For merged entries, relock their root inputs with the real `nix flake
   update <inputs> --refresh`, into a scratch lock. What the input's
   `flake.nix` URL resolves to decides: for `common`, `swan-lake-current` on
   GitHub, so the change must be promoted and mirrored. An entry is satisfied
   when its MR's merge commit is in the history of the rev now locked at its
   input path (asked of that source by a history-only fetch). A nested entry
   (`common/zwanenzang`) is satisfied through its parent's relock.
3. Only root inputs with a satisfied entry are relocked. The new lock may
   differ from the old one only below those inputs: other nodes and root
   edges, other inputs' graphs (shared nodes included) and new nodes are
   checked, as finalize-common did, and the result must pass the pin policy.
4. One commit on the current branch, of `flake.lock` and `swl.toml` only
   (satisfied entries removed, the file deleted when empty):
   `swl: land swan-lake-common#24 (common)`, with the old and new revs in the
   body. Nothing is pushed.

It refuses a detached HEAD and uncommitted changes to `flake.nix`,
`flake.lock` or `swl.toml`. When nothing is satisfied it says why per entry,
commits nothing and exits 1; so does a `swl.toml` without entries, which
should be removed. `--wait` polls (default every 10 s, for up to
300 s) while merged entries are not yet reachable, then finalizes what is.

The landing procedure, after `finalize`:

```sh
git push origin HEAD:master          # fast-forward only: the MR must be based on current master
git push origin --delete <branch>    # Forgejo marks the MR merged; stacked MRs restack
```

Master's CI then runs `swl check` on the result. When entries remain, the
commit (which changes `flake.lock`) must stay local: pushed to the MR branch
it fails the MR's `swl check`. Rerun `finalize` once the rest has landed,
then push to master; `swl publish` refuses to push it.

### Forge access

`start --from` reads MR state with the login of `fj`: the token for
`forgejo.lynx-lizard.ts.net` in fj's `keys.json`
(`$XDG_DATA_HOME/forgejo-cli/keys.json`, default
`~/.local/share/forgejo-cli/keys.json`; `SWL_FJ_KEYS` overrides the path).
That covers private repos. It only reads: the token goes to curl on stdin,
never on a command line or into the output, and only to the forge (or a
loopback test server). Without a login it reads anonymously. swl has no
token configuration of its own.

`check`, `ci env` and `finalize` read MR state anonymously
(`GET /api/v1/repos/binarin/<repo>/pulls/<n>`, through curl) and CI fetches
`refs/pull/<n>/head` anonymously. So every repo named in a manifest must be
public. Should one become private, give CI a dedicated read-only token as a
secret, through nix `access-tokens` or a netrc for the fetch, and the same for
the API; Forgejo's automatic Actions token is scoped to the job's own repo
and is not enough. `publish` uses `fj` and its login.

`SWL_FORGE_API` (default `https://forgejo.lynx-lizard.ts.net/api/v1`) and
`SWL_FORGE_GIT` (default `https://forgejo.lynx-lizard.ts.net`, the base of
the override URLs) redirect them, for tests.

## Limits

- Only checkouts listed directly in the workspace count. An input that is in
  `flake.nix` but not yet in the lock cannot be derived by the workspace
  shim; CI gets it from the manifest.
- `git+file:` sees tracked files only. Use `git add -N` for new files.
- Only `.nix` files and the lock of overridden checkouts are watched. After
  changing other sources (for example a Rust tool in common), run
  `nix-direnv-reload`.
- The wrappers name the `swl` store path that ran `swl env`, which `env`
  keeps alive with the workspace's GC root. Rerun `swl env` after updating
  swl; until then the wrappers run the old one. A swl run from outside the
  store (a cargo build) is not rooted.
- The nix option table is finite. The value of an unknown option can be taken
  for an installable, so the shim may then add overrides to a store-path call
  (harmless) or miss the default `.`. Before the subcommand, only words that
  are nix subcommands are taken as one.
- `nix profile`, `nix fmt` and the legacy `nix-*` commands pass through.
- nh's lock-flag refusal applies to any flake, not only to workspace ones.
- Not verified: `nh os switch/boot/test` activation, `--build-host`,
  `--target-host`, and deploy-rs `--remote-build`.
- `publish` parses fj's text output; a different fj version or output
  language can break it. It opens MRs against the base branch only (no
  stacked MRs).
- `finalize`'s history fetch is cheap for swan-lake-common, but would fetch
  the whole commit history of a large fork such as nixpkgs.
- `finalize` relocks a root input as a whole: nested entries land when the
  parent (common) has bumped and promoted them.

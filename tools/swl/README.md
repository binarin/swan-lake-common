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
bump. `swl start` sets up the workspace for a change, `swl status` shows
where it stands, and `swl land` takes it to master. `plan` is not part of
swl yet.

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
swl status [--from swan-lake#12]          # where the change stands, and the next step
swl shim nix --workspace <ws> -- ARGS…    # what the wrappers run
swl publish swan-lake                     # push, open MRs, write swl.toml
swl check                                 # CI: the MR or master gate
swl ci env                                # CI: nix wrapper with the MR overrides
swl finalize [--wait]                     # landed entries -> one lock commit
swl land [--dry-run] [--keep] [--from swan-lake#12]   # merge, promote, finalize, merge, clean up
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
  ones are added. An interrupted `start` is finished by rerunning it. An
  existing directory that is not a worktree of the repo's configured source
  is refused.
- `start --from CONSUMER#MR [--workspace DIR] [--no-fetch]` joins a
  published change, for example on another machine. With git (consumer repos
  are private), it reads `refs/pull/MR/head` from the consumer source's
  remote and the one branch at that head, other than the base branch; with
  none (the MR was merged, its branch deleted, or the branch moved on) or
  several, it refuses. It
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
- `status [--no-fetch]` shows where the workspace's change stands; see
  [Status](#status).

`publish`, `check`, `ci env`, `finalize` and `land` are described under
[Manifest and landing](#manifest-and-landing).

## Status

`swl status [--workspace DIR] [--no-fetch]` for a workspace, and
`swl status --from CONSUMER#MR [--no-fetch]` for a published change without
one. It prints one row per shared checkout, per `swl.toml` entry that no
checkout follows, and per consumer:

```
swan-lake-common@common  CI green  next: merge swan-lake-common#24  waits: owner
  input common  HEAD 72f1eec  sl-0060-foo  clean
  swan-lake: common locked 9477137, ahead 2, behind 0
  swan-lake-common#24: open, sl-0060-foo at 72f1eec824d6, CI green
swan-lake  CI green  next: wait for swan-lake-common#24 (merge swan-lake-common#24)  waits: owner
  HEAD 1c0ffee  sl-0060-foo  clean
  swl.toml: common swan-lake-common#24
  swan-lake#31: open, sl-0060-foo at 1c0ffee00000, CI green
```

A row is the checkout or MR, its stage, the next step and who it waits for:
`owner` (merging, promoting, requesting a merge), `anyone` (whoever works on
the change), or `nobody` (CI, a mirror, or nothing to do). The detail lines
below it are the local facts:
- `input <input>  HEAD <rev>  <branch>  clean|dirty` for a shared checkout,
  and for a consumer the same without the input (the consumer line). Dirty
  counts tracked changes only, as `git+file:` sees them.
- Under a shared checkout, one line per flake in the workspace that it
  overrides: the input path, the rev that flake's own `flake.lock` locks
  there, and HEAD ahead/behind of that rev.
- Under a consumer, its `swl.toml` entries, and the overridden inputs that
  are changed without an entry.
- Where the remote branch is, when it is not at HEAD, and what the forge says
  about the MR: open (draft), its head and CI; or merged, whether the merge
  commit is in the branch the consumer's `flake.nix` declares, and whether the
  declared source (for common the GitHub mirror) has it.

The stages, in order:

| stage | shared checkout | consumer |
|---|---|---|
| `local` | its branch is not on the remote (or detached with commits not in the base) | likewise |
| `needs publish` | the remote branch is behind HEAD, or no consumer's `swl.toml` names an MR for it, or a consumer that overrides it has no entry, or the MR was closed | the branch has unpushed commits, or an overridden input's checkout differs from the lock without an entry, or its MR was closed |
| `pushed` | pushed, but the MR cannot be read | pushed, and no MR yet, or it cannot be read (also with the landing commit pushed) |
| `MR open`, `MR open (draft)` | the `swl.toml` entry's MR is open; a draft has a `WIP:` title or `draft` set | the MR whose `refs/pull/N/head` is the remote branch head (not looked up once that head is in the base) |
| `CI pending`, `CI failing`, `CI green` | the combined commit status of the MR head | likewise |
| `merged` | the MR is merged (its merge commit) | |
| `promoted` | the merge commit is in the declared branch on the forge (`swan-lake-current` for common; an input without a ref is promoted when merged) | |
| `landing pushed`, `landing pushed, CI failing` | | the MR is open, and its branch has no pending `swl.toml` entries, had some (its commits since the merge base touch `swl.toml`), and changes `flake.lock` |
| `landed` | merged, and every consumer that names it in `swl.toml` has landed | the MR is merged, or both the local and the remote branch heads are in the base |

A shared checkout without an MR whose HEAD is in the rev each consumer locks,
or already in its base, is `no MR needed`; a consumer whose HEAD is in its
base is `no change`. A consumer MR with entries is not merged on its own:
while its own step waits for the owner (merge) or nobody (CI), its next step
waits for the entries' rows, then is `swl land` (see [Landing](#landing)).
A draft or failing CI is its own step first.

The next steps are `swl publish <consumer>` (anyone), `mark <repo>#N ready`
(anyone), wait for CI (nobody), `merge <repo>#N` (owner),
`promote <branch>` (owner), `swl land` (anyone), `open the MR` (anyone) and
`request merge when checks succeed` (owner) once the landing is pushed
(`fix CI`, anyone, while it fails).

By default `status` fetches each checkout's remote (as `ws add` does, once per
source) so that the stages are current, and finds a consumer's MR with
`git ls-remote <remote> 'refs/pull/*/head'`; the mirror check fetches commit
history only. `--no-fetch` uses the refs already known and looks up no
consumer MR. A forge, network or git error makes that row
`unknown (<reason>)`; it is never an error of the command.

MR state and CI come from the forge API: anonymously, and for what that
cannot see with fj's login (see [Forge access](#forge-access)). A repo that
cannot be read (private, and no login) falls back to git: the branch on the
remote, and `landed` when its head is in the base; its MR shows as
`unknown (not readable anonymously, and no fj login: …)`, and a pushed
landing as `pushed`. A login the forge rejects makes the row
`unknown (fj login rejected: run fj auth login)`.

`--from CONSUMER#MR` reads the consumer MR as `start --from` does (with git),
then shows a row per `swl.toml` entry at its head and the consumer row. A
cross-repo list of all open changes is not part of swl.

## Diagnostics

`SWL_LOG` sets what swl logs on stderr, as a `tracing` filter: `debug`,
`swl::forge=trace`, `swl::cmd=debug,swl::land=debug`. The default is
`warn`. At debug: every external command (git, nix, curl, fj, direnv) with
its arguments, exit status and duration (target `swl::cmd`); each forge or
GitHub API request with its method, URL, whether it carried fj's token, and
the HTTP status (`swl::forge`; the answer's body at trace); and the
decisions of `status` (each row's stage, next step and waits) and `land`
(per pass, per MR and consumer). The token never appears: it goes to curl
on stdin, which is not logged, and arguments that look like credentials
(an `Authorization` header, a password in a URL, the value of an
`access-tokens` option) are scrubbed. The shims (`swl shim`, `swl ci
shim`) log nothing unless `SWL_LOG` is set; `SWL_SHIM_LOG` is separate.

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
stderr except when it refuses a call (exit 1) or fails (`swl: …`, exit 2), or
when `SWL_LOG` asks for diagnostics (see [Diagnostics](#diagnostics)). Set
`SWL_SHIM_LOG=FILE` to append one line per call: the tool, the action and the
arguments.

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

Rules shared by all commands: an MR may change `flake.lock` in two ways
only, which `swl check` enforces per root input: a landing (the relock of
manifest entries whose MRs merged and were promoted, made by `swl
finalize`, with those entries removed), or a forward move (a routine bump to
a descendant of the old rev on the declared branch). Anything else in the
lock is refused. A branch with entries lands once they are merged: its lock
commit comes from `swl finalize`.

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

`publish` pushes with git and opens MRs only through `fj`, which must be on
PATH and logged in (`fj auth`); swl has no token configuration (`status`
and `start` read fj's token, see [Forge access](#forge-access)). swl parses
fj 0.7's English
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
- On an MR, against the merge base of HEAD and the base: `--base REV`,
  else `origin/$GITHUB_BASE_REF`, which `check` fetches (`--unshallow` in a
  shallow checkout; `fetch-depth: 0` is better). Several merge bases
  (criss-cross merges) fail the check: rebase onto the base.
  - `flake.lock` at HEAD equals the one at the merge base, or changes only
    by the two accepted kinds of change. The removed entries are those named
    in `swl.toml` at the merge base or at any commit since, minus HEAD's
    entries. Each root input whose locked graph changed (its node, its
    dependencies, or for a `follows` input its path) is one of:
    - (i) a landing: it is the root of removed entries whose MRs are merged
      (anonymous API); each merge commit is in the rev now locked at its
      input path (a nested `common/zwanenzang` through common's new lock);
      and the input moved as in (ii), or, new in the lock, is the repo its
      entry names and is in its declared branch. A removed entry whose MR is
      open, closed or unknown was dropped, not landed: a note, and its root
      must be a forward move.
    - (ii) a forward move: the input keeps its source (its `original` and
      `flake` unchanged, its `locked` unchanged but for `rev`, `narHash`,
      `lastModified` and `revCount`), and the new rev descends from the old
      one and is in the branch the input's `flake.nix` declaration names
      (promoted, e.g. `swan-lake-current`; the default branch without a
      ref). Its `flake.nix` declaration (the evaluated `inputs.<input>`: URL,
      ref, `flake`, and nested `inputs.<input>.inputs.*` overrides and
      follows) must be the merge base's: a moved input whose declaration
      changes fails the check; change the declaration and the lock in
      separate MRs. Only an input new in the lock may lack a declaration at
      the merge base; a moved one that had none there (an implicit input)
      counts as changed. A merge base `flake.nix` that cannot be evaluated
      fails the check with its error.

    Besides, no root input was removed, only a landing adds one, a
    reformatted lock is not a change, nothing else in the lock changed
    (other nodes, other inputs' graphs, shared nodes; a moved input that
    keeps its rev keeps its graph), and the lock below the moved inputs is
    exactly what nix locks for them at their new revisions: `check` runs
    `nix flake lock` on HEAD's flake with the merge base's lock as the
    reference, without the moved inputs, and each overridden
    (`--override-input`) to its new locked source, into a scratch lock;
    nix then takes their inputs from their own `flake.lock` at that
    revision, with the consumer's `follows`, and the result must equal
    HEAD's lock as a graph (every field of every node). That fetches the
    moved inputs' sources, as CI does anyway. It needs Nix 2.34 or later
    (the version it is tested with): `--reference-lock-file`, and an
    `--override-input` that keeps `flake.nix`'s `original` in the lock.
    HEAD's lock passes the pin policy below. A partial landing
    (entries remain) passes; a removed entry whose input did not change is a
    note. Entries still at HEAD are checked as below.

    Only GitHub sources on github.com and git sources over `https://` are
    judged; their revs must be full, and owner and repo names plain. Other
    inputs (tarballs, `path:`, `file://`, ssh, …) that move fail the check
    (`SWL_ALLOW_FILE_URLS=1` allows `file://`, for tests).

    Ancestry is asked of the locked source: for git sources (Forgejo, any
    `git+https` URL) by a commit-only fetch; for GitHub sources (`github:`
    and `https://github.com/` URLs) by GitHub's compare API
    (`GET /repos/{owner}/{repo}/compare/{base}...{head}`), since a nixpkgs
    fetch is too large. The API is anonymous, which GitHub limits to 60
    requests an hour per address, a few per changed input; set
    `SWL_GITHUB_TOKEN` to send a token (only to `https://api.github.com`, or
    a loopback test server; `SWL_GITHUB_API` redirects it, for tests). An
    answer that cannot be had (offline, rate limited, an unknown revision)
    fails the check with the reason.
  - `swl.toml`, if present, parses, and for each entry: the input is that
    repo (both its lock node's `original` and `locked` URLs by the config's
    URL patterns, or for a root
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
uses the real `nix` on PATH, never a swl wrapper. Its evals of the MR's
text run in nix's restricted mode (`restrict-eval`): the declarations
(impure only to import `flake.nix`, with only the repo readable) and each
flake reference (pure, passed as an escaped string literal, never as Nix
code). They fetch nothing, read no file outside the repo and see no
environment variable. The relock (`nix flake lock`) fetches the moved
inputs, as any lock does. No nix call of swl gets `SWL_GITHUB_TOKEN` or
`SWL_FJ_KEYS`.

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

`finalize` is a building block of `swl land`, which pushes its commit to
the MR branch and requests the merge. By hand: push the commit to the MR
branch (fast-forward) and merge the MR once its CI, whose `swl check`
accepts the landing, is green. When entries remain, the commit is a partial
landing, which the MR's `swl check` accepts too; rerun `finalize` once the
rest has landed. `swl publish` refuses to push a lock change.

### Landing

`swl land [--dry-run] [--keep] [--workspace DIR] [--timeout S] [--interval S]`
takes the workspace's change to master, and `swl land --from CONSUMER#MR`
does so for a published change: it runs `swl start --from` first (or reuses
its workspace, `--workspace DIR` or `./<consumer branch>`) and lands there;
a consumer MR that `swl status --from` shows as landed has nothing left to
do (with `--workspace DIR` of an existing workspace, that is removed).

It is stateless: each run reads the state as `swl status` does (the
checkouts, the manifests, the forge and the mirror), does what it can, and
stops at the first step it cannot do, saying what it waits for and for
whom. Rerun it to continue. The steps:

1. The shared MRs that the consumers' `swl.toml` entries name, nested
   entries (`common/zwanenzang`) first. A draft (`WIP:` title) is marked
   ready (its title without the prefix); a draft without that prefix is for
   anyone to mark ready on the forge. Then it is merged with Forgejo's API,
   `POST /repos/{owner}/{repo}/pulls/{n}/merge` with `{"Do":
   "fast-forward-only", "delete_branch_after_merge": true,
   "head_commit_id": <the head read>}`, so that a moved head is not merged
   blindly, after a fresh read of the head's combined commit status:
   - green (`success`): a plain merge, now. Any scheduled merge is cancelled
     first (`DELETE /repos/{owner}/{repo}/pulls/{n}/merge`: 204 cancelled,
     404 nothing scheduled), since a run cannot tell whether an earlier one
     scheduled it;
   - pending, no status yet, or unreadable: the same request with
     `"merge_when_checks_succeed": true`. Scheduled: it waits for CI, then
     merges; nobody waits, rerun. When the status reads green right after
     scheduling, the schedule is cancelled and the MR merged directly, in
     the same run;
   - failing: fix CI first, for anyone.

   Forgejo (17.0.1) starts a scheduled merge only on a later commit status
   event: scheduled while the checks are green already, it answers
   "scheduled" and never merges. So `land` never leaves a green MR
   scheduled, and never says it waits for CI while the status is green. A
   refused cancellation (403, 423: another user's schedule, a locked repo)
   waits for the owner. The user may not merge (403, 405): it waits for the
   owner, `merge <repo>#N`. Not a fast-forward (the base moved): `rebase
   <branch> onto <base> (swl never force-pushes)`, for anyone. An MR merged
   by hand meanwhile is merged: on to the next step. A closed MR: reopen
   it, or drop the entry. A shared checkout that is not published (local,
   needs publish) stops with its `status` step.
2. Promotion, for a merged MR whose input the consumer's `flake.nix`
   declares at a branch other than the repo's base branch (common's
   `swan-lake-current`; zwanenzang, pinned at master, needs none): when
   that branch on the forge is an ancestor of the merge commit,
   `git push origin <merge commit>:refs/heads/<branch>` from the repo's
   source, never forced; skipped when it has the commit. A refused push (a
   protected branch) waits for the owner, with the exact command; a push
   that is no longer a fast-forward (the branch moved since the fetch)
   waits for nobody: rerun. Then the
   declared source (common's GitHub mirror) must have it: polled every
   `--interval` (default 10 s) for up to `--timeout` (default 300 s), as
   `finalize --wait`; after that, rerun.
3. Each consumer: when every entry is satisfied, `finalize` (no partial
   landing is pushed; otherwise it says what it waits for), then the
   landing commit is pushed to the consumer's MR branch (fast-forward
   only: other unpushed commits are refused, `swl publish` them first), and
   after the forge has seen the new head, its merge is requested as above.
   The consumer MR (found by the pull ref at the branch head) must be open,
   for that branch, and at the remote branch head; else it stops for anyone
   to check it on the forge. A draft is marked ready. A consumer without
   entries just has its merge requested.
4. When every consumer MR is merged, the workspace is removed, unless
   `--keep`: each checkout is removed from its source with `git worktree
   remove`, after its now merged local branch is deleted with `git branch
   -d` (the checkout is detached at the base first). A checkout is kept,
   and said so, with uncommitted, untracked or ignored files (other than
   the generated `.envrc.local`, `.direnv/`, `result*` links and `.swl`),
   or with commits in neither the base nor its remote branch. Then `.swl`
   and the directory go. A workspace where no consumer landed through a
   merged MR (its MR merged, or its pushed branch, now in the base, the
   head of a merged MR) has nothing to land and stays.

`--dry-run` reads as a run does and prints the first writes it would do and
what follows, writing nothing (no forge request other than reads, no push,
commit or removal), and needs no fj login for that (only for private
repos' reads); with `--from` and no workspace yet, the plan for the shared
MRs. Exit status: 0 when everything has landed or there is nothing to land
(and for a dry run), 3 when the next step waits for the owner, 4 when it
waits for anyone, 5 when it waits for nobody (CI, the mirror, the forge:
rerun later), 2 on errors.

A green combined status covers the statuses reported so far, and "merge
when checks succeed" waits for the checks that the branch protection of the
base branch requires; with none required, either can merge before `swl
check` has run (Forgejo refuses a direct merge only while a required check
is not green). Each repo's master protection must therefore require the
`swl check` status (and CI) for `land` to be safe.

Writes are only those: the title, merge requests and the cancellation of a
scheduled merge through fj's login (see
[Forge access](#forge-access)), the git pushes of the promotion and of the
landing commit, the finalize commit, and the workspace removal. A write
that one run did already and that the forge does not show yet is not
repeated: the run stops, rerun it.

### Forge access

`status` and `start --from` read MR state and commit status anonymously
first: public repos are read without a token unless the read 404s. On a
404 they ask again with the
login of `fj`: the token for `forgejo.lynx-lizard.ts.net` in fj's
`keys.json` (`$XDG_DATA_HOME/forgejo-cli/keys.json`, default
`~/.local/share/forgejo-cli/keys.json`; `SWL_FJ_KEYS` overrides the path).
That covers the private consumer repos. They only read: the token goes to
curl on stdin, never on a command line or into the output, follows no
redirect, and goes only to `https://` on the forge host (or a loopback test
server; an address with user info is refused). An OAuth login whose
`expires_at` has passed is no login, with a warning; a token the forge
rejects (401 or 403) makes the row `unknown (fj login rejected: run fj auth
login)`. Without a login what the anonymous API cannot see is `unknown`.
swl has no token configuration of its own.

`land` reads the same way, and writes with fj's login, which it needs: the
title of a draft (`PATCH /repos/{owner}/{repo}/pulls/{n}`), merge requests
and the cancellation of a scheduled merge (`POST` and `DELETE
/repos/{owner}/{repo}/pulls/{n}/merge`), under the same token rules (stdin,
no redirect, only the forge).
A 403 or 405 to a merge request is a refusal for the owner, not a rejected
login; a 401 is.

`check`, `ci env` and `finalize` read MR state anonymously only, also with
an fj login (`GET /api/v1/repos/binarin/<repo>/pulls/<n>`, through curl),
and CI fetches
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
- `land` follows one change in one workspace, step by step: an MR that waits
  for CI stops the run, also when later MRs could proceed. It knows a
  merge's outcome from Forgejo's answer (200 merged, 201 scheduled, 409
  conflict, 403/405 refused) and messages, and a cancellation's (204, 404,
  403/423); a scheduled auto-merge that fails shows as the MR still open on
  the next run. These answers, and Forgejo's scheduled merge that waits for
  a status event, are tested against the fake forge of the tests only, not
  against Forgejo itself. The cancellation before a direct merge is a
  write even when nothing is scheduled (it answers 404).
- `land` tells a landed consumer from the forge's pull refs and its
  remote-tracking branch: with `fetch.prune` the pruned branch of a merged
  MR is not seen, so the workspace stays; remove it by hand.
- `swl check` guards against mistakes, not against a hostile MR author: on
  `pull_request` the MR's own workflow runs it, with the swl of the MR's own
  lock (`nix run .#swl`), so the MR can change both. A hardened setup would
  run it from `pull_request_target`, with swl from the base branch and no
  secrets in that job.
- `check` refuses moves of inputs that are not GitHub or `https://` git
  sources (tarballs, GitLab, sourcehut, …), even legitimate ones: bump
  those on master directly.
- `finalize` relocks a root input as a whole: nested entries land when the
  parent (common) has bumped and promoted them.
- `status` tells a landing from a plain lock bump by the branch's history:
  commits since the merge base must touch `swl.toml`. A bump on a branch
  that once had entries for other reasons looks like a landing.
- `status` knows a shared MR only through a consumer's `swl.toml` entry: an
  MR opened by hand without one shows as `needs publish`. A consumer's MR is
  the pull ref at its branch head, so a push that Forgejo has not recorded
  yet hides it for a moment.
- `status --from` reads the declared branch of a nested entry
  (`common/zwanenzang`) from its parent entry's MR; without a parent entry
  the promotion is `unknown`.
- `start` keeps an existing checkout as it is, also on another branch; it
  says so, and switching is left to you.

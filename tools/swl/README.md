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
where it stands (and `swl ls` where all work in progress stands), `swl
refresh` rebases its MR branches onto their bases, and `swl land` takes it
to master. `swl bump` moves
common's inputs forward and the consumers after it; `swl promote` promotes
what reached common's master without a consumer. `plan` is not part of
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
swl ws remove [sl-60] [--force]           # drop a workspace: its worktrees, pushed branches, the directory
swl env                                   # .envrc.local per consumer, and the wrappers
swl status [--from swan-lake#12]          # where the change stands, and the next step
swl ls [--no-fetch]                       # every workspace's stage, and the open MRs in none
swl shim nix --workspace <ws> -- ARGS…    # what the wrappers run
swl publish swan-lake                     # push, open MRs, write swl.toml
swl check                                 # CI: the MR or master gate
swl ci env                                # CI: nix wrapper with the MR overrides
swl finalize [--wait]                     # landed entries -> one lock commit
swl land [--dry-run] [--keep] [--from swan-lake#12]   # merge, promote, finalize, merge, clean up
swl refresh [--dry-run]                   # the change's MR branches rebased onto their bases, pushed with a lease
swl bump [--dry-run] [--check]            # common's inputs forward, promoted, a bump MR per consumer
swl promote [<rev>] [--dry-run]           # common's swan-lake-current fast-forwarded to master (or <rev> of it)
swl hook zsh|bash                         # the shell function swl, which cds after start: eval "$(swl hook zsh)"
```

`start` creates workspaces below the repos config's `workspaces` directory,
regardless of the current directory (also with `--from`). Its default is
`~/personal-workspace/swan-lake-umbrella/workspaces`. `start --workspace DIR`
still creates or resumes an explicit directory instead.

`status`, `land`, `refresh`, `publish`, `env`, `ws add` and `ws remove` select
the enclosing workspace when run anywhere inside it, including inside a
checkout. They also accept `--workspace NAME|TICKET|PATH` from anywhere.
`status`, `land`, `refresh`, `env` and `ws remove` accept the same selector as
an optional positional argument; `publish` and `ws add` keep their existing
consumer/checkout positional argument:

```sh
swl status sl-88
swl env --workspace SL-0088-swl-version
swl refresh sl-88 --dry-run
swl publish swan-lake --workspace sl-88
swl ws add swan-lake-common@common --workspace sl-88
swl ws remove sl-88
```

Names are looked up directly below `workspaces`, case-insensitively and ignoring
leading zeros in ticket numbers: `sl-88` finds `SL-0088-swl-version`, as does
`sl-88-SWL-VERSION`. A ticket-only selector matches any suffix, not a numeric
prefix (`sl-8` does not match `sl-88`). Multiple matches are an error listing
all candidates, even when one has the exact requested spelling. A full name
can disambiguate different suffixes; an explicit path can disambiguate different
spellings of the same name.

Absolute paths, `~/…`, `./…` and paths containing `/` bypass name lookup.
Configured names take precedence over bare relative directories; if no name
matches, an existing bare relative directory still works for compatibility.
Outside a recognizable workspace, commands without a selector retain the old
current-directory behavior (so `ws add` can build up an empty directory).
The global `--config FILE` selects the repos config.

The global `--color auto|always|never` colours the output: the `swl <cmd>:`
prefix bold, errors red, warnings yellow, what waits for the owner magenta,
what is done green, the progress output's tree (see [Progress
output](#progress-output)), and `status` (see [Status](#status)). `auto`, the
default, colours a terminal only, never with `NO_COLOR` set, and always with
`CLICOLOR_FORCE` set (and no `NO_COLOR`). Without colour the text is the same
as with it, escape codes aside. `--color` does not reach the output of the
argument parser itself (`--help`, usage errors), which colours itself on a
terminal.

- `start NAME CHECKOUT... [--workspace DIR] [--branch B] [--no-fetch]` sets up
  the workspace for one change: `DIR` (default `<workspaces>/NAME`) with each checkout
  (at least one consumer, plus `<repo>@<input>` shared checkouts) on branch
  `B` (default `NAME` lower-cased: workspace `SL-0060-foo`, branches
  `sl-0060-foo`), added as `ws add --branch B` does, fetching each source
  once. Then `env` once, and `direnv allow` for each consumer with an
  `.envrc` when `direnv` is on PATH (else a note to run it). It ends with the
  checkouts and a `cd` hint, and writes the directory the hint names to
  `$SWL_CD_FILE` when that is set (see `hook` below). Rerunnable: an
  existing checkout directory is kept, and reported when it is on another
  branch (never changed); missing ones are added. An interrupted `start` is
  finished by rerunning it. A differently spelled equivalent workspace name
  (case or ticket zero-padding) is refused with a hint to use the existing
  workspace, rather than creating a duplicate. This also applies to
  `start --from` and explicit directories' sibling names. An existing directory
  that is not a worktree of the repo's configured source is refused.
- `start --from CONSUMER#MR [--workspace DIR] [--no-fetch]` joins a
  published change, for example on another machine. With git (consumer repos
  are private), it reads `refs/pull/MR/head` from the consumer source's
  remote and the one branch at that head, other than the base branch; with
  none (the MR was merged, its branch deleted, or the branch moved on) or
  several, it refuses. It
  fetches that head and reads its `swl.toml`. The workspace is `DIR`
  (default `<workspaces>/<consumer branch>`): the consumer on its branch, tracking the
  remote's, and for each entry `[inputs.<path>] repo, mr` the checkout
  `<repo>@<last segment of path>` (`common/zwanenzang` gives
  `zwanenzang@zwanenzang`): on an open MR's head branch, or detached at a
  merged MR's merge commit. A closed or unknown MR is an error, before
  anything is added. Then `env`, `direnv allow`, the `cd` hint and
  `$SWL_CD_FILE` as above; rerunnable likewise.

- `ws add NAME [--branch B] [--no-fetch] [--no-env]` runs `git worktree add`
  in the repo's configured source, which can be a bare repository or a main
  checkout.
  - First it fetches the source's remote that the `base` names (`origin` for
    `origin/master`) with `git fetch --prune`, so that the new worktree
    starts at the current base and no branch deleted there is joined. A
    base without a remote prefix is not fetched. `--no-fetch` skips the fetch
    for offline work; a failed fetch is an error, not a stale base.
  - Without `--branch`, the worktree is detached at `base`. With `--branch B`:
    an existing local B is checked out, fast-forwarded first when it is
    strictly behind `<remote>/B`; else B is created at `<remote>/B` with that
    upstream, to join a published branch; else B is created at `base`
    without an upstream.
  - An existing local B that has diverged from `<remote>/B` (a stale
    branch, left at its old head when the MR was rebased and force-pushed)
    is reset to `<remote>/B`, and said so (`swl ws add: reset the branch B
    of <source> from <old> to <remote>/B at <new>: …`), only when none of
    its commits would be lost: each is on a branch of the remote it fetched
    (`<remote>/*`; other remotes, not fetched, do not count), or its change
    is on `<remote>/B` (the same patch, as `git cherry` tells it: a plain
    rebase; an empty commit is never the same patch). With `--no-fetch`,
    that is judged by the `<remote>/*` refs the source already has.
    Otherwise it is refused, naming the commits pushed nowhere (short sha
    and subject), with how to keep or drop them; nothing is reset. By
    design this refuses a branch whose commits were amended, or rebased
    with conflicts resolved, before the force-push: their patches differ.
    Other worktrees are not touched: B is checked out in none (see below).
  - A worktree that stands in the way, registered at the checkout's
    directory or holding the existing branch B, but whose directory is gone
    (a workspace removed by hand with `rm -rf`), is pruned first: `git
    worktree remove <path>` in the source drops just that registration, and
    says so with its full HEAD sha (`swl ws add: pruned the worktree <path>
    (branch B at <sha>) of <source>, whose directory is gone (git worktree
    remove)`; `(detached at <sha>)` for a detached one). A detached one's
    commits pushed nowhere (as for the reset above, but with no remote
    branch to compare patches with) are kept first: on the new branch
    `swl-kept/<workspace>-<checkout>` of the source, named in that message
    with the commits and the `git branch -D` that drops them; it refuses,
    pruning nothing, when that branch exists already or is not a valid
    name. Only those: nothing is pruned when nothing stands in the way,
    other registrations whose directories are gone (a worktree moved, or
    on media not mounted) are left alone, and a worktree whose directory
    exists, or a locked one, still refuses (`branch B is already checked
    out at <path>`; git's `is a missing but locked worktree` at the
    checkout's directory). `start`, `start --from` and `land --from` add
    their checkouts the same way.
  - Then it runs `env` for the workspace, unless `--no-env`, or while the
    workspace has no consumer checkout yet. When `env` refuses (a
    hand-written `.envrc.local`), the checkout is still added; fix it and
    rerun `swl env` (`--force` replaces the file).
- `ws remove [WORKSPACE] [--workspace NAME|TICKET|PATH] [--force] [--no-fetch]`
  drops a workspace that is abandoned, or that is to be joined afresh
  (`start --from`, `land --from`); it is the supported way, rather than `rm
  -rf`, which leaves the sources with the worktrees registered and the
  local branches (see [Recovering from a workspace removed by
  hand](#recovering-from-a-workspace-removed-by-hand)).
  - First it fetches each source's base remote, as `ws add` does, and
    checks every checkout before removing any: it must be a linked worktree
    of its repo's configured source, there and not through a symbolic link,
    not the source's main working tree, and not locked, else nothing is
    removed (`.envrc.local` included). A checkout with uncommitted changes
    or untracked files (but swl's generated `.envrc.local`; whatever
    `status.showUntrackedFiles` says), or with commits pushed nowhere, is
    listed with why, commits by short sha and subject; then nothing is
    removed and it fails, unless `--force`. Pushed means as for `ws add`'s
    reset: on a branch of the base remote that this run fetched, or the
    same patch as one on the checkout's remote branch (never for an empty
    commit). `--no-fetch` skips the fetch, and then judges by the refs the
    source already has, which can be stale: a branch deleted on the forge
    still counts as pushed. Ignored files do not keep a checkout (as for
    `ls`'s `removable`); those it deletes are listed (`deleting the ignored
    files of <checkout>: …`), but for direnv's `.direnv/`, `result*` links
    and `.swl`.
  - Then per checkout: `git worktree remove` from its source (`git worktree
    remove --force` with `--force`), after the generated `.envrc.local` is
    deleted. Without `--force`, a checkout whose HEAD, branch or files
    changed after it was checked is kept, and the run stops there (`stopped
    at <checkout>: …`; the checkouts removed before it are said so). Then
    its branch, `git branch -D` in the source, when the branch's commits are
    all pushed. A branch with commits pushed nowhere (only with `--force`)
    is kept, and said so, with the `git branch -D` that drops it. So are a
    detached checkout's commits pushed nowhere, with `--force`: on the new
    branch `swl-kept/<workspace>-<checkout>` of its source, said so with
    the full sha (`kept the commits of <checkout>, detached at <sha>, on the
    branch swl-kept/…`); the run refuses up front, removing nothing, when
    that branch exists already.
  - Last `.swl` and the directory go; a directory with other files is kept,
    and said so.
  - Run from inside the workspace (a checkout included), it works the same;
    it says that the current directory was in the workspace, to `cd` out of
    it.

  ```
  swl ws remove: workspace /ws/SL-0005-rm
  ├─ removed /ws/SL-0005-rm/swan-lake
  ├─ deleted the branch sl-0005-rm of swan-lake: its commits are on remote branches
  ├─ removed /ws/SL-0005-rm/swan-lake-common@lib
  ├─ deleted the branch sl-0005-rm of swan-lake-common: its commits are on remote branches
  └─ removed /ws/SL-0005-rm
  ```
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
- `ls [--no-fetch]` shows all work in progress: a line per workspace, and
  the forge's open MRs that no workspace holds; see [Ls](#ls).
- `hook zsh|bash` prints, on stdout, the shell function `swl` for an
  interactive shell, to be loaded with `eval "$(swl hook zsh)"` (or `bash`)
  in its rc file; `homeModules.swl` adds that line to home-manager's zsh and
  bash init. Any other shell is a usage error. The function runs `command
  swl` from `PATH` with its arguments, with `SWL_CD_FILE` set to a new
  temporary file (`mktemp` in `$TMPDIR`, else `/tmp`). When swl succeeds and
  has written a directory to that file, the function changes to it; it
  removes the file either way and returns swl's exit status (or, when the
  `cd` fails, that of `cd`). `set -e`, zsh's `ERR_RETURN` or an ERR trap
  in the shell do not stop it before it removes the file. When `mktemp`
  fails, it runs swl with `SWL_CD_FILE` empty and does not change
  directory. A call interrupted with Ctrl-C can leave the file behind.

  The `SWL_CD_FILE` protocol: a command that leaves the shell somewhere
  else writes that directory's absolute path to the file named by
  `$SWL_CD_FILE`, as the file's only content (no newline), when the variable
  is set and not empty, and only on success. Only `start` (also with `--from`) does: it
  writes the first consumer's checkout, the one its `cd` hint names; `land
  --from`, which can remove the workspace it starts, does not. Without the
  variable (or with it empty) nothing is written; a failure to write is a
  warning.

`publish`, `check`, `ci env`, `finalize`, `land` and `refresh` are
described under [Manifest and landing](#manifest-and-landing), `bump` under
[Bump](#bump), `promote` under [Promote](#promote).

### Recovering from a workspace removed by hand

A workspace directory removed with `rm -rf` leaves two things in each source
(bare repo): its worktrees stay registered, holding their branches (`swl
land --from swan-lake#61` then failed with `branch … is already checked out
at <gone path>`), and its local branches stay where they were, which after
the MR was rebased and force-pushed is a stale, diverged head (that `land`
then asked to merge or rebase). Since swl 0.10.0, `start`, `start --from`,
`land --from` and `ws add` prune such a worktree and reset such a branch
when they join (see `ws add` above), and `ws remove` is the way to drop a
workspace. With an older swl, by hand, in each source of the workspace's
checkouts:

```sh
git -C <source> worktree prune                         # forget worktrees whose directories are gone
git -C <source> log --oneline <remote>/<branch>..<branch>   # what only the local branch has
git -C <source> branch -D <branch>                     # drop the stale branch, joining creates it anew
# or keep the name and move it: git -C <source> branch -f <branch> <remote>/<branch>
```

## Progress output

The multi-step commands (`land`, `bump`, `promote`, `refresh`, `publish`,
`finalize`, `ws remove`) print their progress as a tree, as the run goes on: a section
per repo, MR or phase, headed `swl <cmd>: <name>`, its steps nested below.
A `swl bump` that bumps common and opens the consumers' bump MRs:

```
swl bump: common
├─ up to date: disko, emacs-niri-awareness, emacs-overlay, emacs-tramp-rpc, flake-parts,
│  home-manager, import-tree, llm-agents, nixpkgs, nixpkgs-unstable, omnidiff, sops-nix
├─ zwanenzang 11a574392516 -> e40d4498c7c3 (the default branch)
├─ relock zwanenzang on master at 4f5d3c028d43 commit "swl: bump zwanenzang" and push it to origin's master (fast-forward)
├─ pushed 8ab4ff130b05 to master: swl: bump zwanenzang
├─ swan-lake-current at 4f5d3c028d43 gets 1 commit(s):
│  8ab4ff1 swl: bump zwanenzang
├─ promote swan-lake-common master
│  git -C /home/binarin/personal-workspace/bare-repos/swan-lake-common.git push origin 8ab4ff13…:refs/heads/swan-lake-current
├─ promoted swan-lake-current to 8ab4ff130b05
│  https://github.com/binarin/swan-lake-common.git swan-lake-current does not have 8ab4ff130b05 yet; checking again in 10s
╵
swl bump: nixos-config
├─ push nixos-config's bump of common to 8ab4ff130b05 as origin's swl-bump-common
├─ open the bump MR "swl: bump common to 8ab4ff130b05" of nixos-config
├─ request the merge of nixos-config#372 at 23f6f85ddb3a (fast-forward only, when its checks succeed, deleting its branch)
└─ waits for nobody: nixos-config#372 waits for CI, then it merges (merge when checks succeed); rerun swl bump
swl bump: swan-lake
├─ push swan-lake's bump of common to 8ab4ff130b05 as origin's swl-bump-common
├─ open the bump MR "swl: bump common to 8ab4ff130b05" of swan-lake
├─ request the merge of swan-lake#58 at bae3292f4218 (fast-forward only, when its checks succeed, deleting its branch)
└─ waits for nobody: swan-lake#58 waits for CI, then it merges (merge when checks succeed); rerun swl bump
```

- A section's heading comes with its first line: one with nothing to say
  (an MR landed already) prints nothing. `land` has a section per shared
  MR and per consumer MR (`swan-lake#31`), and `workspace <dir>` for the
  removal; `bump` one for common and one per consumer; `promote` one for
  what it promotes (`swan-lake-common origin/master`); `refresh` one per MR;
  `publish` one per checkout; `finalize` one for `swl.toml`; `ws remove`
  one, `workspace <dir>`.
- `├─ ` a step: what swl does (with `--dry-run`, `would …`) or found;
  outcomes (pushed, promoted, merged, committed, an MR opened) bold, in
  green.
- `│  ` dimmed, a detail of the step above: the command swl runs (a
  promotion's `git push`), a step's further lines (the commits a promotion
  brings), a wait (`checking again in 10s`).
- `└─ ` the section's final outcome, which closes it: what it waits for
  (`waits for the owner|anyone|nobody: …`, coloured as in `status`), what
  a dry run would do next (`dry run, nothing was written; then: …`), or
  its result.
- `╵` closes a section that the run leaves without a final outcome: it
  goes on to the next section (common, bumped and promoted, before the
  consumers; a shared MR merged before its consumer), or an error ends the
  run (on stderr, after it).
- A line drops a leading `<name>: ` that repeats its section's heading
  (`swan-lake#31: pushed …` under `swan-lake#31` reads `pushed …`); the
  wording is otherwise the same as without the tree. A long list (`up to
  date: …`) wraps at the commas before 100 columns.
- Lines about the whole run stand outside any section, prefixed `swl
  <cmd>:` as headings are: `swl bump: nothing to bump`, `swl bump: done:
  …`, `swl land: every MR of the change is merged`; also a stop that is
  not one item's (`land` with no consumer that holds a change, too many
  steps in one run).

Why not the `├─`/`└─` of a finished tree throughout: the output streams
(waits, retries, pushes that take a while), so when a line is printed it is
not known whether more lines of its section follow. A step is therefore
always `├─ `, and `└─ ` is kept for the line that the command knows to be
the section's last, its final outcome; `╵` is the end of a section that
had none. Nothing is held back to be redrawn, so each line appears as the
step happens, also in a pipe or a file.

The glyphs are Unicode box drawing, the same as `status`'s tree, also
without colour: `--color never`, `NO_COLOR` or a pipe give the same text
without escape codes. Errors and `warning:` lines stay on stderr, prefixed
and greppable: `swl bump: error: <consumer>: …`, `swl publish: warning: …`.
A step that needs attention (an input not bumped, a checkout kept) is a
step of its section on stdout, in the warning colour.
`swl start` ends with the workspace's checkouts as a tree too, each on its
branch, then the `cd` hint.

## Status

`swl status [WORKSPACE] [--workspace NAME|TICKET|PATH] [--no-fetch]` for a workspace, and
`swl status --from CONSUMER#MR [--no-fetch]` for a published change without
one. It prints one row per shared checkout, per `swl.toml` entry that no
checkout follows, and per consumer, as a tree per consumer:

```
swan-lake                              CI green  waits: owner   next: wait for swan-lake-common#24 (merge swan-lake-common#24)
│  HEAD 1c0ffee  sl-0060-foo  clean
│  swl.toml: common swan-lake-common#24, common/zwanenzang zwanenzang#7
│  swan-lake#31: open, sl-0060-foo at 1c0ffee00000, CI green
└─ common: swan-lake-common@common     CI green  waits: owner   next: merge swan-lake-common#24
   │  input common  HEAD 72f1eec  sl-0060-foo  clean
   │  swan-lake: common locked 9477137, ahead 2, behind 0
   │  swan-lake-common#24: open, sl-0060-foo at 72f1eec824d6, CI green
   └─ common/zwanenzang: zwanenzang#7  promoted  waits: anyone  next: swl land
         swan-lake: common/zwanenzang
         zwanenzang#7: merged as 5e1f00d9a2b4, in master; the declared source is not checked
```

Each consumer is a root. Under it, by input path, come the shared checkouts
that override its inputs and its `swl.toml` entries (`<input path>:
<checkout or MR>`), a nested path under its parent's row when there is one.
A row under several consumers is shown in full under the first; under the
others its line says `details under <consumer>: <path>`. Rows under no
consumer follow as roots of their own; a blank line separates the roots.
The columns are aligned: the stage, `waits: <who>`, `next: <step>` (a very
long name or stage pushes its own line's columns). The glyphs are Unicode
box drawing, as in the [progress output](#progress-output), also in a pipe
or a file; this plain form is stable. On a
terminal (see `--color` under [Commands](#commands)) each row's stage and
waits are coloured by whom it waits for: the owner magenta (the next step
too), anyone yellow, nobody cyan, done (`next: -`) green, unknown red; names
are bold, glyphs and detail lines dimmed.

A row is the checkout or MR, its stage, who it waits for and the next step:
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
| `merged` | the MR is merged (its merge commit) | its branch is gone on the remote, and the MR of that branch merged without HEAD's commits outside the base (they predate a rebase): remove the workspace; common, landed as below but not promoted yet: `promote <branch>`, for the owner |
| `promoted` | the merge commit is in the declared branch on the forge (`swan-lake-current` for common; an input without a ref is promoted when merged) | common, landed as below and promoted, but not in a declared source yet: wait for it, nobody |
| `landing pushed`, `landing pushed, CI failing` | | the MR is open, and its branch has no pending `swl.toml` entries, had some (its commits since the merge base touch `swl.toml`), and changes `flake.lock` |
| `obsolete: its change is in <base>` | | the MR is open, and its change is in its base already (below): close it, for anyone |
| `landed` | merged, and every consumer that names it in `swl.toml` has landed | the MR is merged, or both the local and the remote branch heads are in the base, or the branch is gone on the remote (a merge deletes it) and HEAD, in the base, is the head of the branch's merged MR or has its merge commit; common as a consumer, its MR merged, also promoted and in the declared sources |

A shared checkout without an MR whose HEAD is in the rev each consumer
locks, or already in its base, is `no MR needed`; a consumer on a topic
branch that is not on the remote, with its HEAD in its base, is `no change`.
A consumer that is detached or on its base branch, at a commit of its base
(as `swl ws add` leaves it), is `not a change (detached at origin/master)`:
it does not hold the change. A consumer that uses an overridden checkout
changed without a `swl.toml` entry (commits, or uncommitted changes to
tracked files) is neither: it is `local`, as with commits of its own. For a consumer at its base that is not on the
workspace's branch (the workspace's directory name lower-cased, the branch
`swl start` makes), `status` asks the forge for that branch's MR in the
consumer's repo (`GET /repos/{owner}/{repo}/pulls/{base branch}/{branch}`);
when it is open, the next step is `git -C <consumer> switch <branch>, then
swl land` (anyone), and a detail line names the other way, `swl land --from
<consumer>#<MR> --workspace <new-path>`: as `start --from`, it keeps an existing
checkout as it is, so joining elsewhere needs an explicit new directory and the
branches of the MR's shared entries not checked out in this one. A consumer with commits
outside its base on a branch that is not on the remote is looked up
likewise: when that branch's MR merged and its merge commit is not in HEAD's
history (the commits from before a rebase), it is `merged`, next `remove the
workspace`; with the merge commit in HEAD's history it is new work, `local`;
with a merge commit not known here, `unknown`, next to check the MR.
A lookup that fails is a detail line and a warning; the row stays as it is.
Without fj's login, a lookup that finds none is a detail line too: a private
repo answers so to anyone.
Common as a consumer (a common-only change) goes on as a shared MR does
once its MR merged (also when the merge kept its branch: the MR of that
branch, at its head): the MR's merge commit must be in the branch that
common's consumers declare for it and in their declared sources, the same
that `swl bump` promotes (see [Bump](#bump)); until then it is `merged`
(waits: owner, next: `promote swan-lake-current`) or `promoted` (waits:
nobody, the mirror), with a detail line `promotion of <rev>: …`, and the
workspace is not done. A change of common in its base without a merged MR
(such as a branch pushed at master) is `landed` by git alone, and promotes
nothing. When that branch or those sources cannot be read, it is
`unknown`, next `rerun swl status` (waits: nobody).
An open consumer MR whose change its base has already (a duplicate of a
bump that landed by another MR on the same parent, as nixos-config#377 was
of #376; a rebased copy that landed) is `obsolete: its change is in
<base>`, next `close <consumer>#<MR>`, waiting for anyone, with a
detail line `<consumer>#<MR>: its change is in <base> already
(merging it changes nothing)`; unpushed commits of the checkout come first
(`swl publish`). Once it is closed the row reads as any closed MR's,
`reopen <consumer>#<MR>, or swl publish <consumer> for a new MR`; neither
applies here (a new MR would be obsolete too): remove the workspace
instead (`swl ws remove`). The test is git's: merging the MR head into the
base's tip with `git merge-tree --write-tree` (in the object store, no
checkout) is clean and gives the tip's own tree. A head that its base contains is
`landed` instead; a change that the base has only in part, or that
conflicts with how the base changed the same lines since (a bump to an
older rev), is not obsolete. A rebase onto the tip would tell the same but
needs a worktree; a diff against the merge base, or `git cherry`, misses a
change that landed as other commits.
When no consumer holds a change (each is `no change` or `not a change`), a
line after the rows says so. A consumer MR with entries is not merged on its
own: while its own step waits for the owner (merge) or nobody (CI), its next
step waits for the entries' rows, then is `swl land` (see
[Landing](#landing)). A draft or failing CI is its own step first.

The next steps are `swl publish <consumer>` (anyone), `mark <repo>#N ready`
(anyone), wait for CI (nobody), `merge <repo>#N` (owner),
`promote <branch>` (owner), `swl land` (anyone), `open the MR` (anyone) and
`request merge when checks succeed` (owner) once the landing is pushed
(`fix CI`, anyone, while it fails), and `close <consumer>#N` (anyone) for an
obsolete consumer MR.

By default `status` fetches each checkout's remote (as `ws add` does, once
per source, with `--prune`) so that the stages are current, also "gone on
the remote" whatever the git config's `fetch.prune`, and finds a consumer's
MR with
`git ls-remote <remote> 'refs/pull/*/head'`; the mirror check fetches commit
history only. For the promotion of common's merged MR as a consumer, it
(and so `land`) also fetches the remote of each other repo of the swl
config, to read the branch and the declared sources of common's consumers
at their base, as `swl bump` does. `--no-fetch` uses the refs already known
and looks up no consumer MR, by pull ref or by branch: a pushed consumer is
`pushed`, next `rerun without --no-fetch (it does not look up the MR)`
(anyone); a consumer with commits outside its base on a branch not on the
remote is `local`, next `swl publish <consumer> (unless <branch>'s MR
merged: --no-fetch does not look it up)`, since that MR may have merged
them as rebased (see above). A forge, network or git error makes
that row `unknown (<reason>)`; it is never an error of the command.

MR state and CI come from the forge API: anonymously, and for what that
cannot see with fj's login (see [Forge access](#forge-access)). A repo that
cannot be read (private, and no login) falls back to git: the branch on the
remote, and `landed` when its head is in the base; its MR shows as
`unknown (not readable anonymously, and no fj login: …)`, and a pushed
landing as `pushed`. A login the forge rejects makes the row
`unknown (fj login rejected: run fj auth login)`.

`--from CONSUMER#MR` reads the consumer MR as `start --from` does (with git),
then shows a row per `swl.toml` entry at its head and the consumer row. A
cross-repo list of all open changes is `swl ls` (see [Ls](#ls)).

For a workspace, after the rows, `status` names the running swl (at the
package's wrapper that `SWL_SELF` names, when set) and the one the wrappers
exec (see [Version](#version)):

```
running swl: swl 0.2.0 (abc1234 2026-11-26) at /nix/store/…-swl-0.2.0/bin/swl
wrappers' swl: swl 0.2.0 (abc1234 2026-11-26) at /nix/store/…-swl-0.2.0/bin/swl
```

The wrappers' swl is the one `<ws>/.swl/bin/nix` execs, and its version is
what it prints for `--version`. When the two versions differ, or that swl
does not run (`wrappers' swl: unknown (<reason>)`: it is gone, not
executable, or its `--version` does not finish in 5 s, after which it is
killed), a warning on stderr says to rerun `swl env`. Without wrappers
yet the line says `none yet`, without a warning. `--from` prints neither
line.

## Ls

`swl ls [--no-fetch]` shows all work in progress: the workspaces, and what
the forge has open that no workspace holds.

```
workspaces in /home/binarin/personal-workspace/swan-lake-umbrella/workspaces:
  SL-0060-foo                     CI green  waits: owner   next: wait for swan-lake-common#24 (merge swan-lake-common#24)
  SL-0088-swl-version  removable  landed    waits: nobody  next: -
  SL-0094-swl-ls                  local     waits: anyone  next: swl publish swan-lake

open on the forge, in no workspace:
  SL-0042
    swan-lake#5         open (draft)  sl-0042-a        WIP: a
    swan-lake-common#7  open          sl-0042-a        a
    swl start --from swan-lake#5
  swl-bump-common
    swan-lake#8 (bump)  open          swl-bump-common  swl: bump common to 33a2bda17982
    swl start --from swan-lake#8
```

The workspaces are the directories below the repos config's `workspaces`
(see [Config](#config)), dot directories aside, by name. Each gets one
line: its stage, who it waits for and the next step, from the rows that
`swl status` computes for it (and as it colours them). Of the roots of its
tree (the consumers, and rows under no consumer), whose verdicts already
wait for the rows below them, the line is an `unknown` one first, else the
least advanced (in the order of the stages, see [Status](#status)) that has
a next step, else the least advanced. Its next step ends with the other
roots that have a next step at another stage, `(+ <row>: <stage>, …)`: a
CI-green consumer's `merge swan-lake#3 (+ nixos-config: merged)`. A
directory without checkouts is `no change (no checkouts)`; one that cannot
be read, `unknown (<reason>)`. The workspaces are read one after another,
with one fetch per source (as `status` fetches) for all of them.

A workspace that `swl ws prune` would remove is marked `removable`: each of
its checkouts has its HEAD in its base (also detached at the base), its
branch is gone on the remote or in the base, and `git status --porcelain`
shows no changes and no untracked files but swl's own generated
`.envrc.local` (the one with the generated marker; ignored files do not
count). A workspace without checkouts is not removable. This reads the refs
as the fetch left them, from git alone.

Then the open MRs (drafts too, by anyone) of every repo of the config,
from `GET /repos/{owner}/{repo}/pulls?state=open` (read as `status` reads
the forge, see [Forge access](#forge-access)), but those that a workspace
holds: an MR is held when a checkout of its repo is on its branch, when it
is of a consumer repo of a workspace on the workspace's branch (its name
lower-cased, the branch `swl start` makes; also with that checkout detached
or on another branch), or when a consumer's `swl.toml` in a workspace names
it (a `swl.toml` that cannot be read names none, logged at debug). They are grouped by the
ticket ID in their branch (`SL-NNNN`, anywhere in it after a character
that is not a letter or digit, compared case-insensitively and without
leading zeros, as workspace names are; shown as the first MR's branch spells
it), else by branch: tickets by number first, then branches by name; in a
group, by repo and number. An MR of the branch `swl-bump-common` (`swl
bump`'s) is labelled `bump`. Each group ends with the MR to join it from,
`swl start --from <consumer>#<MR>`: the first MR of a consumer (a repo
whose base branch locks common, as for `swl bump`), naming the other
consumer MRs too; with none, common's MR, as a change of common alone (common
as the consumer); with neither, it says that there are only shared-repo MRs
and no consumer MR to start from. A repo whose MRs cannot be read is a
line `<repo>: not read (<reason>)` (a private repo without fj's login, say).

`--no-fetch` gives a quick offline view of the workspaces only: no fetch
and no forge request at all, and the open MRs are not listed. A line
claims nothing that needs the forge: a pushed consumer is `pushed`, next
`rerun without --no-fetch (it does not look up the MR)` (anyone), its MR,
CI and entries unknown (with its landing commit pushed, `check the MR of
<consumer> on the forge …`); a consumer with commits on a branch gone
from the remote refs is `local`, next `swl publish <consumer> (unless
<branch>'s MR merged: --no-fetch does not look it up)`, as `status
--no-fetch` says (see [Status](#status)). The rows below the consumers
that need the forge are `unknown (--no-fetch: the forge is not read)`; the
line waits for them through the consumer's verdict only. The exit status is
0, also with unknown lines.

## Diagnostics

`SWL_LOG` sets what swl logs on stderr, as a `tracing` filter: `debug`,
`swl::forge=trace`, `swl::cmd=debug,swl::land=debug`. The default is
`warn`. At debug: every external command (git, nix, curl, direnv) with
its arguments, exit status and duration (target `swl::cmd`); each forge or
GitHub API request with its method, URL, whether it carried fj's token, and
the HTTP status (`swl::forge`; the answer's body at trace); and the
decisions of `status` (each row's stage, next step and waits) and `land`
(per pass, per MR and consumer). The token never appears: it goes to curl
on stdin, which is not logged, and arguments that look like credentials
(an `Authorization` header, a password in a URL, the value of an
`access-tokens` option) are scrubbed. The shims (`swl shim`, `swl ci
shim`) log nothing unless `SWL_LOG` is set; `SWL_SHIM_LOG` is separate.

## Version

`swl --version` prints one line: `swl 0.2.0 (abc1234 2026-11-26)`. First
the version in `tools/swl/Cargo.toml`, the only place it is written (the
package reads it from there). Then the short rev and the commit date (UTC)
of the swan-lake-common commit the package is from: the flake's `shortRev`
and `lastModifiedDate`, which swl reads at runtime from `SWL_COMMIT` and
`SWL_COMMIT_DATE`.

`modules/swl.nix` builds the binary as `packages.swl-unwrapped`, from swl's
sources only (see below), so that it is rebuilt only when they change.
`packages.swl`, which the dev shell, `homeModules.swl` and `nix run .#swl`
use, is a wrapper around it, remade on every commit of common, that sets
`SWL_COMMIT`, `SWL_COMMIT_DATE` and `SWL_SELF`, the wrapper's own path. A
dirty tree shows the rev with `-dirty` (`abc1234-dirty`); without a rev, or
without the wrapper (`swl-unwrapped`, a plain `cargo build`), it is
`swl 0.2.0 (unknown)`. `swl env` pins `SWL_SELF` when it is set, else the
running executable, so that the workspace's wrappers, its GC root and the
version in `swl status` go through the package's wrapper. swl removes the
three variables from its environment when it starts, so that what it runs
does not inherit them.

Every MR that changes swl bumps its version in `Cargo.toml` (and the swl
entry of `Cargo.lock`). Before 1.0: the minor version for a breaking change
or a new feature, the patch version for a fix. swl's sources are the paths
below `tools/swl` that `tools/swl/src-fileset.json` lists, which is also the
source fileset of `swl-unwrapped`, and that list itself. The `swl version`
workflow (`.forgejo/workflows/swl-version.yaml`) enforces the rule on MRs
with `scripts/check-swl-version-bump.py`: when the MR changes a source
since its merge base with the target branch, the version at its head must
be greater, by semver precedence, than at the merge base and at the target
branch; otherwise the job fails. It runs when the MR is opened, pushed to,
reopened or edited, so retargeting it checks it again. Every push to
`master` runs the same check of the pushed master against master before the
push: two MRs that made the same bump each pass against the old master, and
the second to merge fails `master`, to be fixed with a further bump. The push
that creates `master` is not checked.

## Config

The repos config is `--config FILE`, else `$SWL_CONFIG`, else
`$XDG_CONFIG_HOME/swl/repos.toml` (`~/.config/swl/repos.toml`). A missing file
at the default location means an empty config: the shim and `env` still work,
but `ws add` needs the repos listed. A file named by `--config` or
`$SWL_CONFIG` must exist. When `env` runs with one of them, the wrappers pass
it on as `--config <absolute path>`, so the shim uses the same config as the
`.envrc.local` files.

```toml
# Optional; this is the default. ~/ is expanded using HOME.
workspaces = "~/personal-workspace/swan-lake-umbrella/workspaces"

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

`workspaces` may be absolute or relative to the config file's directory;
`~` and `~/…` expand to `$HOME`. It need not exist yet: `start` creates it.
This setting does not relocate or rename existing workspaces; use explicit paths
for workspaces outside it.

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
consumers' dev shells need no change (with `--config FILE` before `shim`
when the config was chosen explicitly). That `swl` is the package's wrapper
that `SWL_SELF` names, else the running executable; see
[Version](#version):

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

### `swl publish [--workspace NAME|TICKET|PATH] <consumer>`

From a workspace, for the consumer checkout `<consumer>`:

1. For each shared checkout that overrides one of the consumer's inputs and
   whose HEAD differs from the rev the consumer locks: push its branch to
   `origin`, fast-forward only. A HEAD already in the repo's base (config
   `base`, default `origin/master`, fetched first) is a lock bump or merged
   work, not an MR, and is skipped.
2. Find the open MR from that branch into the base branch, in the forge's
   list of open MRs (`GET /repos/{owner}/{repo}/pulls?state=open`, by its
   head and base branch; an open MR swl cannot read is skipped with a
   warning), or open one as a draft (`POST /repos/{owner}/{repo}/pulls`,
   title `WIP: ` and the first commit's subject, body the commit list).
   When the forge refuses to open it because it has one already (409 or
   422), that one is looked up again and used; publish fails only when the
   list still does not have it.
3. Write `mr = N` entries for its input paths into the consumer's
   `swl.toml` and commit only that file on the consumer branch
   (`swl: manifest for swan-lake-common#24`). With nothing published, an
   existing `swl.toml` is left as it is.
4. Push the consumer branch and find or open its MR (not a draft; the body
   lists the dependencies).

Everything is checked before anything is pushed. Refused: a branch that is
not on `origin`, with commits outside the base, whose MR, looked up as
`status` does (`GET /repos/{owner}/{repo}/pulls/{base branch}/{branch}`,
with fj's login for a private repo), merged without them (they predate a
rebase): publishing would open a new MR for landed work, so remove the
workspace, or put new work on a new branch (a lookup that fails, or a merge
commit not known here, is a warning). Also refused: a detached HEAD,
the base branch itself (publish from a topic branch), a consumer branch that
changes `flake.lock`, uncommitted changes to tracked files, and a branch on
`origin` that HEAD does not contain (publish never force-pushes; fetch and rebase or merge). So a
manifest never names unpushed commits. Rerunning finds the existing MRs,
leaves a current manifest alone and pushes only new commits. Entries are
added or updated, never removed by `publish`.

`publish` pushes with git and reads and writes the forge through its REST
API with fj's login (see [Forge access](#forge-access)); it never runs
`fj`. Without a usable login (`fj auth login`) it refuses before anything
is pushed. `publish` fetches each checkout's base branch (an
`origin/<branch>` base) and reads the remote branch state with
`ls-remote`; it fetches nothing else. A squash-merged MR is
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

`swl land [WORKSPACE] [--dry-run] [--keep] [--workspace NAME|TICKET|PATH] [--timeout S] [--interval S]`
takes the workspace's change to master, and `swl land --from CONSUMER#MR`
does so for a published change: it runs `swl start --from` first (or reuses
its workspace, `--workspace NAME|TICKET|PATH` or
`<workspaces>/<consumer branch>`) and lands there;
a consumer MR that `swl status --from` shows as landed has nothing left to
do (with `--workspace` selecting an existing workspace, that is removed).
The default `land --from` lookup reuses a uniquely matching full branch-name
workspace, including a differently cased or zero-padded name (not a ticket-prefix
search); ambiguity is an error. This is an existing-operation lookup, unlike `start`'s duplicate-name
refusal.

It is stateless: each run reads the state as `swl status` does (the
checkouts, the manifests, the forge and the mirror), does what it can, and
stops at the first step it cannot do, saying what it waits for and for
whom. Rerun it to continue. The steps:

1. The shared MRs that the consumers' `swl.toml` entries name, nested
   entries (`common/zwanenzang`) first. An open MR whose head does not
   contain its base's tip is rebased onto it and pushed with a lease first,
   as `swl refresh` does (see [Refresh](#refresh)): in a scratch worktree,
   then its workspace checkout is moved to the pushed head; an MR without a
   checkout in the workspace (or, in a `--from` dry run, without a
   workspace) needs none. Its CI is then pending, so its merge is scheduled
   below.
   When the MR is not the change's own (into another branch, from a fork),
   the checkout does not allow it (changes or untracked files, another
   branch, a HEAD other than the remote branch's), the branch has merge
   commits, or the rebase conflicts, it waits for anyone, with the reason;
   nothing is merged. A draft (`WIP:`
   title) is marked ready (its title without the prefix); a draft without
   that prefix is for anyone to mark ready on the forge. Then it is merged
   with Forgejo's API,
   `POST /repos/{owner}/{repo}/pulls/{n}/merge` with `{"Do":
   "fast-forward-only", "delete_branch_after_merge": true,
   "head_commit_id": <the head read>}`, after a fresh read of the head's
   combined commit status:
   - green (`success`): a plain merge, now. Forgejo deletes a pending
     schedule when it merges, and a stale one on a merged MR does nothing
     (relied on, not verified against the real forge);
   - pending, no status yet, or unreadable: the same request with
     `"merge_when_checks_succeed": true`. Scheduled (201, or 409 "already
     scheduled"): it waits for CI, then merges; nobody waits, rerun. When
     the status reads green right after scheduling, a direct merge is
     attempted in the same run;
   - failing: fix CI first, for anyone.

   `head_commit_id` makes Forgejo refuse a direct merge of a head that
   moved (409, rerun). A scheduled merge is different: Forgejo merges the
   head that gets the success event, which need not be the head read.

   No merge is requested, now or scheduled, for a head that cannot be a
   fast-forward: first the base branch is fetched into the repo's source
   (`+refs/heads/<branch>:refs/remotes/<remote>/<branch>`, and the MR's
   pull ref when its head is not there), and a head that does not contain
   the base's tip is not mergeable by fast-forward, as below. Forgejo
   (17) answers a fast-forward-only merge that it cannot do with HTTP 500
   and an empty message; after a 500 the base is fetched again, and a head
   that does not contain it is not mergeable by fast-forward. Any other 500
   is an error. When the ancestry cannot be told (a repo not in the
   config, a base without a remote), the merge is requested as before.

   Forgejo (17.0.1) starts a scheduled merge only on a later commit status
   event: scheduled while the checks are green already, it answers
   "scheduled" and never merges. So `land` merges a green MR directly, and
   never says it waits for CI while the status is green. A direct merge
   that is refused never loses a schedule: the schedule is kept, or
   requested when the run has not made one, and the 405 is told by its
   reason (`not allowed to merge [reason: …]`, as Forgejo's source words
   it):
   - a required status check that has not passed (one green so far covers
     only the statuses reported): it waits for nobody, rerun;
   - approvals missing, changes or official reviews requested: it waits for
     the owner, `approve <repo>#N on the forge`;
   - the head behind the base: `rebase <branch> onto <base>: rerun swl
     land, or swl refresh`, for anyone, as for a 409 that is not a
     fast-forward (the base moved after it was read);
   - anything else, such as `User not allowed to merge PR` (the merge
     whitelist, permissions): it waits for the owner, `merge <repo>#N`.
     When scheduling is refused too, that is only reported.

   An MR merged by hand (or by its schedule) meanwhile is merged: on to the
   next step. A closed MR: reopen it, or drop the entry. A shared checkout
   that is not published (local, needs publish) stops with its `status`
   step.
2. Promotion, for a merged MR whose input the consumer's `flake.nix`
   declares at a branch other than the repo's base branch (common's
   `swan-lake-current`; zwanenzang, pinned at master, needs none): when
   that branch on the forge is an ancestor of the merge commit,
   `git push origin <merge commit>:refs/heads/<branch>` from the repo's
   source, never forced, after listing each commit it moves the branch over
   (`<rev> <subject>`; also earlier commits of master not promoted yet);
   skipped when it has the commit. A refused push (a
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
   Before finalizing, a consumer branch at its remote head that does not
   contain its base's tip is rebased as a shared MR is (a landing commit not
   pushed yet is pushed first, and rebased in the next pass); swl's own landing
   commit at its tip is made anew when the base moved `flake.lock` (see
   [Refresh](#refresh)).
   The consumer MR (found by the pull ref at the branch head) must be open,
   for that branch, and at the remote branch head; else it stops for anyone
   to check it on the forge. A draft is marked ready. A consumer without
   entries just has its merge requested. An obsolete consumer MR (its change
   is in its base already, see [Status](#status)) is not rebased, finalized
   or merged: `land` stops there, for anyone, `<consumer>#<MR>: obsolete:
   its change is in <base>; close it` (then remove the workspace, `swl ws
   remove`). It only reports it: whoever
   opened the MR, or the owner, closes it (`swl bump` closes its own, see
   [Bump](#bump)).

   Common as a consumer (a common-only change), once its MR merged, is
   promoted as in step 2, in its own consumer step: like any consumer step,
   in the order of the consumers (by name), so only once those before it
   are done. Its merge commit is pushed to the branch that common's
   consumers declare (`swan-lake-current`), and their declared sources (the
   GitHub mirror) are waited for. The branch and the sources are those that
   `swl bump` promotes to (see [Bump](#bump)): one declared branch; when
   they cannot be read (none or several branches declared, or a fetch
   failed) it stops, waiting for nobody.
   Earlier commits of master that were not promoted go along; each commit
   that the push moves the branch over is listed first (`<rev> <subject>`),
   as for any promotion. After the push it prints `next: swl bump`, to move
   the consumers to it; it does not run `bump`. `land --from
   swan-lake-common#N` of such a merged MR promotes it too, without a
   workspace (in the one `--workspace DIR` names, when that exists), and
   says it has landed only once promoted and mirrored.
4. When every consumer MR is merged, the workspace is removed, unless
   `--keep`: each checkout is removed from its source with `git worktree
   remove`, after its now merged local branch is deleted with `git branch
   -d` (the checkout is detached at the base first). A checkout is kept,
   and said so, with uncommitted, untracked or ignored files (other than
   the generated `.envrc.local`, `.direnv/`, `result*` links and `.swl`),
   or with commits in neither the base nor its remote branch. Then `.swl`
   and the directory go. A workspace where no consumer landed through a
   merged MR (its MR merged, also once the merge deleted its branch and a
   fetch pruned it, or its pushed branch, now in the base, the head of a
   merged MR) has nothing to land and stays.

A consumer at its base (`no change`, `not a change`) is skipped, unless the
workspace's branch has an open MR in its repo (see [Status](#status)): then
`land` stops there, for anyone, with `git -C <checkout> switch <branch>,
then swl land` and the `swl land --from` alternative, before anything is
merged or removed. When that lookup (or the one of the consumer's own
branch) fails, `land` stops there too, to rerun, rather than land the rest
and remove the workspace. When every consumer is at its base, the workspace does
not hold a change: `land` refuses, for anyone, saying where each consumer
is, instead of saying it has nothing to land. The hint is not `swl land
--from` alone because it reuses a matching workspace and `start --from` keeps
an existing checkout as it is: into this workspace it would leave the consumer
detached. To join elsewhere, pass `--workspace <new-path>` explicitly.

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

Writes are only those: the title and merge requests through fj's login (see
[Forge access](#forge-access)), the git pushes of the promotion, of the
landing commit and of rebased MR branches (with a lease, the checkouts
moved to them, and `swl env` rerun when a lock moved), the finalize commit,
and the workspace removal; besides, the scratch worktrees of rebases, and
fetches of the base branches and the MRs' branches into the sources. A write
that one run did already and that the forge does not show yet is not
repeated: the run stops, rerun it.

### Refresh

`swl refresh [WORKSPACE] [--dry-run] [--workspace NAME|TICKET|PATH] [--timeout S] [--interval S]`
rebases the change's open MR branches onto their bases' current tips and
pushes them, bottom-up: the shared MRs that the consumers' `swl.toml`
entries name (nested input paths first), then the consumers' own MRs. It
reads the state as `swl status` does; an MR that is not open is not a part
of it. Per MR:

1. The base branch is fetched into the repo's source
   (`+refs/heads/<b>:refs/remotes/<remote>/<b>`), and the MR's head (its
   pull ref, when the source lacks it). A head that contains the base's tip
   is up to date: nothing to do, whatever its branch or repo.
2. Else the MR must be the change's own: into the repo's base branch (the
   forge's `base.ref`), from the repo itself (`head.repo`, not a fork), on
   a branch swl may rewrite (below); and its branch, fetched likewise, must
   be at the forge's head (else it waits for nobody: rerun). It must have
   no merge commits since the base: a rebase would drop them, and what they
   resolve, so it waits for anyone, naming them; rebase it by hand.
3. Its workspace checkout must be clean (`git status --porcelain`: no
   changes and no untracked files but swl's generated `.envrc.local`, as
   for removing a landed checkout), on the MR's branch, and at the remote
   branch head (nothing unpushed, nothing new on the remote). Otherwise it
   waits for anyone, with the reason; nothing is written. A shared MR
   without a checkout in the workspace needs none.
4. In a scratch worktree of the source, detached at the head, `git rebase
   <base tip>` (`--no-autosquash`, `--no-update-refs`). A conflict is
   aborted (`git rebase --abort`) and waits for anyone, naming the repo,
   the commit and the conflicting files; the checkout and the remote branch
   are left as they were. A branch with nothing left after the rebase (all
   of it in the base) waits for anyone: close the MR.
5. `git push --force-with-lease=refs/heads/<branch>:<head read> <remote>
   <new head>:refs/heads/<branch>`. A lease that no longer holds (someone
   pushed meanwhile) waits for nobody: nothing was changed, rerun (which
   then stops for the checkout that lacks the new commits). A push that the
   forge refuses (a protected branch) waits for the owner. The scratch
   worktree, the rebase, a finalize commit and the push run without the
   source's hooks.
6. The checkout is checked again and moved to the pushed head: its branch
   by compare-and-swap from the head read (`git update-ref`), then, when it
   is checked out there, its index and working tree (`git read-tree -m -u`,
   which overwrites no change). When the checkout changed during the run
   (new commits, changes, another branch), it is not moved and it waits for
   anyone, with the commands to do it by hand; the push stays. Without a
   checkout, the source's own local branch follows likewise when it was at
   the head read and no worktree has it; else it is left, with a warning
   (a later push of it would not be a fast-forward). `swl env` runs again
   when `flake.lock` moved, so that the overrides and the dev shell follow
   (a failure is a warning); then it waits up to `--timeout` (default
   300 s) for the forge's pull ref to show the new head. The push reruns
   the MR's CI: `swl status` shows it as `CI pending`, and `swl land`
   schedules its merge.

A consumer branch whose tip is swl's own landing commit, behind a base
whose `flake.lock` moved since the merge base (a `swl bump`), is rebased
without it, and in the same scratch worktree finalized anew (as `swl
finalize` does, on a temporary branch deleted afterwards) before the one
push. So no pushed consumer head lacks a landing: the consumer MR's merge,
scheduled by an earlier `swl land`, could otherwise merge a head that still
has `swl.toml` and the old lock. A commit is swl's landing when it has one
parent, a subject `swl: land `, a change to `swl.toml` and `flake.lock`
only, removes entries from its parent's `swl.toml` and changes none of the
others, and its lock moves nothing but those entries' root inputs (checked
as `finalize` checks its relock). Made anew, it must land exactly the
entries it landed (a partial landing stays partial) and leave the same
`swl.toml`; when it cannot (an entry no longer satisfied, more of them
satisfied now, nix fails), it waits for anyone and nothing is pushed. A
base whose lock did not move keeps the landing as it is. Any other commit,
one that only looks like a landing included, is rebased as it is; a
`flake.lock` conflict there is a conflict as above.

swl never rewrites a base branch (the repo's, `master`, `main`), a
promotion branch (`swan-lake-current`, `swan-lake-current/…`) or
`swl-bump-common`: those are never force-pushed. The change's own MR
branches are force-pushed only so, with a lease on the head read, by
`swl refresh` and `swl land`; `swl bump` rebuilds its own branch likewise.
Pushing an MR branch waits for nobody's permission: anyone (an agent too)
may run `refresh`. It reads the forge as `status` does and writes nothing
to it.

It stops at the first MR it cannot refresh. `--dry-run` reads and fetches
as a run does and prints what it would rebase and push, writing nothing
(no worktree, commit or push). Exit status as for `land`: 0 when every MR
branch contains its base or was pushed (and for a dry run), 3 when it waits
for the owner, 4 for anyone, 5 for nobody, 2 on errors.

### Forge access

swl never runs `fj`: it talks to the forge through its REST API, with curl,
and needs fj only for its login (`fj auth login`).

`status`, `ls` and `start --from` read MR state and commit status (and
`ls` the list of open MRs) anonymously first: public repos are read
without a token unless the read 404s. On a 404 they ask again with the
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
swl has no token configuration of its own. A list of open MRs skips an MR
that swl cannot read (a field missing or of the wrong type), with a
warning naming it (`<repo>#<n>`); a read of one MR fails instead.

`land` reads the same way, and writes with fj's login, which it needs: the
title of a draft (`PATCH /repos/{owner}/{repo}/pulls/{n}`) and merge
requests (`POST /repos/{owner}/{repo}/pulls/{n}/merge`), under the same
token rules (stdin, no redirect, only the forge).
A 403 or 405 to a merge request is a refusal (see [Landing](#landing)), not
a rejected login; a 401 is. `bump` reads and writes as `land` does, and
opens its MRs too (`POST /repos/{owner}/{repo}/pulls`, see [Bump](#bump)).

`check`, `ci env` and `finalize` read MR state anonymously only, also with
an fj login (`GET /api/v1/repos/binarin/<repo>/pulls/<n>`, through curl),
and CI fetches
`refs/pull/<n>/head` anonymously. So every repo named in a manifest must be
public. Should one become private, give CI a dedicated read-only token as a
secret, through nix `access-tokens` or a netrc for the fetch, and the same for
the API; Forgejo's automatic Actions token is scoped to the job's own repo
and is not enough. `publish` reads as `status` does (the list of open MRs,
and the MR of a branch that is not on `origin`), and needs fj's login: it
opens MRs (`POST /repos/{owner}/{repo}/pulls`) under the same token rules
as `land`.

`SWL_FORGE_API` (default `https://forgejo.lynx-lizard.ts.net/api/v1`) and
`SWL_FORGE_GIT` (default `https://forgejo.lynx-lizard.ts.net`, the base of
the override URLs) redirect them, for tests.

## Bump

`swl bump [--dry-run] [--check] [--timeout S] [--interval S]` moves
common's inputs forward to their declared branches, and then each
consumer's `common` to that. It replaced `scripts/relock-chain.py`. Like
`land --from` it needs no workspace: it works in scratch worktrees of the
repos' configured sources (see [Config](#config)), detached at the heads it
fetched, and removes them. The consumers are the config's repos whose base
branch locks common: a root input of `swan-lake-common` by both its
sources, other than the experiment inputs `common-next…`. The branch they
declare for it (`swan-lake-current`) is the one promoted, and their declared
source (the GitHub mirror) the one waited for; they must all declare the
same branch.

1. Common, at its base (`origin/master`): for each root input, the head of
   the ref its lock node's `original` names (the forks' `swan-lake-current`,
   `swan-lake-current/nixos-26.05`, …; the default branch without a ref, as
   for zwanenzang and emacs-niri-awareness), by `git ls-remote`. An input
   whose head descends from the locked rev moves there. Ancestry is asked
   as `swl check` asks it: a commit-only fetch for git sources, GitHub's
   compare API for GitHub ones (`SWL_GITHUB_TOKEN` as there). An input whose
   head does not descend from the locked rev (a rebased fork), or that
   cannot be judged (a rev in `flake.nix`, a source that is neither git nor
   GitHub, offline, rate limited), is not bumped: it is reported with its
   old and new revs, left at its rev, and the others still move. The moves
   are locked as `swl check` relocks (`nix flake lock` from the old lock
   without them, each overridden to its new rev), must change nothing else
   in the lock, are checked with `nix flake check --no-build` with
   `--check`, and committed: `swl: bump nixpkgs, zwanenzang`, with the revs
   in the body (and the inputs left). The commit is pushed to master
   directly, fast-forward only.
2. Promotion, as in `land`: `git push origin <rev>:refs/heads/swan-lake-current`
   from common's source when the branch is an ancestor of master, never
   forced; then the declared source must have it, polled every
   `--interval` (default 10 s) for up to `--timeout` (default 300 s).
3. Each consumer whose common is behind the promoted rev: a bump MR that
   moves only its common inputs there, a forward move (rule (ii)) that its
   `swl check` accepts. On the consumer's base, its common inputs are
   relocked as above, checked as `finalize` checks a landing (nothing else
   changes, the pin policy) and with `--check`, committed as `swl: bump
   common to <rev>`, and pushed to the branch `swl-bump-common`. The MR is
   opened with Forgejo's API (`POST /repos/{owner}/{repo}/pulls`, fj's
   login), titled like the commit, and merged as `land` merges: directly
   when its checks are green, scheduled when they are pending, a refusal
   told by its reason. The consumer's other inputs are not touched. A
   consumer that stops, or fails (a relock, the pin policy, the forge),
   does not stop the others; a failure is reported at the end. A consumer whose common is
   not behind the promoted rev, but ahead or diverged, is reported and
   left.

Anyone may run `bump`; it does every step it can and stops at the first
one the forge refuses. The owner's steps are the push to common's master,
the promotion and the consumers' merges (and closing a bump MR that an
agent may not close): like `land`, `bump` tries them,
and a push that branch protection refuses, or a merge refused as `land`
tells (`User not allowed to merge PR`, approvals), waits for the owner,
with the command. For a refused push to master that is `git -C <source>
push origin <rev>:refs/heads/master`, the commit kept unreferenced in the
source's objects, or rerun `swl bump` as the owner (which makes it anew).
So common's master and `swan-lake-current` must be protected against the
agents' pushes for that boundary to hold. The exit status is `land`'s: 0
when everything is bumped or there is nothing to bump (and for a dry run),
3 when a step waits for the owner, 4 for anyone, 5 for nobody, 2 on errors
(also when one consumer failed and the others went on); with several
consumers stopping, the most urgent of those, in that order.

It is stateless and rerunnable: each run reads common's master and
promotion branch, the locks, the bump branches, the forge's MRs and the
mirror, and goes on from there.
- The promotion branch behind master by bump's own commits only: no new
  bump, those are promoted. A commit counts as bump's by its form alone:
  one parent, a change to `flake.lock` only, and a subject starting `swl:
  bump `. So a landed MR whose single commit has that form counts too
  (it is on master, so the owner pushed or merged it).
- Behind master by any other commit (an unpromoted landing): it stops for
  the owner and writes nothing, listing those commits; it does not promote
  that landing as a side effect. Promote it with `swl promote <master rev>`
  (as the message says; see [Promote](#promote)), or wait for its `swl
  land`, then rerun.
  The promotion branch with commits that master lacks stops for the owner
  too.
- Common bumped and promoted, or nothing to bump: on to the consumers. A
  consumer at the promoted rev is done; one with an open bump MR at the
  right head goes to its merge.
- A consumer with nothing to bump whose `swl-bump-common` is still there
  (its change landed by another MR, as nixos-config#377 duplicated #376, or
  it is in the base): when the branch is swl's own (the rules below, as for
  rebuilding it) and moves common no further than the base locks it (each
  common input at the base's rev or one of its ancestors), its open bump
  MR is closed (`PATCH /repos/{owner}/{repo}/pulls/{n}` with `{"state":
  "closed"}`, fj's login), then the branch is deleted, `git push
  --force-with-lease=refs/heads/swl-bump-common:<head read> <remote>
  --delete refs/heads/swl-bump-common` from the consumer's source; without
  an open MR it is only deleted. Only the consumer's own bump MR is
  closed: an open MR at the branch's head into another branch, or from
  another repo, waits for anyone, and nothing is closed or deleted. A
  branch with anything else on it is left
  and reported (`… is not needed any more, but it has commits other than
  swl bump's own; left as it is`). One that moves common past this run's
  rev may be another run's newer bump, pushed meanwhile: it is left too,
  and waits for nobody (`… moves lib to <rev>, past <target>, which this
  run bumps to: another run's newer bump, maybe; left as it is; rerun swl
  bump`). When reruns do not clear it (the promotion branch will never
  reach that rev), delete `swl-bump-common` on the forge. A close that the forge refuses (403:
  the agent may not edit the MR) waits for the owner, to close it on the
  forge (its URL is given), then rerun, which deletes the branch; a refused
  deletion (a protected branch) waits for the owner with `git -C <source>
  push --delete <remote> swl-bump-common`; a branch that moved since it
  was read waits for nobody: rerun. When common moved first, the branch is
  needed again: it is rebuilt and its open MR reused, retitled, as below.
- The consumer's base is read again with `git ls-remote` just before the
  bump branch is pushed (after the relock, which takes a while) and just
  before its MR is opened. When it moved since it was read (another bump of
  the same change landed meanwhile, as nixos-config#376 did while a second
  run built #377), nothing is pushed or opened on the old base: the base
  branch is fetched and that consumer's step starts over on it (done when
  the new base has the change, rebuilt on it otherwise), up to three times
  in one run; then it waits for nobody: rerun. The base can still move
  between that read and the push, or after the MR is opened: then the MR is
  rebuilt (or found obsolete and closed) by the next run.
- Nothing new anywhere: `nothing to bump`, and nothing is written.

`--dry-run` reads as a run does, prints the plan and the first write it
would do, and writes nothing on the forge or GitHub: no worktree, commit,
push or forge write, and no fj login is needed but for the private
consumers' reads. It does fetch into the configured sources, as a run and
`land --dry-run` do (their remote-tracking refs, and the bump branch's
objects), and asks `ls-remote`, the compare API and the forge.

The branch `swl-bump-common` is swl's own. A run updates it only while it
has nothing but swl's bump commit: one commit (one parent, only
`flake.lock`, a subject `swl: bump common to `) on a commit of the
consumer's base branch whose lock change is below the common inputs only
(nothing else in the lock moves, as `finalize` checks a landing), or
nothing that branch lacks. When its base or its
rev is stale, it is rebuilt on the current base and pushed with
`--force-with-lease` to the head it read (as `refresh` pushes the change's
MR branches; nothing else is force-pushed), and
its open MR follows, retitled and its description updated (`PATCH
/repos/{owner}/{repo}/pulls/{n}`). Anything else on it stops that consumer for
anyone: delete the branch on the forge, then rerun (an unneeded one is
only reported, above). When the newest bump MR
at the branch's (current) head was closed without merging, no other is
opened for that head: it waits for the owner, to reopen it or delete the
branch.

Limits:
- Every root input of common is judged against its declared ref; there is
  no list of inputs to bump or skip. A fork moves only to what its
  `swan-lake-current…` has: promote the fork first.
- Common has no `swl.toml` yet: nested entries such as `common/zwanenzang`
  are not read. Bump moves inputs to their branches only.
- `--check` evaluates each relocked flake (common and every consumer),
  which takes a lot of memory; it is off by default.
- Each moved GitHub input asks the compare API once (anonymously 60
  requests an hour).
- `swl status` does not show bumps; `bump --dry-run` is the plan.
- An MR is found by the pull ref at the bump branch's head, as `land` finds
  a consumer's, else in the forge's list of open MRs (`GET
  /repos/{owner}/{repo}/pulls?state=open`, by its head branch) before one is
  opened; one that the pull ref does not show at the pushed head yet waits
  for nobody: rerun.

## Promote

`swl promote [<rev>] [--dry-run] [--timeout S] [--interval S]`
fast-forwards common's promotion branch to `<rev>` (default common's
`origin/master`), for what reached master without a consumer (pushed or
merged directly), and then waits for the declared sources. Like `bump` it
needs no workspace and runs from anywhere: it works in common's source from
the config (see [Config](#config)). The branch (`swan-lake-current`) and the
sources (the GitHub mirror) are those that `bump` promotes to (see
[Bump](#bump)): declared by the config's repos that lock common at their
base, one branch, else it refuses.

It fetches common's remote, then:
- refuses (an error) a `<rev>` that is not a commit of `origin/master`'s
  history, or not known;
- when the branch has the rev already, pushes nothing;
- when the branch is an ancestor of the rev, lists each commit it moves the
  branch over (`<rev> <subject>`), then `git push origin
  <rev>:refs/heads/<branch>` from the source, never forced, and prints
  `next: swl bump`, to move the consumers to it (it does not run `bump`). A
  refused push (a protected branch) waits for the owner, with the command,
  as in `land`; a branch that moved since the fetch waits for nobody:
  rerun;
- else (the branch has commits the rev lacks) it refuses, for the owner;
- then, also when nothing was pushed, each declared source must have the
  rev in the branch: polled every `--interval` (default 10 s) for up to
  `--timeout` (default 300 s), as `land`; after that, rerun.

There is no confirmation prompt. `--dry-run` fetches and reads as a run does
and prints the push it would do, writing nothing. Exit status as `bump`'s: 0
when promoted and in the declared sources, also when it was already (and for
a dry run), 3 when it waits for the owner, 5 for nobody (the mirror), 2 on
errors. Only common is promoted: the forks' `swan-lake-current…` branches
are not.

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
  swl; until then the wrappers run the old one, and `status` warns about it.
  A swl run from outside the store (a cargo build) is not rooted.
- The nix option table is finite. The value of an unknown option can be taken
  for an installable, so the shim may then add overrides to a store-path call
  (harmless) or miss the default `.`. Before the subcommand, only words that
  are nix subcommands are taken as one.
- `nix profile`, `nix fmt` and the legacy `nix-*` commands pass through.
- nh's lock-flag refusal applies to any flake, not only to workspace ones.
- Not verified: `nh os switch/boot/test` activation, `--build-host`,
  `--target-host`, and deploy-rs `--remote-build`.
- `publish` opens MRs against the base branch only (no stacked MRs).
- `finalize`'s history fetch is cheap for swan-lake-common, but would fetch
  the whole commit history of a large fork such as nixpkgs.
- `ls` reads the workspaces one after another, each as `status` does, so
  it takes as long as their `status` runs together (less the repeated
  fetches); `--no-fetch` is the quick view. It lists up to 20 pages of 50
  open MRs per repo. Which MRs a workspace holds it tells by its checkouts'
  branches, the workspace's branch in its consumer repos and its
  `swl.toml` entries only: an MR of the workspace's ticket on a repo it
  has no consumer checkout of, on another branch than its checkout's, and
  that no entry names, is listed.
- `land` follows one change in one workspace, step by step: an MR that waits
  for CI stops the run, also when later MRs could proceed. It knows a
  merge's outcome from Forgejo's answer (200 merged, 201 scheduled, 409
  conflict, 403/405 refused, the reasons of a 405, a 500 by git
  ancestry) and messages; a
  scheduled auto-merge that fails shows as the MR still open on the next
  run. These answers, Forgejo's scheduled merge that waits for a status
  event, and the schedule that a direct merge deletes, are tested against
  the fake forge of the tests only, not against Forgejo itself.
- `refresh` (and `land`'s rebase) knows a protected MR branch only from a
  refused push. It refuses branches with merge commits rather than
  linearizing them. It rebases with the user's git identity and signing
  configuration, as a rebase by hand does. A consumer's CI that ran against
  a shared MR's old head is not rerun by the shared MR's push. `status`
  does not show that a branch is behind its base; `refresh --dry-run`
  does.
- `land` tells a landed consumer from the forge's pull refs and its
  remote-tracking branch, or, once the merge deleted the branch, from the
  MR of the branch (`GET /repos/{owner}/{repo}/pulls/{base}/{branch}`): a
  topic branch at the base whose name an older, merged MR had, with its
  merge commit in HEAD, reads as landed too.
- `status`, `land` and `publish` look up the MR of a branch with `GET
  /repos/{owner}/{repo}/pulls/{base}/{branch}` (percent-encoded), which
  Forgejo answers with any one MR of that head and base: for a branch name
  used again, possibly an older one. The tests' fake forge answers with the
  highest-numbered, so which one Forgejo picks is not tested. A base branch
  with a `/` is sent as `%2F`, also not tested against Forgejo: should its
  router not decode it, the lookup finds no MR.
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
  MR opened by hand without one shows as `needs publish`. It tells an
  obsolete MR (its change in its base already) for consumer MRs only; an
  obsolete shared MR shows as open, and `land` (or `refresh`) stops at it
  when its rebase leaves nothing: close it. A consumer's MR is
  the pull ref at its branch head, so a push that Forgejo has not recorded
  yet hides it for a moment.
- `status --from` reads the declared branch of a nested entry
  (`common/zwanenzang`) from its parent entry's MR; without a parent entry
  the promotion is `unknown`.
- `start` keeps an existing checkout as it is, also on another branch; it
  says so, and switching is left to you.

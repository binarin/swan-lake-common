# Home links into mutable checkouts

Import `common.homeModules.local-checkout-links` persistently into Home Manager
(or NixOS `home-manager.sharedModules`), then declare:

```nix
home.localCheckoutLinks = {
  checkouts.project = "/home/alice/src/project"; # runtime string, or null
  files.".config/app/settings.json" = {
    checkout = "project";
    source = "config/settings.json";
    fallback = ./settings.json;
    # type = "file"; # default; "directory" recursively merges leaf links
  };
};
```

Targets/sources must be normalized relative paths. Roots are absolute strings,
without Nix context or store paths. Fallbacks are required immutable sources;
there is no checkout probe or copy during evaluation. Fallback inventory/type
validation runs at build time, rejecting dangling links anywhere in the fallback tree. Enabled targets must not overlap each other,
`home.file` (including translated `xdg.configFile`), or the ownership state subtree.
HM target aliases containing dot/dotdot or repeated slashes are normalized
lexically for ownership checks, without reading mutable home contents.

Activation selects local files when present, fallback for missing children,
and the recursive union for directories. Real directories and unrelated entries
remain in place; directories are never linked as a unit. Local-only leaves are
retired when removed. Supported leaves are regular files, including symlinks
resolving to regular files. Dangling local links count as missing; directory
symlinks, special files, present type mismatches and source/destination overlap
fail before writes. Permission/I/O errors fail rather than selecting fallback.

Run the same generation's activation again to discover checkout disappearance,
reappearance or new children. Local content edits are visible immediately through
links; removing a source can leave a dangling link until activation is repeated.
This module does not integrate its leaves with Home Manager `onChange` tracking.

Exact correct link text is idempotent. All conflicts, including obstructing
parents and different/dangling symlinks, go through pinned `trash-cli`. Both a
successful exit and disappearance of the original entry are required. Failure
stops without installing a replacement or deleting a backup. Nonzero after a
move leaves the backup in trash. A failed install leaves any displaced content
in trash; restore manually if desired, or repeat activation to converge.

`checkLocalCheckoutLinks` validates read-only before `writeBoundary`.
`reconcileLocalCheckoutLinks` recomputes after `linkGeneration` and before
`onFilesChange`. Custom hooks can depend on the latter node. Presence of
`DRY_RUN`, even empty, prints plans without writing home, state or trash.
Upstream HM may initialize unrelated Nix profile metadata on a first dry run.

State lives in `$XDG_STATE_HOME/home-manager/local-checkout-links`. It stores
only a version, previous home-relative leaf paths and their exact installed link
text, plus at most one pending change (`path`, `old`, `new`). It contains no file
contents or secrets. The immutable generation cannot remember which local-only
children a mutable directory had at an earlier activation, including a rerun of
the same generation. Without these records, removing a declaration or a local-only
child would either leave stale links forever or require guessing which entries
belong to the module. Exact link text lets cleanup preserve user replacements.

The pending record handles interruption between installing a link and committing
its ownership: retry compares the actual link with old/new text, then accounts
for the operation safely. An atomic ledger update and exclusive lock prevent
partial records and competing reconciliations. The state directory must belong
to the current user and deny group/other access; symlinked state ancestors are
rejected. These private permissions protect ownership **integrity**, not secret
configuration data: another user's writable record must never authorize cleanup
of your files.

Source and destination identity checks and no-follow parent descriptors catch
observed changes and stop. Expected first-use trash directory creation is accepted
only after checking the watched ancestor and its other entries; source watches
are retained. This is per-path recovery, not an all-files transaction, filesystem
snapshot or protection against malicious concurrent home edits. Back up valuable
content independently.

Disable/remove a declaration and activate while keeping this module imported.
A target can be handed back to `home.file` in the same generation that removes
its checkout declaration. Overlapping stale ledger entries and pending operations
are forgotten without touching those paths: HM performs its normal collision
checks, backup and linking, and checkout cleanup must not unlink HM's new files.
This includes ancestor/descendant overlaps and normalized target aliases. Active
checkout declarations still cannot overlap `home.file`. A check or dry run only
reports the ownership release; it does not change the ledger. Ordinary HM backup
requirements still apply to conflicting content. In particular, HM rejects a
non-HM symlink with differing content before invoking its backup command; retire
that link in an intermediate activation or move it to trash before handback.
A two-generation handback also remains supported. To uninstall this module, first activate empty declarations,
then remove the import. Rollbacks to generations predating the import cannot
clean custom links automatically.

The feature is contained here: `default.nix` exports the reusable HM module
and default checks; `_hm`, `_runtime` and `_tests` hold support files that
import-tree excludes from automatic flake-module discovery.

`nix flake check` runs runtime and module contract checks by default, including
real pinned trash, alias ownership and fallback validation regressions. It does
not schedule the full machine test. Run that test explicitly on a capable builder:

```sh
nix build .#legacyPackages.x86_64-linux.local-checkout-links-activation
```

It uses pinned NixOS systemd-nspawn **containers**, an unprivileged user and
disposable HOME/trash. Sandboxed container builds require daemon `uid-range`,
`auto-allocate-uids` and `cgroups` support. On a host without these builder
features, build and run the exact same test through its root driver:

```sh
nix build .#legacyPackages.x86_64-linux.local-checkout-links-activation.driver -o /tmp/checkout-links-driver
mkdir -p /tmp/checkout-links-run/output
cd /tmp/checkout-links-run
sudo /tmp/checkout-links-driver/bin/nixos-test-driver -o output
```

No host/runner setting change is required by this module. Default check success
does not claim that the opt-in machine test ran; report its result separately.

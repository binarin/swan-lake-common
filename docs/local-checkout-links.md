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
validation runs at build time. Enabled targets must not overlap each other,
`home.file` (including `xdg.configFile`), or the ownership state subtree.

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

Private state lives in `$XDG_STATE_HOME/home-manager/local-checkout-links`.
A versioned exact-link ledger with a pending-operation journal allows retries
following interruption. Stale cleanup unlinks only exact recorded symlinks;
user replacements survive, and all real directories remain. An exclusive lock
prevents concurrent reconciliations. Source and destination identity checks and
no-follow parent descriptors catch observed changes and stop; this is per-path
recovery, not an all-files transaction, filesystem snapshot or protection against
malicious concurrent home edits. Back up valuable content independently.

Disable/remove a declaration and activate while keeping this module imported.
To hand a target back to `home.file`, first activate its removal, then declare
`home.file` in a second generation. A one-step handback is rejected to protect
new HM ownership. To uninstall this module, first activate empty declarations,
then remove the import. Rollbacks to generations predating the import cannot
clean custom links automatically.

Run `nix flake check`, or build the named runtime, module and activation checks:
`checks.x86_64-linux.local-checkout-links{,-module,-activation}`. The activation
check uses pinned NixOS systemd-nspawn **containers**, with an unprivileged user
and disposable HOME/trash. Container builds require daemon `uid-range`,
`auto-allocate-uids` and `cgroups` support. Alternatively build the activation
check's `.driver` and run `sudo <driver>/bin/nixos-test-driver` from a temporary
directory; it runs the same test script without changing host Nix settings.

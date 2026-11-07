# Remote-session desktop launchers

## Overview

`homeModules.remote-session-launchers` is a Home Manager module that writes one
`~/.local/share/applications/remote-session-<id>.desktop` entry per item. An
entry focuses the niri window with the item's exact title, or else opens foot
and connects with ssh or mosh to one remote target: a login shell, tmux, herdr,
a single program, or `nix run`. An optional tag supplies a background color and
title decoration; the item's own settings beat its tag. Items never inherit
from each other or from per-host defaults.

Host lists stay in consumers. Nothing in common uses the module yet.

## Example

```nix
{
  imports = [ inputs.swan-lake-common.homeModules.remote-session-launchers ];
  programs.remoteSessionLaunchers = {
    enable = true;
    uwsm.enable = true; # when the session runs under uwsm
    tags.dev = { background = "001800"; titleSuffix = " [dev]"; };
    tags.prod = { background = "180000"; titlePrefix = "[PROD] "; };
    items = {
      box-tmux = {
        name = "box (tmux)";
        host = "me@box.example.com";
        protocol = "mosh";
        remote.tmux.session = "main";
        tag = "dev";
      };
      prod-btop = {
        name = "prod btop";
        host = "prod.example.net";
        protocol = "ssh";
        protocolArgs = [ "-p" "2222" ];
        remote.exec = [ "btop" ];
        tag = "prod";
        background = "200000"; # beats the tag
      };
      box-hello = {
        name = "box: nix run hello";
        host = "box.example.com";
        protocol = "ssh";
        remote.nixRun = { package = "hello"; args = [ "--greeting" "hi there" ]; };
      };
    };
  };
}
```

`remote` takes exactly one of `shell = { }`, `tmux = { session; detachOthers ?
true; }` (`tmux -u new-session -A [-D] -s <session>`), `herdr = { }`, `exec = [
argv ]` or `nixRun = { package; args ? [ ]; flake ? null; }`. `nixRun` runs
`nix run <flake>#<package> -- <args>`; `flake` defaults to `nixRunFlake`
(`github:binarin/swan-lake-common/swan-lake-current`). The remote host provides
tmux, herdr, nix and mosh-server.

## Titles, tags and focus

The final title is `prefix + title + suffix`, where `title` defaults to the item
id. Each of `background`, `titlePrefix` and `titleSuffix` comes from the item
when non-null, else from its tag. The title is the window's identity: foot gets
`main.locked-title=yes`, and final titles must be unique. Renaming a title or a
tag decoration starts a new window instead of focusing the old one.

With `uwsm.enable`, foot starts as `uwsm app -a remote-session-<id> -- foot ...`
unless the `uwsm.wrappedMarker` variable (`_UWSM_WRAPPED`) is present. A launcher
such as fuzzel can set it via `uwsm app -p Environment=_UWSM_WRAPPED=1 --` to
avoid nesting uwsm units.

## Quoting

`Exec=` names a per-item wrapper script, so desktop-entry quoting never applies.
ssh receives the remote argv as one word quoted for a POSIX shell, because the
remote login shell parses it. mosh receives separate words and quotes them
itself. Either way, the remote login shell must accept POSIX single quoting:
bash (as sh), dash and zsh are tested. fish misreads arguments containing `\\`
or `\'`, so avoid such arguments for hosts where fish is the login shell.

## Checks

`nix flake check` runs:

- `remote-session-launchers-golden`: entries and wrappers for `fixture.nix`
  compared with `golden/` (all under `home-modules/remote-session-launchers/tests/`;
  the regeneration command is in `default.nix` there);
- `remote-session-launchers-desktop-file-validate`;
- `remote-session-launchers-negative`: invalid ids, hosts, names, colors, tags,
  duplicate titles and malformed `remote` values must fail;
- `remote-session-launchers-chain`: every entry is run against fake ssh, foot,
  niri and uwsm (plus the real mosh client and jq), with awkward arguments and
  bash, dash and zsh as remote shells, and checks the argv that reaches the remote
  program; it also checks the exact-title focus and uwsm marker handling.

Not covered: real foot rendering, real niri focus, real ssh or mosh
connections and remote `nix run`.

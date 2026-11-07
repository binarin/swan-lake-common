{
  config,
  lib,
  pkgs,
  ...
}:
let
  cfg = config.programs.remoteSessionLaunchers;
  inherit (lib) mkOption types;

  color = types.strMatching "[0-9A-Fa-f]{6}";
  noControl = s: builtins.match "[^[:cntrl:]]*" s != null;
  validId = id: builtins.match "[A-Za-z0-9][A-Za-z0-9._-]*" id != null;
  validHost = h: builtins.match "[^-[:space:][:cntrl:]][^[:space:][:cntrl:]]*" h != null;

  tagOptions = {
    background = mkOption {
      type = types.nullOr color;
      default = null;
      description = "foot `colors-dark.background` for items with this tag.";
    };
    titlePrefix = mkOption {
      type = types.str;
      default = "";
      description = "Prepended to the base title of items with this tag.";
    };
    titleSuffix = mkOption {
      type = types.str;
      default = "";
      description = "Appended to the base title of items with this tag.";
    };
  };

  emptyVariant = mkOption { type = types.submodule { }; };

  remoteType = types.attrTag {
    shell = emptyVariant // {
      description = "Remote login shell.";
    };
    tmux = mkOption {
      description = "Attach to or create a remote tmux session.";
      type = types.submodule {
        options.session = mkOption { type = types.nonEmptyStr; };
        options.detachOthers = mkOption {
          type = types.bool;
          default = true;
        };
      };
    };
    herdr = emptyVariant // {
      description = "Plain `herdr`, installed on the remote host.";
    };
    exec = mkOption {
      type = types.nonEmptyListOf types.str;
      description = "Remote program argv, e.g. `[ \"btop\" ]`.";
    };
    nixRun = mkOption {
      description = "`nix run <flake>#<package> -- <args>` on the remote host.";
      type = types.submodule {
        options.package = mkOption { type = types.nonEmptyStr; };
        options.args = mkOption {
          type = types.listOf types.str;
          default = [ ];
        };
        options.flake = mkOption {
          type = types.nullOr types.nonEmptyStr;
          default = null;
          description = "Overrides `nixRunFlake` for this item.";
        };
      };
    };
  };

  itemModule =
    { name, ... }:
    {
      options = {
        name = mkOption {
          type = types.str;
          description = "Desktop entry `Name=`.";
        };
        host = mkOption {
          type = types.str;
          description = "Destination given verbatim to ssh/mosh (user@host or ssh_config alias).";
        };
        protocol = mkOption {
          type = types.enum [
            "ssh"
            "mosh"
          ];
        };
        protocolArgs = mkOption {
          type = types.listOf types.str;
          default = [ ];
          description = "Extra ssh or mosh options, placed before `--`.";
        };
        terminal = mkOption {
          type = types.enum [ "foot" ];
          default = "foot";
        };
        remote = mkOption {
          type = remoteType;
          description = "Exactly one remote target.";
        };
        tag = mkOption {
          type = types.nullOr types.str;
          default = null;
        };
        title = mkOption {
          type = types.str;
          default = name;
          description = "Base window title; prefix and suffix are added around it.";
        };
        background = mkOption {
          type = types.nullOr color;
          default = null;
          description = "Beats the tag's background.";
        };
        titlePrefix = mkOption {
          type = types.nullOr types.str;
          default = null;
          description = "Beats the tag's prefix when non-null.";
        };
        titleSuffix = mkOption {
          type = types.nullOr types.str;
          default = null;
          description = "Beats the tag's suffix when non-null.";
        };
      };
    };

  resolve =
    id: item:
    let
      tag = if item.tag != null then cfg.tags.${item.tag} or { } else { };
      pick = f: neutral: if item.${f} != null then item.${f} else tag.${f} or neutral;
    in
    {
      inherit id item;
      background = pick "background" null;
      title = pick "titlePrefix" "" + item.title + pick "titleSuffix" "";
    };

  resolved = lib.mapAttrs resolve cfg.items;

  remoteArgv =
    remote:
    if remote ? shell then
      [ ]
    else if remote ? tmux then
      [
        "tmux"
        "-u"
        "new-session"
        "-A"
      ]
      ++ lib.optional remote.tmux.detachOthers "-D"
      ++ [
        "-s"
        remote.tmux.session
      ]
    else if remote ? herdr then
      [ "herdr" ]
    else if remote ? exec then
      remote.exec
    else
      let
        n = remote.nixRun;
        flake = if n.flake != null then n.flake else cfg.nixRunFlake;
      in
      [
        "nix"
        "run"
        "${flake}#${n.package}"
        "--"
      ]
      ++ n.args;

  # ssh joins the words after the host and hands them to the remote login
  # shell, so the argv becomes one pre-quoted word. mosh quotes each word
  # itself and preserves argv boundaries.
  transport =
    item:
    let
      argv = remoteArgv item.remote;
      ssh = "${cfg.packages.openssh}/bin/ssh";
    in
    if item.protocol == "ssh" then
      [
        ssh
        "-t"
      ]
      ++ item.protocolArgs
      ++ [
        "--"
        item.host
      ]
      ++ lib.optional (argv != [ ]) (lib.escapeShellArgs argv)
    else
      [
        "${cfg.packages.mosh}/bin/mosh"
        "--ssh=${ssh}"
      ]
      ++ item.protocolArgs
      ++ [
        "--"
        item.host
      ]
      ++ argv;

  focusHelper = pkgs.writeShellApplication {
    name = "remote-session-focus";
    text =
      let
        niri = lib.escapeShellArg "${cfg.packages.niri}/bin/niri";
        jq = lib.escapeShellArg "${cfg.packages.jq}/bin/jq";
        foot = lib.escapeShellArg "${cfg.packages.foot}/bin/foot";
        uwsm = lib.escapeShellArg "${cfg.packages.uwsm}/bin/uwsm";
      in
      ''
        if [ "$#" -lt 3 ] || [ "$3" != -- ]; then
          echo "usage: remote-session-focus TITLE APP-NAME -- FOOT-ARGS..." >&2
          exit 64
        fi
        title=$1
        app=$2
        shift 3

        # Exact title match only; a missing or failing niri starts a new window.
        id=$(${niri} msg --json windows 2>/dev/null \
          | ${jq} -r --arg t "$title" 'first(.[] | select(.title == $t) | .id) // empty' 2>/dev/null) || id=
        if [ -n "$id" ] && ${niri} msg action focus-window --id "$id"; then
          exit 0
        fi
      ''
      + (
        if cfg.uwsm.enable then
          ''
            if [ -z "''${${cfg.uwsm.wrappedMarker}+x}" ]; then
              exec ${uwsm} app -a "$app" -- ${foot} --title "$title" "$@"
            fi
            exec ${foot} --title "$title" "$@"
          ''
        else
          ''
            : "$app"
            exec ${foot} --title "$title" "$@"
          ''
      );
  };

  wrapper =
    r:
    let
      words = map lib.escapeShellArgs (
        [
          [
            "${focusHelper}/bin/remote-session-focus"
            r.title
            "remote-session-${r.id}"
            "--"
          ]
          [
            "-o"
            "main.locked-title=yes"
          ]
        ]
        ++ lib.optional (r.background != null) [
          "-o"
          "colors-dark.background=${r.background}"
        ]
        ++ [ ([ "-e" ] ++ transport r.item) ]
      );
    in
    pkgs.writeShellScript "remote-session-${if validId r.id then r.id else "invalid"}" ''
      exec ${lib.concatStringsSep " \\\n  " words}
    '';

  desktopString = lib.replaceStrings [ "\\" ] [ "\\\\" ];

  desktopEntry = r: ''
    [Desktop Entry]
    Type=Application
    Name=${desktopString r.item.name}
    Exec=${wrapper r}
    Terminal=false
    Categories=System;
    Icon=foot
  '';

  titles = lib.groupBy (r: r.title) (lib.attrValues resolved);

  itemAssertions =
    r:
    let
      at = "programs.remoteSessionLaunchers.items.${builtins.toJSON r.id}";
    in
    [
      {
        assertion = validId r.id;
        message = "${at}: item id must match [A-Za-z0-9][A-Za-z0-9._-]*.";
      }
      {
        assertion = validHost r.item.host;
        message = "${at}.host must not be empty, start with '-' or contain whitespace or control characters.";
      }
      {
        assertion = noControl r.item.name;
        message = "${at}.name must not contain control characters.";
      }
      {
        assertion = noControl r.title;
        message = "${at}: final title must not contain control characters.";
      }
      {
        assertion = r.item.tag == null || cfg.tags ? ${r.item.tag};
        message = "${at}.tag refers to unknown tag ${builtins.toJSON r.item.tag}.";
      }
    ];
in
{
  key = "swan-lake-common.modules.home.remote-session-launchers";

  options.programs.remoteSessionLaunchers = {
    enable = lib.mkEnableOption "remote-session desktop launchers";

    nixRunFlake = mkOption {
      type = types.nonEmptyStr;
      default = "github:binarin/swan-lake-common/swan-lake-current";
      description = "Default flake reference that `nixRun` items prepend to their package.";
    };

    uwsm.enable = lib.mkEnableOption "starting foot through `uwsm app` unless already inside a uwsm-wrapped launch";
    uwsm.wrappedMarker = mkOption {
      type = types.strMatching "[A-Za-z_][A-Za-z0-9_]*";
      default = "_UWSM_WRAPPED";
      description = "Environment variable whose presence means the launch already runs inside a uwsm unit.";
    };

    packages = lib.genAttrs [ "foot" "niri" "jq" "openssh" "mosh" "uwsm" ] (
      p:
      mkOption {
        type = types.package;
        default = pkgs.${p};
        defaultText = lib.literalExpression "pkgs.${p}";
      }
    );

    tags = mkOption {
      type = types.attrsOf (types.submodule { options = tagOptions; });
      default = { };
      description = "Environment tags; an item's own settings beat its tag.";
    };

    items = mkOption {
      type = types.attrsOf (types.submodule itemModule);
      default = { };
      description = "One launcher per item. Items do not inherit from each other.";
    };
  };

  config = lib.mkIf cfg.enable {
    assertions =
      lib.concatMap itemAssertions (lib.attrValues resolved)
      ++ lib.mapAttrsToList (title: rs: {
        assertion = lib.length rs == 1;
        message = "programs.remoteSessionLaunchers: items ${
          lib.concatMapStringsSep ", " (r: builtins.toJSON r.id) rs
        } share the final title ${builtins.toJSON title}, which must be unique.";
      }) titles;

    xdg.dataFile = lib.mapAttrs' (
      id: r: lib.nameValuePair "applications/remote-session-${id}.desktop" { text = desktopEntry r; }
    ) resolved;
  };
}

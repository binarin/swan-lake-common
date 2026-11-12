{ inputs, self, ... }:
{
  imports = [ inputs.home-manager.flakeModules.home-manager ];

  # Separate herdr-app instances, e.g.
  #
  #   programs.herdr-app.instances.sl = "Swan Lake";
  #
  # installs `herdr-sl`, a "Herdr (Swan Lake)" desktop entry, and keeps that
  # instance's home in ~/.config/herdrs/sl.
  # It also talks to its own herdr-core (HC_SESSION=sl: socket
  # $XDG_RUNTIME_DIR/herdr-core/sl.sock, state ~/.local/state/herdr-core/sl),
  # since spaces, tabs, terminals and machines live in the core, not the app
  # home; HERDR_APP_HOME alone only separates settings/keys/layouts.
  #
  # Look and code are shared: each home's mods/, themes/ and layouts/ is a
  # symlink into one shared directory (~/.config/herdrs/shared by default),
  # and `settings` optionally links every home's settings.json to a single
  # file in a mutable checkout (home.localCheckoutLinks). The rest of a home
  # (view.json, window.json, shared.json, data/, installed.json, the control
  # socket, ...) is per-window state and stays separate; one home is one
  # running window. layouts/.state (splitter sizes, picked panels) lives in
  # layouts/ and so is shared too.
  #
  # `ui.enable` adds the swan-lake UI from ./_ui (mods, a `main-hosts` layout
  # and keys.json; see the option), from the store or, with `ui.checkout`, linked
  # from a mutable swan-lake-common checkout so edits apply live.
  #
  # Instance homes, core state and the shared directory are persisted under
  # impermanence.
  flake.homeModules.herdr-app =
    {
      config,
      lib,
      options,
      pkgs,
      ...
    }:
    let
      cfg = config.programs.herdr-app;
      inherit (lib) mkOption types;

      validName = n: builtins.match "[A-Za-z0-9][A-Za-z0-9._-]*" n != null;
      relHome = name: ".config/herdrs/${name}";
      relCoreState = name: ".local/state/herdr-core/${name}";
      sharedDirs = [
        "mods"
        "themes"
        "layouts"
      ];
      absShared = "${config.home.homeDirectory}/${cfg.sharedDirectory}";
      nested = a: b: a == b || lib.hasPrefix "${a}/" b || lib.hasPrefix "${b}/" a;
      # Run by every wrapper, so the links never dangle. Not an activation
      # step: a newly persisted directory is bind-mounted after Home Manager
      # activation, hiding anything activation created there. A failure is
      # left for the app to report.
      mkShared = "mkdir -p ${lib.escapeShellArgs (map (d: "${absShared}/${d}") sharedDirs)} || true";

      # programs.herdr-app.ui: the files under ./_ui, as home-relative target ->
      # { src (relative to _ui), type }. Mods and the layout go to the shared
      # directory (every instance links to it), keys.json into each instance.
      uiDir = ./_ui;
      uiCheckoutDir = "modules/herdr-app/_ui";
      uiMods = [
        "tmux-prefix"
        "last-tab"
        "host-spaces"
        "host-agents"
        "host-tabs"
        "host-picker"
        "keys-help"
      ];
      uiLinks =
        lib.listToAttrs (
          map (
            m:
            lib.nameValuePair "${cfg.sharedDirectory}/mods/${m}" {
              src = "mods/${m}";
              type = "directory";
            }
          ) uiMods
        )
        // {
          "${cfg.sharedDirectory}/layouts/${uiLayout}.json" = {
            src = "layouts/${uiLayout}.json";
            type = "file";
          };
        }
        // lib.optionalAttrs cfg.ui.keys (
          lib.genAttrs' (lib.attrNames cfg.instances) (
            name:
            lib.nameValuePair "${relHome name}/keys.json" {
              src = "keys.json";
              type = "file";
            }
          )
        );
      uiLayout = "main-hosts";
      hostTagsJson = builtins.toJSON {
        hosts = lib.mapAttrs (
          _: t: if builtins.isString t then { alias = t; } else lib.filterAttrs (_: v: v != null) t
        ) cfg.ui.hostTags;
      };

      mkInstance =
        name: title:
        let
          bin = "herdr-${name}";
          desktop = pkgs.makeDesktopItem {
            name = bin;
            desktopName = "Herdr (${title})";
            comment = "Terminals and coding agents that keep running";
            exec = bin;
            icon = "herdr-app";
            terminal = false;
            categories = [ "Development" ];
            # Matches the Wayland app-id set through HERDR_APP_ID below, so
            # each instance's windows group under its own entry.
            startupWMClass = bin;
          };
        in
        pkgs.runCommand bin
          {
            nativeBuildInputs = [ pkgs.makeWrapper ];
            meta.mainProgram = bin;
          }
          ''
            makeWrapper ${lib.getExe cfg.package} "$out/bin/${bin}" \
              --set HERDR_APP_HOME ${lib.escapeShellArg "${config.home.homeDirectory}/${relHome name}"} \
              --set HERDR_APP_ID ${lib.escapeShellArg bin} \
              --set HC_SESSION ${lib.escapeShellArg name} \
              --run ${lib.escapeShellArg mkShared}
            install -Dm444 ${desktop}/share/applications/${bin}.desktop \
              "$out/share/applications/${bin}.desktop"
            # The entry's icon, for when the plain app is not installed too.
            # Identical content, so no buildEnv collision when it is.
            install -Dm444 ${cfg.package}/share/icons/hicolor/scalable/apps/herdr-app.svg \
              "$out/share/icons/hicolor/scalable/apps/herdr-app.svg"
          '';
    in
    {
      key = "swan-lake-common.modules.home.herdr-app";

      # Backs `settings`; same module path as a direct import, so deduplicated.
      imports = [ self.homeModules.local-checkout-links ];

      options.programs.herdr-app = {
        package = mkOption {
          type = types.package;
          # 0.1.14's capability-based asset loader refuses our shared mods
          # symlink and logs "a path led outside of the filesystem" on every
          # repaint. Backport upstream #427 until the next release is pinned.
          default = pkgs.llm-agents.herdr-app.overrideAttrs (old: {
            patches = (old.patches or [ ]) ++ lib.optional (old.version == "0.1.14") ./linked-assets.patch;
          });
          defaultText = lib.literalExpression "pkgs.llm-agents.herdr-app (with the 0.1.14 linked-assets fix)";
          description = "herdr-app package the instances wrap.";
        };
        instances = mkOption {
          type = types.attrsOf types.nonEmptyStr;
          default = { };
          example = {
            sl = "Swan Lake";
          };
          description = ''
            Instance name to desktop title. Each `<name> = "<Title>"` gets a
            `herdr-<name>` binary with `HERDR_APP_HOME=~/.config/herdrs/<name>`,
            its own herdr-core session (`HC_SESSION=<name>`), and a desktop
            entry titled `Herdr (<Title>)`.
          '';
        };
        sharedDirectory = mkOption {
          type = types.str;
          default = ".config/herdrs/shared";
          description = ''
            Home-relative directory holding the `mods/`, `themes/` and
            `layouts/` every instance home links to. Created by each instance wrapper;
            the app fills in its shipped mods and layouts on start.
          '';
        };
        ui = {
          enable = lib.mkEnableOption ''
            the swan-lake Herdr UI, shared by every instance: a `main-hosts`
            layout (Herdr's main, with host-spaces, host-agents and host-tabs:
            a colored [alias] of each space's and agent's machine, an overview of
            every space's tabs, unread agents, numbers while the prefix is open),
            a Ctrl+O prefix with nested keymaps and repeat maps (tmux-prefix,
            last-tab), a fuzzy go-to (host-picker) and one list of every key
            (keys-help). Switch to it once per instance:
            `herdr-<name> do layout.switch --name main-hosts`
          '';
          checkout = mkOption {
            type = types.nullOr types.str;
            default = null;
            example = "swan-lake-common";
            description = ''
              Name in `home.localCheckoutLinks.checkouts` of a swan-lake-common
              checkout to link the UI files from (edits show at once; a file
              added to a mod needs an activation). `null`: from the store.
            '';
          };
          keys = mkOption {
            type = types.bool;
            default = true;
            description = ''
              Also link the UI's keys.json into every instance: only Ctrl+O and
              the clipboard keys stay outside the prefix, so terminals get
              everything else. Replaces Herdr's default keys.
            '';
          };
          hostTags = mkOption {
            type = types.attrsOf (
              types.either types.nonEmptyStr (
                types.submodule {
                  options = {
                    alias = mkOption {
                      type = types.nonEmptyStr;
                      description = "Shown as [alias].";
                    };
                    color = mkOption {
                      type = types.nullOr types.str;
                      default = null;
                      description = "A theme color name (ansi1..ansi15, accent, ...) or #rrggbb; null: picked from the name.";
                    };
                  };
                }
              )
            );
            default = { };
            example = {
              local = {
                alias = "local";
                color = "ansi2";
              };
              my-long-dev-vm-name = "dev";
            };
            description = ''
              Host tags, written to each instance's host-tags.json. Keys: Herdr's
              machine name (`hc machine ls`), that machine's host, or `local`
              for this computer. A string is only the alias. A machine not listed
              shows its own name.
            '';
          };
        };

        settings = mkOption {
          default = null;
          description = ''
            Link every instance's `settings.json` (fonts, zoom, sounds,
            appearance) to one file in a mutable checkout, through
            `home.localCheckoutLinks.files`. `null` leaves each instance's
            settings its own.
          '';
          example = lib.literalExpression ''
            {
              checkout = "nixos-config";
              source = "files/herdr-app/settings.json";
              fallback = ./settings.json;
            }
          '';
          type = types.nullOr (
            types.submodule {
              options = {
                checkout = mkOption {
                  type = types.str;
                  description = "Name in `home.localCheckoutLinks.checkouts`.";
                };
                source = mkOption {
                  type = types.str;
                  description = "Path of settings.json within the checkout.";
                };
                fallback = mkOption {
                  type = types.path;
                  description = "Immutable settings.json used when the checkout is absent.";
                };
              };
            }
          );
        };
      };

      config = lib.mkIf (cfg.instances != { }) (
        lib.mkMerge [
          {
            assertions =
              lib.mapAttrsToList (name: _: {
                assertion = validName name;
                message = "programs.herdr-app.instances.${name}: name must match [A-Za-z0-9][A-Za-z0-9._-]*";
              }) cfg.instances
              ++ [
                {
                  assertion = lib.all (name: !nested cfg.sharedDirectory (relHome name)) (
                    lib.attrNames cfg.instances
                  );
                  message = "programs.herdr-app.sharedDirectory must not be, contain or sit inside an instance home.";
                }
              ];

            home.packages = lib.mapAttrsToList mkInstance cfg.instances;

            home.file = lib.listToAttrs (
              lib.concatMap (
                name:
                map (dir: {
                  name = "${relHome name}/${dir}";
                  value.source = config.lib.file.mkOutOfStoreSymlink "${absShared}/${dir}";
                }) sharedDirs
              ) (lib.attrNames cfg.instances)
            );

            home.localCheckoutLinks.files = lib.mkIf (cfg.settings != null) (
              lib.genAttrs' (lib.attrNames cfg.instances) (
                name:
                lib.nameValuePair "${relHome name}/settings.json" {
                  inherit (cfg.settings) checkout source fallback;
                }
              )
            );
          }

          (lib.mkIf cfg.ui.enable {
            home.file = lib.mkMerge [
              (lib.genAttrs' (lib.attrNames cfg.instances) (
                name: lib.nameValuePair "${relHome name}/host-tags.json" { text = hostTagsJson; }
              ))
              (lib.mkIf (cfg.ui.checkout == null) (
                lib.mapAttrs (_: l: {
                  source = uiDir + "/${l.src}";
                  recursive = l.type == "directory";
                }) uiLinks
              ))
            ];
            home.localCheckoutLinks.files = lib.mkIf (cfg.ui.checkout != null) (
              lib.mapAttrs (_: l: {
                inherit (l) type;
                checkout = cfg.ui.checkout;
                source = "${uiCheckoutDir}/${l.src}";
                fallback = uiDir + "/${l.src}";
              }) uiLinks
            );
          })

          (lib.optionalAttrs (options ? impermanence) {
            impermanence.persist-directories =
              lib.concatMap (name: [
                (relHome name)
                (relCoreState name)
              ]) (lib.attrNames cfg.instances)
              ++ [ cfg.sharedDirectory ];
          })
        ]
      );
    };

  perSystem =
    { pkgs, ... }:
    {
      checks.herdr-app-instances = import ./_tests.nix {
        inherit pkgs inputs;
        module = self.homeModules.herdr-app;
      };
    };
}

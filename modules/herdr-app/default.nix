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
  # Both directories are persisted under impermanence.
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
              --set HC_SESSION ${lib.escapeShellArg name}
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

      options.programs.herdr-app = {
        package = mkOption {
          type = types.package;
          default = pkgs.llm-agents.herdr-app;
          defaultText = lib.literalExpression "pkgs.llm-agents.herdr-app";
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
      };

      config = lib.mkIf (cfg.instances != { }) (
        lib.mkMerge [
          {
            assertions = lib.mapAttrsToList (name: _: {
              assertion = validName name;
              message = "programs.herdr-app.instances.${name}: name must match [A-Za-z0-9][A-Za-z0-9._-]*";
            }) cfg.instances;

            home.packages = lib.mapAttrsToList mkInstance cfg.instances;
          }

          (lib.optionalAttrs (options ? impermanence) {
            impermanence.persist-directories = lib.concatMap (name: [
              (relHome name)
              (relCoreState name)
            ]) (lib.attrNames cfg.instances);
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

{
  config,
  lib,
  pkgs,
  ...
}:
let
  cfg = config.home.localCheckoutLinks;
  inherit (lib) mkOption types;
  files = lib.filterAttrs (_: f: f.enable) cfg.files;
  targets = builtins.attrNames files;
  relative =
    p:
    p != ""
    && !(builtins.hasContext p)
    && lib.all (x: x != "" && x != "." && x != "..") (lib.splitString "/" p);
  overlap = a: b: a == b || lib.hasPrefix (a + "/") b || lib.hasPrefix (b + "/") a;
  rootValid =
    p:
    p == null
    || (
      !builtins.hasContext p
      && lib.hasPrefix "/" p
      && !(overlap "/nix/store" p)
      && (p == "/" || relative (lib.removePrefix "/" p))
    );
  homeFiles = map (f: "${config.home.homeDirectory}/${f.target}") (
    lib.filter (f: f.enable) (lib.attrValues config.home.file)
  );
  state = "${config.xdg.stateHome}/home-manager/local-checkout-links";
  helper = "${pkgs.python3}/bin/python3 ${../tools/local-checkout-links/reconcile.py}";
  rawManifest = pkgs.writeText "local-checkout-links-raw.json" (
    builtins.toJSON {
      home = config.home.homeDirectory;
      inherit state homeFiles;
      checkouts = cfg.checkouts;
      files = lib.mapAttrs (_: f: {
        inherit (f) checkout source type;
        fallback = "${
          if builtins.hasContext (toString f.fallback) then
            f.fallback
          else
            builtins.path { path = f.fallback; }
        }";
      }) files;
    }
  );
  manifest = pkgs.runCommand "local-checkout-links-manifest.json" { } ''
    ${helper} ${rawManifest} --prepare "$out"
  '';
  command = "${helper} ${manifest} --trash ${lib.getExe pkgs.trash-cli}";
in
{
  options.home.localCheckoutLinks = {
    checkouts = mkOption {
      type = types.attrsOf (types.nullOr types.str);
      default = { };
      description = "Named absolute runtime checkout strings (no Nix context or store paths); null selects fallback.";
      example = {
        project = "/home/alice/src/project";
      };
    };
    files = mkOption {
      default = { };
      description = "Home-relative targets, recursively merged for directories. Keep this module imported for stale-link cleanup.";
      type = types.attrsOf (
        types.submodule {
          options = {
            enable = mkOption {
              type = types.bool;
              default = true;
              description = "Manage this target; false retires exact owned links.";
            };
            checkout = mkOption {
              type = types.str;
              description = "Name in checkouts.";
            };
            source = mkOption {
              type = types.str;
              description = "Normalized relative path within the runtime checkout.";
            };
            fallback = mkOption {
              type = types.path;
              description = "Required immutable file or directory source, retained in the generation.";
            };
            type = mkOption {
              type = types.enum [
                "file"
                "directory"
              ];
              default = "file";
              description = "Source type; directories always merge real directories and link individual leaves.";
            };
          };
        }
      );
    };
  };
  config = {
    assertions = [
      {
        assertion = lib.all rootValid (lib.attrValues cfg.checkouts);
        message = "localCheckoutLinks checkout roots must be absolute runtime strings without Nix context or store paths.";
      }
      {
        assertion = lib.all (
          t: relative t && relative files.${t}.source && builtins.hasAttr files.${t}.checkout cfg.checkouts
        ) targets;
        message = "localCheckoutLinks requires normalized relative targets/sources and known checkout names.";
      }
      {
        assertion = lib.all (a: lib.all (b: a == b || !(overlap a b)) targets) targets;
        message = "localCheckoutLinks targets must not overlap each other.";
      }
      {
        assertion = lib.all (
          t:
          let
            absolute = "${config.home.homeDirectory}/${t}";
          in
          !(overlap absolute state) && lib.all (f: !(overlap absolute f)) homeFiles
        ) targets;
        message = "localCheckoutLinks targets must not overlap home.file (including xdg.configFile) or ownership state.";
      }
    ];
    home.activation.checkLocalCheckoutLinks = lib.hm.dag.entryBefore [ "writeBoundary" ] ''
      ${command} --check
    '';
    home.activation.reconcileLocalCheckoutLinks =
      lib.hm.dag.entryBetween [ "onFilesChange" ] [ "writeBoundary" "linkGeneration" ]
        ''
          if [[ -v DRY_RUN ]]; then
            ${command} --dry-run
          else
            ${command}
          fi
        '';
  };
}

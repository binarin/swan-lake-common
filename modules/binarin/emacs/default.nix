{
  self,
  inputs,
  lib,
  config,
  ...
}:
let
  # The overlay needs `bleeding` from common's configured-pkgs. A consumer that
  # already applied an overlay providing my-emacs keeps its own package set.
  withMyEmacs = pkgs: if pkgs ? my-emacs then pkgs else pkgs.extend self.overlays.my-emacs;
in
{
  flake.overlays.my-emacs =
    final: prev:
    let
      emacsMaker =
        base:
        final.callPackage ./_package {
          emacsBasePackage = base;
          bleedingEmacsPackages = final.bleeding.emacsPackages;
          emacsPackagesFn = epkgs: [
            (epkgs.melpaBuild {
              pname = "niri-rpc";
              version = "0.0.0";
              src = inputs.emacs-niri-awareness;
            })
            (epkgs.melpaBuild {
              pname = "tramp-rpc";
              version = "0.10.0";
              src = inputs.emacs-tramp-rpc;
              packageRequires = with epkgs; [
                msgpack
              ];
            })
            # Built in this package set from zwanenzang's own packaging, so
            # it matches whichever Emacs the consumer builds, and does not
            # depend on zwanenzang's overlay being applied to `pkgs`.
            (epkgs.callPackage "${inputs.zwanenzang}/nix/_packages/zwanenzang-el.nix" {
              zwanenzang =
                inputs.zwanenzang.packages.${final.stdenv.hostPlatform.system}.zwanenzang;
            })
          ];
          flakyConfigDir =
            { emacsWithPackages, ... }:
            final.stdenv.mkDerivation {
              name = "user-init-dir";
              src = ./user-init-dir;
              buildPhase = ''
                mkdir "$out"
                ${final.lib.getExe final.rsync} -a "$src/" "$out/"
                chmod -R u+rwX "$out"
                ${final.lib.getExe emacsWithPackages} --init-directory "$out" --batch --eval \
                   '(progn
                      ;; No writable eln cache in the sandbox; byte-compile only.
                      (advice-add (quote native-compile-async) :override (function ignore))
                      (prepare-user-lisp nil "'"$out"'/user-lisp/.user-lisp-autoloads.el" t))'
              '';
            };
        };
    in
    (lib.genAttrs [ "emacs-git" "emacs-git-nox" "emacs-git-pgtk" ] (
      name:
      prev.${name}.overrideAttrs (old: {
        # The common overlay owns patch selection. Fail if upstream equivalents regress.
        postPatch = old.postPatch + ''
          grep -Fq 'Lisp_Object css = make_formatted_string' src/image.c
          grep -Fq 'SBYTES (css), NULL);' src/image.c
          sed -n '/DEFUN ("intern",/,/^}/p' src/lread.c | grep -Fq 'tem = oblookup (obarray,'
          grep -Fq "(if (fboundp 'trusted-content-p) (trusted-content-p) t)" lisp/progmodes/flymake.el
          grep -Fq "(function-get backend 'flymake-always-safe)" lisp/progmodes/flymake.el
          grep -Fq '(rx bos (literal (tramp-build-prefix-format)))' lisp/net/tramp.el
          grep -Fq 'Not a valid user name:' lisp/net/tramp.el
          grep -Fq 'Not a valid host name:' lisp/net/tramp.el
          grep -Fq '(defsubst tramp-get-connection-local-criteria (vec)' lisp/net/tramp.el
          grep -Fq '(tramp-allow-double-percent . t)' lisp/net/tramp-archive.el
        '';
      })
    ))
    // {
      # Match common's own config: released Emacs from the shared unstable pin.
      my-emacs = emacsMaker final.bleeding.emacs-nox;
      my-emacs-nox = emacsMaker final.bleeding.emacs-nox;
      my-emacs-pgtk = emacsMaker final.bleeding.emacs-pgtk;
      tramp-rpc-server = final.callPackage "${inputs.emacs-tramp-rpc}/default.nix" { };
    };

  perSystem =
    {
      pkgs,
      lib,
      ...
    }:
    let
      emacsPkgs = pkgs.extend self.overlays.my-emacs;
    in
    {
      packages = {
        inherit (emacsPkgs)
          my-emacs
          my-emacs-nox
          my-emacs-pgtk
          tramp-rpc-server
          ;
      };

      devShells.emacs-test = emacsPkgs.mkShell {
        name = "emacs-test-devshell";
        packages = with emacsPkgs; [
          coreutils
          bash
          bubblewrap
          rsync
          gnugrep
          my-emacs-nox
        ];
        shellHook = ''
          export IN_TEST_SHELL=1
          export EMACS_PGTK="${lib.getExe emacsPkgs.my-emacs-pgtk}"
        '';
      };
    };

  flake.nixosModules.emacs =
    {
      pkgs,
      ...
    }:
    {
      key = "swan-lake-common.modules.nixos.emacs";

      environment.systemPackages = with (withMyEmacs pkgs); [
        my-emacs
        tramp-rpc-server
      ];
    };

  flake.homeModules.emacs =
    {
      config,
      pkgs,
      lib,
      options,
      osConfig,
      ...
    }:
    let
      emacsPkgs = withMyEmacs pkgs;
    in
    {
      key = "swan-lake-common.modules.home.emacs";

      config = lib.mkMerge [
        {

          home.file.".config/emacs/snippets".source = ./_assets/yasnippets;

          programs.emacs = {
            enable = lib.mkForce false;
            package = lib.mkOverride 90 (
              emacsPkgs.my-emacs.override {
                emacsBasePackage = pkgs.bleeding.emacs-nox.override {
                  withNS = false;
                  withX = false;
                  withGTK3 = false;
                  withWebP = false;
                };
              }
            );
          };

          home.sessionVariables.EDITOR = "emacs-as-console-editor";

          home.packages = [
            config.programs.emacs.package
            # The CLI the Emacs client shells out to; the library comes with
            # the Emacs package above. Taken straight from the input so the
            # module works in configurations without zwanenzang's overlay.
            inputs.zwanenzang.packages.${pkgs.stdenv.hostPlatform.system}.zwanenzang
            (pkgs.writeShellScriptBin "emacs-as-console-editor" ''
              exec emacsclient -a 'emacs -nw' -nw "$@"
            '')
          ];
        }

        (lib.optionalAttrs (options ? impermanence) {
          impermanence.persist-directories = [
            ".local/state/emacs-clean"
            ".local/state/emacs-swan-lake"
          ];

          impermanence.local-directories = [
            ".cache/emacs-clean"
            ".cache/emacs-swan-lake"
          ];
        })

        (lib.mkIf (osConfig.services.graphical-desktop.enable or false) {
          xdg.dataFile."applications/org-protocol.desktop".source = ./_assets/org-protocol.desktop;

          xdg.configFile."autostart/emacs.desktop".source =
            "${config.programs.emacs.package}/share/applications/emacs.desktop";

          xdg.mimeApps.defaultApplications = lib.mkIf pkgs.stdenv.isLinux {
            "x-scheme-handler/org-protocol" = "org-protocol.desktop";
          };

          xdg.dataFile."icons/emacs/org.svg".source = ./_assets/org.svg;

          programs.emacs.package = lib.mkForce (
            emacsPkgs.my-emacs.override {
              emacsBasePackage = pkgs.bleeding.emacs-pgtk;
              extraPackages =
                with pkgs;
                [
                  direnv
                  emacs-all-the-icons-fonts
                  ghostscript
                  wtype
                ]
                ++ lib.optional (pkgs ? sicstus-manual) pkgs.sicstus-manual;
            }
          );
        })
      ];
    };
}

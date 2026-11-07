{
  pkgs,
  inputs,
  module,
}:
let
  inherit (pkgs) lib;
  hm =
    extra:
    inputs.home-manager.lib.homeManagerConfiguration {
      inherit pkgs;
      modules = [
        module
        {
          home.username = "tester";
          home.homeDirectory = "/home/tester";
          home.stateVersion = "26.05";
          programs.man.enable = false;
        }
        extra
      ];
    };
  file = {
    checkout = "project";
    source = "file";
    fallback = ./fallback/file;
  };
  base = {
    home.localCheckoutLinks = {
      checkouts.project = "/home/tester/checkout";
      files."config/file" = file;
    };
  };
  good = hm base;
  closure = pkgs.closureInfo { rootPaths = [ good.activationPackage ]; };
  succeeds = extra: (builtins.tryEval (hm extra).activationPackage.drvPath).success;
  assertions = [
    (succeeds base)
    (succeeds {
      home.localCheckoutLinks = {
        checkouts.project = null;
        files.a = file;
      };
    })
    (succeeds {
      home.localCheckoutLinks = {
        checkouts.project = "/nonexistent-runtime-only";
        files.a = file;
        files.b = file;
      };
    })
    (!(succeeds { home.localCheckoutLinks.files.a = file; }))
    (
      !(succeeds {
        home.localCheckoutLinks = {
          checkouts.project = "relative";
          files.a = file;
        };
      })
    )
    (
      !(succeeds {
        home.localCheckoutLinks = {
          checkouts.project = "${./fallback}";
          files.a = file;
        };
      })
    )
    (
      !(succeeds {
        home.localCheckoutLinks = {
          checkouts.project = "/nix/store/fake";
          files.a = file;
        };
      })
    )
    (
      !(succeeds {
        home.localCheckoutLinks = {
          checkouts.project = null;
          files."../escape" = file;
        };
      })
    )
    (
      !(succeeds {
        home.localCheckoutLinks = {
          checkouts.project = null;
          files."a//b" = file;
        };
      })
    )
    (
      !(succeeds {
        home.localCheckoutLinks = {
          checkouts.project = null;
          files."/absolute" = file;
        };
      })
    )
    (
      !(succeeds {
        home.localCheckoutLinks = {
          checkouts.project = null;
          files."a/" = file;
        };
      })
    )
    (
      !(succeeds {
        home.localCheckoutLinks = {
          checkouts.project = null;
          files.a = file // {
            source = "a/../b";
          };
        };
      })
    )
    (
      !(succeeds {
        home.localCheckoutLinks = {
          checkouts.project = null;
          files.a = file;
          files."a/b" = file;
        };
      })
    )
    (
      !(succeeds {
        home.localCheckoutLinks = {
          checkouts.project = null;
          files.".local/state" = file;
        };
      })
    )
    (
      !(succeeds {
        imports = [ base ];
        home.file."config".source = ./fallback;
      })
    )
    (
      !(succeeds {
        imports = [ base ];
        home.file."config/file/child".text = "collision";
      })
    )
    (
      !(succeeds {
        home.localCheckoutLinks = {
          checkouts.project = null;
          files.".config/app" = file;
        };
        xdg.configFile."app/child".text = "collision";
      })
    )
    (succeeds {
      imports = [ base ];
      home.file."config/sibling".text = "okay";
    })
    (
      !(succeeds {
        home.localCheckoutLinks = {
          checkouts.project = null;
          files.a = builtins.removeAttrs file [ "fallback" ];
        };
      })
    )
    (succeeds {
      imports = [ base ];
      home.localCheckoutLinks.files."config/file".enable = false;
      home.file."config/file".text = "disabled declaration";
    })
    (good.config.home.activation.checkLocalCheckoutLinks.before == [ "writeBoundary" ])
    (good.config.home.activation.reconcileLocalCheckoutLinks.before == [ "onFilesChange" ])
    (
      good.config.home.activation.reconcileLocalCheckoutLinks.after == [
        "writeBoundary"
        "linkGeneration"
      ]
    )
  ];
in
{
  local-checkout-links =
    pkgs.runCommand "local-checkout-links-runtime"
      {
        nativeBuildInputs = [ pkgs.python3 ];
        REAL_TRASH = lib.getExe pkgs.trash-cli;
      }
      ''
        cp -r ${../../tools/local-checkout-links} runtime
        chmod -R u+w runtime
        cd runtime
        python3 -m unittest -v
        touch "$out"
      '';
  local-checkout-links-module =
    assert lib.all (x: x) assertions;
    pkgs.runCommand "local-checkout-links-module" { } ''
      test -x ${good.activationPackage}/activate
      grep -q 'checkLocalCheckoutLinks' ${good.activationPackage}/activate
      grep -Fx '${../../tools/local-checkout-links/reconcile.py}' ${closure}/store-paths
      grep -Fx '${builtins.path { path = ./fallback/file; }}' ${closure}/store-paths
      touch "$out"
    '';
  local-checkout-links-activation = import ./activation.nix { inherit pkgs inputs module; };
}

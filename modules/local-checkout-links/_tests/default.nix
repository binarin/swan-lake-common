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
  aliasAssertions =
    lib.concatMap
      (target: [
        (
          !(succeeds {
            imports = [ base ];
            home.file.ordinary = {
              inherit target;
              text = "collision";
            };
          })
        )
        (
          !(succeeds {
            home.localCheckoutLinks = {
              checkouts.project = null;
              files.".config/config/file" = file;
            };
            xdg.configFile.ordinary = {
              inherit target;
              text = "collision";
            };
          })
        )
      ])
      [
        "config/./file"
        "config/a/../file"
        "config//file"
        "./config"
        "other/../config/file/child"
      ];
  danglingFallback = pkgs.runCommand "dangling-fallback" { } ''
    mkdir -p "$out/nested"
    echo valid > "$out/valid"
    ln -s /nonexistent-zl0040 "$out/nested/broken"
  '';
  danglingManifest = pkgs.writeText "dangling-fallback-manifest.json" (
    builtins.toJSON {
      files.tree = {
        fallback = danglingFallback;
        type = "directory";
      };
    }
  );
  assertions = [
    (succeeds base)
    (succeeds {
      home.localCheckoutLinks = {
        checkouts.project = "/";
        files.a = file;
      };
    })
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
      home.file.ordinary = {
        target = "config/./sibling";
        text = "okay";
      };
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
        cp -r ${../_runtime} runtime
        chmod -R u+w runtime
        cd runtime
        python3 -m unittest -v
        touch "$out"
      '';
  local-checkout-links-module =
    assert lib.all (x: x) (assertions ++ aliasAssertions);
    pkgs.runCommand "local-checkout-links-module" { } ''
      test -x ${good.activationPackage}/activate
      grep -q 'checkLocalCheckoutLinks' ${good.activationPackage}/activate
      grep -Fx '${../_runtime/reconcile.py}' ${closure}/store-paths
      grep -Fx '${builtins.path { path = ./fallback/file; }}' ${closure}/store-paths
      if ${pkgs.python3}/bin/python3 ${../_runtime/reconcile.py} \
        ${danglingManifest} --prepare "$TMPDIR/bad.json" > "$TMPDIR/bad.log" 2>&1; then
        echo 'dangling fallback unexpectedly accepted' >&2
        exit 1
      fi
      grep -F 'dangling fallback symlink:' "$TMPDIR/bad.log"
      touch "$out"
    '';
}

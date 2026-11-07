{ inputs, self, ... }:
{
  perSystem =
    { pkgs, ... }:
    let
      inherit (pkgs) lib;

      home =
        modules:
        (inputs.home-manager.lib.homeManagerConfiguration {
          inherit pkgs;
          modules = [
            self.homeModules.remote-session-launchers
            {
              home.username = "tester";
              home.homeDirectory = "/home/tester";
              home.stateVersion = "26.05";
            }
          ]
          ++ modules;
        }).config;

      # Golden matrix; reserved example hosts only.
      fixture = {
        programs.remoteSessionLaunchers = {
          enable = true;
          tags = {
            dev = {
              background = "001800";
              titleSuffix = " [dev]";
            };
            prod = {
              background = "180000";
              titlePrefix = "[PROD] ";
            };
          };
          items = {
            dev-shell = {
              name = "dev shell";
              host = "dev.example.com";
              protocol = "ssh";
              remote.shell = { };
              tag = "dev";
            };
            dev-tmux = {
              name = "dev tmux";
              host = "dev.example.com";
              protocol = "ssh";
              remote.tmux.session = "work";
              tag = "dev";
            };
            dev-herdr = {
              name = "dev herdr";
              host = "dev.example.com";
              protocol = "ssh";
              remote.herdr = { };
            };
            prod-btop = {
              name = "prod btop";
              host = "admin@prod.example.net";
              protocol = "ssh";
              protocolArgs = [
                "-p"
                "2222"
              ];
              remote.exec = [ "btop" ];
              tag = "prod";
            };
            prod-hello = {
              name = ''prod "hello" \ nix run'';
              host = "prod.example.net";
              protocol = "ssh";
              remote.nixRun = {
                package = "hello";
                args = [
                  "--greeting"
                  "hi there"
                ];
              };
              tag = "prod";
              background = "101010";
              titlePrefix = "[P] ";
            };
            box-shell = {
              name = "box mosh";
              host = "box.example.org";
              protocol = "mosh";
              remote.shell = { };
              title = "box";
            };
            box-tmux = {
              name = "box mosh tmux";
              host = "box.example.org";
              protocol = "mosh";
              protocolArgs = [ "--predict=always" ];
              remote.tmux = {
                session = "main";
                detachOthers = false;
              };
              tag = "dev";
              titleSuffix = "";
            };
            box-hello = {
              name = "box nix run";
              host = "box.example.org";
              protocol = "mosh";
              remote.nixRun = {
                package = "hello";
                args = [
                  "it's"
                  ""
                ];
                flake = "github:example/flake/main";
              };
            };
          };
        };
      };

      applications = config: "${config.home.activationPackage}/home-files/.local/share/applications";

      # Stable stand-ins so the goldens do not change with package versions.
      stubs = {
        programs.remoteSessionLaunchers.packages = lib.genAttrs [
          "foot"
          "niri"
          "jq"
          "openssh"
          "mosh"
          "uwsm"
        ] (p: pkgs.runCommand "stub-${p}" { } "mkdir $out");
      };

      # Regenerate after an intended change (from the repository root):
      #   g=modules/remote-session-launchers/golden; rm -rf $g && cp -rT --no-preserve=mode \
      #     "$(nix build --no-link --print-out-paths .#checks.x86_64-linux.remote-session-launchers-golden.actual)" $g
      goldenActual = pkgs.runCommand "remote-session-launchers-golden-actual" { } ''
        mkdir $out
        normalize() {
          sed -E -e 's,/nix/store/[a-z0-9]{32}-,/nix/store/HASH-,g' -e '1s,^#!/nix/store/HASH-bash-[^/]*/,#!/nix/store/HASH-bash/,' "$1"
        }
        for entry in ${
          applications (home [
            fixture
            stubs
          ])
        }/*; do
          base=$(basename "$entry" .desktop)
          normalize "$entry" > "$out/$base.desktop"
          normalize "$(sed -n 's/^Exec=//p' "$entry")" > "$out/$base.sh"
        done
      '';
    in
    {
      checks.remote-session-launchers-golden =
        pkgs.runCommand "remote-session-launchers-golden"
          {
            passthru.actual = goldenActual;
            nativeBuildInputs = [ pkgs.diffutils ];
          }
          ''
            diff -ru ${./golden} ${goldenActual}
            touch $out
          '';

      # Real packages, so the entries reference what a consumer would get.
      checks.remote-session-launchers-desktop-file-validate =
        pkgs.runCommand "remote-session-launchers-desktop-file-validate"
          { nativeBuildInputs = [ pkgs.desktop-file-utils ]; }
          ''
            entries=(${applications (home [ fixture ])}/*.desktop)
            [ "''${#entries[@]}" -eq 8 ]
            desktop-file-validate "''${entries[@]}"
            touch $out
          '';
    };
}

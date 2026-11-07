{
  pkgs,
  inputs,
  module,
}:
let
  inherit (pkgs) lib;

  home =
    modules:
    (inputs.home-manager.lib.homeManagerConfiguration {
      inherit pkgs;
      modules = [
        module
        {
          home.username = "tester";
          home.homeDirectory = "/home/tester";
          home.stateVersion = "26.05";
        }
      ]
      ++ modules;
    }).config;

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
  #   g=home-modules/remote-session-launchers/tests/golden; rm -rf $g && cp -rT --no-preserve=mode \
  #     "$(nix build --no-link --print-out-paths .#checks.x86_64-linux.remote-session-launchers-golden.actual)" $g
  goldenActual = pkgs.runCommand "remote-session-launchers-golden-actual" { } ''
    mkdir $out
    normalize() {
      sed -E -e 's,/nix/store/[a-z0-9]{32}-,/nix/store/HASH-,g' -e '1s,^#!/nix/store/HASH-bash-[^/]*/,#!/nix/store/HASH-bash/,' "$1"
    }
    for entry in ${
      applications (home [
        ./fixture.nix
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
  remote-session-launchers-golden =
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
  remote-session-launchers-desktop-file-validate =
    pkgs.runCommand "remote-session-launchers-desktop-file-validate"
      { nativeBuildInputs = [ pkgs.desktop-file-utils ]; }
      ''
        entries=(${applications (home [ ./fixture.nix ])}/*.desktop)
        [ "''${#entries[@]}" -eq 8 ]
        desktop-file-validate "''${entries[@]}"
        touch $out
      '';
}

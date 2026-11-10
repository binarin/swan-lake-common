{
  pkgs,
  inputs,
  module,
}:
let
  inherit (pkgs) lib;

  # Prints the env the wrapper sets; stands in for the real (heavy) app.
  stub = pkgs.runCommand "herdr-app-stub" { meta.mainProgram = "herdr-app"; } ''
    mkdir -p $out/bin $out/share/icons/hicolor/scalable/apps
    cat > $out/bin/herdr-app <<'EOF'
    #!${pkgs.runtimeShell}
    echo "$HERDR_APP_HOME|$HERDR_APP_ID|$HC_SESSION"
    EOF
    chmod +x $out/bin/herdr-app
    echo '<svg/>' > $out/share/icons/hicolor/scalable/apps/herdr-app.svg
  '';

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
          programs.herdr-app.package = stub;
          programs.herdr-app.instances.sl = "Swan Lake";
        }
        extra
      ];
    };

  # Same shape as nixos-config's per-user impermanence options.
  impermanenceStub = {
    options.impermanence.persist-directories = lib.mkOption {
      type = with lib.types; listOf str;
      default = [ ];
    };
  };

  plain = hm { };
  persisted = hm impermanenceStub;
  badName =
    builtins.tryEval
      (hm { programs.herdr-app.instances."-x" = "X"; }).activationPackage.drvPath;

  expect = cond: msg: lib.assertMsg cond "herdr-app test: ${msg}";
in
assert expect (
  persisted.config.impermanence.persist-directories == [
    ".config/herdrs/sl"
    ".local/state/herdr-core/sl"
  ]
) "instance home and core state are persisted";
assert expect (!badName.success) "invalid instance name is rejected";
pkgs.runCommand "herdr-app-instances" { } ''
  p=${plain.config.home.path}
  out_=$("$p/bin/herdr-sl")
  [ "$out_" = "/home/tester/.config/herdrs/sl|herdr-sl|sl" ] || { echo "env: $out_"; exit 1; }
  d="$p/share/applications/herdr-sl.desktop"
  grep -qx 'Name=Herdr (Swan Lake)' "$d"
  grep -qx 'Exec=herdr-sl' "$d"
  grep -qx 'StartupWMClass=herdr-sl' "$d"
  grep -qx 'Icon=herdr-app' "$d"
  [ -e "$p/share/icons/hicolor/scalable/apps/herdr-app.svg" ]
  touch $out
''

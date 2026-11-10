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
  fails = extra: !(builtins.tryEval (hm extra).activationPackage.drvPath).success;

  settingsFallback = pkgs.writeText "settings.json" ''{"font_size": 22}'';
  withSettings = hm {
    programs.herdr-app.instances.bk = "BK";
    home.localCheckoutLinks.checkouts.cfg = "/home/tester/cfg";
    programs.herdr-app.settings = {
      checkout = "cfg";
      source = "herdr/settings.json";
      fallback = settingsFallback;
    };
  };
  settingsLinks = withSettings.config.home.localCheckoutLinks.files;

  expect = cond: msg: lib.assertMsg cond "herdr-app test: ${msg}";
in
assert expect (
  persisted.config.impermanence.persist-directories == [
    ".config/herdrs/sl"
    ".local/state/herdr-core/sl"
    ".config/herdrs/shared"
  ]
) "instance home, core state and shared directory are persisted";
assert expect (fails {
  programs.herdr-app.instances."-x" = "X";
}) "invalid instance name is rejected";
assert expect (fails {
  programs.herdr-app.sharedDirectory = ".config/herdrs/sl/x";
}) "shared directory inside an instance home is rejected";
assert expect (
  plain.config.home.localCheckoutLinks.files == { }
) "settings stay per instance by default";
assert expect (
  lib.attrNames settingsLinks == [
    ".config/herdrs/bk/settings.json"
    ".config/herdrs/sl/settings.json"
  ]
  && settingsLinks.".config/herdrs/sl/settings.json".source == "herdr/settings.json"
  && settingsLinks.".config/herdrs/sl/settings.json".checkout == "cfg"
) "every instance's settings.json links to the shared checkout file";
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
  f=${plain.config.home-files}
  for d in mods themes layouts; do
    t=$(readlink "$(readlink "$f/.config/herdrs/sl/$d")")
    [ "$t" = "/home/tester/.config/herdrs/shared/$d" ] || { echo "$d -> $t"; exit 1; }
  done
  a=${builtins.toFile "activation" plain.config.home.activation.herdrAppSharedDirectory.data}
  grep -qx 'run mkdir -p /home/tester/.config/herdrs/shared/mods /home/tester/.config/herdrs/shared/themes /home/tester/.config/herdrs/shared/layouts' "$a"
  # Builds the local-checkout-links manifest (fallback validation) too.
  [ -e ${withSettings.activationPackage}/activate ]
  touch $out
''

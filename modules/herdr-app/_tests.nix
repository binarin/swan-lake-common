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
    echo "$HERDR_APP_HOME|$HERDR_APP_ID|$HC_SESSION|$HERDR_APP_TITLE"
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
  defaultPackage = plain.options.programs.herdr-app.package.default;
  needsLinkedAssetsFix = pkgs.llm-agents.herdr-app.version == "0.1.14";
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

  uiModDirs = lib.attrNames (lib.filterAttrs (_: t: t == "directory") (builtins.readDir ./_ui/mods));
  withUi = hm {
    programs.herdr-app.ui = {
      enable = true;
      hostTags = {
        local = {
          alias = "local";
          color = "ansi2";
        };
        db3.alias = "db";
        vps = "v";
      };
    };
  };
  uiFiles = withUi.config.home.file;
  withUiCheckout = hm {
    home.localCheckoutLinks.checkouts.common = "/home/tester/common";
    programs.herdr-app.ui = {
      enable = true;
      checkout = "common";
      keys = false;
    };
  };
  uiCheckoutLinks = withUiCheckout.config.home.localCheckoutLinks.files;

  expect = cond: msg: lib.assertMsg cond "herdr-app test: ${msg}";
in
assert expect (
  !needsLinkedAssetsFix || builtins.elem ./linked-assets.patch defaultPackage.patches
) "the default 0.1.14 package includes upstream's linked-assets fix";
assert expect (
  !needsLinkedAssetsFix || builtins.elem ./instance-title.patch defaultPackage.patches
) "the default 0.1.14 package supports instance window titles";
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
assert expect (
  lib.all (m: uiFiles.".config/herdrs/shared/mods/${m}".recursive or false) uiModDirs
  && uiFiles ? ".config/herdrs/shared/layouts/main-hosts.json"
  && uiFiles ? ".config/herdrs/sl/keys.json"
) "ui ships every mod under _ui/mods (as real folders), the layout and keys.json";
assert expect (
  builtins.fromJSON uiFiles.".config/herdrs/sl/host-tags.json".text == {
    hosts = {
      local = {
        alias = "local";
        color = "ansi2";
      };
      db3.alias = "db";
      vps.alias = "v";
    };
  }
) "host tags are written per instance, strings as aliases, unset colors left out";
assert expect (
  lib.all (
    m:
    removeAttrs uiCheckoutLinks.".config/herdrs/shared/mods/${m}" [ "enable" ] == {
      checkout = "common";
      source = "modules/herdr-app/_ui/mods/${m}";
      fallback = ./_ui/mods + "/${m}";
      type = "directory";
    }
  ) uiModDirs
  && !(uiCheckoutLinks ? ".config/herdrs/sl/keys.json")
  && !(withUiCheckout.config.home.file ? ".config/herdrs/shared/mods/host-spaces")
  && withUiCheckout.config.home.file ? ".config/herdrs/sl/host-tags.json"
) "with a checkout, the ui links from it (keys only when asked), host tags stay generated";
assert expect (
  !(plain.config.home.file ? ".config/herdrs/sl/host-tags.json")
) "no ui unless enabled";
pkgs.runCommand "herdr-app-instances" { nativeBuildInputs = [ pkgs.patch ]; } ''
  ${lib.optionalString needsLinkedAssetsFix ''
    # Apply against the pinned release, not a newer reference checkout. Only
    # the affected files are needed; don't build the GUI for wrapper tests.
    mkdir -p source/src source/vendor/gpui-kit/crates/shell/src
    cp ${pkgs.llm-agents.herdr-app.src}/src/{main,setup}.rs source/src/
    cp ${pkgs.llm-agents.herdr-app.src}/vendor/gpui-kit/crates/shell/src/assets.rs \
      source/vendor/gpui-kit/crates/shell/src/
    chmod -R u+w source
    patch --batch --fuzz=0 -d source -p1 < ${./linked-assets.patch}
    patch --batch --fuzz=0 -d source -p1 < ${./instance-title.patch}
    grep -q 'HERDR_APP_TITLE' source/src/main.rs
    grep -q 'HERDR_APP_TITLE' source/src/setup.rs
    grep -q 'AppAssets::new(mods::app_dir()).following_links()' source/src/main.rs
    grep -q 'fn following_links_serves_a_linked_folder_but_not_traversal()' \
      source/vendor/gpui-kit/crates/shell/src/assets.rs
  ''}
  p=${plain.config.home.path}
  out_=$("$p/bin/herdr-sl")
  [ "$out_" = "/home/tester/.config/herdrs/sl|herdr-sl|sl|Herdr (Swan Lake)" ] || { echo "env: $out_"; exit 1; }
  out_=$(${withSettings.config.home.path}/bin/herdr-bk)
  [ "$out_" = "/home/tester/.config/herdrs/bk|herdr-bk|bk|Herdr (BK)" ] || { echo "env: $out_"; exit 1; }
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
  grep -qx 'mkdir -p /home/tester/.config/herdrs/shared/mods /home/tester/.config/herdrs/shared/themes /home/tester/.config/herdrs/shared/layouts || true' "$(readlink -f "$p/bin/herdr-sl")"
  # Builds the local-checkout-links manifest (fallback validation) too.
  [ -e ${withSettings.activationPackage}/activate ]
  [ -e ${withUiCheckout.activationPackage}/activate ]
  u=${withUi.config.home-files}
  for m in ${lib.escapeShellArgs uiModDirs}; do
    [ -d "$u/.config/herdrs/shared/mods/$m" ] && [ ! -L "$u/.config/herdrs/shared/mods/$m" ] || { echo "mod $m is not a real folder"; exit 1; }
    [ -e "$u/.config/herdrs/shared/mods/$m/main.js" ] && [ -e "$u/.config/herdrs/shared/mods/$m/mod.json" ]
  done
  [ -e "$u/.config/herdrs/shared/layouts/main-hosts.json" ]
  grep -q '"mod:tmux-prefix"' "$u/.config/herdrs/sl/keys.json"
  touch $out
''

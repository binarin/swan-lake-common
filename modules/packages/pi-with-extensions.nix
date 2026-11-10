{ self, inputs, ... }:
let
  packageFn =
    {
      lib,
      symlinkJoin,
      makeWrapper,
      python3,
      pi,
      pi-subagents,
      pi-claude-bridge,
    }:
    symlinkJoin {
      name = "pi-with-extensions-${pi.version}";
      paths = [ pi ];
      nativeBuildInputs = [
        makeWrapper
        python3
      ];
      postBuild = ''
        # Older kernels (including Bentos) derive a bogus AT_PHDR from Bun's
        # unsorted PT_LOAD headers. Sorting is harmless on newer kernels too.
        # Copy both files privately and retain upstream's environment/assets.
        cp --remove-destination ${pi}/libexec/pi/pi "$out/libexec/pi/pi"
        chmod u+w "$out/libexec/pi/pi"
        python3 ${../../lib/sort-elf-load-segments.py} "$out/libexec/pi/pi"
        cp --remove-destination ${pi}/bin/pi "$out/bin/pi"
        chmod u+w "$out/bin/pi"
        substituteInPlace "$out/bin/pi" \
          --replace-fail 'exec "${pi}/libexec/pi/pi"' \
            "exec \"$out/libexec/pi/pi\""
        wrapProgram "$out/bin/pi" \
          --add-flags ${lib.escapeShellArg "--extension ${pi-subagents} --extension ${pi-claude-bridge}"}
      '';
      meta = pi.meta // {
        platforms = lib.intersectLists pi.meta.platforms lib.platforms.linux;
      };
    };
in
{
  perSystem =
    { pkgs, ... }:
    {
      packages.pi-with-extensions = pkgs.pi-with-extensions;
    };

  flake.overlays.pi-with-extensions = final: prev: {
    pi-with-extensions = final.callPackage packageFn {
      # Remain usable as a standalone overlay, while respecting a caller's
      # overrides when these attributes already exist.
      pi = final.llm-agents.pi or (inputs.llm-agents.overlays.shared-nixpkgs final prev).llm-agents.pi;
      pi-subagents = final.pi-subagents or (self.overlays.pi-subagents final prev).pi-subagents;
      pi-claude-bridge =
        final.pi-claude-bridge or (self.overlays.pi-claude-bridge final prev).pi-claude-bridge;
    };
  };
}

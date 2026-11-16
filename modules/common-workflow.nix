{ self, lib, ... }:
let
  commands = [ "with-common" "check-common-pin" "finalize-common" ];
  descriptions = {
    with-common = "Run a nix command with a local common checkout as an input override";
    check-common-pin = "Require the canonical promoted common pin";
    finalize-common = "Finalize a common input back to the promoted swan-lake-current";
  };
  mkApp = package: name: {
    type = "app";
    meta.description = descriptions.${name};
    program = "${package}/bin/${name}";
  };
  commonSelf = self;
in
{
  perSystem = { pkgs, self', ... }: {
    packages.common-workflow = pkgs.stdenvNoCC.mkDerivation {
      pname = "common-workflow";
      version = "0.1.0";
      src = ../tools/common-workflow;
      nativeBuildInputs = [ pkgs.makeWrapper pkgs.python3 pkgs.git ];
      doCheck = true;
      checkPhase = ''
        python3 -m unittest -v
      '';
      installPhase = ''
        mkdir -p $out/lib/common-workflow $out/bin
        cp workflow.py $out/lib/common-workflow/
        for command in ${lib.escapeShellArgs commands}; do
          makeWrapper ${pkgs.python3}/bin/python3 $out/bin/$command \
            --add-flags "$out/lib/common-workflow/$command" \
            --prefix PATH : ${pkgs.lib.makeBinPath [ pkgs.git pkgs.nix ]}
          ln -s workflow.py $out/lib/common-workflow/$command
        done
      '';
    };
    checks.common-workflow = self'.packages.common-workflow;
    apps = lib.genAttrs commands (mkApp self'.packages.common-workflow);
  };

  # Consumer-facing apps, imported by flakeModules.default: `nix run
  # .#check-common-pin` (CI, pre-commit) and `nix run .#finalize-common` in a
  # consumer checkout. Each attribute is a default so a consumer that defines
  # its own app of the same name keeps its definition.
  flake.flakeModules.common-workflow = { ... }: {
    key = "swan-lake-common.modules.flake.common-workflow";
    perSystem = { system, ... }: {
      apps = lib.genAttrs [ "check-common-pin" "finalize-common" ] (name:
        lib.mapAttrsRecursive (_: lib.mkDefault)
          (mkApp commonSelf.packages.${system}.common-workflow name));
    };
  };
}

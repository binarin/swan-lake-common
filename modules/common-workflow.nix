{ ... }:
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
        for command in with-common check-common-pin finalize-common; do
          makeWrapper ${pkgs.python3}/bin/python3 $out/bin/$command \
            --add-flags "$out/lib/common-workflow/$command" \
            --prefix PATH : ${pkgs.lib.makeBinPath [ pkgs.git pkgs.nix ]}
          ln -s workflow.py $out/lib/common-workflow/$command
        done
      '';
    };
    checks.common-workflow = self'.packages.common-workflow;
    apps = builtins.listToAttrs (map (name: {
      inherit name;
      value = { type = "app"; meta.description = "Common consumer workflow: ${name}"; program = "${self'.packages.common-workflow}/bin/${name}"; };
    }) [ "with-common" "check-common-pin" "finalize-common" ]);
  };
}

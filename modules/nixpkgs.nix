{ self, inputs, ... }:
let

  nixpkgsConfig = {
    allowUnfree = true;
  };
  defaultOverlays = [
    inputs.emacs-overlay.overlays.package
    inputs.zwanenzang.overlays.default
  ];
  importNixpkgs =
    {
      input,
      system,
      extraOverlays ? [ ],
      extraConfig ? { },
    }:
    import input {
      inherit system;
      config = nixpkgsConfig // extraConfig;
      overlays = extraOverlays ++ defaultOverlays;
    };
in
{
  options = {
  };
  config = {
    flake.flakeModules.configured-pkgs =
      let
        commonSelf = self;
      in
      { ... }:
      {
        key = "swan-lake-common.modules.flake.configured-pkgs";
        perSystem = { system, ... }: {
          _module.args.pkgs = commonSelf.configured-pkgs."${system}".nixpkgs;
        };
      };

    perSystem = { system, ... }: {
      _module.args.pkgs = self.configured-pkgs."${system}".nixpkgs;
    };

    flake.configured-pkgs."x86_64-linux".nixpkgs = importNixpkgs {
      input = inputs.nixpkgs;
      system = "x86_64-linux";
      extraOverlays = [
        (_final: _prev: {
          bleeding = self.configured-pkgs.x86_64-linux.nixpkgs-unstable;
        })
      ];
    };

    flake.configured-pkgs.x86_64-linux.nixpkgs-unstable = importNixpkgs {
      input = inputs.nixpkgs-unstable;
      system = "x86_64-linux";
    };
  };
}

{
  self,
  inputs,
  lib,
  ...
}:
let

  nixpkgsConfig = {
    allowUnfree = true;
    oraclejdk.accept_license = true;
    # Version-pinned: these have to be refreshed together with the nixpkgs pin.
    permittedInsecurePackages = [
      "qtwebengine-5.15.19"
      "python3.12-ecdsa-0.19.1"
      "python3.13-ecdsa-0.19.1"
      "olm-3.2.16"
      "immich-2.7.5"
    ];
  };
  defaultOverlays = [
    inputs.emacs-overlay.overlays.package
    inputs.zwanenzang.overlays.default
  ];

  # llm-agents is developed against nixpkgs-unstable (see the input's follows
  # above). Its shared-nixpkgs overlay evaluates the packages against the
  # consumer's set, so only apply it to the unstable sets: nixos-26.05 lacks
  # inputs the packages need (`installAgentSkills`, zig 0.17), which makes part
  # of the set fail to evaluate. The stable set re-exports the result instead
  # (see `llm-agents` below).
  llmAgentsOverlay = inputs.llm-agents.overlays.shared-nixpkgs;

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

  # Systems the consumer flakes generate outputs for. Keep this in sync with
  # them: consumers such as nixos-config publish packages for both.
  systems = [
    "x86_64-linux"
    "aarch64-linux"
  ];

  configuredPkgs = system: rec {
    nixpkgs-unstable = importNixpkgs {
      inherit system;
      input = inputs.nixpkgs-unstable;
      extraOverlays = [ llmAgentsOverlay ];
    };

    nixpkgs-unstable-cuda = importNixpkgs {
      inherit system;
      input = inputs.nixpkgs-unstable;
      extraOverlays = [ llmAgentsOverlay ];
      extraConfig.cudaSupport = true;
    };

    nixpkgs = importNixpkgs {
      inherit system;
      input = inputs.nixpkgs;
      extraOverlays = [
        (_final: _prev: {
          bleeding = nixpkgs-unstable;
          # Re-export the llm-agents set built against nixpkgs-unstable, so
          # consumers get a working `pkgs.llm-agents` without re-evaluating
          # the overlay against this stable set.
          llm-agents = nixpkgs-unstable.llm-agents;
        })
      ];
    };
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

    flake.configured-pkgs = lib.genAttrs systems configuredPkgs;
  };
}

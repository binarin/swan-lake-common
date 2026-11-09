{ self, inputs, ... }:
let

  nixpkgsConfig = {
    allowUnfree = true;
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
          # Re-export the llm-agents set built against nixpkgs-unstable, so
          # consumers get a working `pkgs.llm-agents` without re-evaluating
          # the overlay against this stable set.
          llm-agents = self.configured-pkgs.x86_64-linux.nixpkgs-unstable.llm-agents;
        })
      ];
    };

    flake.configured-pkgs.x86_64-linux.nixpkgs-unstable = importNixpkgs {
      input = inputs.nixpkgs-unstable;
      system = "x86_64-linux";
      extraOverlays = [ llmAgentsOverlay ];
    };
  };
}

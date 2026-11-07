{ inputs, self, ... }:
{
  imports = [ inputs.home-manager.flakeModules.home-manager ];
  flake.homeModules.local-checkout-links = ../home-modules/local-checkout-links.nix;
  perSystem = { pkgs, ... }: {
    checks = import ../tests/local-checkout-links {
      inherit pkgs inputs;
      module = self.homeModules.local-checkout-links;
    };
  };
}

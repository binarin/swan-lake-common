{ inputs, self, ... }:
{
  imports = [ inputs.home-manager.flakeModules.home-manager ];
  flake.homeModules.local-checkout-links = ./_hm;
  perSystem = { pkgs, ... }: {
    checks = import ./_tests {
      inherit pkgs inputs;
      module = self.homeModules.local-checkout-links;
    };
    # The full nspawn lifecycle needs a container-capable builder or root driver.
    # Keep it explicitly runnable without scheduling it in default flake checks.
    legacyPackages.local-checkout-links-activation = import ./_tests/activation.nix {
      inherit pkgs inputs;
      module = self.homeModules.local-checkout-links;
    };
  };
}

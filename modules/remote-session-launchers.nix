{ inputs, self, ... }:
{
  flake.homeModules.remote-session-launchers = ../home-modules/remote-session-launchers;
  perSystem = { pkgs, ... }: {
    checks = import ../home-modules/remote-session-launchers/tests {
      inherit pkgs inputs;
      module = self.homeModules.remote-session-launchers;
    };
  };
}

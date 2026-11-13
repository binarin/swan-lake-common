{ inputs, lib, ... }:
let
  sshTrust = import ./_lib.nix { inherit lib; };
  mkModule = platform: key: {
    inherit key;
    imports = [ (import ./_module.nix { inherit sshTrust platform; }) ];
  };
in
{
  imports = [ inputs.flake-parts.flakeModules.modules ];

  flake.lib.sshTrust = sshTrust;
  flake.modules.nixos.ssh-trust = mkModule "nixos" "swan-lake-common.modules.nixos.ssh-trust";
  flake.modules.systemManager.ssh-trust = mkModule "system-manager" "swan-lake-common.modules.systemManager.ssh-trust";

  perSystem =
    { pkgs, ... }:
    {
      checks = import ./_tests {
        inherit pkgs inputs sshTrust;
        nixosModule = mkModule "nixos" "swan-lake-common.modules.nixos.ssh-trust";
        systemManagerModule = mkModule "system-manager" "swan-lake-common.modules.systemManager.ssh-trust";
      };
    };
}

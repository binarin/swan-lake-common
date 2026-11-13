{ self, lib, ... }:
let
  mkPackage =
    pkgs:
    import ./_package.nix {
      inherit pkgs;
      inherit (self.lib.sshTrust) registryDocument;
    };
in
{
  perSystem =
    { pkgs, self', ... }:
    {
      packages.ssh-ca = mkPackage pkgs;
      checks.ssh-ca = self'.packages.ssh-ca.overrideAttrs (old: {
        nativeCheckInputs = old.nativeCheckInputs ++ [
          pkgs.rustfmt
          pkgs.clippy
        ];
        preCheck = ''
          cargo fmt --check
          cargo clippy --all-targets -- -D warnings
        '';
      });
      apps.ssh-ca = {
        type = "app";
        meta.description = "Issue OpenSSH user certificates from trusted CAs";
        program = lib.getExe self'.packages.ssh-ca;
      };
      devShells.ssh-ca = pkgs.mkShell {
        name = "ssh-ca devshell";
        packages = [
          pkgs.cargo
          pkgs.rustc
          pkgs.rustfmt
          pkgs.clippy
          pkgs.rust-analyzer
          pkgs.gcc
          pkgs.openssh
        ];
      };
    };

  # The flake's own systems are x86_64-linux only; the CLI is also supported
  # on ARM64 Linux, built from the configured package set for it.
  flake.packages.aarch64-linux.ssh-ca = mkPackage self.configured-pkgs.aarch64-linux.nixpkgs;
}

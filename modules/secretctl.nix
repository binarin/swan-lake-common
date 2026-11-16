{ ... }:
{
  perSystem =
    { pkgs, self', ... }:
    let
      inherit (pkgs) lib;
      src = ../tools/secretctl;
    in
    {
      packages.secretctl = pkgs.rustPlatform.buildRustPackage {
        pname = "secretctl";
        version = "0.1.0";
        src = lib.fileset.toSource {
          root = src;
          fileset = lib.fileset.unions [
            (src + "/Cargo.toml")
            (src + "/Cargo.lock")
            (src + "/src")
            (src + "/tests")
          ];
        };
        cargoLock.lockFile = src + "/Cargo.lock";
        nativeBuildInputs = [ pkgs.makeWrapper ];
        # The integration tests drive real sops, age, age-keygen and
        # ssh-keygen with throwaway age identities; PGP is only exercised at
        # the policy level.
        nativeCheckInputs = [
          pkgs.git
          pkgs.sops
          pkgs.age
          pkgs.openssh
        ];
        # Prefixed, not replaced: gpg, gpg-agent and age plugins (for example
        # age-plugin-yubikey) still come from the caller's PATH. age-keygen and
        # ssh-keygen generate the keys of `machine add`.
        postInstall = ''
          wrapProgram "$out/bin/secretctl" --prefix PATH : ${
            lib.makeBinPath [
              pkgs.git
              pkgs.sops
              pkgs.age
              pkgs.openssh
            ]
          }
        '';
        meta = {
          description = "Recipient-policy driven sops encryption for a secrets tree";
          mainProgram = "secretctl";
          platforms = lib.platforms.linux;
        };
      };
      checks.secretctl = self'.packages.secretctl.overrideAttrs (old: {
        nativeCheckInputs = old.nativeCheckInputs ++ [
          pkgs.rustfmt
          pkgs.clippy
        ];
        preCheck = ''
          cargo fmt --check
          cargo clippy --all-targets -- -D warnings
        '';
      });
    };
}

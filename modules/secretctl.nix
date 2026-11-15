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
        nativeCheckInputs = [ pkgs.git ];
        postInstall = ''
          wrapProgram "$out/bin/secretctl" --prefix PATH : ${lib.makeBinPath [ pkgs.git ]}
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

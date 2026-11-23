{ ... }:
{
  perSystem =
    { pkgs, self', ... }:
    let
      inherit (pkgs) lib;
      src = ../tools/swl;
    in
    {
      packages.swl = pkgs.rustPlatform.buildRustPackage {
        pname = "swl";
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
        # swl runs git for worktrees and status, and curl for the forge API.
        # It is not wrapped with a PATH: the shim must find the caller's
        # nix, nh and deploy, and the generated wrappers exec this binary by
        # its store path. fj, for `swl publish`, comes from the caller's PATH.
        env.SWL_GIT = lib.getExe pkgs.git;
        env.SWL_CURL = lib.getExe pkgs.curl;
        # The chain and landing tests drive real git and nix on throwaway
        # local flakes, with a store in the build directory; the landing test
        # serves a fake forge API on 127.0.0.1.
        nativeCheckInputs = [
          pkgs.git
          pkgs.nix
        ];
        preCheck = ''
          export SWL_TEST_NIX_STORE=$TMPDIR/swl-test-store
        '';
        meta = {
          description = "Swan Lake cross-repo workspaces: checkouts, input overrides, nix/nh/deploy shims, manifests and CI checks";
          mainProgram = "swl";
          platforms = lib.platforms.linux;
        };
      };
      checks.swl = self'.packages.swl.overrideAttrs (old: {
        nativeCheckInputs = old.nativeCheckInputs ++ [
          pkgs.rustfmt
          pkgs.clippy
        ];
        preCheck = old.preCheck + ''
          cargo fmt --check
          cargo clippy --all-targets -- -D warnings
        '';
      });
    };
}

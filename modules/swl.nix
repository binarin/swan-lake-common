{ self, lib, ... }:
let
  commonSelf = self;
  mkApp = package: {
    type = "app";
    meta.description = "Swan Lake cross-repo workspaces, manifests and CI checks";
    program = "${package}/bin/swl";
  };
in
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
        # its store path. From the caller's PATH come fj, for `swl publish`;
        # nix, for `swl env` to make the workspace's GC root of this store
        # path; and direnv, optional, for `swl start` to allow the consumers'
        # .envrc. `status`, `start` and `land` use fj's login from its keys.json,
        # not fj itself.
        env.SWL_GIT = lib.getExe pkgs.git;
        env.SWL_CURL = lib.getExe pkgs.curl;
        # The tests drive real git and nix on throwaway local flakes, with a
        # store in the build directory, and serve a fake forge API on
        # 127.0.0.1. In the sandbox swl is not in /nix/store (no GC root is
        # made) and direnv is absent; the tests that need it fake it.
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
      apps.swl = mkApp self'.packages.swl;
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

  # Consumer-facing app, imported by flakeModules.default: `nix run .#swl --
  # check` and `nix run .#swl -- ci env` in a consumer's CI, with the swl of
  # the consumer's locked common. A default, so a consumer's own app of the
  # same name wins.
  flake.flakeModules.swl = { ... }: {
    key = "swan-lake-common.modules.flake.swl";
    perSystem = { system, ... }: {
      apps.swl = lib.mapAttrsRecursive (_: lib.mkDefault) (mkApp commonSelf.packages.${system}.swl);
    };
  };
}

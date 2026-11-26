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
      # The paths below tools/swl that swl is built from. A change to any of
      # them must bump the version in Cargo.toml: the CI job
      # .forgejo/workflows/swl-version.yaml checks it with
      # scripts/check-swl-version-bump.py, which reads this same list.
      srcPaths = lib.importJSON (src + "/src-fileset.json");
      # lastModifiedDate is YYYYMMDDHHMMSS, in UTC.
      commitDate =
        d: "${builtins.substring 0 4 d}-${builtins.substring 4 2 d}-${builtins.substring 6 2 d}";
      version = (builtins.fromTOML (builtins.readFile (src + "/Cargo.toml"))).package.version;
      # What packages.swl sets as SWL_COMMIT and SWL_COMMIT_DATE; empty when
      # common has no rev or date.
      commit = commonSelf.shortRev or commonSelf.dirtyShortRev or "";
      date = lib.optionalString (commonSelf ? lastModifiedDate) (commitDate commonSelf.lastModifiedDate);
      meta = {
        description = "Swan Lake cross-repo workspaces: checkouts, input overrides, nix/nh/deploy shims, manifests and CI checks";
        mainProgram = "swl";
        platforms = lib.platforms.linux;
      };
    in
    {
      # The compiled swl, from its fileset only: it is rebuilt when swl's own
      # sources change, not on every commit of common.
      packages.swl-unwrapped = pkgs.rustPlatform.buildRustPackage {
        pname = "swl-unwrapped";
        inherit version;
        src = lib.fileset.toSource {
          root = src;
          fileset = lib.fileset.unions (map (p: src + "/${p}") srcPaths);
        };
        cargoLock.lockFile = src + "/Cargo.lock";
        # swl runs git for worktrees and status, and curl for the forge API.
        # It is not wrapped with a PATH: the shim must find the caller's
        # nix, nh and deploy, and the generated wrappers exec swl by its
        # store path. From the caller's PATH come fj, for `swl publish`;
        # nix, for `swl env` to make the workspace's GC root of this store
        # path; and direnv, optional, for `swl start` to allow the consumers'
        # .envrc. `status`, `start`, `land` and `bump` use fj's login from its
        # keys.json, not fj itself.
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
        meta = meta // {
          description = "${meta.description} (without the commit of common)";
        };
      };
      # swl as it is used: a wrapper around swl-unwrapped, rebuilt cheaply on
      # every commit of common. `swl --version` names that commit: a dirty
      # tree gives the `-dirty` rev; a tree without a rev, `(unknown)`.
      # SWL_SELF is the wrapper itself, which `swl env` pins, so that the
      # workspace's wrappers and `swl status` see the rev too.
      packages.swl =
        pkgs.runCommand "swl-${version}"
          {
            nativeBuildInputs = [ pkgs.makeWrapper ];
            passthru.unwrapped = self'.packages.swl-unwrapped;
            inherit meta version;
          }
          ''
            makeWrapper ${lib.getExe self'.packages.swl-unwrapped} $out/bin/swl \
              --set SWL_COMMIT ${lib.escapeShellArg commit} \
              --set SWL_COMMIT_DATE ${lib.escapeShellArg date} \
              --set SWL_SELF $out/bin/swl
          '';
      apps.swl = mkApp self'.packages.swl;
      # That the wrapper's `swl --version` names common's rev and date.
      checks.swl-wrapper =
        let
          expected =
            if commit == "" then
              "swl ${version} (unknown)"
            else
              "swl ${version} (${commit}${lib.optionalString (date != "") " ${date}"})";
        in
        pkgs.runCommand "swl-wrapper-test" { } ''
          actual=$(${lib.getExe self'.packages.swl} --version)
          if [ "$actual" != ${lib.escapeShellArg expected} ]; then
            echo "swl --version printed '$actual', not "${lib.escapeShellArg expected} >&2
            exit 1
          fi
          touch $out
        '';
      # The CI check that an MR changing swl bumps its version, on scratch git
      # repos.
      checks.swl-version-bump =
        pkgs.runCommand "swl-version-bump-test"
          {
            nativeBuildInputs = [
              pkgs.python3
              pkgs.git
            ];
            src = lib.fileset.toSource {
              root = ../scripts;
              fileset = lib.fileset.unions [
                ../scripts/check-swl-version-bump.py
                ../scripts/test_check_swl_version_bump.py
              ];
            };
          }
          ''
            cd $src
            PYTHONDONTWRITEBYTECODE=1 python3 -m unittest -v test_check_swl_version_bump
            touch $out
          '';
      checks.swl = self'.packages.swl-unwrapped.overrideAttrs (old: {
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

  # swl on PATH, for the machines where Swan Lake changes are made.
  flake.homeModules.swl =
    { pkgs, ... }:
    {
      key = "swan-lake-common.modules.home.swl";
      home.packages = [ commonSelf.packages.${pkgs.stdenv.hostPlatform.system}.swl ];
    };
}

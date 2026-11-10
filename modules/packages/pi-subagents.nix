# Load with `pi -e /nix/store/...-pi-subagents-...`, or add the package's
# output path to programs.pi-coding-agent.settings.packages.
let
  packageFn =
    {
      lib,
      buildNpmPackage,
      fetchFromGitHub,
      nodejs_24,
    }:
    buildNpmPackage (finalAttrs: {
      pname = "pi-subagents";
      version = "0.87.0";

      src = fetchFromGitHub {
        owner = "nicobailon";
        repo = "pi-subagents";
        rev = "52e68e9ef0e563d5c97b28519f9999f1b5fbbbea";
        hash = "sha256-jjumBpE2OrUmeut7M3wZDlLjEA9mbA7Q21NepUYHU5s=";
      };

      nodejs = nodejs_24;
      npmDepsHash = "sha256-aGw6EkTn6zIf29rx69B/6lEjHjxF6ZZCHbslXhy4ITg=";
      npmRebuildFlags = [ "--ignore-scripts" ];
      npmBuildScript = "build:pkg";

      # This is a Pi resource package, not the upstream installer CLI (which
      # clones a mutable checkout into ~/.pi). Keep the manifest at $out so the
      # output itself can be used as a local Pi package.
      installPhase = ''
        runHook preInstall

        npm prune --omit=dev --ignore-scripts
        mkdir -p "$out"
        cp -r dist-pkg/. "$out/"
        cp -r node_modules "$out/node_modules"

        runHook postInstall
      '';

      doInstallCheck = true;
      installCheckPhase = ''
        runHook preInstallCheck

        cd "$out"
        node --input-type=module <<'JS'
        import assert from 'node:assert/strict';
        import fs from 'node:fs';
        import { createRequire } from 'node:module';
        const require = createRequire(process.cwd() + '/package.json');
        const pkg = JSON.parse(fs.readFileSync('package.json', 'utf8'));
        assert.equal(pkg.version, '${finalAttrs.version}');
        for (const paths of Object.values(pkg.pi)) {
          for (const path of paths) assert.ok(fs.existsSync(path), path);
        }
        for (const name of Object.keys(pkg.dependencies)) require.resolve(name);
        for (const name of Object.keys(pkg.peerDependencies)) {
          assert.ok(!fs.existsSync('node_modules/' + name), 'Bundled host peer: ' + name);
        }
        for (const path of ['agents', 'runner-peer-loader.mjs', 'runner-peer-preload.mjs',
          'inspector-runner.mjs', 'async-retention-discovery-worker.mjs',
          'src/runs/background/subagent-runner-bootstrap.js']) {
          assert.ok(fs.existsSync(path), path);
        }
        JS

        runHook postInstallCheck
      '';

      meta = {
        description = "Pi extension for single-agent delegation and scripted multi-agent workflows";
        homepage = "https://github.com/nicobailon/pi-subagents";
        changelog = "https://github.com/nicobailon/pi-subagents/blob/${finalAttrs.src.rev}/CHANGELOG.md";
        license = lib.licenses.mit;
        platforms = lib.platforms.unix;
      };
    });
in
{
  perSystem =
    { pkgs, ... }:
    {
      packages.pi-subagents = pkgs.callPackage packageFn { };
    };

  flake.overlays.pi-subagents = final: _prev: {
    pi-subagents = final.callPackage packageFn { };
  };
}

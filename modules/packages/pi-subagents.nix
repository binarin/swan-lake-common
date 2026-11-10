# Load with `pi -e /nix/store/...-pi-subagents-...`, or add the package's
# output path to programs.pi-coding-agent.settings.packages.
let
  packageFn =
    {
      lib,
      buildNpmPackage,
      fetchFromGitHub,
      nodejs_24,
      git,
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

      # pi-subagents returns the child boundary as a replacement system prompt,
      # which pi-claude-bridge drops. Add it as a prompt section instead, which
      # the bridge forwards to Claude Code.
      patches = [ ./pi-subagents-child-boundary-section.patch ];

      nodejs = nodejs_24;
      npmDepsHash = "sha256-aGw6EkTn6zIf29rx69B/6lEjHjxF6ZZCHbslXhy4ITg=";
      npmRebuildFlags = [ "--ignore-scripts" ];
      npmBuildScript = "build:pkg";

      # Upstream's prompt tests cover the patched boundary. The watchdog test
      # in the second file needs git.
      doCheck = true;
      nativeCheckInputs = [ git ];
      checkPhase = ''
        runHook preCheck
        node --experimental-strip-types --import ./test/support/isolated-temp-root.mjs --test \
          test/unit/child-prompt-sections.test.ts test/unit/subagent-prompt-runtime.test.ts
        runHook postCheck
      '';

      # This is a Pi resource package, not the upstream installer CLI (which
      # clones a mutable checkout into ~/.pi). Keep the manifest at $out so the
      # output itself can be used as a local Pi package.
      installPhase = ''
        runHook preInstall

        npm prune --omit=dev --ignore-scripts
        mkdir -p "$out"
        cp -r dist-pkg/. "$out/"
        cp -r node_modules "$out/node_modules"

        # Pi labels index.js extensions by their parent directory, which is
        # "bundle" upstream. Route the manifest through a named directory so
        # the extension list shows "subagents".
        mkdir "$out/subagents"
        echo 'export { default } from "../bundle/index.js";' > "$out/subagents/index.js"
        node --input-type=module <<'JS'
        import fs from 'node:fs';
        const file = process.env.out + '/package.json';
        const pkg = JSON.parse(fs.readFileSync(file, 'utf8'));
        if (JSON.stringify(pkg.pi.extensions) !== JSON.stringify(['./bundle/index.js']))
          throw new Error('unexpected pi.extensions: ' + JSON.stringify(pkg.pi.extensions));
        pkg.pi.extensions = ['./subagents/index.js'];
        fs.writeFileSync(file, JSON.stringify(pkg, null, 2) + '\n');
        JS

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
        assert.deepEqual(pkg.pi.extensions, ['./subagents/index.js']);
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

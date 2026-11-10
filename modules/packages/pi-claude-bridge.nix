{ inputs, ... }:
let
  packageFn =
    {
      lib,
      buildNpmPackage,
      fetchFromGitHub,
      nodejs_24,
      claude-code,
    }:
    buildNpmPackage (finalAttrs: {
      pname = "pi-claude-bridge";
      version = "0.11.0";

      src = fetchFromGitHub {
        owner = "elidickinson";
        repo = "pi-claude-bridge";
        rev = "debda8d669d978f0deba5249e32d7f6a2a489d86";
        hash = "sha256-94VxD3FND+fHwTlYmZvUKyp5X3BqMKoJifCawGCIZFk=";
      };

      nodejs = nodejs_24;
      npmDepsHash = "sha256-nS1KZnHHwTVV7B32tc7B7ZYssoPbKLo9GcXZr2Shot8=";
      npmFlags = [ "--legacy-peer-deps" ];
      npmRebuildFlags = [ "--ignore-scripts" ];
      dontNpmBuild = true;

      # The SDK's bundled native executable cannot run directly on NixOS.
      # Keep the user-configured executable override, but provide a working
      # Nix-packaged default without modifying mutable user settings.
      postPatch = ''
        substituteInPlace src/index.ts \
          --replace-fail 'providerSettings = config.provider ?? {};' \
            'providerSettings = { pathToClaudeCodeExecutable: "${claude-code}/bin/claude", ...config.provider };'
      '';

      # Offline SDK contract tests invoke query() directly, bypassing the
      # provider default above. Replace its platform binary for those too.
      postConfigure = ''
        find node_modules/@anthropic-ai -type f -name claude \
          -exec ln -sf ${claude-code}/bin/claude {} \;
      '';

      doCheck = true;
      checkPhase = ''
        runHook preCheck
        npm run test:unit
        runHook postCheck
      '';

      # Pi loads TypeScript resources directly. Only ship runtime dependencies;
      # the Pi host supplies its own packages through the extension loader.
      installPhase = ''
        runHook preInstall
        npm prune --omit=dev --ignore-scripts --legacy-peer-deps
        mkdir -p "$out"
        cp -r package.json src node_modules README.md LICENSE "$out/"

        # Pi labels local index.ts extensions by their parent directory. An
        # entry point in a named subdirectory gives a short label instead of
        # "src" or the store path, without moving upstream files that resolve
        # assets relative to src.
        node --input-type=module <<'JS'
        import fs from 'node:fs';
        const root = process.env.out;
        const pkg = JSON.parse(fs.readFileSync(root + '/package.json', 'utf8'));
        pkg.pi.extensions = ['./claude-bridge/index.ts'];
        fs.writeFileSync(root + '/package.json', JSON.stringify(pkg, null, 2) + '\n');
        fs.mkdirSync(root + '/claude-bridge');
        fs.writeFileSync(root + '/claude-bridge/index.ts', 'export { default } from "../src/index.ts";\n');
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
        const pkg = JSON.parse(fs.readFileSync('package.json', 'utf8'));
        assert.equal(pkg.version, '${finalAttrs.version}');
        assert.deepEqual(pkg.pi.extensions, ['./claude-bridge/index.ts']);
        assert.equal(fs.readFileSync('claude-bridge/index.ts', 'utf8'), 'export { default } from "../src/index.ts";\n');
        for (const path of pkg.pi.extensions) assert.ok(fs.existsSync(path), path);
        for (const name of Object.keys(pkg.dependencies)) {
          await import(name === '@modelcontextprotocol/sdk' ? name + '/server/mcp.js' : name);
        }
        for (const name of Object.keys(pkg.peerDependencies)) {
          assert.ok(!fs.existsSync('node_modules/' + name), 'Bundled host peer: ' + name);
        }
        assert.ok(fs.readFileSync('src/index.ts', 'utf8').includes('${claude-code}/bin/claude'));
        JS
        test -x ${claude-code}/bin/claude
        runHook postInstallCheck
      '';

      meta = {
        description = "Pi model provider using Claude Code through the Claude Agent SDK";
        homepage = "https://github.com/elidickinson/pi-claude-bridge";
        license = lib.licenses.mit;
        platforms = lib.platforms.linux;
      };
    });
in
{
  perSystem =
    { pkgs, ... }:
    {
      packages.pi-claude-bridge = pkgs.pi-claude-bridge;
    };

  flake.overlays.pi-claude-bridge = final: prev: {
    pi-claude-bridge = final.callPackage packageFn {
      claude-code =
        final.llm-agents.claude-code or (inputs.llm-agents.overlays.shared-nixpkgs final prev)
        .llm-agents.claude-code;
    };
  };
}

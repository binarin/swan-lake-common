{
  pkgs,
  inputs,
  module,
}:
let
  inherit (pkgs) lib;

  hm =
    extra:
    inputs.home-manager.lib.homeManagerConfiguration {
      inherit pkgs;
      modules = [
        module
        {
          home.username = "tester";
          home.homeDirectory = "/home/tester";
          home.stateVersion = "26.05";
          programs.man.enable = false;
        }
        extra
      ];
    };

  agents = {
    programs.claude-code = {
      enable = true;
      package = null;
    };
    programs.codex = {
      enable = true;
      package = null;
    };
  };

  skillDir =
    name:
    pkgs.writeTextDir "SKILL.md" ''
      ---
      name: ${name}
      description: Test skill ${name}.
      ---
      Body of ${name}.
    '';
  # A source path, as a downstream flake's ./skills/... would be.
  localDir =
    name:
    builtins.path {
      name = "skill-${name}";
      path = skillDir name;
    };
  localFile =
    name:
    builtins.path {
      name = "skill-${name}.md";
      path = "${skillDir name}/SKILL.md";
    };

  defaults = hm agents;
  piOnly = hm { };
  extended = hm {
    imports = [ agents ];
    programs.agent-skills.skills = {
      claudy-comment-detector.enable = false;
      from-dir.source = localDir "from-dir";
      from-file.source = localFile "from-file";
      registered-off = {
        source = localDir "registered-off";
        enable = lib.mkDefault false;
      };
    };
    programs.claude-code.skills.direct = "---\nname: direct\ndescription: d\n---\n";
  };

  fails = extra: !(builtins.tryEval (hm extra).activationPackage.drvPath).success;
  expect = cond: msg: lib.assertMsg cond "agent-skills test: ${msg}";
in
assert expect (fails {
  programs.agent-skills.skills.wrong-name.source = localDir "other";
}) "frontmatter name differing from the attribute name is rejected";
assert expect (
  piOnly.config.home.file ? ".pi/agent/skills/grilling"
  && !(piOnly.config.home.file ? ".claude/skills/grilling/SKILL.md")
) "skills reach pi without claude-code or codex enabled";
pkgs.runCommand "agent-skills" { } ''
  set -eu
  check() { # dir, expected skills
    d=$1; shift
    actual=$(ls "$d" | sort | tr '\n' ' ')
    [ "$actual" = "$* " ] || { echo "$d: '$actual' != '$* '"; exit 1; }
    for s in "$@"; do
      [ -f "$d/$s/SKILL.md" ] || { echo "$d/$s/SKILL.md missing"; exit 1; }
    done
  }

  f=${defaults.config.home-files}
  for d in .claude/skills .codex/skills .pi/agent/skills; do
    check "$f/$d" claudy-comment-detector grilling hc-handoff
  done
  grep -qx 'name: claudy-comment-detector' "$f/.pi/agent/skills/claudy-comment-detector/SKILL.md"
  for d in .claude/skills .codex/skills .pi/agent/skills; do
    cmp ${./_skills/hc-handoff/SKILL.md} "$f/$d/hc-handoff/SKILL.md"
  done

  f=${extended.config.home-files}
  check "$f/.claude/skills" direct from-dir from-file grilling hc-handoff
  check "$f/.codex/skills" from-dir from-file grilling hc-handoff
  check "$f/.pi/agent/skills" from-dir from-file grilling hc-handoff
  # Codex skips a symlinked SKILL.md (openai/codex#10470); pi gets the same copy.
  for d in .codex/skills .pi/agent/skills; do
    [ ! -L "$(readlink -f "$f/$d/from-file")/SKILL.md" ] || { echo "$d/from-file/SKILL.md is a symlink"; exit 1; }
  done

  f=${piOnly.config.home-files}
  [ ! -e "$f/.claude" ] && [ ! -e "$f/.codex" ]
  check "$f/.pi/agent/skills" claudy-comment-detector grilling hc-handoff
  touch $out
''

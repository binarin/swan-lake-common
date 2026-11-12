{ inputs, self, ... }:
{
  imports = [ inputs.home-manager.flakeModules.home-manager ];

  # Agent skills shared by every coding agent in the home, e.g.
  #
  #   programs.agent-skills.skills.my-skill.source = ./skills/my-skill;
  #   programs.agent-skills.skills.grilling.enable = false;
  #
  # Enabled skills go to Home Manager's `programs.claude-code.skills` and
  # `programs.codex.skills` (used only when that program is enabled), and to
  # ~/.pi/agent/skills, as pi has no Home Manager module.
  flake.homeModules.agent-skills =
    {
      config,
      lib,
      pkgs,
      ...
    }:
    let
      cfg = config.programs.agent-skills;
      inherit (lib) mkOption types;

      enabled = lib.filterAttrs (_: s: s.enable) cfg.skills;
      isDir = s: lib.pathIsDirectory s.source;
      # Append to the source rather than interpolating it: interpolation names
      # a store copy that `nix flake check --no-build` does not create, so
      # reading SKILL.md through it would fail.
      skillFile = s: if isDir s then s.source + "/SKILL.md" else s.source;

      # Reading a built store path during evaluation would be import from
      # derivation; such sources skip the name check.
      needsBuild =
        src:
        lib.any (c: c ? outputs || c.allOutputs or false) (lib.attrValues (builtins.getContext "${src}"));

      frontmatterName =
        text:
        let
          lines = lib.splitString "\n" text;
          body = lib.tail lines;
          end = lib.lists.findFirstIndex (l: l == "---") null body;
          frontmatter =
            if lines != [ ] && lib.head lines == "---" && end != null then lib.take end body else [ ];
          names = lib.concatMap (
            l:
            let
              m = builtins.match "name:[ \t]*[\"']?([^\"']*)[\"']?[ \t]*" l;
            in
            if m == null then [ ] else m
          ) frontmatter;
        in
        if names == [ ] then null else lib.head names;

      piSkillDir =
        s: if isDir s then s.source else pkgs.writeTextDir "SKILL.md" (builtins.readFile s.source);
    in
    {
      key = "swan-lake-common.modules.home.agent-skills";

      options.programs.agent-skills.skills = mkOption {
        type = types.attrsOf (
          types.submodule {
            options = {
              enable = mkOption {
                type = types.bool;
                default = true;
                description = "Whether to install this skill for the coding agents.";
              };
              source = mkOption {
                type = types.path;
                description = ''
                  Skill directory containing {file}`SKILL.md` (plus any
                  scripts, references or assets), or a lone {file}`SKILL.md`.
                  Its frontmatter `name` must equal the attribute name.
                '';
              };
            };
          }
        );
        default = { };
        example = lib.literalExpression ''
          {
            my-skill.source = ./skills/my-skill;
            review-style.source = ./review-style.md;
            grilling.enable = false;
          }
        '';
        description = ''
          Agent skills by name. Two are predefined and enabled:
          `grilling` and `claudy-comment-detector`.
        '';
      };

      config = {
        programs.agent-skills.skills = {
          grilling.source = lib.mkDefault ./_skills/grilling;
          claudy-comment-detector.source = lib.mkDefault (builtins.fetchurl {
            url = "https://raw.githubusercontent.com/citelao/dotfiles/d804f322eac538576230ddeeeafc780542bd98f3/dotfiles/.chezmoitemplates/skills/claudy-comment-detector/SKILL.md";
            sha256 = "sha256-4kE0FLldUuutxrWjmCEU7j38uNb0p0SngFAxG+pncbw=";
          });
        };

        assertions = lib.mapAttrsToList (
          name: s:
          let
            found = frontmatterName (builtins.readFile (skillFile s));
          in
          {
            assertion = needsBuild s.source || found == name;
            message = "programs.agent-skills.skills.${name}: SKILL.md frontmatter ${
              if found == null then "has no name" else "name is `${found}`"
            }, expected `${name}`.";
          }
        ) enabled;

        programs.claude-code.skills = lib.mkIf (enabled != { }) (lib.mapAttrs (_: s: s.source) enabled);
        programs.codex.skills = lib.mkIf (enabled != { }) (lib.mapAttrs (_: s: s.source) enabled);

        home.file = lib.mapAttrs' (
          name: s: lib.nameValuePair ".pi/agent/skills/${name}" { source = piSkillDir s; }
        ) enabled;
      };
    };

  perSystem =
    { pkgs, ... }:
    {
      checks.agent-skills = import ./_tests.nix {
        inherit pkgs inputs;
        module = self.homeModules.agent-skills;
      };
    };
}

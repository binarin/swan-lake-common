{ self, ... }:
{
  perSystem =
    { pkgs, ... }:
    let
      inherit (pkgs) lib;
      module = self.homeModules.remote-session-launchers;

      ok = {
        name = "ok";
        host = "ok.example.com";
        protocol = "ssh";
        remote.shell = { };
      };
      withItems = items: {
        programs.remoteSessionLaunchers = {
          enable = true;
          tags.dev.titleSuffix = " [dev]";
          inherit items;
        };
      };

      # Home Manager's result throws on any failed assertion, so evaluate the
      # module alone with stand-ins for the two Home Manager options it sets.
      eval =
        config:
        (lib.evalModules {
          modules = [
            module
            config
            {
              options.assertions = lib.mkOption { type = lib.types.listOf lib.types.unspecified; };
              options.xdg.dataFile = lib.mkOption { type = lib.types.attrsOf lib.types.unspecified; };
              config._module.args = { inherit pkgs; };
            }
          ];
        }).config;
      failedMessages = config: map (a: a.message) (lib.filter (a: !a.assertion) (eval config).assertions);
      evalFails =
        config:
        !(builtins.tryEval (builtins.deepSeq (eval config).programs.remoteSessionLaunchers.items true))
        .success;

      # Assertion cases: exactly one expected failure, matched by substring.
      assertionCases = {
        unknown-tag = {
          items.a = ok // {
            tag = "prod";
          };
          expect = ''.tag refers to unknown tag "prod"'';
        };
        duplicate-title = {
          items.a = ok // {
            title = "same";
          };
          items.b = ok // {
            title = "sa";
            titleSuffix = "me";
          };
          expect = ''items "a", "b" share the final title "same"'';
        };
        tag-makes-duplicate-title = {
          items.a = ok // {
            title = "x [dev]";
          };
          items.b = ok // {
            title = "x";
            tag = "dev";
          };
          expect = ''share the final title "x [dev]"'';
        };
        bad-id = {
          items."-a" = ok;
          expect = "item id must match";
        };
        bad-id-space = {
          items."a b" = ok;
          expect = "item id must match";
        };
        host-option = {
          items.a = ok // {
            host = "-oProxyCommand=x";
          };
          expect = ".host must not be empty, start with '-'";
        };
        host-space = {
          items.a = ok // {
            host = "a.example.com b";
          };
          expect = ".host must not be empty";
        };
        host-empty = {
          items.a = ok // {
            host = "";
          };
          expect = ".host must not be empty";
        };
        name-newline = {
          items.a = ok // {
            name = "a\nExec=evil";
          };
          expect = ".name must not contain control characters";
        };
        title-control = {
          items.a = ok // {
            titleSuffix = "\t";
          };
          expect = "final title must not contain control characters";
        };
      };

      # Type and definition errors raised by the option types themselves.
      evalCases = {
        bad-color = ok // {
          background = "#001800";
        };
        short-color = ok // {
          background = "0018";
        };
        two-remotes = ok // {
          remote = {
            shell = { };
            herdr = { };
          };
        };
        no-remote = removeAttrs ok [ "remote" ];
        empty-exec = ok // {
          remote.exec = [ ];
        };
        nix-run-without-package = ok // {
          remote.nixRun.args = [ "x" ];
        };
        unknown-protocol = ok // {
          protocol = "telnet";
        };
        unknown-terminal = ok // {
          terminal = "kitty";
        };
      };

      results =
        lib.mapAttrsToList (
          case: c:
          let
            got = failedMessages (withItems (removeAttrs c [ "expect" ]).items);
          in
          {
            inherit case;
            pass = lib.length got == 1 && lib.hasInfix c.expect (lib.head got);
            detail = builtins.toJSON got;
          }
        ) assertionCases
        ++ lib.mapAttrsToList (case: item: {
          inherit case;
          pass = evalFails (withItems {
            a = item;
          });
          detail = "expected an evaluation error";
        }) evalCases
        ++ [
          {
            case = "positive-control";
            pass =
              failedMessages (withItems {
                a = ok;
                b = ok // {
                  tag = "dev";
                };
              }) == [ ]
              && !evalFails (withItems {
                a = ok;
              });
            detail = "a valid configuration must pass";
          }
          {
            case = "disabled-ignores-items";
            pass =
              failedMessages {
                programs.remoteSessionLaunchers.items.a = ok // {
                  tag = "missing";
                };
              } == [ ];
            detail = "assertions apply only when enabled";
          }
        ];
      failures = lib.filter (r: !r.pass) results;
    in
    {
      checks.remote-session-launchers-negative =
        pkgs.runCommand "remote-session-launchers-negative" { }
          ''
            cat <<'EOF'
            ${lib.concatMapStringsSep "\n" (
              r: "${if r.pass then "ok  " else "FAIL"} ${r.case}: ${r.detail}"
            ) results}
            EOF
            ${if failures == [ ] then "touch $out" else "exit 1"}
          '';
    };
}

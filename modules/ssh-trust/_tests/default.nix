{
  pkgs,
  inputs,
  sshTrust,
  nixosModule,
  systemManagerModule,
}:
let
  inherit (pkgs) lib;

  hsmIds = [
    "hsm-tz-ca"
    "hsm-yk-sk-nfc"
    "hsm-yk-usb-a-5-nfc-fixed"
    "hsm-yk-usb-a-5-nfc-keychain"
    "hsm-yk-usb-c-5-nfc"
  ];
  # Direct login only.
  termiusIds = [
    "termius-tablet"
    "termius-phone"
  ];
  loginIds = lib.sort lib.lessThan (hsmIds ++ termiusIds);
  hsmLines = map sshTrust.keyLine hsmIds;
  loginLines = map sshTrust.keyLine loginIds;

  nixos =
    extra:
    (inputs.nixpkgs.lib.nixosSystem {
      modules = [
        nixosModule
        {
          nixpkgs.hostPlatform = pkgs.stdenv.hostPlatform.system;
          boot.loader.grub.enable = false;
          fileSystems."/" = {
            device = "none";
            fsType = "tmpfs";
          };
          system.stateVersion = "26.05";
          networking.hostName = "alpha";
          services.openssh.enable = true;
          swanLake.sshTrust.enable = true;
          users.users.binarin = {
            isNormalUser = true;
            group = "binarin";
          };
          users.groups.binarin = { };
          users.users.svc = {
            isSystemUser = true;
            group = "svc";
          };
          users.groups.svc = { };
        }
        extra
      ];
    }).config;

  # system-manager reuses the nixpkgs users/openssh option shapes; stub just
  # the options the policy touches.
  systemManager =
    extra:
    (lib.evalModules {
      modules = [
        systemManagerModule
        {
          options = {
            assertions = lib.mkOption {
              type = lib.types.listOf lib.types.raw;
              default = [ ];
            };
            services.openssh.settings = lib.mkOption {
              type = lib.types.attrsOf lib.types.str;
              default = { };
            };
            environment.etc = lib.mkOption {
              type = lib.types.attrsOf (
                lib.types.submodule { options.text = lib.mkOption { type = lib.types.lines; }; }
              );
              default = { };
            };
            users.users = lib.mkOption {
              type = lib.types.attrsOf (
                lib.types.submodule {
                  options.openssh.authorizedKeys.keys = lib.mkOption {
                    type = lib.types.listOf lib.types.str;
                    default = [ ];
                  };
                  options.openssh.authorizedPrincipals = lib.mkOption {
                    type = lib.types.listOf lib.types.str;
                    default = [ ];
                  };
                }
              );
              default = { };
            };
          };
          config.swanLake.sshTrust.enable = true;
        }
        extra
      ];
    }).config;

  ssh = cfg: user: {
    keys = cfg.users.users.${user}.openssh.authorizedKeys.keys;
    principals = cfg.users.users.${user}.openssh.authorizedPrincipals;
  };

  failedAssertions = cfg: map (a: a.message) (lib.filter (a: !a.assertion) cfg.assertions);

  throws = v: !(builtins.tryEval (builtins.deepSeq v v)).success;

  base = nixos { };

  cases = {
    registry-roles = {
      actual = {
        login = sshTrust.loginKeyIds;
        ca = sshTrust.userCaKeyIds;
        retired = lib.filter (id: sshTrust.keys ? ${id}) [
          "termius-pixel"
          "termius-tab-s9"
          "trezor-root-pve"
          "trezor-trusted-user-ca"
          "hsm-tz"
        ];
        pkcs11 = sshTrust.keys.pkcs11-raum-root.trust;
        termius = map (id: sshTrust.keys.${id}.trust) termiusIds;
      };
      expected = {
        login = loginIds;
        ca = hsmIds;
        retired = [ ];
        pkcs11 = {
          login = false;
          userCa = false;
        };
        termius = map (_: {
          login = true;
          userCa = false;
        }) termiusIds;
      };
    };

    identity-parsing = {
      actual = map sshTrust.identityOf [
        "${sshTrust.keys.hsm-tz-ca.publicKey} some comment"
        ''restrict,command="a b" ${sshTrust.keys.binarin-at-nixos.publicKey} binarin@nixos''
        sshTrust.keys.hsm-yk-usb-c-5-nfc.publicKey
      ];
      expected = [
        sshTrust.keys.hsm-tz-ca.publicKey
        sshTrust.keys.binarin-at-nixos.publicKey
        sshTrust.keys.hsm-yk-usb-c-5-nfc.publicKey
      ];
    };

    nixos-defaults = {
      actual = {
        root = ssh base "root";
        binarin = ssh base "binarin";
        svc = ssh base "svc";
        ca = base.environment.etc."ssh/trusted_user_ca_keys".text;
        settings = {
          inherit (base.services.openssh.settings) TrustedUserCaKeys AuthorizedPrincipalsFile;
        };
      };
      expected = {
        root = {
          keys = loginLines;
          principals = [ "alpha--root" ];
        };
        binarin = {
          keys = loginLines;
          principals = [ "alpha--binarin" ];
        };
        svc = {
          keys = [ ];
          principals = [ ];
        };
        ca = lib.concatStringsSep "\n" hsmLines + "\n";
        settings = {
          TrustedUserCaKeys = "/etc/ssh/trusted_user_ca_keys";
          AuthorizedPrincipalsFile = "/etc/ssh/authorized_principals.d/%u";
        };
      };
    };

    extra-principals-are-additive = {
      actual = ssh (nixos {
        swanLake.sshTrust.accounts.binarin.extraPrincipals = [
          "root"
          "binarin"
          "root"
        ];
      }) "binarin";
      expected = {
        keys = loginLines;
        principals = [
          "alpha--binarin"
          "root"
          "binarin"
        ];
      };
    };

    explicit-default-disable-keeps-options = {
      actual =
        (ssh (nixos {
          swanLake.sshTrust.accounts.root = {
            includeDefaultPrincipal = false;
            extraPrincipals = [
              {
                name = "root";
                options = ''restrict,command="nix-daemon --stdio"'';
              }
            ];
          };
        }) "root").principals;
      expected = [ ''restrict,command="nix-daemon --stdio" root'' ];
    };

    machine-name-override = {
      actual = (ssh (nixos { swanLake.sshTrust.machineName = "custom"; }) "root").principals;
      expected = [ "custom--root" ];
    };

    service-account-explicit = {
      actual = ssh (nixos {
        swanLake.sshTrust.accounts.svc = {
          trustedLoginKeys = false;
          includeDefaultPrincipal = false;
          directKeys = [
            "brother-scanner"
            {
              key = "binarin-at-nixos";
              options = "restrict";
            }
          ];
        };
      }) "svc";
      expected = {
        keys = [
          (sshTrust.keyLine "brother-scanner")
          "restrict ${sshTrust.keyLine "binarin-at-nixos"}"
        ];
        principals = [ ];
      };
    };

    account-opt-out = {
      actual = ssh (nixos { swanLake.sshTrust.accounts.binarin.enable = false; }) "binarin";
      expected = {
        keys = [ ];
        principals = [ ];
      };
    };

    missing-account-asserts = {
      actual = failedAssertions (nixos {
        swanLake.sshTrust.accounts.ghost = { };
      });
      expected = [ "swanLake.sshTrust.accounts lists accounts that do not exist: ghost" ];
    };

    # Clan's users service assigns the key list at normal priority, which
    # would defeat a mkDefault; normal definitions must concatenate instead.
    composes-with-normal-priority-definitions = {
      actual =
        ssh (nixos {
          users.users.binarin.openssh.authorizedKeys.keys = [ ];
          users.users.root.openssh.authorizedKeys.keys = [ "extra-root-line" ];
        }) "binarin"
        // {
          root =
            (ssh (nixos { users.users.root.openssh.authorizedKeys.keys = [ "extra-root-line" ]; }) "root").keys;
        };
      expected = {
        keys = loginLines;
        principals = [ "alpha--binarin" ];
        root = loginLines ++ [ "extra-root-line" ];
      };
    };

    disabled-policy-is-inert = {
      actual = ssh (nixos { swanLake.sshTrust.enable = lib.mkForce false; }) "root";
      expected = {
        keys = [ ];
        principals = [ ];
      };
    };

    invalid-inputs-throw = {
      actual = {
        principal-with-space = throws (
          ssh (nixos {
            swanLake.sshTrust.accounts.root.extraPrincipals = [ "restrict root" ];
          }) "root"
        );
        principal-with-comma = throws (sshTrust.normalizePrincipal "a,b");
        unknown-key = throws (
          ssh (nixos { swanLake.sshTrust.accounts.root.directKeys = [ "nope" ]; }) "root"
        );
        duplicate-manifest = throws (
          sshTrust.mkManifest [
            base.swanLake.sshTrust.manifestEntry
            base.swanLake.sshTrust.manifestEntry
          ]
        );
      };
      expected = {
        principal-with-space = true;
        principal-with-comma = true;
        unknown-key = true;
        duplicate-manifest = true;
      };
    };

    manifest = {
      actual =
        let
          legacy = nixos {
            networking.hostName = lib.mkForce "beta";
            swanLake.sshTrust.accounts.root = {
              includeDefaultPrincipal = false;
              extraPrincipals = [ "root" ];
            };
          };
          m = sshTrust.mkManifest [
            base.swanLake.sshTrust.manifestEntry
            legacy.swanLake.sshTrust.manifestEntry
          ];
        in
        builtins.removeAttrs m [ "userCaKeys" ]
        // {
          userCaKeys = map (k: k.id) m.userCaKeys;
        };
      expected = {
        schema = "swan-lake-ssh-trust-manifest";
        version = 1;
        userCaKeys = hsmIds;
        machines = {
          alpha = {
            root = {
              defaultPrincipal = "alpha--root";
              principals = [
                {
                  name = "alpha--root";
                  options = null;
                }
              ];
            };
            binarin = {
              defaultPrincipal = "alpha--binarin";
              principals = [
                {
                  name = "alpha--binarin";
                  options = null;
                }
              ];
            };
          };
          beta = {
            root = {
              defaultPrincipal = null;
              principals = [
                {
                  name = "root";
                  options = null;
                }
              ];
            };
            binarin = {
              defaultPrincipal = "beta--binarin";
              principals = [
                {
                  name = "beta--binarin";
                  options = null;
                }
              ];
            };
          };
        };
      };
    };

    system-manager-explicit-accounts =
      let
        cfg = systemManager {
          swanLake.sshTrust.machineName = "murmur";
          swanLake.sshTrust.accounts.allebedev.extraPrincipals = [ "binarin" ];
          swanLake.sshTrust.accounts.binarin = { };
        };
      in
      {
        actual = {
          users = lib.attrNames cfg.users.users;
          root = ssh cfg "root";
          allebedev = (ssh cfg "allebedev").principals;
          ca = cfg.environment.etc."ssh/trusted_user_ca_keys".text;
          manifest = lib.attrNames cfg.swanLake.sshTrust.manifestEntry.accounts;
        };
        expected = {
          users = [
            "allebedev"
            "binarin"
            "root"
          ];
          root = {
            keys = loginLines;
            principals = [ "murmur--root" ];
          };
          allebedev = [
            "murmur--allebedev"
            "binarin"
          ];
          ca = lib.concatStringsSep "\n" hsmLines + "\n";
          manifest = [
            "allebedev"
            "binarin"
            "root"
          ];
        };
      };

    system-manager-needs-machine-name = {
      actual = throws (systemManager { }).users.users.root.openssh.authorizedPrincipals;
      expected = true;
    };
  };

  failures = lib.filterAttrs (_: c: c.actual != c.expected) cases;
  report = lib.concatStringsSep "\n" (
    lib.mapAttrsToList (name: c: ''
      FAIL ${name}
        expected: ${builtins.toJSON c.expected}
        actual:   ${builtins.toJSON c.actual}'') failures
  );
in
{
  ssh-trust =
    pkgs.runCommand "ssh-trust-tests"
      {
        passAsFile = [ "report" ];
        inherit report;
      }
      ''
        if [ -s "$reportPath" ]; then
          cat "$reportPath"
          exit 1
        fi
        echo "${toString (builtins.length (builtins.attrNames cases))} ssh-trust cases passed" > $out
      '';
}

# SSH user trust policy, shared by the NixOS and system-manager modules.
#
# NixOS covers root and every `isNormalUser` account automatically; other
# accounts must be listed in `swanLake.sshTrust.accounts`. system-manager hosts
# do not declare their accounts, so there coverage is root plus the accounts
# listed explicitly. Accounts are never created by this module.
{ sshTrust, platform }:
{
  lib,
  config,
  options,
  ...
}:
let
  inherit (lib) mkOption types;
  cfg = config.swanLake.sshTrust;

  optionsEntry =
    nameOption:
    types.submodule {
      options = {
        ${nameOption} = mkOption { type = types.str; };
        options = mkOption {
          type = types.nullOr types.str;
          default = null;
          description = "authorized_keys/authorized_principals options, such as `restrict,command=\"...\"`.";
        };
      };
    };

  accountModule = {
    options = {
      enable = mkOption {
        type = types.bool;
        default = true;
        description = "Whether this account is covered by the trust policy.";
      };
      trustedLoginKeys = mkOption {
        type = types.bool;
        default = true;
        description = "Install the registry keys with the universal `login` role as direct keys.";
      };
      directKeys = mkOption {
        type = types.listOf (types.either types.str (optionsEntry "key"));
        default = [ ];
        description = "Additional registry key ids granted for direct login to this account on this machine.";
      };
      includeDefaultPrincipal = mkOption {
        type = types.bool;
        default = true;
        description = "Accept certificates for the `MACHINE--ACCOUNT` principal.";
      };
      extraPrincipals = mkOption {
        type = types.listOf (types.either types.str (optionsEntry "name"));
        default = [ ];
        description = ''
          Additional accepted certificate principals. They add to the default
          principal; only `includeDefaultPrincipal = false` removes it.
        '';
      };
    };
  };

  accountDefaults = {
    enable = true;
    trustedLoginKeys = true;
    directKeys = [ ];
    includeDefaultPrincipal = true;
    extraPrincipals = [ ];
  };

  account = name: cfg.accounts.${name} or accountDefaults;

  autoCovered = name: user: name == "root" || (cfg.coverNormalUsers && (user.isNormalUser or false));

  covered = name: user: (cfg.accounts ? ${name} || autoCovered name user) && (account name).enable;

  principalsOf =
    name:
    let
      acc = account name;
    in
    lib.optional acc.includeDefaultPrincipal (
      sshTrust.normalizePrincipal (sshTrust.defaultPrincipal cfg.machineName name)
    )
    ++ map sshTrust.normalizePrincipal acc.extraPrincipals;

  keyLinesOf =
    name:
    let
      acc = account name;
    in
    lib.unique (
      lib.optionals acc.trustedLoginKeys sshTrust.loginKeyLines
      ++ map (g: sshTrust.directKeyLine (sshTrust.normalizeDirectKey g)) acc.directKeys
    );

  opensshFor = name: {
    authorizedKeys.keys = keyLinesOf name;
    authorizedPrincipals = lib.unique (map sshTrust.principalLine (principalsOf name));
  };

  coveredNames = lib.filter (n: covered n config.users.users.${n}) (lib.attrNames config.users.users);

  hasHostName = options ? networking.hostName;
in
{
  # Extending the users submodule lets coverage depend on `isNormalUser`
  # without defining `users.users` in terms of itself.
  options.users = lib.optionalAttrs (platform == "nixos") {
    users = mkOption {
      type = types.attrsOf (
        types.submodule (
          { name, config, ... }:
          {
            config.openssh = lib.mkIf (cfg.enable && covered name config) (opensshFor name);
          }
        )
      );
    };
  };

  options.swanLake.sshTrust = {
    enable = lib.mkEnableOption "the shared SSH user trust policy";

    machineName =
      mkOption {
        type = types.str;
        description = "Machine part of the default `MACHINE--ACCOUNT` principal.";
      }
      // lib.optionalAttrs hasHostName {
        default = config.networking.hostName;
        defaultText = lib.literalExpression "config.networking.hostName";
      };

    coverNormalUsers = mkOption {
      type = types.bool;
      default = platform == "nixos";
      defaultText = lib.literalMD "`true` on NixOS, `false` on system-manager";
      description = "Cover every `isNormalUser` account in addition to root and listed accounts.";
    };

    accounts = mkOption {
      type = types.attrsOf (types.submodule accountModule);
      default = { };
      description = "Per-account policy. Listing an account covers it.";
    };

    userCaKeyLines = mkOption {
      type = types.listOf types.str;
      readOnly = true;
      default = sshTrust.userCaKeyLines;
      description = "Trusted user CA keys, for applications that need them besides sshd.";
    };

    manifestEntry = mkOption {
      type = types.raw;
      readOnly = true;
      description = "This machine's entry for `lib.sshTrust.mkManifest`.";
      default = {
        machine = cfg.machineName;
        accounts = lib.genAttrs coveredNames (
          name:
          let
            acc = account name;
          in
          {
            defaultPrincipal =
              if acc.includeDefaultPrincipal then sshTrust.defaultPrincipal cfg.machineName name else null;
            principals = principalsOf name;
          }
        );
      };
    };
  };

  config = lib.mkMerge [
    (lib.mkIf cfg.enable {
      assertions = [
        (
          let
            missing = lib.filter (n: (cfg.accounts.${n}).enable && !(config.users.users ? ${n})) (
              lib.attrNames cfg.accounts
            );
          in
          {
            assertion = platform != "nixos" || missing == [ ];
            message = "swanLake.sshTrust.accounts lists accounts that do not exist: ${lib.concatStringsSep ", " missing}";
          }
        )
      ];

      services.openssh.settings.TrustedUserCaKeys = "/etc/ssh/trusted_user_ca_keys";
      services.openssh.settings.AuthorizedPrincipalsFile = "/etc/ssh/authorized_principals.d/%u";
      environment.etc."ssh/trusted_user_ca_keys".text =
        lib.concatStringsSep "\n" sshTrust.userCaKeyLines + "\n";
    })

    (lib.optionalAttrs (platform != "nixos") {
      users.users = lib.mkIf cfg.enable (
        lib.genAttrs
          (lib.filter (n: (account n).enable) (lib.unique ([ "root" ] ++ lib.attrNames cfg.accounts)))
          (name: {
            openssh = opensshFor name;
          })
      );
    })
  ];
}

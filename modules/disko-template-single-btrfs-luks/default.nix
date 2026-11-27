{ self, inputs, ... }:
{
  # The layout of disko-template-single-btrfs with the btrfs partition inside
  # LUKS (`cryptroot`), unlocked in a systemd initrd by the TPM 2.0 through
  # clevis, or by its passphrase typed over SSH in the initrd as a fallback.
  # A machine imports this module and sets `disko.devices.disk.main.device`
  # and `swanLake.encryptedRoot.initrdSsh.authorizedKeys`; it adds the drivers
  # of its network cards to `boot.initrd.availableKernelModules`.
  #
  # Installing (swan-lake's `provision`, through nixos-anywhere): the passphrase
  # goes to `installPassphraseFile` on the installer (`--disk-encryption-keys`),
  # which only formatting reads, and the initrd SSH host key to
  # `initrdSsh.hostKey` on the new root (`--extra-files`). The bootloader
  # install appends that key to the initrd on the ESP, so it never enters the
  # Nix store, and it is read again from the root on every rebuild.
  #
  # The initrd brings every physical Ethernet port up with DHCP and runs sshd
  # for root on `initrdSsh.port`, which keeps its host key apart from the
  # system's port 22 in known_hosts. To unlock by hand:
  #   ssh -t -p 2222 root@HOST systemd-tty-ask-password-agent --query
  # It asks as often as needed (tries=0). If the unlock is stuck anyway:
  #   ssh -p 2222 root@HOST systemctl restart systemd-cryptsetup@cryptroot.service
  # The passphrase stays what disko formatted with: changing it later takes
  # `cryptsetup luksChangeKey` on the machine and a new JWE.
  #
  # TPM unlock stays off while `clevisSecretFile` is null: every boot asks for
  # the passphrase. Enrol from a workstation once the machine runs, with the
  # passphrase on stdin and the JWE written locally:
  #   PASSPHRASE | ssh HOST "sudo clevis encrypt tpm2 '{\"pcr_bank\":\"sha256\",\"pcr_ids\":\"7\"}'" > HOST.jwe
  # Check that the TPM gives the passphrase back:
  #   ssh HOST 'sudo sh -c "clevis decrypt | cryptsetup open --test-passphrase --key-file - /dev/disk/by-partlabel/disk-main-luks"' < HOST.jwe
  # then set `clevisSecretFile` to the JWE and deploy. The JWE is sealed to
  # this TPM and useless elsewhere; it sits unencrypted on the ESP anyway.
  #
  # PCR 7 (the Secure Boot state and key databases) survives kernel, initrd
  # and bootloader updates. A firmware update that changes those databases,
  # or turning Secure Boot on or off, makes clevis fail: the boot asks for the
  # passphrase and the JWE must be made again. With Secure Boot off PCR 7 does
  # not bind what boots, so another OS booted on the same machine can unseal
  # the passphrase: this guards the disk away from its machine, not the
  # machine itself. clevis seals with `noda`, so failed unseals never count
  # towards the TPM's dictionary-attack lockout.
  flake.nixosModules.disko-template-single-btrfs-luks =
    { config, lib, ... }:
    let
      cfg = config.swanLake.encryptedRoot;
      inherit (lib) mkOption types;
    in
    {
      key = "swan-lake-common.modules.nixos.disko-template-single-btrfs-luks";
      imports = [ inputs.disko.nixosModules.default ];

      options.swanLake.encryptedRoot = {
        installPassphraseFile = mkOption {
          type = types.str;
          default = "/tmp/disk-main-luks.key";
          description = ''
            Path on the installer of the file holding the LUKS passphrase,
            without a trailing newline, that disko formats `cryptroot` with.
            Booting never reads it.
          '';
        };
        clevisSecretFile = mkOption {
          type = types.nullOr types.path;
          default = null;
          description = ''
            Clevis JWE, sealed to this machine's TPM 2.0, that holds the LUKS
            passphrase. When null, TPM unlock is off and every boot asks for
            the passphrase.
          '';
        };
        initrdSsh = {
          port = mkOption {
            type = types.port;
            default = 2222;
            description = "Port of the initrd sshd.";
          };
          hostKey = mkOption {
            type = types.str;
            default = "/etc/secrets/initrd/ssh_host_ed25519_key";
            description = ''
              Path on the machine's root of the initrd sshd's ed25519 host
              key, a key of its own: the initrd on the unencrypted ESP holds
              it in plain text.
            '';
          };
          authorizedKeys = mkOption {
            type = types.listOf types.str;
            description = "authorized_keys lines that may log in to the initrd as root.";
          };
        };
      };

      config = lib.mkMerge [
        {
          assertions = [
            {
              assertion =
                lib.hasPrefix "/" cfg.initrdSsh.hostKey && !lib.hasPrefix builtins.storeDir cfg.initrdSsh.hostKey;
              message = "swanLake.encryptedRoot.initrdSsh.hostKey must be an absolute path outside the Nix store";
            }
          ];

          disko.devices.disk.main = {
            type = "disk";
            content = {
              type = "gpt";
              partitions = {
                boot = {
                  size = "1M";
                  type = "EF02";
                };
                ESP = {
                  size = "1G";
                  type = "EF00";
                  content = {
                    type = "filesystem";
                    format = "vfat";
                    mountpoint = "/boot";
                    mountOptions = [ "umask=0077" ];
                  };
                };
                luks = {
                  size = "100%";
                  content = {
                    type = "luks";
                    name = "cryptroot";
                    passwordFile = cfg.installPassphraseFile;
                    settings.allowDiscards = true;
                    content = {
                      type = "btrfs";
                      # The subvolumes of disko-template-single-btrfs.
                      subvolumes = {
                        "/root" = {
                          mountpoint = "/";
                          mountOptions = [ "compress=zstd" ];
                        };
                        "/nix" = {
                          mountpoint = "/nix";
                          mountOptions = [
                            "compress=zstd"
                            "noatime"
                          ];
                        };
                        "/var/lib/docker" = {
                          mountpoint = "/var/lib/docker";
                          mountOptions = [ "compress=zstd" ];
                        };
                      };
                    };
                  };
                };
              };
            };
          };

          # systemd-cryptsetup allows 3 tries by default, and a failed clevis
          # key file uses one up: typos over SSH must not end the boot.
          boot.initrd.luks.devices.cryptroot.crypttabExtraOpts = [ "tries=0" ];

          boot.initrd.systemd.enable = true;
          # Only these settings: boot.initrd.network.enable would add the
          # stage 2 networking.* interfaces to the initrd.
          boot.initrd.systemd.network = {
            enable = true;
            networks."10-initrd-ethernet" = {
              matchConfig = {
                Type = "ether";
                Kind = "!*";
              };
              networkConfig.DHCP = "yes";
              # The initrd's machine-id, and so its DUID, is new on every boot;
              # the MAC as client id keeps one lease per port.
              dhcpV4Config.ClientIdentifier = "mac";
            };
          };
          boot.initrd.network.ssh = {
            enable = true;
            inherit (cfg.initrdSsh) port authorizedKeys;
            hostKeys = [ cfg.initrdSsh.hostKey ];
          };

          # Once the TPM unlocks, an edited kernel command line such as
          # init=/bin/sh would be a root shell for anyone at the console.
          boot.loader.systemd-boot.editor = lib.mkDefault false;

          # For enrolment: clevis encrypt tpm2 on the running system.
          environment.systemPackages = [ config.boot.initrd.clevis.package ];
        }
        (lib.mkIf (cfg.clevisSecretFile != null) {
          boot.initrd.clevis = {
            enable = true;
            devices.cryptroot.secretFile = cfg.clevisSecretFile;
          };
        })
      ];
    };

  perSystem =
    { pkgs, lib, ... }:
    let
      device = "/dev/disk/by-id/test-disk";
      authorizedKey = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIDPQXmEVMVLmeFRyafKMVWgPDkv8/uRBTwmcEDatZzMD test";
      jwe = pkgs.writeText "test.jwe" "not a real JWE";
      evalWith =
        extraModules:
        (inputs.nixpkgs.lib.nixosSystem {
          modules = [
            self.nixosModules.disko-template-single-btrfs-luks
            {
              nixpkgs.hostPlatform = pkgs.stdenv.hostPlatform.system;
              system.stateVersion = "26.05";
              disko.devices.disk.main.device = device;
              swanLake.encryptedRoot.initrdSsh.authorizedKeys = [ authorizedKey ];
            }
          ]
          ++ extraModules;
        }).config;

      summarize = config: {
        fileSystems =
          lib.mapAttrs
            (_: fs: {
              inherit (fs) device fsType options;
            })
            (
              lib.filterAttrs (
                n: _:
                lib.elem n [
                  "/"
                  "/nix"
                  "/var/lib/docker"
                  "/boot"
                ]
              ) config.fileSystems
            );
        mountpoints = lib.sort lib.lessThan (lib.attrNames config.fileSystems);
        luksDevices = lib.mapAttrs (_: d: {
          inherit (d)
            device
            allowDiscards
            keyFile
            crypttabExtraOpts
            ;
        }) config.boot.initrd.luks.devices;
        grubDevices = config.boot.loader.grub.devices;
        diskDevice = config.disko.devices.disk.main.device;
        systemdInitrd = config.boot.initrd.systemd.enable;
        initrdNetworks = lib.mapAttrs (_: n: {
          inherit (n) matchConfig networkConfig dhcpV4Config;
        }) config.boot.initrd.systemd.network.networks;
        initrdSsh = {
          inherit (config.boot.initrd.network.ssh)
            enable
            port
            hostKeys
            authorizedKeys
            ;
        };
        # Sources as strings: a path in the store would be a store path here.
        initrdSecrets = lib.mapAttrs (_: toString) config.boot.initrd.secrets;
        clevis = {
          inherit (config.boot.initrd.clevis) enable useTang;
          devices = lib.mapAttrs (_: d: toString d.secretFile) config.boot.initrd.clevis.devices;
        };
        bootEntryEditor = config.boot.loader.systemd-boot.editor;
        clevisInstalled = lib.elem config.boot.initrd.clevis.package config.environment.systemPackages;
        installPassphraseFile = config.disko.devices.disk.main.content.partitions.luks.content.passwordFile;
      };

      expectedWith =
        clevis:
        let
          btrfs = "/dev/mapper/cryptroot";
          hostKey = "/etc/secrets/initrd/ssh_host_ed25519_key";
        in
        {
          fileSystems = {
            "/" = {
              device = btrfs;
              fsType = "btrfs";
              options = [
                "x-initrd.mount"
                "compress=zstd"
                "subvol=/root"
              ];
            };
            "/nix" = {
              device = btrfs;
              fsType = "btrfs";
              options = [
                "x-initrd.mount"
                "compress=zstd"
                "noatime"
                "subvol=/nix"
              ];
            };
            "/var/lib/docker" = {
              device = btrfs;
              fsType = "btrfs";
              options = [
                "compress=zstd"
                "subvol=/var/lib/docker"
              ];
            };
            "/boot" = {
              device = "/dev/disk/by-partlabel/disk-main-ESP";
              fsType = "vfat";
              options = [ "umask=0077" ];
            };
          };
          mountpoints = [
            "/"
            "/boot"
            "/nix"
            "/var/lib/docker"
          ];
          luksDevices.cryptroot = {
            device = "/dev/disk/by-partlabel/disk-main-luks";
            allowDiscards = true;
            keyFile = if clevis then "/clevis-cryptroot/decrypted" else null;
            crypttabExtraOpts = [ "tries=0" ];
          };
          grubDevices = [ device ];
          diskDevice = device;
          systemdInitrd = true;
          initrdNetworks."10-initrd-ethernet" = {
            matchConfig = {
              Type = "ether";
              Kind = "!*";
            };
            networkConfig.DHCP = "yes";
            dhcpV4Config.ClientIdentifier = "mac";
          };
          initrdSsh = {
            enable = true;
            port = 2222;
            hostKeys = [ hostKey ];
            authorizedKeys = [ authorizedKey ];
          };
          initrdSecrets = {
            ${hostKey} = hostKey;
          }
          // lib.optionalAttrs clevis { "/etc/clevis/cryptroot.jwe" = toString jwe; };
          clevis = {
            enable = clevis;
            useTang = false;
            devices = lib.optionalAttrs clevis { cryptroot = toString jwe; };
          };
          bootEntryEditor = false;
          clevisInstalled = true;
          installPassphraseFile = "/tmp/disk-main-luks.key";
        };

      cases = {
        template-only = {
          actual = summarize (evalWith [ ]);
          expected = expectedWith false;
        };
        # Consumers that already import disko themselves.
        with-direct-disko-import = {
          actual = summarize (evalWith [ inputs.disko.nixosModules.default ]);
          expected = expectedWith false;
        };
        with-clevis = {
          actual = summarize (evalWith [ { swanLake.encryptedRoot.clevisSecretFile = jwe; } ]);
          expected = expectedWith true;
        };
      };

      failures = lib.filterAttrs (_: c: c.actual != c.expected) cases;

      failedAssertions = config: map (a: a.message) (lib.filter (a: !a.assertion) config.assertions);
      baseFailedAssertions = failedAssertions (evalWith [ ]);
      # A host key in the store would be world-readable there.
      storeHostKeyRejected = lib.any (lib.hasInfix "initrdSsh.hostKey must be") (
        failedAssertions (evalWith [
          { swanLake.encryptedRoot.initrdSsh.hostKey = "${pkgs.writeText "key" ""}"; }
        ])
      );
    in
    {
      checks.disko-template-single-btrfs-luks =
        assert lib.assertMsg (failures == { }) ''
          disko-template-single-btrfs-luks: unexpected evaluation result
          ${builtins.toJSON failures}
        '';
        assert lib.assertMsg (
          baseFailedAssertions == [ ]
        ) "disko-template-single-btrfs-luks: failing assertions: ${builtins.toJSON baseFailedAssertions}";
        assert lib.assertMsg storeHostKeyRejected
          "disko-template-single-btrfs-luks: a host key in the store is not rejected";
        pkgs.writeText "disko-template-single-btrfs-luks-check.json" (
          builtins.toJSON (lib.mapAttrs (_: c: c.actual) cases)
        );

      # Not a flake check (CI runners lack kvm); run it with
      # `nix build -L .#legacyPackages.x86_64-linux.disko-template-single-btrfs-luks-vm-test`.
      legacyPackages.disko-template-single-btrfs-luks-vm-test = import ./_vm-test.nix {
        inherit pkgs inputs;
        module = self.nixosModules.disko-template-single-btrfs-luks;
      };
    };
}

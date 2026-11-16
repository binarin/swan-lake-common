{ self, inputs, ... }:
{
  # Single-disk layout: BIOS boot partition, 1G ESP on /boot and one btrfs
  # partition with subvolumes for /, /nix and /var/lib/docker. A machine
  # imports this module and sets `disko.devices.disk.main.device`.
  #
  # disko's module is imported by store path, so the module system
  # deduplicates it when a consumer whose disko follows common/disko also
  # imports `inputs.disko.nixosModules.default` directly.
  flake.nixosModules.disko-template-single-btrfs =
    { ... }:
    {
      key = "swan-lake-common.modules.nixos.disko-template-single-btrfs";
      imports = [ inputs.disko.nixosModules.default ];
      config = {
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
              all = {
                size = "100%";
                content = {
                  type = "btrfs";
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
    };

  perSystem =
    { pkgs, lib, ... }:
    let
      device = "/dev/disk/by-id/test-disk";
      evalWith =
        extraModules:
        (inputs.nixpkgs.lib.nixosSystem {
          modules = [
            self.nixosModules.disko-template-single-btrfs
            {
              nixpkgs.hostPlatform = pkgs.stdenv.hostPlatform.system;
              system.stateVersion = "26.05";
              disko.devices.disk.main.device = device;
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
        grubDevices = config.boot.loader.grub.devices;
        diskDevice = config.disko.devices.disk.main.device;
      };

      expected =
        let
          btrfs = "/dev/disk/by-partlabel/disk-main-all";
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
          grubDevices = [ device ];
          diskDevice = device;
        };

      cases = {
        template-only = summarize (evalWith [ ]);
        # Consumers that already import disko themselves.
        with-direct-disko-import = summarize (evalWith [ inputs.disko.nixosModules.default ]);
      };

      failures = lib.filterAttrs (_: actual: actual != expected) cases;
    in
    {
      checks.disko-template-single-btrfs =
        assert lib.assertMsg (failures == { }) ''
          disko-template-single-btrfs: unexpected evaluation result
          expected: ${builtins.toJSON expected}
          actual: ${builtins.toJSON failures}
        '';
        pkgs.writeText "disko-template-single-btrfs-check.json" (builtins.toJSON cases);
    };
}

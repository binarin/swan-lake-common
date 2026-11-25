# Installs a system built on disko-template-single-btrfs-luks onto an empty
# disk the way swan-lake's provision does through nixos-anywhere (disko with
# the passphrase file, then the initrd host key as an extra file, then
# nixos-install's bootloader step), and boots it under OVMF with a software
# TPM:
#   1. clevis is enabled with a JWE that no TPM opens, so the initrd falls back
#      to the passphrase, which the test types over the initrd's SSH;
#   2. on the running system the passphrase is sealed to PCR 7 with
#      `clevis encrypt tpm2`, the bootloader is reinstalled, and the next boot
#      unlocks without anyone typing.
# The JWE is a path on the machine here, so that one system covers both boots;
# a machine that commits its JWE gives a path in its flake instead, which
# reaches the initrd through the same boot.initrd.secrets.
{
  pkgs,
  inputs,
  module,
}:
let
  inherit (pkgs) lib;
  sshKeys = import (pkgs.path + "/nixos/tests/ssh-keys.nix") pkgs;
  qemu-common = import (pkgs.path + "/nixos/lib/qemu-common.nix") {
    inherit lib;
    inherit (pkgs) stdenv;
  };
  passphrase = "correct horse battery staple";
  jwe = "/etc/secrets/initrd/cryptroot.jwe";

  installed =
    (inputs.nixpkgs.lib.nixosSystem {
      modules = [
        module
        (
          { modulesPath, ... }:
          {
            imports = [
              (modulesPath + "/testing/test-instrumentation.nix")
              (modulesPath + "/profiles/qemu-guest.nix")
            ];
            nixpkgs.hostPlatform = pkgs.stdenv.hostPlatform.system;
            system.stateVersion = "26.05";
            documentation.enable = false;
            networking.hostName = "encrypted";

            # The installer's empty disk; the installed system finds its
            # partitions by label, so it boots from whatever the disk is called.
            disko.devices.disk.main.device = "/dev/vdb";
            # Quick key derivation, for the test only.
            disko.devices.disk.main.content.partitions.luks.content.extraFormatArgs = [ "--iter-time=1" ];
            swanLake.encryptedRoot = {
              initrdSsh.authorizedKeys = [ sshKeys.snakeOilPublicKey ];
              clevisSecretFile = jwe;
            };

            boot.loader.systemd-boot.enable = true;
            # Both VMs mount the host's store, as disko's install tests do.
            fileSystems."/nix/store" = {
              device = "nix-store";
              fsType = "9p";
              neededForBoot = true;
              options = [
                "trans=virtio"
                "version=9p2000.L"
                "cache=loose"
              ];
            };
          }
        )
      ];
    }).config;
  inherit (installed.system.build) toplevel diskoScript;
  closure = pkgs.closureInfo { rootPaths = [ toplevel ]; };
  qemu = qemu-common.qemuBinary pkgs.qemu_test;
in
pkgs.testers.runNixOSTest {
  name = "disko-template-single-btrfs-luks";

  nodes.installer = {
    virtualisation.emptyDiskImages = [ 4096 ];
    virtualisation.memorySize = 2048;
    documentation.enable = false;
    nix.settings.substituters = lib.mkForce [ ];
    system.stateVersion = "26.05";
  };

  testScript = ''
    import os
    import shlex
    import shutil
    import socket
    import subprocess

    tmp = os.environ.get("NIX_BUILD_TOP", "/tmp")
    passphrase = ${builtins.toJSON passphrase}

    installer.start()
    installer.wait_for_unit("multi-user.target")
    installer.succeed("udevadm settle")

    with subtest("format with the template, the passphrase in its install file"):
        installer.succeed(f"printf %s {shlex.quote(passphrase)} > ${installed.swanLake.encryptedRoot.installPassphraseFile}")
        installer.succeed("${diskoScript}")
        installer.succeed("test -b /dev/mapper/cryptroot")
        installer.succeed("findmnt /mnt/boot")

    with subtest("install with the initrd host key and a JWE that no TPM opens"):
        installer.succeed(
            "install -d -m 700 /mnt/etc/secrets/initrd",
            "install -m 600 ${sshKeys.snakeOilEd25519PrivateKey} /mnt${installed.swanLake.encryptedRoot.initrdSsh.hostKey}",
            "echo not-a-jwe > /mnt${jwe}",
            "mkdir -p /mnt/nix/store",
            "mount --bind /nix/store /mnt/nix/store",
            "nix-store --load-db < ${closure}/registration",
            "touch /mnt/etc/NIXOS",
            "mkdir -p /mnt/nix/var/nix/profiles",
            "nix-env -p /mnt/nix/var/nix/profiles/system --set ${toplevel}",
            "NIXOS_INSTALL_BOOTLOADER=1 nixos-enter --root /mnt -- ${toplevel}/bin/switch-to-configuration boot",
        )
        installer.succeed("sync")
        installer.shutdown()

    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        port = s.getsockname()[1]

    os.mkdir(f"{tmp}/swtpm")
    vars_file = f"{tmp}/OVMF_VARS.fd"
    shutil.copy("${pkgs.OVMFFull.variables}", vars_file)
    os.chmod(vars_file, 0o644)

    class Tpm:
        proc = None

        def start(self):
            if self.proc is not None and self.proc.poll() is None:
                return
            if os.path.exists(f"{tmp}/swtpm-sock"):
                os.unlink(f"{tmp}/swtpm-sock")
            self.proc = subprocess.Popen([
                "${pkgs.swtpm}/bin/swtpm", "socket",
                "--tpmstate", f"dir={tmp}/swtpm",
                "--ctrl", f"type=unixio,path={tmp}/swtpm-sock",
                "--tpm2",
            ])
            retry(lambda _: os.path.exists(f"{tmp}/swtpm-sock"))

    tpm = Tpm()

    machine = create_machine(
        start_command=" ".join([
            "${qemu}",
            "-m 2048",
            "-virtfs local,path=/nix/store,security_model=none,mount_tag=nix-store",
            f"-drive file={installer.state_dir}/empty0.qcow2,id=drive1,if=none,index=1,werror=report",
            "-device virtio-blk-pci,drive=drive1",
            # OVMFFull has the TPM measurements; it needs SMM, so q35.
            "-machine q35,smm=on",
            "-global driver=cfi.pflash01,property=secure,value=on",
            "-drive if=pflash,format=raw,unit=0,readonly=on,file=${pkgs.OVMFFull.firmware}",
            f"-drive if=pflash,format=raw,unit=1,file={vars_file}",
            f"-chardev socket,id=chrtpm,path={tmp}/swtpm-sock",
            "-tpmdev emulator,id=tpm0,chardev=chrtpm",
            "-device tpm-tis,tpmdev=tpm0",
            f"-netdev user,id=net0,hostfwd=tcp:127.0.0.1:{port}-:${toString installed.swanLake.encryptedRoot.initrdSsh.port}",
            "-device virtio-net-pci,netdev=net0",
        ]),
        name="encrypted",
    )
    driver.machines_qemu.append(machine)

    client_key = f"{tmp}/id_ecdsa"
    shutil.copy("${sshKeys.snakeOilPrivateKey}", client_key)
    os.chmod(client_key, 0o600)
    known_hosts = f"{tmp}/known_hosts"
    with open(known_hosts, "w") as f:
        f.write(f"[127.0.0.1]:{port} ${sshKeys.snakeOilEd25519PublicKey}\n")

    def initrd_ssh(command, stdin=None, tty=False):
        args = [
            "${pkgs.openssh}/bin/ssh", "-F", "/dev/null",
            "-i", client_key, "-o", "IdentitiesOnly=yes",
            "-o", f"UserKnownHostsFile={known_hosts}", "-o", "StrictHostKeyChecking=yes",
            "-o", "ConnectTimeout=5", "-p", str(port),
        ]
        if tty:
            args.append("-tt")
        return subprocess.run(
            args + ["root@127.0.0.1", command],
            input=stdin, capture_output=True, text=True, timeout=60,
        )

    with subtest("first boot: clevis fails, the passphrase over initrd SSH unlocks"):
        tpm.start()
        machine.start()
        retry(lambda _: initrd_ssh("test -e /etc/initrd-release").returncode == 0, timeout_seconds=300)
        # The initrd has a shell and systemd, but no grep.
        ask_pending = "for f in /run/systemd/ask-password/ask.*; do test -e \"$f\" && exit 0; done; exit 1"
        retry(lambda _: initrd_ssh(ask_pending).returncode == 0, timeout_seconds=120)
        result = initrd_ssh("systemctl is-failed cryptsetup-clevis-cryptroot.service")
        assert result.stdout.strip() == "failed", f"clevis did not fail: {result.stdout!r} {result.stderr!r}"
        result = initrd_ssh("systemd-tty-ask-password-agent --query", stdin=passphrase + "\n", tty=True)
        print(result.stdout, result.stderr)
        machine.wait_for_unit("multi-user.target")
        machine.succeed("findmnt -n -o SOURCE / | grep -F '/dev/mapper/cryptroot[/root]'")

    with subtest("enrol the TPM on PCR 7 and reinstall the bootloader"):
        machine.succeed(
            f"printf %s {shlex.quote(passphrase)}"
            " | clevis encrypt tpm2 '{\"pcr_bank\":\"sha256\",\"pcr_ids\":\"7\"}' > ${jwe}"
        )
        machine.succeed(
            "clevis decrypt < ${jwe}"
            " | cryptsetup open --test-passphrase --key-file - /dev/disk/by-partlabel/disk-main-luks"
        )
        # What `switch-to-configuration boot` does for the initrd, which this
        # VM cannot run whole (nix-env cannot open the 9p store): a fresh copy
        # of the initrd on the ESP, with the secrets appended from the root.
        machine.succeed(
            "initrd=/boot$(awk '$1 == \"initrd\" { print $2 }' /boot/loader/entries/nixos-generation-1.conf)"
            " && cp ${toplevel}/initrd \"$initrd\""
            " && ${toplevel}/append-initrd-secrets \"$initrd\""
        )
        machine.succeed("sync")
        machine.shutdown()

    with subtest("second boot: the TPM unlocks"):
        tpm.start()
        machine.start()
        machine.wait_for_unit("multi-user.target")
        machine.succeed("findmnt -n -o SOURCE / | grep -F '/dev/mapper/cryptroot[/root]'")
        machine.succeed("journalctl -b -u cryptsetup-clevis-cryptroot.service | grep -F 'Finished'")
        machine.fail("journalctl -b | grep -F 'Failed to activate with key file'")
  '';
}

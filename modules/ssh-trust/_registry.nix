# Public SSH user/authentication keys.
#
# Only public material belongs here. `trust` holds the universal roles; a key
# without them is never installed anywhere unless a consumer grants it
# explicitly for a machine and account.
{
  schemaVersion = 1;

  keys = {
    hsm-tz = {
      description = "Trezor SSH identity (formerly trezor-root-pve, root@pve)";
      publicKey = "ecdsa-sha2-nistp256 AAAAE2VjZHNhLXNoYTItbmlzdHAyNTYAAAAIbmlzdHAyNTYAAABBBAAHsiCVekjyoPDPOGPJFJ5z9FQUfSmThnGWHABNfjz0APf/qcdHHy+Yj2bG0ICcDbBrE10MJiQjrnYRlKUGvAE=";
      trust = {
        login = true;
        userCa = true;
      };
    };

    hsm-tz-ca = {
      description = "Trezor SSH identity (formerly trezor-trusted-user-ca)";
      publicKey = "ecdsa-sha2-nistp256 AAAAE2VjZHNhLXNoYTItbmlzdHAyNTYAAAAIbmlzdHAyNTYAAABBBKx4e7PnTrPabkbTwYVsCum+j/Fxgs46NR2ltvgqmUnsJyxVm42rhoqNBp9+1D2Xt0WxikWUBXpe5liSPZzFleo=";
      trust = {
        login = true;
        userCa = true;
      };
    };

    hsm-yk-usb-c-5-nfc = {
      description = "Resident FIDO2 key on YubiKey 5 NFC (USB-C, keychain)";
      publicKey = "sk-ssh-ed25519@openssh.com AAAAGnNrLXNzaC1lZDI1NTE5QG9wZW5zc2guY29tAAAAIDaRvW2TCKgLVQuHAv5FyNqQuFidJ+5WhIaV9j6nGU3HAAAABHNzaDo=";
      comment = "usb-c yubi - residen ssh fido2";
      trust = {
        login = true;
        userCa = true;
      };
    };

    hsm-yk-usb-a-5-nfc-fixed = {
      description = "Resident FIDO2 key on YubiKey 5 NFC (USB-A, fixed)";
      publicKey = "sk-ssh-ed25519@openssh.com AAAAGnNrLXNzaC1lZDI1NTE5QG9wZW5zc2guY29tAAAAIEriEHU1OODgpI5oWyeH0zjyUA8N5MsgUiiYIVNXMdQrAAAABHNzaDo=";
      comment = "usb-a yubikey 5 nfc - fixed  - resident ssh fido2";
      trust = {
        login = true;
        userCa = true;
      };
    };

    hsm-yk-usb-a-5-nfc-keychain = {
      description = "Resident FIDO2 key on YubiKey 5 NFC (USB-A, keychain)";
      publicKey = "sk-ssh-ed25519@openssh.com AAAAGnNrLXNzaC1lZDI1NTE5QG9wZW5zc2guY29tAAAAINC200GwDhLpi27iCDBppww5QPuRtXACjXsrtJ5hnW0LAAAABHNzaDo=";
      comment = "usb-a yubikey 5 nfc - keychain  - resident ssh fido2";
      trust = {
        login = true;
        userCa = true;
      };
    };

    pkcs11-raum-root = {
      description = "root PKCS#11 key on raum, for Proxmox VE operations";
      publicKey = "ecdsa-sha2-nistp256 AAAAE2VjZHNhLXNoYTItbmlzdHAyNTYAAAAIbmlzdHAyNTYAAABBBPKyEXbiRblfl23aNjy73yHjFfV/xct885HH4eIzuW3X2VT0BS/jMEFhNJE7eFJE5vO/QIwJ1FKbEonPRGOvNBI=";
    };

    pkcs11-bael-root = {
      description = "root PKCS#11 key on bael, for Proxmox VE operations";
      publicKey = "ecdsa-sha2-nistp256 AAAAE2VjZHNhLXNoYTItbmlzdHAyNTYAAAAIbmlzdHAyNTYAAABBBB2KztRbE1TXYv26vLcezAhNRGw1h2XDzwES0eoARXhtvC2q2JLGNEYyej84wUBi8MkLOmdS1qBxp+24w5dxWDo=";
    };

    pkcs11-valak-root = {
      description = "root PKCS#11 key on valak, for Proxmox VE operations";
      publicKey = "ecdsa-sha2-nistp256 AAAAE2VjZHNhLXNoYTItbmlzdHAyNTYAAAAIbmlzdHAyNTYAAABBBOQGa6V7J3mlk5WFuIUUfjc61ESaCAkmAvzv3vC1qg4A6IJIm083t8MMQFdstFEr6qFrzZZvwyIbWYdN5gUtJ2o=";
    };

    brother-scanner = {
      description = "Brother scanner key, for SFTP uploads";
      publicKey = "ssh-rsa AAAAB3NzaC1yc2EAAAADAQABAAABAQCrBq26K0dIo5wDqyMF8FL4NoqP9p4+IJxuTa5bLfEdp/yS9F07orqIfJDx6j+Pb4dRV3F4o9Qe23250SBEmf6z5SY8PxrzkECpx+EzzegkNJkTCgJN0XAep4treDnnO0YqYNtcVCsz+/Td+6aRr/cJGVlKdnqMAUAN35wmBbsc+npwt3yt3K2N11ZmCAQj9sBbS8FCRxFJhBB3h0JiSGeYDQZZle63j2gavDUbBUR0dXuiMnRoitCc1d9Bv3rBMhy4sRKqubcXK48g2CDDxHno5hXd3pyEfbx1N3HdIevV7OnhTUCp6X4KG7h9Sh18ccsJNVr5Awagb5RKAM/9kYbL";
    };

    claude-nixos-config = {
      description = "Agent experimentation key";
      publicKey = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIJJEu8TMOeD/d6H+1wt6vKBypE3gGJig1JS+zcdmCq4U";
      comment = "claude-nixos-config - 2025-02-26";
    };

    binarin-at-nixos = {
      description = "Software key binarin@nixos";
      publicKey = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIMCVAKqmUdCkJ1gbi2ZA6vLnmf880U/9v5bfxhChapWB";
      comment = "binarin@nixos";
    };

    termux-tab-s9-2025-12-20 = {
      description = "Termux software key on Tab S9";
      publicKey = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIEh4s6QbLJMwYzLpC1r0OZVl6gthLSe2k/fgM66DHGGz";
      comment = "termux-tab-s9-2025-12-20";
    };
  };
}

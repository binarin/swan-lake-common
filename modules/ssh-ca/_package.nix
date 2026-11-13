{
  pkgs,
  registryDocument,
}:
let
  inherit (pkgs) lib;
  src = ../../tools/ssh-ca;
  registry = pkgs.writeText "ssh-trust-registry.json" (builtins.toJSON registryDocument);
in
pkgs.rustPlatform.buildRustPackage {
  pname = "ssh-ca";
  version = "0.1.0";
  src = lib.fileset.toSource {
    root = src;
    fileset = lib.fileset.unions [
      (src + "/Cargo.toml")
      (src + "/Cargo.lock")
      (src + "/src")
      (src + "/tests")
    ];
  };
  cargoLock.lockFile = src + "/Cargo.lock";
  nativeBuildInputs = [ pkgs.makeWrapper ];
  # The integration tests drive real ssh-keygen, ssh-agent and ssh-add.
  nativeCheckInputs = [ pkgs.openssh ];
  postInstall = ''
    wrapProgram "$out/bin/ssh-ca" \
      --set-default SSH_CA_REGISTRY ${registry} \
      --set-default SSH_CA_SSH_KEYGEN ${pkgs.openssh}/bin/ssh-keygen \
      --set-default SSH_CA_YKMAN ${pkgs.yubikey-manager}/bin/ykman
  '';
  passthru = { inherit registry; };
  meta = {
    description = "Issue OpenSSH user certificates from trusted hardware-backed CAs";
    mainProgram = "ssh-ca";
    platforms = [
      "x86_64-linux"
      "aarch64-linux"
    ];
  };
}

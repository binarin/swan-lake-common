# ssh-ca

Issues OpenSSH user certificates from the CAs in common's ssh-trust registry
(`modules/ssh-trust`). OpenSSH's `ssh-keygen -s` does the signing; this tool
plans, checks and validates around it.

## Workflows

```sh
# Everyday: fresh in-memory Ed25519 key, certified and added to $SSH_AUTH_SOCK.
ssh-ca session --ca hsm-yk-usb-c-5-nfc --ca-key ~/.ssh/id_ed25519_sk_rk \
  --policy manifest.json --cartesian server-a,server-b binarin,root

# Certify an existing public key; writes id_ed25519-cert.pub next to it.
ssh-ca sign ~/.ssh/id_ed25519.pub --ca hsm-tz-ca --ca-agent "$TREZOR_AGENT_SOCK" \
  --policy manifest.json --target server-a/binarin

# Exact principals, nothing inferred.
ssh-ca sign key.pub --ca hsm-tz-ca --ca-agent "$TREZOR_AGENT_SOCK" --principal custom
```

The manifest is exported by the consumer flake (nixos-config:
`.#ssh-trust-manifest`). Without one, target mode needs `--default-principals`,
which assumes every target accepts `MACHINE--ACCOUNT` and knows nothing about
custom or legacy server policy.

## Rules

- One subject and one certificate per run. `--target` and each `--cartesian
  MACHINES ACCOUNTS` group are expanded separately and united; groups are never
  multiplied with each other. At most 256 targets.
- With a manifest, unknown targets fail. A target's scoped default principal
  is used; a target without one uses its first accepted principal, which is
  shared. Shared principals, and principals other manifest targets also accept,
  need `--allow-shared-principals`. The certificate authorizes every host that
  accepts its principals; the target list and key id restrict nothing.
- `--plan` prints the plan and exits without touching a device, agent or file.
  Otherwise the plan is shown and must be confirmed (`--yes` for automation).
- The CA is identified by registry id. `--ca-key` must hold that public key;
  `--ca-agent` must list it. `--yubikey-serial` requires that exactly that
  YubiKey is connected (via `ykman list --serials`); OpenSSH then selects the
  FIDO device by credential. An agent cannot prove which device holds a key, so
  serial selection is only available with `--ca-key`. There is no fallback to
  another key, agent or device.
- The returned certificate is checked against the plan (type, CA, subject,
  principals, validity, key id, signature) before it is written or installed.
- `session` requires an existing destination agent (`--agent`, default
  `$SSH_AUTH_SOCK`) and adds the identity with an explicit lifetime, at most the
  certificate validity (default one week). The private key is never written to
  a file. The CA agent and the destination agent are separate options.
- Public inputs and the certificate are staged in a private directory under
  `$XDG_RUNTIME_DIR`, removed (best effort) on normal exit, failure and Ctrl-C. A crash or
  SIGKILL can leave that public material behind until the runtime directory is
  cleaned. Deleting a certificate does not revoke it.
- PINs and touch prompts are OpenSSH's; the tool never takes a PIN.

## Status

Tested with disposable software CAs (Ed25519, ECDSA) and disposable
`ssh-agent`s. Signing with YubiKey resident keys, `--yubikey-serial`, and
Trezor through trezor-agent are not yet verified on hardware. Resident-key
recovery (`ssh-keygen -K`) is not part of this tool.

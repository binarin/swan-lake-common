//! Certificate issuance through `ssh-keygen -s`, and validation of what it
//! returns.

use std::collections::BTreeSet;
use std::env;
use std::ffi::OsString;
use std::fs;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result, bail};
use ssh_agent_lib::blocking::Client;
use ssh_key::certificate::CertType;
use ssh_key::public::KeyData;
use ssh_key::{Certificate, HashAlg, PrivateKey, PublicKey};
use tempfile::TempDir;

use crate::policy::CaIdentity;

/// Set while `ssh-keygen` runs, so an interrupt is left to the child and the
/// staging directory is still cleaned up on the way out.
pub static CHILD_RUNNING: AtomicBool = AtomicBool::new(false);

/// How the CA private key is reached.
#[derive(Debug, Clone)]
pub enum Signer {
    /// An SSH agent holding the CA identity (YubiKey via ssh-agent, trezor-agent, ...).
    Agent { socket: PathBuf },
    /// A private key file; for FIDO keys, the resident-key handle file.
    KeyFile {
        path: PathBuf,
        yubikey_serial: Option<String>,
    },
}

impl Signer {
    pub fn describe(&self) -> String {
        match self {
            Signer::Agent { socket } => format!(
                "SSH agent at {} (agent identities carry no device serial)",
                socket.display()
            ),
            Signer::KeyFile {
                path,
                yubikey_serial: None,
            } => format!("key file {}", path.display()),
            Signer::KeyFile {
                path,
                yubikey_serial: Some(serial),
            } => format!(
                "key file {} on YubiKey serial {serial} (must be the only YubiKey connected)",
                path.display()
            ),
        }
    }

    /// Check, before anything is signed, that the transport really offers the
    /// expected CA. Never falls back to another key, agent or device.
    pub fn preflight(&self, ca: &CaIdentity) -> Result<()> {
        match self {
            Signer::Agent { socket } => {
                let stream = UnixStream::connect(socket)
                    .with_context(|| format!("cannot connect to CA agent {}", socket.display()))?;
                let identities = Client::new(stream)
                    .request_identities()
                    .context("CA agent did not list its identities")?;
                if !identities
                    .iter()
                    .any(|i| i.credential.key_data() == ca.public_key.key_data())
                {
                    bail!(
                        "CA agent {} does not hold {} ({})",
                        socket.display(),
                        ca.id,
                        ca.fingerprint()
                    );
                }
            }
            Signer::KeyFile {
                path,
                yubikey_serial,
            } => {
                // The OpenSSH private key format keeps the public key in the
                // clear, so this needs neither the device nor a passphrase.
                let key = PrivateKey::read_openssh_file(path)
                    .with_context(|| format!("cannot read CA key file {}", path.display()))?;
                if key.public_key().key_data() != ca.public_key.key_data() {
                    bail!(
                        "CA key file {} holds {}, not {} ({})",
                        path.display(),
                        key.public_key().fingerprint(HashAlg::Sha256),
                        ca.id,
                        ca.fingerprint()
                    );
                }
                if let Some(serial) = yubikey_serial {
                    check_single_yubikey(serial)?;
                }
            }
        }
        Ok(())
    }
}

/// `ykman list --serials` must report exactly the requested serial. OpenSSH
/// picks the FIDO device by credential, so with one YubiKey present the
/// requested device is the only candidate.
fn check_single_yubikey(serial: &str) -> Result<()> {
    let ykman = env::var_os("SSH_CA_YKMAN").unwrap_or_else(|| OsString::from("ykman"));
    let out = Command::new(&ykman)
        .args(["list", "--serials"])
        .output()
        .with_context(|| format!("cannot run {}", ykman.to_string_lossy()))?;
    if !out.status.success() {
        bail!(
            "ykman list --serials failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let serials: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();
    match serials.as_slice() {
        [only] if only == serial => Ok(()),
        [] => bail!("YubiKey {serial} is not connected"),
        _ if !serials.iter().any(|s| s == serial) => {
            bail!(
                "YubiKey {serial} is not connected (found: {})",
                serials.join(", ")
            )
        }
        _ => bail!(
            "several YubiKeys connected ({}); leave only {serial} plugged in",
            serials.join(", ")
        ),
    }
}

/// What the certificate must contain.
#[derive(Debug, Clone)]
pub struct Request {
    pub key_id: String,
    pub principals: Vec<String>,
    pub valid_after: u64,
    pub valid_before: u64,
}

/// A private directory on volatile runtime storage for the public subject
/// key and the returned certificate. Private key material is never put here.
pub fn staging_dir() -> Result<TempDir> {
    let Some(runtime) = env::var_os("XDG_RUNTIME_DIR") else {
        bail!(
            "XDG_RUNTIME_DIR is not set; refusing to stage certificate files on persistent storage"
        );
    };
    let runtime = PathBuf::from(runtime);
    if !runtime.is_dir() {
        bail!("XDG_RUNTIME_DIR {} is not a directory", runtime.display());
    }
    // tempfile creates the directory with mode 0700.
    tempfile::Builder::new()
        .prefix("ssh-ca-")
        .tempdir_in(&runtime)
        .with_context(|| format!("cannot create a staging directory in {}", runtime.display()))
}

pub fn ssh_keygen() -> OsString {
    env::var_os("SSH_CA_SSH_KEYGEN").unwrap_or_else(|| OsString::from("ssh-keygen"))
}

/// Run `ssh-keygen -s` for `subject` and return the validated certificate.
pub fn issue(
    staging: &Path,
    signer: &Signer,
    ca: &CaIdentity,
    subject: &PublicKey,
    request: &Request,
) -> Result<Certificate> {
    let subject_path = staging.join("subject.pub");
    let cert_path = staging.join("subject-cert.pub");
    fs::write(&subject_path, subject.to_openssh()? + "\n")?;

    let mut cmd = Command::new(ssh_keygen());
    match signer {
        Signer::Agent { socket } => {
            let ca_path = staging.join("ca.pub");
            fs::write(&ca_path, ca.public_key.to_openssh()? + "\n")?;
            cmd.arg("-U")
                .arg("-s")
                .arg(&ca_path)
                .env("SSH_AUTH_SOCK", socket);
        }
        Signer::KeyFile { path, .. } => {
            cmd.arg("-s").arg(path).env_remove("SSH_AUTH_SOCK");
        }
    }
    cmd.arg("-I")
        .arg(&request.key_id)
        .arg("-n")
        .arg(request.principals.join(","))
        .arg("-V")
        .arg(format!(
            "{:#x}:{:#x}",
            request.valid_after, request.valid_before
        ))
        .arg(&subject_path);

    CHILD_RUNNING.store(true, Ordering::SeqCst);
    let status = cmd.status();
    CHILD_RUNNING.store(false, Ordering::SeqCst);
    let status =
        status.with_context(|| format!("cannot run {}", ssh_keygen().to_string_lossy()))?;
    if !status.success() {
        bail!("ssh-keygen failed to sign the certificate ({status})");
    }

    let text = fs::read_to_string(&cert_path).context("ssh-keygen produced no certificate")?;
    let cert = Certificate::from_openssh(text.trim())
        .context("ssh-keygen returned an unparsable certificate")?;
    validate(&cert, ca, subject.key_data(), request)?;
    Ok(cert)
}

/// The returned certificate must be exactly what was planned.
pub fn validate(
    cert: &Certificate,
    ca: &CaIdentity,
    subject: &KeyData,
    request: &Request,
) -> Result<()> {
    if cert.cert_type() != CertType::User {
        bail!("returned certificate is not a user certificate");
    }
    if cert.signature_key() != ca.public_key.key_data() {
        bail!("returned certificate is signed by an unexpected CA");
    }
    if cert.public_key() != subject {
        bail!("returned certificate certifies an unexpected subject key");
    }
    let got: BTreeSet<&String> = cert.valid_principals().iter().collect();
    let want: BTreeSet<&String> = request.principals.iter().collect();
    if got != want || cert.valid_principals().len() != request.principals.len() {
        bail!(
            "returned certificate principals {:?} differ from the requested {:?}",
            cert.valid_principals(),
            request.principals
        );
    }
    if cert.valid_after() != request.valid_after || cert.valid_before() != request.valid_before {
        bail!("returned certificate validity differs from the requested interval");
    }
    if cert.key_id() != request.key_id {
        bail!("returned certificate key id differs from the requested one");
    }
    let fp = ca.public_key.fingerprint(HashAlg::Sha256);
    cert.validate_at(request.valid_after, [&fp])
        .context("returned certificate signature does not verify against the CA")?;
    Ok(())
}

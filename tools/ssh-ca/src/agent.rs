//! Installing a certified ephemeral identity into an existing agent.

use std::os::unix::net::UnixStream;
use std::path::Path;

use anyhow::{Context, Result, bail};
use ssh_agent_lib::blocking::Client;
use ssh_agent_lib::proto::{
    AddIdentity, AddIdentityConstrained, KeyConstraint, PrivateCredential, PrivateKeyData,
    PublicCredential, RemoveIdentity,
};
use ssh_key::private::Ed25519Keypair;
use ssh_key::public::KeyData;
use ssh_key::{Algorithm, Certificate};

fn connect(socket: &Path) -> Result<Client<UnixStream>> {
    let stream = UnixStream::connect(socket).with_context(|| {
        format!(
            "no SSH agent at {}; start one (for example `eval $(ssh-agent)`) or pass --agent",
            socket.display()
        )
    })?;
    Ok(Client::new(stream))
}

/// The destination agent must exist and answer before anything is signed.
pub fn probe(socket: &Path) -> Result<()> {
    connect(socket)?
        .request_identities()
        .with_context(|| format!("SSH agent at {} does not answer", socket.display()))?;
    Ok(())
}

/// Same signed certificate; the comment is not part of it.
fn same_cert(a: &Certificate, b: &Certificate) -> bool {
    a.signature() == b.signature() && a.public_key() == b.public_key()
}

fn holds(client: &mut Client<UnixStream>, cert: &Certificate) -> Result<bool> {
    Ok(client
        .request_identities()
        .context("agent did not list its identities")?
        .iter()
        .any(|i| matches!(&i.credential, PublicCredential::Cert(c) if same_cert(c, cert))))
}

/// Remove every certificate identity signed by `ca` except `keep`. Plain
/// keys and certificates from other CAs are left alone. Returns the key ids
/// of the removed certificates.
pub fn remove_superseded(socket: &Path, ca: &KeyData, keep: &Certificate) -> Result<Vec<String>> {
    let mut client = connect(socket)?;
    let identities = client
        .request_identities()
        .context("agent did not list its identities")?;
    let mut removed = Vec::new();
    for identity in identities {
        let PublicCredential::Cert(cert) = &identity.credential else {
            continue;
        };
        if cert.signature_key() != ca || same_cert(cert, keep) {
            continue;
        }
        client
            .remove_identity(RemoveIdentity {
                credential: identity.credential.clone(),
            })
            .with_context(|| format!("cannot remove superseded certificate `{}`", cert.key_id()))?;
        removed.push(cert.key_id().to_string());
    }
    Ok(removed)
}

/// Add `cert` with its private key, deleted by the agent after `lifetime`
/// seconds. Consumes the key so no copy outlives the request.
pub fn install(
    socket: &Path,
    cert: &Certificate,
    keypair: Ed25519Keypair,
    comment: &str,
    lifetime: u32,
) -> Result<()> {
    if lifetime == 0 {
        bail!("agent lifetime must be positive");
    }
    let mut client = connect(socket)?;
    if holds(&mut client, cert)? {
        bail!("the agent already holds this certificate");
    }
    let request = AddIdentityConstrained {
        identity: AddIdentity {
            credential: PrivateCredential::Cert {
                algorithm: Algorithm::Ed25519,
                certificate: Box::new(cert.clone()),
                privkey: PrivateKeyData::Ed25519(keypair),
                comment: comment.to_string(),
            },
        },
        constraints: vec![KeyConstraint::Lifetime(lifetime)],
    };
    let added = client.add_identity_constrained(request);
    added.context("the agent refused the certified identity")?;

    if !holds(&mut client, cert)? {
        let _ = client.remove_identity(RemoveIdentity {
            credential: PublicCredential::Cert(Box::new(cert.clone())),
        });
        bail!("the agent accepted the identity but does not list the certificate");
    }
    Ok(())
}

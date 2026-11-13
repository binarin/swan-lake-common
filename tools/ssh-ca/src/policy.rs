//! Key registry and target manifest documents, both exported by Nix.

use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use ssh_key::PublicKey;

pub const REGISTRY_SCHEMA: &str = "swan-lake-ssh-trust-registry";
pub const MANIFEST_SCHEMA: &str = "swan-lake-ssh-trust-manifest";
const SUPPORTED_VERSION: u64 = 1;
const MAX_DOCUMENT_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Trust {
    #[serde(default)]
    pub user_ca: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegistryKey {
    pub public_key: String,
    pub trust: Trust,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RegistryDocument {
    #[allow(dead_code)]
    schema: String,
    #[allow(dead_code)]
    version: u64,
    keys: BTreeMap<String, RegistryKey>,
}

#[derive(Debug)]
pub struct Registry {
    pub keys: BTreeMap<String, RegistryKey>,
}

/// A trusted user CA: registry id plus its public key.
#[derive(Debug, Clone)]
pub struct CaIdentity {
    pub id: String,
    pub public_key: PublicKey,
}

impl CaIdentity {
    pub fn fingerprint(&self) -> String {
        self.public_key
            .fingerprint(ssh_key::HashAlg::Sha256)
            .to_string()
    }
}

impl Registry {
    pub fn load(path: &Path) -> Result<Self> {
        let text = read_bounded(path)?;
        check_header(&text, REGISTRY_SCHEMA, path)?;
        let doc: RegistryDocument = serde_json::from_str(&text)
            .with_context(|| format!("{}: malformed key registry", path.display()))?;
        Ok(Self { keys: doc.keys })
    }

    /// Resolve `--ca ID`; the key must carry the `userCa` trust role.
    pub fn ca(&self, id: &str) -> Result<CaIdentity> {
        let Some(key) = self.keys.get(id) else {
            bail!("unknown CA `{id}`: not in the key registry");
        };
        if !key.trust.user_ca {
            bail!("registry key `{id}` is not a trusted user CA");
        }
        let public_key = PublicKey::from_openssh(&key.public_key)
            .with_context(|| format!("registry key `{id}` has an unparsable public key"))?;
        Ok(CaIdentity {
            id: id.to_string(),
            public_key,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Principal {
    pub name: String,
    pub options: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Account {
    pub default_principal: Option<String>,
    pub principals: Vec<Principal>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestCa {
    pub public_key: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Manifest {
    #[allow(dead_code)]
    schema: String,
    #[allow(dead_code)]
    version: u64,
    pub user_ca_keys: Vec<ManifestCa>,
    pub machines: BTreeMap<String, BTreeMap<String, Account>>,
}

impl Manifest {
    pub fn load(path: &Path) -> Result<Self> {
        let text = read_bounded(path)?;
        check_header(&text, MANIFEST_SCHEMA, path)?;
        let manifest: Manifest = serde_json::from_str(&text)
            .with_context(|| format!("{}: malformed target manifest", path.display()))?;
        for (machine, accounts) in &manifest.machines {
            for (user, account) in accounts {
                if let Some(p) = &account.default_principal
                    && !account.principals.iter().any(|q| &q.name == p)
                {
                    bail!(
                        "{}: {machine}/{user}: default principal `{p}` is not among its accepted principals",
                        path.display()
                    );
                }
                for p in &account.principals {
                    validate_principal(&p.name)
                        .with_context(|| format!("{}: {machine}/{user}", path.display()))?;
                }
            }
        }
        Ok(manifest)
    }

    /// Servers covered by the manifest trust exactly these CAs.
    pub fn trusts_ca(&self, ca: &CaIdentity) -> bool {
        self.user_ca_keys.iter().any(|k| {
            PublicKey::from_openssh(&k.public_key)
                .map(|pk| pk.key_data() == ca.public_key.key_data())
                .unwrap_or(false)
        })
    }
}

/// A certificate principal name as passed to `ssh-keygen -n`.
pub fn validate_principal(name: &str) -> Result<()> {
    if name.is_empty()
        || name
            .chars()
            .any(|c| c.is_whitespace() || c == ',' || c.is_control())
    {
        bail!("invalid principal `{name}`: must be non-empty, without whitespace or commas");
    }
    Ok(())
}

fn read_bounded(path: &Path) -> Result<String> {
    let file = fs::File::open(path).with_context(|| format!("cannot open {}", path.display()))?;
    let mut text = String::new();
    file.take(MAX_DOCUMENT_BYTES + 1)
        .read_to_string(&mut text)
        .with_context(|| format!("cannot read {}", path.display()))?;
    if text.len() as u64 > MAX_DOCUMENT_BYTES {
        bail!("{}: larger than {MAX_DOCUMENT_BYTES} bytes", path.display());
    }
    Ok(text)
}

/// Refuse documents of another kind or version before interpreting them.
fn check_header(text: &str, schema: &str, path: &Path) -> Result<()> {
    #[derive(Deserialize)]
    struct Loose {
        schema: Option<String>,
        version: Option<u64>,
    }
    let loose: Loose = serde_json::from_str(text)
        .with_context(|| format!("{}: not a JSON object", path.display()))?;
    let found = loose.schema.unwrap_or_default();
    if found != schema {
        bail!(
            "{}: expected schema `{schema}`, found `{found}`",
            path.display()
        );
    }
    let version = loose.version.unwrap_or(0);
    if version != SUPPORTED_VERSION {
        bail!(
            "{}: unsupported {schema} version {version} (this tool understands {SUPPORTED_VERSION})",
            path.display()
        );
    }
    Ok(())
}

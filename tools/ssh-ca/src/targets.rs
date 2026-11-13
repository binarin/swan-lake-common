//! Turn `--target`, `--cartesian` and `--principal` arguments into the
//! principal set of one certificate.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Result, bail};

use crate::policy::{Manifest, validate_principal};

/// Upper bound on expanded machine/account pairs and raw principals.
pub const MAX_TARGETS: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Target {
    pub machine: String,
    pub account: String,
}

impl std::fmt::Display for Target {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.machine, self.account)
    }
}

fn validate_name(kind: &str, s: &str) -> Result<()> {
    if s.is_empty()
        || s.chars()
            .any(|c| c.is_whitespace() || c == ',' || c == '/' || c.is_control())
    {
        bail!("invalid {kind} name `{s}`");
    }
    Ok(())
}

pub fn parse_target(s: &str) -> Result<Target> {
    let Some((machine, account)) = s.split_once('/') else {
        bail!("invalid target `{s}`: expected MACHINE/ACCOUNT");
    };
    validate_name("machine", machine)?;
    validate_name("account", account)?;
    Ok(Target {
        machine: machine.to_string(),
        account: account.to_string(),
    })
}

fn split_list(kind: &str, s: &str) -> Result<Vec<String>> {
    let items: Vec<String> = s.split(',').map(str::to_string).collect();
    for item in &items {
        validate_name(kind, item)?;
    }
    Ok(items)
}

/// Union of explicit targets and each `--cartesian MACHINES ACCOUNTS` group,
/// expanded independently: groups never multiply with each other.
pub fn expand(targets: &[String], cartesian: &[Vec<String>]) -> Result<BTreeSet<Target>> {
    let mut out = BTreeSet::new();
    let push = |t: Target, out: &mut BTreeSet<Target>| -> Result<()> {
        out.insert(t);
        if out.len() > MAX_TARGETS {
            bail!("more than {MAX_TARGETS} targets requested");
        }
        Ok(())
    };
    for t in targets {
        push(parse_target(t)?, &mut out)?;
    }
    for group in cartesian {
        let [machines, accounts] = group.as_slice() else {
            bail!("--cartesian takes exactly MACHINES and ACCOUNTS");
        };
        let machines = split_list("machine", machines)?;
        let accounts = split_list("account", accounts)?;
        if machines.len().saturating_mul(accounts.len()) > MAX_TARGETS {
            bail!(
                "--cartesian {} {}: more than {MAX_TARGETS} targets",
                group[0],
                group[1]
            );
        }
        for m in &machines {
            for a in &accounts {
                push(
                    Target {
                        machine: m.clone(),
                        account: a.clone(),
                    },
                    &mut out,
                )?;
            }
        }
    }
    Ok(out)
}

/// How one requested target is satisfied.
#[derive(Debug, Clone)]
pub struct Grant {
    pub target: Target,
    pub principal: String,
    /// Server-side options attached to this principal, such as a forced command.
    pub options: Option<String>,
    /// True when the target has no scoped default principal.
    pub legacy: bool,
}

#[derive(Debug, Clone)]
pub enum Resolution {
    /// Explicit `--principal` values; no target inference.
    Raw { principals: Vec<String> },
    Targets {
        grants: Vec<Grant>,
        /// Whether a manifest backed the resolution.
        from_manifest: bool,
    },
}

impl Resolution {
    /// Deduplicated, sorted certificate principals.
    pub fn principals(&self) -> Vec<String> {
        let set: BTreeSet<String> = match self {
            Resolution::Raw { principals } => principals.iter().cloned().collect(),
            Resolution::Targets { grants, .. } => {
                grants.iter().map(|g| g.principal.clone()).collect()
            }
        };
        set.into_iter().collect()
    }
}

pub fn resolve_raw(principals: &[String]) -> Result<Resolution> {
    let mut set = BTreeSet::new();
    for p in principals {
        validate_principal(p)?;
        set.insert(p.clone());
    }
    if set.len() > MAX_TARGETS {
        bail!("more than {MAX_TARGETS} principals requested");
    }
    Ok(Resolution::Raw {
        principals: set.into_iter().collect(),
    })
}

pub fn resolve_default_principals(targets: &BTreeSet<Target>) -> Result<Resolution> {
    let grants = targets
        .iter()
        .map(|t| {
            let principal = format!("{}--{}", t.machine, t.account);
            validate_principal(&principal)?;
            Ok(Grant {
                target: t.clone(),
                principal,
                options: None,
                legacy: false,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(Resolution::Targets {
        grants,
        from_manifest: false,
    })
}

/// Each target must be in the manifest. Its scoped default principal is
/// preferred; otherwise its first accepted principal is used, which is
/// typically shared with other machines.
pub fn resolve_manifest(manifest: &Manifest, targets: &BTreeSet<Target>) -> Result<Resolution> {
    let mut unknown = Vec::new();
    let mut grants = Vec::new();
    for t in targets {
        let Some(account) = manifest
            .machines
            .get(&t.machine)
            .and_then(|accounts| accounts.get(&t.account))
        else {
            unknown.push(t.to_string());
            continue;
        };
        let (principal, legacy) = match &account.default_principal {
            Some(p) => (p.clone(), false),
            None => match account.principals.first() {
                Some(p) => (p.name.clone(), true),
                None => bail!("target {t} accepts no certificate principals"),
            },
        };
        let options = account
            .principals
            .iter()
            .find(|p| p.name == principal)
            .and_then(|p| p.options.clone());
        grants.push(Grant {
            target: t.clone(),
            principal,
            options,
            legacy,
        });
    }
    if !unknown.is_empty() {
        bail!("targets not in the manifest: {}", unknown.join(", "));
    }
    Ok(Resolution::Targets {
        grants,
        from_manifest: true,
    })
}

/// Every manifest target that accepts each principal, with its options.
pub fn acceptance(
    manifest: &Manifest,
    principals: &[String],
) -> BTreeMap<String, Vec<(Target, Option<String>)>> {
    let mut out: BTreeMap<String, Vec<(Target, Option<String>)>> =
        principals.iter().map(|p| (p.clone(), Vec::new())).collect();
    for (machine, accounts) in &manifest.machines {
        for (account, acc) in accounts {
            for p in &acc.principals {
                if let Some(list) = out.get_mut(&p.name) {
                    list.push((
                        Target {
                            machine: machine.clone(),
                            account: account.clone(),
                        },
                        p.options.clone(),
                    ));
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn cartesian_groups_expand_independently() {
        let got = expand(
            &s(&["server-d/root"]),
            &[
                s(&["server-a,server-b", "binarin,root"]),
                s(&["server-c", "deploy"]),
            ],
        )
        .unwrap();
        let got: Vec<String> = got.iter().map(|t| t.to_string()).collect();
        assert_eq!(
            got,
            s(&[
                "server-a/binarin",
                "server-a/root",
                "server-b/binarin",
                "server-b/root",
                "server-c/deploy",
                "server-d/root",
            ])
        );
    }

    #[test]
    fn duplicates_collapse() {
        let got = expand(&s(&["a/root", "a/root"]), &[s(&["a", "root,root"])]).unwrap();
        assert_eq!(got.len(), 1);
    }

    #[test]
    fn bad_targets_rejected() {
        for bad in ["a", "/root", "a/", "a/b/c", "a b/root", "a,b/root"] {
            assert!(parse_target(bad).is_err(), "{bad}");
        }
        assert!(expand(&[], &[s(&["a,", "root"])]).is_err());
    }

    #[test]
    fn expansion_is_bounded() {
        let machines: Vec<String> = (0..20).map(|i| format!("m{i}")).collect();
        let users: Vec<String> = (0..20).map(|i| format!("u{i}")).collect();
        let err = expand(&[], &[vec![machines.join(","), users.join(",")]]).unwrap_err();
        assert!(err.to_string().contains("more than"));
    }

    #[test]
    fn raw_principals_validated_and_deduplicated() {
        let r = resolve_raw(&s(&["b", "a", "b"])).unwrap();
        assert_eq!(r.principals(), s(&["a", "b"]));
        assert!(resolve_raw(&s(&["a b"])).is_err());
        assert!(resolve_raw(&s(&["a,b"])).is_err());
    }
}

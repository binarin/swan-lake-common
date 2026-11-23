//! `swl check`: the CI gate. On an MR, `flake.lock` is unchanged against
//! the merge base with the target branch and `swl.toml` is valid. On master,
//! there is no `swl.toml` and the common pin policy holds.

use std::{collections::BTreeMap, fs, path::Path, process::ExitCode};

use anyhow::{Context, Result, bail};
use clap::ValueEnum;
use serde_json::Value;

use crate::{
    config::Config,
    forge::{PrReader, PrState},
    git::{git, git_ok},
    lock::{Lock, attrs_url},
    manifest::{Entry, MANIFEST_FILE, Manifest},
    nix::Nix,
    pin_policy,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Event {
    /// A merge request into the base branch.
    Mr,
    /// The master branch after a push.
    Master,
}

/// The event from the Forgejo Actions environment (GitHub-compatible
/// variables): `pull_request` is an MR, a push to master is master.
pub fn event_from_env(var: &dyn Fn(&str) -> Option<String>) -> Result<Event> {
    match var("GITHUB_EVENT_NAME").as_deref() {
        Some("pull_request" | "pull_request_target") => Ok(Event::Mr),
        Some("push") => match var("GITHUB_REF").as_deref() {
            Some("refs/heads/master") => Ok(Event::Master),
            other => bail!(
                "a push to {} is neither an MR nor master; pass --event",
                other.unwrap_or("an unknown ref")
            ),
        },
        Some(other) => bail!("CI event {other:?} is neither an MR nor a push; pass --event"),
        None => bail!("no GITHUB_EVENT_NAME; pass --event mr or --event master"),
    }
}

/// The revision to diff an MR against: `--base`, else `origin/<GITHUB_BASE_REF>`
/// after fetching it (and the history, in a shallow CI checkout).
fn mr_base(
    root: &Path,
    flag: Option<&str>,
    var: &dyn Fn(&str) -> Option<String>,
) -> Result<String> {
    if let Some(b) = flag {
        return Ok(b.to_string());
    }
    let Some(branch) = var("GITHUB_BASE_REF").filter(|b| !b.is_empty()) else {
        bail!("no GITHUB_BASE_REF; pass --base REV");
    };
    let tracking = format!("refs/remotes/origin/{branch}");
    let mut args = vec!["fetch", "--quiet", "--no-tags"];
    if git_ok(root, &["rev-parse", "--is-shallow-repository"]).as_deref() == Some("true") {
        args.push("--unshallow");
    }
    let refspec = format!("+refs/heads/{branch}:{tracking}");
    args.extend(["origin", &refspec]);
    git(root, &args).context("fetching the MR's base branch")?;
    Ok(tracking)
}

/// The failure, if `flake.lock` at HEAD differs from the merge base of HEAD
/// and `base`.
pub fn lock_change(root: &Path, base: &str) -> Result<Option<String>> {
    let mb = git(root, &["merge-base", "HEAD", base]).with_context(|| {
        format!("no merge base of HEAD and {base} (a shallow checkout needs fetch-depth: 0)")
    })?;
    let changed = git(
        root,
        &["diff", "--name-only", &mb, "HEAD", "--", "flake.lock"],
    )?;
    Ok((!changed.is_empty()).then(|| {
        format!(
            "flake.lock differs from the merge base {} with {base}: an MR must not change \
             flake.lock; name pending shared-repo MRs in {MANIFEST_FILE}, and lock changes land \
             through swl finalize",
            &mb[..mb.len().min(12)]
        )
    }))
}

/// The MR states for CI: open and merged MRs are usable (merged with a note
/// that the entry can land); a closed-unmerged or unknown MR, or a forge that
/// cannot be asked, is a failure.
pub fn usable_mr(
    path: &str,
    entry: &Entry,
    lookup: Result<Option<crate::forge::PrInfo>>,
) -> Result<(crate::forge::PrInfo, Option<String>), String> {
    let which = format!("{path}: {}#{}", entry.repo, entry.mr);
    match lookup {
        Err(e) => Err(format!("{which}: cannot read the MR state: {e:#}")),
        Ok(None) => Err(format!("{which}: {}", crate::forge::NOT_FOUND)),
        Ok(Some(pr)) => match &pr.state {
            PrState::Open => Ok((pr, None)),
            PrState::Merged { .. } => {
                let note = format!(
                    "{which} is {}; the entry can land with swl finalize once promoted",
                    pr.state.describe()
                );
                Ok((pr, Some(note)))
            }
            PrState::ClosedUnmerged => Err(format!(
                "{which} is closed without merging; drop the entry or reopen the MR"
            )),
        },
    }
}

/// Lazily evaluated `flake.nix` input declarations.
struct Decls<'a> {
    root: &'a Path,
    nix: &'a dyn Fn() -> Result<Nix>,
    cache: Option<(Nix, BTreeMap<String, Value>)>,
}

impl Decls<'_> {
    fn get(&mut self) -> Result<&(Nix, BTreeMap<String, Value>)> {
        if self.cache.is_none() {
            let nix = (self.nix)()?;
            let decls = nix.declarations(self.root)?;
            self.cache = Some((nix, decls));
        }
        Ok(self.cache.as_ref().expect("filled above"))
    }
}

/// Whether the entry's input is one of `entry.repo`: by the lock node's
/// URLs, or for a root input not yet locked, by its `flake.nix` URL.
fn entry_repo_error(
    path: &str,
    entry: &Entry,
    lock: Option<&Lock>,
    config: &Config,
    decls: &mut Decls,
) -> Result<Option<String>> {
    let segments: Vec<String> = path.split('/').map(str::to_string).collect();
    let node = lock.and_then(|l| l.resolve(&segments).and_then(|k| l.node(&k)));
    let mut urls = match node {
        Some(node) => node.urls(),
        None if segments.len() == 1 => {
            let (nix, d) = decls.get()?;
            let Some(url) = d
                .get(path)
                .and_then(|v| v.get("url"))
                .and_then(Value::as_str)
            else {
                return Ok(Some(format!(
                    "{path}: neither in flake.lock nor declared with a url in flake.nix"
                )));
            };
            attrs_url(&nix.parse_flake_ref(url)?).into_iter().collect()
        }
        None => return Ok(Some(format!("{path}: not in flake.lock"))),
    };
    urls.sort();
    urls.dedup();
    if urls.iter().any(|u| config.url_names_repo(u, &entry.repo)) {
        Ok(None)
    } else {
        Ok(Some(format!(
            "{path}: the input is not the repo {:?} (its URLs: {})",
            entry.repo,
            urls.join(", ")
        )))
    }
}

pub struct Outcome {
    pub errors: Vec<String>,
    pub notes: Vec<String>,
}

pub fn check_mr(
    root: &Path,
    base: &str,
    config: &Config,
    api: &dyn PrReader,
    nix: &dyn Fn() -> Result<Nix>,
) -> Result<Outcome> {
    let mut out = Outcome {
        errors: Vec::new(),
        notes: Vec::new(),
    };
    out.errors.extend(lock_change(root, base)?);
    let manifest = match Manifest::read(root) {
        Ok(m) => m.unwrap_or_default(),
        Err(e) => {
            out.errors.push(format!("{e:#}"));
            return Ok(out);
        }
    };
    let lock = Lock::read(root)?;
    let mut decls = Decls {
        root,
        nix,
        cache: None,
    };
    for (path, entry) in &manifest.inputs {
        if let Some(e) = entry_repo_error(path, entry, lock.as_ref(), config, &mut decls)? {
            out.errors.push(e);
        }
        match usable_mr(path, entry, api.pr(&entry.repo, entry.mr)) {
            Ok((pr, note)) => {
                out.notes.push(format!(
                    "{path}: {}#{} {} at {}",
                    entry.repo,
                    entry.mr,
                    pr.state.describe(),
                    &pr.head_sha[..pr.head_sha.len().min(12)]
                ));
                out.notes.extend(note);
            }
            Err(e) => out.errors.push(e),
        }
    }
    if manifest.is_empty() {
        out.notes
            .push(format!("no pending entries in {MANIFEST_FILE}"));
    }
    Ok(out)
}

pub fn check_master(root: &Path, nix: &Nix) -> Result<Outcome> {
    let mut out = Outcome {
        errors: Vec::new(),
        notes: Vec::new(),
    };
    if root.join(MANIFEST_FILE).exists() {
        out.errors.push(format!(
            "master has a {MANIFEST_FILE}: land its entries with swl finalize before pushing to master"
        ));
    }
    let path = root.join("flake.lock");
    let lock: Value = serde_json::from_str(
        &fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?,
    )
    .with_context(|| format!("parsing {}", path.display()))?;
    let decls = nix.declarations(root)?;
    match pin_policy::check(&decls, &lock, &|url| nix.parse_flake_ref(url)) {
        Ok(None) => out
            .notes
            .push("no common input: the pin policy does not apply".into()),
        Ok(Some(pins)) => {
            out.notes
                .push(format!("common is canonical at {}", &pins.common_rev[..12]));
            for (name, locked) in &pins.next {
                out.notes
                    .push(pin_policy::next_report(name, locked, &pins.common_rev));
            }
        }
        Err(e) => out.errors.push(format!("{e:#}")),
    }
    Ok(out)
}

pub fn run(
    dir: &Path,
    event: Option<Event>,
    base: Option<&str>,
    config: &Config,
    api: &dyn PrReader,
) -> Result<ExitCode> {
    let var = |k: &str| std::env::var(k).ok();
    let root = std::path::PathBuf::from(git(dir, &["rev-parse", "--show-toplevel"])?);
    let event = match event {
        Some(e) => e,
        None => event_from_env(&var)?,
    };
    let out = match event {
        Event::Mr => {
            let base = mr_base(&root, base, &var)?;
            check_mr(&root, &base, config, api, &Nix::find)?
        }
        Event::Master => check_master(&root, &Nix::find()?)?,
    };
    for n in &out.notes {
        println!("swl check: {n}");
    }
    for e in &out.errors {
        eprintln!("swl check: error: {e}");
    }
    let what = match event {
        Event::Mr => "MR",
        Event::Master => "master",
    };
    if out.errors.is_empty() {
        println!("swl check: {what} checks passed");
        Ok(ExitCode::SUCCESS)
    } else {
        eprintln!("swl check: {what} checks failed");
        Ok(ExitCode::from(1))
    }
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use super::*;
    use crate::{forge::PrInfo, tempdir::TempDir};

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: BTreeMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |k| map.get(k).cloned()
    }

    #[test]
    fn events_from_forgejo_actions() {
        let e = |pairs: &[(&str, &str)]| event_from_env(&env(pairs));
        assert_eq!(
            e(&[("GITHUB_EVENT_NAME", "pull_request")]).unwrap(),
            Event::Mr
        );
        assert_eq!(
            e(&[
                ("GITHUB_EVENT_NAME", "push"),
                ("GITHUB_REF", "refs/heads/master")
            ])
            .unwrap(),
            Event::Master
        );
        assert!(
            e(&[
                ("GITHUB_EVENT_NAME", "push"),
                ("GITHUB_REF", "refs/heads/x")
            ])
            .is_err()
        );
        assert!(e(&[("GITHUB_EVENT_NAME", "schedule")]).is_err());
        assert!(e(&[]).is_err());
    }

    fn entry() -> Entry {
        Entry {
            repo: "swan-lake-common".into(),
            mr: 24,
        }
    }

    fn pr(state: PrState) -> Result<Option<PrInfo>> {
        Ok(Some(PrInfo {
            number: 24,
            state,
            head_sha: "abc".into(),
            title: String::new(),
        }))
    }

    #[test]
    fn mr_states() {
        assert_eq!(
            usable_mr("common", &entry(), pr(PrState::Open)).unwrap().1,
            None
        );
        let (_, note) = usable_mr(
            "common",
            &entry(),
            pr(PrState::Merged {
                merge_commit: "def".into(),
            }),
        )
        .unwrap();
        assert!(note.unwrap().contains("can land"));
        let closed = usable_mr("common", &entry(), pr(PrState::ClosedUnmerged)).unwrap_err();
        assert!(closed.contains("closed without merging"), "{closed}");
        let missing = usable_mr("common", &entry(), Ok(None)).unwrap_err();
        assert!(missing.contains("no such MR"), "{missing}");
        let offline =
            usable_mr("common", &entry(), Err(anyhow::anyhow!("unreachable"))).unwrap_err();
        assert!(offline.contains("cannot read the MR state"), "{offline}");
    }

    fn g(dir: &Path, args: &[&str]) {
        let out = Command::new(crate::git::GIT)
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@example.org"])
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
    }

    #[test]
    fn lock_diff_against_the_merge_base() {
        let tmp = TempDir::new("swl-check-test").unwrap();
        let r = tmp.path();
        g(r, &["init", "-q", "-b", "master"]);
        fs::write(r.join("flake.lock"), "{}\n").unwrap();
        fs::write(r.join("flake.nix"), "{}\n").unwrap();
        g(r, &["add", "."]);
        g(r, &["commit", "-qm", "base"]);
        g(r, &["checkout", "-qb", "mr"]);
        fs::write(r.join("flake.nix"), "{ }\n").unwrap();
        g(r, &["commit", "-qam", "flake.nix only"]);
        // Master moves on, lock included: not the MR's change.
        g(r, &["checkout", "-q", "master"]);
        fs::write(r.join("flake.lock"), "{ \"master\": 1 }\n").unwrap();
        g(r, &["commit", "-qam", "bump on master"]);
        g(r, &["checkout", "-q", "mr"]);
        assert_eq!(lock_change(r, "master").unwrap(), None);
        fs::write(r.join("flake.lock"), "{ \"mr\": 1 }\n").unwrap();
        g(r, &["commit", "-qam", "lock change"]);
        let e = lock_change(r, "master").unwrap().unwrap();
        assert!(e.contains("must not change flake.lock"), "{e}");
    }
}

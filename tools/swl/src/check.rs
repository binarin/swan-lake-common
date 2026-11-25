//! `swl check`: the CI gate. On an MR, `flake.lock` is unchanged against
//! the merge base with the target branch, or changed only by a landing or
//! forward moves (`lock_rules`), and `swl.toml` is valid. On master, there
//! is no `swl.toml` and the common pin policy holds.

use std::{collections::BTreeMap, fs, path::Path, process::ExitCode};

use anyhow::{Context, Result, bail};
use clap::ValueEnum;
use serde_json::Value;

use crate::{
    ancestry::History,
    config::Config,
    forge::{PrReader, PrState},
    git::{git, git_ok, short},
    lock::{Lock, attrs_url},
    lock_rules,
    manifest::{Entry, MANIFEST_FILE, Manifest},
    nix::Nix,
    pin_policy,
    tempdir::TempDir,
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

/// The one merge base of HEAD and `base`; several (criss-cross merges) are
/// refused.
fn merge_base(root: &Path, base: &str) -> Result<String> {
    let all = git(root, &["merge-base", "--all", "HEAD", base]).with_context(|| {
        format!("no merge base of HEAD and {base} (a shallow checkout needs fetch-depth: 0)")
    })?;
    match all.lines().collect::<Vec<_>>().as_slice() {
        [one] => Ok(one.to_string()),
        many => bail!(
            "HEAD and {base} have {} merge bases (criss-cross merges); rebase onto {base}",
            many.len()
        ),
    }
}

fn lock_differs(root: &Path, mb: &str) -> Result<bool> {
    Ok(!git(
        root,
        &["diff", "--name-only", mb, "HEAD", "--", "flake.lock"],
    )?
    .is_empty())
}

/// The failure, if `flake.lock` at HEAD differs from the merge base of HEAD
/// and `base`: `publish` pushes no lock change.
pub fn lock_change(root: &Path, base: &str) -> Result<Option<String>> {
    let mb = merge_base(root, base)?;
    let changed = git(
        root,
        &["diff", "--name-only", &mb, "HEAD", "--", "flake.lock"],
    )?;
    Ok((!changed.is_empty()).then(|| {
        format!(
            "flake.lock differs from the merge base {} with {base}: an MR must not change \
             flake.lock; name pending shared-repo MRs in {MANIFEST_FILE}, and lock changes land \
             through swl finalize",
            short(&mb)
        )
    }))
}

/// The lock at `rev`, parsed.
fn lock_at(root: &Path, rev: &str) -> Result<Option<Value>> {
    let file = format!("{rev}:flake.lock");
    if git_ok(root, &["cat-file", "-e", &file]).is_none() {
        return Ok(None);
    }
    let text = git(root, &["show", &file])?;
    serde_json::from_str(&text)
        .map(Some)
        .with_context(|| format!("parsing flake.lock at {}", short(rev)))
}

/// The declarations of `flake.nix` at the merge base `mb`.
fn base_declarations(root: &Path, mb: &str, nix: &Nix) -> Result<BTreeMap<String, Value>> {
    let tmp = TempDir::new("swl-check-base")?;
    fs::write(
        tmp.path().join("flake.nix"),
        git(root, &["show", &format!("{mb}:flake.nix")])?,
    )?;
    nix.declarations(tmp.path())
}

/// Whether HEAD's evaluated declaration of an input (URL, ref, nested input
/// overrides and follows) is not the merge base's. Absent at the merge base
/// is a change, but for an input `new` in the lock.
fn declaration_changed(base: Option<&Value>, head: &Value, new: bool) -> bool {
    match base {
        Some(old) => old != head,
        None => !new,
    }
}

/// A root input's declaration in HEAD's `flake.nix`: its ref, and whether
/// it changed from the merge base's (`declaration_changed`). A merge base
/// whose `flake.nix` cannot be read is an error with its reason.
fn declaration(
    decls: &mut Decls,
    base: &Result<BTreeMap<String, Value>, String>,
    input: &str,
    new: bool,
) -> Result<lock_rules::Declaration, String> {
    let base = base
        .as_ref()
        .map_err(|e| format!("cannot read the merge base's flake.nix: {e}"))?;
    let (nix, d) = decls.get().map_err(|e| format!("{e:#}"))?;
    let decl = d
        .get(input)
        .ok_or_else(|| format!("flake.nix does not declare {input}"))?;
    let attrs = match decl.get("url").and_then(Value::as_str) {
        Some(url) => nix.parse_flake_ref(url).map_err(|e| format!("{e:#}"))?,
        None if decl.get("type").is_some() => decl.clone(),
        None => return Err(format!("{input} has no url in flake.nix")),
    };
    Ok(lock_rules::Declaration {
        branch: attrs.get("ref").and_then(Value::as_str).map(str::to_string),
        changed: declaration_changed(base.get(input), decl, new),
    })
}

/// The lock nix makes for the flake at `root` from `reference`, with the
/// overrides (`Nix::lock_from`). It fetches the overridden sources.
fn relock(
    nix: &Nix,
    root: &Path,
    reference: &Value,
    overrides: &[(String, String)],
) -> Result<Value, String> {
    let run = || -> Result<Value> {
        let tmp = TempDir::new("swl-check-relock")?;
        let (reference_path, output) = (
            tmp.path().join("reference.lock"),
            tmp.path().join("flake.lock"),
        );
        fs::write(&reference_path, serde_json::to_vec_pretty(reference)?)?;
        nix.lock_from(root, &reference_path, overrides, &output)?;
        let text = fs::read_to_string(&output).context("nix wrote no lock")?;
        serde_json::from_str(&text).context("parsing the lock nix wrote")
    };
    run().map_err(|e| format!("{e:#}"))
}

/// Whether every one of an input's source URLs (`None`: a source without
/// one) names `repo`; there must be some.
fn sources_name_repo(config: &Config, urls: &[Option<String>], repo: &str) -> bool {
    !urls.is_empty()
        && urls
            .iter()
            .all(|u| u.as_deref().is_some_and(|u| config.url_names_repo(u, repo)))
}

/// The URLs of a lock node's `original` and `locked` sources.
fn node_urls(original: Option<&Value>, locked: Option<&Value>) -> Vec<Option<String>> {
    [original, locked]
        .into_iter()
        .map(|s| s.and_then(attrs_url))
        .collect()
}

/// Whether a lock node is `repo`: both its `original` and its `locked`
/// source name it, so that neither can point elsewhere.
fn node_names_repo(config: &Config, node: &Value, repo: &str) -> bool {
    let urls = node_urls(node.get("original"), node.get("locked"));
    sources_name_repo(config, &urls, repo)
}

/// The lock rules for an MR whose `flake.lock` differs from the merge base
/// `mb`: a landing or forward moves (`lock_rules`), and the pin policy.
#[allow(clippy::too_many_arguments)]
fn check_lock_change(
    root: &Path,
    mb: &str,
    head: &Manifest,
    config: &Config,
    api: &dyn PrReader,
    history: &dyn History,
    decls: &mut Decls,
    out: &mut Outcome,
) -> Result<()> {
    let (Some(old), Some(new)) = (lock_at(root, mb)?, lock_at(root, "HEAD")?) else {
        out.errors.push(format!(
            "flake.lock is added or removed against the merge base {}: an MR must not do that",
            short(mb)
        ));
        return Ok(());
    };
    let (removed, notes) = lock_rules::removed_entries(root, mb, head)?;
    out.notes.extend(notes);
    let decls = std::cell::RefCell::new(decls);
    let base = std::cell::OnceCell::new();
    let declared = |input: &str, new: bool| {
        let mut d = decls.borrow_mut();
        let base = base.get_or_init(|| {
            let (nix, _) = d.get().map_err(|e| format!("{e:#}"))?;
            base_declarations(root, mb, nix).map_err(|e| format!("{e:#}"))
        });
        declaration(&mut d, base, input, new)
    };
    let relock = |reference: &Value, overrides: &[(String, String)]| {
        let mut d = decls.borrow_mut();
        let (nix, _) = d.get().map_err(|e| format!("{e:#}"))?;
        relock(nix, root, reference, overrides)
    };
    let names_repo = |node: &Value, repo: &str| node_names_repo(config, node, repo);
    let j = lock_rules::Rules {
        old: &old,
        new: &new,
        removed: &removed,
        declared: &declared,
        api,
        history,
        names_repo: &names_repo,
        relock: &relock,
    }
    .judge()?;
    out.notes.extend(j.notes);
    out.errors.extend(j.errors);
    let mut decls = decls.borrow_mut();
    match decls.get() {
        Ok((nix, d)) => match pin_policy::check(d, &new, &|url| nix.parse_flake_ref(url)) {
            Ok(Some(pins)) => out.notes.push(format!(
                "HEAD's flake.lock passes the pin policy: common is canonical at {}",
                short(&pins.common_rev)
            )),
            Ok(None) => {}
            Err(e) => out
                .errors
                .push(format!("HEAD's flake.lock breaks the pin policy: {e:#}")),
        },
        Err(e) => out.errors.push(format!("{e:#}")),
    }
    Ok(())
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

/// Whether the entry's input is one of `entry.repo`: by both of the lock
/// node's sources, or for a root input not yet locked, by its `flake.nix`
/// URL.
fn entry_repo_error(
    path: &str,
    entry: &Entry,
    lock: Option<&Lock>,
    config: &Config,
    decls: &mut Decls,
) -> Result<Option<String>> {
    let segments: Vec<String> = path.split('/').map(str::to_string).collect();
    let node = lock.and_then(|l| l.resolve(&segments).and_then(|k| l.node(&k)));
    let urls = match node {
        Some(node) => node_urls(node.original.as_ref(), node.locked.as_ref()),
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
            vec![attrs_url(&nix.parse_flake_ref(url)?)]
        }
        None => return Ok(Some(format!("{path}: not in flake.lock"))),
    };
    if sources_name_repo(config, &urls, &entry.repo) {
        return Ok(None);
    }
    let mut shown: Vec<&str> = urls
        .iter()
        .map(|u| u.as_deref().unwrap_or("(no URL)"))
        .collect();
    shown.sort();
    shown.dedup();
    Ok(Some(format!(
        "{path}: the input is not the repo {:?} (its URLs: {})",
        entry.repo,
        shown.join(", ")
    )))
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
    history: &dyn History,
    nix: &dyn Fn() -> Result<Nix>,
) -> Result<Outcome> {
    let mut out = Outcome {
        errors: Vec::new(),
        notes: Vec::new(),
    };
    let mb = merge_base(root, base)?;
    let manifest = match Manifest::read(root) {
        Ok(m) => m.unwrap_or_default(),
        Err(e) => {
            out.errors.push(format!("{e:#}"));
            return Ok(out);
        }
    };
    let mut decls = Decls {
        root,
        nix,
        cache: None,
    };
    if lock_differs(root, &mb)? {
        check_lock_change(
            root, &mb, &manifest, config, api, history, &mut decls, &mut out,
        )?;
    }
    let lock = Lock::read(root)?;
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
                    short(&pr.head_sha)
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
            out.notes.push(format!(
                "common is canonical at {}",
                short(&pins.common_rev)
            ));
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
    history: &dyn History,
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
            check_mr(&root, &base, config, api, history, &Nix::find)?
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

    #[test]
    fn an_entry_is_its_repo_by_both_sources_of_its_lock_node() {
        let config = Config::default();
        let common = "https://forgejo.lynx-lizard.ts.net/binarin/swan-lake-common";
        let lock = |locked: &str| {
            Lock::parse(
                &serde_json::json!({"version": 7, "root": "root", "nodes": {
                    "root": {"inputs": {"lib": "lib"}},
                    "lib": {"original": {"type": "git", "url": common},
                            "locked": {"type": "git", "url": locked, "rev": "a".repeat(40)}},
                }})
                .to_string(),
            )
            .unwrap()
        };
        let no_nix = || -> Result<Nix> { bail!("not needed") };
        let mut decls = Decls {
            root: Path::new("/nonexistent"),
            nix: &no_nix,
            cache: None,
        };
        let e = Entry {
            repo: "swan-lake-common".into(),
            mr: 1,
        };
        let ok = lock(common);
        assert_eq!(
            entry_repo_error("lib", &e, Some(&ok), &config, &mut decls).unwrap(),
            None
        );
        let elsewhere = lock("https://evil.example/c");
        let err = entry_repo_error("lib", &e, Some(&elsewhere), &config, &mut decls)
            .unwrap()
            .unwrap();
        assert!(err.contains("git+https://evil.example/c"), "{err}");
    }

    #[test]
    fn a_declaration_absent_at_the_merge_base_is_new_only_for_a_new_input() {
        let decl = serde_json::json!({"url": "github:binarin/swan-lake-common"});
        let other = serde_json::json!({"url": "github:binarin/swan-lake-common", "flake": true});
        assert!(!declaration_changed(Some(&decl), &decl, false));
        assert!(declaration_changed(Some(&decl), &other, false));
        assert!(declaration_changed(Some(&decl), &other, true));
        // A moved input the merge base did not declare (an implicit
        // registry input then): declaring it now is a change.
        assert!(declaration_changed(None, &decl, false));
        assert!(!declaration_changed(None, &decl, true));
    }

    #[test]
    fn an_added_input_is_its_repo_by_every_url() {
        let config = Config::default();
        let zw = "https://forgejo.lynx-lizard.ts.net/binarin/zwanenzang";
        let node = |original: &str, locked: &str| {
            serde_json::json!({
                "original": {"type": "git", "url": original},
                "locked": {"type": "git", "url": locked, "rev": "a".repeat(40)},
            })
        };
        assert!(node_names_repo(&config, &node(zw, zw), "zwanenzang"));
        // The review's attack: the forge in original, another repo locked.
        assert!(!node_names_repo(
            &config,
            &node(zw, "https://evil.example/zw"),
            "zwanenzang"
        ));
        assert!(!node_names_repo(
            &config,
            &node("https://evil.example/zw", zw),
            "zwanenzang"
        ));
        let mut only_original = node(zw, zw);
        only_original.as_object_mut().unwrap().remove("locked");
        assert!(!node_names_repo(&config, &only_original, "zwanenzang"));
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
            head_ref: "b".into(),
            title: String::new(),
            draft: false,
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

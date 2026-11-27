//! `swl ws add`: a git worktree in the workspace, from a freshly fetched
//! source, then `swl env`.

use std::{
    cell::RefCell,
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};

use crate::{
    config::Config,
    env,
    git::{git, git_ok, short, unpushed, valid_branch},
    report::Report,
    style::Tone,
    workspace::{Workspace, parse_shared_name},
};

/// Fetches each repository at most once per command; disabled by
/// `--no-fetch`.
pub struct Fetcher {
    enabled: bool,
    /// `(git common dir, remote)` already fetched; worktrees of one source
    /// share their common dir.
    done: RefCell<BTreeSet<(PathBuf, String)>>,
}

impl Fetcher {
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            done: RefCell::new(BTreeSet::new()),
        }
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// `git fetch --prune <remote>` in the repository at `dir`, unless
    /// fetching is off or that repository and remote were fetched already.
    /// Pruned, so that a branch gone on the remote is gone here too, as
    /// status reads it, whatever the git config's `fetch.prune`.
    pub fn fetch(&self, dir: &Path, remote: &str) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }
        let common = git(dir, &["rev-parse", "--git-common-dir"])?;
        let common = dir.join(common);
        let common = common.canonicalize().unwrap_or(common);
        if !self.done.borrow_mut().insert((common, remote.to_string())) {
            return Ok(());
        }
        git(dir, &["fetch", "--quiet", "--prune", remote]).with_context(|| {
            format!(
                "fetching {remote} in {} (pass --no-fetch to work on the refs already known)",
                dir.display()
            )
        })?;
        Ok(())
    }
}

/// The remote a base such as `origin/master` names: the part before the first
/// `/`, when the repository at `dir` has a remote of that name.
pub fn base_remote(dir: &Path, base: &str) -> Result<Option<String>> {
    let Some((remote, _)) = base.split_once('/') else {
        return Ok(None);
    };
    let remotes = git(dir, &["remote"])?;
    Ok(remotes
        .lines()
        .any(|r| r == remote)
        .then(|| remote.to_string()))
}

/// Where a new worktree starts.
#[derive(Debug, Clone, Copy)]
pub enum Start<'a> {
    /// Detached at the repo's base.
    Base,
    /// On this branch: the local one, else one created from the remote's,
    /// else one created at the base.
    Branch(&'a str),
    /// Detached at this revision.
    Detached(&'a str),
}

fn rev(dir: &Path, refname: &str) -> Option<String> {
    git_ok(
        dir,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{refname}^{{commit}}"),
        ],
    )
}

fn is_ancestor(dir: &Path, a: &str, b: &str) -> bool {
    git_ok(dir, &["merge-base", "--is-ancestor", a, b]).is_some()
}

/// A worktree of a source, as `git worktree list --porcelain` shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Worktree {
    pub path: PathBuf,
    /// The full sha of its HEAD (none for a bare main one).
    pub head: Option<String>,
    /// The branch checked out, without `refs/heads/`.
    pub branch: Option<String>,
    pub locked: bool,
    /// Its directory is gone (and it is not locked): `git worktree prune`
    /// drops it.
    pub prunable: bool,
}

/// The worktrees of the repository at `source`, its main one included.
pub fn worktrees(source: &Path) -> Result<Vec<Worktree>> {
    Ok(parse_worktrees(&git(
        source,
        &["worktree", "list", "--porcelain"],
    )?))
}

fn parse_worktrees(text: &str) -> Vec<Worktree> {
    let mut out: Vec<Worktree> = Vec::new();
    for line in text.lines() {
        if let Some(p) = line.strip_prefix("worktree ") {
            out.push(Worktree {
                path: PathBuf::from(p),
                head: None,
                branch: None,
                locked: false,
                prunable: false,
            });
        } else if let Some(w) = out.last_mut() {
            if let Some(h) = line.strip_prefix("HEAD ") {
                w.head = Some(h.to_string());
            } else if let Some(b) = line.strip_prefix("branch refs/heads/") {
                w.branch = Some(b.to_string());
            } else if line == "locked" || line.starts_with("locked ") {
                w.locked = true;
            } else if line == "prunable" || line.starts_with("prunable ") {
                w.prunable = true;
            }
        }
    }
    out
}

/// Clears what stands in the way of `git worktree add` at `dest` (of
/// `branch`, when an existing branch is checked out): a worktree registered
/// there, or one that has the branch. One whose directory is gone (left by
/// a workspace removed by hand) is dropped, `git worktree remove` of just
/// that registration in the source, and said so with its full HEAD; one
/// that exists, or a locked one, refuses. A detached one's commits that are
/// pushed nowhere ([`unpushed`], against the base `remote`) are kept first
/// on the new branch `swl-kept/<workspace>-<checkout>` of the source; when
/// that branch name is invalid or taken, it refuses before dropping any.
/// Other registrations whose directories are gone (a worktree moved, on
/// media not mounted) are not touched.
fn clear_stale_worktrees(
    source: &Path,
    dest: &Path,
    branch: Option<&str>,
    remote: Option<&str>,
    report: &Report,
) -> Result<()> {
    let all = worktrees(source)?;
    let has_branch = |w: &Worktree| branch.is_some() && w.branch.as_deref() == branch;
    let blocking: Vec<&Worktree> = all
        .iter()
        .filter(|w| w.path == dest || has_branch(w))
        .collect();
    if let Some(w) = blocking.iter().find(|w| !w.prunable && has_branch(w)) {
        bail!(
            "branch {} is already checked out at {}{}",
            branch.expect("matched"),
            w.path.display(),
            if w.locked {
                " (locked: git worktree unlock it, or remove it)"
            } else {
                ""
            }
        );
    }
    let mut prunes = Vec::new();
    for w in blocking.iter().filter(|w| w.prunable) {
        let kept = match (&w.branch, &w.head) {
            (None, Some(head)) => {
                let commits = unpushed(source, head, remote, None)?;
                if commits.is_empty() {
                    None
                } else {
                    let kept_branch = kept_branch_name(&w.path);
                    check_kept_branch(source, &kept_branch, &w.path)?;
                    Some((kept_branch, commits))
                }
            }
            _ => None,
        };
        prunes.push((w, kept));
    }
    for (w, kept) in prunes {
        let path = w.path.to_string_lossy();
        let head = w
            .head
            .as_deref()
            .map_or(String::new(), |h| format!(" at {h}"));
        let what = match &w.branch {
            Some(b) => format!(" (branch {b}{head})"),
            None if w.head.is_some() => format!(" (detached{head})"),
            None => String::new(),
        };
        if let (Some((kept_branch, _)), Some(head)) = (&kept, &w.head) {
            let kept_ref = format!("refs/heads/{kept_branch}");
            git(
                source,
                &[
                    "update-ref",
                    "-m",
                    "swl ws add: keep the commits of a detached worktree whose directory is gone",
                    &kept_ref,
                    head,
                    "",
                ],
            )
            .with_context(|| format!("keeping the commits of {path} on {kept_ref}"))?;
        }
        git(source, &["worktree", "remove", &path])?;
        let kept_note = kept.map_or(String::new(), |(kept_branch, commits)| {
            format!(
                "; its commits on no remote branch are kept on the branch {kept_branch}:\n  {}\n\
                 git -C {} branch -D {kept_branch} drops them",
                commits.join("\n  "),
                source.display()
            )
        });
        report.say(
            Tone::Warn,
            format!(
                "pruned the worktree {path}{what} of {}, whose directory is gone (git worktree \
                 remove){kept_note}",
                source.display()
            ),
        );
    }
    Ok(())
}

/// `swl-kept/<workspace>-<checkout>` for the worktree at `path`
/// (`<workspace>/<checkout>`).
fn kept_branch_name(path: &Path) -> String {
    let name = |p: Option<&Path>| {
        p.and_then(Path::file_name)
            .map_or(String::new(), |n| n.to_string_lossy().into_owned())
    };
    format!("swl-kept/{}-{}", name(path.parent()), name(Some(path)))
}

/// Refuses when `kept_branch`, to keep the commits of the detached worktree
/// at `path`, is not a valid branch name or exists already in `source`.
fn check_kept_branch(source: &Path, kept_branch: &str, path: &Path) -> Result<()> {
    if !valid_branch(kept_branch) {
        bail!(
            "{kept_branch:?}, the branch to keep the commits of the detached worktree {}, whose \
             directory is gone, is not a valid branch name; nothing was pruned",
            path.display()
        );
    }
    if rev(source, &format!("refs/heads/{kept_branch}")).is_some() {
        bail!(
            "the branch {kept_branch} of {}, to keep the commits of the detached worktree {}, \
             whose directory is gone, exists already: rename or delete it (git -C {0} branch), \
             then rerun; nothing was pruned",
            source.display(),
            path.display()
        );
    }
    Ok(())
}

/// Readies the existing local `branch`, which no worktree has checked out,
/// to be checked out: fast-forwarded when it is strictly behind the
/// remote's, reset to it when it diverged from it (a stale branch, left
/// at its old head when the remote's was rebased and force-pushed) and
/// said so. A diverged branch with commits that are pushed nowhere
/// ([`unpushed`]) is refused, naming them; nothing is lost.
fn existing_branch(
    source: &Path,
    remote: Option<&str>,
    branch: &str,
    report: &Report,
) -> Result<String> {
    let local = rev(source, &format!("refs/heads/{branch}")).expect("checked by the caller");
    let Some(remote) = remote else {
        return Ok(format!("existing branch {branch}"));
    };
    let tracking = format!("{remote}/{branch}");
    let Some(theirs) = rev(source, &format!("refs/remotes/{tracking}")) else {
        return Ok(format!("existing branch {branch}"));
    };
    if theirs == local || is_ancestor(source, &theirs, &local) {
        return Ok(format!("existing branch {branch}"));
    }
    let behind = is_ancestor(source, &local, &theirs);
    if !behind {
        let lost = unpushed(
            source,
            &local,
            Some(remote),
            Some(&format!("refs/remotes/{tracking}")),
        )?;
        if !lost.is_empty() {
            bail!(
                "branch {branch} has diverged from {tracking}, and it has commits that are on no \
                 remote branch, which resetting it would lose:\n  {}\nto keep them, push them \
                 (or rebase them onto {tracking}) from a checkout of {branch}; to drop them, `git \
                 -C {} branch -D {branch}`; then rerun to join afresh (`swl ws remove` drops a \
                 workspace with its pushed branches, so that none is left behind)",
                lost.join("\n  "),
                source.display()
            );
        }
    }
    let what = if behind { "fast-forward" } else { "reset" };
    git(
        source,
        &[
            "update-ref",
            "-m",
            &format!("swl ws add: {what} to {tracking}"),
            &format!("refs/heads/{branch}"),
            &theirs,
            &local,
        ],
    )?;
    if behind {
        return Ok(format!(
            "existing branch {branch}, fast-forwarded to {tracking}"
        ));
    }
    report.say(
        Tone::Warn,
        format!(
            "reset the branch {branch} of {} from {} to {tracking} at {}: it had diverged, \
             and its commits are on remote branches",
            source.display(),
            short(&local),
            short(&theirs)
        ),
    );
    Ok(format!("existing branch {branch}, reset to {tracking}"))
}

/// Adds the checkout `name` (`<repo>` or `<repo>@<input>`) to the workspace
/// `root` from the repo's configured source, after fetching the remote of its
/// base. Returns the checkout's directory.
pub fn add(
    config: &Config,
    root: &Path,
    name: &str,
    start: Start,
    fetcher: &Fetcher,
    report: &Report,
) -> Result<PathBuf> {
    let repo = checkout_repo(name)?;
    let repo_config = config.repo(&repo)?;
    let source = &repo_config.source;
    let base = repo_config.base();
    fs::create_dir_all(root).with_context(|| format!("creating {}", root.display()))?;
    let dest = root.canonicalize()?.join(name);
    if dest.exists() {
        bail!("{} already exists", dest.display());
    }
    let remote = base_remote(source, base)?;
    if let Some(remote) = &remote {
        fetcher.fetch(source, remote)?;
    }
    let existing = match start {
        Start::Branch(b) if rev(source, &format!("refs/heads/{b}")).is_some() => Some(b),
        _ => None,
    };
    clear_stale_worktrees(source, &dest, existing, remote.as_deref(), report)?;
    let dest_str = dest.to_string_lossy().into_owned();
    let how = match start {
        Start::Base => {
            git(
                source,
                &["worktree", "add", "--quiet", "--detach", &dest_str, base],
            )?;
            format!("detached at {base}")
        }
        Start::Detached(at) => {
            git(
                source,
                &["worktree", "add", "--quiet", "--detach", &dest_str, at],
            )?;
            format!("detached at {}", crate::git::short(at))
        }
        Start::Branch(b) if rev(source, &format!("refs/heads/{b}")).is_some() => {
            let how = existing_branch(source, remote.as_deref(), b, report)?;
            git(source, &["worktree", "add", "--quiet", &dest_str, b])?;
            how
        }
        Start::Branch(b)
            if remote
                .as_ref()
                .is_some_and(|r| rev(source, &format!("refs/remotes/{r}/{b}")).is_some()) =>
        {
            let tracking = format!("{}/{b}", remote.as_deref().expect("matched"));
            git(
                source,
                &[
                    "worktree", "add", "--quiet", "--track", "-b", b, &dest_str, &tracking,
                ],
            )?;
            format!("new branch {b} tracking {tracking}")
        }
        Start::Branch(b) => {
            git(
                source,
                &[
                    "worktree",
                    "add",
                    "--quiet",
                    "--no-track",
                    "-b",
                    b,
                    &dest_str,
                    base,
                ],
            )?;
            format!("new branch {b} at {base}")
        }
    };
    let head = git(&dest, &["rev-parse", "--short", "HEAD"])?;
    println!("{}: {how} ({head})", dest.display());
    Ok(dest)
}

/// The repo of a checkout name, refusing names that are not a checkout.
pub fn checkout_repo(name: &str) -> Result<String> {
    if name.is_empty() || name.starts_with('.') || name.contains('/') {
        bail!("{name:?} is not a checkout name: <repo> or <repo>@<input>");
    }
    Ok(match parse_shared_name(name)? {
        Some((repo, _)) => repo,
        None => name.to_string(),
    })
}

/// `swl env` for the workspace after checkouts were added (`what`), skipped
/// with a note while it has no consumer checkout. Its failure says that the
/// checkouts are there and how to finish.
pub fn env_after_add(config: &Config, root: &Path, swl: &Path, what: &str) -> Result<()> {
    let ws = Workspace::scan(root)?;
    if !ws.checkouts.iter().any(|c| c.is_consumer()) {
        println!(
            "no consumer checkout in {} yet; swl env runs once one is added",
            ws.root.display()
        );
        return Ok(());
    }
    env::run(&ws, config, swl, false).with_context(|| {
        format!(
            "{what} added, but swl env failed; fix that and run `swl env --workspace {}` \
             (`swl env --force` replaces a hand-written .envrc.local)",
            ws.root.display()
        )
    })
}

/// `swl ws add`: [`add`], then `swl env` unless `swl` is `None` (`--no-env`).
pub fn run(
    config: &Config,
    root: &Path,
    name: &str,
    branch: Option<&str>,
    fetcher: &Fetcher,
    swl: Option<&Path>,
) -> Result<()> {
    let start = match branch {
        Some(b) => Start::Branch(b),
        None => Start::Base,
    };
    let dest = add(
        config,
        root,
        name,
        start,
        fetcher,
        &Report::new("swl ws add"),
    )?;
    match swl {
        Some(swl) => env_after_add(config, root, swl, &dest.display().to_string()),
        None => {
            println!("run `swl env` to update the overrides");
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tempdir::TempDir;

    fn g(dir: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new(crate::git::GIT)
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@example.org"])
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    #[test]
    fn base_remote_is_a_remote_of_the_source() {
        let tmp = TempDir::new("swl-ws-add-test").unwrap();
        let dir = tmp.path();
        g(dir, &["init", "-q", "--bare"]);
        g(dir, &["remote", "add", "origin", "/nowhere"]);
        assert_eq!(
            base_remote(dir, "origin/master").unwrap().as_deref(),
            Some("origin")
        );
        assert_eq!(base_remote(dir, "upstream/master").unwrap(), None);
        assert_eq!(base_remote(dir, "master").unwrap(), None);
    }

    #[test]
    fn worktree_list_porcelain() {
        let text = "worktree /src\nbare\n\n\
            worktree /ws/a\nHEAD 1111\nbranch refs/heads/feature/x\nprunable gitdir file points to non-existent location\n\n\
            worktree /ws/b\nHEAD 2222\ndetached\nlocked\n\n\
            worktree /ws/c\nHEAD 3333\nbranch refs/heads/c\nlocked because\n";
        let w = |path: &str, head: Option<&str>, branch: Option<&str>, locked, prunable| Worktree {
            path: PathBuf::from(path),
            head: head.map(str::to_string),
            branch: branch.map(str::to_string),
            locked,
            prunable,
        };
        assert_eq!(
            parse_worktrees(text),
            [
                w("/src", None, None, false, false),
                w("/ws/a", Some("1111"), Some("feature/x"), false, true),
                w("/ws/b", Some("2222"), None, true, false),
                w("/ws/c", Some("3333"), Some("c"), true, false),
            ]
        );
    }

    #[test]
    fn checkout_names() {
        assert_eq!(checkout_repo("swan-lake").unwrap(), "swan-lake");
        assert_eq!(
            checkout_repo("swan-lake-common@common").unwrap(),
            "swan-lake-common"
        );
        for bad in ["", ".swl", "a/b", "a@"] {
            assert!(checkout_repo(bad).is_err(), "{bad}");
        }
    }
}

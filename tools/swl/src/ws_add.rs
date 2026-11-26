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
    git::{git, git_ok},
    style::{self, Tone},
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

/// The worktree that has `branch` checked out, if any.
fn checked_out_at(source: &Path, branch: &str) -> Result<Option<String>> {
    let list = git(source, &["worktree", "list", "--porcelain"])?;
    let mut path = None;
    for line in list.lines() {
        if let Some(p) = line.strip_prefix("worktree ") {
            path = Some(p.to_string());
        } else if line == format!("branch refs/heads/{branch}") {
            return Ok(path);
        }
    }
    Ok(None)
}

/// Checks out the existing local `branch`, fast-forwarding it first when it
/// is strictly behind the remote's. Never resets: a diverged branch is left
/// with a warning.
fn existing_branch(source: &Path, remote: Option<&str>, branch: &str) -> Result<String> {
    if let Some(at) = checked_out_at(source, branch)? {
        bail!("branch {branch} is already checked out at {at}");
    }
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
    if is_ancestor(source, &local, &theirs) {
        git(
            source,
            &[
                "update-ref",
                "-m",
                &format!("swl ws add: fast-forward to {tracking}"),
                &format!("refs/heads/{branch}"),
                &theirs,
                &local,
            ],
        )?;
        return Ok(format!(
            "existing branch {branch}, fast-forwarded to {tracking}"
        ));
    }
    style::tell(
        "swl ws add",
        Tone::Warn,
        format!(
            "warning: {branch} and {tracking} have diverged; left as it is (merge or \
         rebase in the checkout)"
        ),
    );
    Ok(format!(
        "existing branch {branch}, diverged from {tracking}"
    ))
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
            let how = existing_branch(source, remote.as_deref(), b)?;
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
    let dest = add(config, root, name, start, fetcher)?;
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

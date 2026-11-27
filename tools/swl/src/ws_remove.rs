//! `swl ws remove`: drops a workspace that is abandoned, or to be joined
//! afresh. Each checkout goes with `git worktree remove` from its source,
//! its branch with it when the branch's commits are pushed, then the
//! directory. Nothing is removed while a checkout holds work that is not
//! committed or pushed, unless `--force`.

use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};

use crate::{
    config::Config,
    env::GENERATED_MARKER,
    git::{STATUS_PORCELAIN, git, git_ok, unpushed, valid_branch},
    land::disposable,
    report::Report,
    start::common_dir,
    style::Tone,
    workspace::{Checkout, Workspace},
    ws_add::{Fetcher, base_remote, checkout_repo, worktrees},
    ws_prune::uncommitted,
};

/// A checkout to remove, and what of it would be lost.
struct Removal<'a> {
    checkout: &'a Checkout,
    repo: String,
    source: PathBuf,
    /// The source's base remote: only its branches count as pushed.
    remote: Option<String>,
    /// HEAD and its branch when the checkout was checked.
    head: String,
    branch: Option<String>,
    /// `refs/remotes/<remote>/<branch>`, when there is one.
    tracking: Option<String>,
    /// The branch that keeps the commits pushed nowhere of a detached
    /// checkout removed with `--force`: `swl-kept/<workspace>-<checkout>`.
    kept_branch: String,
    /// Why it holds work: uncommitted or untracked files, commits pushed
    /// nowhere.
    held: Vec<String>,
    /// Its ignored files that removing it deletes (but [`disposable`] ones).
    ignored: Vec<String>,
}

/// The commits of a list, for a message: one per line, at most ten.
fn commit_lines(commits: &[String]) -> String {
    let mut lines: Vec<String> = commits.iter().take(10).cloned().collect();
    if commits.len() > 10 {
        lines.push(format!("… and {} more", commits.len() - 10));
    }
    lines.join("\n")
}

/// The ignored files of the checkout at `dir`, as `git status --porcelain
/// --ignored` shows them (a directory as `<dir>/`), but [`disposable`] ones.
fn ignored_files(dir: &Path) -> Result<Vec<String>> {
    Ok(git(dir, &[&STATUS_PORCELAIN[..], &["--ignored"]].concat())?
        .lines()
        .filter_map(|l| l.strip_prefix("!! "))
        .filter(|path| !disposable(dir, path, true))
        .map(str::to_string)
        .collect())
}

/// The checkout as one to remove, after the source's base remote is
/// fetched (unless `--no-fetch`). It must be a linked worktree of its
/// repo's configured source, there and not through a symbolic link, not
/// the source's main working tree, and not locked; the branch that would
/// keep its commits with `--force` must be free.
fn plan<'a>(
    config: &Config,
    workspace: &str,
    c: &'a Checkout,
    force: bool,
    fetcher: &Fetcher,
) -> Result<Removal<'a>> {
    let repo = checkout_repo(&c.name)?;
    let rc = config.repo(&repo)?;
    let source = rc.source.clone();
    let remote = base_remote(&source, rc.base())?;
    if let Some(remote) = &remote {
        fetcher.fetch(&source, remote)?;
    }
    let dir = c.dir.display();
    if fs::symlink_metadata(&c.dir)
        .with_context(|| format!("reading {dir}"))?
        .file_type()
        .is_symlink()
    {
        bail!("{dir} is a symbolic link; nothing was removed (remove the link by hand)");
    }
    let top = git_ok(&c.dir, &["rev-parse", "--show-toplevel"])
        .map(PathBuf::from)
        .and_then(|t| t.canonicalize().ok());
    if top.as_deref() != Some(c.dir.as_path())
        || common_dir(&c.dir).is_none()
        || common_dir(&c.dir) != common_dir(&source)
    {
        bail!(
            "{dir} is not a worktree of {}; nothing was removed (move it away, or remove it by hand)",
            source.display()
        );
    }
    let all = worktrees(&source)?;
    match all
        .iter()
        .position(|w| w.path.canonicalize().is_ok_and(|p| p == c.dir))
    {
        Some(0) => bail!(
            "{dir} is the main working tree of {}, not a worktree of a workspace; nothing was \
             removed",
            source.display()
        ),
        Some(i) if all[i].locked => bail!(
            "{dir} is locked; nothing was removed (git -C {} worktree unlock {dir})",
            source.display()
        ),
        Some(_) => {}
        None => bail!(
            "{dir} is not a worktree of {}; nothing was removed (move it away, or remove it by hand)",
            source.display()
        ),
    }
    let head = git(&c.dir, &["rev-parse", "HEAD"])?;
    let branch = git_ok(&c.dir, &["symbolic-ref", "--quiet", "--short", "HEAD"]);
    let tracking = match (&remote, &branch) {
        (Some(r), Some(b)) => Some(format!("refs/remotes/{r}/{b}"))
            .filter(|t| git_ok(&c.dir, &["rev-parse", "--verify", "--quiet", t]).is_some()),
        _ => None,
    };
    let mut held = Vec::new();
    let changed = uncommitted(&c.dir)?;
    if !changed.is_empty() {
        held.push(format!(
            "uncommitted or untracked files: {}",
            changed.join(", ")
        ));
    }
    let commits = unpushed(&c.dir, "HEAD", remote.as_deref(), tracking.as_deref())?;
    if !commits.is_empty() {
        held.push(format!(
            "commits on no remote branch:\n{}",
            commit_lines(&commits)
        ));
    }
    let kept_branch = format!("swl-kept/{workspace}-{}", c.name);
    if force && branch.is_none() && !commits.is_empty() {
        if !valid_branch(&kept_branch) {
            bail!(
                "{kept_branch:?}, the branch to keep the commits of the detached {dir}, is not a \
                 valid branch name; nothing was removed"
            );
        }
        if git_ok(
            &source,
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("refs/heads/{kept_branch}"),
            ],
        )
        .is_some()
        {
            bail!(
                "the branch {kept_branch} of {repo}, to keep the commits of the detached {dir}, \
                 exists already: rename or delete it (git -C {} branch), then rerun; nothing was \
                 removed",
                source.display()
            );
        }
    }
    Ok(Removal {
        checkout: c,
        repo,
        source,
        remote,
        head,
        branch,
        tracking,
        kept_branch,
        held,
        ignored: ignored_files(&c.dir)?,
    })
}

/// Removes the checkout's worktree, then its branch when the branch's
/// commits are pushed (else the branch is kept, and said so). Without
/// `--force`, only when its HEAD, branch and files are as [`plan`] found
/// them. With `--force`, the commits pushed nowhere of a detached checkout
/// are kept on the branch `kept_branch`, and said so.
fn remove(report: &Report, r: &Removal, force: bool) -> Result<()> {
    let dir = &r.checkout.dir;
    let head = git(dir, &["rev-parse", "HEAD"])?;
    let branch = git_ok(dir, &["symbolic-ref", "--quiet", "--short", "HEAD"]);
    if !force {
        let changed = uncommitted(dir)?;
        if head != r.head || branch != r.branch || !changed.is_empty() {
            bail!(
                "{} changed while swl ws remove ran (its HEAD, branch or files); it was kept: \
                 rerun swl ws remove",
                dir.display()
            );
        }
    }
    if force && branch.is_none() {
        let commits = unpushed(dir, "HEAD", r.remote.as_deref(), None)?;
        if !commits.is_empty() {
            let kept = format!("refs/heads/{}", r.kept_branch);
            git(
                &r.source,
                &[
                    "update-ref",
                    "-m",
                    "swl ws remove: keep the commits of a detached checkout",
                    &kept,
                    &head,
                    "",
                ],
            )
            .with_context(|| format!("keeping the commits of {} on {kept}", dir.display()))?;
            report.warn(format!(
                "kept the commits of {}, detached at {head}, on the branch {} of {}:\n{}\n\
                 git -C {} branch -D {1} drops them",
                r.checkout.name,
                r.kept_branch,
                r.repo,
                commit_lines(&commits),
                r.source.display()
            ));
        }
    }
    if !r.ignored.is_empty() {
        report.step(format!(
            "deleting the ignored files of {}: {}",
            r.checkout.name,
            r.ignored.join(", ")
        ));
    }
    let envrc = dir.join(".envrc.local");
    if fs::read_to_string(&envrc).is_ok_and(|t| t.starts_with(GENERATED_MARKER)) {
        fs::remove_file(&envrc).with_context(|| format!("removing {}", envrc.display()))?;
    }
    let dir_s = dir.to_string_lossy();
    let mut args = vec!["worktree", "remove"];
    if force {
        args.push("--force");
    }
    args.push(&dir_s);
    git(&r.source, &args).with_context(|| format!("removing the worktree {}", dir.display()))?;
    report.outcome(Tone::Good, format!("removed {}", dir.display()));
    let Some(b) = &branch else {
        return Ok(());
    };
    let commits = unpushed(
        &r.source,
        &format!("refs/heads/{b}"),
        r.remote.as_deref(),
        // With --force, the checkout may have switched branches since it
        // was checked: no remote branch is known for the new one.
        r.tracking
            .as_deref()
            .filter(|_| r.branch.as_ref() == Some(b)),
    )?;
    if !commits.is_empty() {
        report.warn(format!(
            "kept the branch {b} of {}, which has commits on no remote branch:\n{}\n\
             git -C {} branch -D {b} drops them",
            r.repo,
            commit_lines(&commits),
            r.source.display()
        ));
        return Ok(());
    }
    match git(&r.source, &["branch", "--quiet", "-D", b]) {
        Ok(_) => report.outcome(
            Tone::Good,
            format!(
                "deleted the branch {b} of {}: its commits are on remote branches",
                r.repo
            ),
        ),
        Err(e) => report.warn(format!("kept the branch {b} of {}: {e:#}", r.repo)),
    }
    Ok(())
}

/// `swl ws remove`: the workspace at `root`, from `cwd`.
pub fn run(config: &Config, root: &Path, cwd: &Path, force: bool, fetcher: &Fetcher) -> Result<()> {
    let ws = Workspace::scan(root)?;
    let dot_swl = ws.root.join(".swl");
    if ws.checkouts.is_empty() && !dot_swl.is_dir() {
        bail!(
            "{} is not a workspace: it has no checkouts and no .swl; nothing was removed",
            ws.root.display()
        );
    }
    let name = ws
        .root
        .file_name()
        .map_or(String::new(), |n| n.to_string_lossy().into_owned());
    let removals = ws
        .checkouts
        .iter()
        .map(|c| plan(config, &name, c, force, fetcher))
        .collect::<Result<Vec<_>>>()?;
    let report = Report::new("swl ws remove");
    report.section(format!("workspace {}", ws.root.display()));
    let held: Vec<&Removal> = removals.iter().filter(|r| !r.held.is_empty()).collect();
    for r in &held {
        let why = r.held.join("; ");
        if force {
            report.warn(format!(
                "removing {} anyway (--force): {why}",
                r.checkout.name
            ));
        } else {
            report.warn(format!("keeping {}: {why}", r.checkout.name));
        }
    }
    if !force && !held.is_empty() {
        report.end(
            Tone::Warn,
            format!(
                "kept the workspace {}; nothing was removed",
                ws.root.display()
            ),
        );
        bail!(
            "{} holds work that is not committed or pushed (above): commit and push it, or pass \
             --force to remove the workspace anyway",
            ws.root.display()
        );
    }
    // The current directory may be in the workspace: out of it, so that
    // nothing that runs on is left in a removed directory.
    let inside = cwd
        .canonicalize()
        .is_ok_and(|cwd| cwd.starts_with(&ws.root));
    if inside && let Some(parent) = ws.root.parent() {
        std::env::set_current_dir(parent)
            .with_context(|| format!("changing to {}", parent.display()))?;
    }
    for r in &removals {
        if let Err(e) = remove(&report, r, force) {
            report.end(
                Tone::Error,
                format!(
                    "stopped at {}: what is not said removed above is kept",
                    r.checkout.name
                ),
            );
            return Err(e);
        }
    }
    if dot_swl.exists() {
        fs::remove_dir_all(&dot_swl).with_context(|| format!("removing {}", dot_swl.display()))?;
    }
    match fs::remove_dir(&ws.root) {
        Ok(()) => report.end(Tone::Good, format!("removed {}", ws.root.display())),
        Err(e) => report.end(
            Tone::Warn,
            format!("kept {}, which has other files: {e}", ws.root.display()),
        ),
    }
    if inside {
        report.say(
            Tone::Plain,
            format!(
                "the current directory was in {}; cd out of it",
                ws.root.display()
            ),
        );
    }
    Ok(())
}

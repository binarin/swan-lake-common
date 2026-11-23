//! `swl ws add`: a git worktree in the workspace.

use std::{fs, path::Path};

use anyhow::{Context, Result, bail};

use crate::{
    config::Config,
    git::{git, git_ok},
    workspace::parse_shared_name,
};

/// Adds the checkout `name` (`<repo>` or `<repo>@<input>`) to the workspace
/// `root`, from the repo's configured source. Without `branch` the worktree
/// is detached at the repo's base; with it, an existing branch is checked out
/// or a new one is created at the base, without an upstream.
pub fn run(config: &Config, root: &Path, name: &str, branch: Option<&str>) -> Result<()> {
    if name.is_empty() || name.starts_with('.') || name.contains('/') {
        bail!("{name:?} is not a checkout name: <repo> or <repo>@<input>");
    }
    let repo = match parse_shared_name(name)? {
        Some((repo, _)) => repo,
        None => name.to_string(),
    };
    let repo_config = config.repo(&repo)?;
    let source = &repo_config.source;
    let base = repo_config.base();
    fs::create_dir_all(root).with_context(|| format!("creating {}", root.display()))?;
    let dest = root.canonicalize()?.join(name);
    if dest.exists() {
        bail!("{} already exists", dest.display());
    }
    let dest_str = dest.to_string_lossy().into_owned();
    let how = match branch {
        None => {
            git(
                source,
                &["worktree", "add", "--quiet", "--detach", &dest_str, base],
            )?;
            format!("detached at {base}")
        }
        Some(b)
            if git_ok(
                source,
                &[
                    "rev-parse",
                    "--verify",
                    "--quiet",
                    &format!("refs/heads/{b}"),
                ],
            )
            .is_some() =>
        {
            git(source, &["worktree", "add", "--quiet", &dest_str, b])?;
            format!("existing branch {b}")
        }
        Some(b) => {
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
    println!(
        "{}: {how} ({head}); run `swl env` to update the overrides",
        dest.display()
    );
    Ok(())
}

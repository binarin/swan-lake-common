//! Which workspaces `swl ws prune` removes (SL-95), and `swl ls` marks
//! `removable`: those whose every checkout holds nothing that is not in its
//! base. Read from git alone, with the refs already known (the caller
//! fetches).

use std::path::Path;

use anyhow::Result;

use crate::{
    config::Config,
    git::{git, git_ok, short},
    land::disposable,
    workspace::{Checkout, Workspace},
    ws_add::base_remote,
};

/// Why the workspace is not removable, or `None` when it is: every
/// checkout's HEAD is in its base (detached at the base too), its branch is
/// gone on the remote or in the base, and `git status --porcelain` shows no
/// changes and no untracked files but swl's generated `.envrc.local`
/// (ignored files do not count). A workspace without checkouts is kept.
pub fn keep_reason(ws: &Workspace, config: &Config) -> Result<Option<String>> {
    if ws.checkouts.is_empty() {
        return Ok(Some("no checkouts".to_string()));
    }
    for c in &ws.checkouts {
        let repo = c
            .shared
            .as_ref()
            .map_or(c.name.as_str(), |(r, _)| r.as_str());
        if let Some(why) = checkout_keep_reason(c, config.base_for(repo))? {
            return Ok(Some(format!("{}: {why}", c.name)));
        }
    }
    Ok(None)
}

fn in_base(dir: &Path, rev: &str, base: &str) -> bool {
    git_ok(dir, &["merge-base", "--is-ancestor", rev, base]).is_some()
}

/// Why the checkout holds something not in `base`, if it does.
fn checkout_keep_reason(c: &Checkout, base: &str) -> Result<Option<String>> {
    let dir = &c.dir;
    if git_ok(
        dir,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{base}^{{commit}}"),
        ],
    )
    .is_none()
    {
        return Ok(Some(format!("its base {base} is not known here")));
    }
    let changed: Vec<String> = git(dir, &["status", "--porcelain"])?
        .lines()
        .filter(|l| {
            l.strip_prefix("?? ")
                .is_none_or(|path| !disposable(dir, path, false))
        })
        .map(str::to_string)
        .collect();
    if !changed.is_empty() {
        return Ok(Some(format!(
            "uncommitted or untracked files: {}",
            changed.join(", ")
        )));
    }
    let head = git(dir, &["rev-parse", "HEAD"])?;
    if !in_base(dir, &head, base) {
        return Ok(Some(format!("HEAD {} is not in {base}", short(&head))));
    }
    if let Some(branch) = git_ok(dir, &["symbolic-ref", "--quiet", "--short", "HEAD"])
        && let Some(remote) = base_remote(dir, base)?
    {
        let tracking = format!("refs/remotes/{remote}/{branch}");
        if git_ok(dir, &["rev-parse", "--verify", "--quiet", &tracking]).is_some()
            && !in_base(dir, &tracking, base)
        {
            return Ok(Some(format!("{remote}/{branch} has commits not in {base}")));
        }
    }
    Ok(None)
}

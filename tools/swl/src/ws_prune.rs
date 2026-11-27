//! Which workspaces `swl ws prune` removes (SL-95), and `swl ls` marks
//! `removable`: those whose every checkout holds nothing that is not in its
//! base. Read from git alone, with the refs already known (the caller
//! fetches).

use std::path::Path;

use anyhow::Result;

use crate::{
    config::Config,
    git::{STATUS_PORCELAIN, git, git_ok, short},
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

/// The `git status --porcelain` lines of the checkout at `dir`: its changes
/// and untracked files, but swl's generated `.envrc.local`; ignored files
/// do not count. Also what `swl ws remove` refuses without `--force`.
pub fn uncommitted(dir: &Path) -> Result<Vec<String>> {
    Ok(git(dir, &STATUS_PORCELAIN)?
        .lines()
        .filter(|l| {
            l.strip_prefix("?? ")
                .is_none_or(|path| !disposable(dir, path, false))
        })
        .map(str::to_string)
        .collect())
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
    let changed = uncommitted(dir)?;
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

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf, process::Command};

    use super::*;
    use crate::{env::GENERATED_MARKER, git::GIT, tempdir::TempDir};

    fn g(dir: &Path, args: &[&str]) -> String {
        let out = Command::new(GIT)
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@example.org"])
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// A clone of a forge repo with one commit on master, detached at
    /// `origin/master`.
    fn checkout(tmp: &TempDir) -> Checkout {
        let forge = tmp.path().join("forge");
        let dir = tmp.path().join("swan-lake");
        fs::create_dir_all(&forge).unwrap();
        g(&forge, &["init", "-q", "--bare", "-b", "master"]);
        g(
            tmp.path(),
            &["clone", "-q", &forge.display().to_string(), "swan-lake"],
        );
        g(&dir, &["commit", "-q", "--allow-empty", "-m", "base"]);
        g(&dir, &["push", "-q", "origin", "HEAD:master"]);
        g(&dir, &["fetch", "-q", "origin"]);
        g(&dir, &["checkout", "-q", "--detach", "origin/master"]);
        Checkout {
            name: "swan-lake".into(),
            dir,
            shared: None,
        }
    }

    fn keep(c: &Checkout) -> Option<String> {
        checkout_keep_reason(c, "origin/master").unwrap()
    }

    #[test]
    fn a_checkout_detached_at_its_base_is_removable() {
        let tmp = TempDir::new("swl-ws-prune-test").unwrap();
        let c = checkout(&tmp);
        assert_eq!(keep(&c), None);
        g(&c.dir, &["commit", "-q", "--allow-empty", "-m", "local"]);
        assert!(keep(&c).unwrap().contains("is not in origin/master"));
    }

    #[test]
    fn a_branch_whose_remote_branch_is_in_the_base_is_removable() {
        let tmp = TempDir::new("swl-ws-prune-test").unwrap();
        let c = checkout(&tmp);
        g(&c.dir, &["switch", "-q", "-c", "sl-1-x"]);
        g(&c.dir, &["commit", "-q", "--allow-empty", "-m", "change"]);
        g(&c.dir, &["push", "-q", "origin", "sl-1-x"]);
        // Pushed, and not merged: kept, also with HEAD back at the base.
        g(&c.dir, &["reset", "-q", "--hard", "origin/master"]);
        assert_eq!(
            keep(&c).as_deref(),
            Some("origin/sl-1-x has commits not in origin/master")
        );
        // Merged: the remote branch is in the base.
        g(&c.dir, &["push", "-q", "origin", "origin/sl-1-x:master"]);
        g(&c.dir, &["fetch", "-q", "origin"]);
        assert_eq!(keep(&c), None);
    }

    #[test]
    fn only_swls_generated_envrc_local_does_not_keep_a_checkout() {
        let tmp = TempDir::new("swl-ws-prune-test").unwrap();
        let c = checkout(&tmp);
        let envrc: PathBuf = c.dir.join(".envrc.local");
        fs::write(&envrc, "use_flake_args+=(--impure)\n").unwrap();
        assert_eq!(
            keep(&c).as_deref(),
            Some("uncommitted or untracked files: ?? .envrc.local")
        );
        fs::write(&envrc, format!("{GENERATED_MARKER} for the workspace x\n")).unwrap();
        assert!(envrc.is_file());
        assert_eq!(keep(&c), None);
        fs::write(c.dir.join("notes.txt"), "x").unwrap();
        assert_eq!(
            keep(&c).as_deref(),
            Some("uncommitted or untracked files: ?? notes.txt")
        );
    }
}

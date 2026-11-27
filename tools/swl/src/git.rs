use std::{path::Path, process::Command};

use anyhow::{Context, Result, bail};

/// The git binary: the package sets `SWL_GIT` at build time.
pub const GIT: &str = match option_env!("SWL_GIT") {
    Some(g) => g,
    None => "git",
};

/// Runs git in `dir`; stdout without the trailing newline, or an error with
/// git's stderr.
pub fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let out = crate::logging::output(Command::new(GIT).arg("-C").arg(dir).args(args))
        .with_context(|| format!("running {GIT}"))?;
    if !out.status.success() {
        bail!(
            "git -C {} {} failed: {}",
            dir.display(),
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim_end().to_string())
}

/// Like [`git`], but `None` when git fails.
pub fn git_ok(dir: &Path, args: &[&str]) -> Option<String> {
    git(dir, args).ok()
}

/// The first 12 characters of a revision, for messages; never splits a
/// character, whatever the input.
pub fn short(rev: &str) -> &str {
    rev.get(..12).unwrap_or(rev)
}

/// The commits of `rev` that are pushed nowhere, as `<short sha> <subject>`,
/// newest first: those on no remote-tracking branch of `remote` (the one
/// the command fetched, or would fetch but for `--no-fetch`; none counts
/// without it) in the repository at `dir`, whose change is not on the
/// remote branch `tracking` either (by patch id, as `git cherry` tells it).
/// So the old commits of a branch that was rebased and force-pushed count
/// as pushed; amended ones, or a rebase that resolved conflicts, do not.
/// An empty commit is never the same patch as another (all empty commits
/// have one patch id): it is pushed only when on a remote branch.
pub fn unpushed(
    dir: &Path,
    rev: &str,
    remote: Option<&str>,
    tracking: Option<&str>,
) -> Result<Vec<String>> {
    let remotes = remote.map(|r| format!("--remotes={r}"));
    let mut args = vec!["log", "--format=%H %h %s", rev];
    if let Some(remotes) = &remotes {
        args.extend(["--not", remotes]);
    }
    let log = git(dir, &args)?;
    if log.is_empty() {
        return Ok(Vec::new());
    }
    let mut equivalent = Vec::new();
    if let Some(t) = tracking {
        for sha in git(dir, &["cherry", t, rev])?
            .lines()
            .filter_map(|l| l.strip_prefix("- "))
        {
            if changes_something(dir, sha)? {
                equivalent.push(sha.to_string());
            }
        }
    }
    Ok(log
        .lines()
        .filter_map(|l| l.split_once(' '))
        .filter(|(sha, _)| !equivalent.iter().any(|e| e == sha))
        .map(|(_, line)| line.to_string())
        .collect())
}

/// Whether the (non-merge) commit `sha` changes any file:
/// `git diff-tree --quiet` exits 1 when it does.
fn changes_something(dir: &Path, sha: &str) -> Result<bool> {
    let out = crate::logging::output(Command::new(GIT).arg("-C").arg(dir).args([
        "diff-tree",
        "--quiet",
        "--root",
        "-r",
        sha,
    ]))
    .with_context(|| format!("running {GIT}"))?;
    match out.status.code() {
        Some(0) => Ok(false),
        Some(1) => Ok(true),
        _ => bail!(
            "git -C {} diff-tree --quiet --root -r {sha} failed: {}",
            dir.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        ),
    }
}

/// A full revision: 40 lower-case hex digits.
pub fn is_rev(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// A valid branch name (`git check-ref-format --branch`), not an option.
pub fn valid_branch(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('-')
        && crate::logging::status(
            Command::new(GIT)
                .args(["check-ref-format", "--branch", name])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null()),
        )
        .is_ok_and(|s| s.success())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tempdir::TempDir;

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

    #[test]
    fn unpushed_counts_the_fetched_remote_and_the_same_patch_but_no_empty_commit() {
        let tmp = TempDir::new("swl-git-test").unwrap();
        let dir = tmp.path();
        g(dir, &["init", "-q", "-b", "master"]);
        std::fs::write(dir.join("a"), "a").unwrap();
        g(dir, &["add", "a"]);
        g(dir, &["commit", "-qm", "base"]);
        g(dir, &["update-ref", "refs/remotes/origin/master", "HEAD"]);
        // Local: a change, then an empty commit.
        g(dir, &["switch", "-q", "-c", "feature"]);
        std::fs::write(dir.join("b"), "b").unwrap();
        g(dir, &["add", "b"]);
        g(dir, &["commit", "-qm", "change"]);
        let change = g(dir, &["rev-parse", "HEAD"]);
        g(dir, &["commit", "-q", "--allow-empty", "-m", "local empty"]);
        let empty = g(dir, &["log", "-1", "--format=%h local empty"]);
        let local = g(dir, &["rev-parse", "HEAD"]);
        // origin/feature: the same change as another commit, and an empty one.
        g(dir, &["switch", "-q", "--detach", "master"]);
        g(dir, &["cherry-pick", change.as_str()]);
        g(dir, &["commit", "-q", "--amend", "-m", "change, rebased"]);
        g(dir, &["commit", "-q", "--allow-empty", "-m", "their empty"]);
        g(dir, &["update-ref", "refs/remotes/origin/feature", "HEAD"]);
        // Another remote that has it all, which this run did not fetch.
        g(dir, &["update-ref", "refs/remotes/other/feature", &local]);
        let tracking = Some("refs/remotes/origin/feature");

        assert_eq!(
            unpushed(dir, &local, Some("origin"), tracking).unwrap(),
            [empty]
        );
        assert_eq!(unpushed(dir, &local, Some("other"), None).unwrap(), [""; 0]);
        // Without the remote branch, both; without a remote, the base too.
        assert_eq!(
            unpushed(dir, &local, Some("origin"), None).unwrap().len(),
            2
        );
        assert_eq!(unpushed(dir, &local, None, None).unwrap().len(), 3);
        // On the fetched remote's branches, an empty commit is pushed.
        g(dir, &["update-ref", "refs/remotes/origin/feature", &local]);
        assert_eq!(
            unpushed(dir, &local, Some("origin"), tracking).unwrap(),
            [""; 0]
        );
    }

    #[test]
    fn revisions_and_branches() {
        assert_eq!(short("0123456789abcdef"), "0123456789ab");
        assert_eq!(short("abc"), "abc");
        // A multi-byte character across byte 12 is not split.
        assert_eq!(short("aaaaaaaaaaaé"), "aaaaaaaaaaaé");
        assert!(is_rev(&"a".repeat(40)));
        assert!(!is_rev(&"A".repeat(40)));
        assert!(!is_rev("."));
        assert!(valid_branch("swan-lake-current"));
        assert!(valid_branch("feature/x"));
        for bad in ["", "-x", "--upload-pack=x", "a..b", "a b", "a~1", "x.lock"] {
            assert!(!valid_branch(bad), "{bad}");
        }
    }
}

//! Whether one commit is in another's history, asked of a remote repository
//! by fetching commit history only (no trees or blobs) into a scratch bare
//! repository, or for GitHub sources of GitHub's compare API.

use std::{
    io::Read,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use serde_json::Value;

use crate::{
    git::{GIT, is_rev, valid_branch},
    tempdir::TempDir,
};

const TIMEOUT: Duration = Duration::from_secs(60);

/// The git URL of a locked flake source: `github` and `git` sources.
pub fn git_url(locked: &Value) -> Option<String> {
    let s = |k: &str| locked.get(k).and_then(Value::as_str);
    match s("type")? {
        "github" => Some(format!(
            "https://github.com/{}/{}.git",
            s("owner")?,
            s("repo")?
        )),
        "git" => s("url").map(str::to_string),
        _ => None,
    }
}

/// Where a locked flake source's history is read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    GitHub { owner: String, repo: String },
    Git { url: String },
}

/// Allows `file://` git sources, which are refused otherwise; for tests.
pub const ALLOW_FILE_URLS_ENV: &str = "SWL_ALLOW_FILE_URLS";

/// A GitHub owner or repository name.
pub fn valid_github_name(s: &str) -> bool {
    !s.is_empty()
        && s != "."
        && s != ".."
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}

/// An `https://` URL without spaces or control characters; `file://` too
/// when [`ALLOW_FILE_URLS_ENV`] is `1`.
fn valid_git_url(url: &str) -> bool {
    let file_ok = std::env::var(ALLOW_FILE_URLS_ENV).is_ok_and(|v| v == "1");
    (url.starts_with("https://") || (file_ok && url.starts_with("file://")))
        && !url
            .bytes()
            .any(|b| b.is_ascii_whitespace() || b.is_ascii_control())
}

impl Source {
    /// The source of a lock node's `locked` (or `original`) attributes:
    /// GitHub repositories on github.com, and git repositories over
    /// `https://`. Anything else is refused, with the reason.
    pub fn of(locked: &Value) -> Result<Self, String> {
        let s = |k: &str| locked.get(k).and_then(Value::as_str);
        let github = |owner: &str, repo: &str| {
            if valid_github_name(owner) && valid_github_name(repo) {
                Ok(Source::GitHub {
                    owner: owner.to_string(),
                    repo: repo.to_string(),
                })
            } else {
                Err(format!(
                    "{owner:?}/{repo:?} is not a GitHub repository name"
                ))
            }
        };
        match s("type") {
            Some("github") => {
                if s("host").is_some_and(|h| h != "github.com") {
                    return Err("a GitHub source on another host".into());
                }
                match (s("owner"), s("repo")) {
                    (Some(owner), Some(repo)) => github(owner, repo),
                    _ => Err("a GitHub source without owner or repo".into()),
                }
            }
            Some("git") => {
                let url = s("url").ok_or("a git source without a url")?;
                if !valid_git_url(url) {
                    return Err(format!("{url:?} is not an https:// git URL"));
                }
                let path = url
                    .strip_prefix("https://github.com/")
                    .map(|p| p.trim_end_matches('/').trim_end_matches(".git"));
                match path.and_then(|p| p.split_once('/')) {
                    Some((owner, repo)) if !repo.contains('/') => github(owner, repo),
                    _ => Ok(Source::Git {
                        url: url.to_string(),
                    }),
                }
            }
            Some(other) => Err(format!("a {other} source, not a git source")),
            None => Err("a source without a type".into()),
        }
    }
}

impl std::fmt::Display for Source {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Source::GitHub { owner, repo } => write!(f, "github:{owner}/{repo}"),
            Source::Git { url } => f.write_str(url),
        }
    }
}

/// Ancestry questions about a source's history. An error is "cannot
/// determine" (offline, rate limited, unknown revision).
pub trait History {
    /// Whether `ancestor` is `descendant` or in its history.
    fn contains(&self, source: &Source, ancestor: &str, descendant: &str) -> Result<bool, String>;
    /// Whether `rev` is in the history of `branch` (the default branch for
    /// `None`).
    fn in_branch(&self, source: &Source, rev: &str, branch: Option<&str>) -> Result<bool, String>;
}

/// The real sources: a history-only fetch for git, the compare API for
/// GitHub.
pub struct RemoteHistory {
    pub github: crate::github::GitHub,
}

impl RemoteHistory {
    pub fn from_env() -> Self {
        Self {
            github: crate::github::GitHub::from_env(),
        }
    }
}

/// The head of `refname` in the repository at `url`, by `ls-remote`. Never
/// prompts for credentials.
pub fn remote_head(url: &str, refname: &str) -> Result<String, String> {
    let out = Command::new(GIT)
        .args(["ls-remote", "--end-of-options", url, refname])
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("running {GIT}: {e}"))?;
    if !out.status.success() {
        return Err(last_line(
            &String::from_utf8_lossy(&out.stderr),
            "git ls-remote failed",
        ));
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .find(|l| l.split_whitespace().nth(1) == Some(refname))
        .and_then(|l| l.split_whitespace().next())
        .map(str::to_string)
        .ok_or_else(|| format!("no {refname} in {url}"))
}

fn check_rev(rev: &str) -> Result<(), String> {
    if is_rev(rev) {
        Ok(())
    } else {
        Err(format!("{rev:?} is not a full revision"))
    }
}

impl History for RemoteHistory {
    fn contains(&self, source: &Source, ancestor: &str, descendant: &str) -> Result<bool, String> {
        check_rev(ancestor)?;
        check_rev(descendant)?;
        match source {
            Source::GitHub { owner, repo } => self
                .github
                .compare(owner, repo, ancestor, descendant)
                .map(|c| c.contains_base()),
            Source::Git { url } => is_ancestor(url, ancestor, descendant, false),
        }
    }

    fn in_branch(&self, source: &Source, rev: &str, branch: Option<&str>) -> Result<bool, String> {
        check_rev(rev)?;
        if let Some(b) = branch
            && !valid_branch(b)
        {
            return Err(format!("{b:?} is not a branch name"));
        }
        match source {
            Source::GitHub { owner, repo } => {
                let branch = match branch {
                    Some(b) => b.to_string(),
                    None => self.github.default_branch(owner, repo)?,
                };
                self.github
                    .compare(owner, repo, rev, &branch)
                    .map(|c| c.contains_base())
            }
            Source::Git { url } => {
                let refname =
                    branch.map_or_else(|| "HEAD".to_string(), |b| format!("refs/heads/{b}"));
                let tip = remote_head(url, &refname)?;
                is_ancestor(url, rev, &tip, false)
            }
        }
    }
}

/// `Ok(true)` when `ancestor` is `descendant` or in its history, `Ok(false)`
/// when it is not, and `Err(reason)` when that cannot be determined (offline,
/// unknown revision). With `fetch_ancestor`, both revisions are fetched and a
/// missing ancestor is an error; without, only the descendant's history is
/// fetched and an ancestor outside it is `Ok(false)`.
pub fn is_ancestor(
    url: &str,
    ancestor: &str,
    descendant: &str,
    fetch_ancestor: bool,
) -> Result<bool, String> {
    check_rev(ancestor)?;
    check_rev(descendant)?;
    if ancestor == descendant {
        return Ok(true);
    }
    let tmp = TempDir::new("swl-ancestry").map_err(|e| e.to_string())?;
    let dir = tmp.path().to_string_lossy().into_owned();
    git(&["init", "-q", "--bare", &dir])?;
    let mut fetch = vec![
        "-C",
        &dir,
        "fetch",
        "-q",
        "--no-tags",
        "--filter=tree:0",
        "--end-of-options",
        url,
        descendant,
    ];
    if fetch_ancestor {
        fetch.push(ancestor);
    }
    git(&fetch)?;
    if !fetch_ancestor
        && git(&[
            "-C",
            &dir,
            "cat-file",
            "-e",
            &format!("{ancestor}^{{commit}}"),
        ])
        .is_err()
    {
        return Ok(false);
    }
    match run(&[
        "-C",
        &dir,
        "merge-base",
        "--is-ancestor",
        ancestor,
        descendant,
    ]) {
        Ok((0, _)) => Ok(true),
        Ok((1, _)) => Ok(false),
        Ok((_, err)) => Err(last_line(&err, "git merge-base failed")),
        Err(e) => Err(e),
    }
}

fn last_line(text: &str, default: &str) -> String {
    text.trim()
        .lines()
        .last()
        .map(str::to_string)
        .unwrap_or_else(|| default.to_string())
}

fn git(args: &[&str]) -> Result<(), String> {
    match run(args)? {
        (0, _) => Ok(()),
        (code, err) => Err(last_line(&err, &format!("git exited with {code}"))),
    }
}

/// `(exit code, stderr)` of git, killed after [`TIMEOUT`].
fn run(args: &[&str]) -> Result<(i32, String), String> {
    let mut child = Command::new(GIT)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_NO_LAZY_FETCH", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("running {GIT}: {e}"))?;
    let mut stderr = child.stderr.take();
    let reader = thread::spawn(move || {
        let mut s = String::new();
        if let Some(e) = stderr.as_mut() {
            let _ = e.read_to_string(&mut s);
        }
        s
    });
    let deadline = Instant::now() + TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("git {} timed out", args.get(2).unwrap_or(&"")));
            }
            Ok(None) => thread::sleep(Duration::from_millis(50)),
            Err(e) => return Err(e.to_string()),
        }
    };
    let err = reader.join().unwrap_or_default();
    Ok((status.code().unwrap_or(-1), err))
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use super::*;

    fn g(dir: &std::path::Path, args: &[&str]) -> String {
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

    /// Ported from test_workflow.py's `Ancestry.test_report`.
    #[test]
    fn ancestry_of_local_commits() {
        let tmp = TempDir::new("swl-ancestry-test").unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        g(&repo, &["init", "-q", "-b", "master"]);
        g(&repo, &["commit", "-q", "--allow-empty", "-m", "base"]);
        let base = g(&repo, &["rev-parse", "HEAD"]);
        g(&repo, &["commit", "-q", "--allow-empty", "-m", "next"]);
        let child = g(&repo, &["rev-parse", "HEAD"]);
        g(&repo, &["checkout", "-q", "-b", "side", &base]);
        g(&repo, &["commit", "-q", "--allow-empty", "-m", "side"]);
        let side = g(&repo, &["rev-parse", "HEAD"]);
        let url = format!("file://{}", repo.display());
        assert_eq!(is_ancestor(&url, &base, &base, true), Ok(true));
        assert_eq!(is_ancestor(&url, &base, &child, true), Ok(true));
        assert_eq!(is_ancestor(&url, &child, &side, true), Ok(false));
        assert_eq!(is_ancestor(&url, &child, &base, true), Ok(false));
        // Without fetching the ancestor: one outside the history is no.
        assert_eq!(is_ancestor(&url, &side, &child, false), Ok(false));
        assert_eq!(is_ancestor(&url, &base, &child, false), Ok(true));
        let missing = format!("file://{}", tmp.path().join("missing").display());
        assert!(is_ancestor(&missing, &base, &child, true).is_err());
        assert!(is_ancestor(&url, &base, &"f".repeat(40), true).is_err());
    }

    #[test]
    fn git_urls_of_locked_sources() {
        let v = |s: &str| serde_json::from_str::<Value>(s).unwrap();
        assert_eq!(
            git_url(&v(
                r#"{"type":"github","owner":"binarin","repo":"swan-lake-common"}"#
            ))
            .as_deref(),
            Some("https://github.com/binarin/swan-lake-common.git")
        );
        assert_eq!(
            git_url(&v(r#"{"type":"git","url":"https://f/x"}"#)).as_deref(),
            Some("https://f/x")
        );
        assert_eq!(git_url(&v(r#"{"type":"path","path":"/x"}"#)), None);
    }

    #[test]
    fn sources_of_locked_nodes() {
        let v = |s: &str| serde_json::from_str::<Value>(s).unwrap();
        let gh = Ok(Source::GitHub {
            owner: "NixOS".into(),
            repo: "nixpkgs".into(),
        });
        assert_eq!(
            Source::of(&v(r#"{"type":"github","owner":"NixOS","repo":"nixpkgs"}"#)),
            gh
        );
        assert_eq!(
            Source::of(&v(
                r#"{"type":"git","url":"https://github.com/NixOS/nixpkgs.git"}"#
            )),
            gh
        );
        assert_eq!(
            Source::of(&v(r#"{"type":"git","url":"https://f/x"}"#)),
            Ok(Source::Git {
                url: "https://f/x".into()
            })
        );
        assert!(Source::of(&v(r#"{"type":"path","path":"/x"}"#)).is_err());
        assert!(Source::of(&v(r#"{"type":"tarball","url":"https://x/a.tar.gz"}"#)).is_err());
    }

    #[test]
    fn sources_that_could_inject_are_refused() {
        let v = |s: &str| serde_json::from_str::<Value>(s).unwrap();
        for bad in [
            r#"{"type":"git","url":"--upload-pack=touch /tmp/x"}"#,
            r#"{"type":"git","url":"ssh://git@example.org/x"}"#,
            r#"{"type":"git","url":"file:///etc"}"#,
            r#"{"type":"git","url":"https://example.org/x y"}"#,
            r#"{"type":"git","url":"https://github.com/../x"}"#,
            r#"{"type":"github","owner":"..","repo":"x"}"#,
            r#"{"type":"github","owner":"o","repo":"x#y"}"#,
            r#"{"type":"github","owner":"o/../p","repo":"x"}"#,
            r#"{"type":"github","owner":"o","repo":"x","host":"evil.example"}"#,
        ] {
            if std::env::var(ALLOW_FILE_URLS_ENV).is_ok() && bad.contains("file://") {
                continue;
            }
            assert!(Source::of(&v(bad)).is_err(), "{bad}");
        }
        // Options and partial revisions never reach git.
        let h = RemoteHistory::from_env();
        let src = Source::Git {
            url: "https://example.invalid/x".into(),
        };
        let rev = "a".repeat(40);
        for bad in [".", "--upload-pack=x", "HEAD", "abc"] {
            assert!(
                h.contains(&src, bad, &rev)
                    .unwrap_err()
                    .contains("not a full revision")
            );
            assert!(h.contains(&src, &rev, bad).is_err());
            assert!(h.in_branch(&src, bad, None).is_err());
        }
        let e = h
            .in_branch(&src, &rev, Some("--upload-pack=x"))
            .unwrap_err();
        assert!(e.contains("not a branch name"), "{e}");
        assert!(is_ancestor("--upload-pack=x", &rev, &"b".repeat(40), false).is_err());
    }

    #[test]
    fn history_of_a_git_source() {
        let tmp = TempDir::new("swl-history-test").unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        g(&repo, &["init", "-q", "-b", "master"]);
        g(&repo, &["commit", "-q", "--allow-empty", "-m", "base"]);
        let base = g(&repo, &["rev-parse", "HEAD"]);
        g(&repo, &["checkout", "-q", "-b", "current"]);
        g(&repo, &["commit", "-q", "--allow-empty", "-m", "next"]);
        let next = g(&repo, &["rev-parse", "HEAD"]);
        g(&repo, &["checkout", "-q", "master"]);
        let h = RemoteHistory::from_env();
        let src = Source::Git {
            url: format!("file://{}", repo.display()),
        };
        assert_eq!(h.contains(&src, &base, &next), Ok(true));
        assert_eq!(h.contains(&src, &next, &base), Ok(false));
        assert_eq!(h.in_branch(&src, &next, Some("current")), Ok(true));
        assert_eq!(h.in_branch(&src, &next, None), Ok(false));
        assert_eq!(h.in_branch(&src, &base, None), Ok(true));
        assert!(h.in_branch(&src, &base, Some("nope")).is_err());
    }
}

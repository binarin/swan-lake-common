//! Whether one commit is in another's history, asked of a remote repository
//! by fetching commit history only (no trees or blobs) into a scratch bare
//! repository.

use std::{
    io::Read,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use serde_json::Value;

use crate::{git::GIT, tempdir::TempDir};

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
}

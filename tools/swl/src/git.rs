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
    let out = Command::new(GIT)
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
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

/// A full revision: 40 lower-case hex digits.
pub fn is_rev(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// A valid branch name (`git check-ref-format --branch`), not an option.
pub fn valid_branch(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('-')
        && std::process::Command::new(GIT)
            .args(["check-ref-format", "--branch", name])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
}

#[cfg(test)]
mod tests {
    use super::*;

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

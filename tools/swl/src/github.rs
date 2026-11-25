//! GitHub's REST API, for ancestry of GitHub sources without fetching them
//! (a nixpkgs history fetch is too large): `GET /repos/{owner}/{repo}/compare/{base}...{head}`.
//! Anonymous unless `SWL_GITHUB_TOKEN` is set; anonymous requests are
//! limited to 60 an hour per address.

use std::env;

use anyhow::{Context, Result, anyhow};
use serde_json::Value;

use crate::{
    ancestry::valid_github_name,
    forge::{Request, http},
};

/// Overrides the API base, `https://api.github.com`, e.g. for tests.
pub const API_ENV: &str = "SWL_GITHUB_API";
/// A GitHub token for the compare API; unset means anonymous.
pub const TOKEN_ENV: &str = "SWL_GITHUB_TOKEN";

const DEFAULT_API: &str = "https://api.github.com";

/// How `head` relates to `base` (the compare API's `status`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compare {
    /// `head` descends from `base`.
    Ahead,
    Identical,
    /// `head` is an ancestor of `base`.
    Behind,
    Diverged,
}

impl Compare {
    /// Whether `base` is in `head`'s history.
    pub fn contains_base(self) -> bool {
        matches!(self, Compare::Ahead | Compare::Identical)
    }
}

/// Whether the token may go to `base`: `https://api.github.com` itself, or a
/// test server on the loopback interface.
pub fn token_allowed(base: &str) -> bool {
    let base = base.trim_end_matches('/');
    if base == DEFAULT_API {
        return true;
    }
    let Some(rest) = base.strip_prefix("http://") else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = authority.rsplit_once(':').map_or(authority, |(h, _)| h);
    !authority.contains('@') && matches!(host, "127.0.0.1" | "localhost")
}

pub struct GitHub {
    base: String,
    token: Option<String>,
}

impl GitHub {
    pub fn from_env() -> Self {
        let base = env::var(API_ENV)
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| DEFAULT_API.to_string());
        let token = env::var(TOKEN_ENV).ok().filter(|t| !t.is_empty());
        Self::new(base, token)
    }

    pub fn new(base: String, token: Option<String>) -> Self {
        let token = token.filter(|_| {
            let ok = token_allowed(&base);
            if !ok {
                eprintln!("swl: warning: not sending {TOKEN_ENV} to {base}, which is not GitHub");
            }
            ok
        });
        Self { base, token }
    }

    fn get(&self, path: &str) -> Result<Value, String> {
        let url = format!("{}/{path}", self.base.trim_end_matches('/'));
        let (code, body) = http(&Request {
            method: "GET",
            url: &url,
            token: self.token.as_deref(),
            accept: "application/vnd.github+json",
            body: None,
            what: "GitHub API",
        })
        .map_err(|e| format!("{e:#}"))?;
        let message = || {
            serde_json::from_str::<Value>(&body)
                .ok()
                .and_then(|v| v.get("message")?.as_str().map(str::to_string))
                .unwrap_or_default()
        };
        match code {
            200 => serde_json::from_str(&body).map_err(|e| format!("GET {path}: not JSON: {e}")),
            403 | 429 if self.token.is_none() => Err(format!(
                "GitHub API: HTTP {code} ({}); anonymous requests are limited to 60 an hour, \
                 set {TOKEN_ENV} for more",
                message()
            )),
            404 => Err(format!(
                "GitHub API: GET {path}: not found (an unknown revision or repo)"
            )),
            _ => Err(format!(
                "GitHub API: GET {path}: HTTP {code} ({})",
                message()
            )),
        }
    }

    /// How `head` relates to `base` in `owner/repo`; `head` may be a branch.
    pub fn compare(
        &self,
        owner: &str,
        repo: &str,
        base: &str,
        head: &str,
    ) -> Result<Compare, String> {
        let v = self.get(&format!(
            "{}/compare/{}...{}?per_page=1",
            repo_path(owner, repo)?,
            encode(base),
            encode(head)
        ))?;
        parse_compare(&v).map_err(|e| format!("{e:#}"))
    }

    /// The default branch of `owner/repo`.
    pub fn default_branch(&self, owner: &str, repo: &str) -> Result<String, String> {
        let v = self.get(&repo_path(owner, repo)?)?;
        v.get("default_branch")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| format!("GitHub API: {owner}/{repo} has no default_branch"))
    }
}

/// `repos/{owner}/{repo}`, for names that cannot leave that path.
fn repo_path(owner: &str, repo: &str) -> Result<String, String> {
    if valid_github_name(owner) && valid_github_name(repo) {
        Ok(format!("repos/{owner}/{repo}"))
    } else {
        Err(format!(
            "{owner:?}/{repo:?} is not a GitHub repository name"
        ))
    }
}

/// A revision or branch name as a path segment: anything but letters,
/// digits and `._/-` percent-encoded.
fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"._/-".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

pub fn parse_compare(v: &Value) -> Result<Compare> {
    let status = v
        .get("status")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("the compare answer has no status"))
        .context("GitHub API")?;
    Ok(match status {
        "ahead" => Compare::Ahead,
        "identical" => Compare::Identical,
        "behind" => Compare::Behind,
        "diverged" => Compare::Diverged,
        other => return Err(anyhow!("GitHub API: unknown compare status {other:?}")),
    })
}

#[cfg(test)]
mod tests {
    use std::{
        io::{BufRead, BufReader, Write},
        net::TcpListener,
        sync::{Arc, Mutex},
        thread,
    };

    use super::*;

    #[test]
    fn compare_statuses() {
        let c = |s: &str| parse_compare(&serde_json::json!({ "status": s }));
        assert_eq!(c("ahead").unwrap(), Compare::Ahead);
        assert!(c("identical").unwrap().contains_base());
        assert!(!c("behind").unwrap().contains_base());
        assert!(!c("diverged").unwrap().contains_base());
        assert!(c("weird").is_err());
        assert!(parse_compare(&serde_json::json!({})).is_err());
    }

    #[test]
    fn the_token_goes_only_to_github_or_a_local_test_server() {
        assert!(token_allowed("https://api.github.com"));
        assert!(token_allowed("https://api.github.com/"));
        assert!(token_allowed("http://127.0.0.1:4711"));
        assert!(!token_allowed("http://api.github.com"));
        assert!(!token_allowed("https://api.github.com.example.org"));
        assert!(!token_allowed("http://127.0.0.1@evil.example/"));
        assert!(!token_allowed("https://example.org"));
    }

    /// A server that answers each request with the next `(status, body)`
    /// and records the request line and Authorization header.
    fn serve(answers: Vec<(&'static str, &'static str)>) -> (String, Arc<Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        thread::spawn(move || {
            for ((status, body), stream) in answers.into_iter().zip(listener.incoming()) {
                let mut stream = stream.unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut request = String::new();
                reader.read_line(&mut request).unwrap();
                let mut auth = String::from("-");
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                        break;
                    }
                    if line.to_ascii_lowercase().starts_with("authorization:") {
                        auth = line.trim().to_string();
                    }
                }
                log.lock()
                    .unwrap()
                    .push(format!("{} {auth}", request.trim()));
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
            }
        });
        (base, seen)
    }

    #[test]
    fn compare_and_rate_limits() {
        let (base, seen) = serve(vec![
            ("200 OK", r#"{"status":"ahead","ahead_by":3}"#),
            (
                "403 Forbidden",
                r#"{"message":"API rate limit exceeded for 192.0.2.1."}"#,
            ),
            ("404 Not Found", r#"{"message":"Not Found"}"#),
            ("200 OK", r#"{"default_branch":"master"}"#),
        ]);
        let gh = GitHub::new(base, None);
        assert_eq!(gh.compare("o", "r", "a", "b"), Ok(Compare::Ahead));
        let limited = gh.compare("o", "r", "a", "b").unwrap_err();
        assert!(limited.contains("limited to 60 an hour"), "{limited}");
        assert!(limited.contains("rate limit exceeded"), "{limited}");
        let missing = gh.compare("o", "r", "a", "b").unwrap_err();
        assert!(missing.contains("not found"), "{missing}");
        assert_eq!(gh.default_branch("o", "r").as_deref(), Ok("master"));
        let seen = seen.lock().unwrap();
        assert_eq!(
            seen[0], "GET /repos/o/r/compare/a...b?per_page=1 HTTP/1.1 -",
            "{seen:?}"
        );
        assert_eq!(seen[3], "GET /repos/o/r HTTP/1.1 -");
    }

    #[test]
    fn names_and_refs_stay_in_their_path_segments() {
        assert_eq!(encode("feature/x#1"), "feature/x%231");
        assert!(repo_path("o", "..").is_err());
        assert!(repo_path("o/p", "r").is_err());
        let gh = GitHub::new("http://127.0.0.1:9".into(), None);
        assert!(
            gh.compare("..", "r", "a", "b")
                .unwrap_err()
                .contains("not a GitHub")
        );
    }

    #[test]
    fn the_default_branch_through_remote_history() {
        use crate::ancestry::{History, RemoteHistory, Source};
        let (base, seen) = serve(vec![
            ("200 OK", r#"{"default_branch":"nixos-unstable"}"#),
            ("200 OK", r#"{"status":"behind"}"#),
        ]);
        let h = RemoteHistory {
            github: GitHub::new(base, None),
        };
        let src = Source::GitHub {
            owner: "NixOS".into(),
            repo: "nixpkgs".into(),
        };
        let rev = "a".repeat(40);
        assert_eq!(h.in_branch(&src, &rev, None), Ok(false));
        let seen = seen.lock().unwrap();
        assert_eq!(seen[0], "GET /repos/NixOS/nixpkgs HTTP/1.1 -");
        assert_eq!(
            seen[1],
            format!(
                "GET /repos/NixOS/nixpkgs/compare/{rev}...nixos-unstable?per_page=1 HTTP/1.1 -"
            )
        );
    }

    #[test]
    fn a_token_only_when_set() {
        let (base, seen) = serve(vec![("200 OK", r#"{"status":"identical"}"#)]);
        let gh = GitHub::new(base, Some("gh-secret".into()));
        assert_eq!(gh.compare("o", "r", "a", "a"), Ok(Compare::Identical));
        assert!(
            seen.lock().unwrap()[0].ends_with("Authorization: token gh-secret"),
            "{:?}",
            seen.lock().unwrap()
        );
        let gh = GitHub::new("https://example.org".into(), Some("gh-secret".into()));
        assert!(gh.token.is_none());
    }
}

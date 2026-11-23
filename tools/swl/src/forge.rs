//! The forge (Forgejo): MR state through the anonymous REST API, and MR
//! creation through the `fj` CLI, which holds the owner's login. swl itself
//! has no token configuration.

use std::{
    env,
    process::{Command, Stdio},
};

use anyhow::{Context, Result, anyhow, bail};
use serde_json::Value;

/// The curl binary: the package sets `SWL_CURL` at build time.
const CURL: &str = match option_env!("SWL_CURL") {
    Some(c) => c,
    None => "curl",
};

pub const FORGE_HOST: &str = "forgejo.lynx-lizard.ts.net";
pub const FORGE_OWNER: &str = "binarin";

/// Overrides the API base, `https://<host>/api/v1`, e.g. for tests.
pub const API_ENV: &str = "SWL_FORGE_API";
/// Overrides the git base that CI fetches `refs/pull/N/head` from,
/// `https://<host>`; the repo URL is `<base>/<owner>/<repo>`.
pub const GIT_ENV: &str = "SWL_FORGE_GIT";

pub fn api_base() -> String {
    env::var(API_ENV)
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| format!("https://{FORGE_HOST}/api/v1"))
}

pub fn git_base() -> String {
    env::var(GIT_ENV)
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| format!("https://{FORGE_HOST}"))
}

/// `owner/repo` on the forge.
pub fn slug(repo: &str) -> String {
    format!("{FORGE_OWNER}/{repo}")
}

/// The flake URL CI overrides an input with: the MR's head ref, pinned to
/// the head commit so that the whole job builds one revision.
pub fn pull_head_url(repo: &str, number: u64, head_sha: &str) -> String {
    format!(
        "git+{}/{}?ref=refs/pull/{number}/head&rev={head_sha}",
        git_base().trim_end_matches('/'),
        slug(repo)
    )
}

/// What the API says about an MR.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrInfo {
    pub number: u64,
    pub state: PrState,
    pub head_sha: String,
    pub title: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrState {
    Open,
    /// Merged; the commit that master got.
    Merged {
        merge_commit: String,
    },
    ClosedUnmerged,
}

impl PrState {
    pub fn describe(&self) -> String {
        match self {
            PrState::Open => "open".into(),
            PrState::Merged { merge_commit } => {
                format!("merged as {}", &merge_commit[..merge_commit.len().min(12)])
            }
            PrState::ClosedUnmerged => "closed without merging".into(),
        }
    }
}

impl PrInfo {
    /// Parses `GET /repos/{owner}/{repo}/pulls/{n}`.
    pub fn from_api(v: &Value) -> Result<Self> {
        let s = |p: &str| v.pointer(p).and_then(Value::as_str);
        let number = v
            .get("number")
            .and_then(Value::as_u64)
            .ok_or_else(|| anyhow!("no number in the pull request JSON"))?;
        let merged = v.get("merged").and_then(Value::as_bool).unwrap_or(false);
        let state = match (s("/state"), merged) {
            (_, true) => PrState::Merged {
                merge_commit: s("/merge_commit_sha")
                    .filter(|c| !c.is_empty())
                    .ok_or_else(|| anyhow!("MR {number} is merged but has no merge_commit_sha"))?
                    .to_string(),
            },
            (Some("open"), false) => PrState::Open,
            (Some("closed"), false) => PrState::ClosedUnmerged,
            (other, _) => bail!("MR {number} has an unknown state {other:?}"),
        };
        Ok(Self {
            number,
            state,
            head_sha: s("/head/sha")
                .ok_or_else(|| anyhow!("MR {number} has no head sha"))?
                .to_string(),
            title: s("/title").unwrap_or_default().to_string(),
        })
    }
}

/// What a 404 from the anonymous API means: Forgejo answers 404 for a
/// private repo too.
pub const NOT_FOUND: &str = "no such MR (or the repo is not public)";

/// Reads MR state. `Ok(None)` is an MR that does not exist; an error is a
/// forge that could not be asked.
pub trait PrReader {
    fn pr(&self, repo: &str, number: u64) -> Result<Option<PrInfo>>;
}

/// The anonymous Forgejo API, through curl. The manifest repos must be
/// public for this to work.
pub struct AnonApi {
    pub base: String,
}

impl AnonApi {
    pub fn from_env() -> Self {
        Self { base: api_base() }
    }

    /// `(HTTP status, body)` of a GET, or an error when the forge is
    /// unreachable.
    fn get(&self, path: &str) -> Result<(u16, String)> {
        let url = format!("{}/{path}", self.base.trim_end_matches('/'));
        let out = Command::new(CURL)
            .args([
                "--silent",
                "--show-error",
                "--location",
                "--max-time",
                "30",
                "--header",
                "Accept: application/json",
                "--write-out",
                "\n%{http_code}",
                &url,
            ])
            .stdin(Stdio::null())
            .output()
            .with_context(|| format!("running {CURL}"))?;
        if !out.status.success() {
            bail!(
                "forge API unreachable: GET {url}: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        let text = String::from_utf8_lossy(&out.stdout);
        let (body, code) = text.rsplit_once('\n').unwrap_or(("", &text));
        let code = code
            .trim()
            .parse()
            .with_context(|| format!("GET {url}: no HTTP status"))?;
        Ok((code, body.to_string()))
    }
}

impl PrReader for AnonApi {
    fn pr(&self, repo: &str, number: u64) -> Result<Option<PrInfo>> {
        let path = format!("repos/{}/pulls/{number}", slug(repo));
        let (code, body) = self.get(&path)?;
        match code {
            200 => {
                let v: Value =
                    serde_json::from_str(&body).with_context(|| format!("GET {path}: not JSON"))?;
                PrInfo::from_api(&v).map(Some)
            }
            404 => Ok(None),
            _ => bail!("GET {path}: HTTP {code} (the repo must be public; see the swl README)"),
        }
    }
}

/// Finds and opens MRs for `swl publish`.
pub trait PrPublisher {
    /// The open MR whose head is `branch`, if any.
    fn find_open(&self, repo: &str, branch: &str) -> Result<Option<u64>>;
    /// Opens an MR and returns its number. A `WIP: ` title prefix makes it a
    /// draft on Forgejo.
    fn create(&self, repo: &str, base: &str, head: &str, title: &str, body: &str) -> Result<u64>;
}

/// The `fj` CLI from PATH, logged in to the forge. Its output is text, so
/// this parses fj 0.7's English messages.
pub struct Fj;

impl Fj {
    fn run(&self, args: &[&str]) -> Result<String> {
        let out = Command::new("fj")
            .arg("-H")
            .arg(FORGE_HOST)
            .args(args)
            .stdin(Stdio::null())
            .output()
            .context("running fj (the Forgejo CLI) from PATH")?;
        if !out.status.success() {
            bail!(
                "fj {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }
}

/// MR numbers in `fj pr search` output: lines `#N: title (by user)`.
pub fn parse_fj_search(text: &str) -> Vec<u64> {
    text.lines()
        .filter_map(|l| l.trim().strip_prefix('#')?.split_once(':')?.0.parse().ok())
        .collect()
}

/// The head branch in `fj pr view` output: the line ``From `head` into `base` ``.
pub fn parse_fj_view_head(text: &str) -> Option<String> {
    text.lines().find_map(|l| {
        let rest = l.trim().strip_prefix("From `")?;
        let (head, rest) = rest.split_once('`')?;
        rest.trim_start()
            .starts_with("into ")
            .then(|| head.to_string())
    })
}

/// The number in `fj pr create` output: `created pull request #N: title`.
pub fn parse_fj_created(text: &str) -> Option<u64> {
    let rest = text.split("pull request #").nth(1)?;
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

impl PrPublisher for Fj {
    fn find_open(&self, repo: &str, branch: &str) -> Result<Option<u64>> {
        let slug = slug(repo);
        // `--all`: fj shows 25 per page otherwise.
        let found = self.run(&["pr", "search", "--repo", &slug, "--state", "open", "--all"])?;
        for n in parse_fj_search(&found) {
            let view = self.run(&["pr", "view", &format!("{slug}#{n}")])?;
            if parse_fj_view_head(&view).as_deref() == Some(branch) {
                return Ok(Some(n));
            }
        }
        Ok(None)
    }

    fn create(&self, repo: &str, base: &str, head: &str, title: &str, body: &str) -> Result<u64> {
        let dir = crate::tempdir::TempDir::new("swl-publish")?;
        let body_file = dir.path().join("body.md");
        std::fs::write(&body_file, body)?;
        let out = self.run(&[
            "pr",
            "create",
            "--repo",
            &slug(repo),
            "--base",
            base,
            "--head",
            head,
            "--body-file",
            &body_file.to_string_lossy(),
            title,
        ])?;
        match parse_fj_created(&out) {
            Some(n) => Ok(n),
            None => self.find_open(repo, head)?.ok_or_else(|| {
                anyhow!("fj pr create printed no MR number and none is found for {head}: {out}")
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn api(json: &str) -> Result<PrInfo> {
        PrInfo::from_api(&serde_json::from_str(json).unwrap())
    }

    #[test]
    fn api_states() {
        let open = api(r#"{"number":24,"state":"open","merged":false,"title":"t","head":{"sha":"abc","ref":"b"},"merge_commit_sha":null}"#).unwrap();
        assert_eq!(open.state, PrState::Open);
        assert_eq!(open.head_sha, "abc");
        let merged = api(r#"{"number":23,"state":"closed","merged":true,"head":{"sha":"abc","ref":"refs/pull/23/head"},"merge_commit_sha":"def"}"#).unwrap();
        assert_eq!(
            merged.state,
            PrState::Merged {
                merge_commit: "def".into()
            }
        );
        let closed = api(
            r#"{"number":2,"state":"closed","merged":false,"head":{"sha":"abc"},"merge_commit_sha":null}"#,
        )
        .unwrap();
        assert_eq!(closed.state, PrState::ClosedUnmerged);
        assert!(
            api(r#"{"number":2,"state":"closed","merged":true,"head":{"sha":"abc"}}"#).is_err()
        );
        assert!(api(r#"{"number":2,"state":"weird","merged":false,"head":{"sha":"a"}}"#).is_err());
        assert!(api(r#"{"state":"open"}"#).is_err());
    }

    #[test]
    fn fj_output() {
        assert_eq!(
            parse_fj_search(
                "2 pull requests\n#41: devshell: watch (SL-0055) (by u)\n#7: x: y (by u)\n"
            ),
            [41, 7]
        );
        assert!(parse_fj_search("Page 1 out of 0 pages in total (0 pull requests)\n").is_empty());
        let view =
            "swl v0: x #24\nBy u — Open — +1 -0\nFrom `sl-0036-swl` into `master`\n\n> body\n";
        assert_eq!(parse_fj_view_head(view).as_deref(), Some("sl-0036-swl"));
        assert_eq!(parse_fj_view_head("From me\n"), None);
        assert_eq!(
            parse_fj_created("created pull request #25: WIP: swl: x\n"),
            Some(25)
        );
        assert_eq!(parse_fj_created("nothing"), None);
    }

    #[test]
    fn pull_head_urls() {
        // The default applies only without SWL_FORGE_GIT.
        if env::var_os(GIT_ENV).is_none() {
            assert_eq!(
                pull_head_url("swan-lake-common", 24, "72f1eec"),
                "git+https://forgejo.lynx-lizard.ts.net/binarin/swan-lake-common?ref=refs/pull/24/head&rev=72f1eec"
            );
        }
    }
}

//! The forge (Forgejo): MR state through the anonymous REST API; swl has no
//! token configuration.

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

pub fn api_base() -> String {
    env::var(API_ENV)
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| format!("https://{FORGE_HOST}/api/v1"))
}

/// `owner/repo` on the forge.
pub fn slug(repo: &str) -> String {
    format!("{FORGE_OWNER}/{repo}")
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
}

//! The forge (Forgejo): MR state and commit status through the REST API,
//! anonymously or, for `status` and `start`, read-only with the login of the
//! `fj` CLI; and MR creation through `fj`. swl itself has no token
//! configuration.

use std::{
    env,
    io::Write,
    path::PathBuf,
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
    /// The head branch while the MR is open; `refs/pull/N/head` once its
    /// branch is gone.
    pub head_ref: String,
    pub title: String,
    /// A draft: `draft` in the API, or a `WIP:` title.
    pub draft: bool,
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
        let title = s("/title").unwrap_or_default().to_string();
        let draft = v.get("draft").and_then(Value::as_bool).unwrap_or(false)
            || title.to_ascii_uppercase().starts_with("WIP:");
        Ok(Self {
            number,
            state,
            head_sha: s("/head/sha")
                .ok_or_else(|| anyhow!("MR {number} has no head sha"))?
                .to_string(),
            head_ref: s("/head/ref").unwrap_or_default().to_string(),
            title,
            draft,
        })
    }
}

/// The combined commit status of a revision, as CI reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CiState {
    /// No status reported (yet).
    None,
    Pending,
    /// `failure`, `error` or another state that is not a success.
    Failing(String),
    Green,
}

impl CiState {
    /// Parses `GET /repos/{owner}/{repo}/commits/{sha}/status`.
    pub fn from_api(v: &Value) -> Self {
        let state = v.get("state").and_then(Value::as_str).unwrap_or("");
        let count = v.get("total_count").and_then(Value::as_u64).unwrap_or(0);
        match state {
            _ if count == 0 => CiState::None,
            "" | "pending" => CiState::Pending,
            "success" => CiState::Green,
            other => CiState::Failing(other.to_string()),
        }
    }
}

/// What a 404 from the anonymous API means: Forgejo answers 404 for a
/// private repo too.
pub const NOT_FOUND: &str = "no such MR (or the repo is not public)";

/// Reads MR state and commit status. `Ok(None)` is a 404: no such MR or
/// revision, or a repo this reader may not see; an error is a forge that
/// could not be asked.
pub trait PrReader {
    fn pr(&self, repo: &str, number: u64) -> Result<Option<PrInfo>>;
    fn commit_status(&self, repo: &str, sha: &str) -> Result<Option<CiState>>;
    /// What a 404 means for this reader.
    fn not_found(&self) -> &'static str {
        NOT_FOUND
    }
}

/// `(HTTP status, body)` of a GET of `<base>/<path>` through curl, or an
/// error when the forge is unreachable. A token goes to curl on stdin, never
/// on its command line.
fn api_get(base: &str, path: &str, token: Option<&str>) -> Result<(u16, String)> {
    let url = format!("{}/{path}", base.trim_end_matches('/'));
    let mut cmd = Command::new(CURL);
    cmd.args([
        "--silent",
        "--show-error",
        "--location",
        "--max-time",
        "30",
        "--header",
        "Accept: application/json",
        "--write-out",
        "\n%{http_code}",
    ]);
    if token.is_some() {
        cmd.args(["--config", "-"]);
    }
    let mut child = cmd
        .arg(&url)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("running {CURL}"))?;
    let mut stdin = child.stdin.take().expect("piped");
    if let Some(token) = token {
        let token = token.replace('\\', "\\\\").replace('"', "\\\"");
        writeln!(stdin, "header = \"Authorization: token {token}\"")
            .context("passing the token to curl")?;
    }
    drop(stdin);
    let out = child
        .wait_with_output()
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

/// The reads over [`api_get`]; `hint` explains other HTTP statuses.
fn read_pr(
    base: &str,
    token: Option<&str>,
    repo: &str,
    number: u64,
    hint: &str,
) -> Result<Option<PrInfo>> {
    let path = format!("repos/{}/pulls/{number}", slug(repo));
    match api_get(base, &path, token)? {
        (200, body) => {
            let v: Value =
                serde_json::from_str(&body).with_context(|| format!("GET {path}: not JSON"))?;
            PrInfo::from_api(&v).map(Some)
        }
        (404, _) => Ok(None),
        (code, _) => bail!("GET {path}: HTTP {code} ({hint})"),
    }
}

fn read_commit_status(
    base: &str,
    token: Option<&str>,
    repo: &str,
    sha: &str,
    hint: &str,
) -> Result<Option<CiState>> {
    let path = format!("repos/{}/commits/{sha}/status", slug(repo));
    match api_get(base, &path, token)? {
        (200, body) => {
            let v: Value =
                serde_json::from_str(&body).with_context(|| format!("GET {path}: not JSON"))?;
            Ok(Some(CiState::from_api(&v)))
        }
        (404, _) => Ok(None),
        (code, _) => bail!("GET {path}: HTTP {code} ({hint})"),
    }
}

/// The anonymous Forgejo API, through curl. The manifest repos must be
/// public for this to work.
pub struct AnonApi {
    pub base: String,
    /// What a 404 means, see [`PrReader::not_found`].
    pub not_found: &'static str,
}

impl AnonApi {
    pub fn from_env() -> Self {
        Self {
            base: api_base(),
            not_found: NOT_FOUND,
        }
    }

    /// For `status` and `start`, which would have used a login.
    pub fn without_login() -> Self {
        Self {
            not_found: NOT_FOUND_WITHOUT_LOGIN,
            ..Self::from_env()
        }
    }
}

const ANON_HINT: &str = "the repo must be public; see the swl README";

impl PrReader for AnonApi {
    fn pr(&self, repo: &str, number: u64) -> Result<Option<PrInfo>> {
        read_pr(&self.base, None, repo, number, ANON_HINT)
    }

    fn commit_status(&self, repo: &str, sha: &str) -> Result<Option<CiState>> {
        read_commit_status(&self.base, None, repo, sha, ANON_HINT)
    }

    fn not_found(&self) -> &'static str {
        self.not_found
    }
}

/// Overrides the path of fj's `keys.json`, e.g. for tests.
pub const FJ_KEYS_ENV: &str = "SWL_FJ_KEYS";

/// fj's logins: `$SWL_FJ_KEYS`, else `$XDG_DATA_HOME/forgejo-cli/keys.json`
/// (`~/.local/share/forgejo-cli/keys.json`).
pub fn fj_keys_path() -> Option<PathBuf> {
    let var = |k: &str| env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
    if let Some(p) = var(FJ_KEYS_ENV) {
        return Some(p);
    }
    let data = var("XDG_DATA_HOME").or_else(|| var("HOME").map(|h| h.join(".local/share")))?;
    Some(data.join("forgejo-cli/keys.json"))
}

/// The token of fj's login to the forge host in a `keys.json`, if any.
pub fn fj_token(keys_json: &str) -> Result<Option<String>> {
    let v: Value = serde_json::from_str(keys_json).context("fj's keys.json is not JSON")?;
    Ok(v.pointer(&format!("/hosts/{FORGE_HOST}/token"))
        .and_then(Value::as_str)
        .filter(|t| !t.is_empty())
        .map(str::to_string))
}

/// Whether the API base may receive the token: the forge itself, or a test
/// server on the loopback interface.
pub fn token_allowed(base: &str) -> bool {
    let rest = base
        .strip_prefix("https://")
        .or_else(|| base.strip_prefix("http://"))
        .unwrap_or("");
    let authority = rest.split('/').next().unwrap_or("");
    let host = authority.rsplit_once(':').map_or(authority, |(h, _)| h);
    (base.starts_with("https://") && host == FORGE_HOST)
        || matches!(host, "127.0.0.1" | "localhost")
}

/// The Forgejo API with fj's login, for reads only: MR state and commit
/// status of private repos too. The token never reaches a command line or
/// the output.
pub struct AuthApi {
    base: String,
    token: String,
}

const AUTH_HINT: &str = "is the fj login still valid? `fj auth login`";

impl AuthApi {
    /// fj's login to the forge host, or `None` without one.
    pub fn from_fj_login() -> Result<Option<Self>> {
        let Some(path) = fj_keys_path() else {
            return Ok(None);
        };
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        let base = api_base();
        let token = fj_token(&text).with_context(|| path.display().to_string())?;
        match token {
            Some(token) if token_allowed(&base) => Ok(Some(Self { base, token })),
            Some(_) => bail!("not sending fj's token to {base}, which is not the forge"),
            None => Ok(None),
        }
    }
}

impl PrReader for AuthApi {
    fn pr(&self, repo: &str, number: u64) -> Result<Option<PrInfo>> {
        read_pr(&self.base, Some(&self.token), repo, number, AUTH_HINT)
    }

    fn commit_status(&self, repo: &str, sha: &str) -> Result<Option<CiState>> {
        read_commit_status(&self.base, Some(&self.token), repo, sha, AUTH_HINT)
    }

    fn not_found(&self) -> &'static str {
        "no such MR"
    }
}

/// What a 404 from the anonymous reader means to `status` and `start`.
pub const NOT_FOUND_WITHOUT_LOGIN: &str =
    "not readable anonymously, and no fj login: no such MR, or the repo is private";

/// The reader for `status` and `start`: fj's login when there is one, else
/// the anonymous API. A login that cannot be used is a warning.
pub fn interactive_reader() -> Box<dyn PrReader> {
    match AuthApi::from_fj_login() {
        Ok(Some(api)) => Box::new(api),
        Ok(None) => Box::new(AnonApi::without_login()),
        Err(e) => {
            eprintln!("swl: warning: not using fj's login: {e:#}");
            Box::new(AnonApi::without_login())
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
    fn api_head_ref_and_draft() {
        let open = api(r#"{"number":24,"state":"open","merged":false,"title":"t","draft":false,"head":{"sha":"abc","ref":"sl-1-x"}}"#).unwrap();
        assert_eq!(open.head_ref, "sl-1-x");
        assert!(!open.draft);
        let wip = api(r#"{"number":24,"state":"open","merged":false,"title":"WIP: t","head":{"sha":"abc","ref":"b"}}"#).unwrap();
        assert!(wip.draft);
        let draft = api(r#"{"number":24,"state":"open","merged":false,"title":"t","draft":true,"head":{"sha":"abc","ref":"b"}}"#).unwrap();
        assert!(draft.draft);
    }

    #[test]
    fn combined_commit_status() {
        let ci = |json: &str| CiState::from_api(&serde_json::from_str(json).unwrap());
        assert_eq!(
            ci(r#"{"state":"","sha":"","total_count":0,"statuses":null}"#),
            CiState::None
        );
        assert_eq!(
            ci(r#"{"state":"pending","total_count":2}"#),
            CiState::Pending
        );
        assert_eq!(ci(r#"{"state":"success","total_count":4}"#), CiState::Green);
        assert_eq!(
            ci(r#"{"state":"failure","total_count":4}"#),
            CiState::Failing("failure".into())
        );
    }

    #[test]
    fn fj_login_token() {
        let keys = r#"{"hosts":{"forgejo.lynx-lizard.ts.net":{"type":"Application","name":"u","token":"t0k"}},"aliases":{},"default_ssh":[]}"#;
        assert_eq!(fj_token(keys).unwrap().as_deref(), Some("t0k"));
        assert_eq!(
            fj_token(r#"{"hosts":{"codeberg.org":{"token":"x"}}}"#).unwrap(),
            None
        );
        assert_eq!(fj_token(r#"{"hosts":{}}"#).unwrap(), None);
        assert!(fj_token("not json").is_err());
    }

    #[test]
    fn the_token_goes_only_to_the_forge_or_a_local_test_server() {
        assert!(token_allowed("https://forgejo.lynx-lizard.ts.net/api/v1"));
        assert!(token_allowed("http://127.0.0.1:4711/api/v1"));
        assert!(token_allowed("http://localhost/api/v1"));
        assert!(!token_allowed("http://forgejo.lynx-lizard.ts.net/api/v1"));
        assert!(!token_allowed("https://example.org/api/v1"));
        assert!(!token_allowed(
            "https://forgejo.lynx-lizard.ts.net.example.org/api/v1"
        ));
        assert!(!token_allowed("https://127.0.0.1@example.org/api/v1"));
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

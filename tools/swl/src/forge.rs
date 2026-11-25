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
/// on its command line, and a request with it follows no redirect.
fn api_get(base: &str, path: &str, token: Option<&str>) -> Result<(u16, String)> {
    let url = format!("{}/{path}", base.trim_end_matches('/'));
    let mut cmd = Command::new(CURL);
    cmd.args([
        "--silent",
        "--show-error",
        "--max-time",
        "30",
        "--header",
        "Accept: application/json",
        "--write-out",
        "\n%{http_code}",
    ]);
    match token {
        Some(_) => cmd.args(["--config", "-"]),
        None => cmd.arg("--location"),
    };
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

/// What a 401 or 403 to a request with fj's token means.
pub const LOGIN_REJECTED: &str = "fj login rejected: run fj auth login";

/// The error for an HTTP status other than 200 and 404; `hint` explains it.
fn http_error(path: &str, code: u16, token: Option<&str>, hint: &str) -> anyhow::Error {
    match (token, code) {
        (Some(_), 401 | 403) => anyhow!(LOGIN_REJECTED),
        _ => anyhow!("GET {path}: HTTP {code} ({hint})"),
    }
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
        (code, _) => Err(http_error(&path, code, token, hint)),
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
        (code, _) => Err(http_error(&path, code, token, hint)),
    }
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
}

const ANON_HINT: &str = "the repo must be public; see the swl README";

impl PrReader for AnonApi {
    fn pr(&self, repo: &str, number: u64) -> Result<Option<PrInfo>> {
        read_pr(&self.base, None, repo, number, ANON_HINT)
    }

    fn commit_status(&self, repo: &str, sha: &str) -> Result<Option<CiState>> {
        read_commit_status(&self.base, None, repo, sha, ANON_HINT)
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

/// fj's login to the forge host in a `keys.json`.
#[derive(Debug, PartialEq, Eq)]
pub enum FjLogin {
    None,
    Token(String),
    /// An OAuth login whose `expires_at` has passed.
    Expired,
}

/// Days since 1970-01-01 of a proleptic Gregorian date.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// Unix seconds of an `expires_at`: the `time` crate's default form
/// `[year, ordinal, hour, minute, second, nanosecond, offset h, m, s]` that
/// fj writes, Unix seconds, or RFC 3339. `None` for anything else.
pub fn expiry_seconds(v: &Value) -> Option<i64> {
    match v {
        Value::Number(n) => n.as_i64(),
        Value::Array(a) => {
            let n: Vec<i64> = a.iter().map(Value::as_i64).collect::<Option<_>>()?;
            let (year, ordinal, h, m, sec) =
                (n.first()?, n.get(1)?, n.get(2)?, n.get(3)?, n.get(4)?);
            let offset = n.get(6).unwrap_or(&0) * 3600
                + n.get(7).unwrap_or(&0) * 60
                + n.get(8).unwrap_or(&0);
            let days = days_from_civil(*year, 1, 1) + ordinal - 1;
            Some(days * 86400 + h * 3600 + m * 60 + sec - offset)
        }
        Value::String(s) => {
            let num = |r: std::ops::Range<usize>| s.get(r)?.parse::<i64>().ok();
            let days = days_from_civil(num(0..4)?, num(5..7)?, num(8..10)?);
            let secs = days * 86400 + num(11..13)? * 3600 + num(14..16)? * 60 + num(17..19)?;
            let rest = s
                .get(19..)?
                .trim_start_matches(|c: char| c == '.' || c.is_ascii_digit());
            let offset = match rest {
                "Z" | "z" => 0,
                _ => {
                    let sign = match rest.get(..1)? {
                        "+" => 1,
                        "-" => -1,
                        _ => return None,
                    };
                    let oh: i64 = rest.get(1..3)?.parse().ok()?;
                    let om: i64 = rest.get(4..6)?.parse().ok()?;
                    sign * (oh * 3600 + om * 60)
                }
            };
            Some(secs - offset)
        }
        _ => None,
    }
}

/// The login to the forge host in fj's `keys.json`, judged at Unix time
/// `now`. An `expires_at` that cannot be read is taken as not expired: a
/// rejected token is reported when it is used.
pub fn fj_login(keys_json: &str, now: i64) -> Result<FjLogin> {
    let v: Value = serde_json::from_str(keys_json).context("fj's keys.json is not JSON")?;
    let Some(host) = v.pointer(&format!("/hosts/{FORGE_HOST}")) else {
        return Ok(FjLogin::None);
    };
    let Some(token) = host
        .get("token")
        .and_then(Value::as_str)
        .filter(|t| !t.is_empty())
    else {
        return Ok(FjLogin::None);
    };
    if host
        .get("expires_at")
        .and_then(expiry_seconds)
        .is_some_and(|t| t <= now)
    {
        return Ok(FjLogin::Expired);
    }
    Ok(FjLogin::Token(token.to_string()))
}

/// Whether the API base may receive the token: `https://` to the forge
/// itself, or a test server on the loopback interface. An authority with
/// user info (`@`) is refused.
pub fn token_allowed(base: &str) -> bool {
    let (https, rest) = match (base.strip_prefix("https://"), base.strip_prefix("http://")) {
        (Some(rest), _) => (true, rest),
        (None, Some(rest)) => (false, rest),
        (None, None) => return false,
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    if authority.contains('@') {
        return false;
    }
    let host = authority.rsplit_once(':').map_or(authority, |(h, _)| h);
    (https && host == FORGE_HOST) || matches!(host, "127.0.0.1" | "localhost")
}

/// The reader for `status` and `start`: the anonymous API first, so that
/// public repos never see the token; on a 404 again with fj's login, when
/// there is one, so that private repos (the consumers) are readable too.
/// Reads only. The token never reaches a command line or the output.
pub struct ForgeReader {
    anon: AnonApi,
    token: Option<String>,
}

/// What a 404 means without an fj login.
pub const NOT_FOUND_WITHOUT_LOGIN: &str =
    "not readable anonymously, and no fj login: no such MR, or the repo is private";

const AUTH_HINT: &str = "with fj's login";

impl ForgeReader {
    /// With fj's login to the forge host, if there is a usable one; a login
    /// that cannot be used is a warning.
    pub fn from_env() -> Self {
        let anon = AnonApi::from_env();
        let token = match Self::fj_token(&anon.base) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("swl: warning: not using fj's login: {e:#}");
                None
            }
        };
        Self { anon, token }
    }

    fn fj_token(base: &str) -> Result<Option<String>> {
        let Some(path) = fj_keys_path() else {
            return Ok(None);
        };
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as i64);
        match fj_login(&text, now).with_context(|| path.display().to_string())? {
            FjLogin::None => Ok(None),
            FjLogin::Expired => {
                bail!("it has expired; run fj auth login")
            }
            FjLogin::Token(_) if !token_allowed(base) => {
                bail!("not sending fj's token to {base}, which is not the forge")
            }
            FjLogin::Token(t) => Ok(Some(t)),
        }
    }

    /// `anon`, then on a 404 `authed` with the token.
    fn read<T>(
        &self,
        anon: impl Fn() -> Result<Option<T>>,
        authed: impl Fn(&str) -> Result<Option<T>>,
    ) -> Result<Option<T>> {
        match (anon()?, &self.token) {
            (None, Some(token)) => authed(token),
            (found, _) => Ok(found),
        }
    }
}

impl PrReader for ForgeReader {
    fn pr(&self, repo: &str, number: u64) -> Result<Option<PrInfo>> {
        self.read(
            || self.anon.pr(repo, number),
            |t| read_pr(&self.anon.base, Some(t), repo, number, AUTH_HINT),
        )
    }

    fn commit_status(&self, repo: &str, sha: &str) -> Result<Option<CiState>> {
        self.read(
            || self.anon.commit_status(repo, sha),
            |t| read_commit_status(&self.anon.base, Some(t), repo, sha, AUTH_HINT),
        )
    }

    fn not_found(&self) -> &'static str {
        match self.token {
            Some(_) => "no such MR",
            None => NOT_FOUND_WITHOUT_LOGIN,
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
        assert_eq!(fj_login(keys, 0).unwrap(), FjLogin::Token("t0k".into()));
        assert_eq!(
            fj_login(r#"{"hosts":{"codeberg.org":{"token":"x"}}}"#, 0).unwrap(),
            FjLogin::None
        );
        assert_eq!(fj_login(r#"{"hosts":{}}"#, 0).unwrap(), FjLogin::None);
        assert!(fj_login("not json", 0).is_err());
    }

    #[test]
    fn an_expired_oauth_login_is_no_login() {
        // 2026-11-25T08:00:00Z, as the time crate writes it, and as text.
        let at = 1_795_593_600;
        for expires in [
            "[2026,329,8,0,0,0,0,0,0]",
            "1795593600",
            "\"2026-11-25T08:00:00Z\"",
            "\"2026-11-25T09:00:00.5+01:00\"",
        ] {
            let keys = format!(
                r#"{{"hosts":{{"forgejo.lynx-lizard.ts.net":{{"type":"OAuth","name":"u","token":"t","refresh_token":"r","expires_at":{expires}}}}}}}"#
            );
            assert_eq!(
                expiry_seconds(&serde_json::from_str(expires).unwrap()),
                Some(at),
                "{expires}"
            );
            assert_eq!(
                fj_login(&keys, at - 1).unwrap(),
                FjLogin::Token("t".into()),
                "{expires}"
            );
            assert_eq!(fj_login(&keys, at).unwrap(), FjLogin::Expired, "{expires}");
        }
        let unreadable =
            r#"{"hosts":{"forgejo.lynx-lizard.ts.net":{"token":"t","expires_at":"soon"}}}"#;
        assert_eq!(
            fj_login(unreadable, i64::MAX).unwrap(),
            FjLogin::Token("t".into())
        );
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
        assert!(!token_allowed(
            "https://forgejo.lynx-lizard.ts.net:443@evil.example/api/v1"
        ));
        assert!(!token_allowed("http://127.0.0.1:1@evil.example/"));
        // `?` ends the authority: this host is 127.0.0.1, as for curl.
        assert!(token_allowed("http://127.0.0.1?@evil.example/"));
        assert!(!token_allowed("ftp://127.0.0.1/"));
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

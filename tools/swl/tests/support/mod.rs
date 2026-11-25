//! A disposable forge for the workspace tests (`ws add`, `start`, `status`):
//! bare "forge" repos reached by `git+file:`, a bare source per repo whose
//! `origin` is the forge repo (as in `repos.toml`), and a fake forge API (a
//! local HTTP server) whose private repos answer only with a token. Offline,
//! with real git and Nix; set SWL_TEST_NIX_STORE to use a store in a scratch
//! directory, as the package check does inside the build sandbox.

#![allow(dead_code)]

use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    env, fs,
    io::{BufRead, BufReader, Read, Write},
    net::TcpListener,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::{Arc, Mutex},
    thread,
};

/// Common pins zwanenzang at its default branch, like the real common.
pub const COMMON_FLAKE: &str = r#"{
  inputs.zwanenzang.url = "git+file://@FORGE@/binarin/zwanenzang";
  outputs = { self, zwanenzang }: {
    value = builtins.readFile ./value.txt;
    zw = zwanenzang.value;
  };
}
"#;

pub const ZW_FLAKE: &str = r#"{
  outputs = { self }: {
    value = builtins.readFile ./value.txt;
  };
}
"#;

/// The consumer pins common's promotion branch `current`, like
/// `swan-lake-current`, as `lib`.
pub const CONSUMER_FLAKE: &str = r#"{
  inputs.lib.url = "git+file://@FORGE@/binarin/swan-lake-common?ref=current";
  outputs = { self, lib, ... }: {
    value = builtins.readFile ./value.txt + "+" + lib.value;
  };
}
"#;

/// The token the fake API wants for private repos.
pub const TOKEN: &str = "test-token-0123";

/// The path and Authorization header of each request.
type SeenAuth = Arc<Mutex<Vec<(String, Option<String>)>>>;

/// One request to the fake API.
#[derive(Clone, Debug)]
pub struct Seen {
    pub method: String,
    pub path: String,
    pub auth: Option<String>,
    pub body: String,
}

/// What the fake answers to the next merge request of an MR. Without one
/// programmed it answers as Forgejo 17 does: a request with
/// `merge_when_checks_succeed` is scheduled ([`MergeReply::Schedule`]), also
/// when the checks are green already; one without merges now, as Forgejo's
/// merge does, which deletes a pending schedule.
#[derive(Clone, Debug)]
pub enum MergeReply {
    /// Merges now, as Forgejo's fast-forward-only merge does: the base
    /// branch moves to the head, the head branch is deleted, the MR is
    /// merged. 409 when the head is not the asked `head_commit_id`, or not a
    /// fast-forward of master.
    MergeNow,
    /// 201: scheduled to merge when checks succeed (409 "already scheduled"
    /// when it is). As on Forgejo, only a later status event
    /// ([`FakeApi::set_status`] with `success`) merges it; checks green
    /// already leave it scheduled.
    Schedule,
    /// Scheduled as [`MergeReply::Schedule`], while the head's checks turn
    /// green without a status event that would merge it: the race of CI
    /// finishing as the merge is scheduled.
    ScheduleAsChecksTurnGreen,
    /// Any other answer.
    Status(u16, String),
}

/// GET responses by path; anything else is 404. Paths under a private repo
/// answer only with `Authorization: token TOKEN`, as Forgejo does; any other
/// token is a 401. Writes (`PATCH .../pulls/N` for the title, `POST
/// .../pulls` to open an MR, `POST .../pulls/N/merge`) need the token, and
/// merges answer as programmed with [`FakeApi::on_merge`].
#[derive(Clone, Default)]
pub struct FakeApi {
    pub port: u16,
    pub responses: Arc<Mutex<BTreeMap<String, String>>>,
    pub private: Arc<Mutex<BTreeSet<String>>>,
    pub seen_auth: SeenAuth,
    pub requests: Arc<Mutex<Vec<Seen>>>,
    pub merge_replies: Arc<Mutex<BTreeMap<String, VecDeque<MergeReply>>>>,
    /// The MRs (API paths) whose merge is scheduled for when checks succeed.
    pub scheduled: Arc<Mutex<BTreeSet<String>>>,
    /// The forge's bare repos, `<forge>/binarin/<repo>`, for merges and,
    /// with `follow_pull_refs`, the heads of open MRs.
    pub forge: Arc<Mutex<Option<PathBuf>>>,
    pub follow_pull_refs: Arc<Mutex<bool>>,
}

fn git_in(dir: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

impl FakeApi {
    pub fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let api = Self {
            port: listener.local_addr().unwrap().port(),
            ..Default::default()
        };
        let server = api.clone();
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut request = String::new();
                reader.read_line(&mut request).ok();
                let mut auth = None;
                let mut length = 0;
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                        break;
                    }
                    if let Some((k, v)) = line.split_once(':') {
                        if k.eq_ignore_ascii_case("authorization") {
                            auth = Some(v.trim().to_string());
                        } else if k.eq_ignore_ascii_case("content-length") {
                            length = v.trim().parse().unwrap_or(0);
                        }
                    }
                }
                let mut body = vec![0; length];
                reader.read_exact(&mut body).ok();
                let body = String::from_utf8_lossy(&body).into_owned();
                let mut words = request.split_whitespace();
                let method = words.next().unwrap_or("").to_string();
                let path = words.next().unwrap_or("").to_string();
                server
                    .seen_auth
                    .lock()
                    .unwrap()
                    .push((path.clone(), auth.clone()));
                server.requests.lock().unwrap().push(Seen {
                    method: method.clone(),
                    path: path.clone(),
                    auth: auth.clone(),
                    body: body.clone(),
                });
                let (status, body) = server.answer(&method, &path, auth.as_deref(), &body);
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
            }
        });
        api
    }

    fn answer(&self, method: &str, path: &str, auth: Option<&str>, body: &str) -> (String, String) {
        let repo = path
            .strip_prefix("/api/v1/repos/binarin/")
            .and_then(|r| r.split('/').next())
            .unwrap_or("")
            .to_string();
        let good = auth == Some(&format!("token {TOKEN}"));
        let status = |code: u16, text: &str| (code.to_string() + " " + text, String::new());
        if auth.is_some() && !good {
            return (
                "401 Unauthorized".into(),
                r#"{"message":"invalid token"}"#.into(),
            );
        }
        if method != "GET" {
            if !good {
                return status(401, "Unauthorized");
            }
            return self.write(method, path, &repo, body);
        }
        let allowed = !self.private.lock().unwrap().contains(&repo) || good;
        // Like Forgejo, a commit without statuses has an empty
        // combined status, not a 404.
        let no_status = path
            .ends_with("/status")
            .then(|| r#"{"state":"","sha":"","total_count":0,"statuses":null}"#.to_string());
        if let Some(query) = path
            .strip_prefix(&format!("/api/v1/repos/binarin/{repo}/pulls?"))
            .filter(|_| allowed)
        {
            return ("200 OK".into(), self.list_prs(&repo, query));
        }
        let found = allowed
            .then(|| {
                self.responses
                    .lock()
                    .unwrap()
                    .get(path)
                    .cloned()
                    .map(|b| self.live(path, &repo, b))
                    .or(no_status)
            })
            .flatten();
        match found {
            Some(b) => ("200 OK".into(), b),
            None => (
                "404 Not Found".into(),
                r#"{"message":"not found"}"#.to_string(),
            ),
        }
    }

    /// `GET .../pulls?state=open&limit=L&page=P`: the open MRs by number, as
    /// Forgejo pages them.
    fn list_prs(&self, repo: &str, query: &str) -> String {
        let param = |k: &str| {
            query
                .split('&')
                .find_map(|p| p.strip_prefix(&format!("{k}=")))
                .map(str::to_string)
        };
        let limit: usize = param("limit").and_then(|l| l.parse().ok()).unwrap_or(30);
        let page: usize = param("page").and_then(|p| p.parse().ok()).unwrap_or(1);
        let prefix = format!("/api/v1/repos/binarin/{repo}/pulls/");
        let prs: Vec<(String, String)> = self
            .responses
            .lock()
            .unwrap()
            .iter()
            .filter(|(p, _)| {
                p.strip_prefix(&prefix)
                    .is_some_and(|n| n.parse::<u64>().is_ok())
            })
            .map(|(p, b)| (p.clone(), b.clone()))
            .collect();
        let mut open: Vec<serde_json::Value> = prs
            .into_iter()
            .map(|(p, b)| serde_json::from_str(&self.live(&p, repo, b)).unwrap())
            .filter(|v: &serde_json::Value| {
                v["state"] == "open" && param("state").is_none_or(|s| s == "open")
            })
            .collect();
        open.sort_by_key(|v| v["number"].as_u64());
        let page: Vec<_> = open
            .into_iter()
            .skip((page - 1) * limit)
            .take(limit)
            .collect();
        serde_json::Value::Array(page).to_string()
    }

    /// An open MR's head as the forge has it, with `follow_pull_refs`.
    fn live(&self, path: &str, repo: &str, body: String) -> String {
        let Some(n) = path.rsplit_once("/pulls/").map(|(_, n)| n.to_string()) else {
            return body;
        };
        let forge = self.forge.lock().unwrap().clone();
        let Some(forge) = forge.filter(|_| *self.follow_pull_refs.lock().unwrap()) else {
            return body;
        };
        let mut v: serde_json::Value = serde_json::from_str(&body).unwrap();
        if v["state"] == "open"
            && let Some(head) = git_in(
                &forge.join("binarin").join(repo),
                &[
                    "rev-parse",
                    "--verify",
                    "-q",
                    &format!("refs/pull/{n}/head"),
                ],
            )
        {
            v["head"]["sha"] = head.into();
        }
        v.to_string()
    }

    fn write(&self, method: &str, path: &str, repo: &str, body: &str) -> (String, String) {
        let req: serde_json::Value = serde_json::from_str(body).unwrap_or_default();
        let not_found = || {
            (
                "404 Not Found".to_string(),
                r#"{"message":"not found"}"#.to_string(),
            )
        };
        if method == "PATCH" {
            let mut responses = self.responses.lock().unwrap();
            let Some(pr) = responses.get_mut(path) else {
                return not_found();
            };
            let mut v: serde_json::Value = serde_json::from_str(pr).unwrap();
            if let Some(title) = req["title"].as_str() {
                v["draft"] = title.to_ascii_uppercase().starts_with("WIP:").into();
                v["title"] = title.into();
            }
            if let Some(body) = req["body"].as_str() {
                v["body"] = body.into();
            }
            *pr = v.to_string();
            return ("201 Created".into(), pr.clone());
        }
        if method == "POST" && path == format!("/api/v1/repos/binarin/{repo}/pulls") {
            return self.open_pr(repo, &req);
        }
        let Some(pr_path) = path.strip_suffix("/merge") else {
            return not_found();
        };
        let message = |code: u16, text: &str, msg: &str| {
            (
                format!("{code} {text}"),
                serde_json::json!({ "message": msg }).to_string(),
            )
        };
        if method != "POST" {
            return not_found();
        }
        let reply = self
            .merge_replies
            .lock()
            .unwrap()
            .get_mut(pr_path)
            .and_then(VecDeque::pop_front)
            .unwrap_or(if req["merge_when_checks_succeed"] == true {
                MergeReply::Schedule
            } else {
                MergeReply::MergeNow
            });
        match reply {
            MergeReply::Schedule => {
                if self.scheduled.lock().unwrap().insert(pr_path.to_string()) {
                    ("201 Created".into(), String::new())
                } else {
                    message(
                        409,
                        "Conflict",
                        "pull request is already scheduled to auto merge when checks succeed",
                    )
                }
            }
            MergeReply::ScheduleAsChecksTurnGreen => {
                self.scheduled.lock().unwrap().insert(pr_path.to_string());
                if let Some(head) = self.head(pr_path, repo) {
                    self.set_status_without_event(repo, &head, "success");
                }
                ("201 Created".into(), String::new())
            }
            MergeReply::Status(code, msg) => message(code, "Status", &msg),
            MergeReply::MergeNow => self.merge_pr(pr_path, repo, req["head_commit_id"].as_str()),
        }
    }

    /// Opens an MR as Forgejo's `POST /repos/{owner}/{repo}/pulls` does: the
    /// next number, `refs/pull/N/head` at the head branch, 201 with the MR;
    /// 409 when an open MR of that head and base exists, 404 without the
    /// head branch.
    fn open_pr(&self, repo: &str, req: &serde_json::Value) -> (String, String) {
        let message = |code: &str, msg: &str| {
            (
                code.to_string(),
                serde_json::json!({ "message": msg }).to_string(),
            )
        };
        let (head, base) = (
            req["head"].as_str().unwrap_or(""),
            req["base"].as_str().unwrap_or(""),
        );
        let prefix = format!("/api/v1/repos/binarin/{repo}/pulls/");
        let mut responses = self.responses.lock().unwrap();
        let mut last = 0;
        for (path, body) in responses.iter() {
            let Some(n) = path
                .strip_prefix(&prefix)
                .and_then(|n| n.parse::<u64>().ok())
            else {
                continue;
            };
            last = last.max(n);
            let v: serde_json::Value = serde_json::from_str(body).unwrap();
            if v["state"] == "open" && v["head"]["ref"] == head && v["base"]["ref"] == base {
                return message(
                    "409 Conflict",
                    "pull request already exists for these targets",
                );
            }
        }
        let forge = self
            .forge
            .lock()
            .unwrap()
            .clone()
            .expect("the forge is set");
        let dir = forge.join("binarin").join(repo);
        let Some(sha) = git_in(
            &dir,
            &["rev-parse", "--verify", "-q", &format!("refs/heads/{head}")],
        ) else {
            return message("404 Not Found", "head branch not found");
        };
        let n = last + 1;
        git_in(&dir, &["update-ref", &format!("refs/pull/{n}/head"), &sha]).unwrap();
        let title = req["title"].as_str().unwrap_or("");
        let body = serde_json::json!({
            "number": n,
            "state": "open",
            "merged": false,
            "draft": title.to_ascii_uppercase().starts_with("WIP:"),
            "title": title,
            "body": req["body"],
            "head": { "sha": sha, "ref": head },
            "base": { "ref": base },
            "merge_commit_sha": null,
        })
        .to_string();
        responses.insert(format!("{prefix}{n}"), body.clone());
        ("201 Created".into(), body)
    }

    /// The head of the MR at the API path `pr_path`, as the forge has it.
    fn head(&self, pr_path: &str, repo: &str) -> Option<String> {
        let pr = self.responses.lock().unwrap().get(pr_path).cloned()?;
        let v: serde_json::Value = serde_json::from_str(&self.live(pr_path, repo, pr)).ok()?;
        v["head"]["sha"].as_str().map(str::to_string)
    }

    /// Merges the MR at the API path `pr_path` by fast-forward, as Forgejo
    /// does; with `expected`, only at that head.
    fn merge_pr(&self, pr_path: &str, repo: &str, expected: Option<&str>) -> (String, String) {
        let message = |code: u16, text: &str, msg: &str| {
            (
                format!("{code} {text}"),
                serde_json::json!({ "message": msg }).to_string(),
            )
        };
        let Some(pr) = self.responses.lock().unwrap().get(pr_path).cloned() else {
            return message(404, "Not Found", "not found");
        };
        let mut v: serde_json::Value = serde_json::from_str(&self.live(pr_path, repo, pr)).unwrap();
        let head = v["head"]["sha"].as_str().unwrap_or("").to_string();
        if expected.is_some_and(|e| e != head) {
            return message(409, "Conflict", "head out of date");
        }
        let forge = self
            .forge
            .lock()
            .unwrap()
            .clone()
            .expect("the forge is set");
        let dir = forge.join("binarin").join(repo);
        if git_in(&dir, &["merge-base", "--is-ancestor", "master", &head]).is_none() {
            return message(409, "Conflict", "Not fast-forward: the base moved");
        }
        git_in(&dir, &["update-ref", "refs/heads/master", &head]).unwrap();
        if let Some(branch) = v["head"]["ref"].as_str() {
            git_in(&dir, &["update-ref", "-d", &format!("refs/heads/{branch}")]);
        }
        let n = v["number"].clone();
        v["state"] = "closed".into();
        v["merged"] = true.into();
        v["merge_commit_sha"] = head.into();
        v["head"]["ref"] = format!("refs/pull/{n}/head").into();
        self.responses
            .lock()
            .unwrap()
            .insert(pr_path.to_string(), v.to_string());
        self.scheduled.lock().unwrap().remove(pr_path);
        ("200 OK".into(), String::new())
    }

    pub fn base(&self) -> String {
        format!("http://127.0.0.1:{}/api/v1", self.port)
    }

    pub fn make_private(&self, repo: &str) {
        self.private.lock().unwrap().insert(repo.to_string());
    }

    /// The answers to the next merge requests of `repo#n`, in order.
    pub fn on_merge(&self, repo: &str, n: u64, replies: &[MergeReply]) {
        self.merge_replies
            .lock()
            .unwrap()
            .entry(format!("/api/v1/repos/binarin/{repo}/pulls/{n}"))
            .or_default()
            .extend(replies.iter().cloned());
    }

    /// Whether `repo#n`'s merge is scheduled.
    pub fn is_scheduled(&self, repo: &str, n: u64) -> bool {
        self.scheduled
            .lock()
            .unwrap()
            .contains(&format!("/api/v1/repos/binarin/{repo}/pulls/{n}"))
    }

    /// The requests other than GET: `METHOD path body`.
    pub fn writes(&self) -> Vec<String> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.method != "GET")
            .map(|r| format!("{} {} {}", r.method, r.path, r.body))
            .collect()
    }

    /// An MR: `state` is `open`, `draft`, `merged` or `closed`.
    pub fn set_pr(
        &self,
        repo: &str,
        n: u64,
        state: &str,
        head_ref: &str,
        head: &str,
        merge_commit: Option<&str>,
    ) {
        let body = serde_json::json!({
            "number": n,
            "state": if matches!(state, "open" | "draft") { "open" } else { "closed" },
            "merged": state == "merged",
            "draft": state == "draft",
            "title": if state == "draft" { "WIP: t" } else { "t" },
            "head": { "sha": head, "ref": head_ref },
            "merge_commit_sha": merge_commit,
        });
        self.responses.lock().unwrap().insert(
            format!("/api/v1/repos/binarin/{repo}/pulls/{n}"),
            body.to_string(),
        );
    }

    /// A new commit status of `sha`, which becomes its combined status:
    /// `pending`, `success`, `failure`. As on Forgejo, the status event of a
    /// `success` merges the MRs at that head whose merge is scheduled.
    pub fn set_status(&self, repo: &str, sha: &str, state: &str) {
        self.set_status_without_event(repo, sha, state);
        if state != "success" {
            return;
        }
        let prefix = format!("/api/v1/repos/binarin/{repo}/pulls/");
        let due: Vec<String> = self
            .scheduled
            .lock()
            .unwrap()
            .iter()
            .filter(|p| p.starts_with(&prefix))
            .cloned()
            .collect();
        for pr_path in due {
            if self.head(&pr_path, repo).as_deref() == Some(sha) {
                self.merge_pr(&pr_path, repo, Some(sha));
            }
        }
    }

    /// The combined commit status of `sha`, without the status event that
    /// would start a scheduled merge.
    pub fn set_status_without_event(&self, repo: &str, sha: &str, state: &str) {
        let body = serde_json::json!({
            "state": state,
            "sha": sha,
            "total_count": 1,
            "statuses": [{ "status": state, "context": "ci" }],
        });
        self.responses.lock().unwrap().insert(
            format!("/api/v1/repos/binarin/{repo}/commits/{sha}/status"),
            body.to_string(),
        );
    }
}

pub fn find_on_path(path: &str, name: &str) -> PathBuf {
    env::split_paths(path)
        .map(|d| d.join(name))
        .find(|p| p.is_file())
        .unwrap_or_else(|| panic!("{name} is not on PATH; this test needs it"))
}

pub fn text(out: &Output) -> String {
    format!(
        "status {:?}\nstdout:\n{}\nstderr:\n{}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

pub fn stdout(out: &Output) -> String {
    assert!(out.status.success(), "{}", text(out));
    String::from_utf8_lossy(&out.stdout).trim_end().to_string()
}

pub fn failed(out: &Output, code: i32, needle: &str) {
    assert_eq!(out.status.code(), Some(code), "{}", text(out));
    assert!(
        text(out).contains(needle),
        "missing {needle:?} in\n{}",
        text(out)
    );
}

/// The line of `out` that starts with `prefix`.
pub fn line<'a>(out: &'a str, prefix: &str) -> &'a str {
    out.lines()
        .find(|l| l.starts_with(prefix))
        .unwrap_or_else(|| panic!("no line starting with {prefix:?} in\n{out}"))
}

pub struct Forge {
    pub root: PathBuf,
    pub forge: PathBuf,
    pub sources: PathBuf,
    pub api: FakeApi,
    pub base_path: String,
    pub nix_config: String,
    /// The fake `direnv` logs its arguments here.
    pub direnv_log: PathBuf,
}

/// A post-receive hook: a pushed branch moves the pull refs at its old head.
const FOLLOW_PULL_REFS: &str = r#"#!/bin/sh
while read old new ref; do
  git for-each-ref --format='%(objectname) %(refname)' 'refs/pull/*/head' |
  while read sha r; do
    if [ "$sha" = "$old" ]; then git update-ref "$r" "$new"; fi
  done
done
"#;

/// A fake `direnv` that logs its calls.
const FAKE_DIRENV: &str = "#!/bin/sh\necho \"$*\" >> \"$DIRENV_LOG\"\n";

impl Forge {
    /// Zwanenzang, swan-lake-common (master = current) and a locked
    /// consumer swan-lake on the forge, each with its source.
    pub fn new(name: &str) -> Self {
        let root = env::temp_dir().join(format!("swl-ws-{name}-{}", std::process::id()));
        if root.exists() {
            fs::remove_dir_all(&root).unwrap();
        }
        fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let base_path = env::var("PATH").unwrap();
        let mut nix_config =
            "experimental-features = nix-command flakes\nflake-registry =\n".to_string();
        if let Ok(store) = env::var("SWL_TEST_NIX_STORE") {
            nix_config.push_str(&format!("store = {store}\n"));
        }
        let f = Self {
            forge: root.join("forge"),
            sources: root.join("sources"),
            api: FakeApi::start(),
            direnv_log: root.join("direnv.log"),
            base_path,
            nix_config,
            root,
        };
        *f.api.forge.lock().unwrap() = Some(f.forge.clone());
        // A fresh store (SWL_TEST_NIX_STORE) gets its database on first use,
        // which the tests of one binary would race on: one call first.
        static STORE_READY: std::sync::Once = std::sync::Once::new();
        STORE_READY.call_once(|| {
            stdout(&f.nix(&f.root, &["store", "info"]));
        });
        let fakebin = f.root.join("fakebin");
        fs::create_dir_all(&fakebin).unwrap();
        fs::write(fakebin.join("direnv"), FAKE_DIRENV).unwrap();
        fs::set_permissions(fakebin.join("direnv"), fs::Permissions::from_mode(0o755)).unwrap();
        let mut config = format!(
            "url_patterns = [\"git+file://{}/binarin/{{repo}}\"]\n",
            f.forge.display()
        );
        for repo in ["zwanenzang", "swan-lake-common", "swan-lake"] {
            config.push_str(&format!(
                "[repos.{repo}]\nsource = \"{}\"\n",
                f.source(repo).display()
            ));
        }
        fs::write(f.root.join("repos.toml"), config).unwrap();

        let seed = f.root.join("seed");
        let zw_seed = seed.join("zwanenzang");
        f.seed(&zw_seed, ZW_FLAKE, "z1");
        f.bare("zwanenzang", &zw_seed);
        let common_seed = seed.join("swan-lake-common");
        f.seed(&common_seed, COMMON_FLAKE, "c1");
        stdout(&f.nix(&common_seed, &["flake", "lock"]));
        f.git(&common_seed, &["add", "flake.lock"]);
        f.git(&common_seed, &["commit", "-qm", "lock"]);
        f.git(&common_seed, &["branch", "current"]);
        f.bare("swan-lake-common", &common_seed);
        let consumer_seed = seed.join("swan-lake");
        f.seed(&consumer_seed, CONSUMER_FLAKE, "consumer-1");
        fs::write(
            consumer_seed.join(".envrc"),
            "source_env_if_exists .envrc.local\nuse flake \"${use_flake_args[@]}\"\n",
        )
        .unwrap();
        stdout(&f.nix(&consumer_seed, &["flake", "lock"]));
        f.git(&consumer_seed, &["add", "flake.lock", ".envrc"]);
        f.git(&consumer_seed, &["commit", "-qm", "lock"]);
        f.bare("swan-lake", &consumer_seed);
        for repo in ["zwanenzang", "swan-lake-common", "swan-lake"] {
            let source = f.source(repo);
            f.git(
                &f.root,
                &["init", "-q", "--bare", &source.display().to_string()],
            );
            f.git(
                &source,
                &[
                    "remote",
                    "add",
                    "origin",
                    &f.forge_repo(repo).display().to_string(),
                ],
            );
            f.git(&source, &["fetch", "-q", "origin"]);
        }
        f
    }

    /// Another consumer like swan-lake, locked to common's `current`, on the
    /// forge, with its source and its `[repos.<repo>]` in the config.
    pub fn add_consumer(&self, repo: &str) {
        self.add_consumer_with(repo, CONSUMER_FLAKE);
    }

    /// Another consumer with its own `flake.nix` (`@FORGE@` for the forge).
    pub fn add_consumer_with(&self, repo: &str, flake: &str) {
        let seed = self.root.join("seed").join(repo);
        self.seed(&seed, flake, &format!("{repo}-1"));
        stdout(&self.nix(&seed, &["flake", "lock"]));
        self.git(&seed, &["add", "flake.lock"]);
        self.git(&seed, &["commit", "-qm", "lock"]);
        self.bare(repo, &seed);
        let source = self.source(repo);
        self.git(
            &self.root,
            &["init", "-q", "--bare", &source.display().to_string()],
        );
        self.git(
            &source,
            &[
                "remote",
                "add",
                "origin",
                &self.forge_repo(repo).display().to_string(),
            ],
        );
        self.git(&source, &["fetch", "-q", "origin"]);
        let config = self.root.join("repos.toml");
        let mut text = fs::read_to_string(&config).unwrap();
        text.push_str(&format!(
            "[repos.{repo}]\nsource = \"{}\"\n",
            source.display()
        ));
        fs::write(config, text).unwrap();
    }

    pub fn source(&self, repo: &str) -> PathBuf {
        self.sources.join(format!("{repo}.git"))
    }

    pub fn forge_repo(&self, repo: &str) -> PathBuf {
        self.forge.join("binarin").join(repo)
    }

    fn seed(&self, dir: &Path, flake: &str, value: &str) {
        fs::create_dir_all(dir).unwrap();
        fs::write(
            dir.join("flake.nix"),
            flake.replace("@FORGE@", &self.forge.display().to_string()),
        )
        .unwrap();
        fs::write(dir.join("value.txt"), value).unwrap();
        self.git(dir, &["init", "-q", "-b", "master"]);
        self.git(dir, &["add", "."]);
        self.git(dir, &["commit", "-qm", "init"]);
    }

    fn bare(&self, repo: &str, seed: &Path) {
        let dest = self.forge_repo(repo);
        self.git(
            &self.root,
            &[
                "clone",
                "-q",
                "--bare",
                &seed.display().to_string(),
                &dest.display().to_string(),
            ],
        );
    }

    /// A clone of the forge repo, for commits made "elsewhere".
    pub fn clone_elsewhere(&self, repo: &str, name: &str) -> PathBuf {
        let dir = self.root.join(name);
        self.git(
            &self.root,
            &[
                "clone",
                "-q",
                &self.forge_repo(repo).display().to_string(),
                &dir.display().to_string(),
            ],
        );
        dir
    }

    /// A commit on the forge's `branch` of `repo`, made in a scratch clone;
    /// returns its sha.
    pub fn commit_on_forge(&self, repo: &str, branch: &str, value: &str) -> String {
        let dir = self.root.join(format!("elsewhere-{repo}-{value}"));
        let url = self.forge_repo(repo).display().to_string();
        self.git(
            &self.root,
            &["clone", "-q", &url, &dir.display().to_string()],
        );
        if self
            .git(
                &dir,
                &["ls-remote", "origin", &format!("refs/heads/{branch}")],
            )
            .is_empty()
        {
            self.git(&dir, &["checkout", "-qb", branch]);
        } else {
            self.git(&dir, &["checkout", "-q", branch]);
        }
        fs::write(dir.join("value.txt"), value).unwrap();
        self.git(&dir, &["commit", "-qam", &format!("{repo}: {value}")]);
        self.git(&dir, &["push", "-q", "origin", &format!("HEAD:{branch}")]);
        let sha = self.git(&dir, &["rev-parse", "HEAD"]);
        fs::remove_dir_all(&dir).unwrap();
        sha
    }

    /// Makes the forge repo of `repo` move an MR's `refs/pull/N/head` with
    /// pushes to its branch, as Forgejo does, and the fake API report open
    /// MRs at those heads.
    pub fn follow_pull_refs(&self, repo: &str) {
        self.hook(repo, "post-receive", FOLLOW_PULL_REFS);
        *self.api.follow_pull_refs.lock().unwrap() = true;
    }

    /// Installs (or with `None` removes) a hook in the forge repo.
    pub fn hook_opt(&self, repo: &str, name: &str, script: Option<&str>) {
        let path = self.forge_repo(repo).join("hooks").join(name);
        match script {
            Some(s) => {
                fs::write(&path, s).unwrap();
                fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
            }
            None => {
                let _ = fs::remove_file(&path);
            }
        }
    }

    pub fn hook(&self, repo: &str, name: &str, script: &str) {
        self.hook_opt(repo, name, Some(script));
    }

    /// A fake fj login: keys.json with the token the fake API wants.
    pub fn login(&self) -> PathBuf {
        self.login_with("fj-keys.json", TOKEN, None)
    }

    /// A keys.json with `token`, and an OAuth `expires_at` if given.
    pub fn login_with(
        &self,
        file: &str,
        token: &str,
        expires_at: Option<serde_json::Value>,
    ) -> PathBuf {
        let keys = self.root.join(file);
        let mut host = serde_json::json!({
            "type": "Application", "name": "binarin", "token": token });
        if let Some(at) = expires_at {
            host["type"] = "OAuth".into();
            host["refresh_token"] = "r".into();
            host["expires_at"] = at;
        }
        fs::write(
            &keys,
            serde_json::json!({
                "hosts": { "forgejo.lynx-lizard.ts.net": host },
                "aliases": {},
                "default_ssh": [],
            })
            .to_string(),
        )
        .unwrap();
        keys
    }

    /// The requests to `repo` that carried an Authorization header.
    pub fn authed_requests(&self, repo: &str) -> Vec<String> {
        let prefix = format!("/api/v1/repos/binarin/{repo}/");
        self.api
            .seen_auth
            .lock()
            .unwrap()
            .iter()
            .filter(|(p, a)| p.starts_with(&prefix) && a.is_some())
            .map(|(p, _)| p.clone())
            .collect()
    }

    pub fn env(&self, c: &mut Command, path: &str) {
        let home = self.root.join("home");
        for var in [
            "SWL_FORGE_API",
            "SWL_FORGE_GIT",
            "SWL_FJ_KEYS",
            "SWL_GITHUB_TOKEN",
            "SWL_LOG",
            "NIX_STORE_DIR",
            "NO_COLOR",
            "CLICOLOR",
            "CLICOLOR_FORCE",
        ] {
            c.env_remove(var);
        }
        c.env("PATH", path)
            .env("HOME", &home)
            .env("XDG_CONFIG_HOME", home.join(".config"))
            .env("XDG_CACHE_HOME", home.join(".cache"))
            .env("XDG_DATA_HOME", home.join(".local/share"))
            .env("XDG_STATE_HOME", home.join(".local/state"))
            .env("NIX_CONFIG", &self.nix_config)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "swl test")
            .env("GIT_AUTHOR_EMAIL", "swl@example.org")
            .env("GIT_COMMITTER_NAME", "swl test")
            .env("GIT_COMMITTER_EMAIL", "swl@example.org")
            .env("SWL_CONFIG", self.root.join("repos.toml"))
            .env("DIRENV_LOG", &self.direnv_log)
            .env("SWL_FORGE_API", self.api.base())
            // No GitHub source here: nothing may ask the real API.
            .env("SWL_GITHUB_API", "http://127.0.0.1:9");
    }

    pub fn git(&self, dir: &Path, args: &[&str]) -> String {
        let mut c = Command::new("git");
        self.env(&mut c, &self.base_path);
        stdout(&c.arg("-C").arg(dir).args(args).output().unwrap())
    }

    pub fn nix(&self, dir: &Path, args: &[&str]) -> Output {
        let mut c = Command::new(find_on_path(&self.base_path, "nix"));
        self.env(&mut c, &self.base_path);
        c.current_dir(dir).args(args).output().unwrap()
    }

    /// swl with the fake direnv first on PATH.
    pub fn swl(&self, dir: &Path, args: &[&str]) -> Output {
        self.swl_env(dir, args, &[])
    }

    pub fn swl_env(&self, dir: &Path, args: &[&str], extra_env: &[(&str, &str)]) -> Output {
        let path = format!("{}:{}", self.root.join("fakebin").display(), self.base_path);
        self.swl_path(dir, args, &path, extra_env)
    }

    pub fn swl_path(
        &self,
        dir: &Path,
        args: &[&str],
        path: &str,
        extra_env: &[(&str, &str)],
    ) -> Output {
        let mut c = Command::new(env!("CARGO_BIN_EXE_swl"));
        self.env(&mut c, path);
        for (k, v) in extra_env {
            c.env(k, v);
        }
        c.current_dir(dir).args(args).output().unwrap()
    }

    /// A PATH with git, nix and curl (those found) only: no direnv.
    pub fn path_without_direnv(&self) -> String {
        let bin = self.root.join("toolbin");
        fs::create_dir_all(&bin).unwrap();
        for tool in ["git", "nix", "curl"] {
            let link = bin.join(tool);
            let found = env::split_paths(&self.base_path)
                .map(|d| d.join(tool))
                .find(|p| p.is_file());
            if let Some(found) = found
                && !link.exists()
            {
                std::os::unix::fs::symlink(found, &link).unwrap();
            }
        }
        bin.display().to_string()
    }

    pub fn direnv_calls(&self) -> String {
        fs::read_to_string(&self.direnv_log).unwrap_or_default()
    }
}

impl Drop for Forge {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

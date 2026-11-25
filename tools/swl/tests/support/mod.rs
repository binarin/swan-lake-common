//! A disposable forge for the workspace tests (`ws add`, `start`, `status`):
//! bare "forge" repos reached by `git+file:`, a bare source per repo whose
//! `origin` is the forge repo (as in `repos.toml`), and a fake forge API (a
//! local HTTP server) whose private repos answer only with a token. Offline,
//! with real git and Nix; set SWL_TEST_NIX_STORE to use a store in a scratch
//! directory, as the package check does inside the build sandbox.

#![allow(dead_code)]

use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    io::{BufRead, BufReader, Write},
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

/// GET responses by path; anything else is 404. Paths under a private repo
/// answer only with `Authorization: token TOKEN`, as Forgejo does.
#[derive(Clone, Default)]
pub struct FakeApi {
    pub port: u16,
    pub responses: Arc<Mutex<BTreeMap<String, String>>>,
    pub private: Arc<Mutex<BTreeSet<String>>>,
    pub seen_auth: SeenAuth,
}

impl FakeApi {
    pub fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let api = Self {
            port: listener.local_addr().unwrap().port(),
            ..Default::default()
        };
        let (responses, private, seen) = (
            api.responses.clone(),
            api.private.clone(),
            api.seen_auth.clone(),
        );
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut request = String::new();
                reader.read_line(&mut request).ok();
                let mut auth = None;
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                        break;
                    }
                    if let Some((k, v)) = line.split_once(':')
                        && k.eq_ignore_ascii_case("authorization")
                    {
                        auth = Some(v.trim().to_string());
                    }
                }
                let path = request.split_whitespace().nth(1).unwrap_or("").to_string();
                seen.lock().unwrap().push((path.clone(), auth.clone()));
                let repo = path
                    .strip_prefix("/api/v1/repos/binarin/")
                    .and_then(|r| r.split('/').next())
                    .unwrap_or("")
                    .to_string();
                let allowed = !private.lock().unwrap().contains(&repo)
                    || auth.as_deref() == Some(&format!("token {TOKEN}"));
                let found = allowed
                    .then(|| responses.lock().unwrap().get(&path).cloned())
                    .flatten();
                let (status, body) = match found {
                    Some(b) => ("200 OK", b),
                    None => ("404 Not Found", r#"{"message":"not found"}"#.to_string()),
                };
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
            }
        });
        api
    }

    pub fn base(&self) -> String {
        format!("http://127.0.0.1:{}/api/v1", self.port)
    }

    pub fn make_private(&self, repo: &str) {
        self.private.lock().unwrap().insert(repo.to_string());
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

    /// The combined commit status of `sha`: `pending`, `success`, `failure`.
    pub fn set_status(&self, repo: &str, sha: &str, state: &str) {
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

    /// A fake fj login: keys.json with the token the fake API wants.
    pub fn login(&self) -> PathBuf {
        let keys = self.root.join("fj-keys.json");
        fs::write(
            &keys,
            serde_json::json!({
                "hosts": { "forgejo.lynx-lizard.ts.net": {
                    "type": "Application", "name": "binarin", "token": TOKEN } },
                "aliases": {},
                "default_ssh": [],
            })
            .to_string(),
        )
        .unwrap();
        keys
    }

    pub fn env(&self, c: &mut Command, path: &str) {
        let home = self.root.join("home");
        for var in [
            "SWL_FORGE_API",
            "SWL_FORGE_GIT",
            "SWL_FJ_KEYS",
            "NIX_STORE_DIR",
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
            .env("SWL_FORGE_API", self.api.base());
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

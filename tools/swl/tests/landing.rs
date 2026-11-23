//! The manifest workflow on disposable local repos: `swl publish` with a
//! fake `fj`, `swl check`, `swl ci env` and `swl finalize` against a fake
//! forge API (a local HTTP server) and a "forge" of bare repos reached by
//! `git+file:`. Offline, with real git and Nix; set SWL_TEST_NIX_STORE to use
//! a store in a scratch directory, as the package check does inside the
//! build sandbox.

use std::{
    collections::BTreeMap,
    env, fs,
    io::{BufRead, BufReader, Write},
    net::TcpListener,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::{Arc, Mutex},
    thread,
};

/// Common pins zwanenzang's `current`, for nested (`lib/zwanenzang`) entries.
const COMMON_FLAKE: &str = r#"{
  inputs.zwanenzang.url = "git+file://@FORGE@/binarin/zwanenzang?ref=current";
  outputs = { self, zwanenzang }: {
    value = builtins.readFile ./value.txt;
    zw = zwanenzang.value;
  };
}
"#;

const ZW_FLAKE: &str = r#"{
  outputs = { self }: {
    value = builtins.readFile ./value.txt;
  };
}
"#;

/// The consumer pins common's promotion branch `current`, like
/// `swan-lake-current`, as `lib` and `extra`. Not as `common`, so that the
/// common pin policy, which wants the real GitHub URL, does not apply.
const CONSUMER_FLAKE: &str = r#"{
  inputs.lib.url = "git+file://@FORGE@/binarin/swan-lake-common?ref=current";
  inputs.extra.url = "git+file://@FORGE@/binarin/swan-lake-common?ref=current";
  outputs = { self, lib, extra, ... }: {
    value = builtins.readFile ./value.txt + "+" + lib.value;
    extra = extra.value;
    zw = lib.zw;
  };
}
"#;

/// A fake `fj`: MRs are files `<state>/<owner>_<repo>/<n>` holding
/// `state head base` and the title; every call is logged.
const FAKE_FJ: &str = r##"#!/bin/sh
state="$FJ_STATE"
echo "$*" >> "$state/log"
if [ "$1" = -H ]; then shift 2; fi
cmd="$1 $2"
shift 2
case "$cmd" in
"pr search")
  repo=""
  while [ $# -gt 0 ]; do
    case "$1" in --repo) repo="$2"; shift 2;; *) shift;; esac
  done
  d="$state/$(echo "$repo" | tr / _)"
  echo "some pull requests"
  for f in "$d"/*; do
    [ -f "$f" ] || continue
    read -r st head base < "$f"
    if [ "$st" = open ]; then echo "#$(basename "$f"): $(sed -n 2p "$f") (by fake)"; fi
  done
  ;;
"pr view")
  repo="${1%#*}"
  num="${1#*#}"
  f="$state/$(echo "$repo" | tr / _)/$num"
  read -r st head base < "$f"
  echo "$(sed -n 2p "$f") #$num"
  echo "By fake — Open — +1 -0"
  echo "From \`$head\` into \`$base\`"
  ;;
"pr create")
  while [ $# -gt 1 ]; do
    case "$1" in
      --repo) repo="$2"; shift 2;;
      --base) base="$2"; shift 2;;
      --head) head="$2"; shift 2;;
      --body-file) cp "$2" "$state/last-body"; shift 2;;
      *) shift;;
    esac
  done
  title="$1"
  d="$state/$(echo "$repo" | tr / _)"
  mkdir -p "$d"
  n=$(( $(ls "$d" | wc -l) + 1 ))
  printf 'open %s %s\n%s\n' "$head" "$base" "$title" > "$d/$n"
  echo "created pull request #$n: $title"
  ;;
*) echo "fake fj: unexpected $cmd" >&2; exit 3;;
esac
"##;

/// GET responses by path; anything else is 404.
#[derive(Clone, Default)]
struct FakeApi {
    port: u16,
    responses: Arc<Mutex<BTreeMap<String, String>>>,
}

impl FakeApi {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let api = Self {
            port: listener.local_addr().unwrap().port(),
            ..Default::default()
        };
        let responses = api.responses.clone();
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut request = String::new();
                reader.read_line(&mut request).ok();
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                        break;
                    }
                }
                let path = request.split_whitespace().nth(1).unwrap_or("").to_string();
                let found = responses.lock().unwrap().get(&path).cloned();
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

    fn base(&self) -> String {
        format!("http://127.0.0.1:{}/api/v1", self.port)
    }

    fn set(&self, repo: &str, n: u64, state: &str, head: &str, merge_commit: Option<&str>) {
        let body = serde_json::json!({
            "number": n,
            "state": if state == "open" { "open" } else { "closed" },
            "merged": state == "merged",
            "title": "t",
            "head": { "sha": head, "ref": format!("refs/pull/{n}/head") },
            "merge_commit_sha": merge_commit,
        });
        self.responses.lock().unwrap().insert(
            format!("/api/v1/repos/binarin/{repo}/pulls/{n}"),
            body.to_string(),
        );
    }
}

fn find_on_path(path: &str, name: &str) -> PathBuf {
    env::split_paths(path)
        .map(|d| d.join(name))
        .find(|p| p.is_file())
        .unwrap_or_else(|| panic!("{name} is not on PATH; this test needs it"))
}

fn text(out: &Output) -> String {
    format!(
        "status {:?}\nstdout:\n{}\nstderr:\n{}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn stdout(out: &Output) -> String {
    assert!(out.status.success(), "{}", text(out));
    String::from_utf8_lossy(&out.stdout).trim_end().to_string()
}

fn failed(out: &Output, code: i32, needle: &str) {
    assert_eq!(out.status.code(), Some(code), "{}", text(out));
    assert!(
        text(out).contains(needle),
        "missing {needle:?} in\n{}",
        text(out)
    );
}

struct World {
    root: PathBuf,
    forge: PathBuf,
    ws: PathBuf,
    consumer: PathBuf,
    common: PathBuf,
    fj_state: PathBuf,
    api: FakeApi,
    base_path: String,
    nix_config: String,
}

impl World {
    /// The forge with common (master = current = "c1") and a locked
    /// consumer; a workspace with both cloned, on branches `feat` and
    /// `feature`, each one commit ahead.
    fn new(name: &str) -> Self {
        let root = env::temp_dir().join(format!("swl-landing-{name}-{}", std::process::id()));
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
        let ws = root.join("ws");
        let w = Self {
            forge: root.join("forge"),
            consumer: ws.join("swan-lake"),
            common: ws.join("swan-lake-common@lib"),
            fj_state: root.join("fj-state"),
            ws,
            api: FakeApi::start(),
            base_path,
            nix_config,
            root,
        };
        fs::create_dir_all(&w.fj_state).unwrap();
        let fakebin = w.root.join("fakebin");
        fs::create_dir_all(&fakebin).unwrap();
        fs::write(fakebin.join("fj"), FAKE_FJ).unwrap();
        fs::set_permissions(fakebin.join("fj"), fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(
            w.root.join("repos.toml"),
            format!(
                "url_patterns = [\"git+file://{}/binarin/{{repo}}\"]\n",
                w.forge.display()
            ),
        )
        .unwrap();

        let seed = w.root.join("seed");
        let zw_seed = seed.join("zwanenzang");
        w.seed(&zw_seed, ZW_FLAKE, "z1");
        w.git(&zw_seed, &["branch", "current"]);
        w.bare("zwanenzang", &zw_seed);
        let common_seed = seed.join("swan-lake-common");
        w.seed(&common_seed, COMMON_FLAKE, "c1");
        stdout(&w.nix(&common_seed, &["flake", "lock"]));
        w.git(&common_seed, &["add", "flake.lock"]);
        w.git(&common_seed, &["commit", "-qm", "lock"]);
        w.git(&common_seed, &["branch", "current"]);
        w.bare("swan-lake-common", &common_seed);
        let consumer_seed = seed.join("swan-lake");
        w.seed(&consumer_seed, CONSUMER_FLAKE, "consumer-1");
        stdout(&w.nix(&consumer_seed, &["flake", "lock"]));
        w.git(&consumer_seed, &["add", "flake.lock"]);
        w.git(&consumer_seed, &["commit", "-qm", "lock"]);
        w.bare("swan-lake", &consumer_seed);

        fs::create_dir_all(&w.ws).unwrap();
        for (repo, dir) in [("swan-lake", &w.consumer), ("swan-lake-common", &w.common)] {
            let url = w.forge.join("binarin").join(repo);
            w.git(
                &w.ws,
                &[
                    "clone",
                    "-q",
                    &url.display().to_string(),
                    &dir.display().to_string(),
                ],
            );
        }
        w.git(&w.common, &["checkout", "-qb", "feature"]);
        fs::write(w.common.join("value.txt"), "c2").unwrap();
        w.git(&w.common, &["commit", "-qam", "common: c2"]);
        w.git(&w.consumer, &["checkout", "-qb", "feat"]);
        fs::write(w.consumer.join("value.txt"), "consumer-2").unwrap();
        w.git(&w.consumer, &["commit", "-qam", "consumer: use c2"]);
        w
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
        let dest = self.forge.join("binarin").join(repo);
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

    fn forge_repo(&self, repo: &str) -> PathBuf {
        self.forge.join("binarin").join(repo)
    }

    fn env(&self, c: &mut Command, path: &str) {
        let home = self.root.join("home");
        for var in [
            "GITHUB_EVENT_NAME",
            "GITHUB_REF",
            "GITHUB_BASE_REF",
            "GITHUB_PATH",
            "RUNNER_TEMP",
            "SWL_FORGE_API",
            "SWL_FORGE_GIT",
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
            .env("FJ_STATE", &self.fj_state)
            .env("SWL_FORGE_API", self.api.base())
            .env("SWL_FORGE_GIT", format!("file://{}", self.forge.display()));
    }

    fn git(&self, dir: &Path, args: &[&str]) -> String {
        let mut c = Command::new("git");
        self.env(&mut c, &self.base_path);
        stdout(&c.arg("-C").arg(dir).args(args).output().unwrap())
    }

    fn nix(&self, dir: &Path, args: &[&str]) -> Output {
        let mut c = Command::new(find_on_path(&self.base_path, "nix"));
        self.env(&mut c, &self.base_path);
        c.current_dir(dir).args(args).output().unwrap()
    }

    /// swl with the fake fj first on PATH.
    fn swl(&self, dir: &Path, args: &[&str], extra_env: &[(&str, &str)]) -> Output {
        let mut c = Command::new(env!("CARGO_BIN_EXE_swl"));
        let path = format!("{}:{}", self.root.join("fakebin").display(), self.base_path);
        self.env(&mut c, &path);
        for (k, v) in extra_env {
            c.env(k, v);
        }
        c.current_dir(dir).args(args).output().unwrap()
    }

    fn fj_log(&self) -> String {
        fs::read_to_string(self.fj_state.join("log")).unwrap_or_default()
    }

    /// What `swl publish` would have done for common: the branch and the
    /// MR head on the forge, an open MR 1, and the manifest committed.
    fn published(&self) -> String {
        let head = self.git(&self.common, &["rev-parse", "HEAD"]);
        self.git(&self.common, &["push", "-q", "origin", "feature"]);
        let forge = self.forge_repo("swan-lake-common");
        self.git(&forge, &["update-ref", "refs/pull/1/head", &head]);
        self.api.set("swan-lake-common", 1, "open", &head, None);
        fs::write(
            self.consumer.join("swl.toml"),
            "[inputs.lib]\nrepo = \"swan-lake-common\"\nmr = 1\n",
        )
        .unwrap();
        self.git(&self.consumer, &["add", "swl.toml"]);
        self.git(&self.consumer, &["commit", "-qm", "swl: manifest"]);
        head
    }

    fn eval_value(&self, path: &str) -> String {
        let mut c = Command::new("nix");
        self.env(&mut c, path);
        stdout(
            &c.current_dir(&self.consumer)
                .args(["eval", "--raw", ".#value"])
                .output()
                .unwrap(),
        )
    }
}

impl Drop for World {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn publish_is_rerunnable_and_fast_forward_only() {
    let w = World::new("publish");
    let ws = w.ws.display().to_string();
    let publish = ["publish", "--workspace", &ws, "swan-lake"];
    let out = w.swl(&w.root, &publish, &[]);
    let log = stdout(&out);
    let common_head = w.git(&w.common, &["rev-parse", "HEAD"]);
    let forge_common = w.forge_repo("swan-lake-common");
    assert_eq!(w.git(&forge_common, &["rev-parse", "feature"]), common_head);
    // A draft MR for common, titled from its commit.
    let mr = fs::read_to_string(w.fj_state.join("binarin_swan-lake-common/1")).unwrap();
    assert_eq!(mr, "open feature master\nWIP: common: c2\n");
    assert!(log.contains("opened MR swan-lake-common#1"), "{log}");
    // The manifest, committed on the consumer branch, then pushed with its MR.
    assert_eq!(
        fs::read_to_string(w.consumer.join("swl.toml"))
            .unwrap()
            .lines()
            .filter(|l| !l.starts_with('#') && !l.is_empty())
            .collect::<Vec<_>>(),
        ["[inputs.lib]", "repo = \"swan-lake-common\"", "mr = 1"]
    );
    assert_eq!(
        w.git(&w.consumer, &["log", "-1", "--format=%s"]),
        "swl: manifest for swan-lake-common#1"
    );
    assert_eq!(w.git(&w.consumer, &["status", "--porcelain"]), "");
    let consumer_head = w.git(&w.consumer, &["rev-parse", "HEAD"]);
    assert_eq!(
        w.git(&w.forge_repo("swan-lake"), &["rev-parse", "feat"]),
        consumer_head
    );
    let mr = fs::read_to_string(w.fj_state.join("binarin_swan-lake/1")).unwrap();
    assert_eq!(mr, "open feat master\nconsumer: use c2\n");
    let body = fs::read_to_string(w.fj_state.join("last-body")).unwrap();
    assert!(body.contains("- swan-lake-common#1 (lib)"), "{body}");
    // The consumer's lock is untouched.
    assert_eq!(
        w.git(
            &w.consumer,
            &["diff", "origin/master", "HEAD", "--", "flake.lock"]
        ),
        ""
    );

    // Rerun: everything is found, nothing changes.
    let creates = |log: &str| log.lines().filter(|l| l.contains("pr create")).count();
    assert_eq!(creates(&w.fj_log()), 2);
    let log = stdout(&w.swl(&w.root, &publish, &[]));
    assert_eq!(creates(&w.fj_log()), 2, "{}", w.fj_log());
    assert_eq!(w.git(&w.consumer, &["rev-parse", "HEAD"]), consumer_head);
    for line in [
        "swan-lake-common@lib: feature is pushed",
        "swan-lake-common@lib: MR swan-lake-common#1 is open for feature",
        "swan-lake: swl.toml is current",
        "swan-lake: feat is pushed",
        "swan-lake: MR swan-lake#1 is open for feat",
    ] {
        assert!(log.lines().any(|l| l == line), "missing {line:?} in\n{log}");
    }

    // A new local commit on top publishes fast-forward; the MR stays.
    fs::write(w.common.join("value.txt"), "c3").unwrap();
    w.git(&w.common, &["commit", "-qam", "common: c3"]);
    stdout(&w.swl(&w.root, &publish, &[]));
    assert_eq!(
        w.git(&forge_common, &["rev-parse", "feature"]),
        w.git(&w.common, &["rev-parse", "HEAD"])
    );
    assert_eq!(creates(&w.fj_log()), 2);
    assert_eq!(w.git(&w.consumer, &["rev-parse", "HEAD"]), consumer_head);

    // Someone else's commit on origin's feature: not a fast-forward, refused
    // before anything is pushed.
    let other = w.root.join("other");
    w.git(
        &w.root,
        &[
            "clone",
            "-q",
            "-b",
            "feature",
            &forge_common.display().to_string(),
            "other",
        ],
    );
    fs::write(other.join("value.txt"), "theirs").unwrap();
    w.git(&other, &["commit", "-qam", "theirs"]);
    w.git(&other, &["push", "-q", "origin", "feature"]);
    let theirs = w.git(&other, &["rev-parse", "HEAD"]);
    fs::write(w.common.join("value.txt"), "c4").unwrap();
    w.git(&w.common, &["commit", "-qam", "common: c4"]);
    fs::write(w.consumer.join("value.txt"), "consumer-3").unwrap();
    w.git(&w.consumer, &["commit", "-qam", "consumer: more"]);
    let consumer_head = w.git(&w.consumer, &["rev-parse", "HEAD"]);
    failed(&w.swl(&w.root, &publish, &[]), 2, "not be a fast-forward");
    assert_eq!(w.git(&forge_common, &["rev-parse", "feature"]), theirs);
    assert_ne!(
        w.git(&w.forge_repo("swan-lake"), &["rev-parse", "feat"]),
        consumer_head
    );
    assert_eq!(w.git(&w.consumer, &["rev-parse", "HEAD"]), consumer_head);

    // Detached HEAD and tracked changes are refused too.
    w.git(&w.common, &["checkout", "-q", "--detach"]);
    failed(&w.swl(&w.root, &publish, &[]), 2, "HEAD is detached");
    w.git(&w.common, &["checkout", "-q", "feature"]);
    fs::write(w.common.join("value.txt"), "dirty").unwrap();
    failed(&w.swl(&w.root, &publish, &[]), 2, "uncommitted changes");
    assert_eq!(creates(&w.fj_log()), 2);
}

#[test]
fn ci_env_overrides_inputs_with_the_mr_head() {
    let w = World::new("ci");
    let head = w.published();
    let gh_path = w.root.join("github-path");
    let runner = w.root.join("runner");
    fs::create_dir_all(&runner).unwrap();
    let ci_env = [
        ("GITHUB_PATH", gh_path.to_str().unwrap()),
        ("RUNNER_TEMP", runner.to_str().unwrap()),
    ];
    let lock_before = fs::read(w.consumer.join("flake.lock")).unwrap();
    let out = stdout(&w.swl(&w.consumer, &["ci", "env"], &ci_env));
    assert!(
        out.contains(&format!(
            "--override-input lib git+file://{}/binarin/swan-lake-common?ref=refs/pull/1/head&rev={head}",
            w.forge.display()
        )),
        "{out}"
    );
    let dirs = fs::read_to_string(&gh_path).unwrap();
    let bin = dirs.lines().last().unwrap();
    assert!(bin.starts_with(runner.to_str().unwrap()), "{bin}");

    // Later steps' nix, whatever calls it, builds against the MR head.
    let shimmed = format!("{bin}:{}", w.base_path);
    assert_eq!(w.eval_value(&shimmed), "consumer-2+c2");
    assert_eq!(w.eval_value(&w.base_path), "consumer-2+c1");
    let mut c = Command::new("nix");
    w.env(&mut c, &shimmed);
    let out = c
        .current_dir(&w.consumer)
        .args(["flake", "lock"])
        .output()
        .unwrap();
    failed(&out, 1, "swl shim: refusing");
    assert_eq!(
        fs::read(w.consumer.join("flake.lock")).unwrap(),
        lock_before
    );

    // Merged: still used, with a note.
    w.api
        .set("swan-lake-common", 1, "merged", &head, Some(&head));
    let out = stdout(&w.swl(&w.consumer, &["ci", "env"], &ci_env));
    assert!(out.contains("can land"), "{out}");
    // Closed without merging, unknown, or the API unreachable: fail, and
    // nothing is put on PATH.
    let lines = fs::read_to_string(&gh_path).unwrap().lines().count();
    w.api.set("swan-lake-common", 1, "closed", &head, None);
    failed(
        &w.swl(&w.consumer, &["ci", "env"], &ci_env),
        1,
        "closed without merging",
    );
    w.api.responses.lock().unwrap().clear();
    failed(
        &w.swl(&w.consumer, &["ci", "env"], &ci_env),
        1,
        "no such MR",
    );
    let unreachable = [
        ci_env[0],
        ci_env[1],
        ("SWL_FORGE_API", "http://127.0.0.1:9/api/v1"),
    ];
    failed(
        &w.swl(&w.consumer, &["ci", "env"], &unreachable),
        1,
        "cannot read the MR state",
    );
    assert_eq!(fs::read_to_string(&gh_path).unwrap().lines().count(), lines);
}

#[test]
fn check_gates_mrs_and_master() {
    let w = World::new("check");
    let head = w.published();
    let mr = ["check", "--event", "mr", "--base", "origin/master"];
    let out = stdout(&w.swl(&w.consumer, &mr, &[]));
    assert!(out.contains("MR checks passed"), "{out}");
    // The event and base from the Forgejo Actions environment.
    let pr_env = [
        ("GITHUB_EVENT_NAME", "pull_request"),
        ("GITHUB_BASE_REF", "master"),
    ];
    stdout(&w.swl(&w.consumer, &["check"], &pr_env));

    // A closed MR, an unknown repo name, a missing input.
    w.api.set("swan-lake-common", 1, "closed", &head, None);
    failed(&w.swl(&w.consumer, &mr, &[]), 1, "closed without merging");
    w.api.set("swan-lake-common", 1, "open", &head, None);
    fs::write(
        w.consumer.join("swl.toml"),
        "[inputs.lib]\nrepo = \"zwanenzang\"\nmr = 1\n[inputs.nope]\nrepo = \"swan-lake-common\"\nmr = 1\n",
    )
    .unwrap();
    let out = w.swl(&w.consumer, &mr, &[]);
    failed(&out, 1, "not the repo \"zwanenzang\"");
    failed(&out, 1, "nope: neither in flake.lock nor declared");
    fs::write(
        w.consumer.join("swl.toml"),
        "[inputs.lib]\nrepo = \"x\"\nmr = 1\nbranch = \"b\"\n",
    )
    .unwrap();
    failed(&w.swl(&w.consumer, &mr, &[]), 1, "unknown field");

    // An input new in flake.nix, not locked yet, is known by its URL.
    let flake = fs::read_to_string(w.consumer.join("flake.nix")).unwrap();
    let new_input = format!(
        "  inputs.newin.url = \"git+file://{}/binarin/swan-lake-common?ref=current\";\n",
        w.forge.display()
    );
    fs::write(
        w.consumer.join("flake.nix"),
        flake.replacen("{\n", &format!("{{\n{new_input}"), 1),
    )
    .unwrap();
    fs::write(
        w.consumer.join("swl.toml"),
        "[inputs.newin]\nrepo = \"swan-lake-common\"\nmr = 1\n",
    )
    .unwrap();
    w.git(&w.consumer, &["commit", "-qam", "declare newin"]);
    stdout(&w.swl(&w.consumer, &mr, &[]));
    fs::write(
        w.consumer.join("swl.toml"),
        "[inputs.newin]\nrepo = \"zwanenzang\"\nmr = 1\n",
    )
    .unwrap();
    failed(
        &w.swl(&w.consumer, &mr, &[]),
        1,
        "not the repo \"zwanenzang\"",
    );
    w.git(&w.consumer, &["reset", "-q", "--hard", "HEAD~1"]);
    w.git(&w.consumer, &["checkout", "swl.toml"]);

    // An MR must not change flake.lock.
    let lock = fs::read_to_string(w.consumer.join("flake.lock")).unwrap();
    fs::write(
        w.consumer.join("flake.lock"),
        lock.replace("\"version\": 7", "\"version\":  7"),
    )
    .unwrap();
    w.git(&w.consumer, &["commit", "-qam", "touch the lock"]);
    failed(
        &w.swl(&w.consumer, &mr, &[]),
        1,
        "must not change flake.lock",
    );
    w.git(&w.consumer, &["reset", "-q", "--hard", "HEAD~1"]);

    // Master: no swl.toml, and the pin policy, which has no common input here.
    let master = ["check", "--event", "master"];
    failed(
        &w.swl(&w.consumer, &master, &[]),
        1,
        "master has a swl.toml",
    );
    let push_env = [
        ("GITHUB_EVENT_NAME", "push"),
        ("GITHUB_REF", "refs/heads/master"),
    ];
    failed(
        &w.swl(&w.consumer, &["check"], &push_env),
        1,
        "master has a swl.toml",
    );
    w.git(&w.consumer, &["rm", "-q", "swl.toml"]);
    let out = stdout(&w.swl(&w.consumer, &master, &[]));
    assert!(
        out.contains("no common input: the pin policy does not apply"),
        "{out}"
    );
    assert!(out.contains("master checks passed"), "{out}");
    failed(&w.swl(&w.consumer, &["check"], &[]), 2, "pass --event");
}

#[test]
fn finalize_relocks_landed_entries_in_one_commit() {
    let w = World::new("finalize");
    let head = w.published();
    // A second entry whose MR is still open stays in the manifest.
    fs::write(
        w.consumer.join("swl.toml"),
        "[inputs.lib]\nrepo = \"swan-lake-common\"\nmr = 1\n\n[inputs.extra]\nrepo = \"swan-lake-common\"\nmr = 2\n",
    )
    .unwrap();
    w.api.set("swan-lake-common", 2, "open", &head, None);
    w.git(&w.consumer, &["commit", "-qam", "swl: manifest"]);
    let before = w.git(&w.consumer, &["rev-parse", "HEAD"]);
    let lock_before = fs::read(w.consumer.join("flake.lock")).unwrap();

    // Open: nothing lands.
    let out = w.swl(&w.consumer, &["finalize"], &[]);
    failed(
        &out,
        1,
        "lib (swan-lake-common#1): stays: the MR is still open",
    );

    // Merged on the forge's master, but not promoted to `current` yet.
    let forge = w.forge_repo("swan-lake-common");
    w.git(&forge, &["update-ref", "refs/heads/master", &head]);
    w.api
        .set("swan-lake-common", 1, "merged", &head, Some(&head));
    let out = w.swl(&w.consumer, &["finalize", "--wait", "--timeout", "0"], &[]);
    failed(&out, 1, "promotion or the mirror is pending");
    assert_eq!(w.git(&w.consumer, &["rev-parse", "HEAD"]), before);
    assert_eq!(
        fs::read(w.consumer.join("flake.lock")).unwrap(),
        lock_before
    );

    // Uncommitted changes to the lock or manifest are refused.
    fs::write(w.consumer.join("swl.toml"), "").unwrap();
    failed(
        &w.swl(&w.consumer, &["finalize"], &[]),
        2,
        "commit or discard",
    );
    w.git(&w.consumer, &["checkout", "swl.toml"]);

    // Promoted while `--wait` polls: one commit relocks lib and drops its
    // entry.
    let out = thread::scope(|scope| {
        scope.spawn(|| {
            thread::sleep(std::time::Duration::from_secs(2));
            w.git(&forge, &["update-ref", "refs/heads/current", &head]);
        });
        let wait = ["finalize", "--wait", "--timeout", "120", "--interval", "1"];
        w.swl(&w.consumer, &wait, &[])
    });
    let out = stdout(&out);
    assert!(
        out.contains("not promoted yet; checking again in 1s"),
        "{out}"
    );
    assert!(out.contains("lib (swan-lake-common#1): lands"), "{out}");
    assert!(
        out.contains("extra (swan-lake-common#2): stays: the MR is still open"),
        "{out}"
    );
    assert!(out.contains("1 entry remain"), "{out}");
    assert!(out.contains("Keep this commit local"), "{out}");
    assert_eq!(w.git(&w.consumer, &["rev-parse", "HEAD~1"]), before);
    assert_eq!(
        w.git(&w.consumer, &["log", "-1", "--format=%s"]),
        "swl: land swan-lake-common#1 (lib)"
    );
    assert_eq!(
        w.git(&w.consumer, &["show", "--format=", "--name-only", "HEAD"]),
        "flake.lock\nswl.toml"
    );
    assert_eq!(w.git(&w.consumer, &["status", "--porcelain"]), "");
    let lock: serde_json::Value =
        serde_json::from_slice(&fs::read(w.consumer.join("flake.lock")).unwrap()).unwrap();
    assert_eq!(lock["nodes"]["lib"]["locked"]["rev"], head.as_str());
    assert_eq!(lock["nodes"]["lib"]["original"]["ref"], "current");
    // Only lib was relocked: extra, at the same repo and ref, is unchanged.
    let old: serde_json::Value = serde_json::from_slice(&lock_before).unwrap();
    assert_eq!(lock["nodes"]["extra"], old["nodes"]["extra"]);
    let manifest = fs::read_to_string(w.consumer.join("swl.toml")).unwrap();
    assert!(!manifest.contains("[inputs.lib]") && manifest.contains("[inputs.extra]"));
    assert_eq!(w.eval_value(&w.base_path), "consumer-2+c2");

    // Nothing else has landed: no commit.
    let after = w.git(&w.consumer, &["rev-parse", "HEAD"]);
    failed(
        &w.swl(&w.consumer, &["finalize"], &[]),
        1,
        "nothing has landed",
    );
    assert_eq!(w.git(&w.consumer, &["rev-parse", "HEAD"]), after);

    // The last entry lands; the file goes away and the follow-up is the
    // push to master.
    w.api
        .set("swan-lake-common", 2, "merged", &head, Some(&head));
    let out = stdout(&w.swl(&w.consumer, &["finalize"], &[]));
    assert!(out.contains("git push origin HEAD:master"), "{out}");
    assert!(out.contains("git push origin --delete feat"), "{out}");
    assert!(!w.consumer.join("swl.toml").exists());
    assert_eq!(
        w.git(&w.consumer, &["show", "--format=", "--name-only", "HEAD"]),
        "flake.lock\nswl.toml"
    );
    let lock: serde_json::Value =
        serde_json::from_slice(&fs::read(w.consumer.join("flake.lock")).unwrap()).unwrap();
    assert_eq!(lock["nodes"]["extra"]["locked"]["rev"], head.as_str());
    let out = stdout(&w.swl(&w.consumer, &["finalize"], &[]));
    assert!(out.contains("nothing to finalize"), "{out}");

    // An empty swl.toml is not landed: it should be removed.
    fs::write(w.consumer.join("swl.toml"), "").unwrap();
    w.git(&w.consumer, &["add", "swl.toml"]);
    w.git(&w.consumer, &["commit", "-qm", "empty manifest"]);
    failed(&w.swl(&w.consumer, &["finalize"], &[]), 1, "has no entries");
}

#[test]
fn publish_refuses_the_base_branch_and_lock_changes() {
    let w = World::new("publish-refusals");
    let ws = w.ws.display().to_string();
    let publish = ["publish", "--workspace", &ws, "swan-lake"];
    let forge_common = w.forge_repo("swan-lake-common");
    let common_master = w.git(&forge_common, &["rev-parse", "master"]);
    let consumer_master = w.git(&w.forge_repo("swan-lake"), &["rev-parse", "master"]);

    // A shared checkout on its base branch, with a local commit.
    w.git(&w.common, &["checkout", "-q", "master"]);
    fs::write(w.common.join("value.txt"), "on master").unwrap();
    w.git(&w.common, &["commit", "-qam", "on master"]);
    failed(
        &w.swl(&w.root, &publish, &[]),
        2,
        "master is the base branch",
    );
    w.git(&w.common, &["checkout", "-q", "feature"]);

    // The consumer on its base branch, with a local commit.
    w.git(&w.consumer, &["checkout", "-q", "master"]);
    fs::write(w.consumer.join("value.txt"), "on master").unwrap();
    w.git(&w.consumer, &["commit", "-qam", "on master"]);
    failed(
        &w.swl(&w.root, &publish, &[]),
        2,
        "master is the base branch",
    );
    w.git(&w.consumer, &["checkout", "-q", "feat"]);

    // A consumer branch that changes flake.lock, such as a local partial
    // finalize commit.
    let lock = fs::read_to_string(w.consumer.join("flake.lock")).unwrap();
    fs::write(
        w.consumer.join("flake.lock"),
        lock.replace("\"version\": 7", "\"version\":  7"),
    )
    .unwrap();
    w.git(&w.consumer, &["commit", "-qam", "touch the lock"]);
    failed(
        &w.swl(&w.root, &publish, &[]),
        2,
        "must not change flake.lock",
    );

    // Nothing was pushed and no MR opened.
    assert_eq!(
        w.git(&forge_common, &["rev-parse", "master"]),
        common_master
    );
    assert_eq!(
        w.git(&w.forge_repo("swan-lake"), &["rev-parse", "master"]),
        consumer_master
    );
    let branches = w.git(&forge_common, &["branch", "--format=%(refname:short)"]);
    assert!(!branches.lines().any(|b| b == "feature"), "{branches}");
    assert!(!w.fj_log().contains("pr create"), "{}", w.fj_log());
}

#[test]
fn finalize_lands_a_nested_entry_through_its_parent() {
    let w = World::new("nested");
    let zw_forge = w.forge_repo("zwanenzang");
    let common_forge = w.forge_repo("swan-lake-common");
    fs::write(
        w.consumer.join("swl.toml"),
        "[inputs.\"lib/zwanenzang\"]\nrepo = \"zwanenzang\"\nmr = 1\n",
    )
    .unwrap();
    w.git(&w.consumer, &["add", "swl.toml"]);
    w.git(&w.consumer, &["commit", "-qm", "swl: manifest"]);

    // The zwanenzang MR, merged and promoted in zwanenzang.
    let zw = w.root.join("zw-work");
    w.git(
        &w.root,
        &["clone", "-q", &zw_forge.display().to_string(), "zw-work"],
    );
    fs::write(zw.join("value.txt"), "z2").unwrap();
    w.git(&zw, &["commit", "-qam", "zw: z2"]);
    let z2 = w.git(&zw, &["rev-parse", "HEAD"]);
    w.git(
        &zw,
        &["push", "-q", "origin", "HEAD:master", "HEAD:current"],
    );
    w.api.set("zwanenzang", 1, "open", &z2, None);
    let mr = ["check", "--event", "mr", "--base", "origin/master"];
    stdout(&w.swl(&w.consumer, &mr, &[]));
    w.api.set("zwanenzang", 1, "merged", &z2, Some(&z2));

    // Common has not bumped it yet: nothing lands.
    let out = w.swl(&w.consumer, &["finalize"], &[]);
    failed(&out, 1, "promotion or the mirror is pending");

    // Common bumps and promotes it: the consumer relocks lib.
    let common = w.root.join("common-work");
    w.git(
        &w.root,
        &[
            "clone",
            "-q",
            &common_forge.display().to_string(),
            "common-work",
        ],
    );
    stdout(&w.nix(&common, &["flake", "update", "zwanenzang"]));
    w.git(&common, &["commit", "-qam", "zwanenzang: bump"]);
    w.git(
        &common,
        &["push", "-q", "origin", "HEAD:master", "HEAD:current"],
    );
    let out = stdout(&w.swl(&w.consumer, &["finalize"], &[]));
    assert!(
        out.contains("lib/zwanenzang (zwanenzang#1): lands"),
        "{out}"
    );
    assert!(!w.consumer.join("swl.toml").exists());
    let lock: serde_json::Value =
        serde_json::from_slice(&fs::read(w.consumer.join("flake.lock")).unwrap()).unwrap();
    let lib_zw = lock["nodes"]["lib"]["inputs"]["zwanenzang"]
        .as_str()
        .unwrap();
    assert_eq!(lock["nodes"][lib_zw]["locked"]["rev"], z2.as_str());
    let mut c = Command::new("nix");
    w.env(&mut c, &w.base_path);
    let out = c
        .current_dir(&w.consumer)
        .args(["eval", "--raw", ".#zw"])
        .output()
        .unwrap();
    assert_eq!(stdout(&out), "z2");
}

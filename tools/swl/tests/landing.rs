//! The manifest workflow on disposable local repos: `swl publish`, `swl
//! check`, `swl ci env` and `swl finalize` against the fake forge API of the
//! workspace tests (a local HTTP server, which publish reaches with a fake fj
//! login) and a "forge" of bare repos reached by `git+file:`. Offline, with
//! real git and Nix; set SWL_TEST_NIX_STORE to use a store in a scratch
//! directory, as the package check does inside the build sandbox.

mod support;

use std::{
    env, fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Output},
    thread,
};

use support::{FakeApi, TOKEN, write_test_gitconfig};

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
/// common pin policy, which wants the real GitHub URL, does not apply. Its
/// `zwanenzang` follows lib's, as swan-lake's nixpkgs follows common's.
const CONSUMER_FLAKE: &str = r#"{
  inputs.lib.url = "git+file://@FORGE@/binarin/swan-lake-common?ref=current";
  inputs.extra.url = "git+file://@FORGE@/binarin/swan-lake-common?ref=current";
  inputs.zwanenzang.follows = "lib/zwanenzang";
  outputs = { self, lib, extra, ... }: {
    value = builtins.readFile ./value.txt + "+" + lib.value;
    extra = extra.value;
    zw = lib.zw;
  };
}
"#;

/// An `fj` that only logs its calls, and fails: swl must never run it.
const FAKE_FJ: &str = "#!/bin/sh\necho \"$*\" >> \"$FJ_LOG\"\nexit 3\n";

/// The MRs that check, ci env and finalize read, on the fake forge API of
/// the workspace tests.
trait SetMr {
    fn set(&self, repo: &str, n: u64, state: &str, head: &str, merge_commit: Option<&str>);
}

impl SetMr for FakeApi {
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
    /// What the fake `fj` was called with, which must stay empty.
    fj_log: PathBuf,
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
        write_test_gitconfig(&root.join("home"));
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
            fj_log: root.join("fj.log"),
            ws,
            api: FakeApi::start(),
            base_path,
            nix_config,
            root,
        };
        *w.api.forge.lock().unwrap() = Some(w.forge.clone());
        fs::write(
            w.root.join("fj-keys.json"),
            serde_json::json!({ "hosts": { "forgejo.lynx-lizard.ts.net": {
                "type": "Application", "name": "u", "token": TOKEN } } })
            .to_string(),
        )
        .unwrap();
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
            "SWL_GITHUB_TOKEN",
            "SWL_LOG",
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
            // An fj login, which check, ci env and finalize must not use.
            .env("SWL_FJ_KEYS", self.root.join("fj-keys.json"))
            .env("FJ_LOG", &self.fj_log)
            .env("SWL_FORGE_API", self.api.base())
            .env("SWL_FORGE_GIT", format!("file://{}", self.forge.display()))
            // No GitHub source here: nothing may ask the real API.
            .env("SWL_GITHUB_API", "http://127.0.0.1:9")
            // The forge's repos are file:// URLs.
            .env("SWL_ALLOW_FILE_URLS", "1");
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

    /// swl with the fake fj, which it must not run, first on PATH.
    fn swl(&self, dir: &Path, args: &[&str], extra_env: &[(&str, &str)]) -> Output {
        let mut c = Command::new(env!("CARGO_BIN_EXE_swl"));
        let path = format!("{}:{}", self.root.join("fakebin").display(), self.base_path);
        self.env(&mut c, &path);
        for (k, v) in extra_env {
            c.env(k, v);
        }
        c.current_dir(dir).args(args).output().unwrap()
    }

    /// The MR `repo#n` as the fake forge API has it.
    fn mr(&self, repo: &str, n: u64) -> serde_json::Value {
        let path = format!("/api/v1/repos/binarin/{repo}/pulls/{n}");
        let body = self.api.responses.lock().unwrap().get(&path).cloned();
        serde_json::from_str(&body.unwrap_or_else(|| panic!("no MR {repo}#{n}"))).unwrap()
    }

    /// The number of requests to open an MR.
    fn creates(&self) -> usize {
        self.api
            .writes()
            .iter()
            .filter(|w| w.starts_with("POST ") && w.split(' ').nth(1).unwrap().ends_with("/pulls"))
            .count()
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
        // check, ci env and finalize read anonymously, also with an fj login;
        // publish lists the open MRs (`pulls?state=open`), looks up a
        // branch's MR (`pulls/<base>/<branch>`) and opens MRs with it. No
        // command runs fj.
        if !thread::panicking() {
            let authed: Vec<String> = self
                .api
                .requests
                .lock()
                .unwrap()
                .iter()
                .filter(|r| r.auth.is_some())
                .filter(|r| {
                    !(r.method == "POST" && r.path.ends_with("/pulls")
                        || r.method == "GET"
                            && (r.path.contains("/pulls?state=open")
                                || r.path.contains("/pulls/master/")))
                })
                .map(|r| format!("{} {}", r.method, r.path))
                .collect();
            assert!(authed.is_empty(), "requests with a token: {authed:?}");
            let fj = fs::read_to_string(&self.fj_log).unwrap_or_default();
            assert!(fj.is_empty(), "fj was run: {fj}");
        }
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
    let mr = w.mr("swan-lake-common", 1);
    assert_eq!(
        (&mr["state"], &mr["head"]["ref"], &mr["base"]["ref"]),
        (&"open".into(), &"feature".into(), &"master".into())
    );
    assert_eq!(
        (&mr["title"], &mr["draft"]),
        (&"WIP: common: c2".into(), &true.into())
    );
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
    let mr = w.mr("swan-lake", 1);
    assert_eq!(
        (
            &mr["head"]["ref"],
            &mr["base"]["ref"],
            &mr["title"],
            &mr["draft"]
        ),
        (
            &"feat".into(),
            &"master".into(),
            &"consumer: use c2".into(),
            &false.into()
        )
    );
    let body = mr["body"].as_str().unwrap();
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
    assert_eq!(w.creates(), 2);
    let log = stdout(&w.swl(&w.root, &publish, &[]));
    assert_eq!(w.creates(), 2, "{:?}", w.api.writes());
    assert_eq!(w.git(&w.consumer, &["rev-parse", "HEAD"]), consumer_head);
    assert_eq!(
        log,
        "\
swl publish: swan-lake-common@lib
├─ feature is pushed
├─ MR swan-lake-common#1 is open for feature
╵
swl publish: swan-lake
├─ swl.toml is current
├─ feat is pushed
└─ MR swan-lake#1 is open for feat"
    );

    // A new local commit on top publishes fast-forward; the MR stays.
    fs::write(w.common.join("value.txt"), "c3").unwrap();
    w.git(&w.common, &["commit", "-qam", "common: c3"]);
    stdout(&w.swl(&w.root, &publish, &[]));
    assert_eq!(
        w.git(&forge_common, &["rev-parse", "feature"]),
        w.git(&w.common, &["rev-parse", "HEAD"])
    );
    assert_eq!(w.creates(), 2);
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
    assert_eq!(w.creates(), 2);
}

/// An open MR that swl cannot read, as fj could not read swan-lake#53: no
/// head sha, and null lists.
fn set_malformed(api: &FakeApi, repo: &str, n: u64, head_ref: &str) {
    let body = serde_json::json!({
        "number": n,
        "state": "open",
        "merged": false,
        "title": "unreadable",
        "head": { "sha": null, "ref": head_ref, "label": head_ref },
        "base": { "ref": "master" },
        "labels": null,
        "assignees": null,
        "merge_commit_sha": null,
    });
    api.responses.lock().unwrap().insert(
        format!("/api/v1/repos/binarin/{repo}/pulls/{n}"),
        body.to_string(),
    );
}

#[test]
fn publish_finds_and_opens_its_mrs_beside_an_open_mr_it_cannot_read() {
    let w = World::new("publish-malformed");
    let ws = w.ws.display().to_string();
    let publish = ["publish", "--workspace", &ws, "swan-lake"];
    for repo in ["swan-lake-common", "swan-lake"] {
        set_malformed(&w.api, repo, 1, "sl-0053-other");
    }
    let log = stdout(&w.swl(&w.root, &publish, &[]));
    for line in [
        "├─ opened MR swan-lake-common#2: WIP: common: c2",
        "└─ opened MR swan-lake#2: consumer: use c2",
    ] {
        assert!(log.lines().any(|l| l == line), "missing {line:?} in\n{log}");
    }
    assert_eq!(w.mr("swan-lake-common", 2)["head"]["ref"], "feature");
    assert_eq!(w.mr("swan-lake", 2)["head"]["ref"], "feat");
    let log = stdout(&w.swl(&w.root, &publish, &[]));
    for line in [
        "├─ MR swan-lake-common#2 is open for feature",
        "└─ MR swan-lake#2 is open for feat",
    ] {
        assert!(log.lines().any(|l| l == line), "missing {line:?} in\n{log}");
    }
    assert_eq!(w.creates(), 2, "{:?}", w.api.writes());

    // An open MR of the branch into another base is not its MR.
    w.git(&w.consumer, &["switch", "-qc", "feat2"]);
    let mut other_base = w.mr("swan-lake", 2);
    other_base["number"] = 3.into();
    other_base["head"]["ref"] = "feat2".into();
    other_base["base"]["ref"] = "release".into();
    w.api.responses.lock().unwrap().insert(
        "/api/v1/repos/binarin/swan-lake/pulls/3".into(),
        other_base.to_string(),
    );
    let log = stdout(&w.swl(&w.root, &publish, &[]));
    assert!(
        log.lines()
            .any(|l| l == "└─ opened MR swan-lake#4: consumer: use c2"),
        "{log}"
    );
    assert_eq!(w.mr("swan-lake", 4)["base"]["ref"], "master");
}

#[test]
fn publish_takes_the_open_mr_the_forge_has_when_it_refuses_to_open_one() {
    let w = World::new("publish-exists");
    let ws = w.ws.display().to_string();
    let publish = ["publish", "--workspace", &ws, "swan-lake"];
    // Common's MR, opened meanwhile, is not in the list yet: the forge
    // refuses a second one (409), and the list has it then.
    w.git(&w.common, &["push", "-q", "origin", "feature"]);
    let head = w.git(&w.common, &["rev-parse", "HEAD"]);
    w.api
        .set_pr("swan-lake-common", 7, "draft", "feature", &head, None);
    w.api.unlist_until_open("swan-lake-common", 7);
    // The consumer's own MR is one that swl cannot read: skipped, with a
    // warning, and the forge refuses a second one.
    set_malformed(&w.api, "swan-lake", 5, "feat");
    let out = w.swl(&w.root, &publish, &[]);
    failed(
        &out,
        2,
        "swan-lake: the forge has an MR of feat into master already, but not among the open \
         MRs it lists",
    );
    let log = text(&out);
    assert!(
        log.contains(
            "swl publish: swan-lake-common@lib\n├─ feature is pushed\n├─ MR swan-lake-common#7 is \
             open for feature\n"
        ),
        "{log}"
    );
    assert!(
        log.contains(
            "swl: warning: skipping swan-lake#5, an open MR that swl cannot read: MR 5 has no \
             head sha"
        ),
        "{log}"
    );
    assert!(log.contains("HTTP 409"), "{log}");
    // The manifest names the MR the forge had.
    assert!(
        fs::read_to_string(w.consumer.join("swl.toml"))
            .unwrap()
            .contains("mr = 7"),
        "{log}"
    );
    assert_eq!(w.creates(), 2, "{:?}", w.api.writes());
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
    // The partial landing passes the MR's check by rule (i).
    let check = ["check", "--event", "mr", "--base", "origin/master"];
    let out_check = stdout(&w.swl(&w.consumer, &check, &[]));
    assert!(
        out_check.contains("lib: swan-lake-common#1 lands: merged as"),
        "{out_check}"
    );
    assert!(out_check.contains("MR checks passed"), "{out_check}");
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
    assert!(out.contains("check accepts the landing"), "{out}");
    assert!(!w.consumer.join("swl.toml").exists());
    let out_check = stdout(&w.swl(&w.consumer, &check, &[]));
    assert!(
        out_check.contains("extra: swan-lake-common#2 lands"),
        "{out_check}"
    );
    // An entry dropped unmerged leaves its input to a forward move.
    w.api.set("swan-lake-common", 2, "open", &head, None);
    let out_check = stdout(&w.swl(&w.consumer, &check, &[]));
    assert!(
        out_check.contains(
            "extra: swan-lake-common#2 was removed from swl.toml but is open: not a landing"
        ),
        "{out_check}"
    );
    // Merged as a commit the new rev does not have: not landed.
    let elsewhere = "f".repeat(40);
    w.api
        .set("swan-lake-common", 2, "merged", &head, Some(&elsewhere));
    failed(
        &w.swl(&w.consumer, &check, &[]),
        1,
        "extra: swan-lake-common#2: its merge commit ffffffffffff is not in",
    );
    w.api
        .set("swan-lake-common", 2, "merged", &head, Some(&head));
    // The owner's tamper: a narHash below the landed input.
    let tampered = tamper_nested_hash(&w.consumer);
    w.git(&w.consumer, &["commit", "-qam", "tamper"]);
    failed(
        &w.swl(&w.consumer, &check, &[]),
        1,
        &format!(
            "flake.lock is not what nix locks for extra, lib at the new revisions: it differs at {tampered}"
        ),
    );
    w.git(&w.consumer, &["reset", "-q", "--hard", "HEAD~1"]);
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

/// Changes the narHash of the consumer lock's `lib/zwanenzang` node, below
/// a root input, as the owner's repro did; returns that input path.
fn tamper_nested_hash(consumer: &Path) -> String {
    let path = consumer.join("flake.lock");
    let mut lock: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    let key = lock["nodes"]["lib"]["inputs"]["zwanenzang"]
        .as_str()
        .unwrap()
        .to_string();
    lock["nodes"][&key]["locked"]["narHash"] =
        "sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=".into();
    fs::write(&path, serde_json::to_string_pretty(&lock).unwrap()).unwrap();
    "lib/zwanenzang (its locked)".into()
}

#[test]
fn check_accepts_forward_moves_and_refuses_backwards() {
    let w = World::new("forward");
    // `current` moves on: c2 on the forge, as a promotion would.
    w.git(&w.common, &["push", "-q", "origin", "HEAD:current"]);
    stdout(&w.nix(&w.consumer, &["flake", "update", "lib"]));
    w.git(&w.consumer, &["commit", "-qam", "lib: bump"]);
    let bump = w.git(&w.consumer, &["rev-parse", "HEAD"]);
    let check = ["check", "--event", "mr", "--base", "origin/master"];
    // A root input follows into the moved one, as in swan-lake.
    let lock: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(w.consumer.join("flake.lock")).unwrap()).unwrap();
    assert_eq!(
        lock["nodes"]["root"]["inputs"]["zwanenzang"],
        serde_json::json!(["lib", "zwanenzang"])
    );
    let out = stdout(&w.swl(&w.consumer, &check, &[]));
    assert!(out.contains("lib: forward move "), "{out}");
    assert!(out.contains("is in current of file://"), "{out}");

    // Back to the old rev, against the bump as the base: backwards.
    w.git(
        &w.consumer,
        &["checkout", "-q", "HEAD~1", "--", "flake.lock"],
    );
    w.git(&w.consumer, &["commit", "-qam", "lib: back"]);
    failed(
        &w.swl(
            &w.consumer,
            &["check", "--event", "mr", "--base", &bump],
            &[],
        ),
        1,
        "is not a forward move",
    );

    // The bump with a change to lib's declaration: separate MRs.
    w.git(&w.consumer, &["reset", "-q", "--hard", &bump]);
    let flake = fs::read_to_string(w.consumer.join("flake.nix")).unwrap();
    fs::write(
        w.consumer.join("flake.nix"),
        flake.replacen("{\n", "{\n  inputs.lib.flake = true;\n", 1),
    )
    .unwrap();
    w.git(&w.consumer, &["commit", "-qam", "lib: declare flake"]);
    failed(
        &w.swl(&w.consumer, &check, &[]),
        1,
        "flake.nix changes the declaration of lib; change the declaration and the lock in separate MRs",
    );
    w.git(&w.consumer, &["reset", "-q", "--hard", &bump]);

    // A nested node below the bumped input that nix would not lock so.
    w.git(&w.consumer, &["reset", "-q", "--hard", &bump]);
    let tampered = tamper_nested_hash(&w.consumer);
    w.git(&w.consumer, &["commit", "-qam", "tamper"]);
    failed(
        &w.swl(&w.consumer, &check, &[]),
        1,
        &format!("it differs at {tampered}"),
    );

    // A forward rev with another ref is another source.
    w.git(&w.consumer, &["reset", "-q", "--hard", &bump]);
    let side = w.root.join("side");
    w.git(
        &w.root,
        &[
            "clone",
            "-q",
            &w.forge_repo("swan-lake-common").display().to_string(),
            "side",
        ],
    );
    w.git(&side, &["checkout", "-q", "origin/current"]);
    fs::write(side.join("value.txt"), "c3").unwrap();
    w.git(&side, &["commit", "-qam", "c3"]);
    w.git(&side, &["push", "-q", "origin", "HEAD:refs/heads/side"]);
    let c3 = w.git(&side, &["rev-parse", "HEAD"]);
    let url = format!(
        "git+file://{}?ref=side",
        w.forge_repo("swan-lake-common").display()
    );
    stdout(&w.nix(
        &w.consumer,
        &["flake", "lock", "--override-input", "lib", &url],
    ));
    let lock = fs::read_to_string(w.consumer.join("flake.lock")).unwrap();
    assert!(lock.contains(&c3), "{lock}");
    w.git(&w.consumer, &["commit", "-qam", "lib: side"]);
    let out = w.swl(
        &w.consumer,
        &["check", "--event", "mr", "--base", &bump],
        &[],
    );
    failed(&out, 1, "lib: its locked source");

    // Forward on the same source, but a rev the declared branch lacks.
    w.git(&w.consumer, &["reset", "-q", "--hard", &bump]);
    let mut lock: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(w.consumer.join("flake.lock")).unwrap()).unwrap();
    lock["nodes"]["lib"]["locked"]["rev"] = c3.clone().into();
    fs::write(
        w.consumer.join("flake.lock"),
        serde_json::to_string_pretty(&lock).unwrap(),
    )
    .unwrap();
    w.git(&w.consumer, &["commit", "-qam", "lib: c3"]);
    let out = w.swl(
        &w.consumer,
        &["check", "--event", "mr", "--base", &bump],
        &[],
    );
    failed(&out, 1, "is not in current of file://");

    // An input added to the lock is not a move.
    w.git(&w.consumer, &["reset", "-q", "--hard", &bump]);
    let flake = fs::read_to_string(w.consumer.join("flake.nix")).unwrap();
    let new_input = format!(
        "  inputs.newin.url = \"git+file://{}/binarin/zwanenzang\";\n",
        w.forge.display()
    );
    fs::write(
        w.consumer.join("flake.nix"),
        flake.replacen("{\n", &format!("{{\n{new_input}"), 1),
    )
    .unwrap();
    stdout(&w.nix(&w.consumer, &["flake", "lock"]));
    w.git(&w.consumer, &["commit", "-qam", "newin"]);
    failed(
        &w.swl(
            &w.consumer,
            &["check", "--event", "mr", "--base", &bump],
            &[],
        ),
        1,
        "newin: the root input was added",
    );
}

/// `check` evaluates the MR's flake.nix in restricted mode: it reads
/// nothing outside the repo, and sees no environment.
#[test]
fn check_evaluates_flake_nix_restricted() {
    let w = World::new("restricted");
    let secret = w.root.join("secret.txt");
    fs::write(&secret, "git+file:///nowhere").unwrap();
    let flake = fs::read_to_string(w.consumer.join("flake.nix")).unwrap();
    fs::write(
        w.consumer.join("flake.nix"),
        flake.replacen(
            "{\n",
            &format!(
                "{{\n  inputs.newin.url = builtins.readFile {};\n",
                secret.display()
            ),
            1,
        ),
    )
    .unwrap();
    fs::write(
        w.consumer.join("swl.toml"),
        "[inputs.newin]\nrepo = \"swan-lake-common\"\nmr = 1\n",
    )
    .unwrap();
    w.git(&w.consumer, &["add", "-A"]);
    w.git(&w.consumer, &["commit", "-qm", "newin"]);
    failed(
        &w.swl(
            &w.consumer,
            &["check", "--event", "mr", "--base", "origin/master"],
            &[("SWL_GITHUB_TOKEN", "gh-secret")],
        ),
        2,
        "forbidden in restricted mode",
    );
}

/// A flake reference from the MR's flake.nix is parsed as a string, never
/// evaluated as Nix code.
#[test]
fn check_never_interpolates_a_flake_reference() {
    let w = World::new("interpolate");
    let flake = fs::read_to_string(w.consumer.join("flake.nix")).unwrap();
    fs::write(
        w.consumer.join("flake.nix"),
        flake.replacen(
            "{\n",
            "{\n  inputs.newin.url = \"github:binarin/x\\${builtins.toString 1}\";\n",
            1,
        ),
    )
    .unwrap();
    fs::write(
        w.consumer.join("swl.toml"),
        "[inputs.newin]\nrepo = \"x1\"\nmr = 1\n",
    )
    .unwrap();
    w.git(&w.consumer, &["add", "-A"]);
    w.git(&w.consumer, &["commit", "-qm", "newin"]);
    let out = w.swl(
        &w.consumer,
        &["check", "--event", "mr", "--base", "origin/master"],
        &[],
    );
    failed(
        &out,
        2,
        "parsing the flake reference \"github:binarin/x${builtins.toString 1}\"",
    );
    assert!(!text(&out).contains("github:binarin/x1"), "{}", text(&out));
}

/// The review's attack on a landing that adds an input: its `original` is
/// the entry's repo, its `locked` source another repo.
#[test]
fn check_refuses_an_added_input_locked_elsewhere() {
    let w = World::new("added-elsewhere");
    let zw = w.forge_repo("zwanenzang");
    let zw_head = w.git(&zw, &["rev-parse", "master"]);
    let flake = fs::read_to_string(w.consumer.join("flake.nix")).unwrap();
    fs::write(
        w.consumer.join("flake.nix"),
        flake.replacen(
            "{\n",
            &format!(
                "{{\n  inputs.zwnext.url = \"git+file://{}\";\n",
                zw.display()
            ),
            1,
        ),
    )
    .unwrap();
    stdout(&w.nix(&w.consumer, &["flake", "lock"]));
    // Locked to another repo of the forge, at a rev of its own.
    let path = w.consumer.join("flake.lock");
    let mut lock: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    let elsewhere = w.forge_repo("swan-lake-common");
    lock["nodes"]["zwnext"]["locked"]["url"] = format!("file://{}", elsewhere.display()).into();
    lock["nodes"]["zwnext"]["locked"]["rev"] = w.git(&elsewhere, &["rev-parse", "master"]).into();
    fs::write(&path, serde_json::to_string_pretty(&lock).unwrap()).unwrap();
    fs::write(
        w.consumer.join("swl.toml"),
        "[inputs.zwnext]\nrepo = \"zwanenzang\"\nmr = 5\n",
    )
    .unwrap();
    w.git(&w.consumer, &["add", "-A"]);
    w.git(&w.consumer, &["commit", "-qm", "zwnext"]);
    w.git(&w.consumer, &["rm", "-q", "swl.toml"]);
    w.git(&w.consumer, &["commit", "-qm", "landed, supposedly"]);
    w.api
        .set("zwanenzang", 5, "merged", &zw_head, Some(&zw_head));
    failed(
        &w.swl(
            &w.consumer,
            &["check", "--event", "mr", "--base", "origin/master"],
            &[],
        ),
        1,
        "zwnext: the new input is not the repo \"zwanenzang\"",
    );
}

#[test]
fn publish_refuses_the_base_branch_and_lock_changes() {
    let w = World::new("publish-refusals");
    let ws = w.ws.display().to_string();
    let publish = ["publish", "--workspace", &ws, "swan-lake"];
    let forge_common = w.forge_repo("swan-lake-common");
    let common_master = w.git(&forge_common, &["rev-parse", "master"]);
    let consumer_master = w.git(&w.forge_repo("swan-lake"), &["rev-parse", "master"]);

    // Without an fj login, refused before anything is pushed.
    let no_login = w.root.join("no-keys.json");
    failed(
        &w.swl(
            &w.root,
            &publish,
            &[("SWL_FJ_KEYS", no_login.to_str().unwrap())],
        ),
        2,
        "run fj auth login",
    );

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
    assert_eq!(w.creates(), 0, "{:?}", w.api.writes());
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

#[test]
fn publish_refuses_a_branch_whose_mr_merged_without_its_commits() {
    let w = World::new("publish-merged");
    let ws = w.ws.display().to_string();
    let publish = ["publish", "--workspace", &ws, "swan-lake"];
    // `repo`'s MR `n` from `branch`, merged as `merge_commit`; its branch is
    // gone. The fake API finds it by its `head.label`.
    let merged = |repo: &str, n: u64, branch: &str, merge_commit: &str| {
        let body = serde_json::json!({
            "number": n,
            "state": "closed",
            "merged": true,
            "title": "t",
            "head": { "sha": merge_commit, "ref": format!("refs/pull/{n}/head"), "label": branch },
            "base": { "ref": "master" },
            "merge_commit_sha": merge_commit,
        });
        w.api.responses.lock().unwrap().insert(
            format!("/api/v1/repos/binarin/{repo}/pulls/{n}"),
            body.to_string(),
        );
    };
    // Common's feature was merged as rebased elsewhere: master has its
    // change, not its commit.
    let elsewhere = w.root.join("elsewhere");
    let forge_common = w.forge_repo("swan-lake-common");
    w.git(
        &w.root,
        &[
            "clone",
            "-q",
            &forge_common.display().to_string(),
            &elsewhere.display().to_string(),
        ],
    );
    fs::write(elsewhere.join("other.txt"), "other").unwrap();
    w.git(&elsewhere, &["add", "other.txt"]);
    w.git(&elsewhere, &["commit", "-qm", "other"]);
    fs::write(elsewhere.join("value.txt"), "c2").unwrap();
    w.git(&elsewhere, &["commit", "-qam", "common: c2"]);
    w.git(&elsewhere, &["push", "-q", "origin", "HEAD:master"]);
    let rebased = w.git(&elsewhere, &["rev-parse", "HEAD"]);
    merged("swan-lake-common", 4, "feature", &rebased);
    let out = w.swl(&w.root, &publish, &[]);
    failed(
        &out,
        2,
        &format!(
            "swan-lake-common@lib: swan-lake-common#4, the MR of feature, is merged (as {}), and \
             feature is not on origin any more; HEAD has commits outside origin/master that the \
             merge does not have (as they were before a rebase), so publishing would open a new \
             MR for landed work: remove the workspace, or put new work on a new branch",
            &rebased[..12]
        ),
    );
    w.api.responses.lock().unwrap().clear();

    // Likewise the consumer's branch, merged as a later commit of master.
    let consumer_master = w.git(&w.forge_repo("swan-lake"), &["rev-parse", "master"]);
    let consumer_elsewhere = w.root.join("consumer-elsewhere");
    w.git(
        &w.root,
        &[
            "clone",
            "-q",
            &w.forge_repo("swan-lake").display().to_string(),
            &consumer_elsewhere.display().to_string(),
        ],
    );
    fs::write(consumer_elsewhere.join("other.txt"), "other").unwrap();
    w.git(&consumer_elsewhere, &["add", "other.txt"]);
    w.git(&consumer_elsewhere, &["commit", "-qm", "other"]);
    w.git(
        &consumer_elsewhere,
        &["push", "-q", "origin", "HEAD:master"],
    );
    let later = w.git(&consumer_elsewhere, &["rev-parse", "HEAD"]);
    merged("swan-lake", 5, "feat", &later);
    failed(
        &w.swl(&w.root, &publish, &[]),
        2,
        &format!(
            "swan-lake: swan-lake#5, the MR of feat, is merged (as {}), and feat is not on \
             origin any more",
            &later[..12]
        ),
    );
    // Nothing was pushed and no MR opened.
    for (repo, branch) in [("swan-lake-common", "feature"), ("swan-lake", "feat")] {
        let branches = w.git(
            &w.forge_repo(repo),
            &["branch", "--format=%(refname:short)"],
        );
        assert!(!branches.lines().any(|b| b == branch), "{repo}: {branches}");
    }
    assert_eq!(w.creates(), 0, "{:?}", w.api.writes());

    // A merged MR whose merge HEAD has: the branch is new work after it.
    merged("swan-lake", 5, "feat", &consumer_master);
    w.api
        .responses
        .lock()
        .unwrap()
        .remove("/api/v1/repos/binarin/swan-lake-common/pulls/4");
    w.git(&w.common, &["rebase", "-q", "origin/master"]);
    stdout(&w.swl(&w.root, &publish, &[]));
    assert_eq!(
        w.git(&w.forge_repo("swan-lake"), &["rev-parse", "feat"]),
        w.git(&w.consumer, &["rev-parse", "HEAD"])
    );

    // A branch whose HEAD is in the base has no commits that the merge
    // lacks: not refused, though its MR merged as a later commit of master.
    merged("swan-lake", 6, "at-base", &later);
    w.git(&w.consumer, &["switch", "-qc", "at-base", &consumer_master]);
    let out = stdout(&w.swl(&w.root, &publish, &[]));
    assert!(
        out.contains(
            "swl publish: swan-lake\n└─ HEAD is already in origin/master; no consumer MR to open"
        ),
        "{out}"
    );

    // A merge commit not known here: whether HEAD has it is unknown, a
    // warning, and the branch is published. (The branch's `#` is asked
    // for percent-encoded.)
    w.git(&w.consumer, &["switch", "-qc", "unknown#merge", "feat"]);
    merged("swan-lake", 7, "unknown#merge", &"1".repeat(40));
    let out = w.swl(&w.root, &publish, &[]);
    stdout(&out);
    assert!(
        String::from_utf8_lossy(&out.stderr).contains(
            "warning: swan-lake: swan-lake#7, the MR of unknown#merge, is merged as \
             111111111111, which is not known here: whether HEAD has it is unknown"
        ),
        "{}",
        text(&out)
    );
    assert_eq!(
        w.git(&w.forge_repo("swan-lake"), &["rev-parse", "unknown#merge"]),
        w.git(&w.consumer, &["rev-parse", "HEAD"])
    );
}

#[test]
fn publish_closes_its_section_before_an_error() {
    let w = World::new("publish-error");
    let ws = w.ws.display().to_string();
    // The forge refuses the consumer's push, which comes after common's
    // steps and the consumer's manifest.
    let hook = w.forge_repo("swan-lake").join("hooks/pre-receive");
    fs::write(
        &hook,
        "#!/bin/sh\necho 'push refused by the test' >&2\nexit 1\n",
    )
    .unwrap();
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
    let out = w.swl(&w.root, &["publish", "--workspace", &ws, "swan-lake"], &[]);
    failed(&out, 2, "push refused by the test");
    // The open section is closed on stdout; the error is on stderr only.
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.ends_with("swl publish: swan-lake\n├─ committed swl.toml: swan-lake-common#1\n╵\n"),
        "{}",
        text(&out)
    );
    assert!(!stdout.contains("push refused"), "{}", text(&out));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.starts_with("swl: git -C "), "{}", text(&out));
    assert!(!stderr.contains('╵'), "{}", text(&out));
}

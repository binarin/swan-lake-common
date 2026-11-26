//! A disposable three-level chain of tiny local flakes, zwanenzang ->
//! swan-lake-common -> swan-lake, driven through `swl ws add`, `swl env`,
//! the generated wrappers and real Nix. Offline: the flakes have no inputs
//! beyond each other. Set SWL_TEST_NIX_STORE to use a store in a scratch
//! directory, as the package check does inside the build sandbox.

use std::{
    env, fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Output},
};

const ZW_FLAKE: &str = r#"{
  outputs = { self }: {
    value = builtins.readFile ./value.txt;
    hasNew = builtins.pathExists ./new.txt;
  };
}
"#;

const COMMON_FLAKE: &str = r#"{
  inputs.zwanenzang.url = "git+file://@SRC@/zwanenzang";
  outputs = { self, zwanenzang }: {
    value = builtins.readFile ./value.txt + "+" + zwanenzang.value;
  };
}
"#;

/// Like the real consumers, a root input follows one of common's.
const CONSUMER_FLAKE: &str = r#"{
  inputs.common.url = "git+file://@SRC@/swan-lake-common";
  inputs.zwanenzang.follows = "common/zwanenzang";
  outputs = { self, common, zwanenzang }: {
    info = {
      value = builtins.readFile ./value.txt + "+" + common.value;
      viaFollows = zwanenzang.value;
      zwHasNew = zwanenzang.hasNew;
      commonHasNew = builtins.pathExists "${common}/new.txt";
    };
    selfPath = "${self}";
  };
}
"#;

struct Chain {
    root: PathBuf,
    src: PathBuf,
    ws: PathBuf,
    log: PathBuf,
    real_nix: PathBuf,
    /// Wrappers first, then the fake nh and deploy, then the caller's PATH.
    shim_path: String,
    base_path: String,
    nix_config: String,
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

impl Chain {
    fn new() -> Self {
        let root = env::temp_dir().join(format!("swl-chain-{}", std::process::id()));
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
        let fake = root.join("fakebin");
        let shim_path = format!(
            "{}:{}:{base_path}",
            ws.join(".swl/bin").display(),
            fake.display()
        );
        let chain = Self {
            src: root.join("src"),
            log: root.join("shim.log"),
            real_nix: find_on_path(&base_path, "nix"),
            ws,
            root,
            shim_path,
            base_path,
            nix_config,
        };
        fs::create_dir_all(&fake).unwrap();
        for tool in ["nh", "deploy"] {
            let p = fake.join(tool);
            let out = chain.root.join(format!("{tool}.args"));
            fs::write(
                &p,
                format!("#!/bin/sh\necho \"$@\" > '{}'\n", out.display()),
            )
            .unwrap();
            fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
        }
        fs::write(
            chain.root.join("repos.toml"),
            format!(
                "url_patterns = [\"git+file://{src}/{{repo}}\"]\n\
                 [repos.zwanenzang]\nsource = \"{src}/zwanenzang\"\nbase = \"master\"\n\
                 [repos.swan-lake-common]\nsource = \"{src}/swan-lake-common\"\nbase = \"master\"\n\
                 [repos.swan-lake]\nsource = \"{src}/swan-lake\"\nbase = \"master\"\n",
                src = chain.root.join("src").display()
            ),
        )
        .unwrap();
        chain
    }

    fn env(&self, c: &mut Command, path: &str) {
        let home = self.root.join("home");
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
            .env("SWL_SHIM_LOG", &self.log);
        // The text checks need swl's plain output, whatever the caller's
        // environment.
        for var in ["SWL_LOG", "NO_COLOR", "CLICOLOR", "CLICOLOR_FORCE"] {
            c.env_remove(var);
        }
    }

    fn git(&self, dir: &Path, args: &[&str]) -> String {
        let mut c = Command::new("git");
        self.env(&mut c, &self.base_path);
        stdout(&c.arg("-C").arg(dir).args(args).output().unwrap())
    }

    fn swl(&self, args: &[&str]) -> Output {
        let mut c = Command::new(env!("CARGO_BIN_EXE_swl"));
        self.env(&mut c, &self.base_path);
        c.current_dir(&self.ws).args(args).output().unwrap()
    }

    /// A tool through the workspace wrappers, found on PATH like in the
    /// dev shell. Without `SWL_CONFIG`: the wrappers carry the config.
    fn shimmed(&self, dir: &Path, tool: &str, args: &[&str]) -> Output {
        let mut c = Command::new(tool);
        self.env(&mut c, &self.shim_path);
        c.env_remove("SWL_CONFIG");
        c.current_dir(dir).args(args).output().unwrap()
    }

    fn real_nix(&self, dir: &Path, args: &[&str]) -> Output {
        let mut c = Command::new(&self.real_nix);
        self.env(&mut c, &self.base_path);
        c.current_dir(dir).args(args).output().unwrap()
    }

    fn source_repo(&self, name: &str, flake: &str, value: &str) -> PathBuf {
        let dir = self.src.join(name);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("flake.nix"),
            flake.replace("@SRC@", &self.src.display().to_string()),
        )
        .unwrap();
        fs::write(dir.join("value.txt"), value).unwrap();
        self.git(&dir, &["init", "-q", "-b", "master"]);
        self.git(&dir, &["add", "."]);
        self.git(&dir, &["commit", "-qm", "init"]);
        dir
    }

    fn lock_and_commit(&self, dir: &Path) {
        stdout(&self.real_nix(dir, &["flake", "lock"]));
        self.git(dir, &["add", "flake.lock"]);
        self.git(dir, &["commit", "-qm", "lock"]);
    }

    fn log_lines(&self) -> Vec<String> {
        let lines = fs::read_to_string(&self.log).unwrap_or_default();
        fs::remove_file(&self.log).ok();
        lines.lines().map(str::to_string).collect()
    }

    fn info(&self, out: &Output) -> serde_json::Value {
        serde_json::from_str(&stdout(out)).unwrap()
    }
}

fn refused(out: &Output) {
    assert_eq!(out.status.code(), Some(1), "{}", text(out));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("swl shim: refusing"),
        "{}",
        text(out)
    );
}

#[test]
fn three_level_chain_through_the_wrappers() {
    let t = Chain::new();
    t.source_repo("zwanenzang", ZW_FLAKE, "zw-1");
    let common_src = t.source_repo("swan-lake-common", COMMON_FLAKE, "common-1");
    t.lock_and_commit(&common_src);
    let consumer_src = t.source_repo("swan-lake", CONSUMER_FLAKE, "consumer-1");
    t.lock_and_commit(&consumer_src);

    // Checkouts.
    fs::create_dir_all(&t.ws).unwrap();
    stdout(&t.swl(&["ws", "add", "swan-lake"]));
    stdout(&t.swl(&[
        "ws",
        "add",
        "swan-lake-common@common",
        "--branch",
        "feature",
    ]));
    stdout(&t.swl(&["ws", "add", "zwanenzang@zwanenzang"]));
    assert!(
        !t.swl(&["ws", "add", "zwanenzang@zwanenzang"])
            .status
            .success()
    );
    assert!(!t.swl(&["ws", "add", "unknown@x"]).status.success());
    let consumer = t.ws.join("swan-lake");
    let common = t.ws.join("swan-lake-common@common");
    let zw = t.ws.join("zwanenzang@zwanenzang");
    assert_eq!(
        t.git(&common, &["symbolic-ref", "--short", "HEAD"]),
        "feature"
    );
    assert_eq!(
        t.git(
            &common,
            &["for-each-ref", "--format=%(upstream)", "refs/heads/feature"]
        ),
        ""
    );
    let lock_before = (
        fs::read(consumer.join("flake.lock")).unwrap(),
        fs::read(common.join("flake.lock")).unwrap(),
    );

    // A hand-written .envrc.local is kept unless --force.
    fs::write(consumer.join(".envrc.local"), "# mine\n").unwrap();
    let out = t.swl(&["env"]);
    assert_eq!(out.status.code(), Some(2), "{}", text(&out));
    assert_eq!(
        fs::read_to_string(consumer.join(".envrc.local")).unwrap(),
        "# mine\n"
    );
    assert!(stdout(&t.swl(&["env", "--force"])).contains(
        "swan-lake: common←swan-lake-common@common common/zwanenzang←zwanenzang@zwanenzang"
    ));
    let envrc = fs::read_to_string(consumer.join(".envrc.local")).unwrap();
    assert!(
        envrc.contains(&format!(
            "--override-input common git+file://{}\n",
            common.display()
        )),
        "{envrc}"
    );
    assert!(
        envrc.contains(&format!(
            "--override-input common/zwanenzang git+file://{}\n",
            zw.display()
        )),
        "{envrc}"
    );
    assert!(stdout(&t.swl(&["env"])).ends_with("(unchanged)"));
    let config_arg = format!("--config {}/repos.toml shim", t.root.display());
    for tool in ["nix", "nh", "deploy"] {
        let w = fs::read_to_string(t.ws.join(".swl/bin").join(tool)).unwrap();
        assert!(
            w.contains(&format!("{config_arg} {tool} --workspace")),
            "{w}"
        );
    }
    // A missing $SWL_CONFIG is an error that names it.
    let mut c = Command::new(env!("CARGO_BIN_EXE_swl"));
    t.env(&mut c, &t.base_path);
    let out = c
        .env("SWL_CONFIG", t.root.join("nope.toml"))
        .arg("status")
        .current_dir(&t.ws)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2), "{}", text(&out));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("nope.toml"),
        "{}",
        text(&out)
    );

    // Clean checkouts at the locked revs: the same result as the lock.
    let eval = ["eval", "--json", ".#info"];
    let locked = t.info(&t.real_nix(&consumer, &eval));
    assert_eq!(locked["value"], "consumer-1+common-1+zw-1");
    assert_eq!(t.info(&t.shimmed(&consumer, "nix", &eval)), locked);
    let log = t.log_lines();
    assert_eq!(log.len(), 1, "{log:?}");
    assert!(log[0].starts_with("nix override eval"), "{log:?}");

    // Tracked, uncommitted edits at every hop; the root zwanenzang follows.
    fs::write(zw.join("value.txt"), "zw-2").unwrap();
    fs::write(common.join("value.txt"), "common-2").unwrap();
    let info = t.info(&t.shimmed(&consumer, "nix", &eval));
    assert_eq!(info["value"], "consumer-1+common-2+zw-2");
    assert_eq!(info["viaFollows"], "zw-2");
    assert_eq!(t.info(&t.real_nix(&consumer, &eval)), locked);
    // A shared checkout as the target gets its own nested hop.
    assert_eq!(
        stdout(&t.shimmed(&common, "nix", &["eval", "--raw", ".#value"])),
        "common-2+zw-2"
    );

    // Untracked files are invisible until `git add -N`.
    fs::write(zw.join("new.txt"), "new").unwrap();
    fs::write(common.join("new.txt"), "new").unwrap();
    let info = t.info(&t.shimmed(&consumer, "nix", &eval));
    assert_eq!(
        (info["zwHasNew"].as_bool(), info["commonHasNew"].as_bool()),
        (Some(false), Some(false))
    );
    t.git(&zw, &["add", "-N", "new.txt"]);
    t.git(&common, &["add", "-N", "new.txt"]);
    let info = t.info(&t.shimmed(&consumer, "nix", &eval));
    assert_eq!(
        (info["zwHasNew"].as_bool(), info["commonHasNew"].as_bool()),
        (Some(true), Some(true))
    );

    // Overrides the caller already passes are not repeated.
    let explicit = [
        "eval",
        "--json",
        ".#info",
        "--override-input",
        "common",
        &format!("git+file://{}", common.display()),
        "--override-input",
        "common/zwanenzang",
        &format!("git+file://{}", zw.display()),
        "--no-write-lock-file",
    ];
    assert_eq!(
        t.info(&t.shimmed(&consumer, "nix", &explicit))["value"],
        "consumer-1+common-2+zw-2"
    );
    t.log_lines();

    // Store-path-only calls are passed through untouched.
    let self_path = stdout(&t.real_nix(&consumer, &["eval", "--raw", ".#selfPath"]));
    stdout(&t.shimmed(&consumer, "nix", &["path-info", &self_path]));
    assert_eq!(
        t.log_lines(),
        [format!("nix store-paths path-info {self_path}")]
    );
    // The shim adds nothing to the tool's stderr; with SWL_LOG, what it runs.
    let out = t.shimmed(&consumer, "nix", &["path-info", &self_path]);
    let real = t.real_nix(&consumer, &["path-info", &self_path]);
    assert_eq!(out.stderr, real.stderr, "{}", text(&out));
    let mut c = Command::new("nix");
    t.env(&mut c, &t.shim_path);
    let out = c
        .env("SWL_LOG", "debug")
        .current_dir(&consumer)
        .args(["path-info", &self_path])
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("exec ") && err.contains("path-info"), "{err}");
    t.log_lines();

    // Lock-writing commands and flags are refused while overrides apply.
    refused(&t.shimmed(&consumer, "nix", &["flake", "lock"]));
    refused(&t.shimmed(&consumer, "nix", &["flake", "update", "common"]));
    refused(&t.shimmed(&common, "nix", &["flake", "update"]));
    for flag in [
        &["--commit-lock-file"][..],
        &["--recreate-lock-file"],
        &["--update-input", "common"],
    ] {
        refused(&t.shimmed(&consumer, "nix", &[&eval[..], flag].concat()));
    }
    let out_lock = t.root.join("out.lock");
    let out_lock_arg = out_lock.display().to_string();
    refused(&t.shimmed(
        &consumer,
        "nix",
        &[&eval[..], &["--output-lock-file", &out_lock_arg]].concat(),
    ));
    assert!(!out_lock.exists());
    refused(&t.shimmed(&consumer, "nh", &["os", "build", "-U", "common", "."]));
    refused(&t.shimmed(&consumer, "nh", &["os", "switch", "--update"]));

    // nh and deploy otherwise pass through, past the wrapper directory.
    stdout(&t.shimmed(&consumer, "nh", &["os", "build", "-H", "focalor", "."]));
    assert_eq!(
        fs::read_to_string(t.root.join("nh.args")).unwrap(),
        "os build -H focalor .\n"
    );
    stdout(&t.shimmed(&consumer, "deploy", &[".#host", "--", "--commit-lock-file"]));
    assert_eq!(
        fs::read_to_string(t.root.join("deploy.args")).unwrap(),
        ".#host -- --commit-lock-file\n"
    );
    let log = t.log_lines();
    assert_eq!(
        log.iter().filter(|l| l.contains(" refuse ")).count(),
        9,
        "{log:?}"
    );

    // A flake outside the workspace gets no overrides and may lock.
    let outside = t.root.join("outside");
    t.git(
        &t.root,
        &[
            "clone",
            "-q",
            &consumer_src.display().to_string(),
            "outside",
        ],
    );
    assert_eq!(t.info(&t.shimmed(&outside, "nix", &eval)), locked);
    stdout(&t.shimmed(&outside, "nix", &["flake", "lock"]));
    let log = t.log_lines();
    assert!(
        log.iter().all(|l| l.starts_with("nix no-overrides")),
        "{log:?}"
    );

    // status: the consumer's tree, the nested input under its parent; per
    // shared checkout, the rev each flake in the workspace locks.
    let status = stdout(&t.swl(&["status"]));
    let consumer_rev = t.git(&consumer, &["rev-parse", "--short", "HEAD"]);
    let common_rev = t.git(&common_src, &["rev-parse", "--short", "HEAD"]);
    let zw_rev = t.git(&t.src.join("zwanenzang"), &["rev-parse", "--short", "HEAD"]);
    let publish = "next: commit the changes, then swl publish swan-lake";
    // After the rows, the running swl and the one the wrappers exec: this one.
    let version = stdout(&t.swl(&["--version"]));
    let swl = Path::new(env!("CARGO_BIN_EXE_swl")).canonicalize().unwrap();
    let swl = swl.display();
    assert_eq!(
        status,
        format!(
            "\
swan-lake                                     not a change (detached at master)  waits: nobody  next: -
│  HEAD {consumer_rev}  detached  clean
│  no pending swl.toml entries
└─ common: swan-lake-common@common            local                       waits: anyone  {publish}
   │  input common  HEAD {common_rev}  feature  dirty
   │  swan-lake: common locked {common_rev}, ahead 0, behind 0
   └─ common/zwanenzang: zwanenzang@zwanenzang  local                       waits: anyone  {publish}
         input zwanenzang  HEAD {zw_rev}  detached  dirty
         swan-lake: common/zwanenzang locked {zw_rev}, ahead 0, behind 0
         swan-lake-common@common: zwanenzang locked {zw_rev}, ahead 0, behind 0

no consumer checkout holds a change: none has commits of its own or a pushed branch (swan-lake: not a change (detached at master))

running swl: {version} at {swl}
wrappers' swl: {version} at {swl}"
        )
    );

    // No lock in the workspace was written.
    let lock_after = (
        fs::read(consumer.join("flake.lock")).unwrap(),
        fs::read(common.join("flake.lock")).unwrap(),
    );
    assert!(lock_before == lock_after, "a flake.lock changed");

    // Below the common override Nix uses the common checkout's lock: pin a
    // newer zwanenzang there than the consumer locks.
    let zw_src = t.src.join("zwanenzang");
    fs::write(zw_src.join("value.txt"), "zw-3").unwrap();
    t.git(&zw_src, &["commit", "-qam", "zw-3"]);
    let zw_new = t.git(&zw_src, &["rev-parse", "--short", "HEAD"]);
    stdout(&t.real_nix(&common, &["flake", "update", "zwanenzang"]));
    let status = stdout(&t.swl(&["status"]));
    for line in [
        format!("         swan-lake: common/zwanenzang locked {zw_rev}, ahead 0, behind 0"),
        format!("         swan-lake-common@common: zwanenzang locked {zw_new}, ahead 0, behind 1"),
    ] {
        assert!(
            status.lines().any(|l| l == line),
            "missing {line:?} in\n{status}"
        );
    }
    // The zwanenzang checkout still wins while it is in the workspace.
    assert_eq!(
        t.info(&t.shimmed(&consumer, "nix", &eval))["value"],
        "consumer-1+common-2+zw-2"
    );

    // Without it, a rerun of `env` drops its override and watch, and the
    // zwanenzang below common comes from the common checkout's lock.
    let out = Command::new("git")
        .arg("-C")
        .arg(&zw_src)
        .args(["worktree", "remove", "--force"])
        .arg(&zw)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(
        stdout(&t.swl(&["env"])),
        "swan-lake: common←swan-lake-common@common"
    );
    let envrc = fs::read_to_string(consumer.join(".envrc.local")).unwrap();
    assert!(!envrc.contains("zwanenzang"), "{envrc}");
    let info = t.info(&t.shimmed(&consumer, "nix", &eval));
    assert_eq!(info["value"], "consumer-1+common-2+zw-3");
    assert_eq!(info["viaFollows"], "zw-3");
    assert_eq!(t.info(&t.real_nix(&consumer, &eval)), locked);

    fs::remove_dir_all(&t.root).ok();
}

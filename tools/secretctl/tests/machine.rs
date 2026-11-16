//! `secretctl machine add` and the machine consistency check of `encrypt`,
//! against real `sops`, `age`, `age-keygen`, `ssh-keygen` and `git` with
//! throwaway age identities in a `git init` tree.
use std::os::unix::fs::{PermissionsExt, symlink};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    sync::atomic::{AtomicUsize, Ordering},
};

use serde_json::Value;

static NEXT: AtomicUsize = AtomicUsize::new(0);

/// The five artefacts of machine `m`, relative to the fixture.
const ARTEFACTS: [&str; 5] = [
    "secrets/keys/machines/m.age-key",
    "secrets/keys/machines/m.ssh_host_ed25519-key.public",
    "secrets/values/machines/m/m.age-key",
    "secrets/values/machines/m/openssh/ssh_host_ed25519_key",
    "secrets/values/roots/m.private",
];

#[derive(Debug, PartialEq)]
enum Entry {
    Dir,
    Link(PathBuf),
    File(Vec<u8>),
}

struct Fixture(PathBuf);
impl Fixture {
    /// A committed Git worktree whose `secrets/values` grants admins `a` and `b`.
    fn new() -> (Self, String, String) {
        let f = Self::bare();
        git(&f.0, &["init", "-q"]);
        let a = f.age_key("a");
        let b = f.age_key("b");
        f.grant("values", "a");
        f.grant("values", "b");
        f.put(".gitignore", b"/identities/\n/home/\n/bin/\n/*.log\n");
        git(&f.0, &["add", "."]);
        git(&f.0, &["commit", "-qm", "init"]);
        (f, a, b)
    }
    /// A directory that is not a Git worktree.
    fn bare() -> Self {
        let path = std::env::temp_dir().join(format!(
            "secretctl-machine-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(path.join("secrets/values")).unwrap();
        fs::create_dir_all(path.join("home")).unwrap();
        Self(path)
    }
    fn p(&self, relative: &str) -> PathBuf {
        self.0.join(relative)
    }
    fn put(&self, relative: &str, bytes: &[u8]) {
        let p = self.p(relative);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, bytes).unwrap();
    }
    fn link(&self, to: &str, relative: &str) {
        let p = self.p(relative);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        symlink(to, p).unwrap();
    }
    /// Generates identity `identities/<name>.identity` and its recipient
    /// marker target `secrets/keys/<name>.age-key`; returns the recipient.
    fn age_key(&self, name: &str) -> String {
        let identity = self.identity(name);
        fs::create_dir_all(identity.parent().unwrap()).unwrap();
        let out = Command::new("age-keygen")
            .arg("-o")
            .arg(&identity)
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", text(&out));
        let recipient = public_age(&fs::read(&identity).unwrap());
        self.put(
            &format!("secrets/keys/{name}.age-key"),
            format!("{recipient}\n").as_bytes(),
        );
        recipient
    }
    fn identity(&self, name: &str) -> PathBuf {
        self.p(&format!("identities/{name}.identity"))
    }
    /// Marker `<dir>/<name>.age-key` pointing at `secrets/keys/<name>.age-key`.
    fn grant(&self, dir: &str, name: &str) {
        let depth = dir.split('/').count();
        self.link(
            &format!("{}keys/{name}.age-key", "../".repeat(depth)),
            &format!("secrets/{dir}/{name}.age-key"),
        );
    }
    /// secretctl with a hermetic environment; recipient-adding SOPS_*
    /// variables must be ignored.
    fn secretctl(&self) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_secretctl"));
        for (key, _) in std::env::vars_os() {
            if key.as_encoded_bytes().starts_with(b"SOPS_") {
                c.env_remove(key);
            }
        }
        let foreign = Command::new("age-keygen").output().unwrap();
        c.current_dir(&self.0)
            .env("HOME", self.p("home"))
            .env("XDG_CONFIG_HOME", self.p("home/.config"))
            .env("GNUPGHOME", self.p("home/gnupg-absent"))
            .env("SOPS_AGE_KEY_FILE", self.identity("a"))
            .env("SOPS_AGE_RECIPIENTS", public_age(&foreign.stdout));
        c
    }
    fn add(&self, name: &str) -> Output {
        self.add_with_path(name, None)
    }
    fn add_with_path(&self, name: &str, path: Option<&OsString>) -> Output {
        let mut c = self.secretctl();
        if let Some(path) = path {
            c.env("PATH", path);
        }
        c.args(["machine", "add", "--root", "secrets", "--", name])
            .output()
            .unwrap()
    }
    fn dry_run(&self) -> Output {
        self.secretctl()
            .args([
                "encrypt",
                "--dry-run",
                "--root",
                "secrets",
                "secrets/values",
            ])
            .output()
            .unwrap()
    }
    /// Every entry below `secrets/`, with symlink targets and file contents.
    fn tree(&self) -> BTreeMap<PathBuf, Entry> {
        fn visit(base: &Path, dir: &Path, out: &mut BTreeMap<PathBuf, Entry>) {
            for entry in fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                let meta = fs::symlink_metadata(&path).unwrap();
                let key = path.strip_prefix(base).unwrap().to_owned();
                if meta.file_type().is_symlink() {
                    out.insert(key, Entry::Link(fs::read_link(&path).unwrap()));
                } else if meta.is_dir() {
                    out.insert(key, Entry::Dir);
                    visit(base, &path, out);
                } else {
                    out.insert(key, Entry::File(fs::read(&path).unwrap()));
                }
            }
        }
        let mut out = BTreeMap::new();
        visit(&self.0, &self.p("secrets"), &mut out);
        out
    }
    fn status(&self) -> String {
        let out = Command::new("git")
            .current_dir(&self.0)
            .args(["status", "--porcelain", "--untracked-files=all"])
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", text(&out));
        String::from_utf8(out.stdout).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

fn git(path: &Path, args: &[&str]) {
    let out = Command::new("git")
        .current_dir(path)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .args([
            "-c",
            "user.name=secretctl test",
            "-c",
            "user.email=secretctl@test.invalid",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", text(&out));
}
fn text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}
fn status(o: &Output, code: i32) -> String {
    assert_eq!(o.status.code(), Some(code), "{}", text(o));
    String::from_utf8(o.stdout.clone()).unwrap()
}
/// The recipient in the `# public key:` line of `age-keygen` output.
fn public_age(identity: &[u8]) -> String {
    String::from_utf8_lossy(identity)
        .lines()
        .find_map(|l| l.strip_prefix("# public key: "))
        .unwrap()
        .to_owned()
}
fn json(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}
fn age_recipients(path: &Path) -> BTreeSet<String> {
    json(path)["sops"]["age"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["recipient"].as_str().unwrap().to_owned())
        .collect()
}
fn decrypt(path: &Path, identity: &Path) -> Vec<u8> {
    let out = Command::new("sops")
        .env("SOPS_AGE_KEY_FILE", identity)
        .args(["--config", "/dev/null", "decrypt"])
        .args(["--input-type", "binary", "--output-type", "binary"])
        .arg(path)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", text(&out));
    out.stdout
}
fn piped(command: &mut Command, input: &[u8]) -> Output {
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "{}", text(&out));
    out
}
/// Asserts exit status 2 with no stdout and `reason` on stderr; returns stderr.
fn refused(out: Output, reason: &str) -> String {
    assert_eq!(out.status.code(), Some(2), "{}", text(&out));
    assert!(out.stdout.is_empty(), "{}", text(&out));
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(stderr.contains(reason), "{stderr}");
    stderr
}
/// No generated private key material anywhere in `secrets/`.
fn assert_no_plaintext(f: &Fixture) {
    for (path, entry) in f.tree() {
        if let Entry::File(bytes) = entry {
            for needle in [&b"AGE-SECRET-KEY-"[..], b"PRIVATE KEY"] {
                assert!(
                    !bytes.windows(needle.len()).any(|w| w == needle),
                    "plaintext key material in {}",
                    path.display()
                );
            }
        }
    }
}
fn find_on_path(name: &str) -> PathBuf {
    std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|p| p.join(name))
        .find(|p| p.is_file())
        .unwrap()
}
/// Puts `script` first on PATH as `name`; `$REAL` is the real `name`.
fn spy(f: &Fixture, name: &str, script: &str) -> OsString {
    let path = std::env::var_os("PATH").unwrap();
    f.put(
        &format!("bin/{name}"),
        format!(
            "#!{}\nREAL='{}'\n{script}",
            find_on_path("sh").display(),
            find_on_path(name).display()
        )
        .as_bytes(),
    );
    fs::set_permissions(
        f.p(&format!("bin/{name}")),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    std::env::join_paths(std::iter::once(f.p("bin")).chain(std::env::split_paths(&path))).unwrap()
}
/// A `sops` that logs its arguments to `sops.log` and, on call number `at`,
/// sends `signal` (if not empty) to secretctl and fails.
fn sops_spy(f: &Fixture, at: usize, signal: &str) -> OsString {
    let log = f.p("sops.log");
    spy(
        f,
        "sops",
        &format!(
            "printf '%s\\n' \"$*\" >> '{log}'\n\
             if [ \"$(wc -l < '{log}')\" -eq {at} ]; then\n\
             [ -z '{signal}' ] || kill -s '{signal}' \"$PPID\"\n\
             exit 1\n\
             fi\n\
             exec \"$REAL\" \"$@\"\n",
            log = log.display()
        ),
    )
}
/// Staged plaintext paths that secretctl passed to sops (the last argument).
fn staged_paths(f: &Fixture) -> Vec<PathBuf> {
    fs::read_to_string(f.p("sops.log"))
        .unwrap()
        .lines()
        .map(|l| PathBuf::from(l.rsplit(' ').next().unwrap()))
        .collect()
}

#[test]
fn machine_add_creates_exactly_the_five_artefacts_for_the_policy() {
    let (f, a, b) = Fixture::new();
    let before = f.tree();
    let listing = status(&f.add("m"), 0);
    for artefact in ARTEFACTS {
        assert!(listing.contains(&format!("  {artefact}\n")), "{listing}");
    }
    assert!(listing.contains("machines/m.nix"), "{listing}");
    assert!(listing.contains("provision m "), "{listing}");

    let after = f.tree();
    let new_files: BTreeSet<&Path> = after
        .iter()
        .filter(|(path, entry)| **entry != Entry::Dir && !before.contains_key(*path))
        .map(|(path, _)| path.as_path())
        .collect();
    assert_eq!(new_files, ARTEFACTS.iter().map(Path::new).collect());
    assert!(
        after
            .iter()
            .all(|(path, entry)| before.get(path).is_none_or(|old| old == entry))
    );
    assert_eq!(
        after[Path::new("secrets/values/machines/m/m.age-key")],
        Entry::Link(PathBuf::from("../../../keys/machines/m.age-key"))
    );
    assert_no_plaintext(&f);

    let Entry::File(recipient) = &after[Path::new("secrets/keys/machines/m.age-key")] else {
        panic!("recipient is not a regular file");
    };
    let recipient = String::from_utf8(recipient.clone()).unwrap();
    let machine = recipient.strip_suffix('\n').unwrap().to_owned();
    assert!(machine.starts_with("age1"), "{recipient}");

    // Admins only for the identity; admins plus the machine for the host key.
    // SOPS_AGE_RECIPIENTS from the environment was ignored.
    let identity_file = f.p("secrets/values/roots/m.private");
    let host_key_file = f.p("secrets/values/machines/m/openssh/ssh_host_ed25519_key");
    for file in [&identity_file, &host_key_file] {
        let document = json(file);
        assert!(document["sops"].get("pgp").is_none());
        assert!(
            document["data"]
                .as_str()
                .unwrap()
                .starts_with("ENC[AES256_GCM,")
        );
    }
    assert_eq!(
        age_recipients(&identity_file),
        BTreeSet::from([a.clone(), b.clone()])
    );
    assert_eq!(
        age_recipients(&host_key_file),
        BTreeSet::from([a, b, machine.clone()])
    );

    // The identity is full age-keygen output for the recorded recipient,
    // checked the way provision does.
    let identity = decrypt(&identity_file, &f.identity("b"));
    assert!(identity.starts_with(b"# created: "));
    assert_eq!(public_age(&identity), machine);
    let derived = piped(Command::new("age-keygen").arg("-y"), &identity);
    assert_eq!(String::from_utf8(derived.stdout).unwrap(), recipient);

    // The machine itself can decrypt its host key, which matches `.public`.
    f.put("identities/m.identity", &identity);
    let host_key = decrypt(&host_key_file, &f.identity("m"));
    assert_eq!(host_key, decrypt(&host_key_file, &f.identity("a")));
    f.put("identities/host_key", &host_key);
    fs::set_permissions(
        f.p("identities/host_key"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    let out = Command::new("ssh-keygen")
        .arg("-y")
        .arg("-f")
        .arg(f.p("identities/host_key"))
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", text(&out));
    let fields = |bytes: &[u8]| -> Vec<String> {
        String::from_utf8_lossy(bytes)
            .split_whitespace()
            .take(2)
            .map(str::to_owned)
            .collect()
    };
    let Entry::File(public) =
        &after[Path::new("secrets/keys/machines/m.ssh_host_ed25519-key.public")]
    else {
        panic!("public host key is not a regular file");
    };
    assert_eq!(fields(&out.stdout), fields(public));
    assert_eq!(fields(public)[0], "ssh-ed25519");

    // Only intent-to-add entries: no content staged, nothing committed.
    let expected: String = ARTEFACTS.iter().map(|a| format!(" A {a}\n")).collect();
    assert_eq!(f.status(), expected);
    let staged = Command::new("git")
        .current_dir(&f.0)
        .args(["diff", "--cached", "--name-only"])
        .output()
        .unwrap();
    assert_eq!(text(&staged), "");

    // The result is consistent and matches the policy.
    let listing = status(&f.dry_run(), 0);
    assert!(
        listing.contains("secrets/values/roots/m.private: ok\n"),
        "{listing}"
    );
    assert!(
        listing.contains("secrets/values/machines/m/openssh/ssh_host_ed25519_key: ok\n"),
        "{listing}"
    );
}

#[test]
fn existing_machine_invalid_name_and_non_git_root_are_refused_with_nothing_written() {
    let (f, _, _) = Fixture::new();
    let tree = f.tree();
    let git_status = f.status();
    for name in [
        "", "-m", "_m", "m.x", "m/x", "../m", "m x", "m\u{e9}", "m\n",
    ] {
        refused(f.add(name), "invalid machine name");
        assert_eq!(f.tree(), tree, "{name:?}");
        assert_eq!(f.status(), git_status);
    }

    // Any single existing artefact refuses the whole machine.
    for artefact in ARTEFACTS {
        if artefact.ends_with("m/m.age-key") {
            f.link("../../../keys/machines/m.age-key", artefact);
        } else {
            f.put(artefact, b"existing\n");
        }
        let tree = f.tree();
        let stderr = refused(f.add("m"), "machine m already exists");
        assert!(stderr.contains(&format!("\n  {artefact}\n")), "{stderr}");
        assert_eq!(f.tree(), tree);
        fs::remove_file(f.p(artefact)).unwrap();
    }

    status(&f.add("m"), 0);
    let added = f.tree();
    let git_status = f.status();
    let stderr = refused(f.add("m"), "machine m already exists");
    for artefact in ARTEFACTS {
        assert!(stderr.contains(&format!("\n  {artefact}\n")), "{stderr}");
    }
    assert_eq!(f.tree(), added);
    assert_eq!(f.status(), git_status);
    // With only the intent-to-add entries left it is still refused.
    for artefact in ARTEFACTS {
        fs::remove_file(f.p(artefact)).unwrap();
    }
    let tree = f.tree();
    let stderr = refused(f.add("m"), "machine m already exists");
    for artefact in ARTEFACTS {
        assert!(
            stderr.contains(&format!("\n  {artefact} (only in the Git index)\n")),
            "{stderr}"
        );
    }
    assert_eq!(f.tree(), tree);

    let bare = Fixture::bare();
    bare.age_key("a");
    bare.grant("values", "a");
    let tree = bare.tree();
    refused(bare.add("m"), "secrets root is not inside a Git worktree");
    assert_eq!(bare.tree(), tree);

    // Without admin markers the identity would have no recipients.
    let unguarded = Fixture::bare();
    git(&unguarded.0, &["init", "-q"]);
    let tree = unguarded.tree();
    refused(unguarded.add("m"), "empty effective recipient set");
    assert_eq!(unguarded.tree(), tree);
}

#[test]
fn failed_or_interrupted_runs_roll_back_and_a_retry_succeeds() {
    let (f, _, _) = Fixture::new();
    let tree = f.tree();
    let git_status = f.status();
    // sops runs twice (host key, then identity); fail or interrupt either.
    for (at, signal, reason) in [
        (1, "", "sops for "),
        (2, "", "sops for "),
        (1, "TERM", "interrupted by signal 15"),
        (2, "INT", "interrupted by signal 2"),
        (2, "HUP", "interrupted by signal 1"),
    ] {
        let path = sops_spy(&f, at, signal);
        let out = f.add_with_path("m", Some(&path));
        let stderr = refused(out, reason);
        assert!(
            stderr.contains("machine m not added; this run's changes were undone"),
            "{stderr}"
        );
        assert_eq!(f.tree(), tree, "{at} {signal}");
        assert_eq!(f.status(), git_status);
        for staged in staged_paths(&f) {
            assert!(staged.starts_with("/dev/shm"), "{}", staged.display());
            assert!(!staged.parent().unwrap().exists(), "{}", staged.display());
        }
        fs::remove_file(f.p("sops.log")).unwrap();
    }

    // A failing `git add --intent-to-add` also drops the index entries it made.
    fs::remove_file(f.p("bin/sops")).unwrap();
    let path = spy(&f, "git", "\"$REAL\" \"$@\"\n[ \"$2\" != add ] || exit 1\n");
    let stderr = refused(
        f.add_with_path("m", Some(&path)),
        "git add --intent-to-add failed",
    );
    assert!(
        stderr.contains("this run's changes were undone"),
        "{stderr}"
    );
    assert_eq!(f.tree(), tree);
    assert_eq!(f.status(), git_status);

    status(&f.add("m"), 0);
    status(&f.dry_run(), 0);
}

#[test]
fn killed_run_leaves_no_plaintext_in_the_tree() {
    for at in [1, 2] {
        let (f, _, _) = Fixture::new();
        let path = sops_spy(&f, at, "KILL");
        let out = f.add_with_path("m", Some(&path));
        assert_eq!(out.status.code(), None, "{}", text(&out));
        assert_no_plaintext(&f);
        // SIGKILL cannot be caught: remove the staging directory it left.
        for staged in staged_paths(&f) {
            assert!(staged.starts_with("/dev/shm"), "{}", staged.display());
            let _ = fs::remove_dir_all(staged.parent().unwrap());
        }
    }
}

#[test]
fn consistency_check_flags_missing_marker_and_orphaned_machine_directory() {
    let (f, a, _) = Fixture::new();
    f.put(
        "secrets/keys/machines/x.age-key",
        format!("{a}\n").as_bytes(),
    );
    for out in [
        f.dry_run(),
        f.secretctl()
            .args(["encrypt", "--root", "secrets", "secrets/values"])
            .output()
            .unwrap(),
    ] {
        let stderr = refused(
            out,
            "secrets/keys/machines/x.age-key: machine recipient without marker ",
        );
        assert!(stderr.contains("values/machines/x/x.age-key"), "{stderr}");
    }
    f.put(
        "secrets/values/machines/x/x.age-key",
        format!("{a}\n").as_bytes(),
    );
    refused(f.dry_run(), "machine marker is not a symlink");
    fs::remove_file(f.p("secrets/values/machines/x/x.age-key")).unwrap();
    f.link(
        "../../../keys/machines/x.age-key",
        "secrets/values/machines/x/x.age-key",
    );
    status(&f.dry_run(), 0);

    fs::create_dir_all(f.p("secrets/values/machines/y")).unwrap();
    let stderr = refused(
        f.dry_run(),
        "secrets/values/machines/y: machine directory without ",
    );
    assert!(stderr.contains("keys/machines/y.age-key"), "{stderr}");
    // With the key file the directory is a known machine again; its missing
    // marker is reported instead.
    f.put(
        "secrets/keys/machines/y.age-key",
        format!("{a}\n").as_bytes(),
    );
    refused(
        f.dry_run(),
        "secrets/keys/machines/y.age-key: machine recipient without marker",
    );
}

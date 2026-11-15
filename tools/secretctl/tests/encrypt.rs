//! `secretctl encrypt` against real `sops`, `age` and `age-keygen` with
//! throwaway age identities. PGP is covered at the policy/argument level only.
use std::os::unix::fs::{PermissionsExt, symlink};
use std::{
    collections::BTreeSet,
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    sync::atomic::{AtomicUsize, Ordering},
    time::SystemTime,
};

use serde_json::Value;

static NEXT: AtomicUsize = AtomicUsize::new(0);
const PLAINTEXT: &[u8] = b"secret bytes\0\xff\n";
const PGP: &str = "ABCDEF0123456789ABCDEF0123456789ABCDEF01";

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "secretctl-encrypt-{}-{}",
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
    /// Generates identity `keys/<name>.identity` and its recipient marker
    /// target `secrets/keys/<name>.age-key`; returns the recipient.
    fn age_key(&self, name: &str) -> String {
        let identity = self.identity(name);
        fs::create_dir_all(identity.parent().unwrap()).unwrap();
        let out = Command::new("age-keygen")
            .arg("-o")
            .arg(&identity)
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", text(&out));
        let out = Command::new("age-keygen")
            .arg("-y")
            .arg(&identity)
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", text(&out));
        let recipient = String::from_utf8(out.stdout).unwrap().trim().to_owned();
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
    fn revoke(&self, dir: &str, name: &str) {
        fs::remove_file(self.p(&format!("secrets/{dir}/{name}.age-key"))).unwrap();
    }
    /// secretctl with a hermetic environment: decryption uses `identity`,
    /// and recipient-adding SOPS_* variables must be ignored.
    fn command(&self, identity: &str) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_secretctl"));
        for (key, _) in std::env::vars_os() {
            if key.as_encoded_bytes().starts_with(b"SOPS_") {
                c.env_remove(key);
            }
        }
        let foreign = Command::new("age-keygen").output().unwrap();
        let foreign = String::from_utf8(foreign.stdout).unwrap();
        let foreign = foreign
            .lines()
            .find_map(|l| l.strip_prefix("# public key: "))
            .unwrap()
            .to_owned();
        c.current_dir(&self.0)
            .env("HOME", self.p("home"))
            .env("XDG_CONFIG_HOME", self.p("home/.config"))
            .env("GNUPGHOME", self.p("home/gnupg-absent"))
            .env("SOPS_AGE_KEY_FILE", self.identity(identity))
            .env("SOPS_AGE_RECIPIENTS", foreign)
            .env("SOPS_PGP_FP", PGP)
            .args(["encrypt", "--root", "secrets"]);
        c
    }
    fn encrypt(&self, identity: &str) -> Output {
        self.command(identity)
            .arg("secrets/values")
            .output()
            .unwrap()
    }
    fn dry_run(&self, identity: &str) -> Output {
        self.command(identity)
            .args(["--dry-run", "secrets/values"])
            .output()
            .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
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
fn decrypt(path: &Path, identity: &Path) -> Output {
    Command::new("sops")
        .env("SOPS_AGE_KEY_FILE", identity)
        .args(["--config", "/dev/null", "decrypt"])
        .args(["--input-type", "binary", "--output-type", "binary"])
        .arg(path)
        .output()
        .unwrap()
}
fn decrypts_to(path: &Path, identity: &Path, expected: &[u8]) {
    let out = decrypt(path, identity);
    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(out.stdout, expected);
}
/// The sops data key, recovered with `age` from the stanza for `recipient`.
fn data_key(path: &Path, recipient: &str, identity: &Path) -> Vec<u8> {
    let document = json(path);
    let enc = document["sops"]["age"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["recipient"] == recipient)
        .unwrap()["enc"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut child = Command::new("age")
        .arg("-d")
        .arg("-i")
        .arg(identity)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(enc.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    assert_eq!(out.stdout.len(), 32);
    out.stdout
}
fn snapshot(path: &Path) -> (Vec<u8>, SystemTime) {
    (
        fs::read(path).unwrap(),
        fs::metadata(path).unwrap().modified().unwrap(),
    )
}
/// Puts a `sops` spy first on PATH: it appends its arguments to `sops.log`,
/// its SOPS_* environment to `sops.env`, evaluates `$SPY_HOOK` (if set) and
/// exits with `$SPY_EXIT`.
fn spy_path(f: &Fixture) -> std::ffi::OsString {
    let path = std::env::var_os("PATH").unwrap();
    let shell = std::env::split_paths(&path)
        .map(|p| p.join("sh"))
        .find(|p| p.is_file())
        .unwrap();
    let log = f.p("sops.log");
    let env = f.p("sops.env");
    f.put(
        "bin/sops",
        format!(
            "#!{}\nprintf '%s\\n' \"$*\" >> '{}'\nenv | grep '^SOPS_' | sort >> '{}'\neval \"${{SPY_HOOK:-}}\"\nexit \"$SPY_EXIT\"\n",
            shell.display(),
            log.display(),
            env.display()
        )
        .as_bytes(),
    );
    fs::set_permissions(f.p("bin/sops"), fs::Permissions::from_mode(0o755)).unwrap();
    std::env::join_paths(std::iter::once(f.p("bin")).chain(std::env::split_paths(&path))).unwrap()
}

#[test]
fn plaintext_is_encrypted_in_binary_format_for_the_policy() {
    let f = Fixture::new();
    let a = f.age_key("a");
    let b = f.age_key("b");
    f.grant("values", "a");
    f.grant("values/nested", "b");
    f.put("secrets/values/nested/secret", PLAINTEXT);
    let file = f.p("secrets/values/nested/secret");
    let out = f.encrypt("a");
    let listing = status(&out, 0);
    assert!(
        listing.contains("secrets/values/nested/secret: encrypt\n"),
        "{listing}"
    );
    assert!(!text(&out).contains("secret bytes"));
    // The format sops-nix reads with `format = "binary"`, like the real tree.
    let bytes = fs::read(&file).unwrap();
    assert!(bytes.starts_with(b"{\n\t\"data\": \"ENC[AES256_GCM,"));
    let document = json(&file);
    let keys: BTreeSet<&str> = document
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(keys, BTreeSet::from(["data", "sops"]));
    assert!(document["sops"]["mac"].is_string());
    // SOPS_AGE_RECIPIENTS and SOPS_PGP_FP from the environment were ignored.
    assert!(document["sops"].get("pgp").is_none());
    assert_eq!(age_recipients(&file), BTreeSet::from([a, b]));
    decrypts_to(&file, &f.identity("a"), PLAINTEXT);
    decrypts_to(&file, &f.identity("b"), PLAINTEXT);
    let listing = status(&f.dry_run("a"), 0);
    assert!(listing.contains("secrets/values/nested/secret: ok\n"));
}

#[test]
fn adding_a_recipient_rotates_the_data_key() {
    let f = Fixture::new();
    let a = f.age_key("a");
    let b = f.age_key("b");
    f.grant("values", "a");
    f.put("secrets/values/secret", PLAINTEXT);
    let file = f.p("secrets/values/secret");
    status(&f.encrypt("a"), 0);
    let old_key = data_key(&file, &a, &f.identity("a"));
    assert!(!decrypt(&file, &f.identity("b")).status.success());
    f.grant("values", "b");
    let before = snapshot(&file);
    let listing = status(&f.dry_run("a"), 1);
    assert!(
        listing.contains(&format!("secrets/values/secret: rotate +age:{b}\n")),
        "{listing}"
    );
    assert_eq!(snapshot(&file), before);
    let listing = status(&f.encrypt("a"), 0);
    assert!(listing.contains(&format!("secrets/values/secret: rotate +age:{b}\n")));
    assert_eq!(
        age_recipients(&file),
        BTreeSet::from([a.clone(), b.clone()])
    );
    assert_ne!(data_key(&file, &a, &f.identity("a")), old_key);
    assert_eq!(
        data_key(&file, &a, &f.identity("a")),
        data_key(&file, &b, &f.identity("b"))
    );
    decrypts_to(&file, &f.identity("b"), PLAINTEXT);
    status(&f.dry_run("b"), 0);
}

#[test]
fn removing_a_recipient_rotates_and_revokes_it() {
    let f = Fixture::new();
    let a = f.age_key("a");
    let b = f.age_key("b");
    f.grant("values", "a");
    f.grant("values", "b");
    f.put("secrets/values/secret", PLAINTEXT);
    let file = f.p("secrets/values/secret");
    status(&f.encrypt("b"), 0);
    let old_key = data_key(&file, &a, &f.identity("a"));
    decrypts_to(&file, &f.identity("b"), PLAINTEXT);
    f.revoke("values", "b");
    let listing = status(&f.dry_run("a"), 1);
    assert!(
        listing.contains(&format!("secrets/values/secret: rotate -age:{b}\n")),
        "{listing}"
    );
    status(&f.encrypt("a"), 0);
    assert_eq!(age_recipients(&file), BTreeSet::from([a.clone()]));
    assert_ne!(data_key(&file, &a, &f.identity("a")), old_key);
    decrypts_to(&file, &f.identity("a"), PLAINTEXT);
    let out = decrypt(&file, &f.identity("b"));
    assert!(!out.status.success(), "{}", text(&out));
    assert!(out.stdout.is_empty());
}

#[test]
fn matching_file_is_left_byte_identical() {
    let f = Fixture::new();
    f.age_key("a");
    f.grant("values", "a");
    f.put("secrets/values/secret", PLAINTEXT);
    let file = f.p("secrets/values/secret");
    status(&f.encrypt("a"), 0);
    let before = snapshot(&file);
    let listing = status(&f.encrypt("a"), 0);
    assert!(listing.contains("secrets/values/secret: ok\n"), "{listing}");
    assert_eq!(snapshot(&file), before);
}

#[test]
fn malformed_sops_json_is_an_error_and_nothing_is_modified() {
    let f = Fixture::new();
    let a = f.age_key("a");
    f.grant("values", "a");
    f.put("secrets/values/a-plaintext", PLAINTEXT);
    let plaintext = f.p("secrets/values/a-plaintext");
    let age = format!(r#"[{{"recipient":"{a}","enc":"x"}}]"#);
    for (document, reason) in [
        (r#"{"sops":1}"#.to_owned(), "`sops` is not an object"),
        (
            format!(r#"{{"data":"x","sops":{{"age":{age}}}}}"#),
            "missing string `sops.mac`",
        ),
        (
            r#"{"data":"x","sops":{"mac":"m","age":"age1"}}"#.to_owned(),
            "`sops.age` is not a list",
        ),
        (
            r#"{"data":"x","sops":{"mac":"m","age":[{"enc":"x"}]}}"#.to_owned(),
            "`sops.age` entry without a valid `recipient`",
        ),
        (
            r#"{"data":"x","sops":{"mac":"m","pgp":[{"fp":"A,B"}]}}"#.to_owned(),
            "`sops.pgp` entry without a valid `fp`",
        ),
        (
            format!(r#"{{"data":"x","sops":{{"mac":"m","age":{age},"kms":[{{"arn":"x"}}]}}}}"#),
            "unsupported master key type `sops.kms`",
        ),
        (
            format!(
                r#"{{"data":"x","sops":{{"mac":"m","age":{age},"gcp_kms":[{{"resource_id":"x"}}]}}}}"#
            ),
            "unsupported master key type `sops.gcp_kms`",
        ),
        (
            format!(
                r#"{{"data":"x","sops":{{"mac":"m","age":{age},"hckms":[{{"key_id":"x"}}]}}}}"#
            ),
            "unsupported master key type `sops.hckms`",
        ),
        (
            format!(
                r#"{{"data":"x","sops":{{"mac":"m","age":{age},"azure_kv":[{{"vault_url":"x"}}]}}}}"#
            ),
            "unsupported master key type `sops.azure_kv`",
        ),
        (
            format!(
                r#"{{"data":"x","sops":{{"mac":"m","age":{age},"hc_vault":[{{"vault_address":"x"}}]}}}}"#
            ),
            "unsupported master key type `sops.hc_vault`",
        ),
        (
            format!(r#"{{"data":"x","sops":{{"mac":"m","key_groups":[{{"age":{age}}}]}}}}"#),
            "unsupported master key type `sops.key_groups`",
        ),
        (
            format!(r#"{{"data":"x","other":1,"sops":{{"mac":"m","age":{age}}}}}"#),
            "not sops binary format",
        ),
        (
            format!(r#"{{"sops":{{"mac":"m","age":{age}}}}}"#),
            "not sops binary format",
        ),
        (
            r#"{"data":"x","sops":{"mac":"m","age":null,"pgp":[]}}"#.to_owned(),
            "no age or pgp recipients",
        ),
    ] {
        f.put("secrets/values/z-sops", document.as_bytes());
        let malformed = snapshot(&f.p("secrets/values/z-sops"));
        let before = snapshot(&plaintext);
        let out = f.encrypt("a");
        assert_eq!(out.status.code(), Some(2), "{document}: {}", text(&out));
        assert!(out.stdout.is_empty(), "{}", text(&out));
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("secrets/values/z-sops: malformed sops file: ")
                && stderr.contains(reason),
            "{document}: {stderr}"
        );
        assert_eq!(snapshot(&plaintext), before);
        assert_eq!(snapshot(&f.p("secrets/values/z-sops")), malformed);
    }
    // Explicit nulls and empty lists of other key types, as older sops wrote
    // them, are still a sops file; JSON without `sops` is plaintext.
    f.put(
        "secrets/values/z-sops",
        format!(
            r#"{{"data":"x","sops":{{"mac":"m","age":{age},"pgp":null,"kms":null,"hc_vault":[]}}}}"#
        )
        .as_bytes(),
    );
    f.put("secrets/values/z-json", br#"{"data":"x","mac":"m"}"#);
    let listing = status(&f.dry_run("a"), 1);
    assert!(listing.contains("secrets/values/z-sops: ok\n"), "{listing}");
    assert!(
        listing.contains("secrets/values/z-json: encrypt\n"),
        "{listing}"
    );
}

#[test]
fn ssh_recipient_stored_under_age_is_policy_drift() {
    let f = Fixture::new();
    let a = f.age_key("a");
    f.grant("values", "a");
    // sops keeps age-ssh recipients in `sops.age` like native ones.
    let ssh = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIHnCbvAb3Fg5p6Lz3XKvA1nQ8QYfP7Ck1nYp0r1nYd3G";
    f.put(
        "secrets/values/secret",
        format!(
            r#"{{"data":"x","sops":{{"mac":"m","age":[{{"recipient":"{a}"}},{{"recipient":"{ssh}"}}]}}}}"#
        )
        .as_bytes(),
    );
    let listing = status(&f.dry_run("a"), 1);
    assert!(
        listing.contains(&format!("secrets/values/secret: rotate -age:{ssh}\n")),
        "{listing}"
    );
}

#[test]
fn dry_run_exit_codes() {
    let f = Fixture::new();
    f.age_key("a");
    f.grant("values", "a");
    assert_eq!(status(&f.dry_run("a"), 0), "");
    f.put("secrets/values/secret", PLAINTEXT);
    let before = snapshot(&f.p("secrets/values/secret"));
    let out = f.dry_run("a");
    status(&out, 1);
    assert!(String::from_utf8_lossy(&out.stderr).contains("1 file(s) need action"));
    assert_eq!(snapshot(&f.p("secrets/values/secret")), before);
    f.link("missing", "secrets/values/unexpected");
    let out = f.dry_run("a");
    status(&out, 2);
    assert!(out.stdout.is_empty());
    fs::remove_file(f.p("secrets/values/unexpected")).unwrap();
    status(&f.encrypt("a"), 0);
    status(&f.dry_run("a"), 0);
}

#[test]
fn errors_before_any_sops_run_leave_the_tree_unmodified() {
    let f = Fixture::new();
    f.age_key("a");
    f.grant("values", "a");
    f.put("secrets/values/a-plaintext", PLAINTEXT);
    let path = spy_path(&f);
    for (broken, reason) in [
        ("secrets/values/zz/sops", "malformed sops file"),
        ("secrets/values/zz/unexpected", "unexpected content symlink"),
    ] {
        if broken.ends_with("sops") {
            f.put(broken, br#"{"data":"x","sops":{"age":[]}}"#);
        } else {
            f.link("../a-plaintext", broken);
        }
        let before = snapshot(&f.p("secrets/values/a-plaintext"));
        let out = f
            .command("a")
            .env("PATH", &path)
            .env("SPY_EXIT", "0")
            .arg("secrets/values")
            .output()
            .unwrap();
        status(&out, 2);
        assert!(
            String::from_utf8_lossy(&out.stderr).contains(reason),
            "{}",
            text(&out)
        );
        assert!(
            !f.p("sops.log").exists(),
            "sops ran before validation finished"
        );
        assert_eq!(snapshot(&f.p("secrets/values/a-plaintext")), before);
        fs::remove_file(f.p(broken)).unwrap();
    }
}

#[test]
fn sops_arguments_environment_and_first_failure() {
    let f = Fixture::new();
    let a = f.age_key("a");
    f.grant("values", "a");
    f.put("secrets/keys/admin.pgp-key", PGP.to_lowercase().as_bytes());
    f.link("../keys/admin.pgp-key", "secrets/values/admin.pgp-key");
    let old_pgp = "0123456789abcdef0123456789abcdef01234567";
    f.put(
        "secrets/values/a-sops",
        format!(
            r#"{{"data":"x","sops":{{"mac":"m","age":[{{"recipient":"{a}"}}],"pgp":[{{"fp":"{old_pgp}"}}]}}}}"#
        )
        .as_bytes(),
    );
    f.put("secrets/values/b-plaintext", PLAINTEXT);
    let listing = status(&f.dry_run("a"), 1);
    assert!(
        listing.contains(&format!(
            "secrets/values/a-sops: rotate +pgp:{PGP} -pgp:{}\n",
            old_pgp.to_uppercase()
        )),
        "{listing}"
    );
    let path = spy_path(&f);
    let run = |exit: &str| {
        f.command("a")
            .env("PATH", &path)
            .env("SPY_EXIT", exit)
            .env("SOPS_KEYSERVICE", "tcp://redirect.invalid:1")
            .env("SOPS_CONFIG", "/nonexistent")
            .env("SOPS_GPG_EXEC", "gpg")
            .arg("secrets/values")
            .output()
            .unwrap()
    };
    let out = run("7");
    status(&out, 2);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains(
            "secrets/values/a-sops: sops failed (exit status: 7); stopped after 0 of 2 action(s)"
        ),
        "{stderr}"
    );
    let root = fs::canonicalize(f.p("secrets")).unwrap();
    let rotate = format!(
        "--config /dev/null rotate -i --add-pgp {PGP} --rm-pgp {old_pgp} --input-type binary --output-type binary {}/values/a-sops\n",
        root.display()
    );
    assert_eq!(fs::read_to_string(f.p("sops.log")).unwrap(), rotate);
    assert_eq!(
        fs::read_to_string(f.p("sops.env")).unwrap(),
        format!(
            "SOPS_AGE_KEY_FILE={}\nSOPS_GPG_EXEC=gpg\n",
            f.identity("a").display()
        )
    );
    assert_eq!(
        fs::read(f.p("secrets/values/b-plaintext")).unwrap(),
        PLAINTEXT
    );
    fs::remove_file(f.p("sops.log")).unwrap();
    // The spy "succeeds" without changing anything: the post-check catches it.
    let out = run("0");
    status(&out, 2);
    assert!(
        String::from_utf8_lossy(&out.stderr).contains(
            "secrets/values/a-sops: sops succeeded but the result does not match the recipient policy"
        ),
        "{}",
        text(&out)
    );
    fs::remove_file(f.p("secrets/values/a-sops")).unwrap();
    fs::remove_file(f.p("sops.log")).unwrap();
    status(&run("0"), 2);
    assert_eq!(
        fs::read_to_string(f.p("sops.log")).unwrap(),
        format!(
            "--config /dev/null encrypt -i --age {a} --pgp {PGP} --input-type binary --output-type binary {}/values/b-plaintext\n",
            root.display()
        )
    );
}

#[test]
fn files_changed_after_classification_are_not_passed_to_sops() {
    let f = Fixture::new();
    let a = f.age_key("a");
    let b = f.age_key("b");
    f.grant("values", "a");
    let path = spy_path(&f);
    let sops_for = |recipients: &[&str]| {
        let age: Vec<String> = recipients
            .iter()
            .map(|r| format!(r#"{{"recipient":"{r}"}}"#))
            .collect();
        format!(
            r#"{{"data":"x","sops":{{"mac":"m","age":[{}]}}}}"#,
            age.join(",")
        )
    };
    // The spy plays a sops that encrypts `a-plaintext` correctly while
    // something else concurrently rewrites the second candidate.
    f.put("ready-a", sops_for(&[&a]).as_bytes());
    for (second, initial, replacement, now) in [
        (
            "b-plaintext",
            PLAINTEXT.to_vec(),
            sops_for(&[&a]),
            "ok".to_owned(),
        ),
        (
            "b-sops",
            sops_for(&[&a, &b]).into_bytes(),
            sops_for(&[&b]),
            format!("rotate +age:{a} -age:{b}"),
        ),
    ] {
        f.put("secrets/values/a-plaintext", PLAINTEXT);
        f.put(&format!("secrets/values/{second}"), &initial);
        f.put("replacement", replacement.as_bytes());
        let out = f
            .command("a")
            .env("PATH", &path)
            .env("SPY_EXIT", "0")
            .env(
                "SPY_HOOK",
                format!(
                    "cp '{}' '{}'; cp '{}' '{}'",
                    f.p("ready-a").display(),
                    f.p("secrets/values/a-plaintext").display(),
                    f.p("replacement").display(),
                    f.p(&format!("secrets/values/{second}")).display(),
                ),
            )
            .arg("secrets/values")
            .output()
            .unwrap();
        status(&out, 2);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains(&format!(
                "secrets/values/{second}: changed since classification (now: {now}); \
                 stopped after 1 of 2 action(s), sops was not run for it"
            )),
            "{}",
            text(&out)
        );
        let log = fs::read_to_string(f.p("sops.log")).unwrap();
        assert_eq!(log.lines().count(), 1, "{log}");
        assert!(log.ends_with("/values/a-plaintext\n"), "{log}");
        assert_eq!(
            fs::read(f.p(&format!("secrets/values/{second}"))).unwrap(),
            replacement.as_bytes()
        );
        fs::remove_file(f.p("sops.log")).unwrap();
        fs::remove_file(f.p(&format!("secrets/values/{second}"))).unwrap();
    }
}

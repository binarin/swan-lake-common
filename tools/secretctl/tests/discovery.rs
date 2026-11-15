use std::os::unix::{
    ffi::OsStrExt,
    fs::{PermissionsExt, symlink},
};
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::atomic::{AtomicUsize, Ordering},
};
static NEXT: AtomicUsize = AtomicUsize::new(0);
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "secretctl-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        let f = Self(path);
        git(&f.0, &["init", "-q"]);
        f.put(
            "secrets/keys/admin.pgp-key",
            b"abcdef0123456789abcdef0123456789abcdef01\n",
        );
        f.put("secrets/values/z", b"arbitrary\0\xff");
        f.link("../keys/admin.pgp-key", "secrets/values/admin.pgp-key");
        f
    }
    fn put(&self, p: &str, bytes: &[u8]) {
        let p = self.0.join(p);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, bytes).unwrap();
    }
    fn link(&self, to: &str, p: &str) {
        let p = self.0.join(p);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        symlink(to, p).unwrap();
    }
    fn run(&self, args: &[&str]) -> Output {
        self.command().args(args).output().unwrap()
    }
    fn command(&self) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_secretctl"));
        c.current_dir(&self.0);
        c
    }
    fn preview(&self) -> Output {
        self.run(&["encrypt", "--dry-run", "secrets/values"])
    }
    fn bad(&self, reason: &str) {
        let out = self.preview();
        assert!(!out.status.success(), "unexpected success: {}", text(&out));
        assert!(out.stdout.is_empty(), "partial output: {}", text(&out));
        assert!(
            String::from_utf8_lossy(&out.stderr).contains(reason),
            "{}",
            text(&out)
        );
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}
fn git(path: &Path, args: &[&str]) {
    assert!(
        Command::new("git")
            .current_dir(path)
            .args(args)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .status()
            .unwrap()
            .success()
    );
}
fn text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}
fn good(o: Output) -> String {
    assert!(o.status.success(), "{}", text(&o));
    String::from_utf8(o.stdout).unwrap()
}

fn encode(hrp: &str, bytes: &[u8]) -> String {
    let alphabet = b"qpzry9x8gf2tvdw0s3jn54khce6mua7l";
    let mut words = Vec::new();
    let mut acc = 0u32;
    let mut bits = 0;
    for &b in bytes {
        acc = (acc << 8) | u32::from(b);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            words.push(((acc >> bits) & 31) as u8);
        }
    }
    if bits > 0 {
        words.push(((acc << (5 - bits)) & 31) as u8);
    }
    encode_words(hrp, &words, alphabet)
}
fn encode_words(hrp: &str, words: &[u8], alphabet: &[u8]) -> String {
    let mut chk = 1u32;
    for v in hrp
        .bytes()
        .map(|b| b >> 5)
        .chain([0])
        .chain(hrp.bytes().map(|b| b & 31))
        .chain(words.iter().copied())
        .chain([0; 6])
    {
        let top = chk >> 25;
        chk = ((chk & 0x1ffffff) << 5) ^ u32::from(v);
        for (i, g) in [0x3b6a57b2, 0x26508e6d, 0x1ea119fa, 0x3d4233dd, 0x2a1462b3]
            .iter()
            .enumerate()
        {
            if (top >> i) & 1 != 0 {
                chk ^= g;
            }
        }
    }
    chk ^= 1;
    format!(
        "{hrp}1{}",
        words
            .iter()
            .copied()
            .chain((0..6).rev().map(|i| ((chk >> (i * 5)) & 31) as u8))
            .map(|w| alphabet[w as usize] as char)
            .collect::<String>()
    )
}

#[test]
fn discovery_inherits_deduplicates_and_excludes_only_agreed_files() {
    let f = Fixture::new();
    f.put(
        "secrets/root.pgp-key",
        b"0123456789012345678901234567890123456789",
    );
    f.link("root.pgp-key", "secrets/global.pgp-key");
    f.link("../keys/admin.pgp-key", "secrets/values/duplicate.pgp-key");
    f.put("secrets/values/nested/a.plain", b"plain");
    f.put("secrets/values/nested/b.public", b"public");
    f.put("secrets/values/nested/c.PUBLIC", b"included");
    f.put("secrets/values/unused.age-key", b"not a declaration");
    let output = good(f.preview());
    assert!(output.starts_with("DISCOVERY ONLY:"));
    assert!(
        output.contains("ciphertext classification and reconciliation checks are not implemented")
    );
    assert!(!output.contains("b.public"));
    assert!(!output.contains("unused"));
    assert!(output.contains("a.plain"));
    assert!(output.contains("c.PUBLIC"));
    assert_eq!(
        output
            .matches("pgp:ABCDEF0123456789ABCDEF0123456789ABCDEF01")
            .count(),
        3
    );
    assert_eq!(
        output
            .matches("pgp:0123456789012345678901234567890123456789")
            .count(),
        3
    );
    assert!(output.find("nested/a.plain").unwrap() < output.find("nested/c.PUBLIC").unwrap());
    assert_eq!(output, good(f.preview()));
}
#[test]
fn targeting_and_git_environment_isolation() {
    let f = Fixture::new();
    assert!(!f.run(&["encrypt"]).status.success());
    assert!(!f.run(&["encrypt", "--dry-run"]).status.success());
    assert!(
        !f.run(&["encrypt", "--dry-run", "secrets/values/z"])
            .status
            .success()
    );
    assert!(!f.run(&["encrypt", "--dry-run", "."]).status.success());
    assert!(
        !f.run(&["encrypt", "--dry-run", "secrets/../outside"])
            .status
            .success()
    );
    good(
        f.command()
            .current_dir(f.0.join("secrets/values"))
            .args(["encrypt", "--dry-run"])
            .output()
            .unwrap(),
    );
    good(f.run(&["encrypt", "--dry-run", "secrets/keys"]));
    f.put("secrets/other/data", b"deliberate non-values target");
    f.link("keys/admin.pgp-key", "secrets/root.pgp-key");
    assert!(good(f.run(&["encrypt", "--dry-run", "secrets/other"])).contains("secrets/other/data"));
    fs::remove_file(f.0.join("secrets/root.pgp-key")).unwrap();
    good(
        f.command()
            .env("GIT_DIR", "/nonexistent")
            .env("GIT_WORK_TREE", "/")
            .env("GIT_COMMON_DIR", "/nonexistent")
            .env("GIT_CONFIG_COUNT", "1")
            .env("GIT_CONFIG_KEY_0", "core.bare")
            .env("GIT_CONFIG_VALUE_0", "true")
            .args(["encrypt", "--dry-run", "secrets/values"])
            .output()
            .unwrap(),
    );
    f.put("secrets/values/zz", b"valid candidate before failure");
    f.put("secrets/values/zzz/bad", b"bad");
    fs::remove_file(f.0.join("secrets/values/admin.pgp-key")).unwrap();
    f.bad("empty effective recipient set");
}
#[test]
fn first_line_and_pgp_validation() {
    let f = Fixture::new();
    for first in [
        "abcdef0123456789abcdef0123456789abcdef01",
        "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
    ] {
        for ending in ["", "\n", "\r\n"] {
            f.put(
                "secrets/keys/admin.pgp-key",
                format!(" \t{first}\t {ending}").as_bytes(),
            );
            good(f.preview());
        }
    }
    f.put(
        "secrets/keys/admin.pgp-key",
        b"abcdef0123456789abcdef0123456789abcdef01\n\xff\0 ignored",
    );
    good(f.preview());
    for bad in [
        "",
        "\n",
        "abcd",
        "0xabcdef0123456789abcdef0123456789abcdef01",
        "abcdef0123456789abcdef0123456789abcdef01!",
        "abcdef0123456789abcdef0123456789abcdef01\r",
        "abcdef0123456789abcdef0123456789abcdef01 #comment",
        "abcdef0123456789abcdef0123456789abcdef01 abcdef0123456789abcdef0123456789abcdef01",
        "abcdef0123456789abcdef0123456789abcdef0\0",
        "abcd ef0123456789abcdef0123456789abcdef01",
        "é",
    ] {
        f.put("secrets/keys/admin.pgp-key", bad.as_bytes());
        f.bad("secrets/keys/admin.pgp-key");
    }
}
#[test]
fn age_native_and_plugin_validation() {
    let f = Fixture::new();
    f.link("../keys/test.age-key", "secrets/values/test.age-key");
    for (hrp, payload) in [
        ("age", vec![42; 32]),
        ("age1yubikey", vec![3; 16]),
        ("age1name1+._-", vec![9; 100]),
        ("age1empty", vec![]),
    ] {
        let value = encode(hrp, &payload);
        f.put("secrets/keys/test.age-key", value.as_bytes());
        assert!(good(f.preview()).contains(&value));
    }
    for (hrp, payload, reason) in [
        ("age", vec![1; 31], "32 bytes"),
        ("age1pq", vec![1; 32], "hybrid"),
        ("age1", vec![1], "plugin name"),
        ("age1bad!", vec![1], "plugin name"),
        ("other", vec![1], "HRP"),
    ] {
        f.put(
            "secrets/keys/test.age-key",
            encode(hrp, &payload).as_bytes(),
        );
        f.bad(reason);
    }
    let valid = encode("age", &[42; 32]);
    let mut invalid = valid.clone().into_bytes();
    let last = invalid.last_mut().unwrap();
    *last = if *last == b'q' { b'p' } else { b'q' };
    f.put("secrets/keys/test.age-key", &invalid);
    f.bad("checksum");
    f.put("secrets/keys/test.age-key", valid.to_uppercase().as_bytes());
    f.bad("lowercase");
    let invalid = encode_words("age1test", &[1], b"qpzry9x8gf2tvdw0s3jn54khce6mua7l");
    f.put("secrets/keys/test.age-key", invalid.as_bytes());
    f.bad("padding");
    let invalid = encode_words("age1test", &[0, 1], b"qpzry9x8gf2tvdw0s3jn54khce6mua7l");
    f.put("secrets/keys/test.age-key", invalid.as_bytes());
    f.bad("padding");
}
#[test]
fn relative_chains_and_fail_closed_links() {
    let f = Fixture::new();
    fs::rename(
        f.0.join("secrets/keys/admin.pgp-key"),
        f.0.join("secrets/keys/actual"),
    )
    .unwrap();
    f.link("actual", "secrets/keys/admin.pgp-key");
    good(f.preview());
    fs::remove_file(f.0.join("secrets/keys/admin.pgp-key")).unwrap();
    for (target, reason) in [
        ("missing", "No such file"),
        ("admin.pgp-key", "cyclic"),
        ("/etc/passwd", "absolute"),
        ("../../secrets/keys/actual", "escapes"),
        (".", "not a regular file"),
    ] {
        f.link(target, "secrets/keys/admin.pgp-key");
        f.bad(reason);
        fs::remove_file(f.0.join("secrets/keys/admin.pgp-key")).unwrap();
    }
    f.link("actual", "secrets/keys/admin.pgp-key");
    f.link("z", "secrets/values/content.public");
    f.bad("unexpected content symlink");
    fs::remove_file(f.0.join("secrets/values/content.public")).unwrap();
    f.link("../keys", "secrets/values/content-directory");
    f.bad("unexpected content symlink");
    assert!(
        !f.run(&["encrypt", "--dry-run", "secrets/values/content-directory"])
            .status
            .success()
    );
}
#[test]
fn nested_boundaries_in_traversal_and_marker_resolution() {
    for file in [false, true] {
        let f = Fixture::new();
        f.put("secrets/nested/record", b"unused");
        if file {
            f.put("secrets/nested/.git", b"gitdir: /irrelevant");
        } else {
            fs::create_dir(f.0.join("secrets/nested/.git")).unwrap();
        }
        fs::remove_file(f.0.join("secrets/values/admin.pgp-key")).unwrap();
        f.link(
            "../nested/../keys/admin.pgp-key",
            "secrets/values/admin.pgp-key",
        );
        f.bad("nested Git");
        fs::remove_file(f.0.join("secrets/values/admin.pgp-key")).unwrap();
        f.link("../keys/admin.pgp-key", "secrets/values/admin.pgp-key");
        fs::rename(
            f.0.join("secrets/nested"),
            f.0.join("secrets/values/nested"),
        )
        .unwrap();
        f.bad("nested Git");
    }
}
#[test]
fn long_chains_cycles_and_repeated_links() {
    let f = Fixture::new();
    fs::remove_file(f.0.join("secrets/values/admin.pgp-key")).unwrap();
    for i in 0..150 {
        let target = if i == 149 {
            "../keys/admin.pgp-key".to_owned()
        } else {
            format!("chain-{}", i + 1)
        };
        f.link(&target, &format!("secrets/values/chain-{i}"));
    }
    f.link("chain-0", "secrets/values/admin.pgp-key");
    fs::rename(f.0.join("secrets/values"), f.0.join("secrets/links")).unwrap();
    fs::remove_file(f.0.join("secrets/links/z")).unwrap();
    f.put("secrets/values/z", b"opaque");
    f.link("../links/admin.pgp-key", "secrets/values/admin.pgp-key");
    good(f.preview());
    fs::remove_file(f.0.join("secrets/links/chain-149")).unwrap();
    f.link("chain-0/extra", "secrets/links/chain-149");
    f.bad("cyclic");
    fs::remove_file(f.0.join("secrets/values/admin.pgp-key")).unwrap();
    f.link(".", "secrets/keys/repeated");
    f.link(
        "../keys/repeated/repeated/admin.pgp-key",
        "secrets/values/admin.pgp-key",
    );
    good(f.preview());
}

#[test]
fn late_errors_emit_no_listing_and_ancestor_markers_are_checked() {
    let f = Fixture::new();
    f.link("missing", "secrets/values/zz-unexpected");
    f.bad("zz-unexpected");
    fs::remove_file(f.0.join("secrets/values/zz-unexpected")).unwrap();
    f.link("missing", "secrets/bad.age-key");
    f.bad("bad.age-key");
    fs::remove_file(f.0.join("secrets/bad.age-key")).unwrap();
    fs::rename(f.0.join("secrets"), f.0.join("real-secrets")).unwrap();
    f.link("real-secrets", "secrets");
    f.bad("real directory");
}

#[test]
fn linked_worktree() {
    let f = Fixture::new();
    git(
        &f.0,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "--allow-empty",
            "-qm",
            "fixture",
        ],
    );
    let worktree = f.0.join("linked");
    git(
        &f.0,
        &["worktree", "add", "-qb", "test", worktree.to_str().unwrap()],
    );
    fs::create_dir_all(worktree.join("secrets/values")).unwrap();
    fs::write(worktree.join("secrets/values/data"), b"do not read").unwrap();
    fs::write(
        worktree.join("secrets/key.pgp-key"),
        b"0123456789012345678901234567890123456789",
    )
    .unwrap();
    symlink(
        "../key.pgp-key",
        worktree.join("secrets/values/key.pgp-key"),
    )
    .unwrap();
    good(
        f.command()
            .current_dir(&worktree)
            .args(["encrypt", "--dry-run", "secrets/values"])
            .output()
            .unwrap(),
    );
}
#[test]
fn candidates_are_never_opened_and_backends_are_not_run() {
    let f = Fixture::new();
    let candidate = f.0.join("secrets/values/z");
    fs::set_permissions(&candidate, fs::Permissions::from_mode(0o0)).unwrap();
    assert!(
        fs::File::open(&candidate).is_err(),
        "run this test as an unprivileged user to prove absence of candidate reads"
    );
    let shell = std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|p| p.join("sh"))
        .find(|p| p.is_file())
        .unwrap();
    let spy = format!(
        "#!{}\nprintf invoked > \"$SECRETCTL_BACKEND_LOG\"\nexit 99\n",
        shell.display()
    );
    for backend in ["sops", "gpg", "age", "age-plugin-test"] {
        f.put(&format!("bin/{backend}"), spy.as_bytes());
        fs::set_permissions(
            f.0.join(format!("bin/{backend}")),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    }
    let path = std::env::join_paths(
        std::iter::once(f.0.join("bin"))
            .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .unwrap();
    let before = fs::metadata(&candidate).unwrap();
    f.put(
        "secrets/keys/plugin.age-key",
        encode("age1test", &[3; 12]).as_bytes(),
    );
    f.link("../keys/plugin.age-key", "secrets/values/plugin.age-key");
    let output = good(
        f.command()
            .env("PATH", path)
            .env("SECRETCTL_BACKEND_LOG", f.0.join("backend-invoked"))
            .env("GNUPGHOME", f.0.join("private-must-not-exist"))
            .env("SOPS_AGE_KEY_FILE", f.0.join("private-must-not-exist"))
            .args(["encrypt", "--dry-run", "secrets/values"])
            .output()
            .unwrap(),
    );
    assert!(!f.0.join("backend-invoked").exists());
    assert_eq!(
        before.modified().unwrap(),
        fs::metadata(&candidate).unwrap().modified().unwrap()
    );
    fs::set_permissions(&candidate, fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(fs::read(&candidate).unwrap(), b"arbitrary\0\xff");
    f.put("secrets/values/z", b"completely different data");
    assert_eq!(output, good(f.preview()));
}

fn non_utf8_supported(result: std::io::Result<()>) -> std::io::Result<bool> {
    match result {
        Ok(()) => Ok(true),
        // Linux EILSEQ: UTF-8-only filesystems cannot represent this fixture.
        Err(e) if cfg!(target_os = "linux") && e.raw_os_error() == Some(84) => Ok(false),
        Err(e) => Err(e),
    }
}

#[test]
fn filename_capability_only_accepts_eilseq() {
    assert!(non_utf8_supported(Ok(())).unwrap());
    if cfg!(target_os = "linux") {
        assert!(!non_utf8_supported(Err(std::io::Error::from_raw_os_error(84))).unwrap());
    }
    for code in [1, 2, 13, 28] {
        assert_eq!(
            non_utf8_supported(Err(std::io::Error::from_raw_os_error(code)))
                .unwrap_err()
                .raw_os_error(),
            Some(code)
        );
    }
}

#[test]
fn non_utf8_filenames_are_discovered_and_markers_are_deduplicated() {
    let f = Fixture::new();
    let path = |name: &[u8]| {
        f.0.join("secrets/values")
            .join(std::ffi::OsStr::from_bytes(name))
    };
    if !non_utf8_supported(fs::write(
        path(b"ignored-\xff.pgp-key"),
        b"not a declaration",
    ))
    .unwrap()
    {
        eprintln!("Skipping non-UTF-8 filename coverage: filesystem rejects names with EILSEQ");
        return;
    }
    symlink("../keys/admin.pgp-key", path(b"duplicate-\xff.pgp-key")).unwrap();
    let before = good(f.preview());
    assert!(!before.contains("ignored-"));
    assert_eq!(
        before
            .matches("pgp:ABCDEF0123456789ABCDEF0123456789ABCDEF01")
            .count(),
        1
    );
    fs::write(path(b"file-\xff"), b"opaque").unwrap();
    let after = good(f.preview());
    assert!(after.contains("file-"));
    assert_eq!(
        after
            .matches("pgp:ABCDEF0123456789ABCDEF0123456789ABCDEF01")
            .count(),
        2
    );
}

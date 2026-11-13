//! End-to-end tests against real OpenSSH tools with disposable keys and agents.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

use serde_json::json;

struct Agent {
    child: Child,
    socket: PathBuf,
}

impl Agent {
    fn start(dir: &Path, name: &str) -> Agent {
        let socket = dir.join(name);
        let child = Command::new("ssh-agent")
            .arg("-D")
            .arg("-a")
            .arg(&socket)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("ssh-agent");
        let deadline = Instant::now() + Duration::from_secs(10);
        while !socket.exists() {
            assert!(Instant::now() < deadline, "ssh-agent did not start");
            sleep(Duration::from_millis(20));
        }
        Agent { child, socket }
    }

    fn add(&self, key: &Path) {
        let st = Command::new("ssh-add")
            .arg(key)
            .env("SSH_AUTH_SOCK", &self.socket)
            .stderr(Stdio::null())
            .status()
            .unwrap();
        assert!(st.success());
    }

    fn list(&self) -> String {
        let out = Command::new("ssh-add")
            .arg("-L")
            .env("SSH_AUTH_SOCK", &self.socket)
            .output()
            .unwrap();
        String::from_utf8(out.stdout).unwrap()
    }
}

impl Drop for Agent {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct Fixture {
    dir: tempfile::TempDir,
    runtime: PathBuf,
}

fn keygen(path: &Path, kind: &str) -> String {
    let st = Command::new("ssh-keygen")
        .args(["-q", "-t", kind, "-N", "", "-C", "test", "-f"])
        .arg(path)
        .status()
        .unwrap();
    assert!(st.success());
    let pubkey = fs::read_to_string(path.with_extension("pub")).unwrap();
    pubkey
        .split_whitespace()
        .take(2)
        .collect::<Vec<_>>()
        .join(" ")
}

impl Fixture {
    fn new() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let runtime = dir.path().join("runtime");
        fs::create_dir(&runtime).unwrap();
        let f = Fixture { dir, runtime };
        let ca = keygen(&f.path("ca"), "ed25519");
        let ca2 = keygen(&f.path("ca2"), "ecdsa");
        let other = keygen(&f.path("other"), "ed25519");
        keygen(&f.path("subject"), "ed25519");
        f.write_json(
            "registry.json",
            json!({
                "schema": "swan-lake-ssh-trust-registry",
                "version": 1,
                "keys": {
                    "test-ca": { "publicKey": ca, "description": "", "trust": { "login": true, "userCa": true } },
                    "test-ca2": { "publicKey": ca2, "description": "", "trust": { "login": true, "userCa": true } },
                    "not-ca": { "publicKey": other, "description": "", "trust": { "login": false, "userCa": false } }
                }
            }),
        );
        let p = |name: &str| json!({ "name": name, "options": null });
        f.write_json(
            "manifest.json",
            json!({
                "schema": "swan-lake-ssh-trust-manifest",
                "version": 1,
                "userCaKeys": [ { "id": "test-ca", "publicKey": ca }, { "id": "test-ca2", "publicKey": ca2 } ],
                "machines": {
                    "alpha": {
                        "root": { "defaultPrincipal": "alpha--root", "principals": [p("alpha--root"), p("root")] },
                        "binarin": { "defaultPrincipal": "alpha--binarin", "principals": [p("alpha--binarin"), p("root"), p("binarin")] }
                    },
                    "beta": {
                        "root": { "defaultPrincipal": null, "principals": [p("root")] },
                        "builder": { "defaultPrincipal": null, "principals": [
                            { "name": "builder", "options": "restrict,command=\"nix-daemon --stdio\"" }
                        ] }
                    },
                    "gamma": {
                        "root": { "defaultPrincipal": "gamma--root", "principals": [p("gamma--root"), p("alpha--binarin")] }
                    }
                }
            }),
        );
        f
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    fn write_json(&self, name: &str, v: serde_json::Value) {
        fs::write(self.path(name), serde_json::to_vec_pretty(&v).unwrap()).unwrap();
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_ssh-ca"))
            .args(args)
            .env("SSH_CA_REGISTRY", self.path("registry.json"))
            .env("XDG_RUNTIME_DIR", &self.runtime)
            .env_remove("SSH_AUTH_SOCK")
            .stdin(Stdio::null())
            .output()
            .unwrap()
    }

    fn sign(&self, extra: &[&str]) -> Output {
        let subject = self.path("subject.pub");
        let ca = self.path("ca");
        let mut args = vec![
            "sign",
            subject.to_str().unwrap(),
            "--ca",
            "test-ca",
            "--ca-key",
            ca.to_str().unwrap(),
            "--yes",
        ];
        args.extend_from_slice(extra);
        self.run(&args)
    }

    fn manifest(&self) -> String {
        self.path("manifest.json").to_str().unwrap().to_string()
    }

    fn cert(&self) -> PathBuf {
        self.path("subject-cert.pub")
    }

    fn runtime_is_empty(&self) -> bool {
        fs::read_dir(&self.runtime).unwrap().next().is_none()
    }
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

/// `ssh-keygen -L` view of a certificate: (principals, key id, signing CA line).
fn inspect(cert: &Path) -> (Vec<String>, String) {
    let out = Command::new("ssh-keygen")
        .arg("-L")
        .arg("-f")
        .arg(cert)
        .output()
        .unwrap();
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).unwrap();
    let mut principals = Vec::new();
    let mut in_principals = false;
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with("Principals:") {
            in_principals = true;
            continue;
        }
        if in_principals {
            if t.ends_with(':') || t.starts_with("Critical Options") {
                in_principals = false;
            } else {
                principals.push(t.to_string());
            }
        }
    }
    (principals, text)
}

#[test]
fn no_subcommand_shows_help_only() {
    let f = Fixture::new();
    let o = f.run(&[]);
    assert_eq!(o.status.code(), Some(2));
    assert!(stderr(&o).contains("Usage"));
    assert!(f.runtime_is_empty());
}

#[test]
fn sign_with_manifest_target() {
    let f = Fixture::new();
    let m = f.manifest();
    let o = f.sign(&["--policy", &m, "--target", "alpha/root"]);
    assert!(o.status.success(), "{}", stderr(&o));
    let (principals, text) = inspect(&f.cert());
    assert_eq!(principals, vec!["alpha--root"]);
    assert!(text.contains("Type: ssh-ed25519-cert-v01@openssh.com user certificate"));
    assert!(f.runtime_is_empty(), "staging directory left behind");

    // Never replaces an existing certificate implicitly.
    let o = f.sign(&["--policy", &m, "--target", "alpha/root"]);
    assert!(!o.status.success());
    assert!(stderr(&o).contains("--force"));
    let o = f.sign(&[
        "--policy",
        &m,
        "--target",
        "alpha/binarin",
        "--allow-shared-principals",
        "--force",
    ]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert_eq!(inspect(&f.cert()).0, vec!["alpha--binarin"]);
    assert!(
        stderr(&o).contains("alpha--binarin: gamma/root"),
        "{}",
        stderr(&o)
    );
}

#[test]
fn sign_with_ecdsa_ca_and_cartesian_union() {
    let f = Fixture::new();
    let subject = f.path("subject.pub");
    let ca2 = f.path("ca2");
    let out = f.path("out-cert.pub");
    let o = f.run(&[
        "sign",
        subject.to_str().unwrap(),
        "--ca",
        "test-ca2",
        "--ca-key",
        ca2.to_str().unwrap(),
        "--default-principals",
        "--cartesian",
        "a,b",
        "root,binarin",
        "--cartesian",
        "c",
        "deploy",
        "--target",
        "a/root",
        "--valid-for",
        "2h",
        "--output",
        out.to_str().unwrap(),
        "--yes",
    ]);
    assert!(o.status.success(), "{}", stderr(&o));
    let (principals, text) = inspect(&out);
    assert_eq!(
        principals,
        vec![
            "a--binarin",
            "a--root",
            "b--binarin",
            "b--root",
            "c--deploy"
        ]
    );
    assert!(text.contains("Signing CA: ECDSA"));
}

#[test]
fn plan_touches_nothing() {
    let f = Fixture::new();
    let m = f.manifest();
    let missing = f.path("no-such-agent");
    let o = f.run(&[
        "session",
        "--ca",
        "test-ca",
        "--ca-agent",
        missing.to_str().unwrap(),
        "--agent",
        missing.to_str().unwrap(),
        "--policy",
        &m,
        "--target",
        "alpha/root",
        "--plan",
    ]);
    assert!(o.status.success(), "{}", stderr(&o));
    let stdout = String::from_utf8_lossy(&o.stdout);
    assert!(stdout.contains("Principals:  alpha--root"), "{stdout}");
    assert!(stdout.contains("Agent TTL:   7d00h"), "{stdout}");
    assert!(f.runtime_is_empty());

    let o = f.sign(&["--policy", &m, "--target", "alpha/root", "--plan"]);
    // --yes and --plan are exclusive.
    assert_eq!(o.status.code(), Some(2));
    let subject = f.path("subject.pub");
    let ca = f.path("ca");
    let o = f.run(&[
        "sign",
        subject.to_str().unwrap(),
        "--ca",
        "test-ca",
        "--ca-key",
        ca.to_str().unwrap(),
        "--policy",
        &m,
        "--target",
        "alpha/root",
        "--plan",
    ]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert!(!f.cert().exists());
}

#[test]
fn target_resolution_failures() {
    let f = Fixture::new();
    let m = f.manifest();
    let cases: &[(&[&str], &str)] = &[
        (
            &["--policy", &m, "--target", "delta/root"],
            "not in the manifest: delta/root",
        ),
        (&["--target", "alpha/root"], "needs --policy"),
        (
            &["--policy", &m, "--target", "beta/root"],
            "--allow-shared-principals",
        ),
        // gamma/root also accepts alpha--binarin.
        (
            &["--policy", &m, "--target", "alpha/binarin"],
            "also accepted elsewhere",
        ),
        (
            &[
                "--policy",
                &m,
                "--target",
                "alpha/root",
                "--valid-for",
                "1y",
            ],
            "invalid duration",
        ),
        (
            &[
                "--policy",
                &m,
                "--target",
                "alpha/root",
                "--valid-for",
                "400d",
            ],
            "--valid-for must be between",
        ),
        (
            &["--policy", &m, "--principal", "root"],
            "--allow-shared-principals",
        ),
        (&["--policy", &m], "nothing to certify"),
    ];
    for (args, want) in cases {
        let o = f.sign(args);
        assert!(!o.status.success(), "{args:?}");
        assert!(stderr(&o).contains(want), "{args:?}: {}", stderr(&o));
        assert!(!f.cert().exists());
    }
    // Raw principals and targets are separate modes.
    let o = f.sign(&["--principal", "x", "--target", "alpha/root"]);
    assert_eq!(o.status.code(), Some(2));
}

#[test]
fn shared_principals_need_acknowledgement() {
    let f = Fixture::new();
    let m = f.manifest();
    let o = f.sign(&["--policy", &m, "--target", "beta/builder"]);
    assert!(!o.status.success());
    let o = f.sign(&[
        "--policy",
        &m,
        "--target",
        "beta/builder",
        "--target",
        "beta/root",
        "--allow-shared-principals",
    ]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert_eq!(inspect(&f.cert()).0, vec!["builder", "root"]);
    let err = stderr(&o);
    assert!(
        err.contains("[server options: restrict,command=\"nix-daemon --stdio\"]"),
        "{err}"
    );
    assert!(err.contains("root: alpha/binarin, alpha/root"), "{err}");
}

#[test]
fn raw_principals_exactly() {
    let f = Fixture::new();
    let o = f.sign(&[
        "--principal",
        "zeta",
        "--principal",
        "eta",
        "--principal",
        "zeta",
    ]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert_eq!(inspect(&f.cert()).0, vec!["eta", "zeta"]);
    assert!(stderr(&o).contains("which servers accept these principals is unknown"));
}

#[test]
fn ca_selection_is_strict() {
    let f = Fixture::new();
    let m = f.manifest();
    let subject = f.path("subject.pub");
    let wrong = f.path("ca2");
    // The key file holds another CA than requested.
    let o = f.run(&[
        "sign",
        subject.to_str().unwrap(),
        "--ca",
        "test-ca",
        "--ca-key",
        wrong.to_str().unwrap(),
        "--policy",
        &m,
        "--target",
        "alpha/root",
        "--yes",
    ]);
    assert!(!o.status.success());
    assert!(stderr(&o).contains("not test-ca"), "{}", stderr(&o));
    // A registry key without the userCa role.
    let other = f.path("other");
    let o = f.run(&[
        "sign",
        subject.to_str().unwrap(),
        "--ca",
        "not-ca",
        "--ca-key",
        other.to_str().unwrap(),
        "--principal",
        "x",
        "--yes",
    ]);
    assert!(
        stderr(&o).contains("not a trusted user CA"),
        "{}",
        stderr(&o)
    );
    // An agent that does not hold the CA.
    let agent = Agent::start(f.dir.path(), "ca.sock");
    agent.add(&f.path("ca2"));
    let o = f.run(&[
        "sign",
        subject.to_str().unwrap(),
        "--ca",
        "test-ca",
        "--ca-agent",
        agent.socket.to_str().unwrap(),
        "--principal",
        "x",
        "--yes",
    ]);
    assert!(
        stderr(&o).contains("does not hold test-ca"),
        "{}",
        stderr(&o)
    );
    assert!(!f.cert().exists());
    assert!(f.runtime_is_empty());
}

#[test]
fn manifest_must_match_schema_version_and_ca() {
    let f = Fixture::new();
    let mut doc: serde_json::Value =
        serde_json::from_slice(&fs::read(f.path("manifest.json")).unwrap()).unwrap();
    doc["version"] = json!(2);
    f.write_json("v2.json", doc.clone());
    doc["version"] = json!(1);
    doc["userCaKeys"] = json!([]);
    f.write_json("untrusted.json", doc);
    for (file, want) in [
        ("v2.json", "unsupported"),
        ("untrusted.json", "do not trust CA"),
        ("registry.json", "expected schema"),
    ] {
        let p = f.path(file);
        let o = f.sign(&["--policy", p.to_str().unwrap(), "--target", "alpha/root"]);
        assert!(stderr(&o).contains(want), "{file}: {}", stderr(&o));
    }
}

#[test]
fn session_installs_certified_identity_with_lifetime() {
    let f = Fixture::new();
    let m = f.manifest();
    let ca_agent = Agent::start(f.dir.path(), "ca.sock");
    ca_agent.add(&f.path("ca"));
    let dest = Agent::start(f.dir.path(), "dest.sock");
    let ca_before = ca_agent.list();

    let o = f.run(&[
        "session",
        "--ca",
        "test-ca",
        "--ca-agent",
        ca_agent.socket.to_str().unwrap(),
        "--agent",
        dest.socket.to_str().unwrap(),
        "--policy",
        &m,
        "--cartesian",
        "alpha,gamma",
        "root",
        "--agent-lifetime",
        "3s",
        "--yes",
    ]);
    assert!(o.status.success(), "{}", stderr(&o));
    let listed = dest.list();
    let cert_line = listed
        .lines()
        .find(|l| l.starts_with("ssh-ed25519-cert-v01@openssh.com "))
        .unwrap_or_else(|| panic!("no certificate in destination agent: {listed}"));
    let cert = f.path("listed-cert.pub");
    fs::write(&cert, format!("{cert_line}\n")).unwrap();
    assert_eq!(inspect(&cert).0, vec!["alpha--root", "gamma--root"]);
    assert_eq!(
        ca_agent.list(),
        ca_before,
        "CA agent must not receive the session identity"
    );
    assert!(f.runtime_is_empty(), "staging directory left behind");

    // The agent forgets the identity after the explicit lifetime.
    let deadline = Instant::now() + Duration::from_secs(15);
    while dest.list().contains("cert-v01") {
        assert!(
            Instant::now() < deadline,
            "identity outlived its agent lifetime"
        );
        sleep(Duration::from_millis(250));
    }
}

#[test]
fn session_needs_reachable_destination_agent() {
    let f = Fixture::new();
    let ca_agent = Agent::start(f.dir.path(), "ca.sock");
    ca_agent.add(&f.path("ca"));
    let missing = f.path("missing.sock");
    let o = f.run(&[
        "session",
        "--ca",
        "test-ca",
        "--ca-agent",
        ca_agent.socket.to_str().unwrap(),
        "--agent",
        missing.to_str().unwrap(),
        "--principal",
        "x",
        "--yes",
    ]);
    assert!(!o.status.success());
    assert!(stderr(&o).contains("no SSH agent at"), "{}", stderr(&o));
    assert!(f.runtime_is_empty());

    let o = f.run(&[
        "session",
        "--ca",
        "test-ca",
        "--ca-agent",
        ca_agent.socket.to_str().unwrap(),
        "--principal",
        "x",
        "--yes",
    ]);
    assert!(
        stderr(&o).contains("SSH_AUTH_SOCK is unset"),
        "{}",
        stderr(&o)
    );

    let dest = Agent::start(f.dir.path(), "dest.sock");
    let o = f.run(&[
        "session",
        "--ca",
        "test-ca",
        "--ca-agent",
        ca_agent.socket.to_str().unwrap(),
        "--agent",
        dest.socket.to_str().unwrap(),
        "--principal",
        "x",
        "--valid-for",
        "1h",
        "--agent-lifetime",
        "2h",
        "--yes",
    ]);
    assert!(
        stderr(&o).contains("exceeds the certificate validity"),
        "{}",
        stderr(&o)
    );
    assert!(dest.list().trim().is_empty() || dest.list().contains("no identities"));
}

#[test]
fn staging_requires_runtime_dir() {
    let f = Fixture::new();
    let subject = f.path("subject.pub");
    let ca = f.path("ca");
    let o = Command::new(env!("CARGO_BIN_EXE_ssh-ca"))
        .args([
            "sign",
            subject.to_str().unwrap(),
            "--ca",
            "test-ca",
            "--ca-key",
            ca.to_str().unwrap(),
            "--principal",
            "x",
            "--yes",
        ])
        .env("SSH_CA_REGISTRY", f.path("registry.json"))
        .env_remove("XDG_RUNTIME_DIR")
        .output()
        .unwrap();
    assert!(!o.status.success());
    assert!(stderr(&o).contains("XDG_RUNTIME_DIR is not set"));
    assert!(!f.cert().exists());
}

//! `swl status` on a disposable forge, through every stage of one change:
//! local, needs publish, pushed, MR open, CI, merged, promoted, landing
//! pushed and landed; with and without an fj login for the private consumer,
//! and `--from` without a workspace.

mod support;

use std::{fs, os::unix::fs::PermissionsExt, path::Path};

use support::{Forge, stdout};

/// A line of the status without its tree glyphs.
fn unglyphed(line: &str) -> &str {
    line.trim_start_matches(['│', '├', '└', '─', ' '])
}

/// The status row of `name`: its line without the tree glyphs and the input
/// path label, the columns' padding collapsed to two spaces: `<name>
/// <stage>  waits: <who>  next: <step>`.
fn row(out: &str, name: &str) -> String {
    let want = format!("{name}  ");
    for l in out.lines() {
        let mut l = unglyphed(l).to_string();
        while l.contains("   ") {
            l = l.replace("   ", "  ");
        }
        if let Some((path, rest)) = l.split_once(": ")
            && !path.contains(' ')
            && rest.starts_with(&want)
        {
            return rest.to_string();
        }
        if l.starts_with(&want) {
            return l;
        }
    }
    panic!("no row {name:?} in\n{out}")
}

/// The first detail line that starts with `prefix`, without the glyphs.
fn detail<'a>(out: &'a str, prefix: &str) -> &'a str {
    out.lines()
        .map(unglyphed)
        .find(|l| l.starts_with(prefix))
        .unwrap_or_else(|| panic!("no line starting with {prefix:?} in\n{out}"))
}

fn assert_row(out: &str, name: &str, stage: &str, next: &str, waits: &str) {
    let r = row(out, name);
    assert!(
        r.starts_with(&format!("{name}  {stage}  waits: {waits}  next: ")),
        "stage and waits of {name}: want {stage:?}, {waits:?} in {r:?}\n{out}"
    );
    assert!(
        r.contains(next),
        "next of {name}: want {next:?} in {r:?}\n{out}"
    );
}

struct Change {
    f: Forge,
    ws: std::path::PathBuf,
    consumer: std::path::PathBuf,
    common: std::path::PathBuf,
    keys: String,
}

impl Change {
    fn status(&self, login: bool) -> String {
        let ws = self.ws.display().to_string();
        let args = ["status", "--workspace", &ws];
        let out = if login {
            self.f
                .swl_env(&self.f.root, &args, &[("SWL_FJ_KEYS", &self.keys)])
        } else {
            self.f.swl(&self.f.root, &args)
        };
        // The token never reaches the output.
        assert!(
            !support::text(&out).contains(support::TOKEN),
            "{}",
            support::text(&out)
        );
        stdout(&out)
    }

    fn status_from(&self, from: &str) -> String {
        stdout(&self.f.swl_env(
            &self.f.root,
            &["status", "--from", from],
            &[("SWL_FJ_KEYS", &self.keys)],
        ))
    }

    fn commit(&self, dir: &Path, value: &str) -> String {
        fs::write(dir.join("value.txt"), value).unwrap();
        self.f.git(dir, &["commit", "-qam", value]);
        self.f.git(dir, &["rev-parse", "HEAD"])
    }

    fn push(&self, dir: &Path) {
        self.f.git(dir, &["push", "-q", "origin", "sl-0010-x"]);
    }
}

#[test]
fn status_follows_a_change_from_local_to_landed() {
    let f = Forge::new("status");
    f.api.make_private("swan-lake");
    let keys = f.login().display().to_string();
    let work = f.root.join("work");
    fs::create_dir_all(&work).unwrap();
    stdout(&f.swl(
        &work,
        &["start", "SL-0010-x", "swan-lake", "swan-lake-common@lib"],
    ));
    let ws = work.join("SL-0010-x");
    let t = Change {
        consumer: ws.join("swan-lake"),
        common: ws.join("swan-lake-common@lib"),
        ws,
        f,
        keys,
    };
    let (common, consumer) = ("swan-lake-common@lib", "swan-lake");

    // Nothing yet: both at the base.
    let out = t.status(true);
    assert_row(
        &out,
        common,
        "no MR needed (in the rev swan-lake locks)",
        "-",
        "nobody",
    );
    assert_row(
        &out,
        consumer,
        "no change (HEAD is in origin/master)",
        "-",
        "nobody",
    );
    assert!(detail(&out, "HEAD ").contains("sl-0010-x  clean"), "{out}");
    assert!(out.contains("  swan-lake: lib locked "), "{out}");

    // Local commits.
    let c2 = t.commit(&t.common, "c2");
    t.commit(&t.consumer, "consumer-2");
    let out = t.status(true);
    assert_row(&out, common, "local", "swl publish swan-lake", "anyone");
    assert_row(&out, consumer, "local", "swl publish swan-lake", "anyone");
    assert!(out.contains("swan-lake: lib locked "), "{out}");
    assert!(out.contains(", ahead 1, behind 0"), "{out}");

    // Pushed by hand, without a manifest entry: needs publish.
    t.push(&t.common);
    t.push(&t.consumer);
    let out = t.status(true);
    assert_row(
        &out,
        common,
        "needs publish",
        "swl publish swan-lake",
        "anyone",
    );
    assert_row(
        &out,
        consumer,
        "needs publish",
        "swl publish swan-lake",
        "anyone",
    );
    assert!(
        out.contains("changed without a swl.toml entry: lib"),
        "{out}"
    );

    // The common MR as a draft, and the manifest entry, not pushed yet.
    let forge_common = t.f.forge_repo("swan-lake-common");
    t.f.git(&forge_common, &["update-ref", "refs/pull/1/head", &c2]);
    t.f.api
        .set_pr("swan-lake-common", 1, "draft", "sl-0010-x", &c2, None);
    fs::write(
        t.consumer.join("swl.toml"),
        "[inputs.lib]\nrepo = \"swan-lake-common\"\nmr = 1\n",
    )
    .unwrap();
    t.f.git(&t.consumer, &["add", "swl.toml"]);
    t.f.git(&t.consumer, &["commit", "-qm", "swl: manifest"]);
    let out = t.status(true);
    assert_row(
        &out,
        common,
        "MR open (draft)",
        "mark swan-lake-common#1 ready",
        "anyone",
    );
    assert_row(
        &out,
        consumer,
        "needs publish",
        "swl publish swan-lake",
        "anyone",
    );
    assert!(out.contains("swl.toml: lib swan-lake-common#1"), "{out}");

    // Pushed: no consumer MR yet.
    t.push(&t.consumer);
    let consumer_head = t.f.git(&t.consumer, &["rev-parse", "HEAD"]);
    let out = t.status(true);
    assert_row(&out, consumer, "pushed", "swl publish swan-lake", "anyone");

    // The consumer MR: private, so readable only with the fj login.
    let forge_consumer = t.f.forge_repo("swan-lake");
    t.f.git(
        &forge_consumer,
        &["update-ref", "refs/pull/2/head", &consumer_head],
    );
    t.f.api
        .set_pr("swan-lake", 2, "open", "sl-0010-x", &consumer_head, None);
    t.f.api.set_status("swan-lake", &consumer_head, "success");
    let out = t.status(false);
    assert_row(
        &out,
        consumer,
        "pushed",
        "wait for swan-lake-common#1 (mark swan-lake-common#1 ready)",
        "anyone",
    );
    assert!(
        out.contains("swan-lake#2: unknown (not readable anonymously, and no fj login"),
        "{out}"
    );
    let out = t.status(true);
    assert_row(
        &out,
        consumer,
        "CI green",
        "wait for swan-lake-common#1 (mark swan-lake-common#1 ready)",
        "anyone",
    );
    assert!(out.contains("swan-lake#2: open, sl-0010-x at "), "{out}");
    let seen = t.f.api.seen_auth.lock().unwrap().clone();
    assert!(
        seen.iter().all(|(p, a)| !p.contains("/swan-lake/")
            || a.is_none()
            || a.as_deref() == Some(&format!("token {}", support::TOKEN))),
        "{seen:?}"
    );
    // SWL_LOG=debug: the commands, the forge requests and the verdicts on
    // stderr, never the token; stdout as without it.
    let ws_s = t.ws.display().to_string();
    let debug = t.f.swl_env(
        &t.f.root,
        &["status", "--workspace", &ws_s],
        &[("SWL_FJ_KEYS", &t.keys), ("SWL_LOG", "debug")],
    );
    let err = String::from_utf8_lossy(&debug.stderr);
    assert!(!err.contains(support::TOKEN), "{err}");
    assert!(
        err.contains("swl::cmd") && err.contains(" rev-parse "),
        "{err}"
    );
    assert!(
        err.contains("swl::forge") && err.contains("authed=true") && err.contains("status=200"),
        "{err}"
    );
    assert!(err.contains("stage=CI green"), "{err}");
    assert_eq!(stdout(&debug), out);

    // Ready, CI pending, then green.
    t.f.api
        .set_pr("swan-lake-common", 1, "open", "sl-0010-x", &c2, None);
    t.f.api.set_status("swan-lake-common", &c2, "pending");
    let out = t.status(true);
    assert_row(
        &out,
        common,
        "CI pending",
        "wait for CI, or request merge when checks succeed",
        "nobody",
    );
    t.f.api.set_status("swan-lake-common", &c2, "failure");
    let out = t.status(true);
    assert_row(
        &out,
        common,
        "CI failing (failure)",
        "fix it, then swl publish swan-lake",
        "anyone",
    );
    t.f.api.set_status("swan-lake-common", &c2, "success");
    let out = t.status(true);
    assert_row(
        &out,
        common,
        "CI green",
        "merge swan-lake-common#1",
        "owner",
    );
    assert_row(
        &out,
        consumer,
        "CI green",
        "wait for swan-lake-common#1 (merge swan-lake-common#1)",
        "owner",
    );

    // A rejected fj login: the private consumer's MR is unknown, the public
    // common is read anonymously as before.
    let ws_s = t.ws.display().to_string();
    let bad = t.f.login_with("bad-keys.json", "wrong-token", None);
    let out = stdout(&t.f.swl_env(
        &t.f.root,
        &["status", "--workspace", &ws_s],
        &[("SWL_FJ_KEYS", bad.to_str().unwrap())],
    ));
    assert_row(
        &out,
        consumer,
        "unknown (fj login rejected: run fj auth login)",
        "fj auth login",
        "anyone",
    );
    assert_row(
        &out,
        common,
        "CI green",
        "merge swan-lake-common#1",
        "owner",
    );
    // An expired OAuth login is no login, with one warning.
    let expired = t.f.login_with(
        "expired-keys.json",
        support::TOKEN,
        Some(serde_json::json!([2000, 1, 0, 0, 0, 0, 0, 0, 0])),
    );
    let out = t.f.swl_env(
        &t.f.root,
        &["status", "--workspace", &ws_s],
        &[("SWL_FJ_KEYS", expired.to_str().unwrap())],
    );
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    assert_eq!(err.matches("not using fj's login").count(), 1, "{err}");
    assert!(err.contains("expired"), "{err}");
    assert!(
        stdout(&out).contains("swan-lake#2: unknown (not readable anonymously, and no fj login"),
        "{}",
        support::text(&out)
    );

    // Merged into master, not promoted to `current` yet.
    t.f.git(&forge_common, &["update-ref", "refs/heads/master", &c2]);
    t.f.api.set_pr(
        "swan-lake-common",
        1,
        "merged",
        "refs/pull/1/head",
        &c2,
        Some(&c2),
    );
    let out = t.status(true);
    assert_row(&out, common, "merged", "promote current", "owner");
    assert!(out.contains("not in current yet"), "{out}");

    // Promoted; the declared source (here the forge itself) has it.
    t.f.git(&forge_common, &["update-ref", "refs/heads/current", &c2]);
    let out = t.status(true);
    assert_row(&out, common, "promoted", "swl land", "anyone");
    assert!(out.contains(", in current; file://"), "{out}");
    assert!(out.contains("has it"), "{out}");
    assert_row(&out, consumer, "CI green", "swl land", "anyone");
    // The same, from the published MR alone.
    let out = t.status_from("swan-lake#2");
    assert_row(&out, "swan-lake-common#1", "promoted", "swl land", "anyone");
    assert_row(&out, consumer, "CI green", "swl land", "anyone");
    assert!(out.contains("the head of sl-0010-x"), "{out}");
    // A consumer MR merged with its entry still in swl.toml (merged by
    // hand): the entry needs nothing more.
    t.f.git(
        &forge_consumer,
        &["update-ref", "refs/pull/3/head", &consumer_head],
    );
    t.f.api.set_pr(
        "swan-lake",
        3,
        "merged",
        "refs/pull/3/head",
        &consumer_head,
        Some(&consumer_head),
    );
    let out = t.status_from("swan-lake#3");
    assert_row(&out, consumer, "landed", "-", "nobody");
    assert_row(&out, "swan-lake-common#1", "landed", "-", "nobody");
    assert!(out.contains("  landed in swan-lake"), "{out}");
    t.f.git(&forge_consumer, &["update-ref", "-d", "refs/pull/3/head"]);

    // The landing commit, local, then pushed to the branch.
    stdout(&t.f.swl(&t.consumer, &["finalize"]));
    let out = t.status(true);
    assert_row(&out, consumer, "needs publish", "swl land", "anyone");
    assert_row(
        &out,
        common,
        "no MR needed (in the rev swan-lake locks)",
        "-",
        "nobody",
    );
    t.push(&t.consumer);
    let landing = t.f.git(&t.consumer, &["rev-parse", "HEAD"]);
    t.f.git(
        &forge_consumer,
        &["update-ref", "refs/pull/2/head", &landing],
    );
    t.f.api
        .set_pr("swan-lake", 2, "open", "sl-0010-x", &landing, None);
    let out = t.status(true);
    assert_row(
        &out,
        consumer,
        "landing pushed",
        "request merge when checks succeed",
        "owner",
    );
    // Without a login the MR is not known to be open.
    let out = t.status(false);
    assert_row(
        &out,
        consumer,
        "pushed",
        "check swan-lake#2 on the forge (the landing commit is pushed)",
        "anyone",
    );
    // A rejected login: unknown, not `pushed`.
    let out = stdout(&t.f.swl_env(
        &t.f.root,
        &["status", "--workspace", &ws_s],
        &[("SWL_FJ_KEYS", bad.to_str().unwrap())],
    ));
    assert_row(
        &out,
        consumer,
        "unknown (fj login rejected: run fj auth login)",
        "fj auth login",
        "anyone",
    );
    // A draft MR is marked ready first.
    t.f.api
        .set_pr("swan-lake", 2, "draft", "sl-0010-x", &landing, None);
    let out = t.status(true);
    assert_row(
        &out,
        consumer,
        "MR open (draft)",
        "mark swan-lake#2 ready",
        "anyone",
    );
    t.f.api
        .set_pr("swan-lake", 2, "open", "sl-0010-x", &landing, None);
    // An MR whose head is no branch's any more: check it on the forge.
    t.f.git(
        &forge_consumer,
        &["update-ref", "refs/pull/5/head", &consumer_head],
    );
    t.f.api
        .set_pr("swan-lake", 5, "open", "sl-0010-x", &consumer_head, None);
    let out = t.status_from("swan-lake#5");
    let r = row(&out, consumer);
    assert!(r.starts_with("swan-lake  unknown (the MR head "), "{out}");
    assert!(
        r.contains(
            "the branch moved on (Forgejo has not updated the MR yet), or its branch is gone)"
        ),
        "{out}"
    );
    assert!(
        r.ends_with("waits: anyone  next: check swan-lake#5 on the forge"),
        "{out}"
    );
    t.f.git(&forge_consumer, &["update-ref", "-d", "refs/pull/5/head"]);
    // CI fails on the landing commit.
    t.f.api.set_status("swan-lake", &landing, "failure");
    let out = t.status(true);
    assert_row(
        &out,
        consumer,
        "landing pushed, CI failing",
        "fix CI",
        "anyone",
    );
    // --no-fetch looks up no consumer MR.
    let ws = t.ws.display().to_string();
    let out = stdout(&t.f.swl(&t.f.root, &["status", "--workspace", &ws, "--no-fetch"]));
    assert!(out.contains("--no-fetch: the MR is not looked up"), "{out}");

    // Landed: merged on the forge. Without a login, from git alone.
    t.f.git(
        &forge_consumer,
        &["update-ref", "refs/heads/master", &landing],
    );
    t.f.api.set_pr(
        "swan-lake",
        2,
        "merged",
        "refs/pull/2/head",
        &landing,
        Some(&landing),
    );
    assert_row(&t.status(true), consumer, "landed", "-", "nobody");
    assert_row(&t.status(false), consumer, "landed", "-", "nobody");
    let out = t.status_from("swan-lake#2");
    assert_row(&out, consumer, "landed", "-", "nobody");

    // The token went only to the private consumer, after a 404.
    assert!(
        t.f.authed_requests("swan-lake-common").is_empty(),
        "{:?}",
        t.f.api.seen_auth.lock().unwrap()
    );
    assert!(!t.f.authed_requests("swan-lake").is_empty());
}

#[test]
fn status_rows_are_unknown_when_the_forge_is_unreachable() {
    let f = Forge::new("status-offline");
    let work = f.root.join("work");
    fs::create_dir_all(&work).unwrap();
    stdout(&f.swl(
        &work,
        &["start", "SL-0011-y", "swan-lake", "swan-lake-common@lib"],
    ));
    let ws = work.join("SL-0011-y");
    let common = ws.join("swan-lake-common@lib");
    let consumer = ws.join("swan-lake");
    fs::write(common.join("value.txt"), "c2").unwrap();
    f.git(&common, &["commit", "-qam", "c2"]);
    f.git(&common, &["push", "-q", "origin", "sl-0011-y"]);
    fs::write(
        consumer.join("swl.toml"),
        "[inputs.lib]\nrepo = \"swan-lake-common\"\nmr = 1\n",
    )
    .unwrap();
    let ws_s = ws.display().to_string();
    let out = stdout(&f.swl_env(
        &f.root,
        &["status", "--workspace", &ws_s],
        &[("SWL_FORGE_API", "http://127.0.0.1:9/api/v1")],
    ));
    assert!(
        row(&out, "swan-lake-common@lib").contains("unknown (forge API unreachable"),
        "{out}"
    );
    // A source that cannot be fetched: that row is unknown, the rest is shown.
    f.git(
        &f.source("swan-lake"),
        &["remote", "set-url", "origin", "/nonexistent"],
    );
    let out = stdout(&f.swl(&f.root, &["status", "--workspace", &ws_s]));
    assert!(
        row(&out, "swan-lake").contains("unknown (fetching origin"),
        "{out}"
    );
    let r = row(&out, "swan-lake-common@lib");
    assert!(r.starts_with("swan-lake-common@lib  pushed  "), "{out}");
    assert!(r.contains("not readable anonymously"), "{out}");
}

#[test]
fn a_plain_lock_bump_is_not_a_landing() {
    let f = Forge::new("status-bump");
    let work = f.root.join("work");
    fs::create_dir_all(&work).unwrap();
    stdout(&f.swl(&work, &["start", "SL-0012-bump", "swan-lake"]));
    let consumer = work.join("SL-0012-bump/swan-lake");
    // A bump of lib to a new `current`, without any swl.toml history.
    f.commit_on_forge("swan-lake-common", "current", "c2");
    stdout(&f.nix(&consumer, &["flake", "update", "lib"]));
    f.git(&consumer, &["commit", "-qam", "lib: bump"]);
    f.git(&consumer, &["push", "-q", "origin", "sl-0012-bump"]);
    let head = f.git(&consumer, &["rev-parse", "HEAD"]);
    f.git(
        &f.forge_repo("swan-lake"),
        &["update-ref", "refs/pull/4/head", &head],
    );
    f.api
        .set_pr("swan-lake", 4, "open", "sl-0012-bump", &head, None);
    let ws = work.join("SL-0012-bump").display().to_string();
    let out = stdout(&f.swl(&f.root, &["status", "--workspace", &ws]));
    let r = row(&out, "swan-lake");
    assert!(!r.contains("landing pushed"), "{out}");
    assert!(r.starts_with("swan-lake  CI pending"), "{out}");
}

#[test]
fn status_shows_the_wrappers_swl_version_and_warns_when_it_differs() {
    let f = Forge::new("status-version");
    let work = f.root.join("work");
    fs::create_dir_all(&work).unwrap();
    stdout(&f.swl(&work, &["start", "SL-0013-v", "swan-lake"]));
    let ws = work.join("SL-0013-v");
    let ws_s = ws.display().to_string();
    let status = || {
        let out = f.swl(&f.root, &["status", "--workspace", &ws_s, "--no-fetch"]);
        let err = String::from_utf8_lossy(&out.stderr).to_string();
        (stdout(&out), err)
    };
    let version = stdout(
        &std::process::Command::new(env!("CARGO_BIN_EXE_swl"))
            .arg("--version")
            .output()
            .unwrap(),
    );
    let running = format!("running swl: {version} at ");

    // `start` ran `swl env`: the wrappers exec this swl.
    let (out, err) = status();
    assert!(support::line(&out, &running).ends_with("/swl"), "{out}");
    assert!(
        support::line(&out, "wrappers' swl: ")
            .starts_with(&format!("wrappers' swl: {version} at ")),
        "{out}"
    );
    assert!(!err.contains("warning"), "{err}");

    // The wrappers exec another swl, at a path that needs quoting.
    let old_dir = f.root.join("old swl's");
    fs::create_dir_all(&old_dir).unwrap();
    let old = old_dir.join("swl");
    fs::write(&old, "#!/bin/sh\necho 'swl 0.1.0'\n").unwrap();
    fs::set_permissions(&old, fs::Permissions::from_mode(0o755)).unwrap();
    let quoted = format!("'{}'", old.display().to_string().replace('\'', r"'\''"));
    let nix = ws.join(".swl/bin/nix");
    let wrapper = fs::read_to_string(&nix).unwrap();
    let exec = support::line(&wrapper, "exec ");
    let (_, rest) = exec["exec ".len()..].split_once(' ').unwrap();
    fs::write(
        &nix,
        wrapper.replace(exec, &format!("exec {quoted} {rest}")),
    )
    .unwrap();
    let (out, err) = status();
    assert_eq!(
        support::line(&out, "wrappers' swl: "),
        format!("wrappers' swl: swl 0.1.0 at {}", old.display()),
        "{out}"
    );
    assert!(
        err.contains(&format!(
            "swl status: warning: the wrappers exec swl 0.1.0, not the running {version}; rerun swl env"
        )),
        "{err}"
    );

    // Not executable, then gone.
    fs::set_permissions(&old, fs::Permissions::from_mode(0o644)).unwrap();
    let (out, err) = status();
    assert!(
        support::line(&out, "wrappers' swl: ").starts_with("wrappers' swl: unknown (running "),
        "{out}"
    );
    assert!(
        err.contains("warning: the wrappers' swl does not run (running ")
            && err.contains("ermission denied")
            && err.contains("; rerun swl env"),
        "{err}"
    );
    fs::remove_file(&old).unwrap();
    let (_, err) = status();
    assert!(
        err.contains("warning: the wrappers' swl does not run (running ")
            && err.contains("No such file"),
        "{err}"
    );

    // No wrappers yet: said, without a warning.
    fs::remove_dir_all(ws.join(".swl/bin")).unwrap();
    let (out, err) = status();
    assert_eq!(
        support::line(&out, "wrappers' swl: "),
        format!(
            "wrappers' swl: none yet ({} is missing; swl env writes it)",
            nix.display()
        ),
        "{out}"
    );
    assert!(!err.contains("warning"), "{err}");

    // `swl env` makes them exec this swl again.
    stdout(&f.swl(&f.root, &["env", "--workspace", &ws_s]));
    let (out, err) = status();
    assert!(
        support::line(&out, "wrappers' swl: ")
            .starts_with(&format!("wrappers' swl: {version} at ")),
        "{out}"
    );
    assert!(!err.contains("warning"), "{err}");
}

/// `swl start` with swan-lake on the branch, then `swl ws add nixos-config`,
/// which leaves it detached at the base: the workspace of a change that only
/// nixos-config's MR holds. nixos-config is private.
fn detached_consumer(name: &str) -> (Forge, std::path::PathBuf, String) {
    let f = Forge::new(name);
    f.add_consumer("nixos-config");
    f.api.make_private("nixos-config");
    let keys = f.login().display().to_string();
    let work = f.root.join("work");
    fs::create_dir_all(&work).unwrap();
    stdout(&f.swl(&work, &["start", "SL-0030-held", "swan-lake"]));
    let ws = work.join("SL-0030-held");
    let ws_s = ws.display().to_string();
    stdout(&f.swl(
        &f.root,
        &["ws", "add", "nixos-config", "--workspace", &ws_s],
    ));
    (f, ws, keys)
}

#[test]
fn status_says_a_detached_consumer_is_not_a_change() {
    let (f, ws, keys) = detached_consumer("status-detached");
    let ws_s = ws.display().to_string();
    let status = || {
        stdout(&f.swl_env(
            &f.root,
            &["status", "--workspace", &ws_s],
            &[("SWL_FJ_KEYS", &keys)],
        ))
    };

    // No MR of the workspace's branch: nothing to do, and said so.
    let out = status();
    assert_row(
        &out,
        "nixos-config",
        "not a change (detached at origin/master)",
        "-",
        "nobody",
    );
    assert_row(
        &out,
        "swan-lake",
        "no change (HEAD is in origin/master)",
        "-",
        "nobody",
    );
    assert!(
        out.contains(
            "\nno consumer checkout holds a change: none has commits of its own or a pushed \
             branch (nixos-config: not a change (detached at origin/master); swan-lake: no \
             change (HEAD is in origin/master))\n"
        ),
        "{out}"
    );
    // The branch named like the workspace was looked up, with the login.
    assert!(
        f.authed_requests("nixos-config")
            .contains(&"/api/v1/repos/binarin/nixos-config/pulls/master/sl-0030-held".to_string()),
        "{:?}",
        f.api.seen_auth.lock().unwrap()
    );

    // nixos-config#7 is open on the workspace's branch: switch to it.
    let head = f.commit_on_forge("nixos-config", "sl-0030-held", "nixos-config-2");
    f.git(
        &f.forge_repo("nixos-config"),
        &["update-ref", "refs/pull/7/head", &head],
    );
    f.api
        .set_pr("nixos-config", 7, "open", "sl-0030-held", &head, None);
    let out = status();
    assert_row(
        &out,
        "nixos-config",
        "not a change (detached at origin/master)",
        "git -C nixos-config switch sl-0030-held, then swl land",
        "anyone",
    );
    assert!(
        detail(&out, "nixos-config#7 is open on sl-0030-held").contains(
            "the workspace's branch; or swl land --from nixos-config#7, which makes a workspace \
             of its own, ./sl-0030-held, and needs the branches of its shared MRs not checked \
             out here"
        ),
        "{out}"
    );
    assert!(out.contains("no consumer checkout holds a change"), "{out}");

    // A forge that cannot be asked: a warning and a detail line, the same
    // rows.
    let out = f.swl_env(
        &f.root,
        &["status", "--workspace", &ws_s],
        &[
            ("SWL_FJ_KEYS", &keys),
            ("SWL_FORGE_API", "http://127.0.0.1:9/api/v1"),
        ],
    );
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        err.contains("swl: warning: nixos-config: cannot look up the MR of sl-0030-held: "),
        "{err}"
    );
    let out = stdout(&out);
    assert_row(
        &out,
        "nixos-config",
        "not a change (detached at origin/master)",
        "-",
        "nobody",
    );
    assert!(
        detail(&out, "MR of sl-0030-held: unknown (").contains("forge API unreachable"),
        "{out}"
    );

    // The hint works: on the branch, the checkout holds the change.
    let consumer = ws.join("nixos-config");
    f.git(&consumer, &["switch", "-q", "sl-0030-held"]);
    let out = status();
    assert_row(
        &out,
        "nixos-config",
        "CI pending (no status yet)",
        "wait for CI",
        "nobody",
    );
    assert!(
        !out.contains("no consumer checkout holds a change"),
        "{out}"
    );
    // A checkout detached elsewhere in the base says where.
    f.git(&consumer, &["switch", "-q", "--detach", "origin/master~1"]);
    let short = f.git(&consumer, &["rev-parse", "--short=12", "HEAD"]);
    assert_row(
        &status(),
        "nixos-config",
        &format!("not a change (detached at {short})"),
        "git -C nixos-config switch sl-0030-held, then swl land",
        "anyone",
    );
}

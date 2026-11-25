//! `swl status` on a disposable forge, through every stage of one change:
//! local, needs publish, pushed, MR open, CI, merged, promoted, landing
//! pushed and landed; with and without an fj login for the private consumer,
//! and `--from` without a workspace.

mod support;

use std::{fs, path::Path};

use support::{Forge, line, stdout};

/// The status row of `name` (its first line).
fn row<'a>(out: &'a str, name: &str) -> &'a str {
    line(out, &format!("{name}  "))
}

fn assert_row(out: &str, name: &str, stage: &str, next: &str, waits: &str) {
    let r = row(out, name);
    assert!(
        r.starts_with(&format!("{name}  {stage}  next: ")),
        "stage of {name}: want {stage:?} in {r:?}\n{out}"
    );
    assert!(
        r.contains(next),
        "next of {name}: want {next:?} in {r:?}\n{out}"
    );
    assert!(
        r.ends_with(&format!("waits: {waits}")),
        "waits of {name}: want {waits:?} in {r:?}\n{out}"
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
    assert!(line(&out, "  HEAD ").contains("sl-0010-x  clean"), "{out}");
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
    assert_row(
        &out,
        common,
        "promoted",
        "swl land swan-lake (not implemented yet)",
        "anyone",
    );
    assert!(out.contains(", in current; file://"), "{out}");
    assert!(out.contains("has it"), "{out}");
    assert_row(
        &out,
        consumer,
        "CI green",
        "swl land swan-lake (not implemented yet)",
        "anyone",
    );
    // The same, from the published MR alone.
    let out = t.status_from("swan-lake#2");
    assert_row(&out, "swan-lake-common#1", "promoted", "swl land", "anyone");
    assert_row(&out, consumer, "CI green", "swl land swan-lake", "anyone");
    assert!(out.contains("the head of sl-0010-x"), "{out}");

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
    let out = t.status(false);
    assert_row(&out, consumer, "landing pushed", "request merge", "owner");
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

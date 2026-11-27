//! Common's promotion of what reached its master without a consumer, on a
//! disposable forge with a mirror of common: `swl promote` fast-forwards
//! `current` past master's unpromoted commits and waits for the mirror,
//! refuses a rev outside master and a branch it cannot fast-forward, and is
//! a no-op when promoted; `swl land` of a common-only workspace promotes its
//! merged MR and waits for the mirror, which `swl status` shows as merged,
//! promoted, landed.

mod support;

use std::{
    fs,
    path::{Path, PathBuf},
    process::Output,
};

use support::{Forge, MergeReply, failed, stdout, text};

const COMMON: &str = "swan-lake-common";

/// A pre-receive hook that refuses pushes to `current`, as a protected
/// branch does for an agent.
const PROTECT_CURRENT: &str = r#"#!/bin/sh
while read old new ref; do
  if [ "$ref" = refs/heads/current ]; then
    echo "current is protected" >&2
    exit 1
  fi
done
"#;

struct Common {
    f: Forge,
    mirror: PathBuf,
    keys: String,
}

fn short(rev: &str) -> &str {
    &rev[..12]
}

fn out_text(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

impl Common {
    /// The forge with common (master = current) and swan-lake, which
    /// declares common's `current` from the mirror.
    fn new(name: &str) -> Self {
        let f = Forge::new(name);
        let keys = f.login().display().to_string();
        let mirror = f.mirror_common();
        Self { f, mirror, keys }
    }

    fn swl(&self, dir: &Path, args: &[&str]) -> Output {
        let out = self.f.swl_env(dir, args, &[("SWL_FJ_KEYS", &self.keys)]);
        assert!(!text(&out).contains(support::TOKEN), "{}", text(&out));
        out
    }

    fn promote(&self, extra: &[&str]) -> Output {
        let mut args = vec!["promote", "--timeout", "0", "--interval", "1"];
        args.extend(extra);
        self.swl(&self.f.root, &args)
    }

    fn rev(&self, refname: &str) -> String {
        self.f
            .git(&self.f.forge_repo(COMMON), &["rev-parse", refname])
    }

    /// `<abbreviated sha> <subject>` of a commit, as git abbreviates it in
    /// common's source.
    fn oneline(&self, rev: &str) -> String {
        self.f.git(
            &self.f.source(COMMON),
            &["log", "-1", "--format=%h %s", rev],
        )
    }

    fn forge_refs(&self) -> String {
        self.f.git(&self.f.forge_repo(COMMON), &["for-each-ref"])
    }

    fn mirror_url(&self) -> String {
        format!("file://{}", self.mirror.display())
    }
}

#[test]
fn promote_fast_forwards_past_master_and_waits_for_the_mirror() {
    let t = Common::new("promote");
    let c1 = t.rev("master");
    let c2 = t.f.commit_on_forge(COMMON, "master", "c2");
    let c3 = t.f.commit_on_forge(COMMON, "master", "c3");
    let source = t.f.source(COMMON).display().to_string();

    // A dry run lists what goes to current, and writes nothing.
    let refs = t.forge_refs();
    let log = stdout(&t.promote(&["--dry-run"]));
    for want in [
        format!(
            "swl promote: swan-lake-common origin/master: current at {} gets 2 commit(s):\n  {}\n  {}\n",
            short(&c1),
            t.oneline(&c2),
            t.oneline(&c3)
        ),
        format!(
            "swl promote: would promote swan-lake-common origin/master: git -C {source} push \
             origin {c3}:refs/heads/current"
        ),
    ] {
        assert!(log.contains(&want), "{want:?} in\n{log}");
    }
    assert_eq!(t.forge_refs(), refs);

    // A refused push waits for the owner, with the command.
    t.f.hook(COMMON, "pre-receive", PROTECT_CURRENT);
    failed(
        &t.promote(&[]),
        3,
        &format!(
            "swl promote: waits for the owner: promote: git -C {source} push origin \
             {c3}:refs/heads/current (the push was refused: remote: current is protected"
        ),
    );
    assert_eq!(t.rev("current"), c1);
    t.f.hook_opt(COMMON, "pre-receive", None);

    // Promoted; the mirror does not have it yet.
    let out = t.promote(&[]);
    failed(
        &out,
        5,
        &format!(
            "swl promote: waits for nobody: swan-lake-common origin/master: waiting for {} \
             current to have {} (not yet); rerun swl promote",
            t.mirror_url(),
            short(&c3)
        ),
    );
    assert!(
        out_text(&out).contains(&format!(
            "swl promote: promoted current to {}; next: swl bump",
            short(&c3)
        )),
        "{}",
        text(&out)
    );
    assert_eq!(t.rev("current"), c3);

    // Mirrored: done, nothing moved, no hint.
    t.f.sync_mirror(&t.mirror);
    let log = stdout(&t.promote(&[]));
    assert!(
        log.contains(&format!("swl promote: current has {} already", short(&c3))),
        "{log}"
    );
    assert!(log.contains("swl promote: nothing to promote"), "{log}");
    assert!(!log.contains("next: swl bump"), "{log}");
}

#[test]
fn promote_is_a_no_op_when_promoted() {
    let t = Common::new("promote-noop");
    let c1 = t.rev("master");
    let refs = t.forge_refs();
    let log = stdout(&t.promote(&[]));
    assert!(
        log.contains(&format!("swl promote: current has {} already", short(&c1))),
        "{log}"
    );
    assert!(log.contains("swl promote: nothing to promote"), "{log}");
    assert_eq!(t.forge_refs(), refs);

    // An older commit of master that current has: a no-op too, which
    // still waits for the mirror.
    let c2 = t.f.commit_on_forge(COMMON, "master", "c2");
    t.f.git(
        &t.f.forge_repo(COMMON),
        &["update-ref", "refs/heads/current", &c2],
    );
    failed(
        &t.promote(&[&c2]),
        5,
        &format!(
            "waiting for {} current to have {} (not yet)",
            t.mirror_url(),
            short(&c2)
        ),
    );
    assert!(stdout(&t.promote(&[&c1])).contains("nothing to promote"));
}

#[test]
fn promote_refuses_what_it_cannot_fast_forward() {
    let t = Common::new("promote-refused");
    let c1 = t.rev("master");

    // A rev outside master: an error, nothing written.
    let feature = t.f.commit_on_forge(COMMON, "feature", "f");
    let refs = t.forge_refs();
    failed(
        &t.promote(&[&feature]),
        2,
        &format!(
            "swl: {feature} ({}) is not origin/master ({}) or in its history: swl promote \
             promotes what is on master only",
            short(&feature),
            short(&c1)
        ),
    );
    failed(
        &t.promote(&["no-such-rev"]),
        2,
        "no-such-rev is not a commit of swan-lake-common",
    );
    assert_eq!(t.forge_refs(), refs);

    // Current has what master lacks: for the owner, never forced.
    let side = t.f.commit_on_forge(COMMON, "current", "side");
    let c2 = t.f.commit_on_forge(COMMON, "master", "c2");
    failed(
        &t.promote(&[]),
        3,
        &format!(
            "swl promote: waits for the owner: promote current: origin/current at {} has \
             commits that origin/master at {} does not, so it cannot be fast-forwarded",
            short(&side),
            short(&c2)
        ),
    );
    assert_eq!(t.rev("current"), side);
}

#[test]
fn land_promotes_a_common_only_change_and_waits_for_the_mirror() {
    let t = Common::new("land-common");
    let c1 = t.rev("master");
    t.f.follow_pull_refs(COMMON);
    // A landing on master that is not promoted, which the change's
    // promotion takes along.
    let other = t.f.commit_on_forge(COMMON, "master", "other");

    // A workspace with common as its consumer, and its open MR.
    let work = t.f.root.join("work");
    fs::create_dir_all(&work).unwrap();
    stdout(&t.swl(&work, &["start", "SL-0030-common", COMMON]));
    let ws = work.join("SL-0030-common");
    let checkout = ws.join(COMMON);
    fs::write(checkout.join("value.txt"), "c2").unwrap();
    t.f.git(&checkout, &["commit", "-qam", "c2"]);
    t.f.git(&checkout, &["push", "-q", "origin", "sl-0030-common"]);
    let c2 = t.f.git(&checkout, &["rev-parse", "HEAD"]);
    t.f.git(
        &t.f.forge_repo(COMMON),
        &["update-ref", "refs/pull/1/head", &c2],
    );
    t.f.api
        .set_pr(COMMON, 1, "open", "sl-0030-common", &c2, None);
    let ws_arg = ws.display().to_string();
    let land = |extra: &[&str]| {
        let mut args = vec![
            "land",
            "--workspace",
            &ws_arg,
            "--timeout",
            "0",
            "--interval",
            "1",
        ];
        args.extend(extra);
        t.swl(&t.f.root, &args)
    };
    let status = || stdout(&t.swl(&ws, &["status"]));
    let row = |log: &str| {
        log.lines()
            .find(|l| l.starts_with(&format!("{COMMON} ")))
            .unwrap_or_else(|| panic!("no row of {COMMON} in\n{log}"))
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    };

    // Merged; the promotion branch is protected: for the owner, with what
    // it takes along listed.
    t.f.hook(COMMON, "pre-receive", PROTECT_CURRENT);
    t.f.api.on_merge(COMMON, 1, &[MergeReply::MergeNow]);
    let out = land(&[]);
    let source = t.f.source(COMMON).display().to_string();
    failed(
        &out,
        3,
        &format!(
            "waits for the owner: promote: git -C {source} push origin {c2}:refs/heads/current \
             (the push was refused: remote: current is protected"
        ),
    );
    let log = out_text(&out);
    assert!(log.contains("swan-lake-common#1 is merged"), "{log}");
    assert!(
        log.contains(&format!(
            "swl land: swan-lake-common#1: current at {} gets 2 commit(s):\n  {}\n  {}\n",
            short(&c1),
            t.oneline(&other),
            t.oneline(&c2)
        )),
        "{log}"
    );
    assert_eq!(t.rev("master"), c2);
    assert_eq!(t.rev("current"), c1);
    let log = status();
    assert_eq!(
        row(&log),
        "swan-lake-common merged waits: owner next: promote current",
        "{log}"
    );
    assert!(
        log.contains(&format!(
            "promotion of {}: not in current yet; {} does not have it yet",
            short(&c2),
            t.mirror_url()
        )),
        "{log}"
    );

    // Promoted; the mirror does not have it yet: the workspace stays, and
    // land --from does not take it for landed.
    t.f.hook_opt(COMMON, "pre-receive", None);
    let out = land(&[]);
    let waiting = format!(
        "waits for nobody: swan-lake-common#1: waiting for {} current to have {} (not yet)",
        t.mirror_url(),
        short(&c2)
    );
    failed(&out, 5, &waiting);
    assert!(
        out_text(&out).contains(&format!(
            "swl land: swan-lake-common#1: promoted current to {}; next: swl bump",
            short(&c2)
        )),
        "{}",
        text(&out)
    );
    assert_eq!(t.rev("current"), c2);
    assert!(ws.exists());
    let log = status();
    assert_eq!(
        row(&log),
        format!(
            "swan-lake-common promoted waits: nobody next: wait for {} to have it",
            t.mirror_url()
        ),
        "{log}"
    );
    let from = ["land", "--from", "swan-lake-common#1", "--timeout", "0"];
    failed(&t.swl(&work, &from), 5, &waiting);

    // Mirrored: landed; land removes the workspace.
    t.f.sync_mirror(&t.mirror);
    let log = status();
    assert_eq!(
        row(&log),
        "swan-lake-common landed waits: nobody next: -",
        "{log}"
    );
    let log = stdout(&land(&[]));
    assert!(log.contains("every MR of the change is merged"), "{log}");
    assert!(!ws.exists(), "{log}");
    let log = stdout(&t.swl(&work, &from));
    assert!(
        log.contains("swl land: swan-lake-common#1 has landed: nothing left to do"),
        "{log}"
    );
}

/// The row of `repo` in a status log, its columns joined by one space.
fn row_of(log: &str, repo: &str) -> String {
    log.lines()
        .find(|l| l.starts_with(&format!("{repo} ")))
        .unwrap_or_else(|| panic!("no row of {repo} in\n{log}"))
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

impl Common {
    /// `swl start <name> <repos…>` in `<root>/work`: the workspace.
    fn start(&self, name: &str, repos: &[&str]) -> PathBuf {
        let work = self.f.root.join("work");
        fs::create_dir_all(&work).unwrap();
        let mut args = vec!["start", name];
        args.extend(repos);
        stdout(&self.swl(&work, &args));
        work.join(name)
    }

    /// A commit of `file` in the checkout of `repo`, pushed to its branch
    /// `branch`: its sha.
    fn push_change(&self, ws: &Path, repo: &str, branch: &str, file: &str) -> String {
        let checkout = ws.join(repo);
        fs::write(checkout.join(file), "changed").unwrap();
        self.f.git(&checkout, &["add", file]);
        self.f.git(&checkout, &["commit", "-qm", file]);
        self.f.git(&checkout, &["push", "-q", "origin", branch]);
        self.f.git(&checkout, &["rev-parse", "HEAD"])
    }

    /// The open MR `repo#n` of `branch` at `head`.
    fn open_mr(&self, repo: &str, n: u64, branch: &str, head: &str) {
        self.f.git(
            &self.f.forge_repo(repo),
            &["update-ref", &format!("refs/pull/{n}/head"), head],
        );
        self.f.api.set_pr(repo, n, "open", branch, head, None);
    }

    fn land(&self, ws: &Path) -> Output {
        let ws = ws.display().to_string();
        let args = [
            "land",
            "--workspace",
            &ws,
            "--timeout",
            "0",
            "--interval",
            "1",
        ];
        self.swl(&self.f.root, &args)
    }
}

#[test]
fn land_promotes_common_in_its_consumer_step_after_the_other_consumers() {
    let t = Common::new("land-mixed");
    t.f.follow_pull_refs(COMMON);
    t.f.follow_pull_refs("swan-lake");
    let ws = t.start("SL-0031-mixed", &["swan-lake", COMMON]);
    let lake = t.push_change(&ws, "swan-lake", "sl-0031-mixed", "notes.txt");
    t.open_mr("swan-lake", 1, "sl-0031-mixed", &lake);
    let c2 = t.push_change(&ws, COMMON, "sl-0031-mixed", "notes.txt");
    t.open_mr(COMMON, 1, "sl-0031-mixed", &c2);
    t.f.api.on_merge("swan-lake", 1, &[MergeReply::MergeNow]);
    t.f.api.on_merge(COMMON, 1, &[MergeReply::MergeNow]);

    // Both merged, swan-lake's first; common's step then promotes its
    // merge commit and waits for the mirror.
    let out = t.land(&ws);
    failed(
        &out,
        5,
        &format!(
            "waits for nobody: swan-lake-common#1: waiting for {} current to have {} (not yet)",
            t.mirror_url(),
            short(&c2)
        ),
    );
    let log = out_text(&out);
    let merged = |which: &str| {
        log.find(&format!("{which} is merged"))
            .unwrap_or_else(|| panic!("{which} is not merged in\n{log}"))
    };
    assert!(
        merged("swan-lake#1") < merged("swan-lake-common#1"),
        "{log}"
    );
    assert!(
        log.contains(&format!(
            "swl land: swan-lake-common#1: promoted current to {}; next: swl bump",
            short(&c2)
        )),
        "{log}"
    );
    assert_eq!(t.rev("current"), c2);
    assert!(ws.exists());

    // Mirrored: every MR landed, and the workspace goes.
    t.f.sync_mirror(&t.mirror);
    let log = stdout(&t.swl(&ws, &["status"]));
    assert_eq!(
        row_of(&log, COMMON),
        "swan-lake-common landed waits: nobody next: -",
        "{log}"
    );
    let log = stdout(&t.land(&ws));
    assert!(log.contains("every MR of the change is merged"), "{log}");
    assert!(!ws.exists(), "{log}");
}

#[test]
fn land_promotes_common_only_for_a_merged_mr() {
    let t = Common::new("land-unmerged");
    let c1 = t.rev("master");
    let ws = t.start("SL-0032-pushed", &[COMMON]);

    // The change pushed to master and to its branch, which is kept, with no
    // MR: landed by git alone, nothing promoted.
    let c2 = t.push_change(&ws, COMMON, "sl-0032-pushed", "notes.txt");
    t.f.git(&ws.join(COMMON), &["push", "-q", "origin", "HEAD:master"]);
    let log = stdout(&t.swl(&ws, &["status"]));
    assert_eq!(
        row_of(&log, COMMON),
        "swan-lake-common landed waits: nobody next: -",
        "{log}"
    );
    assert!(!log.contains("promotion of"), "{log}");
    let log = stdout(&t.land(&ws));
    assert!(log.contains("nothing to land"), "{log}");
    assert!(ws.exists());
    assert_eq!(t.rev("current"), c1);

    // Its MR merged at the kept branch's head: promoted.
    t.f.git(
        &t.f.forge_repo(COMMON),
        &["update-ref", "refs/pull/1/head", &c2],
    );
    t.f.api
        .set_pr(COMMON, 1, "merged", "sl-0032-pushed", &c2, Some(&c2));
    let log = stdout(&t.swl(&ws, &["status"]));
    assert_eq!(
        row_of(&log, COMMON),
        "swan-lake-common merged waits: owner next: promote current",
        "{log}"
    );
    failed(
        &t.land(&ws),
        5,
        &format!(
            "waits for nobody: swan-lake-common#1: waiting for {} current to have {} (not yet)",
            t.mirror_url(),
            short(&c2)
        ),
    );
    assert_eq!(t.rev("current"), c2);
}

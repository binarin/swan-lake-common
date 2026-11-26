//! Common's promotion of what reached its master without a consumer, on a
//! disposable forge with a mirror of common: `swl promote` fast-forwards
//! `current` past master's unpromoted commits and waits for the mirror,
//! refuses a rev outside master and a branch it cannot fast-forward, and is
//! a no-op when promoted.

mod support;

use std::{path::PathBuf, process::Output};

use support::{Forge, failed, stdout, text};

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

    fn swl(&self, dir: &std::path::Path, args: &[&str]) -> Output {
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

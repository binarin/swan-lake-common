//! `swl bump` on a disposable forge, over reruns: common's inputs moved
//! forward (and one that is not left), pushed to master, promoted, and a bump
//! MR per consumer opened and merged; the owner-only pushes refused, master's
//! unpromoted commits that are not bump's, and a bump branch with foreign
//! commits; the bump branch rebuilt on a stale base; `--dry-run`, `--check`
//! and the no-op rerun; refs with a slash and revs pinned in flake.nix; a
//! branch that moves between bump's read and its push; consumers that
//! disagree on the promotion branch; a lagging mirror; a bump branch not
//! needed any more, closed and deleted; an obsolete bump MR in `status` and
//! `land`, rebuilt when common moves first; a consumer started over when its
//! base moves before the push.
//!
//! Ported from scripts/test-relock-chain.py: its first run (each pin
//! relocked, committed, pushed and promoted) is `bump_runs_to_the_end_over_reruns`,
//! which ends in its clean no-op rerun. Its dirty checkout that aborts has no
//! counterpart: bump never works in a checkout, only in scratch worktrees of
//! the sources at the forge's heads; what it refuses instead is foreign work
//! on what it would push (`bump_stops_for_what_is_not_its_own`).

mod support;

use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf, process::Output};

use support::{Forge, MergeReply, failed, stdout, text};

/// A pre-receive hook that refuses pushes to `@BRANCH@`, as a protected
/// branch does for an agent.
const PROTECT: &str = r#"#!/bin/sh
while read old new ref; do
  if [ "$ref" = refs/heads/@BRANCH@ ]; then
    echo "@BRANCH@ is protected" >&2
    exit 1
  fi
done
"#;

const COMMON: &str = "swan-lake-common";
const BRANCH: &str = "swl-bump-common";
const CONSUMERS: [&str; 2] = ["nixos-config", "swan-lake"];

struct Bump {
    f: Forge,
    keys: String,
}

fn short(rev: &str) -> &str {
    &rev[..12]
}

fn out_text(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

impl Bump {
    /// The forge with common (master = current), zwanenzang, and two
    /// private consumers locked to common's `current`, whose pull refs
    /// follow their branches.
    fn new(name: &str) -> Self {
        let f = Forge::new(name);
        f.add_consumer("nixos-config");
        for c in CONSUMERS {
            f.api.make_private(c);
            f.follow_pull_refs(c);
        }
        // The sources' hooks are for their checkouts: bump's commits in
        // scratch worktrees skip them.
        for repo in [COMMON, "nixos-config", "swan-lake"] {
            let hook = f.source(repo).join("hooks/pre-commit");
            fs::create_dir_all(hook.parent().unwrap()).unwrap();
            fs::write(&hook, "#!/bin/sh\necho no hooks here >&2\nexit 1\n").unwrap();
            fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let keys = f.login().display().to_string();
        Self { f, keys }
    }

    fn bump(&self, extra: &[&str]) -> Output {
        self.bump_env(extra, &[])
    }

    fn bump_env(&self, extra: &[&str], env: &[(&str, &str)]) -> Output {
        let mut args = vec!["bump", "--timeout", "0", "--interval", "1"];
        args.extend(extra);
        let mut env = env.to_vec();
        env.extend([
            ("SWL_FJ_KEYS", self.keys.as_str()),
            ("SWL_ALLOW_FILE_URLS", "1"),
        ]);
        let out = self.f.swl_env(&self.f.root, &args, &env);
        assert!(!text(&out).contains(support::TOKEN), "{}", text(&out));
        out
    }

    fn rev(&self, repo: &str, refname: &str) -> String {
        self.f
            .git(&self.f.forge_repo(repo), &["rev-parse", refname])
    }

    /// The rev that `repo`'s lock at `rev` locks the root input `input` at.
    fn locked(&self, repo: &str, rev: &str, input: &str) -> String {
        let text = self.f.git(
            &self.f.forge_repo(repo),
            &["show", &format!("{rev}:flake.lock")],
        );
        let lock: serde_json::Value = serde_json::from_str(&text).unwrap();
        let key = lock["nodes"]["root"]["inputs"][input].as_str().unwrap();
        lock["nodes"][key]["locked"]["rev"]
            .as_str()
            .unwrap()
            .to_string()
    }

    fn protect(&self, repo: &str, branch: Option<&str>) {
        let hook = branch.map(|b| PROTECT.replace("@BRANCH@", b));
        self.f.hook_opt(repo, "pre-receive", hook.as_deref());
    }

    /// Every ref of the forge repos.
    fn forge_refs(&self) -> String {
        ["zwanenzang", COMMON, "nixos-config", "swan-lake"]
            .iter()
            .map(|r| self.f.git(&self.f.forge_repo(r), &["for-each-ref"]))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// No scratch worktree is left in any source.
    fn no_worktrees_left(&self) {
        for repo in [COMMON, "nixos-config", "swan-lake"] {
            let list = self
                .f
                .git(&self.f.source(repo), &["worktree", "list", "--porcelain"]);
            assert_eq!(list.matches("worktree ").count(), 1, "{repo}: {list}");
        }
    }

    /// The source of common, as the owner's commands name it.
    fn common_source(&self) -> String {
        self.f.source(COMMON).display().to_string()
    }

    /// A clone of `repo`'s forge repo at `branch`.
    fn clone_at(&self, repo: &str, branch: &str, name: &str) -> PathBuf {
        let dir = self.f.clone_elsewhere(repo, name);
        self.f.git(&dir, &["checkout", "-q", branch]);
        dir
    }
}

#[test]
fn bump_runs_to_the_end_over_reruns() {
    let t = Bump::new("bump");
    let common = t.f.forge_repo(COMMON);
    let c1 = t.rev(COMMON, "master");
    let z1 = t.locked(COMMON, &c1, "zwanenzang");

    // Nothing has moved: nothing to bump, nothing written.
    let log = stdout(&t.bump(&[]));
    assert!(
        log.contains("swl bump: common\n├─ up to date: zwanenzang\n"),
        "{log}"
    );
    assert!(log.contains("swl bump: nothing to bump"), "{log}");
    assert!(t.f.api.writes().is_empty(), "{:?}", t.f.api.writes());

    // zwanenzang moves on. A dry run plans, and writes nothing.
    let z2 = t.f.commit_on_forge("zwanenzang", "master", "z2");
    let refs = t.forge_refs();
    let log = stdout(&t.bump(&["--dry-run"]));
    for want in [
        format!(
            "swl bump: common\n├─ zwanenzang {} -> {} (the default branch)",
            short(&z1),
            short(&z2)
        ),
        format!(
            "├─ would relock zwanenzang on master at {} commit \"swl: bump zwanenzang\" and \
             push it to origin's master (fast-forward)",
            short(&c1)
        ),
        "└─ dry run, nothing was written; then: promote current, wait for its declared source; \
         then the bump MRs of nixos-config, swan-lake"
            .to_string(),
    ] {
        assert!(log.contains(&want), "{want:?} in\n{log}");
    }
    assert_eq!(t.forge_refs(), refs);
    assert!(t.f.api.writes().is_empty());
    t.no_worktrees_left();

    // Master is the owner's: the bump waits for them, with the command.
    t.protect(COMMON, Some("master"));
    let out = t.bump(&[]);
    failed(
        &out,
        3,
        &format!(
            "└─ waits for the owner: push common's bump to master: rerun swl bump as the \
             owner, or git -C {} push origin ",
            t.common_source()
        ),
    );
    failed(
        &out,
        3,
        "(the push was refused: remote: master is protected",
    );
    assert_eq!(t.forge_refs(), refs);
    t.no_worktrees_left();

    // Master may be pushed, the promotion branch not: master has the bump.
    t.protect(COMMON, Some("current"));
    let out = t.bump(&[]);
    let bumped = t.rev(COMMON, "master");
    failed(
        &out,
        3,
        &format!(
            "waits for the owner: promote: git -C {} push origin {bumped}:refs/heads/current (the \
             push was refused: remote: current is protected",
            t.common_source()
        ),
    );
    assert!(out_text(&out).contains(&format!(
        "├─ pushed {} to master: swl: bump zwanenzang",
        short(&bumped)
    )));
    assert_eq!(t.rev(COMMON, "master^"), c1);
    assert_eq!(t.locked(COMMON, &bumped, "zwanenzang"), z2);
    assert_eq!(t.rev(COMMON, "current"), c1);
    let files = t.f.git(&common, &["diff", "--name-only", &c1, &bumped]);
    assert_eq!(files, "flake.lock");
    // A rerun promotes that bump; it makes no second one.
    let out = t.bump(&[]);
    failed(&out, 3, "promote: git -C");
    assert!(
        out_text(&out).contains("has swl bump's commit, not promoted to current yet"),
        "{}",
        text(&out)
    );
    assert_eq!(t.rev(COMMON, "master"), bumped);

    // Promoted; each consumer gets a bump MR, whose merge waits for CI.
    t.protect(COMMON, None);
    let out = t.bump(&[]);
    failed(
        &out,
        5,
        "swl bump: swan-lake\n├─ push swan-lake's bump of lib to ",
    );
    failed(
        &out,
        5,
        "└─ waits for nobody: swan-lake#1 waits for CI, then it merges",
    );
    failed(&out, 5, "nixos-config#1 waits for CI, then it merges");
    assert_eq!(t.rev(COMMON, "current"), bumped);
    for c in CONSUMERS {
        let tip = t.rev(c, BRANCH);
        assert_eq!(t.rev(c, &format!("{BRANCH}^")), t.rev(c, "master"));
        assert_eq!(t.locked(c, &tip, "lib"), bumped);
        assert_eq!(
            t.f.git(&t.f.forge_repo(c), &["log", "-1", "--format=%s", BRANCH]),
            format!("swl: bump common to {}", short(&bumped))
        );
        assert_eq!(t.rev(c, "refs/pull/1/head"), tip);
        assert!(t.f.api.is_scheduled(c, 1));
    }
    let writes = t.f.api.writes();
    let opened: Vec<&String> = writes.iter().filter(|w| w.contains("/pulls {")).collect();
    assert_eq!(opened.len(), 2, "{writes:?}");
    let post: serde_json::Value = serde_json::from_str(
        opened[1]
            .strip_prefix("POST /api/v1/repos/binarin/swan-lake/pulls ")
            .unwrap(),
    )
    .unwrap();
    assert_eq!(post["head"], BRANCH);
    assert_eq!(post["base"], "master");
    assert_eq!(
        post["title"],
        format!("swl: bump common to {}", short(&bumped))
    );
    t.no_worktrees_left();

    // The MR's swl check accepts the bump: a forward move of lib.
    let mr = t.clone_at("swan-lake", BRANCH, "check-swan-lake");
    let out = t.f.swl_env(
        &mr,
        &["check", "--event", "mr", "--base", "origin/master"],
        &[("SWL_ALLOW_FILE_URLS", "1")],
    );
    let log = stdout(&out);
    assert!(log.contains("lib: forward move"), "{log}");
    assert!(log.contains("MR checks passed"), "{log}");

    // The forge lags with swan-lake#1's pull ref: the MR is found by its
    // branch in the list of open MRs, not opened again.
    let sl_tip = t.rev("swan-lake", BRANCH);
    let sl_forge = t.f.forge_repo("swan-lake");
    t.f.git(&sl_forge, &["update-ref", "-d", "refs/pull/1/head"]);
    let out = t.bump(&[]);
    failed(&out, 5, "swan-lake#1 waits for CI, then it merges");
    let opened = |w: &Vec<String>| w.iter().filter(|w| w.contains("/pulls {")).count();
    assert_eq!(opened(&t.f.api.writes()), 2, "{:?}", t.f.api.writes());
    t.f.git(&sl_forge, &["update-ref", "refs/pull/1/head", &sl_tip]);

    // CI succeeds on swan-lake's bump, which merges by its schedule; on
    // nixos-config's too, but its merge is refused: for the owner.
    t.f.api.set_status("swan-lake", &sl_tip, "success");
    assert_eq!(t.rev("swan-lake", "master"), sl_tip);
    let nc_tip = t.rev("nixos-config", BRANCH);
    t.f.api
        .set_status_without_event("nixos-config", &nc_tip, "success");
    let refused = MergeReply::Status(405, "User not allowed to merge PR".into());
    t.f.api
        .on_merge("nixos-config", 1, &[refused.clone(), refused]);
    let out = t.bump(&[]);
    failed(
        &out,
        3,
        "└─ waits for the owner: merge nixos-config#1 (the forge refused: HTTP 405: User not \
         allowed to merge PR)",
    );
    assert!(
        out_text(&out).contains(&format!(
            "swl bump: swan-lake\n└─ common is at {} already\n",
            short(&bumped)
        )),
        "{}",
        text(&out)
    );

    // Allowed: merged directly; done.
    t.f.api.on_merge("nixos-config", 1, &[MergeReply::MergeNow]);
    let log = stdout(&t.bump(&[]));
    assert!(log.contains("nixos-config#1 is merged"), "{log}");
    assert!(
        log.contains(&format!(
            "swl bump: done: nixos-config, swan-lake lock common at {}",
            short(&bumped)
        )),
        "{log}"
    );
    for c in CONSUMERS {
        let master = t.rev(c, "master");
        assert_eq!(t.locked(c, &master, "lib"), bumped);
    }

    // A clean rerun is a no-op.
    let refs = t.forge_refs();
    let writes = t.f.api.writes().len();
    let log = stdout(&t.bump(&[]));
    assert!(log.contains("swl bump: nothing to bump"), "{log}");
    assert_eq!(t.forge_refs(), refs);
    assert_eq!(t.f.api.writes().len(), writes);
    t.no_worktrees_left();
}

/// Common with more inputs of zwanenzang: its `next`, its `a/b` (a ref
/// with a slash, as `swan-lake-current/nixos-26.05`), and a rev pinned in
/// flake.nix.
const COMMON_WITH_NEXT: &str = r#"{
  inputs.zwanenzang.url = "git+file://@FORGE@/binarin/zwanenzang";
  inputs.zw-next.url = "git+file://@FORGE@/binarin/zwanenzang?ref=next";
  inputs.zw-slash.url = "git+file://@FORGE@/binarin/zwanenzang?ref=a/b";
  inputs.zw-pinned.url = "git+file://@FORGE@/binarin/zwanenzang?rev=@Z1@";
  outputs = { self, ... }: {
    value = builtins.readFile ./value.txt;
  };
}
"#;

#[test]
fn bump_leaves_an_input_that_does_not_move_forward() {
    let t = Bump::new("bump-rebased");
    // Common pins zwanenzang's `next` too, as of n1, its `a/b` as of s1,
    // and z1 by rev.
    let n1 = t.f.commit_on_forge("zwanenzang", "next", "n1");
    let s1 = t.f.commit_on_forge("zwanenzang", "a/b", "s1");
    let pinned = t.rev("zwanenzang", "master");
    let seed = t.f.clone_elsewhere(COMMON, "common-next-seed");
    fs::write(
        seed.join("flake.nix"),
        COMMON_WITH_NEXT
            .replace("@FORGE@", &t.f.forge.display().to_string())
            .replace("@Z1@", &pinned),
    )
    .unwrap();
    stdout(&t.f.nix(&seed, &["flake", "lock"]));
    t.f.git(&seed, &["commit", "-qam", "common: zw-next"]);
    t.f.git(
        &seed,
        &["push", "-q", "origin", "HEAD:master", "HEAD:current"],
    );
    let c2 = t.rev(COMMON, "master");
    let z1 = t.locked(COMMON, &c2, "zwanenzang");
    assert_eq!(t.locked(COMMON, &c2, "zw-next"), n1);
    // `next` is rebased: its new head does not descend from n1. Master
    // moves on.
    let rebased = t.f.clone_elsewhere("zwanenzang", "zw-rebased");
    t.f.git(&rebased, &["checkout", "-qB", "next", "origin/master"]);
    fs::write(rebased.join("value.txt"), "n2").unwrap();
    t.f.git(&rebased, &["commit", "-qam", "n2"]);
    t.f.git(&rebased, &["push", "-q", "--force", "origin", "next"]);
    let n2 = t.rev("zwanenzang", "next");
    let z2 = t.f.commit_on_forge("zwanenzang", "master", "z2");
    let s2 = t.f.commit_on_forge("zwanenzang", "a/b", "s2");

    let log = stdout(&t.bump(&["--dry-run", "--check"]));
    for want in [
        format!(
            "├─ zwanenzang {} -> {} (the default branch)",
            short(&z1),
            short(&z2)
        ),
        format!(
            "├─ zw-next: not bumped: {} -> {} (next) is not a forward move, the new rev does \
             not descend from the locked one; left at {}",
            short(&n1),
            short(&n2),
            short(&n1)
        ),
        format!("├─ zw-slash {} -> {} (a/b)", short(&s1), short(&s2)),
        format!(
            "├─ zw-pinned: not bumped: flake.nix pins it to a rev; left at {}",
            short(&pinned)
        ),
        "├─ would relock zw-slash, zwanenzang on master".to_string(),
        "checked with nix flake check --no-build".to_string(),
    ] {
        assert!(log.contains(&want), "{want:?} in\n{log}");
    }

    // The rest still bumps, checked; the consumers' bumps too.
    let out = t.bump_env(&["--check"], &[("SWL_LOG", "swl::cmd=debug")]);
    failed(&out, 5, "swan-lake#1 waits for CI");
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        err.matches("flake check --no-build --no-write-lock-file")
            .count(),
        3,
        "common and both consumers: {err}"
    );
    let bumped = t.rev(COMMON, "master");
    assert_eq!(t.rev(COMMON, "master^"), c2);
    assert_eq!(t.locked(COMMON, &bumped, "zwanenzang"), z2);
    assert_eq!(t.locked(COMMON, &bumped, "zw-next"), n1);
    assert_eq!(t.locked(COMMON, &bumped, "zw-slash"), s2);
    assert_eq!(t.locked(COMMON, &bumped, "zw-pinned"), pinned);
    let body =
        t.f.git(&t.f.forge_repo(COMMON), &["log", "-1", "--format=%b"]);
    assert!(
        body.contains(&format!(
            "zw-next: left at {}: {} is not a forward move",
            short(&n1),
            short(&n2)
        )),
        "{body}"
    );
    assert_eq!(t.rev(COMMON, "current"), bumped);
    assert_eq!(t.locked("swan-lake", BRANCH, "lib"), bumped);
}

#[test]
fn bump_stops_for_what_is_not_its_own() {
    let t = Bump::new("bump-foreign");
    let c1 = t.rev(COMMON, "master");

    // The promotion branch has what master lacks: for the owner.
    let side = t.f.commit_on_forge(COMMON, "current", "side");
    failed(
        &t.bump(&[]),
        3,
        &format!(
            "waits for the owner: common: current at {} has commits that master at {} does not",
            short(&side),
            short(&c1)
        ),
    );
    t.f.git(
        &t.f.forge_repo(COMMON),
        &["update-ref", "refs/heads/current", &c1],
    );

    // A consumer that locks a common ahead of the promoted one: left, and
    // said so.
    let ahead = t.f.commit_on_forge(COMMON, "feature", "ahead");
    let nc_master = t.rev("nixos-config", "master");
    let nc = t.f.clone_elsewhere("nixos-config", "nc-ahead");
    let mut lock: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(nc.join("flake.lock")).unwrap()).unwrap();
    let key = lock["nodes"]["root"]["inputs"]["lib"]
        .as_str()
        .unwrap()
        .to_string();
    lock["nodes"][&key]["locked"]["rev"] = ahead.clone().into();
    fs::write(nc.join("flake.lock"), lock.to_string()).unwrap();
    t.f.git(&nc, &["commit", "-qam", "lib ahead"]);
    t.f.git(&nc, &["push", "-q", "origin", "HEAD:master"]);
    let log = stdout(&t.bump(&[]));
    assert!(
        log.contains(&format!(
            "swl bump: nixos-config\n├─ not bumped: lib locks {}, which is ahead of the promoted \
             {}; left at {}\n╵\n",
            short(&ahead),
            short(&c1),
            short(&ahead)
        )),
        "{log}"
    );
    assert!(log.contains("nothing to bump"), "{log}");
    t.f.git(
        &t.f.forge_repo("nixos-config"),
        &["update-ref", "refs/heads/master", &nc_master],
    );

    // A landing on master that is not promoted: for the owner, nothing
    // written.
    let refs = t.forge_refs();
    let c2 = t.f.commit_on_forge(COMMON, "master", "c2");
    let refs_c2 = t.forge_refs();
    assert_ne!(refs, refs_c2);
    let out = t.bump(&[]);
    failed(
        &out,
        3,
        &format!(
            "waits for the owner: common: master at {} has 1 commit(s) not promoted to current \
             that are not swl bump's ({} swan-lake-common: c2); promote them first (swl promote \
             {c2}), then rerun swl bump",
            short(&c2),
            &c2[..7],
        ),
    );
    assert_eq!(t.forge_refs(), refs_c2);
    assert!(t.f.api.writes().is_empty());

    // Promoted by the owner: nothing to bump in common, so straight to the
    // consumers, which lag.
    t.f.git(
        &t.f.forge_repo(COMMON),
        &["update-ref", "refs/heads/current", &c2],
    );
    let out = t.bump(&[]);
    failed(&out, 5, "swan-lake#1 waits for CI");
    assert!(
        out_text(&out).contains(&format!("├─ nothing to bump at {}", short(&c2))),
        "{}",
        text(&out)
    );
    assert_eq!(t.rev(COMMON, "master"), c2);
    assert_eq!(t.locked("swan-lake", BRANCH, "lib"), c2);

    // A commit that looks like bump's own, common at the target, but that
    // changes more of the lock: not bump's, not merged.
    let forged = t.f.clone_elsewhere("swan-lake", "forged-bump");
    t.f.git(&forged, &["checkout", "-qB", BRANCH, "origin/master"]);
    let mut lock: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(forged.join("flake.lock")).unwrap()).unwrap();
    let key = lock["nodes"]["root"]["inputs"]["lib"]
        .as_str()
        .unwrap()
        .to_string();
    lock["nodes"][&key]["locked"]["rev"] = c2.clone().into();
    lock["nodes"]["stray"] = serde_json::json!({"locked": {"type": "path", "path": "/x"}});
    fs::write(forged.join("flake.lock"), lock.to_string()).unwrap();
    let subject = format!("swl: bump common to {}", short(&c2));
    t.f.git(&forged, &["commit", "-qam", &subject]);
    t.f.git(&forged, &["push", "-q", "--force", "origin", BRANCH]);
    let forged_tip = t.rev("swan-lake", BRANCH);
    let writes = t.f.api.writes().len();
    let out = t.bump(&[]);
    failed(
        &out,
        4,
        &format!(
            "waits for anyone: swan-lake: {BRANCH} at {} has a commit that changes flake.lock \
             beyond common (unexpected new lock node stray)",
            short(&forged_tip)
        ),
    );
    assert_eq!(t.rev("swan-lake", BRANCH), forged_tip);
    assert!(
        !t.f.api.writes()[writes..]
            .iter()
            .any(|w| w.contains("swan-lake/pulls")),
        "{:?}",
        t.f.api.writes()
    );

    // Someone else's commit on swan-lake's bump branch: not updated, for
    // anyone; nixos-config goes on.
    let theirs = t.f.commit_on_forge("swan-lake", BRANCH, "theirs");
    let out = t.bump(&[]);
    failed(
        &out,
        4,
        &format!(
            "waits for anyone: swan-lake: {BRANCH} at {} has commits other than swl bump's own",
            short(&theirs)
        ),
    );
    failed(&out, 4, "waits for nobody: nixos-config#1 waits for CI");
    assert_eq!(t.rev("swan-lake", BRANCH), theirs);
    t.no_worktrees_left();
}

#[test]
fn bump_updates_its_own_branch_and_mr() {
    let t = Bump::new("bump-stale");
    t.f.commit_on_forge("zwanenzang", "master", "z2");
    failed(&t.bump(&[]), 5, "swan-lake#1 waits for CI");
    let first = t.rev(COMMON, "master");
    let old_tip = t.rev("swan-lake", BRANCH);
    let opened =
        t.f.api
            .writes()
            .iter()
            .filter(|w| w.contains("/pulls {"))
            .count();

    // The owner closed swan-lake's bump MR: no new one for the same head,
    // the owner decides.
    let pr_path = "/api/v1/repos/binarin/swan-lake/pulls/1";
    let open_pr = t.f.api.responses.lock().unwrap()[pr_path].clone();
    t.f.api
        .set_pr("swan-lake", 1, "closed", BRANCH, &old_tip, None);
    failed(
        &t.bump(&[]),
        3,
        &format!(
            "waits for the owner: swan-lake#1, the bump MR of {BRANCH} at {}, was closed without \
             merging",
            short(&old_tip)
        ),
    );
    let now =
        t.f.api
            .writes()
            .iter()
            .filter(|w| w.contains("/pulls {"))
            .count();
    assert_eq!(now, opened);
    // Reopened.
    t.f.api
        .responses
        .lock()
        .unwrap()
        .insert(pr_path.into(), open_pr);

    // The consumer's master moves on, and zwanenzang again: the bump
    // branch is rebuilt on the new master, with a lease on its old head,
    // and its MR follows, retitled.
    let master = t.f.commit_on_forge("swan-lake", "master", "consumer-2");
    t.f.commit_on_forge("zwanenzang", "master", "z3");
    let out = t.bump(&[]);
    failed(&out, 5, "swan-lake#1 waits for CI");
    assert!(
        out_text(&out).contains(&format!(
            "as origin's {BRANCH} (replacing its own commit {})",
            short(&old_tip)
        )),
        "{}",
        text(&out)
    );
    let second = t.rev(COMMON, "master");
    assert_eq!(t.rev(COMMON, "master^"), first);
    let tip = t.rev("swan-lake", BRANCH);
    assert_eq!(t.rev("swan-lake", &format!("{BRANCH}^")), master);
    assert_eq!(t.locked("swan-lake", &tip, "lib"), second);
    assert_eq!(t.rev("swan-lake", "refs/pull/1/head"), tip);
    let writes = t.f.api.writes();
    assert_eq!(
        writes.iter().filter(|w| w.contains("/pulls {")).count(),
        opened,
        "no new MR: {writes:?}"
    );
    assert!(
        writes.iter().any(|w| w
            == &format!(
                "PATCH /api/v1/repos/binarin/swan-lake/pulls/1 {{\"title\":\"swl: bump common to {}\"}}",
                short(&second)
            )),
        "{writes:?}"
    );
    let pr = t.f.api.responses.lock().unwrap()["/api/v1/repos/binarin/swan-lake/pulls/1"].clone();
    let pr: serde_json::Value = serde_json::from_str(&pr).unwrap();
    assert!(
        pr["body"]
            .as_str()
            .unwrap()
            .starts_with(&format!("Moves lib to {}, the promoted", short(&second))),
        "{pr}"
    );

    // Green, but the forge finds swan-lake's bump behind its base (master
    // moved meanwhile): for anyone, a rerun rebuilds it.
    t.f.api
        .set_status_without_event("swan-lake", &tip, "success");
    t.f.api.on_merge(
        "swan-lake",
        1,
        &[MergeReply::Status(409, "Not fast-forward".into())],
    );
    failed(
        &t.bump(&[]),
        4,
        &format!(
            "waits for anyone: rerun swl bump, which rebuilds {BRANCH} on master (swan-lake#1 is \
             not mergeable by fast-forward: HTTP 409: Not fast-forward)"
        ),
    );

    // CI succeeds on both: merged by their schedules; done.
    for c in CONSUMERS {
        let tip = t.rev(c, BRANCH);
        t.f.api.set_status(c, &tip, "success");
    }
    let log = stdout(&t.bump(&[]));
    assert!(log.contains("swl bump: nothing to bump"), "{log}");
    for c in CONSUMERS {
        let master = t.rev(c, "master");
        assert_eq!(t.locked(c, &master, "lib"), second);
    }
}

/// A consumer whose input is named `common`, so that the common pin policy
/// applies to its relock, which a `git+file:` common cannot pass.
const CONSUMER_OF_COMMON: &str = r#"{
  inputs.common.url = "git+file://@FORGE@/binarin/swan-lake-common?ref=current";
  outputs = { self, common, ... }: {
    value = builtins.readFile ./value.txt + "+" + common.value;
  };
}
"#;

#[test]
fn bump_reports_a_failing_consumer_and_goes_on() {
    let t = Bump::new("bump-failing");
    t.f.add_consumer_with("aaa-consumer", CONSUMER_OF_COMMON);
    // Common moved and was promoted by hand: every consumer lags.
    let c2 = t.f.commit_on_forge(COMMON, "master", "c2");
    t.f.git(
        &t.f.forge_repo(COMMON),
        &["update-ref", "refs/heads/current", &c2],
    );
    let out = t.bump(&[]);
    // The first consumer fails its relock's pin policy; the others get
    // their MRs.
    failed(
        &out,
        2,
        "swl bump: error: aaa-consumer: the relocked flake.lock breaks the common pin policy",
    );
    failed(&out, 2, "waits for nobody: nixos-config#1 waits for CI");
    failed(&out, 2, "waits for nobody: swan-lake#1 waits for CI");
    assert!(
        t.f.git(
            &t.f.forge_repo("aaa-consumer"),
            &["branch", "--list", BRANCH]
        )
        .is_empty()
    );
    for c in CONSUMERS {
        assert_eq!(t.locked(c, BRANCH, "lib"), c2);
    }
    let list = t.f.git(
        &t.f.source("aaa-consumer"),
        &["worktree", "list", "--porcelain"],
    );
    assert_eq!(list.matches("worktree ").count(), 1, "{list}");
    t.no_worktrees_left();
}

/// A `nix` that, on the first `flake lock` whose arguments contain
/// `$RACE_MATCH`, runs `$RACE_DO` before the real nix: something else moves
/// a branch while bump relocks, between its read and its push.
const RACING_NIX: &str = r#"#!/bin/sh
case "$*" in
  *"flake lock"*"$RACE_MATCH"*)
    if [ -f "$RACE_FLAG" ]; then rm -f "$RACE_FLAG"; sh -c "$RACE_DO"; fi ;;
esac
exec @NIX@ "$@"
"#;

impl Bump {
    /// `swl bump` with [`RACING_NIX`]: `run` once, during the relock of the
    /// scratch worktree named `worktree`.
    fn bump_racing(&self, worktree: &str, run: &str) -> Output {
        let bin = self.f.root.join("racebin");
        fs::create_dir_all(&bin).unwrap();
        let nix = support::find_on_path(&self.f.base_path, "nix");
        let script = RACING_NIX.replace("@NIX@", &nix.display().to_string());
        fs::write(bin.join("nix"), script).unwrap();
        fs::set_permissions(bin.join("nix"), fs::Permissions::from_mode(0o755)).unwrap();
        let flag = self.f.root.join("race-flag");
        fs::write(&flag, "").unwrap();
        let path = format!(
            "{}:{}:{}",
            bin.display(),
            self.f.root.join("fakebin").display(),
            self.f.base_path
        );
        let matching = format!("/{worktree} --reference-lock-file");
        let flag = flag.display().to_string();
        let out = self.f.swl_path(
            &self.f.root,
            &["bump", "--timeout", "0", "--interval", "1"],
            &path,
            &[
                ("SWL_FJ_KEYS", &self.keys),
                ("SWL_ALLOW_FILE_URLS", "1"),
                ("RACE_MATCH", &matching),
                ("RACE_FLAG", &flag),
                ("RACE_DO", run),
            ],
        );
        assert!(
            !self.f.root.join("race-flag").exists(),
            "no race: {}",
            text(&out)
        );
        out
    }

    /// Rewrites `from` to `to` in `repo`'s flake.nix on the forge's master,
    /// relocked.
    fn redeclare(&self, repo: &str, from: &str, to: &str) {
        let dir = self.f.clone_elsewhere(repo, &format!("redeclare-{repo}"));
        let flake = fs::read_to_string(dir.join("flake.nix")).unwrap();
        assert!(flake.contains(from), "{flake}");
        fs::write(dir.join("flake.nix"), flake.replace(from, to)).unwrap();
        stdout(&self.f.nix(&dir, &["flake", "lock"]));
        self.f.git(&dir, &["commit", "-qam", "redeclare lib"]);
        self.f.git(&dir, &["push", "-q", "origin", "HEAD:master"]);
        fs::remove_dir_all(dir).unwrap();
    }
}

#[test]
fn bump_stops_when_what_it_pushes_moved() {
    let t = Bump::new("bump-race");
    let common = t.f.forge_repo(COMMON).display().to_string();
    let c1 = t.rev(COMMON, "master");
    t.f.commit_on_forge("zwanenzang", "master", "z2");

    // Common's master moves while bump relocks: its push is refused, for
    // nobody: rerun.
    let other = t.f.git(
        &t.f.forge_repo(COMMON),
        &[
            "commit-tree",
            "-p",
            &c1,
            "-m",
            "elsewhere",
            &format!("{c1}^{{tree}}"),
        ],
    );
    let out = t.bump_racing(
        COMMON,
        &format!("git -C {common} update-ref refs/heads/master {other}"),
    );
    failed(
        &out,
        5,
        "waits for nobody: common: origin's master moved since it was fetched; rerun swl bump",
    );
    assert_eq!(t.rev(COMMON, "master"), other);
    t.f.git(
        &t.f.forge_repo(COMMON),
        &["update-ref", "refs/heads/master", &c1],
    );
    failed(&t.bump(&[]), 5, "swan-lake#1 waits for CI");

    // swan-lake's bump branch moves while bump rebuilds it: the lease on the
    // head it read refuses the push, for nobody; nixos-config goes on.
    t.f.commit_on_forge("zwanenzang", "master", "z3");
    let consumer = t.f.forge_repo("swan-lake").display().to_string();
    let master = t.rev("swan-lake", "master");
    let out = t.bump_racing(
        "swan-lake",
        &format!("git -C {consumer} update-ref refs/heads/{BRANCH} {master}"),
    );
    failed(
        &out,
        5,
        &format!("waits for nobody: swan-lake: {BRANCH} moved since it was read; rerun swl bump"),
    );
    failed(&out, 5, "nixos-config#1 waits for CI");
    assert_eq!(t.rev("swan-lake", BRANCH), master);
    let bumped = t.rev(COMMON, "master");
    assert_eq!(t.locked("nixos-config", BRANCH, "lib"), bumped);
    t.no_worktrees_left();
}

#[test]
fn bump_needs_one_promotion_branch() {
    let t = Bump::new("bump-branches");
    t.redeclare("nixos-config", "?ref=current", "?ref=master");
    failed(
        &t.bump(&["--dry-run"]),
        2,
        "the consumers declare swan-lake-common at different branches (current, master); swl \
         bump promotes one",
    );
}

#[test]
fn bump_waits_for_the_mirror() {
    let t = Bump::new("bump-mirror");
    // The consumers declare common from a mirror of the forge, which
    // follows it only when told.
    let mirror = t.f.root.join("mirror/binarin/swan-lake-common");
    t.f.git(
        &t.f.root,
        &[
            "clone",
            "-q",
            "--bare",
            &t.f.forge_repo(COMMON).display().to_string(),
            &mirror.display().to_string(),
        ],
    );
    let config = t.f.root.join("repos.toml");
    let text = fs::read_to_string(&config).unwrap().replacen(
        "url_patterns = [",
        &format!(
            "url_patterns = [\"git+file://{}/mirror/binarin/{{repo}}\", ",
            t.f.root.display()
        ),
        1,
    );
    fs::write(&config, text).unwrap();
    let forge_common = format!("{}/binarin/swan-lake-common", t.f.forge.display());
    for c in CONSUMERS {
        t.redeclare(c, &forge_common, &mirror.display().to_string());
    }
    let sync = || {
        let forge = t.f.forge_repo(COMMON).display().to_string();
        t.f.git(
            &mirror,
            &["fetch", "-q", &forge, "+refs/heads/*:refs/heads/*"],
        );
    };
    t.f.commit_on_forge("zwanenzang", "master", "z2");

    // Bumped and promoted on the forge; the mirror does not have it yet.
    let out = t.bump(&[]);
    let bumped = t.rev(COMMON, "master");
    failed(
        &out,
        5,
        &format!(
            "waits for nobody: swan-lake-common master: waiting for file://{} current to have {} \
             (not yet); rerun swl bump",
            mirror.display(),
            short(&bumped)
        ),
    );
    assert_eq!(t.rev(COMMON, "current"), bumped);
    for c in CONSUMERS {
        assert!(
            t.f.git(&t.f.forge_repo(c), &["branch", "--list", BRANCH])
                .is_empty()
        );
    }

    // Mirrored: the consumers get their bumps, locked from the mirror.
    sync();
    let out = t.bump(&[]);
    failed(&out, 5, "swan-lake#1 waits for CI");
    for c in CONSUMERS {
        assert_eq!(t.locked(c, BRANCH, "lib"), bumped);
    }
    let lock = t.f.git(
        &t.f.forge_repo("swan-lake"),
        &["show", &format!("{BRANCH}:flake.lock")],
    );
    assert!(lock.contains(&mirror.display().to_string()), "{lock}");
}

impl Bump {
    /// Lands a copy of `repo`'s bump commit on its master, as when another
    /// run's bump MR of the same change merged (nixos-config#376): the same
    /// parent, tree and subject, another commit. The bump branch and its MR
    /// are left, their change in master already (as nixos-config#377 was).
    /// The new master.
    fn land_a_duplicate(&self, repo: &str) -> String {
        let forge = self.f.forge_repo(repo);
        let tip = self.rev(repo, BRANCH);
        assert_eq!(
            self.rev(repo, &format!("{BRANCH}^")),
            self.rev(repo, "master")
        );
        let subject = self.f.git(&forge, &["log", "-1", "--format=%s", &tip]);
        let copy = self.f.git(
            &forge,
            &[
                "commit-tree",
                "-p",
                "master",
                "-m",
                &subject,
                "-m",
                "made by another run",
                &format!("{tip}^{{tree}}"),
            ],
        );
        self.f
            .git(&forge, &["update-ref", "refs/heads/master", &copy]);
        copy
    }

    /// Whether `repo` has the bump branch on the forge.
    fn has_branch(&self, repo: &str) -> bool {
        !self
            .f
            .git(&self.f.forge_repo(repo), &["branch", "--list", BRANCH])
            .is_empty()
    }

    /// The forge's state of `repo#n`.
    fn pr_state(&self, repo: &str, n: u64) -> String {
        let path = format!("/api/v1/repos/binarin/{repo}/pulls/{n}");
        let pr = self.f.api.responses.lock().unwrap()[&path].clone();
        let pr: serde_json::Value = serde_json::from_str(&pr).unwrap();
        pr["state"].as_str().unwrap().to_string()
    }
}

#[test]
fn bump_closes_and_deletes_its_branch_once_it_is_not_needed() {
    let t = Bump::new("bump-unneeded");
    t.f.commit_on_forge("zwanenzang", "master", "z2");
    failed(&t.bump(&[]), 5, "swan-lake#1 waits for CI");
    let bumped = t.rev(COMMON, "master");
    let tips: Vec<String> = CONSUMERS.iter().map(|c| t.rev(c, BRANCH)).collect();
    // Each consumer's bump lands by another MR: both bump MRs are left
    // with nothing to do. Only the owner may close swan-lake#1.
    for c in CONSUMERS {
        t.land_a_duplicate(c);
    }
    t.f.api.refuse_close("swan-lake", 1, true);

    // A dry run plans the close, and writes nothing.
    let refs = t.forge_refs();
    let writes = t.f.api.writes().len();
    let log = stdout(&t.bump(&["--dry-run"]));
    let want = format!(
        "swl bump: nixos-config\n├─ common is at {} already\n├─ {BRANCH} (at {}) is not needed \
         any more\n├─ would close nixos-config#1, the bump MR of {BRANCH}\n└─ dry run, nothing \
         was written; then: delete {BRANCH} on origin\n",
        short(&bumped),
        short(&tips[0])
    );
    assert!(log.contains(&want), "{want:?} in\n{log}");
    assert_eq!(t.forge_refs(), refs);
    assert_eq!(t.f.api.writes().len(), writes);

    // nixos-config#1 is closed and its branch deleted; closing swan-lake#1
    // is refused: for the owner, on the forge.
    let out = t.bump(&[]);
    failed(
        &out,
        3,
        &format!(
            "waits for the owner: close swan-lake#1 on the forge, \
             https://forgejo.lynx-lizard.ts.net/binarin/swan-lake/pulls/1 (the forge refused: \
             HTTP 403: user should have permission to edit pull request); then rerun swl bump, \
             which deletes {BRANCH}"
        ),
    );
    let log = out_text(&out);
    assert!(
        log.contains(&format!(
            "├─ closed nixos-config#1\n├─ delete {BRANCH} on origin (with a lease on {})\n└─ \
             deleted {BRANCH}\n",
            short(&tips[0])
        )),
        "{log}"
    );
    assert!(!t.has_branch("nixos-config"));
    assert_eq!(t.pr_state("nixos-config", 1), "closed");
    assert!(
        t.f.api.writes().contains(
            &"PATCH /api/v1/repos/binarin/nixos-config/pulls/1 {\"state\":\"closed\"}".to_string()
        ),
        "{:?}",
        t.f.api.writes()
    );
    assert_eq!(t.rev("swan-lake", BRANCH), tips[1]);
    assert_eq!(t.pr_state("swan-lake", 1), "open");

    // The owner closed it; the branch's deletion is refused: for the owner,
    // with the command.
    t.f.api
        .set_pr("swan-lake", 1, "closed", BRANCH, &tips[1], None);
    t.protect("swan-lake", Some(BRANCH));
    let writes = t.f.api.writes().len();
    failed(
        &t.bump(&[]),
        3,
        &format!(
            "waits for the owner: swan-lake: delete {BRANCH}: git -C {} push --delete origin \
             {BRANCH} (the push was refused: remote: {BRANCH} is protected",
            t.f.source("swan-lake").display()
        ),
    );
    assert_eq!(t.f.api.writes().len(), writes, "{:?}", t.f.api.writes());
    assert_eq!(t.rev("swan-lake", BRANCH), tips[1]);

    // Allowed: an unneeded branch without an open MR is deleted.
    t.protect("swan-lake", None);
    let log = stdout(&t.bump(&[]));
    assert!(
        log.contains(&format!(
            "swl bump: swan-lake\n├─ common is at {} already\n├─ {BRANCH} (at {}) is not needed \
             any more\n├─ delete {BRANCH} on origin (with a lease on {})\n└─ deleted {BRANCH}\n",
            short(&bumped),
            short(&tips[1]),
            short(&tips[1])
        )),
        "{log}"
    );
    assert!(!t.has_branch("swan-lake"));
    assert_eq!(t.f.api.writes().len(), writes);

    // A bump branch with someone else's commit is left, and said so.
    let theirs = t.f.commit_on_forge("swan-lake", BRANCH, "theirs");
    let log = stdout(&t.bump(&[]));
    assert!(
        log.contains(&format!(
            "└─ {BRANCH} (at {}) is not needed any more, but it has commits other than swl \
             bump's own; left as it is\n",
            short(&theirs)
        )),
        "{log}"
    );
    assert!(log.contains("swl bump: nothing to bump"), "{log}");
    assert_eq!(t.rev("swan-lake", BRANCH), theirs);
    t.no_worktrees_left();
}

#[test]
fn an_obsolete_bump_mr_is_reported_and_rebuilt_once_common_moves_first() {
    let t = Bump::new("bump-obsolete");
    t.f.commit_on_forge("zwanenzang", "master", "z2");
    failed(&t.bump(&[]), 5, "swan-lake#1 waits for CI");
    let opened = |w: &[String]| w.iter().filter(|w| w.contains("/pulls {")).count();
    let before = opened(&t.f.api.writes());
    let master = t.land_a_duplicate("swan-lake");
    let old_tip = t.rev("swan-lake", BRANCH);

    // Its change is in master: status says to close it, for anyone, and land
    // does not merge it.
    let ws = t.f.root.join("work/obsolete");
    let ws_s = ws.display().to_string();
    let login = [("SWL_FJ_KEYS", t.keys.as_str())];
    stdout(&t.f.swl_env(
        &t.f.root,
        &["start", "--from", "swan-lake#1", "--workspace", &ws_s],
        &login,
    ));
    let row = "obsolete: its change is in origin/master  waits: anyone  next: close swan-lake#1";
    let detail = "swan-lake#1: its change is in origin/master already (merging it changes nothing)";
    for args in [
        vec!["status", "--workspace", &ws_s],
        vec!["status", "--from", "swan-lake#1"],
    ] {
        let log = stdout(&t.f.swl_env(&t.f.root, &args, &login));
        let line = log
            .lines()
            .find(|l| l.starts_with("swan-lake "))
            .unwrap_or_else(|| panic!("no swan-lake row in\n{log}"));
        let line = line.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(
            line.ends_with(&row.split_whitespace().collect::<Vec<_>>().join(" ")),
            "{args:?}: {line}\n{log}"
        );
        assert!(log.contains(detail), "{args:?}:\n{log}");
    }
    let writes = t.f.api.writes().len();
    let out = t.f.swl_env(
        &t.f.root,
        &["land", "--workspace", &ws_s, "--timeout", "0"],
        &login,
    );
    failed(
        &out,
        4,
        "waits for anyone: swan-lake#1: obsolete: its change is in origin/master; close it",
    );
    assert_eq!(
        t.f.api.writes()[writes..],
        [] as [String; 0],
        "land writes nothing"
    );
    assert_eq!(t.rev("swan-lake", "master"), master);

    // Common moves on before bump reruns: the branch is rebuilt on master
    // and its MR reused, retitled; nothing is closed.
    t.f.commit_on_forge("zwanenzang", "master", "z3");
    let out = t.bump(&[]);
    failed(&out, 5, "swan-lake#1 waits for CI");
    assert!(
        out_text(&out).contains(&format!(
            "as origin's {BRANCH} (replacing its own commit {})",
            short(&old_tip)
        )),
        "{}",
        text(&out)
    );
    let second = t.rev(COMMON, "master");
    let tip = t.rev("swan-lake", BRANCH);
    assert_eq!(t.rev("swan-lake", &format!("{BRANCH}^")), master);
    assert_eq!(t.locked("swan-lake", &tip, "lib"), second);
    assert_eq!(t.rev("swan-lake", "refs/pull/1/head"), tip);
    let writes = t.f.api.writes();
    assert_eq!(opened(&writes), before, "no new MR: {writes:?}");
    assert!(
        !writes.iter().any(|w| w.contains("\"state\"")),
        "{writes:?}"
    );
    assert!(
        writes.iter().any(|w| w
            == &format!(
                "PATCH /api/v1/repos/binarin/swan-lake/pulls/1 {{\"title\":\"swl: bump common to {}\"}}",
                short(&second)
            )),
        "{writes:?}"
    );
    assert_eq!(t.pr_state("swan-lake", 1), "open");
}

#[test]
fn bump_starts_a_consumer_over_when_its_base_moves_before_the_push() {
    let t = Bump::new("bump-base-race");
    t.f.commit_on_forge("zwanenzang", "master", "z2");
    failed(&t.bump(&[]), 5, "swan-lake#1 waits for CI");
    let bumped = t.rev(COMMON, "master");

    // swan-lake has no bump branch yet, and another run's bump of the same
    // change lands on its master while this run relocks it: not pushed
    // over, the consumer is started over and is done.
    let sl = t.f.forge_repo("swan-lake");
    let sl_s = sl.display().to_string();
    let read = t.rev("swan-lake", "master");
    let tip = t.rev("swan-lake", BRANCH);
    t.f.api.set_pr("swan-lake", 1, "closed", BRANCH, &tip, None);
    t.f.git(&sl, &["update-ref", "-d", &format!("refs/heads/{BRANCH}")]);
    let copy = t.f.git(
        &sl,
        &[
            "commit-tree",
            "-p",
            "master",
            "-m",
            "swl: bump common, by another run",
            &format!("{tip}^{{tree}}"),
        ],
    );
    let writes = t.f.api.writes().len();
    let out = t.bump_racing(
        "swan-lake",
        &format!("git -C {sl_s} update-ref refs/heads/master {copy}"),
    );
    failed(&out, 5, "nixos-config#1 waits for CI");
    let log = out_text(&out);
    assert!(
        log.contains(&format!(
            "├─ origin/master at {} moved to {} (found before pushing its bump): starting \
             swan-lake over on it\n└─ common is at {} already\n",
            short(&read),
            short(&copy),
            short(&bumped)
        )),
        "{log}"
    );
    assert!(!t.has_branch("swan-lake"));
    assert!(
        !t.f.api.writes()[writes..]
            .iter()
            .any(|w| w.contains("swan-lake/pulls")),
        "{:?}",
        t.f.api.writes()
    );

    // nixos-config's master gets another commit while this run rebuilds its
    // bump branch: rebuilt on that commit, its MR reused.
    t.f.commit_on_forge("zwanenzang", "master", "z3");
    let nc = t.f.forge_repo("nixos-config").display().to_string();
    let nc_read = t.rev("nixos-config", "master");
    let out = t.bump_racing(
        "nixos-config",
        &format!(
            "git -C {nc} update-ref refs/heads/master \
             $(git -C {nc} commit-tree -p master -m elsewhere master^{{tree}})"
        ),
    );
    failed(&out, 5, "nixos-config#1 waits for CI");
    let moved = t.rev("nixos-config", "master");
    assert_ne!(moved, nc_read);
    assert!(
        out_text(&out).contains(&format!(
            "├─ origin/master at {} moved to {} (found before pushing its bump): starting \
             nixos-config over on it\n",
            short(&nc_read),
            short(&moved)
        )),
        "{}",
        text(&out)
    );
    let second = t.rev(COMMON, "master");
    assert_eq!(t.rev("nixos-config", &format!("{BRANCH}^")), moved);
    assert_eq!(t.locked("nixos-config", BRANCH, "lib"), second);
    assert_eq!(
        t.rev("nixos-config", "refs/pull/1/head"),
        t.rev("nixos-config", BRANCH)
    );
    t.no_worktrees_left();
}

#[test]
fn bump_leaves_a_bump_branch_that_moves_common_past_its_target() {
    let t = Bump::new("bump-newer-branch");
    // Common is current everywhere; swan-lake's bump branch, as another run
    // would push it after promoting a newer common, moves lib past it, on
    // swan-lake's master as it is.
    let c1 = t.rev(COMMON, "master");
    let newer = t.f.commit_on_forge(COMMON, "feature", "c2");
    let dir = t.f.clone_elsewhere("swan-lake", "newer-bump");
    t.f.git(&dir, &["checkout", "-qB", BRANCH, "origin/master"]);
    let mut lock: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(dir.join("flake.lock")).unwrap()).unwrap();
    let key = lock["nodes"]["root"]["inputs"]["lib"]
        .as_str()
        .unwrap()
        .to_string();
    lock["nodes"][&key]["locked"]["rev"] = newer.clone().into();
    fs::write(dir.join("flake.lock"), lock.to_string()).unwrap();
    let subject = format!("swl: bump common to {}", short(&newer));
    t.f.git(&dir, &["commit", "-qam", &subject]);
    t.f.git(&dir, &["push", "-q", "origin", BRANCH]);
    let tip = t.rev("swan-lake", BRANCH);
    let sl = t.f.forge_repo("swan-lake");
    t.f.git(&sl, &["update-ref", "refs/pull/1/head", &tip]);
    t.f.api.set_pr("swan-lake", 1, "open", BRANCH, &tip, None);

    // This run has nothing to bump: it leaves that branch and its MR.
    let out = t.bump(&[]);
    failed(
        &out,
        5,
        &format!(
            "waits for nobody: swan-lake: {BRANCH} (at {}) moves lib to {}, past {}, which this \
             run bumps to: another run's newer bump, maybe; left as it is; rerun swl bump",
            short(&tip),
            short(&newer),
            short(&c1)
        ),
    );
    assert_eq!(t.rev("swan-lake", BRANCH), tip);
    assert_eq!(t.pr_state("swan-lake", 1), "open");
    assert!(t.f.api.writes().is_empty(), "{:?}", t.f.api.writes());

    // Its change lands on master: the branch is not needed any more, but
    // the open MR at its head goes into another branch: not closed, for
    // anyone.
    t.land_a_duplicate("swan-lake");
    let path = "/api/v1/repos/binarin/swan-lake/pulls/1";
    let mut pr: serde_json::Value =
        serde_json::from_str(&t.f.api.responses.lock().unwrap()[path]).unwrap();
    pr["base"]["ref"] = "release".into();
    t.f.api
        .responses
        .lock()
        .unwrap()
        .insert(path.into(), pr.to_string());
    failed(
        &t.bump(&[]),
        4,
        &format!(
            "waits for anyone: swan-lake#1, open at {BRANCH}'s head {}, is not swan-lake's bump \
             MR into master (it is from binarin/swan-lake into release): check it on the forge; \
             {BRANCH} is left",
            short(&tip)
        ),
    );
    assert_eq!(t.rev("swan-lake", BRANCH), tip);
    assert!(t.f.api.writes().is_empty(), "{:?}", t.f.api.writes());
}

/// A `post-update` hook that, after each push to the bump branch, moves
/// master on: with `copy`, to a copy of the pushed bump (as another run's
/// MR of the same change merging), once; else to a new commit of master's
/// tree, every time.
fn moving_master(copy: bool) -> String {
    let (guard, parent_tree) = if copy {
        (
            "[ -f hooks/once ] || exit 0; rm -f hooks/once",
            "\"$(git rev-parse refs/heads/@BRANCH@)^{tree}\"",
        )
    } else {
        (":", "\"master^{tree}\"")
    };
    format!(
        "#!/bin/sh\ncase \" $* \" in *\" refs/heads/@BRANCH@ \"*) ;; *) exit 0 ;; esac\n{guard}\n\
         moved=$(git commit-tree -p refs/heads/master -m 'landed meanwhile' {parent_tree})\n\
         git update-ref refs/heads/master \"$moved\"\n"
    )
    .replace("@BRANCH@", BRANCH)
}

#[test]
fn bump_starts_a_consumer_over_when_its_base_moves_before_its_mr_is_opened() {
    let t = Bump::new("bump-base-race-open");
    t.f.commit_on_forge("zwanenzang", "master", "z2");
    let sl = t.f.forge_repo("swan-lake");

    // swan-lake's master moves on after every push of its bump branch: four
    // passes, then it waits for nobody; no MR is opened.
    t.f.hook("swan-lake", "post-update", &moving_master(false));
    let out = t.bump(&[]);
    failed(
        &out,
        5,
        "(found before opening its bump MR), 4 times in this run; rerun swl bump",
    );
    assert_eq!(
        out_text(&out)
            .matches("(found before opening its bump MR): starting swan-lake over on it")
            .count(),
        3,
        "{}",
        text(&out)
    );
    assert!(
        !t.f.api
            .writes()
            .iter()
            .any(|w| w.starts_with("POST /api/v1/repos/binarin/swan-lake/pulls ")),
        "{:?}",
        t.f.api.writes()
    );
    let stale = t.rev("swan-lake", BRANCH);
    assert_ne!(
        t.rev("swan-lake", &format!("{BRANCH}^")),
        t.rev("swan-lake", "master")
    );

    // Next run: the rebuilt bump lands by another MR right after its push
    // (nixos-config#376 against #377): the consumer starts over, has the
    // change, and its branch is deleted without an MR.
    fs::write(sl.join("hooks/once"), "").unwrap();
    t.f.hook("swan-lake", "post-update", &moving_master(true));
    let out = t.bump(&[]);
    failed(&out, 5, "nixos-config#1 waits for CI");
    let log = out_text(&out);
    assert!(
        log.contains(&format!(
            "as origin's {BRANCH} (replacing its own commit {})",
            short(&stale)
        )),
        "{log}"
    );
    let bumped = t.rev(COMMON, "master");
    assert!(
        log.contains(&format!(
            "(found before opening its bump MR): starting swan-lake over on it\n├─ common is at \
             {} already\n├─ {BRANCH} (at ",
            short(&bumped)
        )),
        "{log}"
    );
    assert!(log.contains(&format!("└─ deleted {BRANCH}\n")), "{log}");
    assert!(!t.has_branch("swan-lake"));
    assert_eq!(t.locked("swan-lake", "master", "lib"), bumped);
    assert!(
        !t.f.api
            .writes()
            .iter()
            .any(|w| w.starts_with("POST /api/v1/repos/binarin/swan-lake/pulls ")),
        "{:?}",
        t.f.api.writes()
    );
    t.no_worktrees_left();
}

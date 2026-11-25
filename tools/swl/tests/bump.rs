//! `swl bump` on a disposable forge, over reruns: common's inputs moved
//! forward (and one that is not left), pushed to master, promoted, and a bump
//! MR per consumer opened and merged; the owner-only pushes refused, master's
//! unpromoted commits that are not bump's, and a bump branch with foreign
//! commits; the bump branch rebuilt on a stale base; `--dry-run`, `--check`
//! and the no-op rerun.
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
        log.contains("swl bump: common: up to date: zwanenzang"),
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
            "swl bump: common: zwanenzang {} -> {} (the default branch)",
            short(&z1),
            short(&z2)
        ),
        format!(
            "swl bump: would relock zwanenzang on master at {} commit \"swl: bump zwanenzang\" and \
             push it to origin's master (fast-forward)",
            short(&c1)
        ),
        "then: promote current, wait for its declared source; then the bump MRs of nixos-config, \
         swan-lake"
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
            "swl bump: waits for the owner: push common's bump to master: rerun swl bump as the \
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
        "common: pushed {} to master: swl: bump zwanenzang",
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
        "swl bump: waits for nobody: swan-lake#1 waits for CI, then it merges",
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
        "swl bump: waits for the owner: merge nixos-config#1 (the forge refused: HTTP 405: User not \
         allowed to merge PR)",
    );
    assert!(
        out_text(&out).contains(&format!(
            "swan-lake: common is at {} already",
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

/// Common with a second input, zwanenzang's `next`.
const COMMON_WITH_NEXT: &str = r#"{
  inputs.zwanenzang.url = "git+file://@FORGE@/binarin/zwanenzang";
  inputs.zw-next.url = "git+file://@FORGE@/binarin/zwanenzang?ref=next";
  outputs = { self, ... }: {
    value = builtins.readFile ./value.txt;
  };
}
"#;

#[test]
fn bump_leaves_an_input_that_does_not_move_forward() {
    let t = Bump::new("bump-rebased");
    // Common pins zwanenzang's `next` too, as of n1.
    let n1 = t.f.commit_on_forge("zwanenzang", "next", "n1");
    let seed = t.f.clone_elsewhere(COMMON, "common-next-seed");
    fs::write(
        seed.join("flake.nix"),
        COMMON_WITH_NEXT.replace("@FORGE@", &t.f.forge.display().to_string()),
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

    let log = stdout(&t.bump(&["--dry-run", "--check"]));
    for want in [
        format!(
            "common: zwanenzang {} -> {} (the default branch)",
            short(&z1),
            short(&z2)
        ),
        format!(
            "common: zw-next: not bumped: {} -> {} (next) is not a forward move, the new rev does \
             not descend from the locked one; left at {}",
            short(&n1),
            short(&n2),
            short(&n1)
        ),
        "would relock zwanenzang on master".to_string(),
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
            "nixos-config: not bumped: lib locks {}, which is ahead of the promoted {}; left at {}",
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
             that are not swl bump's ({} swan-lake-common: c2); promote them first (git -C {} \
             push origin {c2}:refs/heads/current), then rerun swl bump",
            short(&c2),
            &c2[..7],
            t.common_source()
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
        out_text(&out).contains(&format!("common: nothing to bump at {}", short(&c2))),
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

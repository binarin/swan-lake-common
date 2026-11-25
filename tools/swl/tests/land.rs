//! `swl land` on a disposable forge, over reruns from the shared MR to the
//! removed workspace: the merge requested, refused, not fast-forward,
//! merged; the promotion refused by a protected branch, pushed; the mirror
//! waited for; the consumer finalized, pushed and merged; `--dry-run` and
//! `--keep`.

mod support;

use std::{
    fs,
    path::{Path, PathBuf},
    process::Output,
};

use support::{Forge, MergeReply, failed, stdout, text};

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

struct Change {
    f: Forge,
    ws: PathBuf,
    consumer: PathBuf,
    common: PathBuf,
    mirror: PathBuf,
    keys: String,
}

impl Change {
    /// The forge with a mirror of common that the consumer declares (as
    /// swan-lake declares GitHub), and a published change: common#1, a
    /// draft, and the consumer's swan-lake#2 naming it.
    fn new(name: &str) -> Self {
        let f = Forge::new(name);
        f.api.make_private("swan-lake");
        let keys = f.login().display().to_string();
        let mirror = f.root.join("mirror/binarin/swan-lake-common");
        f.git(
            &f.root,
            &[
                "clone",
                "-q",
                "--bare",
                &f.forge_repo("swan-lake-common").display().to_string(),
                &mirror.display().to_string(),
            ],
        );
        let config = fs::read_to_string(f.root.join("repos.toml")).unwrap();
        let config = config.replacen(
            "url_patterns = [",
            &format!(
                "url_patterns = [\"git+file://{}/mirror/binarin/{{repo}}\", ",
                f.root.display()
            ),
            1,
        );
        fs::write(f.root.join("repos.toml"), config).unwrap();
        // The consumer's master declares lib from the mirror.
        let seed = f.clone_elsewhere("swan-lake", "consumer-seed");
        let flake = fs::read_to_string(seed.join("flake.nix")).unwrap();
        fs::write(
            seed.join("flake.nix"),
            flake.replace(
                &format!("{}/binarin/swan-lake-common", f.forge.display()),
                &mirror.display().to_string(),
            ),
        )
        .unwrap();
        stdout(&f.nix(&seed, &["flake", "lock"]));
        f.git(&seed, &["commit", "-qam", "lib from the mirror"]);
        f.git(&seed, &["push", "-q", "origin", "HEAD:master"]);
        f.follow_pull_refs("swan-lake-common");
        f.follow_pull_refs("swan-lake");

        let work = f.root.join("work");
        fs::create_dir_all(&work).unwrap();
        stdout(&f.swl(
            &work,
            &["start", "SL-0020-land", "swan-lake", "swan-lake-common@lib"],
        ));
        let ws = work.join("SL-0020-land");
        let t = Self {
            consumer: ws.join("swan-lake"),
            common: ws.join("swan-lake-common@lib"),
            ws,
            mirror,
            keys,
            f,
        };
        let c2 = t.commit(&t.common, "c2");
        t.f.git(&t.common, &["push", "-q", "origin", "sl-0020-land"]);
        let forge_common = t.f.forge_repo("swan-lake-common");
        t.f.git(&forge_common, &["update-ref", "refs/pull/1/head", &c2]);
        t.f.api
            .set_pr("swan-lake-common", 1, "draft", "sl-0020-land", &c2, None);
        t.commit(&t.consumer, "consumer-2");
        fs::write(
            t.consumer.join("swl.toml"),
            "[inputs.lib]\nrepo = \"swan-lake-common\"\nmr = 1\n",
        )
        .unwrap();
        t.f.git(&t.consumer, &["add", "swl.toml"]);
        t.f.git(&t.consumer, &["commit", "-qm", "swl: manifest"]);
        t.f.git(&t.consumer, &["push", "-q", "origin", "sl-0020-land"]);
        let head = t.f.git(&t.consumer, &["rev-parse", "HEAD"]);
        let forge_consumer = t.f.forge_repo("swan-lake");
        t.f.git(&forge_consumer, &["update-ref", "refs/pull/2/head", &head]);
        t.f.api
            .set_pr("swan-lake", 2, "open", "sl-0020-land", &head, None);
        t
    }

    fn commit(&self, dir: &Path, value: &str) -> String {
        fs::write(dir.join("value.txt"), value).unwrap();
        self.f.git(dir, &["commit", "-qam", value]);
        self.f.git(dir, &["rev-parse", "HEAD"])
    }

    /// `swl land --from` in the work directory, where `swl start` made the
    /// workspace.
    fn land_from(&self, extra: &[&str]) -> Output {
        let mut args = vec!["land", "--from", "swan-lake#2", "--timeout", "0"];
        args.extend(extra);
        self.f.swl_env(
            &self.f.root.join("work"),
            &args,
            &[("SWL_FJ_KEYS", &self.keys)],
        )
    }

    fn land(&self, extra: &[&str]) -> Output {
        self.land_env(extra, &[])
    }

    fn land_env(&self, extra: &[&str], env: &[(&str, &str)]) -> Output {
        let ws = self.ws.display().to_string();
        let mut args = vec![
            "land",
            "--workspace",
            &ws,
            "--timeout",
            "0",
            "--interval",
            "1",
        ];
        args.extend(extra);
        let mut env = env.to_vec();
        env.push(("SWL_FJ_KEYS", &self.keys));
        let out = self.f.swl_env(&self.f.root, &args, &env);
        assert!(!text(&out).contains(support::TOKEN), "{}", text(&out));
        out
    }

    /// Every ref of the forge repos.
    fn forge_refs(&self) -> String {
        ["swan-lake-common", "swan-lake"]
            .iter()
            .map(|r| self.f.git(&self.f.forge_repo(r), &["for-each-ref"]))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn sync_mirror(&self) {
        let forge = self.f.forge_repo("swan-lake-common").display().to_string();
        self.f.git(
            &self.mirror,
            &["fetch", "-q", &forge, "+refs/heads/*:refs/heads/*"],
        );
    }
}

fn out_text(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn land_runs_to_the_end_over_reruns() {
    let t = Change::new("land");
    let forge_common = t.f.forge_repo("swan-lake-common");
    let c2 = t.f.git(&t.common, &["rev-parse", "HEAD"]);

    // A dry run writes nothing.
    let refs = t.forge_refs();
    let out = t.land(&["--dry-run"]);
    let log = stdout(&out);
    assert!(
        log.contains("would mark swan-lake-common#1 ready: \"t\""),
        "{log}"
    );
    assert!(
        log.contains(&format!(
            "would request the merge of swan-lake-common#1 at {}",
            &c2[..12]
        )),
        "{log}"
    );
    assert!(t.f.api.writes().is_empty(), "{:?}", t.f.api.writes());
    assert_eq!(t.forge_refs(), refs);
    // From the published MR, in its existing workspace.
    let ws = t.ws.display().to_string();
    let log = stdout(&t.land_from(&["--dry-run", "--workspace", &ws]));
    assert!(log.contains("would mark swan-lake-common#1 ready"), "{log}");
    assert!(!log.contains("would run swl start"), "{log}");
    // Without one (by default ./<consumer branch>): the plan for the shared
    // MRs, and nothing created.
    let log = stdout(&t.land_from(&["--dry-run"]));
    let default_ws = t.f.root.join("work/sl-0020-land");
    assert!(
        log.contains(&format!(
            "would run swl start --from swan-lake#2 (the workspace {})",
            default_ws.display()
        )),
        "{log}"
    );
    assert!(!default_ws.exists());
    assert!(
        log.contains("would request the merge of swan-lake-common#1"),
        "{log}"
    );
    assert!(t.f.api.writes().is_empty(), "{:?}", t.f.api.writes());

    // Marked ready, the merge requested: it waits for CI.
    t.f.api
        .on_merge("swan-lake-common", 1, &[MergeReply::Schedule]);
    let out = t.land(&[]);
    failed(
        &out,
        5,
        "waits for nobody: swan-lake-common#1 waits for CI, then it merges",
    );
    let writes = t.f.api.writes();
    assert_eq!(writes.len(), 2, "{writes:?}");
    assert_eq!(
        writes[0],
        "PATCH /api/v1/repos/binarin/swan-lake-common/pulls/1 {\"title\":\"t\"}"
    );
    let post: serde_json::Value = serde_json::from_str(
        writes[1]
            .strip_prefix("POST /api/v1/repos/binarin/swan-lake-common/pulls/1/merge ")
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        post,
        serde_json::json!({"Do": "fast-forward-only", "merge_when_checks_succeed": true,
                           "delete_branch_after_merge": true, "head_commit_id": c2})
    );
    let requests = t.f.api.requests.lock().unwrap().clone();
    assert!(
        requests
            .iter()
            .filter(|r| r.method != "GET")
            .all(|r| r.auth.as_deref() == Some(&format!("token {}", support::TOKEN))),
        "{requests:?}"
    );

    // The user may not merge: for the owner. The base moved: rebase.
    t.f.api.on_merge(
        "swan-lake-common",
        1,
        &[
            MergeReply::Status(405, "User not allowed to merge PR".into()),
            MergeReply::Status(409, "Not fast-forward".into()),
        ],
    );
    failed(
        &t.land(&[]),
        3,
        "waits for the owner: merge swan-lake-common#1 (the forge refused: HTTP 405: User not allowed to merge PR)",
    );
    failed(
        &t.land(&[]),
        4,
        "waits for anyone: rebase sl-0020-land onto master (swl never force-pushes), then swl publish swan-lake (",
    );
    assert_eq!(t.f.api.writes().len(), 4, "the title is set once");
    // A forge that says merged but shows it open: not asked twice in a run.
    t.f.api.on_merge(
        "swan-lake-common",
        1,
        &[MergeReply::Status(200, String::new())],
    );
    failed(&t.land(&[]), 5, "done once in this run already");
    assert_eq!(t.f.api.writes().len(), 5);

    // Merged now; the promotion branch is protected: for the owner, with
    // the command.
    t.f.hook("swan-lake-common", "pre-receive", PROTECT_CURRENT);
    t.f.api
        .on_merge("swan-lake-common", 1, &[MergeReply::MergeNow]);
    let out = t.land(&[]);
    let source = t.f.source("swan-lake-common").display().to_string();
    failed(
        &out,
        3,
        &format!(
            "waits for the owner: promote: git -C {source} push origin {c2}:refs/heads/current (the push was refused: remote: current is protected"
        ),
    );
    assert!(out_text(&out).contains("swan-lake-common#1 is merged"));
    assert_eq!(t.f.git(&forge_common, &["rev-parse", "master"]), c2);
    assert!(
        t.f.git(&forge_common, &["branch", "--list", "sl-0020-land"])
            .is_empty()
    );

    // A dry run at the promotion.
    t.f.hook_opt("swan-lake-common", "pre-receive", None);
    let refs = t.forge_refs();
    let writes = t.f.api.writes().len();
    let log = stdout(&t.land(&["--dry-run"]));
    assert!(
        log.contains("would promote swan-lake-common#1: git -C"),
        "{log}"
    );
    assert_eq!(t.forge_refs(), refs);
    assert_eq!(t.f.api.writes().len(), writes);

    // Promoted; the mirror does not have it yet.
    let out = t.land(&[]);
    failed(&out, 5, "waits for nobody: swan-lake-common#1: waiting for");
    failed(&out, 5, "mirror/binarin/swan-lake-common current to have");
    assert_eq!(t.f.git(&forge_common, &["rev-parse", "current"]), c2);

    // Mirrored: the consumer lands; its merge waits for CI.
    t.sync_mirror();
    let before = t.f.git(&t.consumer, &["rev-parse", "HEAD"]);
    let log = stdout(&t.land(&["--dry-run"]));
    assert!(log.contains("would finalize swan-lake:"), "{log}");
    assert!(log.contains("lib (swan-lake-common#1): lands"), "{log}");
    assert_eq!(t.f.git(&t.consumer, &["rev-parse", "HEAD"]), before);
    t.f.api.on_merge("swan-lake", 2, &[MergeReply::Schedule]);
    let out = t.land(&[]);
    failed(&out, 5, "swan-lake#2 waits for CI, then it merges");
    let landing = t.f.git(&t.consumer, &["rev-parse", "HEAD"]);
    assert_eq!(t.f.git(&t.consumer, &["rev-parse", "HEAD~1"]), before);
    assert_eq!(
        t.f.git(&t.consumer, &["log", "-1", "--format=%s"]),
        "swl: land swan-lake-common#1 (lib)"
    );
    let forge_consumer = t.f.forge_repo("swan-lake");
    assert_eq!(
        t.f.git(&forge_consumer, &["rev-parse", "sl-0020-land"]),
        landing
    );
    let last = t.f.api.writes().last().unwrap().clone();
    assert!(
        last.starts_with("POST /api/v1/repos/binarin/swan-lake/pulls/2/merge ")
            && last.contains(&landing),
        "{last}"
    );

    // Merged, kept with --keep; removed on the next run.
    t.f.api.on_merge("swan-lake", 2, &[MergeReply::MergeNow]);
    let log = stdout(&t.land(&["--keep"]));
    assert!(log.contains("swan-lake#2 is merged"), "{log}");
    assert!(log.contains("keeping the workspace"), "{log}");
    assert_eq!(t.f.git(&forge_consumer, &["rev-parse", "master"]), landing);
    assert!(t.ws.exists());
    let log = stdout(&t.land(&["--dry-run"]));
    assert!(log.contains("would remove"), "{log}");
    assert!(t.consumer.exists());
    let log = stdout(&t.land(&[]));
    assert!(
        log.contains("deleted the merged branch sl-0020-land of swan-lake-common"),
        "{log}"
    );
    assert!(!t.ws.exists(), "{log}");
    for repo in ["swan-lake-common", "swan-lake"] {
        let source = t.f.source(repo);
        assert!(
            t.f.git(&source, &["branch", "--list", "sl-0020-land"])
                .is_empty()
        );
        let worktrees = t.f.git(&source, &["worktree", "list"]);
        assert!(!worktrees.contains("SL-0020-land"), "{worktrees}");
    }
    // Landed: nothing left to do, and no workspace is made for it.
    let log = stdout(&t.land_from(&[]));
    assert!(
        log.contains("swl land: swan-lake#2 has landed: nothing left to do"),
        "{log}"
    );
    assert!(!t.ws.exists());
}

impl Change {
    /// The forge mirrors common's `current` as soon as it moves.
    fn mirror_at_once(&self) {
        self.f.hook(
            "swan-lake-common",
            "post-update",
            &format!(
                "#!/bin/sh\ngit push -q --force {} refs/heads/current:refs/heads/current\n",
                self.mirror.display()
            ),
        );
    }

    /// Ignores `patterns` in every worktree of `repo`'s source.
    fn ignore(&self, repo: &str, patterns: &str) {
        let info = self.f.source(repo).join("info");
        fs::create_dir_all(&info).unwrap();
        fs::write(info.join("exclude"), patterns).unwrap();
    }
}

#[test]
fn land_keeps_checkouts_with_work_it_would_delete() {
    let t = Change::new("land-keep");
    t.mirror_at_once();
    t.f.api
        .on_merge("swan-lake-common", 1, &[MergeReply::MergeNow]);
    t.f.api.on_merge("swan-lake", 2, &[MergeReply::MergeNow]);
    // Local work on the shared checkout, not pushed; an ignored file in the
    // consumer, besides ignored files that may go.
    fs::write(t.common.join("notes.txt"), "wip").unwrap();
    t.ignore("swan-lake", "progress.md\n.direnv/\nresult\n");
    fs::write(t.consumer.join("progress.md"), "notes").unwrap();
    fs::create_dir_all(t.consumer.join(".direnv")).unwrap();
    fs::write(t.consumer.join(".direnv/cache"), "x").unwrap();
    std::os::unix::fs::symlink("/nonexistent", t.consumer.join("result")).unwrap();
    let log = stdout(&t.land(&[]));
    assert!(log.contains("swan-lake#2 is merged"), "{log}");
    assert!(
        log.contains(&format!(
            "keeping {}: uncommitted or untracked files: ?? notes.txt",
            t.common.display()
        )),
        "{log}"
    );
    assert!(
        log.contains(&format!(
            "keeping {}: ignored files that removing it would delete: progress.md\n",
            t.consumer.display()
        )),
        "{log}"
    );
    assert!(t.common.exists() && t.consumer.exists(), "{log}");
    assert!(log.contains("and the workspace"), "{log}");
    // Without the ignored file, the consumer goes, .direnv and result too.
    fs::remove_file(t.consumer.join("progress.md")).unwrap();
    let log = stdout(&t.land(&[]));
    assert!(!t.consumer.exists(), "{log}");
    assert!(t.common.exists(), "{log}");
}

#[test]
fn land_has_nothing_to_land_in_a_fresh_workspace() {
    let f = Forge::new("land-fresh");
    let keys = f.login().display().to_string();
    let work = f.root.join("work");
    fs::create_dir_all(&work).unwrap();
    stdout(&f.swl(
        &work,
        &[
            "start",
            "SL-0021-fresh",
            "swan-lake",
            "swan-lake-common@lib",
        ],
    ));
    let ws = work.join("SL-0021-fresh").display().to_string();
    let log = stdout(&f.swl_env(
        &f.root,
        &["land", "--workspace", &ws],
        &[("SWL_FJ_KEYS", &keys)],
    ));
    assert!(log.contains("nothing to land"), "{log}");
    assert!(work.join("SL-0021-fresh/swan-lake").exists(), "{log}");
    assert!(f.api.writes().is_empty());
}

#[test]
fn land_checks_the_consumer_mr_and_branch() {
    let t = Change::new("land-mismatch");
    t.mirror_at_once();
    t.f.api
        .on_merge("swan-lake-common", 1, &[MergeReply::MergeNow]);
    // A dry run needs no fj login.
    let ws = t.ws.display().to_string();
    let log = stdout(&t.f.swl_env(
        &t.f.root,
        &["land", "--dry-run", "--workspace", &ws],
        &[("SWL_FJ_KEYS", "/nonexistent/keys.json")],
    ));
    assert!(log.contains("would mark swan-lake-common#1 ready"), "{log}");
    // A draft flag without a WIP: title is for someone to clear on the forge.
    let path = "/api/v1/repos/binarin/swan-lake-common/pulls/1";
    let pr = t.f.api.responses.lock().unwrap()[path].clone();
    let mut v: serde_json::Value = serde_json::from_str(&pr).unwrap();
    v["title"] = "t".into();
    t.f.api
        .responses
        .lock()
        .unwrap()
        .insert(path.into(), v.to_string());
    failed(
        &t.land(&[]),
        4,
        "swan-lake-common#1 is a draft without a WIP: title; mark it ready on the forge",
    );
    v["draft"] = false.into();
    t.f.api
        .responses
        .lock()
        .unwrap()
        .insert(path.into(), v.to_string());
    // The MR at the consumer's head is another branch's.
    let head = t.f.git(&t.consumer, &["rev-parse", "HEAD"]);
    t.f.api.set_pr("swan-lake", 2, "open", "other", &head, None);
    let out = t.land(&[]);
    failed(&out, 4, "swan-lake#2 is not the MR of sl-0020-land at");
    assert!(String::from_utf8_lossy(&out.stdout).contains("swan-lake-common#1 is merged"));
    assert_eq!(
        t.f.git(&t.consumer, &["rev-parse", "HEAD"]),
        head,
        "nothing finalized"
    );
    // The consumer's remote branch moved on elsewhere: rebase first.
    t.f.api
        .set_pr("swan-lake", 2, "open", "sl-0020-land", &head, None);
    t.f.commit_on_forge("swan-lake", "sl-0020-land", "theirs");
    let out = t.land(&[]);
    failed(&out, 4, "merge or rebase onto origin/sl-0020-land");
    assert_eq!(t.f.git(&t.consumer, &["rev-parse", "HEAD"]), head);
    assert!(
        !t.f.api
            .writes()
            .iter()
            .any(|w| w.contains("swan-lake/pulls/2")),
        "{:?}",
        t.f.api.writes()
    );
}

impl Change {
    /// The head of common#1.
    fn common_head(&self) -> String {
        self.f.git(&self.common, &["rev-parse", "HEAD"])
    }

    /// The forge writes since `from`, without the bodies of merge requests:
    /// `METHOD path`, and for a merge request whether it waits for checks.
    fn writes_since(&self, from: usize) -> Vec<String> {
        self.f.api.writes()[from..]
            .iter()
            .map(|w| {
                let (head, body) = w.split_once(" {").map_or((w.as_str(), ""), |(h, b)| (h, b));
                let head = head.trim_end();
                match head.ends_with("/merge") && head.starts_with("POST") {
                    true if body.contains("\"merge_when_checks_succeed\":true") => {
                        format!("{head} when checks succeed")
                    }
                    true => format!("{head} now"),
                    false => head.to_string(),
                }
            })
            .collect()
    }
}

const COMMON_MERGE: &str = "/api/v1/repos/binarin/swan-lake-common/pulls/1/merge";

#[test]
fn land_merges_an_mr_with_green_checks_directly() {
    let t = Change::new("land-green");
    t.mirror_at_once();
    let c2 = t.common_head();
    t.f.api.set_status("swan-lake-common", &c2, "success");

    // A dry run says so, and writes nothing.
    let log = stdout(&t.land(&["--dry-run"]));
    assert!(
        log.contains(&format!(
            "would merge swan-lake-common#1 at {} now, its checks being green",
            &c2[..12]
        )),
        "{log}"
    );
    assert!(log.contains("then: swan-lake-common#1 is merged"), "{log}");
    assert!(t.f.api.writes().is_empty(), "{:?}", t.f.api.writes());

    // A plain merge, after cancelling whatever may be scheduled (nothing).
    // With SWL_LOG=debug, the writes and the decisions on stderr, without
    // the token (which `land` checks).
    let out = t.land_env(&[], &[("SWL_LOG", "debug")]);
    let err = String::from_utf8_lossy(&out.stderr);
    for want in [
        &format!("DELETE http://127.0.0.1:{}{COMMON_MERGE}", t.f.api.port),
        "status=404",
        "no scheduled merge to cancel",
        "the pass ends",
        "authed=true",
    ] {
        assert!(err.contains(want), "{want:?} in {err}");
    }
    let log = out_text(&out);
    assert!(log.contains("swan-lake-common#1 is merged"), "{log}");
    assert!(!log.contains("swan-lake-common#1 waits for CI"), "{log}");
    assert!(!log.contains("cancelled the scheduled merge"), "{log}");
    let writes = t.f.api.writes();
    let post = writes
        .iter()
        .find_map(|w| w.strip_prefix(&format!("POST {COMMON_MERGE} ")))
        .unwrap();
    let post: serde_json::Value = serde_json::from_str(post).unwrap();
    assert_eq!(
        post,
        serde_json::json!({"Do": "fast-forward-only", "delete_branch_after_merge": true,
                           "head_commit_id": c2})
    );
    assert_eq!(
        t.writes_since(0)[..3],
        [
            "PATCH /api/v1/repos/binarin/swan-lake-common/pulls/1".to_string(),
            format!("DELETE {COMMON_MERGE}"),
            format!("POST {COMMON_MERGE} now"),
        ]
    );
    // The consumer's landing has no status yet: its merge is scheduled.
    failed(&out, 5, "swan-lake#2 waits for CI, then it merges");
    assert!(t.f.api.is_scheduled("swan-lake", 2));
    assert_eq!(
        t.writes_since(3).last().unwrap(),
        "POST /api/v1/repos/binarin/swan-lake/pulls/2/merge when checks succeed"
    );

    // CI reports success on the landing: the forge merges it by itself, and
    // the rerun finds it merged.
    let landing = t.f.git(&t.consumer, &["rev-parse", "HEAD"]);
    let writes = t.f.api.writes().len();
    t.f.api.set_status("swan-lake", &landing, "success");
    assert!(!t.f.api.is_scheduled("swan-lake", 2));
    let log = stdout(&t.land(&[]));
    assert!(log.contains("every MR of the change is merged"), "{log}");
    assert!(!t.ws.exists(), "{log}");
    assert_eq!(t.f.api.writes().len(), writes, "nothing more to write");
}

#[test]
fn land_cancels_a_scheduled_merge_once_the_checks_are_green() {
    let t = Change::new("land-scheduled");
    t.mirror_at_once();
    let c2 = t.common_head();
    t.f.api.set_status("swan-lake-common", &c2, "pending");

    // Pending: a dry run plans the scheduled merge; the run schedules it.
    let log = stdout(&t.land(&["--dry-run"]));
    assert!(
        log.contains("then: swan-lake-common#1 merges when its checks succeed"),
        "{log}"
    );
    assert!(t.f.api.writes().is_empty());
    failed(
        &t.land(&[]),
        5,
        "waits for nobody: swan-lake-common#1 waits for CI, then it merges",
    );
    assert_eq!(
        t.writes_since(0),
        [
            "PATCH /api/v1/repos/binarin/swan-lake-common/pulls/1".to_string(),
            format!("POST {COMMON_MERGE} when checks succeed"),
        ]
    );
    assert!(t.f.api.is_scheduled("swan-lake-common", 1));

    // Green without a status event that would start the scheduled merge:
    // it stays scheduled on the forge. A dry run plans the direct merge.
    t.f.api
        .set_status_without_event("swan-lake-common", &c2, "success");
    assert!(t.f.api.is_scheduled("swan-lake-common", 1));
    let log = stdout(&t.land(&["--dry-run"]));
    assert!(log.contains("would merge swan-lake-common#1"), "{log}");
    assert!(!log.contains("waits for CI"), "{log}");
    assert_eq!(t.f.api.writes().len(), 2, "a dry run writes nothing");

    // Cancelling is refused: for the owner.
    t.f.api
        .on_cancel("swan-lake-common", 1, &[(403, "not the scheduler")]);
    failed(
        &t.land(&[]),
        3,
        "waits for the owner: merge swan-lake-common#1 (cancelling its scheduled merge was refused: HTTP 403: not the scheduler)",
    );
    assert_eq!(t.writes_since(2), [format!("DELETE {COMMON_MERGE}")]);

    // Cancelled, then the direct merge is refused: for the owner, as before.
    t.f.api.on_merge(
        "swan-lake-common",
        1,
        &[MergeReply::Status(
            405,
            "User not allowed to merge PR".into(),
        )],
    );
    let out = t.land(&[]);
    failed(
        &out,
        3,
        "waits for the owner: merge swan-lake-common#1 (the forge refused: HTTP 405: User not allowed to merge PR)",
    );
    assert!(
        out_text(&out).contains("cancelled the scheduled merge of swan-lake-common#1"),
        "{}",
        text(&out)
    );
    assert!(!t.f.api.is_scheduled("swan-lake-common", 1));
    assert_eq!(
        t.writes_since(3),
        [
            format!("DELETE {COMMON_MERGE}"),
            format!("POST {COMMON_MERGE} now"),
        ]
    );

    // Allowed now: nothing left to cancel, merged directly.
    let out = t.land(&[]);
    let log = out_text(&out);
    assert!(log.contains("swan-lake-common#1 is merged"), "{log}");
    assert!(!log.contains("swan-lake-common#1 waits for CI"), "{log}");
    assert_eq!(
        t.writes_since(5)[..2],
        [
            format!("DELETE {COMMON_MERGE}"),
            format!("POST {COMMON_MERGE} now"),
        ]
    );
    assert_eq!(
        t.f.git(
            &t.f.forge_repo("swan-lake-common"),
            &["rev-parse", "master"]
        ),
        c2
    );
    failed(&out, 5, "swan-lake#2 waits for CI, then it merges");
}

#[test]
fn land_merges_directly_when_the_checks_turn_green_as_the_merge_is_scheduled() {
    let t = Change::new("land-race");
    t.mirror_at_once();
    let c2 = t.common_head();
    t.f.api.set_status("swan-lake-common", &c2, "pending");
    t.f.api.on_merge(
        "swan-lake-common",
        1,
        &[MergeReply::ScheduleAsChecksTurnGreen],
    );
    let out = t.land(&[]);
    let log = out_text(&out);
    assert!(
        log.contains("swan-lake-common#1: its checks turned green as the merge was scheduled"),
        "{log}"
    );
    assert!(
        log.contains("cancelled the scheduled merge of swan-lake-common#1"),
        "{log}"
    );
    assert!(log.contains("swan-lake-common#1 is merged"), "{log}");
    assert!(!log.contains("swan-lake-common#1 waits for CI"), "{log}");
    assert_eq!(
        t.writes_since(0)[..4],
        [
            "PATCH /api/v1/repos/binarin/swan-lake-common/pulls/1".to_string(),
            format!("POST {COMMON_MERGE} when checks succeed"),
            format!("DELETE {COMMON_MERGE}"),
            format!("POST {COMMON_MERGE} now"),
        ]
    );
    assert!(!t.f.api.is_scheduled("swan-lake-common", 1));
    // On to the consumer, in the same run.
    failed(&out, 5, "swan-lake#2 waits for CI, then it merges");
}

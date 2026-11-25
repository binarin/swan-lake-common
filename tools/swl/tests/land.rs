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
        "waits for anyone: rebase sl-0020-land onto master: rerun swl land, or swl refresh (",
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

    // Colour: never into a pipe by default; with --color always or
    // CLICOLOR_FORCE; never with NO_COLOR or --color never.
    let colored = |args: &[&str], env: &[(&str, &str)]| {
        let mut all = vec!["land", "--workspace", &ws];
        all.extend(args);
        let mut env = env.to_vec();
        env.push(("SWL_FJ_KEYS", &keys));
        let out = stdout(&f.swl_env(&f.root, &all, &env));
        let plain = out.replace("\x1b[1mswl land\x1b[0m: ", "swl land: ");
        assert!(!plain.contains('\x1b'), "{out:?}");
        assert!(plain.contains("swl land: nothing to land"), "{out:?}");
        out != plain
    };
    assert!(!colored(&[], &[]));
    assert!(colored(&["--color", "always"], &[]));
    assert!(colored(&[], &[("CLICOLOR_FORCE", "1")]));
    assert!(!colored(&[], &[("CLICOLOR_FORCE", "1"), ("NO_COLOR", "1")]));
    assert!(!colored(&["--color", "never"], &[("CLICOLOR_FORCE", "1")]));
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

    // A plain merge. With SWL_LOG=debug, the writes and the decisions on
    // stderr, without the token (which `land` checks).
    let out = t.land_env(&[], &[("SWL_LOG", "debug")]);
    let err = String::from_utf8_lossy(&out.stderr);
    for want in [
        &format!("POST http://127.0.0.1:{}{COMMON_MERGE}", t.f.api.port),
        "status=200",
        "the pass ends",
        "authed=true",
    ] {
        assert!(err.contains(want), "{want:?} in {err}");
    }
    let log = out_text(&out);
    assert!(log.contains("swan-lake-common#1 is merged"), "{log}");
    assert!(!log.contains("swan-lake-common#1 waits for CI"), "{log}");
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
        t.writes_since(0)[..2],
        [
            "PATCH /api/v1/repos/binarin/swan-lake-common/pulls/1".to_string(),
            format!("POST {COMMON_MERGE} now"),
        ]
    );
    // The consumer's landing has no status yet: its merge is scheduled.
    failed(&out, 5, "swan-lake#2 waits for CI, then it merges");
    assert!(t.f.api.is_scheduled("swan-lake", 2));
    assert_eq!(
        t.writes_since(2).last().unwrap(),
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

/// Forgejo's 405 for a branch protection `reason`.
fn protected(reason: &str) -> MergeReply {
    MergeReply::Status(405, format!("not allowed to merge [reason: {reason}]"))
}

#[test]
fn land_merges_a_scheduled_mr_directly_once_its_checks_are_green() {
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

    // Green as far as reported, but a required check has not reported: the
    // direct merge is refused, and the schedule stays (Forgejo answers that
    // it is scheduled already).
    t.f.api.on_merge(
        "swan-lake-common",
        1,
        &[protected("Not all required status checks successful")],
    );
    let out = t.land(&[]);
    failed(
        &out,
        5,
        "waits for nobody: swan-lake-common#1: a required status check has not passed yet (HTTP 405: not allowed to merge [reason: Not all required status checks successful]); its merge is scheduled for when its checks succeed; rerun swl land",
    );
    assert!(t.f.api.is_scheduled("swan-lake-common", 1));
    assert_eq!(
        t.writes_since(2),
        [
            format!("POST {COMMON_MERGE} now"),
            format!("POST {COMMON_MERGE} when checks succeed"),
        ]
    );

    // Allowed now: merged directly; the merge deletes the schedule.
    let out = t.land(&[]);
    let log = out_text(&out);
    assert!(log.contains("swan-lake-common#1 is merged"), "{log}");
    assert!(!log.contains("swan-lake-common#1 waits for CI"), "{log}");
    assert_eq!(t.writes_since(4)[0], format!("POST {COMMON_MERGE} now"));
    assert!(!t.f.api.is_scheduled("swan-lake-common", 1));
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
fn land_keeps_or_makes_the_schedule_when_a_green_mr_may_not_be_merged() {
    let t = Change::new("land-refused");
    let c2 = t.common_head();
    t.f.api.set_status("swan-lake-common", &c2, "success");

    // Not on the merge whitelist: for the owner; the merge is scheduled.
    t.f.api.on_merge(
        "swan-lake-common",
        1,
        &[MergeReply::Status(
            405,
            "User not allowed to merge PR".into(),
        )],
    );
    failed(
        &t.land(&[]),
        3,
        "waits for the owner: merge swan-lake-common#1 (the forge refused: HTTP 405: User not allowed to merge PR); its merge is scheduled for when its checks succeed",
    );
    assert!(t.f.api.is_scheduled("swan-lake-common", 1));
    assert_eq!(
        t.writes_since(1),
        [
            format!("POST {COMMON_MERGE} now"),
            format!("POST {COMMON_MERGE} when checks succeed"),
        ]
    );

    // Missing approvals: for the owner; the schedule stays.
    t.f.api.on_merge(
        "swan-lake-common",
        1,
        &[protected("Does not have enough approvals")],
    );
    failed(
        &t.land(&[]),
        3,
        "waits for the owner: approve swan-lake-common#1 on the forge (HTTP 405: not allowed to merge [reason: Does not have enough approvals]); its merge is scheduled for when its checks succeed; then rerun swl land",
    );
    assert!(t.f.api.is_scheduled("swan-lake-common", 1));

    // Behind the base: rebase, for anyone.
    t.f.api.on_merge(
        "swan-lake-common",
        1,
        &[protected("The head branch is behind the base branch")],
    );
    failed(
        &t.land(&[]),
        4,
        "waits for anyone: rebase sl-0020-land onto master: rerun swl land, or swl refresh (",
    );
    assert!(t.f.api.is_scheduled("swan-lake-common", 1));

    // Scheduling is refused too: only reported.
    t.f.api.scheduled.lock().unwrap().clear();
    let refused = MergeReply::Status(405, "User not allowed to merge PR".into());
    t.f.api
        .on_merge("swan-lake-common", 1, &[refused.clone(), refused]);
    failed(
        &t.land(&[]),
        3,
        "merge swan-lake-common#1 (the forge refused: HTTP 405: User not allowed to merge PR); scheduling its merge was refused too: HTTP 405: User not allowed to merge PR",
    );
    assert!(!t.f.api.is_scheduled("swan-lake-common", 1));
    let writes = t.f.api.writes();
    assert!(
        writes
            .iter()
            .all(|w| w.starts_with("POST ") || w.starts_with("PATCH ")),
        "{writes:?}"
    );
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
    assert!(log.contains("swan-lake-common#1 is merged"), "{log}");
    assert!(!log.contains("swan-lake-common#1 waits for CI"), "{log}");
    assert_eq!(
        t.writes_since(0)[..3],
        [
            "PATCH /api/v1/repos/binarin/swan-lake-common/pulls/1".to_string(),
            format!("POST {COMMON_MERGE} when checks succeed"),
            format!("POST {COMMON_MERGE} now"),
        ]
    );
    assert!(!t.f.api.is_scheduled("swan-lake-common", 1));
    // On to the consumer, in the same run.
    failed(&out, 5, "swan-lake#2 waits for CI, then it merges");

    // The race again on the consumer, whose direct merge a required check
    // refuses: the schedule from before stays, not asked for again.
    let landing = t.f.git(&t.consumer, &["rev-parse", "HEAD"]);
    t.f.api.scheduled.lock().unwrap().clear();
    t.f.api.set_status("swan-lake", &landing, "pending");
    let writes = t.f.api.writes().len();
    t.f.api.on_merge(
        "swan-lake",
        2,
        &[
            MergeReply::ScheduleAsChecksTurnGreen,
            protected("Not all required status checks successful"),
        ],
    );
    failed(
        &t.land(&[]),
        5,
        "swan-lake#2: a required status check has not passed yet (HTTP 405: not allowed to merge [reason: Not all required status checks successful]); its merge stays scheduled for when its checks succeed",
    );
    assert!(t.f.api.is_scheduled("swan-lake", 2));
    assert_eq!(
        t.writes_since(writes),
        [
            "POST /api/v1/repos/binarin/swan-lake/pulls/2/merge when checks succeed".to_string(),
            "POST /api/v1/repos/binarin/swan-lake/pulls/2/merge now".to_string(),
        ]
    );
}

#[test]
fn land_reads_a_500_to_a_fast_forward_merge_by_ancestry() {
    let t = Change::new("land-500");
    let c2 = t.common_head();
    t.f.api.set_status("swan-lake-common", &c2, "success");
    let forge_common = t.f.forge_repo("swan-lake-common");

    // A 500 to a head that contains the base: an error, as the forge said.
    t.f.api.on_merge(
        "swan-lake-common",
        1,
        &[MergeReply::Status(500, String::new())],
    );
    failed(
        &t.land(&[]),
        2,
        "merge request of swan-lake-common#1: HTTP 500",
    );

    // The base moves as the merge is requested: Forgejo's 500 is told as
    // not fast-forward by the head that no longer contains master.
    t.f.api
        .on_merge("swan-lake-common", 1, &[MergeReply::BaseMovesFirst]);
    let out = t.land(&[]);
    let moved = t.f.git(&forge_common, &["rev-parse", "master"]);
    failed(
        &out,
        4,
        &format!(
            "swan-lake-common#1 is not mergeable by fast-forward: HTTP 500; its head {} does not \
             contain origin/master at {})",
            &c2[..12],
            &moved[..12]
        ),
    );
    assert_eq!(
        t.writes_since(0),
        [
            "PATCH /api/v1/repos/binarin/swan-lake-common/pulls/1".to_string(),
            format!("POST {COMMON_MERGE} now"),
            format!("POST {COMMON_MERGE} now"),
        ]
    );

    // Known behind the base, and not rebased (an untracked file in its
    // checkout): the head is not asked to merge at all.
    fs::write(t.common.join("notes.txt"), "wip").unwrap();
    let writes = t.f.api.writes().len();
    let out = t.land(&[]);
    failed(
        &out,
        4,
        &format!(
            "waits for anyone: swan-lake-common#1: sl-0020-land does not contain origin/master at \
             {}, and its checkout {} is not rebased: it has uncommitted or untracked files: ?? \
             notes.txt; commit or remove them; then rerun swl land (or swl refresh)",
            &moved[..12],
            t.common.display()
        ),
    );
    assert_eq!(t.f.api.writes().len(), writes, "{:?}", t.f.api.writes());
    assert_eq!(t.f.git(&forge_common, &["rev-parse", "master"]), moved);
    assert_eq!(t.common_head(), c2);
}

impl Change {
    /// `swl refresh` of the workspace.
    fn refresh(&self, extra: &[&str]) -> Output {
        let ws = self.ws.display().to_string();
        let mut args = vec!["refresh", "--workspace", &ws, "--timeout", "0"];
        args.extend(extra);
        self.f
            .swl_env(&self.f.root, &args, &[("SWL_FJ_KEYS", &self.keys)])
    }

    /// The head of `branch` in the forge's `repo`.
    fn forge_head(&self, repo: &str, branch: &str) -> String {
        self.f.git(&self.f.forge_repo(repo), &["rev-parse", branch])
    }

    /// Runs `script` once when a push from `repo`'s source reaches the
    /// forge, before the forge reads the push (git's
    /// `remote.origin.receivepack`); the file that says it ran.
    fn before_receive(&self, repo: &str, script: &str) -> PathBuf {
        let wrapper = self.f.root.join(format!("receive-pack-{repo}"));
        let ran = self.f.root.join(format!("receive-pack-{repo}.ran"));
        fs::write(
            &wrapper,
            format!(
                "#!/bin/sh\nif [ ! -e {ran} ]; then\ntouch {ran}\n{script}\nfi\nexec git \
                 receive-pack \"$@\"\n",
                ran = ran.display()
            ),
        )
        .unwrap();
        let mut perms = fs::metadata(&wrapper).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
        fs::set_permissions(&wrapper, perms).unwrap();
        self.f.git(
            &self.f.source(repo),
            &[
                "config",
                "remote.origin.receivepack",
                &wrapper.display().to_string(),
            ],
        );
        ran
    }

    /// The worktrees of `repo`'s source, by path.
    fn worktrees(&self, repo: &str) -> Vec<String> {
        self.f
            .git(&self.f.source(repo), &["worktree", "list", "--porcelain"])
            .lines()
            .filter(|l| l.starts_with("worktree "))
            .map(str::to_string)
            .collect()
    }
}

#[test]
fn land_rebases_a_shared_mr_behind_its_base_and_schedules_its_merge() {
    let t = Change::new("land-rebase");
    t.mirror_at_once();
    let c2 = t.common_head();
    t.f.api.set_status("swan-lake-common", &c2, "success");
    let moved =
        t.f.commit_file_on_forge("swan-lake-common", "master", "other.txt", "o");

    // A dry run plans the rebase and writes nothing.
    let refs = t.forge_refs();
    let worktrees = t.worktrees("swan-lake-common");
    let log = stdout(&t.land(&["--dry-run"]));
    assert!(
        log.contains(&format!(
            "would rebase sl-0020-land of swan-lake-common#1 (1 commit) onto origin/master at {}, \
             and push it with --force-with-lease=refs/heads/sl-0020-land:{}",
            &moved[..12],
            &c2[..12]
        )),
        "{log}"
    );
    assert!(
        log.contains("then: CI runs on the new head of swan-lake-common#1"),
        "{log}"
    );
    assert_eq!(t.forge_refs(), refs);
    assert_eq!(t.worktrees("swan-lake-common"), worktrees);
    assert_eq!(t.common_head(), c2);
    assert!(t.f.api.writes().is_empty(), "{:?}", t.f.api.writes());

    // Rebased in a scratch worktree, pushed with a lease on the head read,
    // the checkout moved to it; its CI is pending, so its merge is
    // scheduled, and nothing is merged now.
    let out = t.land_env(&[], &[("SWL_LOG", "swl::cmd=debug")]);
    failed(
        &out,
        5,
        "waits for nobody: swan-lake-common#1 waits for CI, then it merges",
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains(&format!("--force-with-lease=refs/heads/sl-0020-land:{c2}")),
        "{err}"
    );
    let rebased = t.forge_head("swan-lake-common", "sl-0020-land");
    assert_ne!(rebased, c2);
    assert_eq!(
        t.f.git(&t.common, &["rev-parse", "HEAD^"]),
        moved,
        "on the new master"
    );
    assert_eq!(t.common_head(), rebased);
    assert_eq!(t.f.git(&t.common, &["status", "--porcelain"]), "");
    assert_eq!(
        t.forge_head("swan-lake-common", "refs/pull/1/head"),
        rebased
    );
    assert_eq!(t.forge_head("swan-lake-common", "master"), moved);
    assert_eq!(t.worktrees("swan-lake-common"), worktrees);
    assert_eq!(
        t.writes_since(0),
        [
            "PATCH /api/v1/repos/binarin/swan-lake-common/pulls/1".to_string(),
            format!("POST {COMMON_MERGE} when checks succeed"),
        ]
    );
    assert!(t.f.api.writes()[1].contains(&rebased));

    // swl status shows it with CI pending; swl refresh has nothing to do.
    let ws = t.ws.display().to_string();
    let status = stdout(&t.f.swl_env(
        &t.f.root,
        &["status", "--workspace", &ws],
        &[("SWL_FJ_KEYS", &t.keys)],
    ));
    assert!(
        support::line(&status, "└─ lib: swan-lake-common@lib").contains("CI pending"),
        "{status}"
    );
    let log = stdout(&t.refresh(&[]));
    assert!(
        log.contains("swan-lake-common#1: sl-0020-land contains origin/master already"),
        "{log}"
    );
}

#[test]
fn refresh_is_a_no_op_when_every_branch_contains_its_base() {
    let t = Change::new("refresh-current");
    let refs = t.forge_refs();
    let log = stdout(&t.refresh(&[]));
    assert!(
        log.contains("swan-lake-common#1: sl-0020-land contains origin/master already"),
        "{log}"
    );
    assert!(
        log.contains("swan-lake#2: sl-0020-land contains origin/master already"),
        "{log}"
    );
    assert_eq!(t.forge_refs(), refs);
    assert!(t.f.api.writes().is_empty(), "{:?}", t.f.api.writes());
}

#[test]
fn refresh_stops_for_a_checkout_that_is_not_clean_or_not_at_the_remote_head() {
    let t = Change::new("refresh-stops");
    let c2 = t.common_head();
    t.f.commit_file_on_forge("swan-lake-common", "master", "other.txt", "o");
    let refs = t.forge_refs();
    let stops = |needle: &str| {
        let out = t.refresh(&[]);
        failed(&out, 4, needle);
        assert_eq!(t.forge_refs(), refs);
    };

    // A dry run says what it would do, and writes nothing.
    let log = stdout(&t.refresh(&["--dry-run"]));
    assert!(
        log.contains("would rebase sl-0020-land of swan-lake-common#1"),
        "{log}"
    );
    assert!(log.contains("dry run, nothing was written"), "{log}");
    assert_eq!(t.forge_refs(), refs);

    // Changes to a tracked file.
    fs::write(t.common.join("value.txt"), "edited").unwrap();
    stops("is not rebased: it has uncommitted or untracked files:  M value.txt");
    t.f.git(&t.common, &["checkout", "-q", "value.txt"]);
    // On another branch.
    t.f.git(&t.common, &["switch", "-q", "--detach"]);
    stops("is not rebased: it is on a detached HEAD, not sl-0020-land; switch to sl-0020-land");
    t.f.git(&t.common, &["switch", "-q", "sl-0020-land"]);
    // An unpushed commit.
    t.commit(&t.common, "c3");
    stops(
        "is not rebased: sl-0020-land has commits that origin/sl-0020-land lacks; push them \
         first (swl publish, or swl land for a landing commit)",
    );
    t.f.git(&t.common, &["reset", "-q", "--hard", &c2]);
    // A commit pushed elsewhere: the forge's MR follows it.
    t.f.commit_file_on_forge("swan-lake-common", "sl-0020-land", "theirs.txt", "t");
    let refs = t.forge_refs();
    let out = t.refresh(&[]);
    failed(
        &out,
        4,
        "is not rebased: origin/sl-0020-land has commits that sl-0020-land lacks; git pull \
         --ff-only first",
    );
    assert_eq!(t.forge_refs(), refs);
    assert_eq!(t.common_head(), c2);
}

#[test]
fn refresh_aborts_a_conflicting_rebase_and_leaves_everything_as_it_was() {
    let t = Change::new("refresh-conflict");
    let c2 = t.common_head();
    // Master changes the file that the MR changes.
    t.f.commit_on_forge("swan-lake-common", "master", "c-other");
    let refs = t.forge_refs();
    let worktrees = t.worktrees("swan-lake-common");
    let out = t.refresh(&[]);
    failed(
        &out,
        4,
        &format!(
            "swan-lake-common#1: sl-0020-land does not contain origin/master at {}, and rebasing \
             it conflicts in {} c2: value.txt; nothing was changed: rebase it by hand in {}, \
             push it, then rerun swl refresh",
            &t.forge_head("swan-lake-common", "master")[..12],
            &c2[..7],
            t.common.display()
        ),
    );
    assert_eq!(t.forge_refs(), refs);
    assert_eq!(t.common_head(), c2);
    assert_eq!(t.f.git(&t.common, &["status", "--porcelain"]), "");
    assert_eq!(t.worktrees("swan-lake-common"), worktrees);
}

#[test]
fn refresh_loses_a_race_for_the_branch_to_its_lease() {
    let t = Change::new("refresh-lease");
    let c2 = t.common_head();
    t.f.commit_file_on_forge("swan-lake-common", "master", "other.txt", "o");
    // Someone pushes to the branch after swl read it: here as swl's push
    // reaches the forge, before the forge advertises its refs.
    let forge = t.f.forge_repo("swan-lake-common").display().to_string();
    let ran = t.before_receive(
        "swan-lake-common",
        &format!(
            "c=$(git --git-dir={forge} commit-tree {c2}^{{tree}} -p {c2} -m race)\n\
             git --git-dir={forge} update-ref refs/heads/sl-0020-land \"$c\"\n"
        ),
    );
    let out = t.refresh(&[]);
    failed(
        &out,
        5,
        &format!(
            "waits for nobody: swan-lake-common#1: origin/sl-0020-land moved since it was read at \
             {}, so the lease refused the push; nothing was changed: rerun swl refresh",
            &c2[..12]
        ),
    );
    assert!(ran.exists(), "the race ran");
    let race = t.forge_head("swan-lake-common", "sl-0020-land");
    assert_eq!(
        t.f.git(
            &t.f.forge_repo("swan-lake-common"),
            &["log", "-1", "--format=%s", &race]
        ),
        "race"
    );
    assert_eq!(t.common_head(), c2);
}

#[test]
fn land_rebases_a_shared_mr_without_a_checkout_in_a_scratch_worktree() {
    let t = Change::new("land-scratch");
    let c2 = t.common_head();
    let source = t.f.source("swan-lake-common");
    t.f.git(
        &source,
        &["worktree", "remove", &t.common.display().to_string()],
    );
    let moved =
        t.f.commit_file_on_forge("swan-lake-common", "master", "other.txt", "o");
    let worktrees = t.worktrees("swan-lake-common");

    // From the published MR, without a workspace: the plan.
    let log = stdout(&t.land_from(&["--dry-run"]));
    assert!(
        log.contains(&format!(
            "would rebase sl-0020-land of swan-lake-common#1 (1 commit) onto origin/master at {}",
            &moved[..12]
        )),
        "{log}"
    );
    assert_eq!(t.forge_head("swan-lake-common", "sl-0020-land"), c2);

    failed(
        &t.land(&[]),
        5,
        "swan-lake-common#1 waits for CI, then it merges",
    );
    let rebased = t.forge_head("swan-lake-common", "sl-0020-land");
    assert_eq!(
        t.f.git(&source, &["rev-parse", &format!("{rebased}^")]),
        moved
    );
    assert_eq!(
        t.f.git(&source, &["log", "-1", "--format=%s", &rebased]),
        "c2"
    );
    assert_eq!(t.worktrees("swan-lake-common"), worktrees);
    assert!(
        t.f.git(&source, &["branch", "--list", "swl-refresh-*"])
            .is_empty()
    );
}

/// The subjects of `branch`'s commits that `master` lacks, newest first.
fn subjects(t: &Change, repo: &str, branch: &str) -> Vec<String> {
    t.f.git(
        &t.f.forge_repo(repo),
        &["log", "--format=%s", &format!("master..{branch}")],
    )
    .lines()
    .map(str::to_string)
    .collect()
}

#[test]
fn land_makes_the_consumer_landing_anew_on_a_master_whose_lock_moved() {
    let t = Change::new("land-relanding");
    t.mirror_at_once();
    t.f.api
        .on_merge("swan-lake-common", 1, &[MergeReply::MergeNow]);
    failed(&t.land(&[]), 5, "swan-lake#2 waits for CI, then it merges");
    assert!(t.f.api.is_scheduled("swan-lake", 2));
    let landed = [
        "swl: land swan-lake-common#1 (lib)",
        "swl: manifest",
        "consumer-2",
    ];
    assert_eq!(subjects(&t, "swan-lake", "sl-0020-land"), landed);

    // Master moves without its lock: rebased with the landing as it is.
    let m1 =
        t.f.commit_file_on_forge("swan-lake", "master", "other.txt", "o");
    let out = t.land(&[]);
    failed(&out, 5, "swan-lake#2 waits for CI, then it merges");
    let log = out_text(&out);
    assert!(
        log.contains(&format!(
            "rebase sl-0020-land of swan-lake#2 (3 commits) onto origin/master at {}, and push",
            &m1[..12]
        )),
        "{log}"
    );
    assert!(!log.contains("finalized anew"), "{log}");
    let head = t.forge_head("swan-lake", "sl-0020-land");
    assert_eq!(subjects(&t, "swan-lake", "sl-0020-land"), landed);
    assert_eq!(t.f.git(&t.consumer, &["rev-parse", "HEAD~3"]), m1);
    assert_eq!(t.f.git(&t.consumer, &["rev-parse", "HEAD"]), head);

    // A bump on master moves the lock: the landing is dropped, the rest
    // rebased and finalized anew on the new lock, and pushed once.
    let c3 = t.f.commit_on_forge("swan-lake-common", "master", "c3");
    let forge_common = t.f.forge_repo("swan-lake-common");
    t.f.git(&forge_common, &["update-ref", "refs/heads/current", &c3]);
    t.sync_mirror();
    let bumper = t.f.clone_elsewhere("swan-lake", "bumper");
    stdout(&t.f.nix(&bumper, &["flake", "update", "lib"]));
    t.f.git(&bumper, &["commit", "-qam", "swl: bump common to c3"]);
    t.f.git(&bumper, &["push", "-q", "origin", "HEAD:master"]);
    let m2 = t.forge_head("swan-lake", "master");

    // While the landing cannot be made anew (the mirror lost the merge
    // commit), nothing is pushed.
    let c1 = t.f.git(&forge_common, &["rev-parse", &format!("{c3}~2")]);
    t.f.git(&t.mirror, &["update-ref", "refs/heads/current", &c1]);
    let out = t.refresh(&[]);
    failed(
        &out,
        4,
        "swan-lake#2: sl-0020-land does not contain origin/master at",
    );
    failed(
        &out,
        4,
        "and its landing commit, dropped to rebase it, cannot be made anew: it landed lib, but \
         what lands now is none:\n  lib (swan-lake-common#1): stays: merged as",
    );
    assert!(out_text(&out).contains("nothing was pushed"));
    assert_eq!(t.forge_head("swan-lake", "sl-0020-land"), head);
    assert_eq!(t.f.git(&t.consumer, &["rev-parse", "HEAD"]), head);
    assert!(
        t.f.git(
            &t.f.source("swan-lake"),
            &["branch", "--list", "swl-refresh-*"]
        )
        .is_empty()
    );
    t.sync_mirror();

    let out = t.land_env(&[], &[("SWL_LOG", "swl::cmd=debug")]);
    failed(&out, 5, "swan-lake#2 waits for CI, then it merges");
    let log = out_text(&out);
    assert!(
        log.contains(&format!(
            "rebase sl-0020-land of swan-lake#2 (2 commits, without swl's landing commit {}, as \
             origin/master moved flake.lock) onto origin/master at {} and finalize it anew, and \
             push it with --force-with-lease=refs/heads/sl-0020-land:{}",
            &head[..12],
            &m2[..12],
            &head[..12]
        )),
        "{log}"
    );
    assert!(
        log.contains("swan-lake#2: finalized anew: swl: land swan-lake-common#1 (lib)"),
        "{log}"
    );
    // swl env ran again for the moved lock.
    assert!(log.contains("swan-lake: lib←swan-lake-common@lib"), "{log}");
    let err = String::from_utf8_lossy(&out.stderr);
    let pushes = err
        .lines()
        .filter(|l| l.contains(" push --quiet ") && l.contains(":refs/heads/sl-0020-land"))
        .count();
    assert_eq!(pushes, 1, "{err}");
    let relanded = t.forge_head("swan-lake", "sl-0020-land");
    assert_eq!(subjects(&t, "swan-lake", "sl-0020-land"), landed);
    assert_eq!(t.f.git(&t.consumer, &["rev-parse", "HEAD"]), relanded);
    assert_eq!(t.f.git(&t.consumer, &["rev-parse", "HEAD~3"]), m2);
    assert_eq!(
        t.f.git(
            &t.consumer,
            &["status", "--porcelain", "--untracked-files=no"]
        ),
        ""
    );
    assert!(!t.consumer.join("swl.toml").exists());
    let lock = |rev: &str| {
        t.f.git(
            &t.f.forge_repo("swan-lake"),
            &["show", &format!("{rev}:flake.lock")],
        )
    };
    assert_eq!(lock(&relanded), lock(&m2), "the bumped lock has c3 already");
    assert!(lock(&m2).contains(&c3));
    assert!(t.f.api.is_scheduled("swan-lake", 2));
    let last = t.f.api.writes().last().unwrap().clone();
    assert!(
        last.starts_with("POST /api/v1/repos/binarin/swan-lake/pulls/2/merge ")
            && last.contains(&relanded),
        "{last}"
    );
}

#[test]
fn refresh_moves_no_checkout_that_changed_during_the_run() {
    for (name, change, want) in [
        (
            "commit",
            "git -C @ commit -q --allow-empty -m mine",
            "sl-0020-land is at",
        ),
        (
            "edit",
            "echo edited > @/value.txt",
            "it has uncommitted or untracked files now:  M value.txt",
        ),
        (
            "switch",
            "git -C @ switch -q --detach",
            "sl-0020-land was moved to it, but the checkout is on a detached HEAD now",
        ),
    ] {
        let t = Change::new(&format!("refresh-moved-{name}"));
        let c2 = t.common_head();
        t.f.commit_file_on_forge("swan-lake-common", "master", "other.txt", "o");
        // The checkout changes as swl pushes.
        let change = change.replace("@", &t.common.display().to_string());
        t.before_receive(
            "swan-lake-common",
            &format!("env -u GIT_DIR -u GIT_INDEX_FILE -u GIT_WORK_TREE sh -c '{change}'"),
        );
        let out = t.refresh(&[]);
        let pushed = t.forge_head("swan-lake-common", "sl-0020-land");
        assert_ne!(pushed, c2, "{name}: {}", text(&out));
        failed(
            &out,
            4,
            &format!(
                "swan-lake-common#1: pushed, but its checkout {} was not moved: {want}",
                t.common.display()
            ),
        );
        let branch =
            t.f.git(&t.common, &["rev-parse", "refs/heads/sl-0020-land"]);
        match name {
            "commit" => {
                assert_eq!(t.f.git(&t.common, &["rev-parse", "HEAD^"]), c2);
                assert_eq!(branch, t.common_head());
            }
            "edit" => {
                assert_eq!(branch, c2);
                assert_eq!(t.common_head(), c2);
                assert_eq!(
                    fs::read_to_string(t.common.join("value.txt")).unwrap(),
                    "edited\n"
                );
            }
            _ => {
                assert_eq!(branch, pushed);
                assert_eq!(t.common_head(), c2);
            }
        }
    }
}

impl Change {
    /// Changes the forge's JSON of `repo#n`.
    fn patch_pr(&self, repo: &str, n: u64, f: impl FnOnce(&mut serde_json::Value)) {
        let path = format!("/api/v1/repos/binarin/{repo}/pulls/{n}");
        let mut responses = self.f.api.responses.lock().unwrap();
        let mut v: serde_json::Value = serde_json::from_str(&responses[&path]).unwrap();
        f(&mut v);
        responses.insert(path, v.to_string());
    }
}

#[test]
fn refresh_rebases_only_the_changes_own_mr_branches_into_the_base() {
    let t = Change::new("refresh-foreign");
    let c2 = t.common_head();
    let mut v = serde_json::Value::Null;
    t.patch_pr("swan-lake-common", 1, |pr| v = pr.clone());
    let restore = |t: &Change| t.patch_pr("swan-lake-common", 1, |pr| *pr = v.clone());

    // An MR from a fork that contains its base needs nothing.
    t.patch_pr("swan-lake-common", 1, |pr| {
        pr["head"]["repo"]["full_name"] = "someone/swan-lake-common".into()
    });
    let log = stdout(&t.refresh(&[]));
    assert!(
        log.contains("swan-lake-common#1: sl-0020-land contains origin/master already"),
        "{log}"
    );

    // Behind its base, it is not rebased: nor is an MR into another branch,
    // one whose head branch is a promotion branch, or one whose branch is
    // not at the forge's head.
    t.f.commit_file_on_forge("swan-lake-common", "master", "other.txt", "o");
    let refs = t.forge_refs();
    let stops = |code: i32, needle: &str| {
        failed(&t.refresh(&[]), code, needle);
        assert_eq!(t.forge_refs(), refs);
        assert_eq!(t.common_head(), c2);
    };
    stops(
        4,
        "swan-lake-common#1's head branch is in someone/swan-lake-common, not \
         binarin/swan-lake-common: swl does not rebase an MR from another repo",
    );
    restore(&t);
    t.patch_pr("swan-lake-common", 1, |pr| {
        pr["base"]["ref"] = "current".into()
    });
    stops(
        4,
        "swan-lake-common#1 merges into current, not master: swl rebases only MRs into the \
         repo's base",
    );
    restore(&t);
    t.patch_pr("swan-lake-common", 1, |pr| {
        pr["head"]["ref"] = "swan-lake-current".into()
    });
    stops(
        4,
        "swan-lake-common#1: its head branch swan-lake-current is a promotion branch; swl \
         rewrites only the change's own MR branches",
    );
    restore(&t);
    // The forge's pull ref lags (or leads) the branch.
    let c1 = t.f.git(&t.common, &["rev-parse", "HEAD^"]);
    let forge_common = t.f.forge_repo("swan-lake-common");
    t.f.git(&forge_common, &["update-ref", "refs/pull/1/head", &c1]);
    let refs = t.forge_refs();
    failed(
        &t.refresh(&[]),
        5,
        &format!(
            "swan-lake-common#1: the forge has its head at {}, origin/sl-0020-land is at {}",
            &c1[..12],
            &c2[..12]
        ),
    );
    assert_eq!(t.forge_refs(), refs);
    assert_eq!(t.common_head(), c2);
}

#[test]
fn refresh_refuses_a_branch_with_merge_commits() {
    let t = Change::new("refresh-merges");
    let c2 = t.common_head();
    // A merge on the branch, as the forge's "update branch" makes one.
    t.f.git(&t.common, &["switch", "-qc", "side", "HEAD^"]);
    fs::write(t.common.join("side.txt"), "s").unwrap();
    t.f.git(&t.common, &["add", "side.txt"]);
    t.f.git(&t.common, &["commit", "-qm", "side"]);
    t.f.git(&t.common, &["switch", "-q", "sl-0020-land"]);
    t.f.git(
        &t.common,
        &["merge", "-q", "--no-ff", "-m", "merge side", "side"],
    );
    t.f.git(&t.common, &["push", "-q", "origin", "sl-0020-land"]);
    let merge = t.common_head();
    t.f.commit_file_on_forge("swan-lake-common", "master", "other.txt", "o");
    let refs = t.forge_refs();
    failed(
        &t.refresh(&["--dry-run"]),
        0,
        &format!(
            "and it has merge commits, which a rebase would not keep (nor what they resolve): {} \
             merge side; rebase it by hand",
            &merge[..7]
        ),
    );
    failed(
        &t.refresh(&[]),
        4,
        "merge side; rebase it by hand, push it, then rerun swl refresh",
    );
    assert_eq!(t.forge_refs(), refs);
    assert_eq!(t.common_head(), merge);
    assert_ne!(merge, c2);
}

impl Change {
    /// Common#1 merged by hand at its head, promoted and mirrored; the
    /// merge commit.
    fn merge_common_by_hand(&self) -> String {
        let c2 = self.common_head();
        let forge_common = self.f.forge_repo("swan-lake-common");
        for branch in ["master", "current"] {
            self.f.git(
                &forge_common,
                &["update-ref", &format!("refs/heads/{branch}"), &c2],
            );
        }
        self.f.api.set_pr(
            "swan-lake-common",
            1,
            "merged",
            "refs/pull/1/head",
            &c2,
            Some(&c2),
        );
        self.sync_mirror();
        c2
    }

    /// A bump of the consumer's master to a new commit of common's master
    /// and `current`, as `swl bump` makes one; the new master.
    fn bump_consumer_master(&self, value: &str) -> String {
        let c = self.f.commit_on_forge("swan-lake-common", "master", value);
        let forge_common = self.f.forge_repo("swan-lake-common");
        self.f
            .git(&forge_common, &["update-ref", "refs/heads/current", &c]);
        self.sync_mirror();
        let bumper = self
            .f
            .clone_elsewhere("swan-lake", &format!("bumper-{value}"));
        stdout(&self.f.nix(&bumper, &["flake", "update", "lib"]));
        self.f.git(
            &bumper,
            &["commit", "-qam", &format!("swl: bump common to {value}")],
        );
        self.f
            .git(&bumper, &["push", "-q", "origin", "HEAD:master"]);
        self.forge_head("swan-lake", "master")
    }
}

#[test]
fn refresh_makes_a_partial_landing_anew_with_the_same_entries() {
    let t = Change::new("refresh-partial");
    // A second entry, whose MR stays open: the landing is partial.
    let zw = t.forge_head("zwanenzang", "master");
    t.f.api.set_pr("zwanenzang", 5, "open", "zw-5", &zw, None);
    let manifest = "[inputs.lib]\nrepo = \"swan-lake-common\"\nmr = 1\n\n\
                    [inputs.\"lib/zwanenzang\"]\nrepo = \"zwanenzang\"\nmr = 5\n";
    fs::write(t.consumer.join("swl.toml"), manifest).unwrap();
    t.f.git(
        &t.consumer,
        &["commit", "-qam", "swl: manifest for zwanenzang#5"],
    );
    t.merge_common_by_hand();
    let consumer = t.consumer.display().to_string();
    stdout(&t.f.swl(&t.f.root, &["finalize", &consumer]));
    t.f.git(&t.consumer, &["push", "-q", "origin", "sl-0020-land"]);
    let landing = t.f.git(&t.consumer, &["rev-parse", "HEAD"]);
    let partial = t.f.git(&t.consumer, &["show", "HEAD:swl.toml"]);
    assert!(partial.contains("zwanenzang"), "{partial}");

    let m2 = t.bump_consumer_master("c3");
    let out = t.refresh(&[]);
    let log = stdout(&out);
    assert!(
        log.contains(&format!(
            "(3 commits, without swl's landing commit {}, as origin/master moved flake.lock)",
            &landing[..12]
        )),
        "{log}"
    );
    assert!(
        log.contains("swan-lake#2: finalized anew: swl: land swan-lake-common#1 (lib)"),
        "{log}"
    );
    let head = t.forge_head("swan-lake", "sl-0020-land");
    assert_eq!(t.f.git(&t.consumer, &["rev-parse", "HEAD"]), head);
    assert_eq!(t.f.git(&t.consumer, &["rev-parse", "HEAD~4"]), m2);
    assert_eq!(t.f.git(&t.consumer, &["show", "HEAD:swl.toml"]), partial);
    let source = t.f.source("swan-lake");
    assert!(
        t.f.git(&source, &["branch", "--list", "swl-refresh-*"])
            .is_empty()
    );
    assert_eq!(
        t.worktrees("swan-lake").len(),
        2,
        "the scratch worktree is gone"
    );
}

#[test]
fn refresh_keeps_a_commit_that_only_looks_like_a_landing() {
    let t = Change::new("refresh-lookalike");
    t.merge_common_by_hand();
    // swl's subject and files, but its lock gains an input besides lib.
    let lock_path = t.consumer.join("flake.lock");
    let mut lock: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&lock_path).unwrap()).unwrap();
    let path = serde_json::json!({"type": "path", "path": "/x"});
    lock["nodes"]["x"] = serde_json::json!({"flake": false, "locked": path, "original": path});
    lock["nodes"]["root"]["inputs"]["x"] = "x".into();
    fs::write(
        &lock_path,
        serde_json::to_string_pretty(&lock).unwrap() + "\n",
    )
    .unwrap();
    t.f.git(&t.consumer, &["rm", "-q", "swl.toml"]);
    t.f.git(
        &t.consumer,
        &["commit", "-qam", "swl: land swan-lake-common#1 (lib)"],
    );
    t.f.git(&t.consumer, &["push", "-q", "origin", "sl-0020-land"]);
    let lookalike = t.f.git(&t.consumer, &["rev-parse", "HEAD"]);
    let m2 = t.bump_consumer_master("c3");
    // Not dropped: rebased as any commit (here without a conflict), its
    // lock change kept.
    let log = stdout(&t.refresh(&[]));
    assert!(
        log.contains(&format!(
            "rebase sl-0020-land of swan-lake#2 (3 commits) onto origin/master at {}, and push it \
             with --force-with-lease=refs/heads/sl-0020-land:{}",
            &m2[..12],
            &lookalike[..12]
        )),
        "{log}"
    );
    assert!(!log.contains("finalized anew"), "{log}");
    let head = t.forge_head("swan-lake", "sl-0020-land");
    assert_eq!(
        subjects(&t, "swan-lake", "sl-0020-land")[0],
        "swl: land swan-lake-common#1 (lib)"
    );
    let rebased_lock =
        t.f.git(&t.consumer, &["show", &format!("{head}:flake.lock")]);
    assert!(rebased_lock.contains("\"/x\""), "{rebased_lock}");
}

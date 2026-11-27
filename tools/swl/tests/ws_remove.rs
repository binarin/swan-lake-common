//! `swl ws remove` on a disposable forge: a pushed workspace goes with its
//! branches; one with uncommitted, untracked or unpushed work is kept
//! unless `--force`.

mod support;

use std::{fs, os::unix::fs::symlink, path::PathBuf};

use support::{Forge, failed, stdout, text};

struct Ws {
    f: Forge,
    work: PathBuf,
    ws: PathBuf,
    consumer: PathBuf,
    common: PathBuf,
}

/// The workspace SL-0005-rm with swan-lake and swan-lake-common@lib on
/// sl-0005-rm, a commit of common's pushed.
fn workspace(name: &str) -> Ws {
    let f = Forge::new(name);
    let work = f.root.join("work");
    fs::create_dir_all(&work).unwrap();
    stdout(&f.swl(
        &work,
        &["start", "SL-0005-rm", "swan-lake", "swan-lake-common@lib"],
    ));
    let ws = work.join("SL-0005-rm");
    let t = Ws {
        consumer: ws.join("swan-lake"),
        common: ws.join("swan-lake-common@lib"),
        ws,
        work,
        f,
    };
    fs::write(t.common.join("value.txt"), "c2").unwrap();
    t.f.git(&t.common, &["commit", "-qam", "c2"]);
    t.f.git(&t.common, &["push", "-q", "origin", "sl-0005-rm"]);
    t
}

impl Ws {
    fn branches(&self, repo: &str) -> String {
        self.f
            .git(&self.f.source(repo), &["branch", "--list", "sl-0005-rm"])
    }

    fn worktrees(&self, repo: &str) -> String {
        self.f.git(&self.f.source(repo), &["worktree", "list"])
    }
}

#[test]
fn ws_remove_removes_a_pushed_workspace_from_inside_it() {
    let t = workspace("remove-pushed");
    assert!(t.consumer.join(".envrc.local").is_file());
    // Ignored files do not keep it: they are deleted, and listed but for
    // disposable ones.
    let exclude = t.f.source("swan-lake-common").join("info/exclude");
    fs::create_dir_all(exclude.parent().unwrap()).unwrap();
    fs::write(&exclude, "build/\n.direnv/\n").unwrap();
    fs::create_dir_all(t.common.join("build")).unwrap();
    fs::write(t.common.join("build/out"), "x").unwrap();
    fs::create_dir_all(t.common.join(".direnv")).unwrap();
    // From inside the consumer checkout, which goes first.
    let out = stdout(&t.f.swl(&t.consumer, &["ws", "remove"]));
    let ws = t.ws.display();
    for (repo, dir) in [("swan-lake", &t.consumer), ("swan-lake-common", &t.common)] {
        assert!(
            out.contains(&format!(
                "├─ removed {}\n├─ deleted the branch sl-0005-rm of {repo}: its commits are on \
                 remote branches",
                dir.display()
            )),
            "{out}"
        );
        assert!(t.branches(repo).is_empty(), "{out}");
        assert!(!t.worktrees(repo).contains("SL-0005-rm"), "{out}");
    }
    assert!(
        out.contains(&format!(
            "├─ deleting the ignored files of swan-lake-common@lib: build/\n├─ removed {}\n",
            t.common.display()
        )),
        "{out}"
    );
    assert_eq!(out.matches("ignored files").count(), 1, "{out}");
    assert!(
        out.starts_with(&format!("swl ws remove: workspace {ws}\n")),
        "{out}"
    );
    assert!(out.contains(&format!("└─ removed {ws}\n")), "{out}");
    assert!(
        out.ends_with(&format!(
            "swl ws remove: the current directory was in {ws}; cd out of it"
        )),
        "{out}"
    );
    assert!(!t.ws.exists());
    // The pushed branch is still on the forge.
    t.f.git(
        &t.f.forge_repo("swan-lake-common"),
        &["rev-parse", "--verify", "sl-0005-rm"],
    );
    // Joined afresh, nothing stands in the way.
    let out = stdout(&t.f.swl(
        &t.work,
        &["start", "SL-0005-rm", "swan-lake", "swan-lake-common@lib"],
    ));
    assert!(!out.contains("pruned"), "{out}");
    assert!(out.contains("tracking origin/sl-0005-rm"), "{out}");
}

#[test]
fn ws_remove_keeps_dirty_or_unpushed_work_unless_forced() {
    let t = workspace("remove-held");
    let remove = |extra: &[&str]| {
        let mut args = vec!["ws", "remove", "sl-5"];
        args.extend(extra);
        t.f.swl(&t.f.root, &args)
    };
    // Untracked in the consumer, also where git status hides untracked
    // files; an unpushed commit in common.
    t.f.git(
        &t.f.source("swan-lake"),
        &["config", "status.showUntrackedFiles", "no"],
    );
    fs::write(t.consumer.join("notes.txt"), "wip").unwrap();
    fs::write(t.common.join("value.txt"), "c3").unwrap();
    t.f.git(&t.common, &["commit", "-qam", "c3 unpushed"]);
    let c3 = t.f.git(&t.common, &["rev-parse", "--short", "HEAD"]);
    let out = remove(&[]);
    failed(&out, 2, "holds work that is not committed or pushed");
    failed(&out, 2, "pass --force");
    let log = String::from_utf8_lossy(&out.stdout);
    assert!(
        log.contains("├─ keeping swan-lake: uncommitted or untracked files: ?? notes.txt\n"),
        "{}",
        text(&out)
    );
    assert!(
        log.contains(&format!(
            "├─ keeping swan-lake-common@lib: commits on no remote branch:\n│  {c3} c3 unpushed\n"
        )),
        "{}",
        text(&out)
    );
    assert!(log.contains("└─ kept the workspace"), "{}", text(&out));
    assert!(t.consumer.join("notes.txt").is_file() && t.common.exists());
    assert!(!t.branches("swan-lake").is_empty());

    // Only the untracked file: still kept. A modified tracked file too.
    t.f.git(&t.common, &["push", "-q", "origin", "sl-0005-rm"]);
    failed(&remove(&[]), 2, "?? notes.txt");
    fs::remove_file(t.consumer.join("notes.txt")).unwrap();
    fs::write(t.consumer.join("value.txt"), "changed").unwrap();
    failed(&remove(&[]), 2, " M value.txt");
    assert!(t.consumer.exists());

    // --force removes them; a branch with unpushed commits is kept.
    fs::write(t.common.join("value.txt"), "c4").unwrap();
    t.f.git(&t.common, &["commit", "-qam", "c4 unpushed"]);
    let c4 = t.f.git(&t.common, &["rev-parse", "--short", "HEAD"]);
    let out = stdout(&remove(&["--force"]));
    assert!(
        out.contains(
            "├─ removing swan-lake anyway (--force): uncommitted or untracked files:  M value.txt\n"
        ),
        "{out}"
    );
    assert!(
        out.contains(&format!(
            "├─ kept the branch sl-0005-rm of swan-lake-common, which has commits on no remote \
             branch:\n│  {c4} c4 unpushed\n│  git -C {} branch -D sl-0005-rm drops them",
            t.f.source("swan-lake-common").display()
        )),
        "{out}"
    );
    assert!(!t.ws.exists(), "{out}");
    assert!(t.branches("swan-lake").is_empty());
    assert!(!t.branches("swan-lake-common").is_empty());
    for repo in ["swan-lake", "swan-lake-common"] {
        assert!(!t.worktrees(repo).contains("SL-0005-rm"), "{out}");
    }
}

#[test]
fn ws_remove_refuses_what_is_not_a_workspace() {
    let t = workspace("remove-not-ws");
    let plain = t.f.root.join("plain");
    fs::create_dir_all(&plain).unwrap();
    let out =
        t.f.swl(&t.f.root, &["ws", "remove", "--workspace", "./plain"]);
    failed(
        &out,
        2,
        "is not a workspace: it has no checkouts and no .swl",
    );
    assert!(plain.exists());

    // A checkout that is not a worktree of its source: nothing is removed.
    let clone =
        t.f.clone_elsewhere("zwanenzang", "work/SL-0005-rm/zwanenzang@zw");
    let out = t.f.swl(&t.f.root, &["ws", "remove", "sl-5"]);
    failed(
        &out,
        2,
        &format!("{} is not a worktree of", clone.display()),
    );
    assert!(t.consumer.exists() && t.common.exists() && clone.exists());
}

#[test]
fn ws_remove_keeps_a_detached_checkouts_unpushed_commits_on_a_branch_when_forced() {
    let t = workspace("remove-detached");
    stdout(&t.f.swl(&t.ws, &["ws", "add", "zwanenzang@zw", "--no-env"]));
    let zw = t.ws.join("zwanenzang@zw");
    fs::write(zw.join("value.txt"), "z2").unwrap();
    t.f.git(&zw, &["commit", "-qam", "z2 detached"]);
    let z2 = t.f.git(&zw, &["rev-parse", "HEAD"]);
    let z2_short = t.f.git(&zw, &["rev-parse", "--short", "HEAD"]);
    let out = t.f.swl(&t.f.root, &["ws", "remove", "sl-5"]);
    failed(
        &out,
        2,
        &format!(
            "├─ keeping zwanenzang@zw: commits on no remote branch:\n│  {z2_short} z2 detached\n"
        ),
    );
    assert!(zw.exists() && t.common.exists() && t.consumer.exists());

    let out = stdout(&t.f.swl(&t.f.root, &["ws", "remove", "sl-5", "--force"]));
    let source = t.f.source("zwanenzang");
    assert!(
        out.contains(&format!(
            "├─ kept the commits of zwanenzang@zw, detached at {z2}, on the branch \
             swl-kept/SL-0005-rm-zwanenzang@zw of zwanenzang:\n│  {z2_short} z2 detached\n│  \
             git -C {} branch -D swl-kept/SL-0005-rm-zwanenzang@zw drops them\n├─ removed {}\n",
            source.display(),
            zw.display()
        )),
        "{out}"
    );
    assert_eq!(
        t.f.git(
            &source,
            &["rev-parse", "refs/heads/swl-kept/SL-0005-rm-zwanenzang@zw"]
        ),
        z2
    );
    assert!(!t.ws.exists(), "{out}");
}

#[test]
fn ws_remove_stops_at_a_checkout_that_changed_after_it_was_checked() {
    let t = workspace("remove-race");
    // Deleting swan-lake's branch, after its checkout is removed and before
    // common's is, commits in common.
    let hook = t.f.source("swan-lake").join("hooks/reference-transaction");
    fs::create_dir_all(hook.parent().unwrap()).unwrap();
    fs::write(
        &hook,
        format!(
            "#!/bin/sh\n[ \"$1\" = committed ] || exit 0\n\
             grep -q ' refs/heads/sl-0005-rm$' || exit 0\n\
             unset GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE\n\
             git -C {} -c user.name=t -c user.email=t@example.org commit -q --allow-empty -m race\n",
            t.common.display()
        ),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
    let out = t.f.swl(&t.f.root, &["ws", "remove", "sl-5"]);
    failed(
        &out,
        2,
        &format!(
            "{} changed while swl ws remove ran (its HEAD, branch or files); it was kept",
            t.common.display()
        ),
    );
    failed(
        &out,
        2,
        "└─ stopped at swan-lake-common@lib: what is not said removed above is kept",
    );
    assert!(!t.consumer.exists());
    assert_eq!(t.f.git(&t.common, &["log", "-1", "--format=%s"]), "race");
    assert!(!t.branches("swan-lake-common").is_empty());
}

#[test]
fn ws_remove_refuses_a_locked_checkout() {
    let t = workspace("remove-locked");
    let source = t.f.source("swan-lake-common");
    t.f.git(
        &source,
        &["worktree", "lock", &t.common.display().to_string()],
    );
    let out = t.f.swl(&t.f.root, &["ws", "remove", "sl-5", "--force"]);
    failed(
        &out,
        2,
        &format!(
            "{} is locked; nothing was removed (git -C {} worktree unlock {0})",
            t.common.display(),
            source.display()
        ),
    );
    assert!(t.consumer.exists() && t.common.exists());
    assert!(!t.branches("swan-lake").is_empty());
}

#[test]
fn ws_remove_deletes_a_branch_rebased_and_force_pushed_elsewhere() {
    let t = workspace("remove-rebased");
    // Elsewhere: master moves on, the branch is rebased onto it and
    // force-pushed. The local c2 is on no remote branch, but its change is.
    t.f.commit_file_on_forge("swan-lake-common", "master", "other.txt", "o");
    let elsewhere = t.f.clone_elsewhere("swan-lake-common", "rebaser");
    t.f.git(&elsewhere, &["switch", "-q", "sl-0005-rm"]);
    t.f.git(&elsewhere, &["rebase", "-q", "origin/master"]);
    t.f.git(&elsewhere, &["push", "-qf", "origin", "sl-0005-rm"]);
    let out = stdout(&t.f.swl(&t.f.root, &["ws", "remove", "sl-5"]));
    assert!(
        out.contains("├─ deleted the branch sl-0005-rm of swan-lake-common: its commits are on remote branches"),
        "{out}"
    );
    assert!(t.branches("swan-lake-common").is_empty());
    assert!(!t.ws.exists());
}

#[test]
fn ws_remove_no_fetch_judges_by_the_refs_already_known() {
    let t = workspace("remove-no-fetch");
    // The forge no longer has the branch; the source still has its ref.
    t.f.git(
        &t.f.forge_repo("swan-lake-common"),
        &["branch", "-qD", "sl-0005-rm"],
    );
    let c2 = t.f.git(&t.common, &["rev-parse", "--short", "HEAD"]);
    fs::write(t.consumer.join("notes.txt"), "wip").unwrap();
    let out = t.f.swl(&t.f.root, &["ws", "remove", "sl-5", "--no-fetch"]);
    failed(
        &out,
        2,
        "├─ keeping swan-lake: uncommitted or untracked files: ?? notes.txt",
    );
    assert!(
        !text(&out).contains("keeping swan-lake-common@lib"),
        "{}",
        text(&out)
    );
    // Fetched (and pruned), c2 is pushed nowhere.
    let out = t.f.swl(&t.f.root, &["ws", "remove", "sl-5"]);
    failed(
        &out,
        2,
        &format!("├─ keeping swan-lake-common@lib: commits on no remote branch:\n│  {c2} c2\n"),
    );
    assert!(t.consumer.exists() && t.common.exists());
}

#[test]
fn ws_remove_refuses_a_symlinked_checkout_and_a_main_working_tree() {
    let t = workspace("remove-links");
    // A link to a worktree elsewhere: refused before anything is removed.
    let elsewhere = t.f.root.join("elsewhere-wt");
    let source = t.f.source("swan-lake-common");
    t.f.git(
        &source,
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            &elsewhere.display().to_string(),
            "origin/master",
        ],
    );
    let link = t.ws.join("swan-lake-common@other");
    symlink(&elsewhere, &link).unwrap();
    let out = t.f.swl(&t.f.root, &["ws", "remove", "sl-5", "--force"]);
    failed(
        &out,
        2,
        &format!("{} is a symbolic link; nothing was removed", link.display()),
    );
    assert!(t.consumer.join(".envrc.local").is_file());
    assert!(t.consumer.exists() && t.common.exists() && elsewhere.exists());

    // A source that is a main checkout, scanned as a workspace: its main
    // working tree is refused, before its .envrc.local or a linked worktree
    // beside it is removed.
    let plain = t.f.root.join("plain");
    let main = t.f.clone_elsewhere("swan-lake", "plain/swan-lake");
    let config = t.f.root.join("repos.toml");
    let text_ = fs::read_to_string(&config).unwrap().replace(
        &t.f.source("swan-lake").display().to_string(),
        &main.display().to_string(),
    );
    fs::write(&config, text_).unwrap();
    let envrc = main.join(".envrc.local");
    fs::write(&envrc, "# Generated by `swl env` for the workspace x\n").unwrap();
    let linked = plain.join("swan-lake-common@lib");
    t.f.git(
        &source,
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            &linked.display().to_string(),
            "origin/master",
        ],
    );
    let out = t.f.swl(
        &t.f.root,
        &["ws", "remove", "--workspace", "./plain", "--force"],
    );
    failed(
        &out,
        2,
        &format!(
            "{} is the main working tree of {}, not a worktree of a workspace; nothing was removed",
            main.display(),
            main.display()
        ),
    );
    assert!(envrc.is_file() && linked.join("flake.nix").is_file());
}

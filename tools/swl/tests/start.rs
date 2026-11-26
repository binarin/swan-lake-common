//! `swl start` on a disposable forge: a new workspace for a change, rerun
//! and resumed, and joining a published consumer MR with `--from`.

mod support;

use std::fs;

use support::{Forge, failed, stdout};

#[test]
fn start_creates_a_workspace_and_is_rerunnable() {
    let f = Forge::new("start-new");
    let work = f.root.join("work");
    fs::create_dir_all(&work).unwrap();
    let start = [
        "start",
        "SL-0001-Foo",
        "swan-lake",
        "swan-lake-common@lib",
        "zwanenzang@zwanenzang",
    ];
    let ws = work.join("SL-0001-Foo");
    let consumer = ws.join("swan-lake");
    let common = ws.join("swan-lake-common@lib");
    let zw = ws.join("zwanenzang@zwanenzang");

    // Interrupted: zwanenzang's source cannot be fetched, after the first two
    // checkouts are added.
    let zw_source = f.source("zwanenzang");
    let zw_url = f.forge_repo("zwanenzang").display().to_string();
    // Nothing is written to $SWL_CD_FILE.
    let cd_file = f.root.join("cd-file");
    fs::write(&cd_file, "").unwrap();
    let cd_env = [("SWL_CD_FILE", cd_file.to_str().unwrap())];
    f.git(&zw_source, &["remote", "set-url", "origin", "/nonexistent"]);
    failed(&f.swl_env(&work, &start, &cd_env), 2, "pass --no-fetch");
    assert!(consumer.join("flake.nix").is_file() && common.join("flake.nix").is_file());
    assert!(!zw.exists());
    assert!(!consumer.join(".envrc.local").exists());
    assert_eq!(fs::read_to_string(&cd_file).unwrap(), "");

    // Rerun after the fix: the two are kept, the third added, then env and
    // direnv allow. The consumer that the cd hint names is in $SWL_CD_FILE.
    f.git(&zw_source, &["remote", "set-url", "origin", &zw_url]);
    let out = stdout(&f.swl_env(&work, &start, &cd_env));
    assert_eq!(
        fs::read_to_string(&cd_file).unwrap(),
        consumer.display().to_string()
    );
    for dir in [&consumer, &common] {
        assert!(
            out.contains(&format!("{}: kept, on sl-0001-foo", dir.display())),
            "{out}"
        );
    }
    assert!(
        out.contains("new branch sl-0001-foo at origin/master"),
        "{out}"
    );
    for dir in [&consumer, &common, &zw] {
        assert_eq!(
            f.git(dir, &["symbolic-ref", "--short", "HEAD"]),
            "sl-0001-foo"
        );
    }
    let envrc = fs::read_to_string(consumer.join(".envrc.local")).unwrap();
    assert!(envrc.contains("--override-input lib "), "{envrc}");
    assert!(
        envrc.contains("--override-input lib/zwanenzang "),
        "{envrc}"
    );
    assert_eq!(f.direnv_calls(), format!("allow {}\n", consumer.display()));
    assert!(
        out.contains(&format!("workspace {}:", ws.display())),
        "{out}"
    );
    assert!(out.contains(&format!("cd {}", consumer.display())), "{out}");

    // A rerun changes nothing; a checkout on another branch is reported.
    f.git(&common, &["checkout", "-qb", "other"]);
    let heads: Vec<String> = [&consumer, &common, &zw]
        .iter()
        .map(|d| f.git(d, &["rev-parse", "HEAD"]))
        .collect();
    let out = stdout(&f.swl(&work, &start));
    assert!(
        out.contains("kept, on other, not sl-0001-foo; not changed"),
        "{out}"
    );
    assert!(out.contains("swan-lake: lib←swan-lake-common@lib"), "{out}");
    assert!(out.contains("(unchanged)"), "{out}");
    let after: Vec<String> = [&consumer, &common, &zw]
        .iter()
        .map(|d| f.git(d, &["rev-parse", "HEAD"]))
        .collect();
    assert_eq!(heads, after);
    assert_eq!(
        f.git(&common, &["symbolic-ref", "--short", "HEAD"]),
        "other"
    );

    // Without direnv on PATH: a note instead.
    let out = stdout(&f.swl_path(&work, &start, &f.path_without_direnv(), &[]));
    assert!(
        out.contains("direnv is not on PATH; run `direnv allow`"),
        "{out}"
    );

    // Nothing is added without a consumer, or for an unknown repo.
    let other = ["start", "SL-0002", "swan-lake-common@lib"];
    failed(&f.swl(&work, &other), 2, "at least one consumer");
    let unknown = ["start", "SL-0002", "swan-lake", "unknown@x"];
    failed(&f.swl(&work, &unknown), 2, "not in the swl config");
    assert!(!work.join("SL-0002").exists());

    // An existing directory that is not a worktree of the configured source
    // is not kept.
    let elsewhere = f.clone_elsewhere("swan-lake", "work/SL-0003/swan-lake");
    let start3 = ["start", "SL-0003", "swan-lake"];
    failed(&f.swl(&work, &start3), 2, "is not a worktree of");
    assert!(!elsewhere.join(".envrc.local").exists());
}

/// The consumer branch `branch` on the forge with `manifest`, and its MR `n`.
fn publish_consumer(f: &Forge, branch: &str, manifest: &str, n: u64) -> String {
    let dir = f.clone_elsewhere("swan-lake", &format!("consumer-{branch}"));
    f.git(&dir, &["checkout", "-qb", branch]);
    fs::write(dir.join("value.txt"), "consumer-2").unwrap();
    fs::write(dir.join("swl.toml"), manifest).unwrap();
    f.git(&dir, &["add", "value.txt", "swl.toml"]);
    f.git(&dir, &["commit", "-qm", "consumer: use the MRs"]);
    f.git(&dir, &["push", "-q", "origin", branch]);
    let head = f.git(&dir, &["rev-parse", "HEAD"]);
    f.git(
        &f.forge_repo("swan-lake"),
        &["update-ref", &format!("refs/pull/{n}/head"), &head],
    );
    head
}

#[test]
fn start_from_joins_an_open_and_a_merged_mr() {
    let f = Forge::new("start-from");
    // The consumer repo is private: start --from reads it with git only.
    f.api.make_private("swan-lake");
    let work = f.root.join("work");
    fs::create_dir_all(&work).unwrap();

    // An open common MR from its branch `feature`.
    let feature = f.commit_on_forge("swan-lake-common", "feature", "c2");
    f.git(
        &f.forge_repo("swan-lake-common"),
        &["update-ref", "refs/pull/1/head", &feature],
    );
    f.api
        .set_pr("swan-lake-common", 1, "open", "feature", &feature, None);
    // A merged zwanenzang MR, in a private repo.
    let merged = f.commit_on_forge("zwanenzang", "master", "z2");
    f.api.set_pr(
        "zwanenzang",
        2,
        "merged",
        "refs/pull/2/head",
        &merged,
        Some(&merged),
    );
    f.api.make_private("zwanenzang");
    let head = publish_consumer(
        &f,
        "sl-0003-join",
        "[inputs.lib]\nrepo = \"swan-lake-common\"\nmr = 1\n\n\
         [inputs.\"lib/zwanenzang\"]\nrepo = \"zwanenzang\"\nmr = 2\n",
        5,
    );

    // Without an fj login the private zwanenzang MR cannot be read: refused
    // before anything is added.
    let from = ["start", "--from", "swan-lake#5"];
    failed(&f.swl(&work, &from), 2, "no fj login");
    let ws = work.join("sl-0003-join");
    assert!(!ws.exists());

    let keys = f.login();
    let login = [("SWL_FJ_KEYS", keys.to_str().unwrap())];
    let out = stdout(&f.swl_env(&work, &from, &login));
    assert!(out.contains("swan-lake#5: branch sl-0003-join"), "{out}");
    let consumer = ws.join("swan-lake");
    let common = ws.join("swan-lake-common@lib");
    let zw = ws.join("zwanenzang@zwanenzang");
    assert_eq!(f.git(&consumer, &["rev-parse", "HEAD"]), head);
    assert_eq!(
        f.git(&consumer, &["rev-parse", "--abbrev-ref", "@{u}"]),
        "origin/sl-0003-join"
    );
    assert_eq!(f.git(&common, &["rev-parse", "HEAD"]), feature);
    assert_eq!(
        f.git(&common, &["rev-parse", "--abbrev-ref", "@{u}"]),
        "origin/feature"
    );
    assert_eq!(f.git(&zw, &["rev-parse", "HEAD"]), merged);
    assert!(
        out.contains("is merged; detached at its merge commit"),
        "{out}"
    );
    let envrc = fs::read_to_string(consumer.join(".envrc.local")).unwrap();
    assert!(
        envrc.contains("--override-input lib/zwanenzang "),
        "{envrc}"
    );
    assert_eq!(f.direnv_calls(), format!("allow {}\n", consumer.display()));
    // The token went only in the Authorization header.
    let seen = f.api.seen_auth.lock().unwrap().clone();
    assert!(
        seen.iter()
            .any(|(p, a)| p.contains("/zwanenzang/pulls/2") && a.is_some()),
        "{seen:?}"
    );

    // Rerun: all kept, and the consumer is in $SWL_CD_FILE.
    let cd_file = f.root.join("cd-file");
    let out = stdout(&f.swl_env(
        &work,
        &from,
        &[login[0], ("SWL_CD_FILE", cd_file.to_str().unwrap())],
    ));
    assert_eq!(out.matches(": kept,").count(), 3, "{out}");
    assert!(out.contains(&format!("cd {}", consumer.display())), "{out}");
    assert_eq!(
        fs::read_to_string(&cd_file).unwrap(),
        consumer.display().to_string()
    );

    // An MR whose head is no branch's head any more: refused.
    let master = f.git(&f.forge_repo("swan-lake"), &["rev-parse", "master"]);
    f.git(
        &f.forge_repo("swan-lake"),
        &["update-ref", "refs/pull/6/head", &format!("{master}~1")],
    );
    failed(
        &f.swl_env(&work, &["start", "--from", "swan-lake#6"], &login),
        2,
        "not the head of any branch",
    );
    // Merged by a fast-forward: its head is master's, not a branch to join.
    f.git(
        &f.forge_repo("swan-lake"),
        &["update-ref", "refs/pull/7/head", &master],
    );
    failed(
        &f.swl_env(&work, &["start", "--from", "swan-lake#7"], &login),
        2,
        "is the head of master: the MR is merged",
    );
    assert!(!work.join("master").exists());
    failed(
        &f.swl_env(&work, &["start", "--from", "swan-lake#9"], &login),
        2,
        "no such MR",
    );
}

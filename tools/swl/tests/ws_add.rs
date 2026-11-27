//! `swl ws add` on a disposable forge: it fetches the source first, joins a
//! published branch, fast-forwards a stale local one, and runs `swl env`.

mod support;

use std::fs;

use support::{Forge, failed, stdout};

#[test]
fn ws_add_fetches_the_base_unless_no_fetch() {
    let f = Forge::new("add-fetch");
    let ws = f.root.join("ws");
    let ws_s = ws.display().to_string();
    let new = f.commit_on_forge("swan-lake-common", "master", "c-new");
    let out = stdout(&f.swl(
        &f.root,
        &["ws", "add", "swan-lake-common@lib", "--workspace", &ws_s],
    ));
    assert!(out.contains("detached at origin/master"), "{out}");
    assert!(out.contains("no consumer checkout"), "{out}");
    assert_eq!(
        f.git(&ws.join("swan-lake-common@lib"), &["rev-parse", "HEAD"]),
        new
    );

    // --no-fetch starts from the refs the source already has.
    f.commit_on_forge("swan-lake-common", "master", "c-newer");
    stdout(&f.swl(
        &f.root,
        &[
            "ws",
            "add",
            "swan-lake-common@extra",
            "--workspace",
            &ws_s,
            "--no-fetch",
        ],
    ));
    assert_eq!(
        f.git(&ws.join("swan-lake-common@extra"), &["rev-parse", "HEAD"]),
        new
    );

    // A failed fetch is an error, not a stale base.
    let zw = f.source("zwanenzang");
    f.git(
        &zw,
        &["remote", "set-url", "origin", "/nonexistent/zwanenzang"],
    );
    let add_zw = ["ws", "add", "zwanenzang@zwanenzang", "--workspace", &ws_s];
    failed(&f.swl(&f.root, &add_zw), 2, "pass --no-fetch");
    assert!(!ws.join("zwanenzang@zwanenzang").exists());
    let mut offline = add_zw.to_vec();
    offline.push("--no-fetch");
    stdout(&f.swl(&f.root, &offline));
    assert!(ws.join("zwanenzang@zwanenzang/flake.nix").is_file());
}

#[test]
fn ws_add_joins_a_published_branch_and_fast_forwards_a_stale_one() {
    let f = Forge::new("add-branch");
    let ws = f.root.join("ws");
    let ws_s = ws.display().to_string();
    let checkout = ws.join("swan-lake-common@lib");
    let source = f.source("swan-lake-common");
    let add = [
        "ws",
        "add",
        "swan-lake-common@lib",
        "--workspace",
        &ws_s,
        "--branch",
        "feature",
        "--no-env",
    ];
    let remove = |f: &Forge| {
        f.git(
            &source,
            &["worktree", "remove", &checkout.display().to_string()],
        )
    };

    // Only on the forge: a local branch tracking it.
    let published = f.commit_on_forge("swan-lake-common", "feature", "c2");
    let out = stdout(&f.swl(&f.root, &add));
    assert!(
        out.contains("new branch feature tracking origin/feature"),
        "{out}"
    );
    assert_eq!(f.git(&checkout, &["rev-parse", "HEAD"]), published);
    assert_eq!(
        f.git(&checkout, &["rev-parse", "--abbrev-ref", "@{u}"]),
        "origin/feature"
    );

    // The local branch is behind the forge's: fast-forwarded.
    remove(&f);
    let newer = f.commit_on_forge("swan-lake-common", "feature", "c3");
    let out = stdout(&f.swl(&f.root, &add));
    assert!(out.contains("fast-forwarded to origin/feature"), "{out}");
    assert_eq!(f.git(&checkout, &["rev-parse", "HEAD"]), newer);

    // Diverged: left as it is, with a warning.
    fs::write(checkout.join("value.txt"), "mine").unwrap();
    f.git(&checkout, &["commit", "-qam", "mine"]);
    let mine = f.git(&checkout, &["rev-parse", "HEAD"]);
    remove(&f);
    f.commit_on_forge("swan-lake-common", "feature", "theirs");
    let out = f.swl(&f.root, &add);
    let log = stdout(&out);
    assert!(log.contains("diverged from origin/feature"), "{log}");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("have diverged"),
        "{}",
        support::text(&out)
    );
    assert_eq!(f.git(&checkout, &["rev-parse", "HEAD"]), mine);

    // A branch checked out in another worktree is refused; nothing is added.
    let other = ws.join("swan-lake-common@other");
    let out = f.swl(
        &f.root,
        &[
            "ws",
            "add",
            "swan-lake-common@other",
            "--workspace",
            &ws_s,
            "--branch",
            "feature",
            "--no-env",
        ],
    );
    failed(
        &out,
        2,
        &format!(
            "branch feature is already checked out at {}",
            checkout.display()
        ),
    );
    assert!(!other.exists());
    assert_eq!(f.git(&checkout, &["rev-parse", "HEAD"]), mine);

    // A branch that is nowhere yet starts at the base, without an upstream.
    let out = stdout(&f.swl(
        &f.root,
        &[
            "ws",
            "add",
            "swan-lake-common@extra",
            "--workspace",
            &ws_s,
            "--branch",
            "fresh",
            "--no-env",
        ],
    ));
    assert!(out.contains("new branch fresh at origin/master"), "{out}");
    let fresh = ws.join("swan-lake-common@extra");
    let mut c = std::process::Command::new("git");
    f.env(&mut c, &f.base_path);
    let upstream = c
        .arg("-C")
        .arg(&fresh)
        .args(["rev-parse", "--abbrev-ref", "@{u}"])
        .output()
        .unwrap();
    assert!(!upstream.status.success());
}

#[test]
fn ws_add_runs_env_and_reports_a_refusal() {
    let f = Forge::new("add-env");
    let ws = f.root.join("ws");
    let ws_s = ws.display().to_string();
    let add = |name: &str, extra: &[&str]| {
        let mut args = vec!["ws", "add", name, "--workspace", &ws_s];
        args.extend(extra);
        f.swl(&f.root, &args)
    };
    let out = stdout(&add("swan-lake-common@lib", &[]));
    assert!(out.contains("swl env runs once one is added"), "{out}");
    assert!(!ws.join(".swl/bin/nix").exists());
    let out = stdout(&add("swan-lake", &[]));
    assert!(out.contains("swan-lake: lib←swan-lake-common@lib"), "{out}");
    let envrc = fs::read_to_string(ws.join("swan-lake/.envrc.local")).unwrap();
    assert!(envrc.contains("--override-input lib "), "{envrc}");
    assert!(ws.join(".swl/bin/nix").is_file());

    // A hand-written .envrc.local: the checkout is added, env refuses.
    fs::write(ws.join("swan-lake/.envrc.local"), "# mine\n").unwrap();
    let out = add("zwanenzang@zwanenzang", &[]);
    failed(&out, 2, "added, but swl env failed");
    failed(&out, 2, "swl env --force");
    assert!(ws.join("zwanenzang@zwanenzang/flake.nix").is_file());
    assert_eq!(
        fs::read_to_string(ws.join("swan-lake/.envrc.local")).unwrap(),
        "# mine\n"
    );
    // --no-env skips it.
    let out = stdout(&add("swan-lake-common@extra", &["--no-env"]));
    assert!(out.contains("run `swl env`"), "{out}");
}

#[test]
fn ws_add_prunes_worktrees_removed_by_hand_and_refuses_a_locked_one() {
    let f = Forge::new("add-stale");
    let ws = f.root.join("ws");
    let ws_s = ws.display().to_string();
    let source = f.source("swan-lake-common");
    let add = |name: &str, extra: &[&str]| {
        let mut args = vec!["ws", "add", name, "--workspace", &ws_s, "--no-env"];
        args.extend(extra);
        f.swl(&f.root, &args)
    };
    let feature = f.commit_on_forge("swan-lake-common", "feature", "c2");
    let lib = ws.join("swan-lake-common@lib");
    stdout(&add("swan-lake-common@lib", &["--branch", "feature"]));

    // Removed by hand: its registration holds the branch until pruned.
    fs::remove_dir_all(&lib).unwrap();
    let other = ws.join("swan-lake-common@other");
    let out = stdout(&add("swan-lake-common@other", &["--branch", "feature"]));
    assert!(
        out.contains(&format!(
            "swl ws add: pruned the worktree {} (branch feature at {feature}) of {}, whose \
             directory is gone (git worktree remove)",
            lib.display(),
            source.display()
        )),
        "{out}"
    );
    assert!(out.contains("existing branch feature"), "{out}");
    assert_eq!(f.git(&other, &["rev-parse", "HEAD"]), feature);
    let list = f.git(&source, &["worktree", "list"]);
    assert!(!list.contains(&lib.display().to_string()), "{list}");

    // Added again where one was removed by hand, detached: its path is
    // registered until pruned.
    let extra = ws.join("swan-lake-common@extra");
    stdout(&add("swan-lake-common@extra", &[]));
    let extra_head = f.git(&extra, &["rev-parse", "HEAD"]);
    fs::remove_dir_all(&extra).unwrap();
    let out = stdout(&add("swan-lake-common@extra", &[]));
    assert!(
        out.contains(&format!(
            "swl ws add: pruned the worktree {} (detached at {extra_head}) of {}, whose \
             directory is gone (git worktree remove)\n",
            extra.display(),
            source.display()
        )),
        "{out}"
    );
    assert!(extra.join("flake.nix").is_file());

    // No prune when nothing stands in the way.
    let out = stdout(&add("swan-lake-common@lib", &[]));
    assert!(!out.contains("pruned"), "{out}");

    // A locked worktree whose directory is gone still refuses.
    f.git(&source, &["worktree", "lock", &other.display().to_string()]);
    fs::remove_dir_all(&other).unwrap();
    let out = add("swan-lake-common@third", &["--branch", "feature"]);
    failed(
        &out,
        2,
        &format!(
            "branch feature is already checked out at {} (locked",
            other.display()
        ),
    );
    assert!(!ws.join("swan-lake-common@third").exists());
    assert!(!support::text(&out).contains("pruned"));
}

#[test]
fn ws_add_prunes_only_the_registration_in_the_way_and_refuses_a_locked_one_at_the_checkout() {
    let f = Forge::new("add-stale-only");
    let ws = f.root.join("ws");
    let ws_s = ws.display().to_string();
    let source = f.source("swan-lake-common");
    let add = |name: &str| {
        f.swl(
            &f.root,
            &["ws", "add", name, "--workspace", &ws_s, "--no-env"],
        )
    };
    let a = ws.join("swan-lake-common@a");
    let b = ws.join("swan-lake-common@b");
    stdout(&add("swan-lake-common@a"));
    stdout(&add("swan-lake-common@b"));
    let a_head = f.git(&a, &["rev-parse", "HEAD"]);
    fs::remove_dir_all(&a).unwrap();
    fs::remove_dir_all(&b).unwrap();

    // Only a's registration is in the way: b's (moved, or on media not
    // mounted, for all swl knows) stays.
    let out = stdout(&add("swan-lake-common@a"));
    let pruned: Vec<&str> = out.lines().filter(|l| l.contains("pruned")).collect();
    assert_eq!(
        pruned,
        [format!(
            "swl ws add: pruned the worktree {} (detached at {a_head}) of {}, whose directory \
             is gone (git worktree remove)",
            a.display(),
            source.display()
        )],
        "{out}"
    );
    let list = f.git(&source, &["worktree", "list", "--porcelain"]);
    assert!(
        list.contains(&format!("worktree {}\n", b.display())),
        "{list}"
    );
    assert!(a.join("flake.nix").is_file());

    // A locked registration at the checkout's directory, gone, refuses.
    let c = ws.join("swan-lake-common@c");
    stdout(&add("swan-lake-common@c"));
    f.git(&source, &["worktree", "lock", &c.display().to_string()]);
    fs::remove_dir_all(&c).unwrap();
    let out = add("swan-lake-common@c");
    failed(&out, 2, "is a missing but locked worktree");
    assert!(!c.exists());
    assert!(!support::text(&out).contains("pruned"));
    let list = f.git(&source, &["worktree", "list", "--porcelain"]);
    assert!(
        list.contains(&format!("worktree {}\n", c.display())),
        "{list}"
    );
}

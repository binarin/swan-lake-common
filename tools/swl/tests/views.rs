//! `swl views` on a disposable forge: a view behind its base is
//! fast-forwarded; one with uncommitted changes, one on another branch and
//! one with commits of its own are skipped with a warning and left as they
//! are; a detached view, a missing one, one that is no checkout, and
//! `--no-fetch`.

mod support;

use std::{fs, path::Path};

use support::{Forge, stdout};

const REPOS: [&str; 4] = [
    "nixos-config",
    "swan-lake",
    "swan-lake-common",
    "zwanenzang",
];

fn head(f: &Forge, dir: &Path) -> String {
    f.git(dir, &["rev-parse", "HEAD"])
}

fn short(rev: &str) -> &str {
    &rev[..12]
}

#[test]
fn views_fast_forward_only_a_clean_master_behind_its_base() {
    let f = Forge::new("views");
    f.add_consumer("nixos-config");
    let views: Vec<_> = REPOS.iter().map(|r| f.add_view(r)).collect();
    let [ahead, other_branch, dirty, behind] = &views[..] else {
        unreachable!()
    };
    let old = head(&f, behind);
    for repo in REPOS {
        f.commit_on_forge(repo, "master", &format!("{repo}-2"));
    }
    let new = f.git(&f.forge_repo("zwanenzang"), &["rev-parse", "master"]);
    fs::write(dirty.join("value.txt"), "edited").unwrap();
    f.git(other_branch, &["switch", "-q", "-c", "topic"]);
    fs::write(ahead.join("value.txt"), "local").unwrap();
    f.git(ahead, &["commit", "-qam", "local"]);
    let before: Vec<_> = views.iter().map(|v| head(&f, v)).collect();

    let log = stdout(&f.swl(&f.root, &["views"]));
    for want in [
        format!(
            "swl views: nixos-config\n└─ skipped {}: master has 1 commit(s) not on origin/master \
             (fast-forward only)\n",
            ahead.display()
        ),
        format!(
            "swl views: swan-lake\n└─ skipped {}: it is on the branch topic, not master\n",
            other_branch.display()
        ),
        format!(
            "swl views: swan-lake-common\n└─ skipped {}: it has uncommitted changes\n",
            dirty.display()
        ),
        format!(
            "swl views: zwanenzang\n└─ fast-forwarded {} from {} to {} (origin/master)",
            behind.display(),
            short(&old),
            short(&new)
        ),
    ] {
        assert!(log.contains(&want), "{want:?} in\n{log}");
    }
    assert_eq!(head(&f, behind), new);
    for (view, was) in views[..3].iter().zip(&before) {
        assert_eq!(&head(&f, view), was, "{}", view.display());
    }
    assert_eq!(
        fs::read_to_string(dirty.join("value.txt")).unwrap(),
        "edited"
    );
    assert_eq!(
        f.git(other_branch, &["symbolic-ref", "--short", "HEAD"]),
        "topic"
    );
    let log = stdout(&f.swl(&f.root, &["views"]));
    assert!(
        log.contains(&format!(
            "swl views: zwanenzang\n└─ {} is up to date at {}",
            behind.display(),
            short(&new)
        )),
        "{log}"
    );

    // Without a fetch, what the source knows: nothing new.
    f.commit_on_forge("zwanenzang", "master", "zwanenzang-3");
    let log = stdout(&f.swl(&f.root, &["views", "--no-fetch"]));
    assert!(log.contains("is up to date at"), "{log}");
    assert_eq!(head(&f, behind), new);
}

#[test]
fn views_warn_about_a_missing_view_and_one_that_is_no_checkout() {
    let f = Forge::new("views-missing");
    let log = stdout(&f.swl(&f.root, &["views"]));
    assert_eq!(log, "swl views: no repo of the config sets view");

    let missing = f.root.join("umbrella/swan-lake");
    f.set_view("swan-lake", &missing);
    let plain = f.root.join("umbrella/zwanenzang");
    fs::create_dir_all(plain.join("sub")).unwrap();
    f.set_view("zwanenzang", &plain);
    let log = stdout(&f.swl(&f.root, &["views"]));
    assert_eq!(
        log,
        format!(
            "swl views: swan-lake\n└─ skipped {}: it does not exist\nswl views: zwanenzang\n└─ \
             skipped {}: it is not a git checkout",
            missing.display(),
            plain.display()
        )
    );

    // A directory inside a checkout is not one.
    let view = f.add_view("swan-lake-common");
    fs::create_dir_all(view.join("sub")).unwrap();
    fs::write(view.join("sub/file"), "x").unwrap();
    f.set_view("zwanenzang", &view.join("sub"));
    let log = stdout(&f.swl(&f.root, &["views"]));
    assert!(
        log.contains(&format!(
            "skipped {}: it is not a git checkout",
            view.join("sub").display()
        )),
        "{log}"
    );

    f.git(&view, &["switch", "-q", "--detach"]);
    let log = stdout(&f.swl(&f.root, &["views"]));
    let at = head(&f, &view);
    assert!(
        log.contains(&format!(
            "swl views: swan-lake-common\n└─ skipped {}: it is detached at {}, not on master",
            view.display(),
            short(&at)
        )),
        "{log}"
    );
}

//! `swl ls` on a disposable forge: a line per workspace with its stage and
//! whether `swl ws prune` would remove it, and the forge's open MRs that no
//! workspace holds, grouped by ticket ID or branch.

mod support;

use std::fs;

use support::{Forge, stdout};

/// The line of workspace `name`, its columns' padding collapsed to two
/// spaces.
fn ws_line(out: &str, name: &str) -> String {
    let want = format!("{name}  ");
    out.lines()
        .map(|l| {
            let mut l = l.trim().to_string();
            while l.contains("   ") {
                l = l.replace("   ", "  ");
            }
            l
        })
        .find(|l| l.starts_with(&want))
        .unwrap_or_else(|| panic!("no line of {name:?} in\n{out}"))
}

/// The forge section: what follows `open on the forge, in no workspace:`.
fn forge_section(out: &str) -> &str {
    out.split_once("open on the forge, in no workspace:\n")
        .unwrap_or_else(|| panic!("no forge section in\n{out}"))
        .1
}

/// The forge section's lines, trimmed, the padding collapsed.
fn forge_lines(out: &str) -> Vec<String> {
    forge_section(out)
        .lines()
        .map(|l| {
            let mut l = l.trim().to_string();
            while l.contains("   ") {
                l = l.replace("   ", "  ");
            }
            l
        })
        .collect()
}

const SHA: &str = "1111111111111111111111111111111111111111";

#[test]
fn ls_lists_each_workspace_with_its_stage_and_whether_it_is_removable() {
    let f = Forge::new("ls-local");
    // Ignored files do not keep a workspace.
    f.commit_file_on_forge("swan-lake", "master", ".gitignore", "*.log\n");
    let work = f.root.join("work");
    fs::create_dir_all(&work).unwrap();
    for name in [
        "SL-0001-clean",
        "SL-0002-dirty",
        "SL-0003-untracked",
        "SL-0004-unpushed",
        "SL-0005-ignored",
        "SL-0006-pushed",
    ] {
        stdout(&f.swl(&work, &["start", name, "swan-lake", "swan-lake-common@lib"]));
    }
    fs::create_dir(work.join("SL-0007-empty")).unwrap();
    let consumer = |ws: &str| work.join(ws).join("swan-lake");
    fs::write(consumer("SL-0002-dirty").join("value.txt"), "changed").unwrap();
    fs::write(consumer("SL-0003-untracked").join("new.txt"), "new").unwrap();
    fs::write(consumer("SL-0004-unpushed").join("value.txt"), "c2").unwrap();
    f.git(&consumer("SL-0004-unpushed"), &["commit", "-qam", "c2"]);
    fs::write(consumer("SL-0005-ignored").join("build.log"), "log").unwrap();
    let pushed = consumer("SL-0006-pushed");
    fs::write(pushed.join("value.txt"), "c3").unwrap();
    f.git(&pushed, &["commit", "-qam", "c3"]);
    f.git(&pushed, &["push", "-q", "origin", "sl-0006-pushed"]);
    f.git(&pushed, &["reset", "-q", "--hard", "origin/master"]);

    for args in [&["ls"][..], &["ls", "--no-fetch"][..]] {
        let out = stdout(&f.swl(&f.root, args));
        assert!(
            out.starts_with(&format!("workspaces in {}:", work.display())),
            "{out}"
        );
        assert_eq!(
            ws_line(&out, "SL-0001-clean"),
            "SL-0001-clean  removable  no change (HEAD is in origin/master)  waits: nobody  next: -",
            "{out}"
        );
        assert_eq!(
            ws_line(&out, "SL-0004-unpushed"),
            "SL-0004-unpushed  local  waits: anyone  next: swl publish swan-lake",
            "{out}"
        );
        for kept in [
            "SL-0002-dirty",
            "SL-0003-untracked",
            "SL-0004-unpushed",
            "SL-0006-pushed",
            "SL-0007-empty",
        ] {
            assert!(
                !ws_line(&out, kept).contains("  removable  "),
                "{kept} is kept\n{out}"
            );
        }
        assert!(
            ws_line(&out, "SL-0005-ignored").contains("  removable  "),
            "{out}"
        );
        assert!(
            ws_line(&out, "SL-0007-empty").contains("no change (no checkouts)"),
            "{out}"
        );
    }
    let out = stdout(&f.swl(&f.root, &["ls"]));
    assert!(
        out.ends_with("no open MR on the forge that no workspace holds"),
        "{out}"
    );
}

#[test]
fn ls_groups_the_open_mrs_no_workspace_holds_by_ticket_or_branch() {
    let f = Forge::new("ls-forge");
    f.api.make_private("swan-lake");
    let keys = f.login().display().to_string();
    let work = f.root.join("work");
    fs::create_dir_all(&work).unwrap();
    stdout(&f.swl(&work, &["start", "SL-0010-x", "swan-lake"]));
    // Held: its checkout is on swan-lake#4's branch, and its swl.toml names
    // swan-lake-common#11.
    fs::write(
        work.join("SL-0010-x/swan-lake/swl.toml"),
        "[inputs.lib]\nrepo = \"swan-lake-common\"\nmr = 11\n",
    )
    .unwrap();
    let pr = |repo: &str, n: u64, state: &str, branch: &str| {
        let merged = (state == "merged").then_some(SHA);
        f.api.set_pr(repo, n, state, branch, SHA, merged);
    };
    pr("swan-lake", 4, "open", "sl-0010-x");
    pr("swan-lake-common", 11, "open", "other-x");
    pr("swan-lake", 5, "draft", "sl-0042-a");
    pr("swan-lake-common", 7, "open", "SL-42-b");
    pr("swan-lake", 6, "open", "feature-x");
    pr("zwanenzang", 2, "open", "feature-x");
    pr("swan-lake", 8, "open", "swl-bump-common");
    pr("zwanenzang", 3, "open", "zw-only");
    pr("swan-lake-common", 9, "open", "common-only");
    pr("swan-lake", 12, "merged", "sl-0050-done");
    pr("swan-lake", 13, "closed", "sl-0051-dropped");

    let ls = f.swl_env(&f.root, &["ls"], &[("SWL_FJ_KEYS", &keys)]);
    assert!(!support::text(&ls).contains(support::TOKEN));
    let out = stdout(&ls);
    assert_eq!(
        forge_lines(&out),
        [
            "SL-0042",
            "swan-lake#5  open (draft)  sl-0042-a  WIP: t",
            "swan-lake-common#7  open  SL-42-b  t",
            "swl start --from swan-lake#5",
            "common-only",
            "swan-lake-common#9  open  common-only  t",
            "swl start --from swan-lake-common#9 (only shared-repo MRs: common alone, as the consumer)",
            "feature-x",
            "swan-lake#6  open  feature-x  t",
            "zwanenzang#2  open  feature-x  t",
            "swl start --from swan-lake#6",
            "swl-bump-common",
            "swan-lake#8 (bump)  open  swl-bump-common  t",
            "swl start --from swan-lake#8",
            "zw-only",
            "zwanenzang#3  open  zw-only  t",
            "only shared-repo MRs: no consumer MR to swl start --from",
        ],
        "{out}"
    );
    // The private consumer's MRs need fj's login.
    assert!(
        f.authed_requests("swan-lake")
            .iter()
            .any(|p| p.contains("/pulls?state=open")),
        "{out}"
    );
    let anon = stdout(&f.swl(&f.root, &["ls"]));
    let lines = forge_lines(&anon);
    assert!(
        lines.iter().any(
            |l| l.starts_with("swan-lake: not read (not readable anonymously, and no fj login")
        ),
        "{anon}"
    );
    assert!(!lines.iter().any(|l| l.starts_with("swan-lake#")), "{anon}");

    // --no-fetch: the workspaces only, and no request to the forge.
    f.api.requests.lock().unwrap().clear();
    let out = stdout(&f.swl_env(&f.root, &["ls", "--no-fetch"], &[("SWL_FJ_KEYS", &keys)]));
    assert_eq!(
        ws_line(&out, "SL-0010-x"),
        "SL-0010-x  no change (HEAD is in origin/master)  waits: nobody  next: -",
        "{out}"
    );
    assert!(
        out.ends_with("--no-fetch: the forge is not read: its open MRs are not listed"),
        "{out}"
    );
    assert!(!out.contains("open on the forge"), "{out}");
    let seen: Vec<String> = f
        .api
        .requests
        .lock()
        .unwrap()
        .iter()
        .map(|r| r.path.clone())
        .collect();
    assert!(seen.is_empty(), "forge requests with --no-fetch: {seen:?}");
}

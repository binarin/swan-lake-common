//! `swl ls` on a disposable forge: a line per workspace with its stage and
//! whether `swl ws prune` would remove it.

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
}

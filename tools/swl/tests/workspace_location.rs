//! Workspace locations and selectors, through the public CLI.

mod support;

use std::fs;
use support::{Forge, failed, stdout, text};

fn workspaces(f: &Forge, value: Option<&str>) {
    let path = f.root.join("repos.toml");
    let old = fs::read_to_string(&path).unwrap();
    let rest = old
        .lines()
        .filter(|l| !l.starts_with("workspaces ="))
        .collect::<Vec<_>>()
        .join("\n");
    let prefix = value.map_or(String::new(), |v| format!("workspaces = {v:?}\n"));
    fs::write(path, format!("{prefix}{rest}\n")).unwrap();
}

#[test]
fn start_uses_default_root_and_expands_configured_home_independently_of_cwd() {
    let f = Forge::new("location-start-default");
    let cwd = f.root.join("unrelated/nested");
    fs::create_dir_all(&cwd).unwrap();
    workspaces(&f, None);
    stdout(&f.swl(&cwd, &["start", "SL-0088-swl-version", "swan-lake"]));
    let default = f
        .root
        .join("home/personal-workspace/swan-lake-umbrella/workspaces/SL-0088-swl-version");
    assert!(default.join("swan-lake/flake.nix").is_file());
    assert!(!cwd.join("SL-0088-swl-version").exists());
    // A relative configured root is based on repos.toml, not this cwd.
    workspaces(&f, Some("relative-workspaces"));
    stdout(&f.swl(&cwd, &["start", "SL-0089-relative", "swan-lake"]));
    assert!(
        f.root
            .join("relative-workspaces/SL-0089-relative/swan-lake/flake.nix")
            .is_file()
    );
    workspaces(&f, Some("~/custom-workspaces"));
    stdout(&f.swl(&cwd, &["start", "SL-0090-home", "swan-lake"]));
    assert!(
        f.root
            .join("home/custom-workspaces/SL-0090-home/swan-lake/flake.nix")
            .is_file()
    );
}

#[test]
fn selectors_and_enclosing_workspace_work_for_every_workspace_command() {
    let f = Forge::new("location-selectors");
    stdout(&f.swl(&f.root, &["start", "SL-0088-swl-version", "swan-lake"]));
    let ws = f.root.join("work/SL-0088-swl-version");
    let nested = ws.join("swan-lake/src/nested");
    fs::create_dir_all(&nested).unwrap();
    let expected = stdout(&f.swl(&ws, &["status", "--no-fetch"]));
    assert_eq!(stdout(&f.swl(&nested, &["status", "--no-fetch"])), expected);
    for selector in ["sl-88", "SL-00088", "sL-88-SWL-version"] {
        assert_eq!(
            stdout(&f.swl(&f.root, &["status", selector, "--no-fetch"])),
            expected
        );
        assert_eq!(
            stdout(&f.swl(&f.root, &["status", "--workspace", selector, "--no-fetch"])),
            expected
        );
        stdout(&f.swl(&f.root, &["env", selector]));
        stdout(&f.swl(&f.root, &["env", "--workspace", selector]));
        for command in ["land", "refresh"] {
            let positional = f.swl(&f.root, &[command, selector, "--dry-run"]);
            let flag = f.swl(&f.root, &[command, "--workspace", selector, "--dry-run"]);
            let local = f.swl(&nested, &[command, "--dry-run"]);
            assert_eq!(
                positional.status.code(),
                flag.status.code(),
                "{}",
                text(&positional)
            );
            assert_eq!(positional.stdout, flag.stdout);
            assert_eq!(positional.stdout, local.stdout);
            assert!(
                !text(&positional).contains("no workspace"),
                "{}",
                text(&positional)
            );
        }
    }
    // ws add and publish already have positional checkout/consumer arguments.
    stdout(&f.swl(
        &nested,
        &["ws", "add", "swan-lake-common@common", "--no-fetch"],
    ));
    stdout(&f.swl(
        &f.root,
        &[
            "ws",
            "add",
            "zwanenzang@zwanenzang",
            "--workspace",
            "sl-88",
            "--no-fetch",
        ],
    ));
    assert!(ws.join("zwanenzang@zwanenzang/flake.nix").is_file());
    let publish = f.swl(
        &f.root,
        &["publish", "not-a-consumer", "--workspace", "sl-88"],
    );
    failed(&publish, 2, "not-a-consumer");
    assert!(
        !text(&publish).contains("no workspace"),
        "{}",
        text(&publish)
    );
    stdout(&f.swl(&nested, &["env"]));
    assert!(!nested.join(".swl").exists());
}

#[test]
fn ambiguous_ticket_lists_candidates_for_all_commands_and_never_chooses_exact_name() {
    let f = Forge::new("location-ambiguity");
    for name in ["SL-0088-one", "sl-88-two"] {
        fs::create_dir_all(f.root.join("work").join(name)).unwrap();
    }
    let commands: &[&[&str]] = &[
        &["status", "sl-88", "--no-fetch"],
        &["env", "sl-88"],
        &["land", "sl-88", "--dry-run"],
        &["refresh", "sl-88", "--dry-run"],
        &["publish", "swan-lake", "--workspace", "sl-88"],
        &["ws", "add", "swan-lake", "--workspace", "sl-88", "--no-env"],
    ];
    for args in commands {
        let out = f.swl(&f.root, args);
        failed(&out, 2, "ambiguous");
        for name in ["SL-0088-one", "sl-88-two"] {
            assert!(text(&out).contains(name), "{}", text(&out));
        }
    }
    // Even an exact spelling must not hide another normalized full-name match.
    fs::create_dir_all(f.root.join("work/sl-88-ONE")).unwrap();
    failed(&f.swl(&f.root, &["status", "SL-0088-one"]), 2, "ambiguous");
    failed(&f.swl(&f.root, &["status", "sl-99"]), 2, "no workspace");
}

#[test]
fn start_refuses_equivalent_names_but_exact_reruns_and_explicit_paths_work() {
    let f = Forge::new("location-collision");
    stdout(&f.swl(&f.root, &["start", "SL-0088-x", "swan-lake"]));
    for name in ["sl-0088-x", "sl-88-X", "SL-00088-x"] {
        let out = f.swl(&f.root, &["start", name, "swan-lake"]);
        failed(&out, 2, "existing workspace");
        assert!(text(&out).contains("SL-0088-x"), "{}", text(&out));
        assert!(!f.root.join("work").join(name).exists());
    }
    stdout(&f.swl(&f.root, &["start", "SL-0088-x", "swan-lake"]));
    let outside = f.root.join("outside workspace");
    stdout(&f.swl(
        &f.root,
        &[
            "start",
            "SL-0089-y",
            "swan-lake",
            "--workspace",
            outside.to_str().unwrap(),
        ],
    ));
    stdout(&f.swl(
        &f.root,
        &[
            "status",
            "--workspace",
            outside.to_str().unwrap(),
            "--no-fetch",
        ],
    ));
    let nested = outside.join("swan-lake/nested");
    fs::create_dir_all(&nested).unwrap();
    stdout(&f.swl(&nested, &["env"]));
    assert!(!nested.join(".swl").exists());
}

#[test]
fn configured_name_precedes_legacy_relative_directory_and_explicit_path_bypasses_lookup() {
    let f = Forge::new("location-paths");
    stdout(&f.swl(&f.root, &["start", "SL-0088-x", "swan-lake"]));
    let legacy = f.root.join("SL-0088-x");
    fs::create_dir_all(&legacy).unwrap();
    let configured = stdout(&f.swl(&f.root, &["status", "SL-0088-x", "--no-fetch"]));
    assert!(configured.contains("swan-lake"), "{configured}");
    let explicit = stdout(&f.swl(&f.root, &["status", "./SL-0088-x", "--no-fetch"]));
    assert!(!explicit.contains("swan-lake "), "{explicit}");
    fs::rename(&legacy, f.root.join("legacy")).unwrap();
    // No configured match: preserve the old bare relative directory syntax.
    stdout(&f.swl(&f.root, &["status", "--workspace", "legacy", "--no-fetch"]));
    for args in [
        vec!["env", "sl-88", "--workspace", "legacy"],
        vec!["status", "sl-88", "--from", "swan-lake#1"],
    ] {
        failed(&f.swl(&f.root, &args), 2, "cannot be used with");
    }
}

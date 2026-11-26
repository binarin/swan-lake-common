//! `swl --version`: one line, the Cargo version and the commit of common
//! that the package sets.

use std::process::Command;

#[test]
fn version_is_one_line_with_the_cargo_version_and_the_commit() {
    let out = Command::new(env!("CARGO_BIN_EXE_swl"))
        .arg("--version")
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    let text = String::from_utf8(out.stdout).unwrap();
    let want = match (option_env!("SWL_COMMIT"), option_env!("SWL_COMMIT_DATE")) {
        (Some(c), Some(d)) if !c.is_empty() && !d.is_empty() => format!("({c} {d})"),
        (Some(c), _) if !c.is_empty() => format!("({c})"),
        _ => "(unknown)".to_string(),
    };
    assert_eq!(text, format!("swl {} {want}\n", env!("CARGO_PKG_VERSION")));
}

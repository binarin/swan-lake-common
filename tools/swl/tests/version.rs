//! `swl --version`: one line, the Cargo version and the commit of common
//! that the package's wrapper sets at runtime.

use std::process::Command;

fn version(env: &[(&str, &str)]) -> String {
    let mut c = Command::new(env!("CARGO_BIN_EXE_swl"));
    for var in ["SWL_COMMIT", "SWL_COMMIT_DATE", "SWL_SELF"] {
        c.env_remove(var);
    }
    let out = c
        .envs(env.iter().copied())
        .arg("--version")
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    String::from_utf8(out.stdout).unwrap()
}

#[test]
fn version_is_one_line_with_the_cargo_version_and_the_commit_from_the_environment() {
    let v = env!("CARGO_PKG_VERSION");
    assert_eq!(
        version(&[("SWL_COMMIT", "abc1234"), ("SWL_COMMIT_DATE", "2026-11-26")]),
        format!("swl {v} (abc1234 2026-11-26)\n")
    );
    assert_eq!(
        version(&[("SWL_COMMIT", "abc1234-dirty"), ("SWL_COMMIT_DATE", "")]),
        format!("swl {v} (abc1234-dirty)\n")
    );
    assert_eq!(
        version(&[("SWL_COMMIT", ""), ("SWL_COMMIT_DATE", "2026-11-26")]),
        format!("swl {v} (unknown)\n")
    );
    assert_eq!(version(&[]), format!("swl {v} (unknown)\n"));
}

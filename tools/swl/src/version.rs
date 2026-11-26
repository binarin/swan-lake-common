//! `swl --version`.

use std::sync::LazyLock;

/// What `swl --version` prints after `swl `: the Cargo version, then the
/// short rev and commit date of common that the package sets at build time
/// (`SWL_COMMIT`, `SWL_COMMIT_DATE`): `0.2.0 (abc1234 2026-11-26)`.
pub static VERSION: LazyLock<String> = LazyLock::new(|| {
    describe(
        env!("CARGO_PKG_VERSION"),
        option_env!("SWL_COMMIT"),
        option_env!("SWL_COMMIT_DATE"),
    )
});

/// `<version> (<commit> <date>)`, without what is unset or empty; a build
/// without a commit (plain cargo) is `<version> (unknown)`.
fn describe(version: &str, commit: Option<&str>, date: Option<&str>) -> String {
    let known: Vec<&str> = [commit, date]
        .into_iter()
        .flatten()
        .filter(|s| !s.is_empty())
        .collect();
    match commit.filter(|c| !c.is_empty()) {
        Some(_) => format!("{version} ({})", known.join(" ")),
        None => format!("{version} (unknown)"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describe_names_the_commit_when_the_build_has_one() {
        assert_eq!(
            describe("0.2.0", Some("abc1234"), Some("2026-11-26")),
            "0.2.0 (abc1234 2026-11-26)"
        );
        assert_eq!(
            describe("0.2.0", Some("abc1234-dirty"), Some("2026-11-26")),
            "0.2.0 (abc1234-dirty 2026-11-26)"
        );
        assert_eq!(describe("0.2.0", Some("abc1234"), None), "0.2.0 (abc1234)");
        assert_eq!(describe("0.2.0", None, None), "0.2.0 (unknown)");
        assert_eq!(describe("0.2.0", Some(""), Some("")), "0.2.0 (unknown)");
        assert_eq!(
            describe("0.2.0", None, Some("2026-11-26")),
            "0.2.0 (unknown)"
        );
    }
}

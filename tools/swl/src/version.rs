//! `swl --version`, and the version of the swl the workspace's wrappers
//! exec, for `swl status`.

use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::LazyLock,
};

use crate::{env::wrapper_target, shim::Tool, workspace::WRAPPER_DIR};

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

/// The `swl --version` line of the running swl.
pub fn line() -> String {
    format!("swl {}", *VERSION)
}

/// The swl that the workspace's wrappers exec, as `<ws>/.swl/bin/nix`
/// names it.
#[derive(Debug, PartialEq, Eq)]
pub enum Wrapped {
    /// No wrapper: `swl env` has not run in the workspace.
    Missing(PathBuf),
    /// The swl it execs and the line its `--version` printed.
    Runs { exe: PathBuf, version: String },
    /// The wrapper names no swl, or that swl does not run.
    Broken(String),
}

/// Reads the nix wrapper of the workspace at `root` and runs the swl it
/// execs with `--version`.
pub fn wrapped(root: &Path) -> Wrapped {
    let wrapper = root.join(WRAPPER_DIR).join(Tool::Nix.name());
    let text = match std::fs::read_to_string(&wrapper) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Wrapped::Missing(wrapper),
        Err(e) => return Wrapped::Broken(format!("reading {}: {e}", wrapper.display())),
    };
    let Some(exe) = wrapper_target(&text) else {
        return Wrapped::Broken(format!(
            "{} execs no swl as swl env writes it",
            wrapper.display()
        ));
    };
    let out = match crate::logging::output(Command::new(&exe).arg("--version")) {
        Ok(out) => out,
        Err(e) => return Wrapped::Broken(format!("running {}: {e}", exe.display())),
    };
    let version = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if !out.status.success() || version.is_empty() {
        return Wrapped::Broken(format!(
            "{} --version printed no version ({}): {}",
            exe.display(),
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Wrapped::Runs { exe, version }
}

/// The lines `swl status` prints about the running swl (`running`, its
/// `--version` line, at `exe`) and the wrappers' one, and the warning when
/// the wrappers' swl differs or does not run.
pub fn status_lines(running: &str, exe: &Path, wrapped: &Wrapped) -> (Vec<String>, Option<String>) {
    let mut lines = vec![format!("running swl: {running} at {}", exe.display())];
    let warning = match wrapped {
        Wrapped::Missing(wrapper) => {
            lines.push(format!(
                "wrappers' swl: none yet ({} is missing; swl env writes it)",
                wrapper.display()
            ));
            None
        }
        Wrapped::Runs { exe, version } => {
            lines.push(format!("wrappers' swl: {version} at {}", exe.display()));
            (version != running).then(|| {
                format!(
                    "warning: the wrappers exec {version}, not the running {running}; rerun swl env"
                )
            })
        }
        Wrapped::Broken(reason) => {
            lines.push(format!("wrappers' swl: unknown ({reason})"));
            Some(format!(
                "warning: the wrappers' swl does not run ({reason}); rerun swl env"
            ))
        }
    };
    (lines, warning)
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
        assert!(line().starts_with(&format!("swl {} (", env!("CARGO_PKG_VERSION"))));
    }

    #[test]
    fn status_warns_only_when_the_wrappers_swl_differs_or_does_not_run() {
        let running = "swl 0.2.0 (abc1234 2026-11-26)";
        let exe = Path::new("/bin/swl");
        let runs = |version: &str| Wrapped::Runs {
            exe: PathBuf::from("/s/swl"),
            version: version.into(),
        };
        assert_eq!(
            status_lines(running, exe, &runs(running)),
            (
                vec![
                    "running swl: swl 0.2.0 (abc1234 2026-11-26) at /bin/swl".to_string(),
                    "wrappers' swl: swl 0.2.0 (abc1234 2026-11-26) at /s/swl".to_string(),
                ],
                None
            )
        );
        let (lines, warning) = status_lines(running, exe, &runs("swl 0.1.0"));
        assert_eq!(lines[1], "wrappers' swl: swl 0.1.0 at /s/swl");
        assert_eq!(
            warning.as_deref(),
            Some(
                "warning: the wrappers exec swl 0.1.0, not the running swl 0.2.0 (abc1234 2026-11-26); rerun swl env"
            )
        );
        let (lines, warning) = status_lines(
            running,
            exe,
            &Wrapped::Missing(PathBuf::from("/w/.swl/bin/nix")),
        );
        assert_eq!(
            lines[1],
            "wrappers' swl: none yet (/w/.swl/bin/nix is missing; swl env writes it)"
        );
        assert_eq!(warning, None);
        let (lines, warning) = status_lines(running, exe, &Wrapped::Broken("gone".into()));
        assert_eq!(lines[1], "wrappers' swl: unknown (gone)");
        assert_eq!(
            warning.as_deref(),
            Some("warning: the wrappers' swl does not run (gone); rerun swl env")
        );
    }
}

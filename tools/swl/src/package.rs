//! What the package tells the binary at runtime. `packages.swl` in
//! modules/swl.nix is a wrapper around the binary (`swl-unwrapped`) that
//! sets `SWL_COMMIT` and `SWL_COMMIT_DATE`, the short rev and commit date of
//! common, for `swl --version`, and `SWL_SELF`, the wrapper's own path, for
//! what `swl env` pins. Without the wrapper (a cargo build) they are unset.

use std::{path::PathBuf, sync::OnceLock};

pub const COMMIT_ENV: &str = "SWL_COMMIT";
pub const COMMIT_DATE_ENV: &str = "SWL_COMMIT_DATE";
pub const SELF_ENV: &str = "SWL_SELF";

#[derive(Debug, Default)]
pub struct PackageEnv {
    pub commit: Option<String>,
    pub commit_date: Option<String>,
    pub self_path: Option<PathBuf>,
}

static PACKAGE_ENV: OnceLock<PackageEnv> = OnceLock::new();

fn non_empty(var: &str) -> Option<String> {
    std::env::var(var).ok().filter(|v| !v.is_empty())
}

/// The variables as the process got them; empty ones count as unset.
pub fn get() -> &'static PackageEnv {
    PACKAGE_ENV.get_or_init(|| PackageEnv {
        commit: non_empty(COMMIT_ENV),
        commit_date: non_empty(COMMIT_DATE_ENV),
        self_path: non_empty(SELF_ENV).map(PathBuf::from),
    })
}

/// Reads the variables, then removes them from the environment, so that
/// what swl runs (nix, a dev shell, another swl in it) does not inherit the
/// rev and the path of this one.
///
/// # Safety
///
/// No other thread may be running: call it first in `main`.
pub unsafe fn take() {
    get();
    for var in [COMMIT_ENV, COMMIT_DATE_ENV, SELF_ENV] {
        // SAFETY: the caller guarantees a single thread.
        unsafe { std::env::remove_var(var) };
    }
}

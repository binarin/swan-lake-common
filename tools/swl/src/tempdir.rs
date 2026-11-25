//! A scratch directory removed on drop.

use std::{
    env, fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU32, Ordering},
};

use anyhow::{Context, Result};

pub struct TempDir(PathBuf);

impl TempDir {
    /// A new empty directory `<tmp>/<prefix>-<pid>-<n>`, by its canonical
    /// path.
    pub fn new(prefix: &str) -> Result<Self> {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = env::temp_dir().join(format!("{prefix}-{}-{n}", std::process::id()));
        if dir.exists() {
            fs::remove_dir_all(&dir).with_context(|| format!("removing {}", dir.display()))?;
        }
        fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        // Canonical, so that paths below it compare as nix resolves them.
        let dir = dir
            .canonicalize()
            .with_context(|| format!("resolving {}", dir.display()))?;
        Ok(Self(dir))
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

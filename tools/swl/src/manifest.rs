//! The manifest `swl.toml` at a repo root: the shared-repo MRs that the
//! repo's branch depends on, one `[inputs.<input-path>]` table each.
//!
//! ```toml
//! [inputs.common]
//! repo = "swan-lake-common"
//! mr = 24
//!
//! [inputs."common/zwanenzang"]
//! repo = "zwanenzang"
//! mr = 7
//! ```
//!
//! An absent file means nothing is pending. Unknown keys are errors.

use std::{collections::BTreeMap, fs, path::Path};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

pub const MANIFEST_FILE: &str = "swl.toml";

#[derive(Debug, Default, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    /// Keyed by input path: `common`, or a nested `common/zwanenzang`.
    #[serde(default)]
    pub inputs: BTreeMap<String, Entry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    /// The shared repo's name, e.g. `swan-lake-common`.
    pub repo: String,
    /// The MR (Forgejo pull request) number in that repo.
    pub mr: u64,
}

fn valid_name(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
        && !s.starts_with('.')
}

/// An input path: `/`-separated input names.
pub fn valid_input_path(path: &str) -> bool {
    path.split('/').all(valid_name)
}

impl Manifest {
    pub fn parse(text: &str) -> Result<Self> {
        let m: Self = toml::from_str(text)?;
        for (path, e) in &m.inputs {
            if !valid_input_path(path) {
                bail!(
                    "[inputs.{path:?}]: not an input path such as common or \"common/zwanenzang\""
                );
            }
            if !valid_name(&e.repo) {
                bail!("[inputs.{path:?}]: repo {:?} is not a repo name", e.repo);
            }
            if e.mr == 0 {
                bail!("[inputs.{path:?}]: mr must be a positive MR number");
            }
        }
        Ok(m)
    }

    /// The manifest of the repo at `root`; `None` when it has no `swl.toml`.
    pub fn read(root: &Path) -> Result<Option<Self>> {
        let path = root.join(MANIFEST_FILE);
        match fs::read_to_string(&path) {
            Ok(text) => Self::parse(&text)
                .map(Some)
                .with_context(|| format!("parsing {}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.inputs.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEXT: &str = r#"
[inputs.common]
repo = "swan-lake-common"
mr = 24

[inputs."common/zwanenzang"]
repo = "zwanenzang"
mr = 7
"#;

    #[test]
    fn parses_nested_paths() {
        let m = Manifest::parse(TEXT).unwrap();
        assert_eq!(
            m.inputs["common"],
            Entry {
                repo: "swan-lake-common".into(),
                mr: 24
            }
        );
        assert_eq!(m.inputs["common/zwanenzang"].mr, 7);
        assert_eq!(Manifest::parse("").unwrap(), Manifest::default());
        let dir = std::env::temp_dir().join(format!("swl-manifest-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        assert_eq!(Manifest::read(&dir).unwrap(), None);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn strict_parsing() {
        for bad in [
            "[inputs.common]\nrepo = \"x\"\nmr = 1\nbranch = \"b\"\n",
            "[inputs.common]\nrepo = \"x\"\n",
            "[inputs.common]\nmr = 1\n",
            "[inputs.common]\nrepo = \"x\"\nmr = \"1\"\n",
            "[inputs.common]\nrepo = \"x\"\nmr = 0\n",
            "[inputs.common]\nrepo = \"x\"\nmr = -3\n",
            "[inputs.common]\nrepo = \"a/b\"\nmr = 1\n",
            "[inputs.\"common/\"]\nrepo = \"x\"\nmr = 1\n",
            "[inputs.\"a b\"]\nrepo = \"x\"\nmr = 1\n",
            "[other]\n",
            "version = 1\n",
        ] {
            assert!(Manifest::parse(bad).is_err(), "{bad}");
        }
    }
}

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

    /// The file contents, in a fixed form: a header comment, then the tables
    /// sorted by input path.
    pub fn render(&self) -> String {
        let mut s = String::from(
            "# Pending shared-repo MRs for this branch, written by `swl publish`.\n\
             # `swl finalize` removes the entries that have landed; master has no swl.toml.\n",
        );
        for (path, e) in &self.inputs {
            s.push_str(&format!(
                "\n[inputs.{}]\nrepo = {}\nmr = {}\n",
                toml_key(path),
                toml_string(&e.repo),
                e.mr
            ));
        }
        s
    }

    /// Writes the manifest, or removes the file when it is empty. Returns
    /// whether the file changed.
    pub fn write(&self, root: &Path) -> Result<bool> {
        let path = root.join(MANIFEST_FILE);
        if self.is_empty() {
            return match fs::remove_file(&path) {
                Ok(()) => Ok(true),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
                Err(e) => Err(e).with_context(|| format!("removing {}", path.display())),
            };
        }
        let text = self.render();
        if fs::read_to_string(&path).is_ok_and(|old| old == text) {
            return Ok(false);
        }
        fs::write(&path, text).with_context(|| format!("writing {}", path.display()))?;
        Ok(true)
    }
}

fn toml_string(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

fn toml_key(k: &str) -> String {
    if !k.is_empty()
        && k.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        k.to_string()
    } else {
        toml_string(k)
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

    #[test]
    fn render_round_trips_and_quotes_nested_paths() {
        let m = Manifest::parse(TEXT).unwrap();
        let text = m.render();
        assert!(text.contains("\n[inputs.common]\nrepo = \"swan-lake-common\"\nmr = 24\n"));
        assert!(text.contains("\n[inputs.\"common/zwanenzang\"]\nrepo = \"zwanenzang\"\nmr = 7\n"));
        assert_eq!(Manifest::parse(&text).unwrap(), m);
    }

    #[test]
    fn write_and_remove() {
        let dir = std::env::temp_dir().join(format!("swl-manifest-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        assert_eq!(Manifest::read(&dir).unwrap(), None);
        let mut m = Manifest::parse(TEXT).unwrap();
        assert!(m.write(&dir).unwrap());
        assert!(!m.write(&dir).unwrap());
        assert_eq!(Manifest::read(&dir).unwrap(), Some(m.clone()));
        m.inputs.clear();
        assert!(m.write(&dir).unwrap());
        assert!(!dir.join(MANIFEST_FILE).exists());
        assert!(!m.write(&dir).unwrap());
        fs::remove_dir_all(&dir).unwrap();
    }
}

//! The parts of a `flake.lock` that hop derivation and `status` read.

use std::{collections::BTreeMap, fs, path::Path};

use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Deserialize)]
pub struct Lock {
    pub nodes: BTreeMap<String, Node>,
    pub root: String,
}

#[derive(Debug, Default, Deserialize)]
pub struct Node {
    #[serde(default)]
    pub inputs: BTreeMap<String, InputRef>,
    pub locked: Option<Value>,
    pub original: Option<Value>,
}

/// A node's input: a node key, or a `follows` path from the root.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum InputRef {
    Node(String),
    Follows(Vec<String>),
}

impl Lock {
    pub fn parse(text: &str) -> Result<Self> {
        Ok(serde_json::from_str(text)?)
    }

    /// The lock of the flake in `dir`, or `None` when it has no `flake.lock`.
    pub fn read(dir: &Path) -> Result<Option<Self>> {
        let path = dir.join("flake.lock");
        match fs::read_to_string(&path) {
            Ok(text) => Self::parse(&text)
                .map(Some)
                .with_context(|| format!("parsing {}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }

    pub fn node(&self, key: &str) -> Option<&Node> {
        self.nodes.get(key)
    }

    /// The node key an input path such as `["common", "zwanenzang"]` resolves
    /// to, following `follows` entries.
    pub fn resolve(&self, path: &[String]) -> Option<String> {
        self.resolve_depth(path, 0)
    }

    fn resolve_depth(&self, path: &[String], depth: usize) -> Option<String> {
        if depth > 64 {
            return None;
        }
        let mut key = self.root.clone();
        for name in path {
            key = match self.node(&key)?.inputs.get(name)? {
                InputRef::Node(k) => k.clone(),
                InputRef::Follows(p) => self.resolve_depth(p, depth + 1)?,
            };
        }
        Some(key)
    }

    /// The locked `rev` at an input path.
    pub fn locked_rev(&self, path: &[String]) -> Option<String> {
        let node = self.node(&self.resolve(path)?)?;
        node.locked
            .as_ref()?
            .get("rev")?
            .as_str()
            .map(str::to_string)
    }
}

impl Node {
    /// The flake URLs this node was written as and locked to, in the forms the
    /// config's URL patterns use (`github:owner/repo`, `git+<url>`).
    pub fn urls(&self) -> Vec<String> {
        [&self.original, &self.locked]
            .into_iter()
            .flatten()
            .filter_map(attrs_url)
            .collect()
    }
}

fn attrs_url(attrs: &Value) -> Option<String> {
    let s = |k: &str| attrs.get(k).and_then(Value::as_str);
    match s("type")? {
        t @ ("github" | "gitlab" | "sourcehut") => {
            Some(format!("{t}:{}/{}", s("owner")?, s("repo")?))
        }
        "git" => Some(format!("git+{}", s("url")?)),
        "path" => Some(format!("path:{}", s("path")?)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOCK: &str = r#"{
      "nodes": {
        "root": { "inputs": { "common": "common", "zwanenzang": ["common", "zwanenzang"] } },
        "common": {
          "inputs": { "zwanenzang": "zwanenzang" },
          "locked": { "type": "github", "owner": "binarin", "repo": "swan-lake-common", "rev": "c0ffee" },
          "original": { "type": "github", "owner": "binarin", "repo": "swan-lake-common", "ref": "swan-lake-current" }
        },
        "zwanenzang": {
          "locked": { "type": "git", "url": "https://forgejo.lynx-lizard.ts.net/binarin/zwanenzang", "rev": "beef" },
          "original": { "type": "git", "url": "https://forgejo.lynx-lizard.ts.net/binarin/zwanenzang" }
        }
      },
      "root": "root",
      "version": 7
    }"#;

    fn path(p: &str) -> Vec<String> {
        p.split('/').map(str::to_string).collect()
    }

    #[test]
    fn resolves_nested_and_follows_paths() {
        let lock = Lock::parse(LOCK).unwrap();
        assert_eq!(lock.resolve(&path("common")).as_deref(), Some("common"));
        assert_eq!(
            lock.resolve(&path("common/zwanenzang")).as_deref(),
            Some("zwanenzang")
        );
        assert_eq!(
            lock.resolve(&path("zwanenzang")).as_deref(),
            Some("zwanenzang")
        );
        assert_eq!(lock.resolve(&path("nope")), None);
        assert_eq!(
            lock.locked_rev(&path("common/zwanenzang")).as_deref(),
            Some("beef")
        );
    }

    #[test]
    fn node_urls_use_pattern_forms() {
        let lock = Lock::parse(LOCK).unwrap();
        assert_eq!(
            lock.node("common").unwrap().urls(),
            [
                "github:binarin/swan-lake-common",
                "github:binarin/swan-lake-common"
            ]
        );
        assert_eq!(
            lock.node("zwanenzang").unwrap().urls()[0],
            "git+https://forgejo.lynx-lizard.ts.net/binarin/zwanenzang"
        );
    }
}

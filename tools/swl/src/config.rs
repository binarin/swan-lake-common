//! The repos config: which local repository a workspace checkout is created
//! from, and which flake URLs name a repo.

use std::{
    collections::BTreeMap,
    env, fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, anyhow};
use serde::Deserialize;

/// URL forms that name the repo `{repo}` when a config does not set
/// `url_patterns`.
pub const DEFAULT_URL_PATTERNS: &[&str] = &[
    "github:binarin/{repo}",
    "git+https://forgejo.lynx-lizard.ts.net/binarin/{repo}",
    "git+ssh://forgejo.lynx-lizard.ts.net/binarin/{repo}",
];

pub const DEFAULT_BASE: &str = "origin/master";

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    url_patterns: Option<Vec<String>>,
    #[serde(default)]
    repos: BTreeMap<String, RepoConfig>,
    /// The file read and where its path came from; `None` for no file.
    #[serde(skip)]
    source: Option<(PathBuf, ConfigSource)>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepoConfig {
    /// A bare repository or a main checkout; worktrees are added from it.
    pub source: PathBuf,
    /// Revision new worktrees start from.
    pub base: Option<String>,
    /// Extra URL forms for this repo only, besides `url_patterns`.
    #[serde(default)]
    pub urls: Vec<String>,
}

impl RepoConfig {
    pub fn base(&self) -> &str {
        self.base.as_deref().unwrap_or(DEFAULT_BASE)
    }
}

/// Where the config path came from: only the default location may be
/// missing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigSource {
    Flag,
    Env,
    Default,
}

/// `--config`, else `$SWL_CONFIG`, else `$XDG_CONFIG_HOME/swl/repos.toml`
/// (`~/.config/swl/repos.toml`).
pub fn config_path(flag: Option<&Path>) -> Option<(PathBuf, ConfigSource)> {
    if let Some(p) = flag {
        return Some((p.to_path_buf(), ConfigSource::Flag));
    }
    if let Some(p) = env::var_os("SWL_CONFIG").filter(|p| !p.is_empty()) {
        return Some((PathBuf::from(p), ConfigSource::Env));
    }
    let base = env::var_os("XDG_CONFIG_HOME")
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some((base.join("swl/repos.toml"), ConfigSource::Default))
}

impl Config {
    /// Loads the config; a missing file at the default location is an empty
    /// config, so the shim works without one. A path given by `--config` or
    /// `$SWL_CONFIG` must exist.
    pub fn load(flag: Option<&Path>) -> Result<Self> {
        let Some((path, source)) = config_path(flag) else {
            return Ok(Self::default());
        };
        let path = std::path::absolute(&path).unwrap_or(path);
        let mut config = match fs::read_to_string(&path) {
            Ok(text) => Self::parse(&text).with_context(|| format!("config {}", path.display()))?,
            Err(e)
                if e.kind() == std::io::ErrorKind::NotFound && source == ConfigSource::Default =>
            {
                Self::default()
            }
            Err(e) => return Err(e).with_context(|| format!("reading config {}", path.display())),
        };
        config.source = Some((path, source));
        Ok(config)
    }

    /// The absolute config path when it was chosen explicitly (`--config` or
    /// `$SWL_CONFIG`), for the wrappers to pass on to the shim.
    pub fn explicit_path(&self) -> Option<&Path> {
        match &self.source {
            Some((path, ConfigSource::Flag | ConfigSource::Env)) => Some(path),
            _ => None,
        }
    }

    pub fn parse(text: &str) -> Result<Self> {
        Ok(toml::from_str(text)?)
    }

    pub fn repo(&self, name: &str) -> Result<&RepoConfig> {
        self.repos.get(name).ok_or_else(|| {
            let read = match &self.source {
                Some((path, _)) => path.display().to_string(),
                None => "no config path".to_string(),
            };
            anyhow!("repo {name:?} is not in the swl config {read} (add a [repos.{name}] table)")
        })
    }

    /// Whether `url` (a flake URL, as built by [`crate::lock::Node::urls`]) names `repo`.
    pub fn url_names_repo(&self, url: &str, repo: &str) -> bool {
        let url = normalize_url(url);
        let patterns: Vec<&str> = match &self.url_patterns {
            Some(p) => p.iter().map(String::as_str).collect(),
            None => DEFAULT_URL_PATTERNS.to_vec(),
        };
        let extra = self
            .repos
            .get(repo)
            .map(|r| r.urls.as_slice())
            .unwrap_or(&[]);
        patterns
            .into_iter()
            .map(|p| p.replace("{repo}", repo))
            .chain(extra.iter().cloned())
            .any(|candidate| normalize_url(&candidate) == url)
    }
}

/// Lower-cases, drops the query and fragment, a trailing `/` or `.git` and the
/// user part of `scheme://user@host`, so that the ssh forms with or without a
/// user compare equal.
pub fn normalize_url(url: &str) -> String {
    let url = url.split(['?', '#']).next().unwrap_or("").to_lowercase();
    let mut url = url.trim_end_matches('/').to_string();
    if let Some(stripped) = url.strip_suffix(".git") {
        url = stripped.to_string();
    }
    if let Some(i) = url.find("://") {
        let rest = &url[i + 3..];
        let host_end = rest.find('/').unwrap_or(rest.len());
        if let Some(at) = rest[..host_end].rfind('@') {
            url = format!("{}{}", &url[..i + 3], &rest[at + 1..]);
        }
    }
    url
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_patterns_recognise_the_lock_url_forms() {
        let c = Config::default();
        for url in [
            "github:binarin/swan-lake-common",
            "github:BinArin/swan-lake-common",
            "git+https://forgejo.lynx-lizard.ts.net/binarin/zwanenzang",
            "git+https://forgejo.lynx-lizard.ts.net/binarin/zwanenzang.git",
            "git+ssh://forgejo@forgejo.lynx-lizard.ts.net/binarin/zwanenzang",
            "git+ssh://git@forgejo.lynx-lizard.ts.net/binarin/zwanenzang/",
        ] {
            let repo = url.rsplit('/').find(|s| !s.is_empty()).unwrap();
            let repo = repo.trim_end_matches(".git");
            assert!(c.url_names_repo(url, repo), "{url}");
        }
        assert!(!c.url_names_repo("github:binarin/swan-lake-common", "swan-lake"));
        assert!(!c.url_names_repo("github:other/swan-lake-common", "swan-lake-common"));
    }

    #[test]
    fn config_patterns_replace_defaults_and_repo_urls_add() {
        let c = Config::parse(
            r#"
url_patterns = ["git+file:///srv/{repo}"]
[repos.zw]
source = "/srv/zw"
urls = ["git+https://example.org/zw-mirror"]
"#,
        )
        .unwrap();
        assert!(c.url_names_repo("git+file:///srv/zw", "zw"));
        assert!(c.url_names_repo("git+https://example.org/zw-mirror", "zw"));
        assert!(!c.url_names_repo("github:binarin/zw", "zw"));
        assert_eq!(c.repo("zw").unwrap().base(), "origin/master");
        assert!(c.repo("other").is_err());
    }

    #[test]
    fn unknown_keys_are_errors() {
        assert!(Config::parse("[repos.x]\nsource = \"/x\"\nbranch = \"m\"\n").is_err());
    }

    #[test]
    fn an_explicit_config_must_exist_and_is_passed_on() {
        let dir = env::temp_dir().join(format!("swl-config-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let missing = dir.join("missing.toml");
        let err = Config::load(Some(&missing)).unwrap_err();
        assert!(format!("{err:#}").contains("missing.toml"), "{err:#}");
        let file = dir.join("repos.toml");
        fs::write(&file, "[repos.zw]\nsource = \"/srv/zw\"\n").unwrap();
        let config = Config::load(Some(&file)).unwrap();
        assert_eq!(config.explicit_path(), Some(file.as_path()));
        let err = config.repo("other").unwrap_err().to_string();
        assert!(err.contains(&file.display().to_string()), "{err}");
        assert_eq!(Config::default().explicit_path(), None);
        fs::remove_dir_all(&dir).unwrap();
    }
}

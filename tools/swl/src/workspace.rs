//! A workspace: a directory of checkouts, and the `--override-input` hops its
//! shared checkouts give a flake in it.

use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};

use crate::{
    config::Config,
    lock::{InputRef, Lock},
};

/// Directory, inside the workspace, of the generated wrappers.
pub const WRAPPER_DIR: &str = ".swl/bin";

#[derive(Debug)]
pub struct Workspace {
    pub root: PathBuf,
    /// Sorted by name.
    pub checkouts: Vec<Checkout>,
}

#[derive(Debug)]
pub struct Checkout {
    /// The directory name: `<repo>` for a consumer, `<repo>@<input>` for a
    /// shared checkout.
    pub name: String,
    pub dir: PathBuf,
    /// `(repo, input)` of a shared checkout.
    pub shared: Option<(String, String)>,
}

impl Checkout {
    pub fn is_consumer(&self) -> bool {
        self.shared.is_none()
    }
}

/// One `--override-input <input_path> git+file://<dir>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Override {
    /// `common`, or a nested hop such as `common/zwanenzang`.
    pub input_path: String,
    pub checkout: String,
    pub dir: PathBuf,
}

impl Override {
    pub fn url(&self) -> String {
        format!("git+file://{}", self.dir.display())
    }
}

/// `common←swan-lake-common@common common/zwanenzang←zwanenzang@zwanenzang`,
/// the value of `SWAN_LAKE_OVERRIDES`.
pub fn overrides_summary(overrides: &[Override]) -> String {
    overrides
        .iter()
        .map(|o| format!("{}←{}", o.input_path, o.checkout))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Splits a checkout directory name; `None` for a consumer name.
pub fn parse_shared_name(name: &str) -> Result<Option<(String, String)>> {
    match name.split_once('@') {
        None => Ok(None),
        Some((repo, input))
            if !repo.is_empty() && !input.is_empty() && !input.contains(['@', '/']) =>
        {
            Ok(Some((repo.to_string(), input.to_string())))
        }
        Some(_) => bail!("{name:?} is not <repo>@<input>"),
    }
}

impl Workspace {
    /// Every direct subdirectory with a `flake.nix` is a checkout; dot
    /// directories are skipped.
    pub fn scan(root: &Path) -> Result<Self> {
        let root = root
            .canonicalize()
            .with_context(|| format!("workspace {}", root.display()))?;
        let mut checkouts = Vec::new();
        for entry in fs::read_dir(&root).with_context(|| format!("reading {}", root.display()))? {
            let entry = entry?;
            let Some(name) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            let dir = entry.path();
            if name.starts_with('.') || !dir.is_dir() || !dir.join("flake.nix").is_file() {
                continue;
            }
            let shared = parse_shared_name(&name)?;
            checkouts.push(Checkout { name, dir, shared });
        }
        checkouts.sort_by(|a, b| a.name.cmp(&b.name));
        let mut inputs = BTreeSet::new();
        for c in &checkouts {
            if let Some((_, input)) = &c.shared
                && !inputs.insert(input.as_str())
            {
                bail!(
                    "two checkouts in {} override the input {input:?}",
                    root.display()
                );
            }
        }
        Ok(Self { root, checkouts })
    }

    pub fn wrapper_dir(&self) -> PathBuf {
        self.root.join(WRAPPER_DIR)
    }

    fn shared_for_input(&self, input: &str) -> Option<&Checkout> {
        self.checkouts
            .iter()
            .find(|c| c.shared.as_ref().is_some_and(|(_, i)| i == input))
    }

    /// The checkout whose top directory is the flake directory `dir`.
    pub fn checkout_at(&self, dir: &Path) -> Option<&Checkout> {
        let dir = dir.canonicalize().ok()?;
        self.checkouts.iter().find(|c| c.dir == dir)
    }

    /// The overrides for the flake of checkout `target`, read from the
    /// checkouts' `flake.lock` files.
    pub fn overrides_for(&self, config: &Config, target: &Checkout) -> Result<Vec<Override>> {
        let Some(lock) = Lock::read(&target.dir)? else {
            return Ok(Vec::new());
        };
        self.derive_overrides(config, &target.name, &lock, &|c| Lock::read(&c.dir))
    }

    /// Hop derivation. A root input of the target that is not a `follows`, is
    /// named like a shared checkout's input and is locked to that checkout's
    /// repo is overridden. The walk then continues in that checkout's own
    /// lock, which Nix uses below the override: its inputs get the path prefix
    /// `<input>/`. Inputs that `follows` another, in either lock, are left to
    /// the follows. Only checkouts in the workspace are walked through, so a
    /// hop needs its intermediate repo checked out too.
    pub fn derive_overrides(
        &self,
        config: &Config,
        target: &str,
        target_lock: &Lock,
        load: &dyn Fn(&Checkout) -> Result<Option<Lock>>,
    ) -> Result<Vec<Override>> {
        let mut out = Vec::new();
        let mut chain = vec![target.to_string()];
        self.hops(
            config,
            target_lock,
            &target_lock.root,
            "",
            &BTreeSet::new(),
            &mut chain,
            load,
            &mut out,
        )?;
        Ok(out)
    }

    #[allow(clippy::too_many_arguments)]
    fn hops(
        &self,
        config: &Config,
        lock: &Lock,
        node_key: &str,
        prefix: &str,
        outer_follows: &BTreeSet<String>,
        chain: &mut Vec<String>,
        load: &dyn Fn(&Checkout) -> Result<Option<Lock>>,
        out: &mut Vec<Override>,
    ) -> Result<()> {
        let Some(node) = lock.node(node_key) else {
            return Ok(());
        };
        for (name, input) in &node.inputs {
            let InputRef::Node(key) = input else { continue };
            if outer_follows.contains(name) {
                continue;
            }
            let Some(checkout) = self.shared_for_input(name) else {
                continue;
            };
            let Some((repo, _)) = &checkout.shared else {
                continue;
            };
            if chain.contains(&checkout.name) {
                continue;
            }
            let Some(child) = lock.node(key) else {
                continue;
            };
            if !child.urls().iter().any(|u| config.url_names_repo(u, repo)) {
                continue;
            }
            let input_path = format!("{prefix}{name}");
            out.push(Override {
                input_path: input_path.clone(),
                checkout: checkout.name.clone(),
                dir: checkout.dir.clone(),
            });
            let follows: BTreeSet<String> = child
                .inputs
                .iter()
                .filter(|(_, i)| matches!(i, InputRef::Follows(_)))
                .map(|(n, _)| n.clone())
                .collect();
            if let Some(checkout_lock) = load(checkout)? {
                chain.push(checkout.name.clone());
                self.hops(
                    config,
                    &checkout_lock,
                    &checkout_lock.root,
                    &format!("{input_path}/"),
                    &follows,
                    chain,
                    load,
                    out,
                )?;
                chain.pop();
            }
        }
        Ok(())
    }
}

/// The directory of the flake a path flake reference names: the nearest
/// directory at or above `path` with a `flake.nix`, as Nix searches.
pub fn find_flake_dir(path: &Path) -> Option<PathBuf> {
    let mut dir = path.canonicalize().ok()?;
    if !dir.is_dir() {
        dir.pop();
    }
    loop {
        if dir.join("flake.nix").is_file() {
            return Some(dir);
        }
        if dir.join(".git").exists() || !dir.pop() {
            return None;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    fn github(repo: &str) -> String {
        format!(
            r#"{{ "type": "github", "owner": "binarin", "repo": "{repo}", "rev": "r-{repo}" }}"#
        )
    }

    fn forgejo(repo: &str) -> String {
        format!(
            r#"{{ "type": "git", "url": "https://forgejo.lynx-lizard.ts.net/binarin/{repo}", "rev": "r-{repo}" }}"#
        )
    }

    /// Like swan-lake: common, a common-next that is not checked out, and
    /// root inputs that follow common's.
    fn consumer_lock() -> Lock {
        Lock::parse(&format!(
            r#"{{ "root": "root", "version": 7, "nodes": {{
              "root": {{ "inputs": {{
                "common": "common", "common-next": "common-next",
                "nixpkgs": ["common", "nixpkgs"], "zwanenzang": ["common", "zwanenzang"] }} }},
              "common": {{ "inputs": {{ "nixpkgs": "nixpkgs", "zwanenzang": "zwanenzang" }},
                           "locked": {common}, "original": {common} }},
              "common-next": {{ "inputs": {{ "zwanenzang": "zwanenzang_2" }},
                                "locked": {common}, "original": {common} }},
              "nixpkgs": {{ "locked": {nixpkgs}, "original": {nixpkgs} }},
              "zwanenzang": {{ "inputs": {{ "nixpkgs": ["common", "nixpkgs"] }},
                               "locked": {zw}, "original": {zw} }},
              "zwanenzang_2": {{ "locked": {zw}, "original": {zw} }}
            }} }}"#,
            common = github("swan-lake-common"),
            nixpkgs = github("nixpkgs"),
            zw = forgejo("zwanenzang"),
        ))
        .unwrap()
    }

    fn common_lock() -> Lock {
        Lock::parse(&format!(
            r#"{{ "root": "root", "version": 7, "nodes": {{
              "root": {{ "inputs": {{ "nixpkgs": "nixpkgs", "zwanenzang": "zwanenzang" }} }},
              "nixpkgs": {{ "locked": {nixpkgs}, "original": {nixpkgs} }},
              "zwanenzang": {{ "inputs": {{ "nixpkgs": ["nixpkgs"] }}, "locked": {zw}, "original": {zw} }}
            }} }}"#,
            nixpkgs = github("nixpkgs"),
            zw = forgejo("zwanenzang"),
        ))
        .unwrap()
    }

    fn workspace(names: &[&str]) -> Workspace {
        Workspace {
            root: PathBuf::from("/ws"),
            checkouts: names
                .iter()
                .map(|n| Checkout {
                    name: n.to_string(),
                    dir: PathBuf::from("/ws").join(n),
                    shared: parse_shared_name(n).unwrap(),
                })
                .collect(),
        }
    }

    fn derive(ws: &Workspace, target: &str, lock: &Lock, locks: BTreeMap<&str, Lock>) -> String {
        let locks = std::cell::RefCell::new(locks);
        let load = |c: &Checkout| Ok(locks.borrow_mut().remove(c.name.as_str()));
        let out = ws
            .derive_overrides(&Config::default(), target, lock, &load)
            .unwrap();
        overrides_summary(&out)
    }

    #[test]
    fn root_hop_only_for_checked_out_input() {
        let ws = workspace(&["swan-lake", "swan-lake-common@common"]);
        let s = derive(&ws, "swan-lake", &consumer_lock(), BTreeMap::new());
        assert_eq!(s, "common←swan-lake-common@common");
    }

    #[test]
    fn nested_hop_comes_from_the_checkout_lock() {
        let ws = workspace(&[
            "swan-lake",
            "swan-lake-common@common",
            "zwanenzang@zwanenzang",
        ]);
        let locks = BTreeMap::from([("swan-lake-common@common", common_lock())]);
        let s = derive(&ws, "swan-lake", &consumer_lock(), locks);
        // The root `zwanenzang` follows common/zwanenzang, and common-next
        // is not checked out, so neither gets an override of its own.
        assert_eq!(
            s,
            "common←swan-lake-common@common common/zwanenzang←zwanenzang@zwanenzang"
        );
    }

    #[test]
    fn no_hop_through_a_repo_that_is_not_checked_out() {
        let ws = workspace(&["swan-lake", "zwanenzang@zwanenzang"]);
        assert_eq!(
            derive(&ws, "swan-lake", &consumer_lock(), BTreeMap::new()),
            ""
        );
    }

    #[test]
    fn shared_checkout_as_target_gets_its_own_inputs() {
        let ws = workspace(&["swan-lake-common@common", "zwanenzang@zwanenzang"]);
        let s = derive(
            &ws,
            "swan-lake-common@common",
            &common_lock(),
            BTreeMap::new(),
        );
        assert_eq!(s, "zwanenzang←zwanenzang@zwanenzang");
    }

    #[test]
    fn input_locked_to_another_repo_is_not_overridden() {
        let ws = workspace(&["swan-lake", "nixpkgs-fork@common"]);
        assert_eq!(
            derive(&ws, "swan-lake", &consumer_lock(), BTreeMap::new()),
            ""
        );
    }

    #[test]
    fn follows_declared_by_the_outer_flake_win_over_the_checkout_lock() {
        // The consumer makes common's zwanenzang follow its own root input;
        // the checkout's lock still has it as a node of its own.
        let consumer = Lock::parse(&format!(
            r#"{{ "root": "root", "version": 7, "nodes": {{
              "root": {{ "inputs": {{ "common": "common", "zwanenzang": "zwanenzang" }} }},
              "common": {{ "inputs": {{ "zwanenzang": ["zwanenzang"] }}, "locked": {common} }},
              "zwanenzang": {{ "locked": {zw} }}
            }} }}"#,
            common = github("swan-lake-common"),
            zw = forgejo("zwanenzang"),
        ))
        .unwrap();
        let ws = workspace(&[
            "swan-lake",
            "swan-lake-common@common",
            "zwanenzang@zwanenzang",
        ]);
        let locks = BTreeMap::from([("swan-lake-common@common", common_lock())]);
        assert_eq!(
            derive(&ws, "swan-lake", &consumer, locks),
            "common←swan-lake-common@common zwanenzang←zwanenzang@zwanenzang"
        );
    }

    #[test]
    fn shared_names() {
        assert_eq!(parse_shared_name("swan-lake").unwrap(), None);
        assert_eq!(
            parse_shared_name("swan-lake-common@common-next-disko").unwrap(),
            Some(("swan-lake-common".into(), "common-next-disko".into()))
        );
        assert!(parse_shared_name("@common").is_err());
        assert!(parse_shared_name("a@b@c").is_err());
    }
}

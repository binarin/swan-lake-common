//! Workspace names and ticket IDs refer to directories below the configured
//! root. Explicit paths remain available for workspaces outside that root.

use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};

use crate::config::{Config, expand_home};

/// Normalise a ticket's numeric part without parsing it into a bounded integer.
/// A full name retains its suffix; a ticket-only selector matches any suffix.
fn normalized(name: &str) -> String {
    let lower = name.to_lowercase();
    let Some((prefix, rest)) = lower.split_once('-') else {
        return lower;
    };
    let (number, suffix) = rest
        .split_once('-')
        .map_or((rest, None), |(n, s)| (n, Some(s)));
    if prefix.is_empty()
        || !prefix.bytes().all(|b| b.is_ascii_alphabetic())
        || number.is_empty()
        || !number.bytes().all(|b| b.is_ascii_digit())
    {
        return lower;
    }
    let number = number.trim_start_matches('0');
    let number = if number.is_empty() { "0" } else { number };
    format!(
        "{prefix}-{number}{}",
        suffix.map_or(String::new(), |s| format!("-{s}"))
    )
}

fn same_name(left: &str, right: &str) -> bool {
    normalized(left) == normalized(right)
}

fn matches_selector(selector: &str, name: &str) -> bool {
    let selector = normalized(selector);
    let name = normalized(name);
    name == selector
        || (selector.split('-').count() == 2
            && selector
                .rsplit('-')
                .next()
                .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
            && name.starts_with(&format!("{selector}-")))
}

fn directories(root: &Path) -> Result<Vec<PathBuf>> {
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e).with_context(|| format!("reading workspaces {}", root.display())),
    };
    let mut dirs = Vec::new();
    for entry in entries {
        let entry = entry?;
        if entry.path().is_dir() && !entry.file_name().to_string_lossy().starts_with('.') {
            dirs.push(entry.path());
        }
    }
    dirs.sort();
    Ok(dirs)
}

fn lookup(root: &Path, selector: &str, matches: fn(&str, &str) -> bool) -> Result<Option<PathBuf>> {
    let candidates: Vec<_> = directories(root)?
        .into_iter()
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| matches(selector, n))
        })
        .collect();
    match candidates.as_slice() {
        [] => Ok(None),
        [path] => Ok(Some(path.clone())),
        _ => bail!(
            "workspace {selector:?} is ambiguous; candidates:\n{}",
            candidates
                .iter()
                .map(|p| format!("  {}", p.display()))
                .collect::<Vec<_>>()
                .join("\n")
        ),
    }
}

fn explicit_path(path: &Path) -> bool {
    path.is_absolute()
        || path.components().count() != 1
        || path == Path::new("~")
        || path == Path::new(".")
        || path == Path::new("..")
}

/// Resolve an existing-operation selector. Configured names take precedence
/// over legacy bare relative directories; `./name` always requests a path.
pub fn resolve(config: &Config, cwd: &Path, selector: Option<&Path>) -> Result<PathBuf> {
    if let Some(selector) = selector.filter(|p| explicit_path(p)) {
        return Ok(cwd.join(expand_home(selector)?));
    }
    let root = config.workspaces()?;
    if let Some(selector) = selector {
        if let Some(path) = lookup(&root, &selector.to_string_lossy(), matches_selector)? {
            return Ok(path);
        }
        let legacy = cwd.join(selector);
        if legacy.is_dir() {
            return Ok(legacy);
        }
        bail!(
            "no workspace {:?} in {} (use an explicit path such as ./{} for a local directory)",
            selector,
            root.display(),
            selector.display()
        );
    }
    let cwd = cwd
        .canonicalize()
        .with_context(|| format!("current directory {}", cwd.display()))?;
    let root = root.canonicalize().unwrap_or(root);
    if let Some(dir) = cwd
        .ancestors()
        .find(|dir| dir.parent() == Some(root.as_path()))
    {
        return Ok(dir.to_path_buf());
    }
    if let Some(dir) = cwd.ancestors().find(|dir| dir.join(".swl").is_dir()) {
        return Ok(dir.to_path_buf());
    }
    // Older workspaces may not have wrappers yet. A containing flake must
    // be a worktree (.git file), not an ordinary main checkout (.git directory).
    if let Some(flake) = cwd.ancestors().find(|dir| dir.join("flake.nix").is_file())
        && flake.join(".git").is_file()
        && let Some(parent) = flake.parent()
        && !parent.join("flake.nix").exists()
    {
        return Ok(parent.to_path_buf());
    }
    Ok(cwd)
}

/// Preserve exact-name reruns, but never create a second spelling of a name.
pub fn for_start(config: &Config, cwd: &Path, name: &str, path: Option<&Path>) -> Result<PathBuf> {
    let target = match path {
        Some(path) => cwd.join(expand_home(path)?),
        None => config.workspaces()?.join(name),
    };
    if let (Some(parent), Some(name)) =
        (target.parent(), target.file_name().and_then(|n| n.to_str()))
    {
        let equivalent: Vec<_> = directories(parent)?
            .into_iter()
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n != name && same_name(n, name))
            })
            .collect();
        if !equivalent.is_empty() {
            bail!(
                "workspace {name:?} already exists with an equivalent name; use the existing workspace:\n{}",
                equivalent
                    .iter()
                    .map(|p| format!("  {}", p.display()))
                    .collect::<Vec<_>>()
                    .join("\n")
            );
        }
    }
    Ok(target)
}

/// `land --from` is an existing operation: reuse a unique branch-name match,
/// otherwise use the same configured destination as `start --from`.
pub fn for_land_from(
    config: &Config,
    cwd: &Path,
    branch: &str,
    selector: Option<&Path>,
) -> Result<PathBuf> {
    if let Some(selector) = selector {
        return resolve(config, cwd, Some(selector));
    }
    let root = config.workspaces()?;
    Ok(lookup(&root, branch, same_name)?.unwrap_or_else(|| root.join(branch)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ticket_matching_is_case_insensitive_and_ignores_zero_padding() {
        for selector in ["sl-88", "SL-0088", "sL-00088-SWL-version"] {
            assert!(matches_selector(selector, "SL-0088-swl-version"));
        }
        assert!(!matches_selector("sl-8", "SL-0088-swl-version"));
        assert!(!matches_selector("sl-88-other", "SL-0088-swl-version"));
        assert_eq!(normalized("SL-0000-X"), "sl-0-x");
        assert_eq!(normalized("Feature-X"), "feature-x");
    }

    #[test]
    fn land_from_reuses_equivalent_names_and_reports_ambiguity() {
        let tmp = crate::tempdir::TempDir::new("swl-location-land").unwrap();
        let config = Config::parse(&format!(
            "workspaces = {:?}",
            tmp.path().display().to_string()
        ))
        .unwrap();
        let existing = tmp.path().join("SL-0088-X");
        fs::create_dir(&existing).unwrap();
        assert_eq!(
            for_land_from(&config, tmp.path(), "sl-88-x", None).unwrap(),
            existing
        );
        assert_eq!(
            for_land_from(&config, tmp.path(), "sl-99-new", None).unwrap(),
            tmp.path().join("sl-99-new")
        );
        fs::create_dir(tmp.path().join("sl-88-x")).unwrap();
        let err = for_land_from(&config, tmp.path(), "sl-88-x", None)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("ambiguous") && err.contains("SL-0088-X") && err.contains("sl-88-x"),
            "{err}"
        );
    }

    #[test]
    fn cwd_inside_legacy_workspace_without_wrappers_selects_its_root() {
        let tmp = crate::tempdir::TempDir::new("swl-location-cwd").unwrap();
        let ws = tmp.path().join("legacy");
        let source = tmp.path().join("source");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("flake.nix"), "{}").unwrap();
        let git = |args: &[&str]| crate::git::git(&source, args).unwrap();
        git(&["init", "-q"]);
        git(&["add", "flake.nix"]);
        git(&[
            "-c",
            "user.name=swl test",
            "-c",
            "user.email=swl@example.org",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-qm",
            "init",
        ]);
        git(&[
            "worktree",
            "add",
            "--detach",
            ws.join("consumer").to_str().unwrap(),
        ]);
        let nested = ws.join("consumer/src/nested");
        fs::create_dir_all(&nested).unwrap();
        let config = Config::parse(&format!(
            "workspaces = {:?}",
            tmp.path().join("elsewhere").display().to_string()
        ))
        .unwrap();
        assert_eq!(
            resolve(&config, &nested, None).unwrap(),
            ws.canonicalize().unwrap()
        );
    }

    #[test]
    fn empty_legacy_cwd_does_not_select_ancestor_with_unrelated_flakes() {
        let tmp = crate::tempdir::TempDir::new("swl-location-empty").unwrap();
        let cwd = tmp.path().join("nested/empty-workspace");
        fs::create_dir_all(&cwd).unwrap();
        fs::create_dir(tmp.path().join("unrelated")).unwrap();
        fs::write(tmp.path().join("unrelated/flake.nix"), "{}").unwrap();
        let config = Config::parse(&format!(
            "workspaces = {:?}",
            tmp.path().join("workspaces").display().to_string()
        ))
        .unwrap();
        assert_eq!(
            resolve(&config, &cwd, None).unwrap(),
            cwd.canonicalize().unwrap()
        );
    }
    #[test]
    fn implicit_land_branch_is_a_full_name_not_a_ticket_selector() {
        let tmp = crate::tempdir::TempDir::new("swl-location-land-ticket").unwrap();
        let config = Config::parse(&format!(
            "workspaces = {:?}",
            tmp.path().display().to_string()
        ))
        .unwrap();
        let other = tmp.path().join("SL-0088-other");
        fs::create_dir(&other).unwrap();
        assert_eq!(
            for_land_from(&config, tmp.path(), "sl-88", None).unwrap(),
            tmp.path().join("sl-88")
        );
        assert_eq!(
            resolve(&config, tmp.path(), Some(Path::new("sl-88"))).unwrap(),
            other
        );
        let exact = tmp.path().join("SL-0088");
        fs::create_dir(&exact).unwrap();
        assert_eq!(
            for_land_from(&config, tmp.path(), "sl-88", None).unwrap(),
            exact
        );
    }

    #[test]
    fn ordinary_main_checkout_alongside_sibling_flake_preserves_cwd() {
        let tmp = crate::tempdir::TempDir::new("swl-location-main").unwrap();
        let main = tmp.path().join("main");
        let nested = main.join("src/nested");
        fs::create_dir_all(&nested).unwrap();
        crate::git::git(&main, &["init", "-q"]).unwrap();
        fs::write(main.join("flake.nix"), "{}").unwrap();
        let sibling = tmp.path().join("sibling");
        fs::create_dir(&sibling).unwrap();
        fs::write(sibling.join("flake.nix"), "{}").unwrap();
        let config = Config::parse(&format!(
            "workspaces = {:?}",
            tmp.path().join("workspaces").display().to_string()
        ))
        .unwrap();
        for cwd in [&main, &nested] {
            assert_eq!(
                resolve(&config, cwd, None).unwrap(),
                cwd.canonicalize().unwrap()
            );
        }
    }
}

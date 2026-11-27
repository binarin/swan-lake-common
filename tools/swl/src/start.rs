//! `swl start`: a workspace for one change, with its checkouts on the
//! change's branches, `swl env` and `direnv allow`. New, or joining a
//! published consumer MR (`--from <consumer>#<MR>`). Rerunnable: existing
//! checkouts are kept, missing ones are added.

use std::{
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use anyhow::{Context, Result, anyhow, bail};

use crate::{
    config::Config,
    forge::{PrReader, PrState},
    git::{git, git_ok, short},
    manifest::{MANIFEST_FILE, Manifest},
    report::{self, Report},
    shim::find_real_tool,
    style::{self, BOLD, Tone, pad, paint, width},
    workspace::parse_shared_name,
    ws_add::{self, Fetcher, Start},
};

/// Where a checkout of the workspace should be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Branch(String),
    Detached(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    /// The checkout directory name.
    pub name: String,
    pub target: Target,
    /// Printed when the checkout is added, e.g. why it is detached.
    pub note: Option<String>,
}

/// `<consumer>#<MR>`.
pub fn parse_from(from: &str) -> Result<(String, u64)> {
    let parse = || {
        let (repo, n) = from.split_once('#')?;
        let n: u64 = n.parse().ok().filter(|n| *n > 0)?;
        (!repo.is_empty() && !repo.contains(['@', '/'])).then(|| (repo.to_string(), n))
    };
    parse().ok_or_else(|| anyhow!("--from {from:?} is not <consumer>#<MR>, e.g. swan-lake#12"))
}

/// `(sha, refname)` lines of `git ls-remote`.
pub fn parse_ls_remote(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|l| {
            let (sha, name) = l.split_once('\t')?;
            Some((sha.trim().to_string(), name.trim().to_string()))
        })
        .collect()
}

/// The head of `refs/pull/<n>/head` and the one branch at that head, from
/// `ls-remote` lines. The base branch is not a candidate: an MR whose head
/// it is was merged.
pub fn mr_branch(
    refs: &[(String, String)],
    repo: &str,
    n: u64,
    base_branch: &str,
) -> Result<(String, String)> {
    let pull = format!("refs/pull/{n}/head");
    let Some((head, _)) = refs.iter().find(|(_, r)| *r == pull) else {
        bail!("{repo}#{n}: no {pull} on the forge: no such MR");
    };
    let branches: Vec<&str> = refs
        .iter()
        .filter(|(sha, _)| sha == head)
        .filter_map(|(_, r)| r.strip_prefix("refs/heads/"))
        .collect();
    let topics: Vec<&str> = branches
        .iter()
        .copied()
        .filter(|b| *b != base_branch)
        .collect();
    match topics.as_slice() {
        [b] => Ok((head.clone(), b.to_string())),
        [] if branches.contains(&base_branch) => bail!(
            "{repo}#{n}: its head {} is the head of {base_branch}: the MR is merged; nothing to join",
            short(head)
        ),
        [] => bail!(
            "{repo}#{n}: its head {} is not the head of any branch: the MR was merged and its \
             branch deleted, or the branch moved on; nothing to join",
            short(head)
        ),
        many => bail!(
            "{repo}#{n}: its head {} is the head of several branches ({}); use swl start with \
             --branch instead",
            short(head),
            many.join(", ")
        ),
    }
}

/// The checkout name of a manifest entry: `<repo>@<last segment of the input
/// path>`, e.g. `zwanenzang@zwanenzang` for `common/zwanenzang`.
pub fn entry_checkout(repo: &str, input_path: &str) -> String {
    let last = input_path.rsplit('/').next().unwrap_or(input_path);
    format!("{repo}@{last}")
}

/// The remote of the repo's source to read MRs and branches from: the one
/// its base names, else `origin`.
pub fn source_remote(config: &Config, repo: &str) -> Result<(PathBuf, String)> {
    let rc = config.repo(repo)?;
    let remote = match ws_add::base_remote(&rc.source, rc.base())? {
        Some(r) => r,
        None if git(&rc.source, &["remote"])?.lines().any(|r| r == "origin") => "origin".into(),
        None => bail!(
            "{repo}: the source {} has no remote to read MRs from",
            rc.source.display()
        ),
    };
    Ok((rc.source.clone(), remote))
}

/// A published consumer MR: its branch, head and manifest at that head.
pub struct ConsumerMr {
    pub branch: String,
    pub head: String,
    pub manifest: Manifest,
}

/// Reads `consumer#n` with git (consumer repos are private, the anonymous
/// API cannot see them): `ls-remote` for the MR head and its branch, then the
/// head fetched into the source for its `swl.toml`.
pub fn read_consumer_mr(
    config: &Config,
    consumer: &str,
    n: u64,
    fetcher: &Fetcher,
) -> Result<ConsumerMr> {
    let (source, remote) = source_remote(config, consumer)?;
    let pull = format!("refs/pull/{n}/head");
    let refs = git(&source, &["ls-remote", &remote, &pull, "refs/heads/*"])
        .with_context(|| format!("reading {consumer}#{n} from {remote}"))?;
    let base = config.base_for(consumer);
    let base_branch = base.split_once('/').map_or(base, |(_, b)| b);
    let (head, branch) = mr_branch(&parse_ls_remote(&refs), consumer, n, base_branch)?;
    fetcher.fetch(&source, &remote)?;
    let present =
        |sha: &str| git_ok(&source, &["cat-file", "-e", &format!("{sha}^{{commit}}")]).is_some();
    if fetcher.enabled() {
        git(&source, &["fetch", "--quiet", &remote, &pull])?;
    } else if !present(&head) {
        bail!(
            "{consumer}#{n}: its head {} is not in {} yet; rerun without --no-fetch",
            &head[..12.min(head.len())],
            source.display()
        );
    }
    let file = format!("{head}:{MANIFEST_FILE}");
    let manifest = if git_ok(&source, &["cat-file", "-e", &file]).is_some() {
        Manifest::parse(&git(&source, &["show", &file])?)
            .with_context(|| format!("{consumer}#{n}: parsing {MANIFEST_FILE}"))?
    } else {
        Manifest::default()
    };
    Ok(ConsumerMr {
        branch,
        head,
        manifest,
    })
}

/// The checkouts for a consumer MR: the consumer on its branch, and each
/// manifest entry's repo on its open MR's branch, or detached at the merge
/// commit of a merged one.
pub fn from_items(consumer: &str, mr: &ConsumerMr, reader: &dyn PrReader) -> Result<Vec<Item>> {
    let mut items = vec![Item {
        name: consumer.to_string(),
        target: Target::Branch(mr.branch.clone()),
        note: None,
    }];
    for (path, entry) in &mr.manifest.inputs {
        let which = format!("{path}: {}#{}", entry.repo, entry.mr);
        let pr = reader
            .pr(&entry.repo, entry.mr)
            .with_context(|| format!("{which}: reading the MR"))?
            .ok_or_else(|| anyhow!("{which}: {}", reader.not_found()))?;
        let (target, note) = match pr.state {
            PrState::Open if !pr.head_ref.is_empty() && !pr.head_ref.starts_with("refs/") => {
                (Target::Branch(pr.head_ref.clone()), None)
            }
            PrState::Open => bail!("{which}: the MR is open but names no head branch"),
            PrState::Merged { merge_commit } => {
                let note = format!(
                    "{which} is merged; detached at its merge commit {}",
                    short(&merge_commit)
                );
                (Target::Detached(merge_commit), Some(note))
            }
            PrState::ClosedUnmerged => bail!("{which}: the MR was closed without merging"),
        };
        let name = entry_checkout(&entry.repo, path);
        match items.iter().find(|i| i.name == name) {
            Some(i) if i.target == target => continue,
            Some(i) => bail!(
                "{which}: the checkout {name} is wanted at {:?} and at {target:?}",
                i.target
            ),
            None => items.push(Item { name, target, note }),
        }
    }
    Ok(items)
}

/// The checkouts of `swl start <name> <checkout>...`, all on `branch`.
pub fn new_items(config: &Config, checkouts: &[String], branch: &str) -> Result<Vec<Item>> {
    let mut items: Vec<Item> = Vec::new();
    for name in checkouts {
        config.repo(&ws_add::checkout_repo(name)?)?;
        if items.iter().any(|i| &i.name == name) {
            bail!("{name} is named twice");
        }
        items.push(Item {
            name: name.clone(),
            target: Target::Branch(branch.to_string()),
            note: None,
        });
    }
    if !items
        .iter()
        .any(|i| parse_shared_name(&i.name).is_ok_and(|s| s.is_none()))
    {
        bail!("name at least one consumer checkout (a repo without @, e.g. swan-lake)");
    }
    Ok(items)
}

/// Where an existing checkout is: its branch, or `detached at <sha>`.
fn describe_head(dir: &Path) -> Result<(Option<String>, String)> {
    let branch = git_ok(dir, &["symbolic-ref", "--quiet", "--short", "HEAD"]);
    let head = git(dir, &["rev-parse", "HEAD"])?;
    Ok((branch, head))
}

/// The canonical git common directory of the repository at `dir`.
pub fn common_dir(dir: &Path) -> Option<PathBuf> {
    let common = git_ok(dir, &["rev-parse", "--git-common-dir"])?;
    dir.join(common).canonicalize().ok()
}

/// Keeps an existing checkout and says where it is against `target`. It must
/// be a worktree of the repo's configured `source`.
fn keep(dest: &Path, source: &Path, target: &Target) -> Result<String> {
    let top = git(dest, &["rev-parse", "--show-toplevel"])
        .ok()
        .map(PathBuf::from)
        .and_then(|t| t.canonicalize().ok());
    if top.as_deref() != Some(dest) {
        bail!("{} exists but is not a git checkout", dest.display());
    }
    if common_dir(dest).is_none() || common_dir(dest) != common_dir(source) {
        bail!(
            "{} exists but is not a worktree of {}; move it away",
            dest.display(),
            source.display()
        );
    }
    let (branch, head) = describe_head(dest)?;
    let at = short(&head);
    Ok(match (target, branch) {
        (Target::Branch(b), Some(cur)) if *b == cur => format!("kept, on {cur}"),
        (Target::Branch(b), Some(cur)) => format!("kept, on {cur}, not {b}; not changed"),
        (Target::Branch(b), None) => format!("kept, detached at {at}, not on {b}; not changed"),
        (Target::Detached(rev), _) if *rev == head => format!("kept, detached at {at}"),
        (Target::Detached(rev), branch) => format!(
            "kept, at {} {at}, not {}; not changed",
            branch.as_deref().unwrap_or("detached"),
            short(rev)
        ),
    })
}

/// `direnv allow` for each consumer that has an `.envrc`, or a note when
/// direnv is not on PATH.
fn direnv_allow(consumers: &[PathBuf]) {
    let consumers: Vec<&PathBuf> = consumers
        .iter()
        .filter(|d| d.join(".envrc").is_file())
        .collect();
    if consumers.is_empty() {
        return;
    }
    let Ok(direnv) = find_real_tool("direnv", None) else {
        let dirs: Vec<String> = consumers.iter().map(|d| d.display().to_string()).collect();
        println!(
            "direnv is not on PATH; run `direnv allow` in {}",
            dirs.join(", ")
        );
        return;
    };
    for dir in consumers {
        let status = crate::logging::status(
            Command::new(&direnv)
                .arg("allow")
                .arg(dir)
                .stdin(Stdio::null()),
        );
        match status {
            Ok(s) if s.success() => println!("direnv allow {}", dir.display()),
            Ok(s) => style::tell(
                "swl start",
                Tone::Warn,
                format!("warning: direnv allow {} {s}", dir.display()),
            ),
            Err(e) => style::tell(
                "swl start",
                Tone::Warn,
                format!("warning: running direnv: {e}"),
            ),
        }
    }
}

/// A workspace that `swl start` set up.
#[derive(Debug)]
pub struct Started {
    pub root: PathBuf,
    /// The first consumer's checkout, which the `cd` hint names.
    pub cd: Option<PathBuf>,
}

/// Adds the missing checkouts, keeps the existing ones, then `swl env` and
/// `direnv allow`.
pub fn execute(
    config: &Config,
    root: &Path,
    items: &[Item],
    fetcher: &Fetcher,
    swl: &Path,
) -> Result<Started> {
    std::fs::create_dir_all(root).with_context(|| format!("creating {}", root.display()))?;
    let root = root.canonicalize()?;
    let report = Report::new("swl start");
    let mut summary = Vec::new();
    for item in items {
        let dest = root.join(&item.name);
        if dest.exists() {
            let source = &config.repo(&ws_add::checkout_repo(&item.name)?)?.source;
            let how = keep(&dest, source, &item.target)?;
            println!("{}: {how}", dest.display());
        } else {
            let start = match &item.target {
                Target::Branch(b) => Start::Branch(b),
                Target::Detached(rev) => Start::Detached(rev),
            };
            ws_add::add(config, &root, &item.name, start, fetcher, &report)?;
            if let Some(note) = &item.note {
                println!("  {note}");
            }
        }
        let (branch, head) = describe_head(&dest)?;
        let at = branch.unwrap_or_else(|| format!("detached at {}", short(&head)));
        summary.push((item.name.as_str(), at));
    }
    ws_add::env_after_add(config, &root, swl, "the checkouts were")?;
    let consumers: Vec<PathBuf> = items
        .iter()
        .filter(|i| parse_shared_name(&i.name).is_ok_and(|s| s.is_none()))
        .map(|i| root.join(&i.name))
        .collect();
    direnv_allow(&consumers);
    let name_w = summary.iter().map(|(n, _)| width(n)).max().unwrap_or(0);
    let children: Vec<String> = summary
        .iter()
        .map(|(n, at)| format!("{}{}  {at}", paint(BOLD, n), pad(name_w, width(n))))
        .collect();
    let mut stdout = anstream::stdout();
    let _ = writeln!(stdout);
    for line in report::tree(format!("workspace {}:", root.display()), &children) {
        let _ = writeln!(stdout, "{line}");
    }
    let cd = consumers.into_iter().next();
    if let Some(first) = &cd {
        println!("cd {}", first.display());
    }
    Ok(Started { root, cd })
}

/// `swl start <name> <checkout>...`.
#[allow(clippy::too_many_arguments)]
pub fn run_new(
    config: &Config,
    cwd: &Path,
    name: &str,
    checkouts: &[String],
    workspace: Option<&Path>,
    branch: Option<&str>,
    fetcher: &Fetcher,
    swl: &Path,
) -> Result<Started> {
    if name.is_empty() || name.contains('/') || name.starts_with('.') {
        bail!("{name:?} is not a workspace name, e.g. SL-0060-foo");
    }
    let branch = branch.map_or_else(|| name.to_lowercase(), str::to_string);
    let items = new_items(config, checkouts, &branch)?;
    let root = crate::workspace_location::for_start(config, cwd, name, workspace)?;
    execute(config, &root, &items, fetcher, swl)
}

/// `swl start --from <consumer>#<MR>`.
pub fn run_from(
    config: &Config,
    cwd: &Path,
    from: &str,
    workspace: Option<&Path>,
    fetcher: &Fetcher,
    reader: &dyn PrReader,
    swl: &Path,
) -> Result<Started> {
    let (consumer, n) = parse_from(from)?;
    let mr = read_consumer_mr(config, &consumer, n, fetcher)?;
    println!(
        "{consumer}#{n}: branch {} at {}",
        mr.branch,
        short(&mr.head)
    );
    let items = from_items(&consumer, &mr, reader)?;
    for item in &items[1..] {
        config.repo(&ws_add::checkout_repo(&item.name)?)?;
    }
    let root = crate::workspace_location::for_start(config, cwd, &mr.branch, workspace)?;
    execute(config, &root, &items, fetcher, swl)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::forge::{CiState, PrInfo};

    fn refs(lines: &[(&str, &str)]) -> Vec<(String, String)> {
        lines
            .iter()
            .map(|(s, r)| (s.to_string(), r.to_string()))
            .collect()
    }

    #[test]
    fn from_spec() {
        assert_eq!(
            parse_from("swan-lake#12").unwrap(),
            ("swan-lake".into(), 12)
        );
        for bad in [
            "swan-lake",
            "swan-lake#",
            "#3",
            "swan-lake#0",
            "a@b#1",
            "x#y",
        ] {
            assert!(parse_from(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_branch_of_an_mr_is_the_one_at_its_head() {
        let text = "aaa\trefs/heads/master\nbbb\trefs/heads/sl-1-x\nbbb\trefs/pull/3/head\nccc\trefs/pull/2/head\n";
        let r = parse_ls_remote(text);
        assert_eq!(
            mr_branch(&r, "swan-lake", 3, "master").unwrap(),
            ("bbb".into(), "sl-1-x".into())
        );
        let e = mr_branch(&r, "swan-lake", 2, "master")
            .unwrap_err()
            .to_string();
        assert!(e.contains("not the head of any branch"), "{e}");
        let e = mr_branch(&r, "swan-lake", 9, "master")
            .unwrap_err()
            .to_string();
        assert!(e.contains("no such MR"), "{e}");
        let two = refs(&[
            ("bbb", "refs/heads/a"),
            ("bbb", "refs/heads/b"),
            ("bbb", "refs/pull/3/head"),
        ]);
        let e = mr_branch(&two, "swan-lake", 3, "master")
            .unwrap_err()
            .to_string();
        assert!(e.contains("several branches (a, b)"), "{e}");
        let merged = refs(&[
            ("aaa", "refs/heads/master"),
            ("aaa", "refs/heads/sl-1-x"),
            ("aaa", "refs/pull/4/head"),
            ("bbb", "refs/pull/5/head"),
            ("bbb", "refs/heads/master"),
        ]);
        // A branch at master's head is still joinable; master itself is not.
        assert_eq!(
            mr_branch(&merged, "swan-lake", 4, "master").unwrap().1,
            "sl-1-x"
        );
        let e = mr_branch(&merged, "swan-lake", 5, "master")
            .unwrap_err()
            .to_string();
        assert!(
            e.ends_with("is the head of master: the MR is merged; nothing to join"),
            "{e}"
        );
    }

    #[test]
    fn entry_checkout_names() {
        assert_eq!(
            entry_checkout("swan-lake-common", "common"),
            "swan-lake-common@common"
        );
        assert_eq!(
            entry_checkout("zwanenzang", "common/zwanenzang"),
            "zwanenzang@zwanenzang"
        );
    }

    struct Reader;

    impl PrReader for Reader {
        fn pr(&self, _: &str, number: u64) -> Result<Option<PrInfo>> {
            let (state, head_ref) = match number {
                1 => (PrState::Open, "feature"),
                2 => (
                    PrState::Merged {
                        merge_commit: "m2".into(),
                    },
                    "refs/pull/2/head",
                ),
                3 => (PrState::ClosedUnmerged, "x"),
                _ => return Ok(None),
            };
            Ok(Some(PrInfo {
                number,
                state,
                head_sha: "h".into(),
                head_ref: head_ref.into(),
                title: String::new(),
                draft: false,
                base_ref: None,
                head_repo: None,
            }))
        }

        fn commit_status(&self, _: &str, _: &str) -> Result<Option<CiState>> {
            Ok(None)
        }
    }

    fn consumer_mr(manifest: &str) -> ConsumerMr {
        ConsumerMr {
            branch: "sl-1-x".into(),
            head: "h".into(),
            manifest: Manifest::parse(manifest).unwrap(),
        }
    }

    #[test]
    fn items_from_a_consumer_mr() {
        let mr = consumer_mr(
            "[inputs.common]\nrepo = \"swan-lake-common\"\nmr = 1\n\
             [inputs.\"common/zwanenzang\"]\nrepo = \"zwanenzang\"\nmr = 2\n",
        );
        let items = from_items("swan-lake", &mr, &Reader).unwrap();
        let got: Vec<(&str, &Target)> =
            items.iter().map(|i| (i.name.as_str(), &i.target)).collect();
        assert_eq!(
            got,
            [
                ("swan-lake", &Target::Branch("sl-1-x".into())),
                ("swan-lake-common@common", &Target::Branch("feature".into())),
                ("zwanenzang@zwanenzang", &Target::Detached("m2".into())),
            ]
        );
        assert!(items[2].note.as_deref().unwrap().contains("merged"));
        let closed = consumer_mr("[inputs.common]\nrepo = \"swan-lake-common\"\nmr = 3\n");
        let e = from_items("swan-lake", &closed, &Reader).unwrap_err();
        assert!(e.to_string().contains("closed without merging"), "{e}");
        let missing = consumer_mr("[inputs.common]\nrepo = \"swan-lake-common\"\nmr = 9\n");
        assert!(from_items("swan-lake", &missing, &Reader).is_err());
    }

    #[test]
    fn new_items_need_a_consumer() {
        let config = Config::parse(
            "[repos.swan-lake]\nsource = \"/s\"\n[repos.swan-lake-common]\nsource = \"/c\"\n",
        )
        .unwrap();
        let names = |n: &[&str]| n.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let items = new_items(
            &config,
            &names(&["swan-lake", "swan-lake-common@common"]),
            "b",
        )
        .unwrap();
        assert_eq!(items.len(), 2);
        assert!(new_items(&config, &names(&["swan-lake-common@common"]), "b").is_err());
        assert!(new_items(&config, &names(&["swan-lake", "swan-lake"]), "b").is_err());
        assert!(new_items(&config, &names(&["swan-lake", "unknown@x"]), "b").is_err());
    }
}

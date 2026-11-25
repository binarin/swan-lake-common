//! `swl publish <consumer>`: push the workspace's changed shared checkouts
//! fast-forward only, find or open their MRs as drafts, record them in the
//! consumer's `swl.toml`, then push the consumer branch and find or open its
//! MR. Rerunnable: what is already current is left alone.

use std::collections::BTreeMap;

use anyhow::{Result, anyhow, bail};

use crate::{
    config::Config,
    forge::PrPublisher,
    git::{git, git_ok},
    lock::Lock,
    manifest::{Entry, MANIFEST_FILE, Manifest},
    workspace::{Checkout, Workspace},
};

/// A branch to bring to origin, checked before anything is pushed.
struct Branch {
    name: String,
    head: String,
    /// The base branch on the forge, e.g. `master`.
    base_branch: String,
    /// The local ref of the base, e.g. `origin/master`.
    base_ref: String,
    /// The branch on origin is already at `head`.
    pushed: bool,
}

/// The base branch name for MRs from a base revision such as `origin/master`.
fn base_branch(base: &str) -> &str {
    base.strip_prefix("origin/").unwrap_or(base)
}

/// Brings the remote-tracking ref of an `origin/<branch>` base up to date, so
/// that the "already in the base" checks see merged work.
fn fetch_base(c: &Checkout, base: &str) -> Result<()> {
    if let Some(branch) = base.strip_prefix("origin/") {
        let refspec = format!("+refs/heads/{branch}:refs/remotes/origin/{branch}");
        git(
            &c.dir,
            &["fetch", "--quiet", "--no-tags", "origin", &refspec],
        )?;
    }
    Ok(())
}

/// The checkout's branch, refused when its state cannot be published as is:
/// a detached HEAD, the base branch itself, tracked changes, or a branch on
/// origin that HEAD does not contain (no force-push, ever).
fn publishable(c: &Checkout, base: &str) -> Result<Branch> {
    let dir = &c.dir;
    let Some(name) = git_ok(dir, &["symbolic-ref", "--quiet", "--short", "HEAD"]) else {
        bail!(
            "{}: HEAD is detached; check out a branch to publish",
            c.name
        );
    };
    if name == base_branch(base) {
        bail!(
            "{}: {name} is the base branch; check out a topic branch, swl publish does not push \
             the base branch",
            c.name
        );
    }
    let dirty = git(dir, &["status", "--porcelain", "--untracked-files=no"])?;
    if !dirty.is_empty() {
        bail!(
            "{}: uncommitted changes to tracked files; commit or stash them first:\n{dirty}",
            c.name
        );
    }
    let head = git(dir, &["rev-parse", "HEAD"])?;
    let remote = git(dir, &["ls-remote", "origin", &format!("refs/heads/{name}")])?;
    let remote = remote.split_whitespace().next().map(str::to_string);
    let pushed = remote.as_deref() == Some(head.as_str());
    if let Some(r) = &remote
        && !pushed
        && git_ok(dir, &["merge-base", "--is-ancestor", r, "HEAD"]).is_none()
    {
        bail!(
            "{}: origin's {name} is at {}, which HEAD does not contain; a push would not be a \
             fast-forward (swl never force-pushes): fetch and rebase or merge first",
            c.name,
            crate::git::short(r)
        );
    }
    Ok(Branch {
        name,
        head,
        base_branch: base_branch(base).to_string(),
        base_ref: base.to_string(),
        pushed,
    })
}

fn push(c: &Checkout, b: &Branch) -> Result<()> {
    if b.pushed {
        println!("{}: {} is pushed", c.name, b.name);
        return Ok(());
    }
    let refspec = format!("refs/heads/{0}:refs/heads/{0}", b.name);
    git(&c.dir, &["push", "--quiet", "origin", &refspec])?;
    println!(
        "{}: pushed {} ({})",
        c.name,
        b.name,
        crate::git::short(&b.head)
    );
    Ok(())
}

/// The MR title and body from the branch's commits since its base.
fn title_and_body(c: &Checkout, b: &Branch, extra: &str) -> Result<(String, String)> {
    let range = format!("{}..HEAD", b.base_ref);
    let subjects = git(&c.dir, &["log", "--reverse", "--format=%s", &range])?;
    let subjects: Vec<&str> = subjects.lines().collect();
    let title = subjects
        .iter()
        .find(|s| !s.starts_with("swl: manifest for "))
        .or(subjects.first())
        .map(|s| s.to_string())
        .unwrap_or_else(|| b.name.clone());
    let mut body = String::new();
    for s in &subjects {
        body.push_str(&format!("- {s}\n"));
    }
    body.push_str(extra);
    body.push_str("\nOpened by `swl publish`.\n");
    Ok((title, body))
}

fn find_or_open(
    forge: &dyn PrPublisher,
    repo: &str,
    c: &Checkout,
    b: &Branch,
    draft: bool,
    extra: &str,
) -> Result<u64> {
    if let Some(n) = forge.find_open(repo, &b.name)? {
        println!("{}: MR {repo}#{n} is open for {}", c.name, b.name);
        return Ok(n);
    }
    let (title, body) = title_and_body(c, b, extra)?;
    let title = if draft {
        format!("WIP: {title}")
    } else {
        title
    };
    let n = forge.create(repo, &b.base_branch, &b.name, &title, &body)?;
    println!("{}: opened MR {repo}#{n}: {title}", c.name);
    Ok(n)
}

pub fn run(ws: &Workspace, config: &Config, consumer: &str, forge: &dyn PrPublisher) -> Result<()> {
    let target = ws
        .checkouts
        .iter()
        .find(|c| c.name == consumer && c.is_consumer())
        .ok_or_else(|| anyhow!("no consumer checkout {consumer:?} in {}", ws.root.display()))?;
    let lock = Lock::read(&target.dir)?;
    // input paths per shared checkout whose HEAD differs from the consumer's lock.
    let mut changed: BTreeMap<&str, (&Checkout, Vec<String>)> = BTreeMap::new();
    for o in ws.overrides_for(config, target)? {
        let checkout = ws
            .checkouts
            .iter()
            .find(|c| c.name == o.checkout)
            .expect("overrides name workspace checkouts");
        let head = git(&checkout.dir, &["rev-parse", "HEAD"])?;
        let path: Vec<String> = o.input_path.split('/').map(str::to_string).collect();
        if lock.as_ref().and_then(|l| l.locked_rev(&path)).as_deref() == Some(head.as_str()) {
            println!(
                "{}: at the rev {consumer} locks for {}",
                checkout.name, o.input_path
            );
            continue;
        }
        changed
            .entry(checkout.name.as_str())
            .or_insert_with(|| (checkout, Vec::new()))
            .1
            .push(o.input_path);
    }

    // Check everything before pushing anything.
    let mut shared = Vec::new();
    for (checkout, paths) in changed.into_values() {
        let (repo, _) = checkout
            .shared
            .as_ref()
            .expect("overrides come from shared checkouts");
        let base = config.base_for(repo);
        fetch_base(checkout, base)?;
        let head = git(&checkout.dir, &["rev-parse", "HEAD"])?;
        if git_ok(&checkout.dir, &["merge-base", "--is-ancestor", &head, base]).is_some() {
            println!(
                "{}: HEAD is already in {base}; that is a lock bump, not an MR",
                checkout.name
            );
            continue;
        }
        let branch = publishable(checkout, base)?;
        shared.push((checkout, repo.as_str(), paths, branch));
    }
    fetch_base(target, config.base_for(consumer))?;
    publishable(target, config.base_for(consumer))?;
    if let Some(e) = crate::check::lock_change(&target.dir, config.base_for(consumer))? {
        bail!("{consumer}: {e}");
    }

    let mut manifest = Manifest::read(&target.dir)?.unwrap_or_default();
    let mut depends = String::new();
    for (checkout, repo, paths, branch) in &shared {
        push(checkout, branch)?;
        let extra = format!("\nUsed by {consumer} as {}.\n", paths.join(", "));
        let mr = find_or_open(forge, repo, checkout, branch, true, &extra)?;
        for p in paths {
            depends.push_str(&format!("- {repo}#{mr} ({p})\n"));
            manifest.inputs.insert(
                p.clone(),
                Entry {
                    repo: repo.to_string(),
                    mr,
                },
            );
        }
    }
    // Only published entries are written; an existing file is otherwise left
    // as it is.
    if !shared.is_empty() && manifest.write(&target.dir)? {
        let names: Vec<String> = manifest
            .inputs
            .values()
            .map(|e| format!("{}#{}", e.repo, e.mr))
            .collect();
        git(&target.dir, &["add", "--", MANIFEST_FILE])?;
        git(
            &target.dir,
            &[
                "commit",
                "--quiet",
                "-m",
                &format!("swl: manifest for {}", names.join(", ")),
                "--",
                MANIFEST_FILE,
            ],
        )?;
        println!(
            "{consumer}: committed {MANIFEST_FILE}: {}",
            names.join(", ")
        );
    } else if !manifest.is_empty() {
        println!("{consumer}: {MANIFEST_FILE} is current");
    }

    // Again, now that the manifest commit may be on top.
    let consumer_branch = publishable(target, config.base_for(consumer))?;
    if git_ok(
        &target.dir,
        &[
            "merge-base",
            "--is-ancestor",
            "HEAD",
            &consumer_branch.base_ref,
        ],
    )
    .is_some()
    {
        println!(
            "{consumer}: HEAD is already in {}; no consumer MR to open",
            consumer_branch.base_ref
        );
        return Ok(());
    }
    push(target, &consumer_branch)?;
    let extra = if depends.is_empty() {
        String::new()
    } else {
        format!("\nDepends on (see {MANIFEST_FILE}):\n{depends}")
    };
    find_or_open(forge, consumer, target, &consumer_branch, false, &extra)?;
    Ok(())
}

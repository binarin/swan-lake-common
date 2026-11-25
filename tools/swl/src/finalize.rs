//! `swl finalize`: one local commit on the consumer's branch that relocks
//! the inputs whose manifest entries have landed and removes those entries.
//! It pushes nothing.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    process::ExitCode,
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use serde_json::Value;

use crate::{
    ancestry,
    forge::{PrReader, PrState},
    git::{git, git_ok, short},
    lock_change,
    manifest::{Entry, MANIFEST_FILE, Manifest},
    nix::Nix,
    pin_policy,
    tempdir::TempDir,
};

pub struct Wait {
    pub timeout: Duration,
    pub interval: Duration,
}

/// Where one manifest entry stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    /// Not merged yet, or not usable: stays, with the reason.
    Pending(String),
    /// Merged as `merge_commit`, but the input's flake.nix URL does not
    /// resolve to a revision containing it yet (promotion or mirror pending).
    NotYetReachable {
        merge_commit: String,
        reason: String,
    },
    /// Merged, and the relocked input contains the merged commit.
    Satisfied { merge_commit: String, rev: String },
}

impl Status {
    fn merged(&self) -> bool {
        !matches!(self, Status::Pending(_))
    }
}

/// The root input that relocking an input path goes through: nested pins
/// come from the parent's lock.
fn root_of(path: &str) -> &str {
    path.split('/').next().unwrap_or(path)
}

/// The root inputs to relock: those with at least one satisfied entry.
pub fn roots_to_relock(statuses: &BTreeMap<String, Status>) -> Vec<String> {
    let roots: BTreeSet<&str> = statuses
        .iter()
        .filter(|(_, s)| matches!(s, Status::Satisfied { .. }))
        .map(|(p, _)| root_of(p))
        .collect();
    roots.into_iter().map(str::to_string).collect()
}

/// Whether the entry at `path`, merged as `merge_commit`, is in what the
/// relocked `lock` pins there.
fn reachability(lock: &Value, path: &str, merge_commit: &str) -> Status {
    let not_yet = |reason: String| Status::NotYetReachable {
        merge_commit: merge_commit.to_string(),
        reason,
    };
    let locked = match lock_change::resolve_path(lock, path) {
        Ok(key) => lock.pointer(&format!("/nodes/{key}/locked")).cloned(),
        Err(e) => return not_yet(format!("{e:#}")),
    };
    let Some(locked) = locked else {
        return not_yet(format!("{path} has no locked source"));
    };
    let rev = locked.get("rev").and_then(Value::as_str).unwrap_or("");
    let Some(url) = ancestry::git_url(&locked) else {
        return not_yet(format!("{path} is not a git source"));
    };
    match ancestry::is_ancestor(&url, merge_commit, rev, false) {
        Ok(true) => Status::Satisfied {
            merge_commit: merge_commit.to_string(),
            rev: rev.to_string(),
        },
        Ok(false) => not_yet(format!(
            "{} is not in {} yet, which {path} resolves to (promotion or the mirror is pending)",
            short(merge_commit),
            short(rev)
        )),
        Err(reason) => not_yet(format!(
            "cannot determine whether {} is in {}: {reason}",
            short(merge_commit),
            short(rev)
        )),
    }
}

/// The MR state of every entry; merged ones start as not reachable.
fn mr_statuses(manifest: &Manifest, api: &dyn PrReader) -> BTreeMap<String, Status> {
    manifest
        .inputs
        .iter()
        .map(|(path, Entry { repo, mr })| {
            let status = match api.pr(repo, *mr) {
                Err(e) => Status::Pending(format!("cannot read the MR state: {e:#}")),
                Ok(None) => Status::Pending(crate::forge::NOT_FOUND.into()),
                Ok(Some(pr)) => match pr.state {
                    PrState::Open => Status::Pending("the MR is still open".into()),
                    PrState::ClosedUnmerged => {
                        Status::Pending("the MR was closed without merging".into())
                    }
                    PrState::Merged { merge_commit } => Status::NotYetReachable {
                        merge_commit,
                        reason: "not checked".into(),
                    },
                },
            };
            (path.clone(), status)
        })
        .collect()
}

/// The new lock (bytes as nix wrote them) after relocking `roots`.
fn relock(nix: &Nix, root: &Path, roots: &[String], old: &[u8]) -> Result<Vec<u8>> {
    let tmp = TempDir::new("swl-finalize")?;
    let out = tmp.path().join("flake.lock");
    nix.update_to(root, roots, &out)?;
    // Nix writes no output lock when nothing changed.
    match fs::read(&out) {
        Ok(bytes) => Ok(bytes),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(old.to_vec()),
        Err(e) => Err(e).context("reading the relocked flake.lock"),
    }
}

struct Probe {
    statuses: BTreeMap<String, Status>,
    /// The relocked lock and the roots relocked, when an entry is satisfied.
    new_lock: Option<(Vec<u8>, Vec<String>)>,
}

/// Asks the forge, relocks the merged entries' root inputs, and keeps the
/// relock of only the roots with a satisfied entry.
fn probe(
    root: &Path,
    manifest: &Manifest,
    api: &dyn PrReader,
    nix: &Nix,
    old: &[u8],
) -> Result<Probe> {
    let mut statuses = mr_statuses(manifest, api);
    let merged_roots: BTreeSet<String> = statuses
        .iter()
        .filter(|(_, s)| s.merged())
        .map(|(p, _)| root_of(p).to_string())
        .collect();
    if merged_roots.is_empty() {
        return Ok(Probe {
            statuses,
            new_lock: None,
        });
    }
    let merged_roots: Vec<String> = merged_roots.into_iter().collect();
    let mut bytes = relock(nix, root, &merged_roots, old)?;
    let check = |statuses: &mut BTreeMap<String, Status>, bytes: &[u8]| -> Result<()> {
        let lock: Value = serde_json::from_slice(bytes).context("parsing the relocked lock")?;
        for (path, status) in statuses.iter_mut() {
            if let Status::NotYetReachable { merge_commit, .. }
            | Status::Satisfied { merge_commit, .. } = status.clone()
            {
                *status = reachability(&lock, path, &merge_commit);
            }
        }
        Ok(())
    };
    check(&mut statuses, &bytes)?;
    let roots = roots_to_relock(&statuses);
    if roots.is_empty() {
        return Ok(Probe {
            statuses,
            new_lock: None,
        });
    }
    if roots != merged_roots {
        // Relock only the roots that have landed, and recheck on that lock.
        bytes = relock(nix, root, &roots, old)?;
        check(&mut statuses, &bytes)?;
    }
    let roots = roots_to_relock(&statuses);
    Ok(Probe {
        statuses,
        new_lock: (!roots.is_empty()).then_some((bytes, roots)),
    })
}

fn describe(status: &Status) -> String {
    match status {
        Status::Pending(why) => format!("stays: {why}"),
        Status::NotYetReachable {
            merge_commit,
            reason,
        } => format!("stays: merged as {}, but {reason}", short(merge_commit)),
        Status::Satisfied { merge_commit, rev } => {
            format!(
                "lands: merged as {}, relocked to {}",
                short(merge_commit),
                short(rev)
            )
        }
    }
}

fn locked_rev(lock: &Value, input: &str) -> Option<String> {
    let key = lock_change::resolve_path(lock, input).ok()?;
    lock.pointer(&format!("/nodes/{key}/locked/rev"))?
        .as_str()
        .map(str::to_string)
}

/// A consumer repo ready to finalize: on a branch, with no uncommitted
/// changes to the flake files, and its manifest.
struct Prepared {
    root: PathBuf,
    branch: String,
    manifest: Manifest,
}

/// `None` when the repo has no manifest. Refuses a detached HEAD and
/// uncommitted changes to flake.nix, flake.lock or swl.toml.
fn prepare(dir: &Path) -> Result<(PathBuf, Option<Prepared>)> {
    let root = PathBuf::from(git(dir, &["rev-parse", "--show-toplevel"])?);
    let Some(manifest) = Manifest::read(&root)? else {
        return Ok((root, None));
    };
    let Some(branch) = git_ok(&root, &["symbolic-ref", "--quiet", "--short", "HEAD"]) else {
        bail!(
            "{}: HEAD is detached; check out the MR's branch",
            root.display()
        );
    };
    let dirty = git(
        &root,
        &[
            "status",
            "--porcelain",
            "--",
            "flake.nix",
            "flake.lock",
            MANIFEST_FILE,
        ],
    )?;
    if !dirty.is_empty() {
        bail!(
            "commit or discard the changes to flake.nix, flake.lock and {MANIFEST_FILE} first:\n{dirty}"
        );
    }
    Ok((
        root.clone(),
        Some(Prepared {
            root,
            branch,
            manifest,
        }),
    ))
}

/// The landing commit: what it lands and how many entries remain.
pub struct Committed {
    pub subject: String,
    /// The new HEAD.
    pub head: String,
    pub remaining: usize,
}

/// Checks the relock of `probe` (nothing else changed, the pin policy) and
/// commits it with the satisfied entries removed from the manifest.
fn commit(p: &mut Prepared, nix: &Nix, probe: &Probe, old_bytes: &[u8]) -> Result<Committed> {
    let root = &p.root;
    let Some((new_bytes, roots)) = &probe.new_lock else {
        bail!("nothing has landed");
    };
    let old: Value = serde_json::from_slice(old_bytes).context("parsing flake.lock")?;
    let new: Value = serde_json::from_slice(new_bytes)?;
    lock_change::verify(&old, &new, roots).context("the relock changed more than intended")?;
    let decls = nix.declarations(root)?;
    pin_policy::check(&decls, &new, &|url| nix.parse_flake_ref(url))
        .context("the relocked flake.lock breaks the common pin policy")?;

    let landed: Vec<(String, Entry)> = probe
        .statuses
        .iter()
        .filter(|(_, s)| matches!(s, Status::Satisfied { .. }))
        .map(|(path, _)| (path.clone(), p.manifest.inputs[path].clone()))
        .collect();
    for (path, _) in &landed {
        p.manifest.inputs.remove(path);
    }
    if new_bytes != old_bytes {
        fs::write(root.join("flake.lock"), new_bytes).context("writing flake.lock")?;
    }
    p.manifest.write(root)?;
    let subject = format!(
        "swl: land {}",
        landed
            .iter()
            .map(|(path, e)| format!("{}#{} ({path})", e.repo, e.mr))
            .collect::<Vec<_>>()
            .join(", ")
    );
    let mut body = String::new();
    for r in roots {
        let (a, b) = (locked_rev(&old, r), locked_rev(&new, r));
        body.push_str(&format!(
            "{r}: {} -> {}\n",
            a.as_deref().map(short).unwrap_or("(new)"),
            b.as_deref().map(short).unwrap_or("?")
        ));
    }
    git(root, &["add", "-A", "--", "flake.lock", MANIFEST_FILE])?;
    git(
        root,
        &[
            "commit",
            "--quiet",
            "-m",
            &subject,
            "-m",
            body.trim_end(),
            "--",
            "flake.lock",
            MANIFEST_FILE,
        ],
    )?;
    Ok(Committed {
        subject,
        head: git(root, &["rev-parse", "HEAD"])?,
        remaining: p.manifest.inputs.len(),
    })
}

/// What [`land_all`] did, for `swl land`.
pub enum Landing {
    /// No swl.toml: nothing to land.
    NoManifest,
    /// A swl.toml without entries, which should be removed.
    Empty,
    /// Not every entry is satisfied; one line per entry.
    Waiting(Vec<String>),
    /// With `dry_run`: every entry is satisfied, and would land.
    WouldLand(Vec<String>),
    Committed(Committed),
}

/// `finalize` for `swl land`: one commit only when every entry is satisfied,
/// so that no partial landing is pushed.
pub fn land_all(dir: &Path, api: &dyn PrReader, dry_run: bool) -> Result<Landing> {
    let (_, Some(mut p)) = prepare(dir)? else {
        return Ok(Landing::NoManifest);
    };
    if p.manifest.is_empty() {
        return Ok(Landing::Empty);
    }
    let nix = Nix::find()?;
    let old_bytes = fs::read(p.root.join("flake.lock")).context("reading flake.lock")?;
    let probe = probe(&p.root, &p.manifest, api, &nix, &old_bytes)?;
    let lines: Vec<String> = probe
        .statuses
        .iter()
        .map(|(path, status)| {
            let e = &p.manifest.inputs[path];
            format!("{path} ({}#{}): {}", e.repo, e.mr, describe(status))
        })
        .collect();
    if !probe
        .statuses
        .values()
        .all(|s| matches!(s, Status::Satisfied { .. }))
    {
        return Ok(Landing::Waiting(lines));
    }
    if dry_run {
        return Ok(Landing::WouldLand(lines));
    }
    commit(&mut p, &nix, &probe, &old_bytes).map(Landing::Committed)
}

pub fn run(dir: &Path, wait: Option<Wait>, api: &dyn PrReader) -> Result<ExitCode> {
    let (root, Some(mut p)) = prepare(dir)? else {
        let root = PathBuf::from(git(dir, &["rev-parse", "--show-toplevel"])?);
        println!(
            "swl finalize: no {MANIFEST_FILE} in {}: nothing to finalize",
            root.display()
        );
        return Ok(ExitCode::SUCCESS);
    };
    if p.manifest.is_empty() {
        println!(
            "swl finalize: {MANIFEST_FILE} has no entries; remove it (git rm {MANIFEST_FILE}) before landing"
        );
        return Ok(ExitCode::from(1));
    }
    let nix = Nix::find()?;
    let old_bytes = fs::read(root.join("flake.lock")).context("reading flake.lock")?;
    let deadline = wait.as_ref().map(|w| Instant::now() + w.timeout);
    let probe = loop {
        let probe = probe(&root, &p.manifest, api, &nix, &old_bytes)?;
        let waiting_for = probe
            .statuses
            .iter()
            .filter(|(_, s)| matches!(s, Status::NotYetReachable { .. }))
            .count();
        match (&wait, deadline) {
            (Some(w), Some(d)) if waiting_for > 0 && Instant::now() + w.interval <= d => {
                println!(
                    "swl finalize: {waiting_for} merged entr{} not promoted yet; checking again in {}s",
                    if waiting_for == 1 { "y" } else { "ies" },
                    w.interval.as_secs()
                );
                thread::sleep(w.interval);
            }
            _ => break probe,
        }
    };
    for (path, status) in &probe.statuses {
        let e = &p.manifest.inputs[path];
        println!(
            "swl finalize: {path} ({}#{}): {}",
            e.repo,
            e.mr,
            describe(status)
        );
    }
    if probe.new_lock.is_none() {
        println!("swl finalize: nothing has landed; no commit made");
        return Ok(ExitCode::from(1));
    }
    let done = commit(&mut p, &nix, &probe, &old_bytes)?;
    println!(
        "swl finalize: committed {} on {}: {}",
        short(&done.head),
        p.branch,
        done.subject
    );
    if done.remaining == 0 {
        println!(
            "Next (nothing was pushed): swl land pushes this commit to the MR branch and requests \
             the merge; or push it to {} yourself (fast-forward) and merge the MR, whose swl check \
             accepts the landing.",
            p.branch
        );
    } else {
        let n = done.remaining;
        println!(
            "{n} entr{} remain in {MANIFEST_FILE}. Nothing was pushed; pushed to the MR branch, \
             this partial landing passes swl check. Rerun swl finalize once the rest has landed.",
            if n == 1 { "y" } else { "ies" }
        );
    }
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relock_only_roots_with_a_landed_entry() {
        let sat = || Status::Satisfied {
            merge_commit: "m".into(),
            rev: "r".into(),
        };
        let not_yet = || Status::NotYetReachable {
            merge_commit: "m".into(),
            reason: "x".into(),
        };
        let statuses = BTreeMap::from([
            ("common".to_string(), sat()),
            ("common/zwanenzang".to_string(), not_yet()),
            ("common-next".to_string(), not_yet()),
            (
                "common-next-disko".to_string(),
                Status::Pending("open".into()),
            ),
        ]);
        assert_eq!(roots_to_relock(&statuses), ["common"]);
        let statuses = BTreeMap::from([("common/zwanenzang".to_string(), sat())]);
        assert_eq!(roots_to_relock(&statuses), ["common"]);
        let statuses = BTreeMap::from([("common".to_string(), not_yet())]);
        assert!(roots_to_relock(&statuses).is_empty());
    }

    #[test]
    fn reachability_reads_the_relocked_node() {
        let lock = serde_json::json!({"version": 7, "root": "root", "nodes": {
            "root": {"inputs": {"common": "c"}},
            "c": {"locked": {"type": "path", "path": "/x", "rev": "r"}},
        }});
        match reachability(&lock, "common", "m") {
            Status::NotYetReachable { reason, .. } => assert!(reason.contains("not a git source")),
            s => panic!("{s:?}"),
        }
        match reachability(&lock, "nope", "m") {
            Status::NotYetReachable { .. } => {}
            s => panic!("{s:?}"),
        }
    }
}

//! `swl refresh`, and the step of `swl land` that it shares: an open MR's
//! branch whose head does not contain its base's tip is rebased onto it, in
//! a scratch worktree, and pushed with `--force-with-lease` on the head that
//! was read. The workspace checkout of the branch, which must be clean, on
//! the branch and at that head, is then moved to the pushed head.
//!
//! Only the change's own MR branches are rewritten: never a base branch, a
//! promotion branch (`swan-lake-current…`) or swl bump's branch. A consumer
//! branch whose tip is swl's own landing commit, behind a base whose
//! `flake.lock` moved, is rebased without it and finalized anew before the
//! one push, so that no pushed head lacks a landing: the MR's scheduled
//! merge could merge such a head.

use std::{path::Path, process::ExitCode};

use anyhow::{Context, Result};

use crate::{
    bump::{BUMP_BRANCH, Scratch},
    config::Config,
    env,
    finalize::{self, Landing},
    forge::{PrInfo, PrReader},
    git::{git, git_ok, short, valid_branch},
    land::{
        self, DryRunWriter, Lander, Options, Rejection, Step, push_rejection, say_waits, stop,
        waits_code,
    },
    status::{self, MrFacts, MrRead, Row, RowFacts, Waits},
    style::{self, Tone},
    workspace::Workspace,
    ws_add::{Fetcher, base_remote},
};

/// Where a part's branch is rebased from.
pub(crate) enum Place<'a> {
    /// The workspace's checkout `dir` of the branch, in the workspace at
    /// `root`: it is moved to the pushed head.
    Checkout { dir: &'a Path, root: &'a Path },
    /// No checkout: only the scratch worktree.
    Scratch,
}

/// One MR branch of the change.
pub(crate) struct Part<'a> {
    /// `repo#n`.
    pub(crate) which: String,
    pub(crate) repo: &'a str,
    /// The open MR, as the forge has it.
    pub(crate) info: &'a PrInfo,
    pub(crate) place: Place<'a>,
    /// A consumer MR: swl's own landing commit at its tip is made anew on
    /// a base whose lock moved.
    pub(crate) consumer: bool,
}

/// Why swl never rewrites `branch`, if it does not: the base branch, a
/// promotion branch, or swl bump's own branch.
fn never_rewritten(branch: &str, base_branch: &str) -> Option<&'static str> {
    if branch == base_branch || branch == "master" || branch == "main" {
        Some("a base branch")
    } else if branch == "swan-lake-current" || branch.starts_with("swan-lake-current/") {
        Some("a promotion branch")
    } else if branch == BUMP_BRANCH {
        Some("swl bump's branch, which swl bump rebuilds")
    } else {
        None
    }
}

fn rev(dir: &Path, refname: &str) -> Result<String> {
    git(
        dir,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{refname}^{{commit}}"),
        ],
    )
    .with_context(|| format!("{refname} is not known in {}", dir.display()))
}

fn is_ancestor(dir: &Path, a: &str, b: &str) -> bool {
    git_ok(dir, &["merge-base", "--is-ancestor", a, b]).is_some()
}

/// Why the checkout at `dir` cannot be moved to a rebased `branch`, and
/// what to do: changed or untracked files (but those that removing a
/// landed checkout may delete, as swl's `.envrc.local`), another branch, or
/// a HEAD other than `observed`, the head of `tracking`.
fn checkout_problem(
    dir: &Path,
    branch: &str,
    observed: &str,
    tracking: &str,
) -> Result<Option<String>> {
    if let Some(changed) = changed_files(dir)? {
        return Ok(Some(format!(
            "it has uncommitted or untracked files: {changed}; commit or remove them"
        )));
    }
    let on = git_ok(dir, &["symbolic-ref", "--quiet", "--short", "HEAD"]);
    if on.as_deref() != Some(branch) {
        return Ok(Some(format!(
            "it is on {}, not {branch}; switch to {branch}",
            on.as_deref().unwrap_or("a detached HEAD")
        )));
    }
    let head = git(dir, &["rev-parse", "HEAD"])?;
    if head == observed {
        return Ok(None);
    }
    Ok(Some(if is_ancestor(dir, observed, &head) {
        format!(
            "{branch} has commits that {tracking} lacks; push them first (swl publish, or swl \
             land for a landing commit)"
        )
    } else if is_ancestor(dir, &head, observed) {
        format!("{tracking} has commits that {branch} lacks; git pull --ff-only first")
    } else {
        format!("{branch} and {tracking} have diverged; merge or rebase onto {tracking} first")
    }))
}

/// The changed and untracked files of the checkout at `dir`, as `git status
/// --porcelain` lines, but those that removing a landed checkout may
/// delete; `None` when there are none.
fn changed_files(dir: &Path) -> Result<Option<String>> {
    let status = git(dir, &["status", "--porcelain"])?;
    let changed: Vec<&str> = status
        .lines()
        .filter(|l| !(l.starts_with("?? ") && land::disposable(dir, &l[3..], true)))
        .collect();
    Ok((!changed.is_empty()).then(|| changed.join(", ")))
}

/// Moves the checkout at `dir` from `observed` to the pushed `head`,
/// checked again now: `branch` by compare-and-swap from `observed`, then,
/// when HEAD is that branch, the index and working tree (`git read-tree -m
/// -u`, which refuses to overwrite changes; the branch is moved back then).
/// Why not, with the commands to do it by hand.
fn move_checkout(dir: &Path, branch: &str, observed: &str, head: &str) -> Result<Option<String>> {
    let by_hand = format!(
        "git -C {d} switch {branch} && git -C {d} reset --keep {head}",
        d = dir.display()
    );
    let refname = format!("refs/heads/{branch}");
    let on = git_ok(dir, &["symbolic-ref", "--quiet", "HEAD"]);
    let checked_out = on.as_deref() == Some(refname.as_str());
    if checked_out && let Some(changed) = changed_files(dir)? {
        return Ok(Some(format!(
            "it has uncommitted or untracked files now: {changed}; once they are committed or \
             removed (commits go on top of the pushed head), {by_hand}"
        )));
    }
    let cas = ["update-ref", "-m", "swl refresh", &refname, head, observed];
    if git(dir, &cas).is_err() {
        let now = git_ok(dir, &["rev-parse", "--verify", "--quiet", &refname]);
        return Ok(Some(format!(
            "{branch} is at {} now, not {}; bring its new commits onto the pushed head: git -C {} \
             switch {branch} && git -C {} rebase {head}",
            now.as_deref().map_or("(none)", short),
            short(observed),
            dir.display(),
            dir.display()
        )));
    }
    if !checked_out {
        return Ok(Some(format!(
            "{branch} was moved to it, but the checkout is on {} now; git -C {} switch {branch}",
            on.as_deref()
                .and_then(|r| r.strip_prefix("refs/heads/"))
                .unwrap_or("a detached HEAD"),
            dir.display()
        )));
    }
    if let Err(e) = git(dir, &["read-tree", "-m", "-u", observed, head]) {
        let _ = git(dir, &["update-ref", &refname, observed, head]);
        return Ok(Some(format!(
            "its working tree cannot be updated ({e:#}); {by_hand}"
        )));
    }
    Ok(None)
}

/// Rebases the detached HEAD of `dir` onto `onto`, without the repo's
/// hooks; on a conflict aborts and says where: the commit and the files.
fn rebase(dir: &Path, onto: &str) -> Result<Option<String>> {
    let args = [
        "-c",
        "core.hooksPath=/dev/null",
        "rebase",
        "--quiet",
        "--no-autosquash",
        "--no-update-refs",
        onto,
    ];
    let Err(e) = git(dir, &args) else {
        return Ok(None);
    };
    let files = git_ok(dir, &["diff", "--name-only", "--diff-filter=U"]).unwrap_or_default();
    let commit = git_ok(dir, &["log", "-1", "--format=%h %s", "REBASE_HEAD"]);
    let _ = git(dir, &["rebase", "--abort"]);
    if files.is_empty() {
        return Err(e.context("rebasing"));
    }
    Ok(Some(format!(
        "{}: {}",
        commit.as_deref().unwrap_or("a commit"),
        files.lines().collect::<Vec<_>>().join(", ")
    )))
}

/// Whether `base_tip` changes `flake.lock` since its merge base with `head`.
fn lock_moved(dir: &Path, head: &str, base_tip: &str) -> Result<bool> {
    let mb = git(dir, &["merge-base", head, base_tip])?;
    Ok(!git(
        dir,
        &["diff", "--name-only", &mb, base_tip, "--", "flake.lock"],
    )?
    .is_empty())
}

/// Reruns `swl env` for the workspace at `root`, for checkouts whose
/// `flake.lock` moved; a failure is a warning.
fn rerun_env(cmd: &str, root: &Path, config: &Config) {
    let result = (|| -> Result<()> {
        let ws = Workspace::scan(root)?;
        env::run(&ws, config, &crate::current_exe()?, false)
    })();
    if let Err(e) = result {
        style::say(
            cmd,
            Tone::Warn,
            format!(
                "swl env failed after the rebase: {e:#}; fix that and run swl env --workspace {}",
                root.display()
            ),
        );
    }
}

/// Finalizes the scratch worktree's HEAD anew, on a temporary branch (which
/// `finalize` needs), without hooks; the new head, or why not.
fn refinalize(l: &Lander, s: &Scratch, source: &Path, which: &str) -> Result<String, String> {
    let tmp = format!("swl-refresh-{}", std::process::id());
    git(&s.dir, &["switch", "--quiet", "-C", &tmp]).map_err(|e| format!("{e:#}"))?;
    let landed = finalize::land_all(&s.dir, l.reader, false, false);
    let _ = git(&s.dir, &["switch", "--quiet", "--detach"]);
    let _ = git(source, &["branch", "--quiet", "-D", &tmp]);
    Err(match landed {
        Err(e) => format!("{e:#}"),
        Ok(Landing::Committed(done)) => {
            style::say(
                l.cmd,
                Tone::Plain,
                format!("{which}: finalized anew: {}", done.subject),
            );
            return Ok(done.head);
        }
        Ok(Landing::Waiting(lines)) => {
            format!("not every entry has landed:\n  {}", lines.join("\n  "))
        }
        Ok(Landing::NoManifest | Landing::Empty) => "it has no swl.toml entries".to_string(),
        Ok(Landing::WouldLand(_)) => unreachable!("not a dry run"),
    })
}

/// Rebases `part`'s branch onto its base's tip and pushes it with a lease,
/// when its head does not contain that tip: [`Step::Done`] when it does,
/// [`Step::Progress`] once pushed and the forge shows the new head.
pub(crate) fn refresh(l: &Lander, part: &Part) -> Result<Step> {
    let cmd = l.cmd;
    let which = &part.which;
    let branch = part.info.head_ref.as_str();
    let rc = l.config.repo(part.repo)?;
    let source = &rc.source;
    let base = rc.base();
    let base_branch = base.split_once('/').map_or(base, |(_, b)| b);
    let Some(remote) = base_remote(source, base)? else {
        return stop(
            Waits::Anyone,
            format!(
                "{which}: the base {base} names no remote of {}, so {branch} is not refreshed",
                source.display()
            ),
        );
    };
    if let Some(why) = never_rewritten(branch, base_branch) {
        return stop(
            Waits::Anyone,
            format!(
                "{which}: its head branch {branch} is {why}; swl rewrites only the change's own \
                 MR branches"
            ),
        );
    }
    if !valid_branch(branch) {
        return stop(
            Waits::Anyone,
            format!("{which}: its head branch {branch:?} is not a branch name"),
        );
    }
    let base_ref = format!("refs/remotes/{remote}/{base_branch}");
    let tracking = format!("{remote}/{branch}");
    let branch_ref = format!("refs/remotes/{tracking}");
    let fetch = |from: &str, to: &str| {
        git(
            source,
            &[
                "fetch",
                "--quiet",
                &remote,
                &format!("+refs/heads/{from}:{to}"),
            ],
        )
    };
    fetch(base_branch, &base_ref)?;
    if let Err(e) = fetch(branch, &branch_ref) {
        return stop(
            Waits::Anyone,
            format!(
                "{which}: its head branch {branch} cannot be fetched from {remote} ({e:#}); check \
                 {which} on the forge"
            ),
        );
    }
    let base_tip = rev(source, &base_ref)?;
    let observed = rev(source, &branch_ref)?;
    if is_ancestor(source, &base_tip, &observed) {
        return Ok(Step::Done);
    }
    if observed != part.info.head_sha {
        return stop(
            Waits::Nobody,
            format!(
                "{which}: the forge has its head at {}, {tracking} is at {}; rerun {cmd}",
                short(&part.info.head_sha),
                short(&observed)
            ),
        );
    }
    let behind = format!(
        "{which}: {branch} does not contain {base} at {}",
        short(&base_tip)
    );
    if let Place::Checkout { dir, .. } = part.place
        && let Some(why) = checkout_problem(dir, branch, &observed, &tracking)?
    {
        return stop(
            Waits::Anyone,
            format!(
                "{behind}, and its checkout {} is not rebased: {why}; then rerun {cmd} (or swl \
                 refresh)",
                dir.display()
            ),
        );
    }
    let drop_landing = part.consumer
        && finalize::is_landing(source, &observed)?
        && lock_moved(source, &observed, &base_tip)?;
    let start = match drop_landing {
        true => rev(source, &format!("{observed}^"))?,
        false => observed.clone(),
    };
    let count = git(
        source,
        &["rev-list", "--count", &format!("{base_tip}..{start}")],
    )?;
    let lease = format!("--force-with-lease=refs/heads/{branch}:{observed}");
    let what = format!(
        "rebase {branch} of {which} ({count} commit{}{}) onto {base} at {}{}, and push it with \
         --force-with-lease=refs/heads/{branch}:{}",
        if count == "1" { "" } else { "s" },
        match drop_landing {
            true => format!(
                ", without swl's landing commit {}, as {base} moved flake.lock",
                short(&observed)
            ),
            false => String::new(),
        },
        short(&base_tip),
        match drop_landing {
            true => " and finalize it anew",
            false => "",
        },
        short(&observed)
    );
    if !l.act(&format!("refresh {which}"), &what)? {
        return Ok(Step::Planned(format!(
            "CI runs on the new head of {which}, then its merge is requested"
        )));
    }
    let scratch = Scratch::new(source, part.repo, &start)?;
    if let Some(conflict) = rebase(&scratch.dir, &base_tip)? {
        let by_hand = match part.place {
            Place::Checkout { dir, .. } => format!(" in {}", dir.display()),
            Place::Scratch => String::new(),
        };
        return stop(
            Waits::Anyone,
            format!(
                "{behind}, and rebasing it conflicts in {conflict}; nothing was changed: rebase it \
                 by hand{by_hand}, push it, then rerun {cmd}"
            ),
        );
    }
    let mut head = git(&scratch.dir, &["rev-parse", "HEAD"])?;
    if head == base_tip {
        return stop(
            Waits::Anyone,
            format!(
                "{which}: everything on {branch} is in {base} already; nothing was pushed: close \
                 {which}"
            ),
        );
    }
    if drop_landing {
        match refinalize(l, &scratch, source, which) {
            Ok(h) => head = h,
            Err(why) => {
                return stop(
                    Waits::Anyone,
                    format!(
                        "{behind}, and its landing commit, dropped to rebase it, cannot be made \
                         anew: {why}; nothing was pushed"
                    ),
                );
            }
        }
    }
    let refspec = format!("{head}:refs/heads/{branch}");
    let push = [
        "-c",
        "core.hooksPath=/dev/null",
        "push",
        "--quiet",
        &lease,
        &remote,
        &refspec,
    ];
    if let Err(e) = git(&scratch.dir, &push) {
        return match push_rejection(&format!("{e:#}")) {
            None => Err(e.context(format!("pushing {branch} of {which}"))),
            Some(Rejection::Moved) => stop(
                Waits::Nobody,
                format!(
                    "{which}: {tracking} moved since it was read at {}, so the lease refused the \
                     push; nothing was changed: rerun {cmd}",
                    short(&observed)
                ),
            ),
            Some(Rejection::Refused(reason)) => stop(
                Waits::Owner,
                format!("{which}: the push of the rebased {branch} was refused: {reason}"),
            ),
        };
    }
    style::say(
        cmd,
        Tone::Good,
        format!(
            "{which}: pushed {branch} at {}, rebased onto {base}",
            short(&head)
        ),
    );
    if let Place::Checkout { dir, root } = part.place {
        if let Some(why) = move_checkout(dir, branch, &observed, &head)? {
            return stop(
                Waits::Anyone,
                format!(
                    "{which}: pushed, but its checkout {} was not moved: {why}",
                    dir.display()
                ),
            );
        }
        if lock_changed(source, &observed, &head)? {
            rerun_env(cmd, root, l.config);
        }
    }
    l.await_pull_at(source, &remote, which, part.info.number, &head)
}

/// Whether `flake.lock` differs between two revisions.
fn lock_changed(dir: &Path, a: &str, b: &str) -> Result<bool> {
    Ok(!git(dir, &["diff", "--name-only", a, b, "--", "flake.lock"])?.is_empty())
}

/// The depth of a row's deepest input path: nested ones are refreshed
/// first.
fn depth(row: &Row) -> usize {
    row.under
        .iter()
        .map(|(_, p)| p.matches('/').count())
        .max()
        .unwrap_or(0)
}

/// The open MRs of the workspace's rows, bottom-up: the shared ones (nested
/// first), then the consumers'.
fn parts<'a>(ws: &'a Workspace, rows: &'a [Row]) -> Vec<Part<'a>> {
    let mut shared: Vec<(&Row, Part)> = Vec::new();
    let mut consumers = Vec::new();
    for row in rows {
        let open = |facts: &'a MrFacts| match facts {
            MrFacts::Read {
                number,
                read: MrRead::Open { info, .. },
            } => Some((*number, info)),
            _ => None,
        };
        let checkout = ws.checkouts.iter().find(|c| c.name == row.name);
        match &row.facts {
            RowFacts::Shared(f) => {
                if let (Some((n, info)), Some(c)) = (open(&f.mr), checkout) {
                    let part = Part {
                        which: format!("{}#{n}", f.repo),
                        repo: &f.repo,
                        info,
                        place: Place::Checkout {
                            dir: &c.dir,
                            root: &ws.root,
                        },
                        consumer: false,
                    };
                    shared.push((row, part));
                }
            }
            RowFacts::Entry {
                repo,
                read: MrRead::Open { info, .. },
            } => {
                let part = Part {
                    which: format!("{repo}#{}", info.number),
                    repo,
                    info,
                    place: Place::Scratch,
                    consumer: false,
                };
                shared.push((row, part));
            }
            RowFacts::Consumer(f) => {
                if let (Some((n, info)), Some(c)) = (open(&f.mr), checkout) {
                    consumers.push(Part {
                        which: format!("{}#{n}", c.name),
                        repo: &c.name,
                        info,
                        place: Place::Checkout {
                            dir: &c.dir,
                            root: &ws.root,
                        },
                        consumer: true,
                    });
                }
            }
            _ => {}
        }
    }
    shared.sort_by_key(|(row, _)| std::cmp::Reverse(depth(row)));
    shared
        .into_iter()
        .map(|(_, p)| p)
        .chain(consumers)
        .collect()
}

const CMD: &str = "swl refresh";

/// `swl refresh`: every open MR branch of the workspace's change, bottom-up,
/// rebased onto its base and pushed when it does not contain the base's
/// tip. Stops at the first that cannot be.
pub fn run(
    root: &Path,
    config: &Config,
    reader: &dyn PrReader,
    opts: &Options,
) -> Result<ExitCode> {
    let lander = Lander::new(CMD, config, reader, &DryRunWriter, opts);
    let ws = Workspace::scan(root)?;
    let rows = status::collect(&ws, config, reader, &Fetcher::new(true))?;
    let parts = parts(&ws, &rows);
    if parts.is_empty() {
        style::say(
            CMD,
            Tone::Plain,
            format!("nothing to refresh: no open MR in {}", ws.root.display()),
        );
        return Ok(ExitCode::SUCCESS);
    }
    let mut pushed: Vec<&str> = Vec::new();
    for part in &parts {
        let _span = tracing::debug_span!("refresh", which = %part.which).entered();
        match refresh(&lander, part)? {
            Step::Done => style::say(
                CMD,
                Tone::Plain,
                format!(
                    "{}: {} contains {} already",
                    part.which,
                    part.info.head_ref,
                    config.base_for(part.repo)
                ),
            ),
            Step::Progress => pushed.push(&part.which),
            Step::Planned(_) => {}
            Step::Stop(waits, msg) => {
                say_waits(CMD, waits, &msg);
                return Ok(match opts.dry_run {
                    true => ExitCode::SUCCESS,
                    false => ExitCode::from(waits_code(waits)),
                });
            }
        }
    }
    if opts.dry_run {
        style::say(CMD, Tone::Plain, "dry run, nothing was written");
    } else if !pushed.is_empty() {
        style::say(
            CMD,
            Tone::Good,
            format!(
                "rebased and pushed {}: CI runs on the new heads (swl status shows CI pending), \
                 then swl land goes on",
                pushed.join(", ")
            ),
        );
    }
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn branches_never_rewritten() {
        for b in [
            "master",
            "main",
            "trunk",
            "swan-lake-current",
            "swan-lake-current/nixos-26.05",
            "swl-bump-common",
        ] {
            assert!(never_rewritten(b, "trunk").is_some(), "{b}");
        }
        for b in ["sl-0067-autorebase", "feature/x", "swan-lake-currentish"] {
            assert_eq!(never_rewritten(b, "master"), None, "{b}");
        }
    }
}

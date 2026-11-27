//! `swl land`: takes a workspace's change to master. Stateless: each run
//! reads the state as `swl status` does (the checkouts, manifests, forge and
//! mirror), does what it can, and stops at the first step it cannot do,
//! saying what it waits for and for whom. Rerun it to continue.
//!
//! 1. The shared MRs that the consumers' `swl.toml` entries name, nested
//!    entries first: one behind its base is rebased and pushed with a lease
//!    ([`refresh`]), a draft is marked ready, then it is merged by
//!    fast-forward: now when its checks are green, else when they succeed.
//! 2. A merged MR whose input `flake.nix` declares at a branch other than the
//!    base (common's `swan-lake-current`) is promoted: that branch is
//!    fast-forwarded on the forge to the merge commit. Then the declared
//!    source (common's GitHub mirror) must have it.
//! 3. Each consumer: rebased as a shared MR is when behind its base, `swl
//!    finalize` when every entry is satisfied, the landing commit pushed to
//!    its MR branch, and its merge requested; an obsolete consumer MR (its
//!    change in its base already) stops it, only reported. Common as a
//!    consumer, its MR merged, is promoted then as in step 2, to the branch
//!    and declared sources that `bump` promotes
//!    ([`crate::bump::promotion_target`]).
//! 4. When every consumer MR is merged, the workspace is removed (unless
//!    `--keep`).
//!
//! Writes: only those, the forge's through [`ForgeWriter`]; git pushes are
//! fast-forward only, but those of rebased MR branches, with a lease.
//! `--dry-run` writes nothing.

use std::{
    cell::RefCell,
    collections::BTreeSet,
    fs,
    path::Path,
    process::ExitCode,
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};

use crate::{
    ancestry,
    bump::COMMON_REPO,
    config::Config,
    env::GENERATED_MARKER,
    finalize::{self, Landing},
    forge::{
        CiState, CloseOutcome, ForgeWriter, MergeOutcome, MergeWhen, PrInfo, PrReader, PrState,
        ready_title,
    },
    git::{STATUS_PORCELAIN, git, git_ok, short, valid_branch},
    manifest::{Entry, MANIFEST_FILE, Manifest},
    refresh::{self, Part, Place},
    report::Report,
    start,
    status::{
        self, ConsumerFacts, ConsumerPromotion, MrFacts, MrRead, RemoteBranch, Row, RowFacts,
        Stage, Waits,
    },
    style::Tone,
    workspace::{Checkout, Workspace},
    ws_add::{Fetcher, base_remote},
};

/// Exit status when the next step waits for the owner (a merge or a
/// promotion that the user may not do).
pub const EXIT_OWNER: u8 = 3;
/// Exit status when the next step waits for whoever works on the change.
pub const EXIT_ANYONE: u8 = 4;
/// Exit status when the next step waits for nobody (CI, a mirror, the
/// forge): rerun later.
pub const EXIT_NOBODY: u8 = 5;

/// Runs before giving up in one run: each step that writes collects the
/// state again.
const MAX_STEPS: usize = 40;

pub struct Options {
    pub dry_run: bool,
    pub keep: bool,
    /// How long to wait for the mirror and the forge, and how often to look.
    pub timeout: Duration,
    pub interval: Duration,
}

/// A write that this run did already is due again: the forge lags.
#[derive(Debug)]
pub(crate) struct Repeated(pub(crate) String);

impl std::fmt::Display for Repeated {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Repeated {}

/// Where a run stopped.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Step {
    /// Something was written: read the state again.
    Progress,
    /// Nothing to do for this item.
    Done,
    /// A dry run reached a write; what follows depends on it.
    Planned(String),
    /// The next step waits for someone.
    Stop(Waits, String),
}

pub(crate) fn stop(waits: Waits, msg: impl Into<String>) -> Result<Step> {
    Ok(Step::Stop(waits, msg.into()))
}

/// A row's own verdict as the reason to stop.
fn stop_at(row: &Row) -> Result<Step> {
    let v = &row.verdict;
    stop(
        v.waits,
        format!("{}: {}; next: {}", row.name, v.stage, v.next),
    )
}

/// What a scheduled merge request does, for [`Lander::act`].
fn schedule_what(which: &str, info: &PrInfo) -> String {
    format!(
        "request the merge of {which} at {} (fast-forward only, when its checks succeed, \
         deleting its branch)",
        short(&info.head_sha)
    )
}

/// Why the forge refused a push.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Rejection {
    /// Not a fast-forward of the branch as the forge has it now: it moved.
    Moved,
    /// Refused (a protected branch, a hook), with the forge's words.
    Refused(String),
}

/// The rejection in git's error output of a push; `None` for another
/// failure (offline, no such remote).
pub(crate) fn push_rejection(msg: &str) -> Option<Rejection> {
    let lower = msg.to_ascii_lowercase();
    if ["non-fast-forward", "fetch first", "stale info"]
        .iter()
        .any(|m| lower.contains(m))
    {
        return Some(Rejection::Moved);
    }
    if !["rejected", "denied", "protected", "declined", "not allowed"]
        .iter()
        .any(|m| lower.contains(m))
    {
        return None;
    }
    // The forge's own words: `remote:` lines and the rejection.
    let reason: Vec<&str> = msg
        .lines()
        .filter_map(|l| match l.find("remote:") {
            Some(i) => Some(l[i..].trim()),
            None => l.contains("rejected").then(|| l.trim()),
        })
        .collect();
    Some(Rejection::Refused(match reason.as_slice() {
        [] => msg.lines().last().unwrap_or("").trim().to_string(),
        lines => lines.join("; "),
    }))
}

/// One shared MR to land: the first consumer that names it, the input path
/// there, and the entry.
type SharedMr = (String, String, Entry);

/// The unique MRs of the consumers' manifests, nested input paths first
/// (deepest first), then by path.
fn shared_entries(manifests: &[(String, Manifest)]) -> Vec<SharedMr> {
    let mut seen = BTreeSet::new();
    let mut out: Vec<SharedMr> = Vec::new();
    for (consumer, m) in manifests {
        for (path, e) in &m.inputs {
            if seen.insert((e.repo.clone(), e.mr)) {
                out.push((consumer.clone(), path.clone(), e.clone()));
            }
        }
    }
    out.sort_by(|(_, a, _), (_, b, _)| {
        let depth = |p: &str| p.matches('/').count();
        depth(b).cmp(&depth(a)).then_with(|| a.cmp(b))
    });
    out
}

/// The steps of `land` that `bump` shares: merging an MR, promoting a
/// branch, waiting for a mirror and for the forge. `cmd` (`swl land`, `swl
/// bump`) is what the messages say to rerun; they go to `report`.
pub(crate) struct Lander<'a> {
    pub(crate) cmd: &'static str,
    pub(crate) report: Report,
    pub(crate) config: &'a Config,
    pub(crate) reader: &'a dyn PrReader,
    writer: &'a dyn ForgeWriter,
    opts: &'a Options,
    /// Writes done in this run, so that none is repeated when the forge
    /// lags.
    done: RefCell<BTreeSet<String>>,
}

impl<'a> Lander<'a> {
    pub(crate) fn new(
        cmd: &'static str,
        config: &'a Config,
        reader: &'a dyn PrReader,
        writer: &'a dyn ForgeWriter,
        opts: &'a Options,
    ) -> Self {
        Self {
            cmd,
            report: Report::new(cmd),
            config,
            reader,
            writer,
            opts,
            done: RefCell::new(BTreeSet::new()),
        }
    }
}

impl Lander<'_> {
    /// Whether this run wrote anything.
    pub(crate) fn wrote(&self) -> bool {
        !self.done.borrow().is_empty()
    }

    /// Says what is (or with `--dry-run` would be) done. `Ok(false)` in a dry
    /// run; a [`Repeated`] error when this run did the same already.
    pub(crate) fn act(&self, key: &str, what: &str) -> Result<bool> {
        self.act_running(key, what, None)
    }

    /// [`Lander::act`] for a step that is the command `command`, which
    /// follows as a detail.
    fn act_running(&self, key: &str, what: &str, command: Option<&str>) -> Result<bool> {
        let cmd = self.cmd;
        let full = match command {
            Some(c) => format!("{what}: {c}"),
            None => what.to_string(),
        };
        if !self.opts.dry_run && !self.done.borrow_mut().insert(key.to_string()) {
            return Err(Repeated(format!(
                "{full}: done once in this run already, but the forge does not show it yet; rerun \
                 {cmd}"
            ))
            .into());
        }
        if self.opts.dry_run {
            self.report.step(format!("would {what}"));
        } else {
            self.report.step(what);
        }
        if let Some(c) = command {
            self.report.detail(c);
        }
        Ok(!self.opts.dry_run)
    }

    fn dry_stop(&self, what: &str) -> Result<Step> {
        Ok(Step::Planned(what.to_string()))
    }

    /// Marks a draft MR ready by dropping its `WIP:` title prefix. A draft
    /// without one is for someone to mark ready on the forge.
    fn mark_ready(&self, which: &str, repo: &str, info: &PrInfo) -> Result<Option<Step>> {
        let cmd = self.cmd;
        if !info.draft {
            return Ok(None);
        }
        let title = ready_title(&info.title);
        if title == info.title.trim() {
            return stop(
                Waits::Anyone,
                format!(
                    "{which} is a draft without a WIP: title; mark it ready on the forge, then \
                     rerun {cmd}"
                ),
            )
            .map(Some);
        }
        if self.act(
            &format!("ready {which}"),
            &format!("mark {which} ready: {title:?}"),
        )? {
            self.writer.set_title(repo, info.number, &title)?;
        }
        Ok(None)
    }

    /// Steps 1 and 2 for one shared MR; its checkout, if any, in `ws`.
    fn shared(
        &self,
        rows: &[Row],
        ws: Option<&Workspace>,
        (_, path, e): &SharedMr,
    ) -> Result<Step> {
        let cmd = self.cmd;
        let which = format!("{}#{}", e.repo, e.mr);
        let _span = tracing::debug_span!("shared", %which, %path).entered();
        self.report.section(&which);
        let Some(row) = rows
            .iter()
            .find(|r| r.follows.as_deref() == Some(which.as_str()))
        else {
            return stop(Waits::Nobody, format!("{which}: no status; rerun {cmd}"));
        };
        if row.verdict.stage == Stage::Landed {
            return Ok(Step::Done);
        }
        if let RowFacts::Shared(_) = &row.facts
            && matches!(
                row.verdict.stage,
                Stage::Local | Stage::NeedsPublish | Stage::Pushed | Stage::Unknown(_)
            )
        {
            return stop_at(row);
        }
        let Some((_, read)) = row.mr_read() else {
            return stop_at(row);
        };
        match read {
            MrRead::Error(_) | MrRead::NotFound(_) => stop_at(row),
            MrRead::Closed => stop(
                Waits::Anyone,
                format!(
                    "{which} ({path}) was closed without merging: reopen it, or drop the entry"
                ),
            ),
            MrRead::Open { info, .. } => {
                let checkout = match (&row.facts, ws) {
                    (RowFacts::Shared(_), Some(ws)) => {
                        ws.checkouts.iter().find(|c| c.name == row.name)
                    }
                    _ => None,
                };
                let place = match (checkout, ws) {
                    (Some(c), Some(ws)) => Place::Checkout {
                        dir: &c.dir,
                        root: &ws.root,
                    },
                    _ => Place::Scratch,
                };
                let part = Part {
                    which: which.clone(),
                    repo: &e.repo,
                    info,
                    place,
                    consumer: false,
                };
                match refresh::refresh(self, &part)? {
                    Step::Done => {}
                    step => return Ok(step),
                }
                self.merge(&which, &e.repo, info, &self.rebase_step(&e.repo, info))
            }
            MrRead::Merged {
                merge_commit,
                promotion,
            } => {
                let Some(p) = promotion else {
                    return stop_at(row);
                };
                if !valid_branch(&p.branch) {
                    return stop(
                        Waits::Anyone,
                        format!(
                            "{which}: the declared branch {:?} is not a branch name",
                            p.branch
                        ),
                    );
                }
                match &p.on_forge {
                    Err(err) => stop(
                        Waits::Owner,
                        format!(
                            "{which}: promote {} (cannot tell whether done: {err})",
                            p.branch
                        ),
                    ),
                    Ok(false) => self.promote(&which, &e.repo, merge_commit, &p.branch),
                    Ok(true) => match &p.declared_source {
                        Some((url, _)) => self.mirror(&which, url, &p.branch, merge_commit),
                        None => Ok(Step::Done),
                    },
                }
            }
        }
    }

    /// Why `info`'s head is not mergeable by fast-forward into `repo`'s
    /// base, told by git in the repo's source after fetching the base
    /// branch: `None` when the head contains the base's tip, or when that
    /// cannot be told (a repo not in the config, a base without a remote, a
    /// head that cannot be fetched).
    fn base_missing(&self, repo: &str, info: &PrInfo) -> Option<String> {
        let rc = self.config.repo(repo).ok()?;
        let (src, base) = (&rc.source, rc.base());
        let remote = base_remote(src, base).ok()??;
        let branch = base.split_once('/').map_or(base, |(_, b)| b);
        let tracking = format!("refs/remotes/{remote}/{branch}");
        let known = |r: &str| {
            git_ok(
                src,
                &[
                    "rev-parse",
                    "--verify",
                    "--quiet",
                    &format!("{r}^{{commit}}"),
                ],
            )
        };
        let refspec = format!("+refs/heads/{branch}:{tracking}");
        if let Err(e) = git(src, &["fetch", "--quiet", &remote, &refspec]) {
            tracing::debug!(error = %format!("{e:#}"), "fetching the base before a merge");
            return None;
        }
        if known(&info.head_sha).is_none() {
            let pull = format!("refs/pull/{}/head", info.number);
            let _ = git(src, &["fetch", "--quiet", &remote, &pull]);
        }
        let tip = known(&tracking)?;
        known(&info.head_sha)?;
        let contains = git_ok(src, &["merge-base", "--is-ancestor", &tip, &info.head_sha]);
        tracing::debug!(%repo, head = %info.head_sha, %tip, contains = contains.is_some(), "the MR head against its base");
        contains.is_none().then(|| {
            format!(
                "its head {} does not contain {base} at {}",
                short(&info.head_sha),
                short(&tip)
            )
        })
    }

    /// Requests the merge of `info`'s head. Forgejo answers a 500 to a
    /// fast-forward-only merge it cannot do: when git shows that the head
    /// does not contain the base, that is [`MergeOutcome::NotFastForward`],
    /// else an error.
    fn request_merge(&self, repo: &str, info: &PrInfo, when: MergeWhen) -> Result<MergeOutcome> {
        let outcome = self
            .writer
            .request_merge(repo, info.number, &info.head_sha, when)?;
        let MergeOutcome::ServerError(why) = outcome else {
            return Ok(outcome);
        };
        match self.base_missing(repo, info) {
            Some(missing) => Ok(MergeOutcome::NotFastForward(format!("{why}; {missing}"))),
            None => bail!("merge request of {repo}#{}: {why}", info.number),
        }
    }

    /// The combined commit status of an MR head, read now; `None` when it
    /// cannot be read.
    fn ci_now(&self, repo: &str, head: &str) -> Option<CiState> {
        self.reader.commit_status(repo, head).ok().flatten()
    }

    /// Marks an open MR ready and merges it: now when its checks are green,
    /// else when they succeed. `rebase` is the step that makes it mergeable
    /// by fast-forward again when the base moved ([`Lander::rebase_step`]
    /// for `land`).
    ///
    /// Forgejo merges a scheduled MR on a later commit status event only:
    /// scheduled on checks that are green already, it never merges. So a
    /// green MR is merged directly, and so is one that turns green right
    /// after its merge was scheduled. A direct merge that branch protection
    /// refuses leaves the schedule in place, or makes one. Nothing is
    /// requested for a head that git shows does not contain the base: that
    /// merge cannot be a fast-forward.
    pub(crate) fn merge(
        &self,
        which: &str,
        repo: &str,
        info: &PrInfo,
        rebase: &str,
    ) -> Result<Step> {
        let cmd = self.cmd;
        let ci = self.ci_now(repo, &info.head_sha);
        tracing::debug!(%which, ?ci, "the head's combined commit status before merging");
        if let Some(CiState::Failing(s)) = &ci {
            return stop(
                Waits::Anyone,
                format!("{which}: CI failing ({s}); fix it, then rerun {cmd}"),
            );
        }
        if let Some(why) = self.base_missing(repo, info) {
            let outcome = MergeOutcome::NotFastForward(why);
            return self.merge_answer(which, repo, info, outcome, rebase);
        }
        if let Some(step) = self.mark_ready(which, repo, info)? {
            return Ok(step);
        }
        if ci == Some(CiState::Green) {
            return self.merge_now(which, repo, info, rebase, false);
        }
        if !self.act(&format!("merge {which}"), &schedule_what(which, info))? {
            return self.dry_stop(&format!("{which} merges when its checks succeed"));
        }
        let outcome = self.request_merge(repo, info, MergeWhen::ChecksSucceed)?;
        if outcome == MergeOutcome::Scheduled {
            if self.ci_now(repo, &info.head_sha) == Some(CiState::Green) {
                self.report.step(format!(
                    "{which}: its checks turned green as the merge was scheduled, and no later \
                     status would start it"
                ));
                return self.merge_now(which, repo, info, rebase, true);
            }
            return stop(
                Waits::Nobody,
                format!(
                    "{which} waits for CI, then it merges (merge when checks succeed); rerun {cmd}"
                ),
            );
        }
        self.merge_answer(which, repo, info, outcome, rebase)
    }

    /// Merges an MR whose checks are green with a plain fast-forward merge.
    /// Forgejo deletes a pending schedule when it merges (relied on, not
    /// verified against a running Forgejo); a stale one on a merged MR does
    /// nothing. A refusal by branch protection or permissions keeps the
    /// schedule (`scheduled`), or requests one.
    fn merge_now(
        &self,
        which: &str,
        repo: &str,
        info: &PrInfo,
        rebase: &str,
        scheduled: bool,
    ) -> Result<Step> {
        let cmd = self.cmd;
        let what = format!(
            "merge {which} at {} now, its checks being green (fast-forward only, deleting its \
             branch)",
            short(&info.head_sha)
        );
        if !self.act(&format!("merge now {which}"), &what)? {
            return self.dry_stop(&format!("{which} is merged"));
        }
        let outcome = self.request_merge(repo, info, MergeWhen::Now)?;
        match &outcome {
            MergeOutcome::Scheduled => stop(
                Waits::Nobody,
                format!("{which}: the forge did not merge it yet; rerun {cmd}"),
            ),
            MergeOutcome::ChecksPending(_)
            | MergeOutcome::NeedsApproval(_)
            | MergeOutcome::Refused(_)
                if !self.merged_now(repo, info.number) =>
            {
                let note = if scheduled {
                    "its merge stays scheduled for when its checks succeed".to_string()
                } else {
                    self.schedule_after_refusal(which, repo, info)?
                };
                self.refusal(which, outcome, &note)
            }
            _ => self.merge_answer(which, repo, info, outcome, rebase),
        }
    }

    /// Requests the scheduled merge of an MR whose direct merge was refused;
    /// what came of it, for the message.
    fn schedule_after_refusal(&self, which: &str, repo: &str, info: &PrInfo) -> Result<String> {
        self.act(&format!("merge {which}"), &schedule_what(which, info))?;
        let answer = self.request_merge(repo, info, MergeWhen::ChecksSucceed);
        Ok(match answer {
            Ok(MergeOutcome::Scheduled) => {
                "its merge is scheduled for when its checks succeed".to_string()
            }
            Ok(MergeOutcome::Refused(why)) => {
                format!("scheduling its merge was refused too: {why}")
            }
            Ok(other) => format!("its merge could not be scheduled: {other:?}"),
            Err(e) => format!("scheduling its merge failed: {e:#}"),
        })
    }

    /// The stop for a refused direct merge; `note` says what became of the
    /// schedule.
    fn refusal(&self, which: &str, outcome: MergeOutcome, note: &str) -> Result<Step> {
        let cmd = self.cmd;
        match outcome {
            MergeOutcome::ChecksPending(why) => stop(
                Waits::Nobody,
                format!(
                    "{which}: a required status check has not passed yet ({why}); {note}; rerun \
                     {cmd}"
                ),
            ),
            MergeOutcome::NeedsApproval(why) => stop(
                Waits::Owner,
                format!("approve {which} on the forge ({why}); {note}; then rerun {cmd}"),
            ),
            MergeOutcome::Refused(why) => stop(
                Waits::Owner,
                format!("merge {which} (the forge refused: {why}); {note}"),
            ),
            other => unreachable!("not a refusal: {other:?}"),
        }
    }

    /// Whether the forge shows the MR merged (by hand, or by its schedule).
    fn merged_now(&self, repo: &str, number: u64) -> bool {
        matches!(
            self.reader.pr(repo, number),
            Ok(Some(PrInfo {
                state: PrState::Merged { .. },
                ..
            }))
        )
    }

    /// The step after the forge's answer to a merge request.
    fn merge_answer(
        &self,
        which: &str,
        repo: &str,
        info: &PrInfo,
        outcome: MergeOutcome,
        rebase: &str,
    ) -> Result<Step> {
        let cmd = self.cmd;
        match outcome {
            MergeOutcome::Merged => {
                self.report
                    .outcome(Tone::Good, format!("{which} is merged"));
                Ok(Step::Progress)
            }
            MergeOutcome::Scheduled => stop(
                Waits::Nobody,
                format!(
                    "{which} waits for CI, then it merges (merge when checks succeed); rerun {cmd}"
                ),
            ),
            _ if self.merged_now(repo, info.number) => Ok(Step::Progress),
            MergeOutcome::Refused(why) => stop(
                Waits::Owner,
                format!("merge {which} (the forge refused: {why})"),
            ),
            MergeOutcome::ChecksPending(why) => stop(
                Waits::Nobody,
                format!("{which}: a required status check has not passed yet ({why}); rerun {cmd}"),
            ),
            MergeOutcome::NeedsApproval(why) => stop(
                Waits::Owner,
                format!("approve {which} on the forge ({why}), then rerun {cmd}"),
            ),
            MergeOutcome::NotFastForward(why) => stop(
                Waits::Anyone,
                format!("{rebase} ({which} is not mergeable by fast-forward: {why})"),
            ),
            MergeOutcome::HeadMoved(why) => stop(
                Waits::Anyone,
                format!("{which}: its head moved ({why}); rerun {cmd}"),
            ),
            MergeOutcome::TryLater(why) => stop(
                Waits::Nobody,
                format!("{which}: the forge is not ready ({why}); rerun {cmd}"),
            ),
            MergeOutcome::ServerError(why) => bail!("{which}: merge request: {why}"),
        }
    }

    /// `land`'s step for an MR whose branch is not mergeable by
    /// fast-forward: a rerun rebases it ([`refresh::refresh`]).
    fn rebase_step(&self, repo: &str, info: &PrInfo) -> String {
        let base = self.config.base_for(repo);
        let base_branch = base.split_once('/').map_or(base, |(_, b)| b);
        format!(
            "rebase {} onto {base_branch}: rerun {}, or swl refresh",
            info.head_ref, self.cmd
        )
    }

    /// Step 2: fast-forwards `branch` on the forge to the merge commit.
    pub(crate) fn promote(
        &self,
        which: &str,
        repo: &str,
        merge_commit: &str,
        branch: &str,
    ) -> Result<Step> {
        let rc = self.config.repo(repo)?;
        let src = &rc.source;
        let remote = base_remote(src, rc.base())?.unwrap_or_else(|| "origin".into());
        let tracking = format!("refs/remotes/{remote}/{branch}");
        let tip =
            git(src, &["rev-parse", "--verify", "--quiet", &tracking]).with_context(|| {
                format!(
                    "{which}: {remote}/{branch} is not known in {}",
                    src.display()
                )
            })?;
        let cmd = format!(
            "git -C {} push {remote} {merge_commit}:refs/heads/{branch}",
            src.display()
        );
        if git_ok(src, &["merge-base", "--is-ancestor", &tip, merge_commit]).is_none() {
            return stop(
                Waits::Owner,
                format!(
                    "promote {branch}: {remote}/{branch} at {} is not an ancestor of {which}'s \
                     merge commit {}, so it cannot be fast-forwarded (swl never force-pushes a \
                     promotion branch)",
                    short(&tip),
                    short(merge_commit)
                ),
            );
        }
        let range = format!("{tip}..{merge_commit}");
        let promoted = git(src, &["log", "--reverse", "--format=%h %s", &range])?;
        if !promoted.is_empty() {
            let lines: Vec<&str> = promoted.lines().collect();
            self.report.step(format!(
                "{which}: {branch} at {} gets {} commit(s):\n  {}",
                short(&tip),
                lines.len(),
                lines.join("\n  ")
            ));
        }
        let what = format!("promote {which}");
        if !self.act_running(&what, &what, Some(&cmd))? {
            return self.dry_stop(&format!(
                "{branch} has {which}, and its declared source too"
            ));
        }
        let refspec = format!("{merge_commit}:refs/heads/{branch}");
        match git(src, &["push", &remote, &refspec]) {
            Ok(_) => Ok(Step::Progress),
            Err(e) => match push_rejection(&format!("{e:#}")) {
                None => Err(e.context(format!("promoting {which}"))),
                Some(Rejection::Moved) => stop(
                    Waits::Nobody,
                    format!(
                        "promote {which}: {remote}'s {branch} moved since it was fetched; rerun \
                         {}",
                        self.cmd
                    ),
                ),
                Some(Rejection::Refused(reason)) => stop(
                    Waits::Owner,
                    format!("promote: {cmd} (the push was refused: {reason})"),
                ),
            },
        }
    }

    /// Common as a consumer, its MR merged: promoted as a shared MR is in
    /// step 2, to the branch and declared sources that common's consumers
    /// declare.
    fn promote_consumer(&self, which: &str, p: &ConsumerPromotion) -> Result<Step> {
        let promotion = match &p.promotion {
            Ok(promotion) => promotion,
            Err(e) => return stop(Waits::Nobody, format!("{which}: {e}; rerun {}", self.cmd)),
        };
        let (commit, branch) = (&p.commit, &promotion.branch);
        match &promotion.on_forge {
            Err(err) => stop(
                Waits::Owner,
                format!("{which}: promote {branch} (cannot tell whether done: {err})"),
            ),
            Ok(false) => {
                let step = self.promote(which, COMMON_REPO, commit, branch)?;
                if step == Step::Progress {
                    self.report.outcome(
                        Tone::Good,
                        format!(
                            "{which}: promoted {branch} to {}; next: swl bump, to move the \
                             consumers",
                            short(commit)
                        ),
                    );
                }
                Ok(step)
            }
            Ok(true) => {
                for url in &p.mirrors {
                    match self.mirror(which, url, branch, commit)? {
                        Step::Done => {}
                        step => return Ok(step),
                    }
                }
                Ok(Step::Done)
            }
        }
    }

    /// Waits for the declared source (common's GitHub mirror) to have the
    /// merge commit in `branch`.
    pub(crate) fn mirror(
        &self,
        which: &str,
        url: &str,
        branch: &str,
        merge_commit: &str,
    ) -> Result<Step> {
        let cmd = self.cmd;
        let deadline = Instant::now() + self.opts.timeout;
        let refname = format!("refs/heads/{branch}");
        loop {
            let has = ancestry::remote_head(url, &refname)
                .and_then(|tip| ancestry::is_ancestor(url, merge_commit, &tip, false));
            if has == Ok(true) {
                return Ok(Step::Done);
            }
            let why = match has {
                Err(e) => format!("cannot tell yet: {e}"),
                _ => "not yet".into(),
            };
            if self.opts.dry_run || Instant::now() + self.opts.interval > deadline {
                return stop(
                    Waits::Nobody,
                    format!(
                        "{which}: waiting for {url} {branch} to have {} ({why}); rerun {cmd}",
                        short(merge_commit)
                    ),
                );
            }
            self.report.detail(format!(
                "{which}: {url} {branch} does not have {} yet; checking again in {}s",
                short(merge_commit),
                self.opts.interval.as_secs()
            ));
            thread::sleep(self.opts.interval);
        }
    }

    /// Step 3 for one consumer checkout of the workspace at `ws_root`.
    fn consumer(&self, rows: &[Row], ws_root: &Path, c: &Checkout) -> Result<Step> {
        let cmd = self.cmd;
        let _span = tracing::debug_span!("consumer", name = %c.name).entered();
        self.report.section(&c.name);
        let Some(row) = rows
            .iter()
            .find(|r| r.name == c.name && r.follows.is_none())
        else {
            return stop(Waits::Nobody, format!("{}: no status; rerun {cmd}", c.name));
        };
        if let RowFacts::Consumer(ConsumerFacts {
            mr: MrFacts::Read { number, .. },
            ..
        }) = &row.facts
        {
            self.report
                .section_also(format!("{}#{number}", c.name), &[&c.name]);
        }
        if let (Stage::NoChange(_) | Stage::NotAChange(_), RowFacts::Consumer(f)) =
            (&row.verdict.stage, &row.facts)
        {
            // Whether the workspace's MR is open, or this branch's merged,
            // is unknown: landing the rest could remove the workspace
            // without it.
            if let Some(why) = &f.branch_mr_error {
                return stop(Waits::Nobody, format!("{}: {why}; rerun {cmd}", c.name));
            }
            // The workspace's open MR is not checked out: landing the rest
            // would remove the workspace without it.
            let Some((n, branch)) = &f.workspace_mr else {
                return Ok(Step::Done);
            };
            let which = format!("{}#{n}", c.name);
            return stop(
                Waits::Anyone,
                format!(
                    "{}: {}; next: {}\n  {}",
                    c.name,
                    row.verdict.stage,
                    status::switch_step(&c.dir.display().to_string(), branch),
                    status::land_from_note(&which, branch)
                ),
            );
        }
        if row.verdict.stage == Stage::Landed {
            return Ok(Step::Done);
        }
        let RowFacts::Consumer(f) = &row.facts else {
            return stop_at(row);
        };
        // Only reported: closing it is for whoever opened it, or the owner.
        if let (Stage::Obsolete(base), MrFacts::Read { number, .. }) = (&row.verdict.stage, &f.mr) {
            return stop(
                Waits::Anyone,
                format!(
                    "{}#{number}: obsolete: its change is in {base}; close it",
                    c.name
                ),
            );
        }
        if let Some(p) = &f.promotion {
            let which = match &f.mr {
                MrFacts::Read { number, .. } => format!("{}#{number}", c.name),
                _ => c.name.clone(),
            };
            return self.promote_consumer(&which, p);
        }
        let Some(branch) = &f.topic else {
            return stop_at(row);
        };
        if f.dirty {
            return stop(
                Waits::Anyone,
                format!("{}: commit or stash the uncommitted changes first", c.name),
            );
        }
        if !f.unpublished_inputs.is_empty() {
            return stop_at(row);
        }
        let remote = base_remote(&c.dir, &f.base)?.unwrap_or_else(|| "origin".into());
        let remote_head = git(
            &c.dir,
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("refs/remotes/{remote}/{branch}"),
            ],
        )
        .ok();
        match f.remote {
            RemoteBranch::Missing | RemoteBranch::Other => return stop_at(row),
            RemoteBranch::Behind => {
                let rh = remote_head.as_deref().unwrap_or("HEAD");
                let changed = git(&c.dir, &["diff", "--name-only", rh, "HEAD"])?;
                if changed
                    .lines()
                    .any(|f| f != "flake.lock" && f != MANIFEST_FILE)
                {
                    return stop(
                        Waits::Anyone,
                        format!(
                            "{}: {branch} has unpushed commits besides a landing; swl publish {}",
                            c.name, c.name
                        ),
                    );
                }
            }
            RemoteBranch::Same => {}
        }
        let (number, read) = match &f.mr {
            MrFacts::Read { number, read } => (*number, read),
            MrFacts::None => {
                return stop(
                    Waits::Anyone,
                    format!("{}: {branch} has no MR; swl publish {}", c.name, c.name),
                );
            }
            _ => return stop_at(row),
        };
        let which = format!("{}#{number}", c.name);
        let info = match read {
            MrRead::Merged { .. } => return Ok(Step::Done),
            MrRead::Open { info, .. } => info,
            _ => return stop_at(row),
        };
        // The MR was found by its head commit: it must be this branch's.
        if info.head_ref != *branch || Some(info.head_sha.as_str()) != remote_head.as_deref() {
            return stop(
                Waits::Anyone,
                format!(
                    "{which} is not the MR of {branch} at {}: the forge has {} at {}; check it on \
                     the forge",
                    remote_head.as_deref().map_or("(none)", short),
                    info.head_ref,
                    short(&info.head_sha)
                ),
            );
        }
        if let Some(step) = self.mark_ready(&which, &c.name, info)? {
            return Ok(step);
        }
        // A landing commit not pushed yet is pushed first; the next pass
        // rebases the branch with it.
        if f.remote == RemoteBranch::Same {
            let part = Part {
                which: which.clone(),
                repo: &c.name,
                info,
                place: Place::Checkout {
                    dir: &c.dir,
                    root: ws_root,
                },
                consumer: true,
            };
            match refresh::refresh(self, &part)? {
                Step::Done => {}
                step => return Ok(step),
            }
        }
        match finalize::land_all(&c.dir, self.reader, self.opts.dry_run)? {
            Landing::NoManifest => {}
            Landing::Empty => {
                return stop(
                    Waits::Anyone,
                    format!(
                        "{}: {MANIFEST_FILE} has no entries; git rm {MANIFEST_FILE}, commit, then \
                         rerun {cmd}",
                        c.name
                    ),
                );
            }
            Landing::Waiting(lines) => {
                return stop(
                    Waits::Anyone,
                    format!(
                        "{}: not every entry has landed, so no landing is pushed:\n  {}",
                        c.name,
                        lines.join("\n  ")
                    ),
                );
            }
            Landing::WouldLand(lines) => {
                self.report.step(format!(
                    "would finalize {}:\n  {}",
                    c.name,
                    lines.join("\n  ")
                ));
                return self.dry_stop(&format!(
                    "then push the landing to {branch} and request the merge of {which}"
                ));
            }
            Landing::Committed(done) => {
                self.report.outcome(
                    Tone::Good,
                    format!(
                        "{}: committed {} on {branch}: {}",
                        c.name,
                        short(&done.head),
                        done.subject
                    ),
                );
                return Ok(Step::Progress);
            }
        }
        if f.remote == RemoteBranch::Behind {
            let what = format!(
                "push {}'s landing to {remote}/{branch} (fast-forward)",
                c.name
            );
            if !self.act(&format!("push {}", c.name), &what)? {
                return self.dry_stop(&format!("then request the merge of {which}"));
            }
            let refspec = format!("HEAD:refs/heads/{branch}");
            git(&c.dir, &["push", "--quiet", &remote, &refspec])?;
            return self.await_pull_head(&c.dir, &remote, &which, number);
        }
        self.merge(&which, &c.name, info, &self.rebase_step(&c.name, info))
    }

    /// After a push of `dir`'s HEAD: waits until the forge has updated the
    /// MR's pull ref.
    pub(crate) fn await_pull_head(
        &self,
        dir: &Path,
        remote: &str,
        which: &str,
        n: u64,
    ) -> Result<Step> {
        let head = git(dir, &["rev-parse", "HEAD"])?;
        self.await_pull_at(dir, remote, which, n, &head)
    }

    /// After a push of `head`: waits until the forge has the MR's pull ref
    /// there.
    pub(crate) fn await_pull_at(
        &self,
        dir: &Path,
        remote: &str,
        which: &str,
        n: u64,
        head: &str,
    ) -> Result<Step> {
        let cmd = self.cmd;
        let pull = format!("refs/pull/{n}/head");
        let deadline = Instant::now() + self.opts.timeout;
        loop {
            let at = git(dir, &["ls-remote", remote, &pull])?;
            if at.split_whitespace().next() == Some(head) {
                return Ok(Step::Progress);
            }
            if Instant::now() + self.opts.interval > deadline {
                return stop(
                    Waits::Nobody,
                    format!(
                        "{which}: waiting for the forge to update the MR to {}; rerun {cmd}",
                        short(head)
                    ),
                );
            }
            thread::sleep(self.opts.interval);
        }
    }

    /// One pass over the state: the first step that writes or stops.
    fn step(&self, ws: &Workspace, rows: &[Row]) -> Result<Step> {
        let consumers: Vec<&Checkout> = ws.checkouts.iter().filter(|c| c.is_consumer()).collect();
        if consumers.is_empty() {
            bail!("no consumer checkout in {}", ws.root.display());
        }
        let mut manifests = Vec::new();
        for c in &consumers {
            if let Some(m) = Manifest::read(&c.dir)? {
                manifests.push((c.name.clone(), m));
            }
        }
        for mr in shared_entries(&manifests) {
            match self.shared(rows, Some(ws), &mr)? {
                Step::Done => {}
                other => return Ok(other),
            }
        }
        for c in &consumers {
            match self.consumer(rows, &ws.root, c)? {
                Step::Done => {}
                other => return Ok(other),
            }
        }
        Ok(Step::Done)
    }
}

/// Whether an untracked (`??`) or ignored (`!!`) path of `git status
/// --porcelain --ignored` is one that removing the checkout may delete:
/// swl's generated `.envrc.local`, direnv's `.direnv/`, `result*` links and
/// `.swl`.
pub(crate) fn disposable(dir: &Path, path: &str, ignored: bool) -> bool {
    if path == ".envrc.local" {
        return fs::read_to_string(dir.join(path)).is_ok_and(|t| t.starts_with(GENERATED_MARKER));
    }
    ignored
        && (path == ".direnv/"
            || path == ".swl/"
            || (path.starts_with("result") && !path.trim_end_matches('/').contains('/')))
}

impl Lander<'_> {
    /// Whether a consumer of the workspace landed through a merged MR: the
    /// forge says its MR is merged, or its pushed branch, now in the base, is
    /// the head of a merged MR.
    fn landed_by_merge(&self, ws: &Workspace, rows: &[Row]) -> Result<bool> {
        for c in ws.checkouts.iter().filter(|c| c.is_consumer()) {
            let Some(RowFacts::Consumer(f)) = rows
                .iter()
                .find(|r| r.name == c.name && r.follows.is_none())
                .map(|r| &r.facts)
            else {
                continue;
            };
            if let MrFacts::Read {
                read: MrRead::Merged { .. },
                ..
            } = f.mr
            {
                return Ok(true);
            }
            let (Some(branch), true) = (&f.topic, f.remote_in_base) else {
                continue;
            };
            let Some(remote) = base_remote(&c.dir, &f.base)? else {
                continue;
            };
            let Some(head) = git_ok(
                &c.dir,
                &[
                    "rev-parse",
                    "--verify",
                    "--quiet",
                    &format!("refs/remotes/{remote}/{branch}"),
                ],
            ) else {
                continue;
            };
            let refs = git(&c.dir, &["ls-remote", &remote, "refs/pull/*/head"])?;
            for (sha, r) in start::parse_ls_remote(&refs) {
                let n = r
                    .strip_prefix("refs/pull/")
                    .and_then(|r| r.strip_suffix("/head"))
                    .and_then(|n| n.parse().ok());
                if let (true, Some(n)) = (sha == head, n)
                    && let Ok(Some(PrInfo {
                        state: PrState::Merged { .. },
                        ..
                    })) = self.reader.pr(&c.name, n)
                {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }
}

/// Why a checkout is kept rather than removed, if it is: uncommitted,
/// untracked or ignored files (but [`disposable`] ones), or commits in
/// neither the base nor its remote branch.
fn keep_reason(c: &Checkout, base: &str) -> Result<Option<String>> {
    let status = git(&c.dir, &[&STATUS_PORCELAIN[..], &["--ignored"]].concat())?;
    let mut changed = Vec::new();
    let mut ignored = Vec::new();
    for line in status.lines() {
        let (code, path) = line.split_at(line.len().min(3));
        match code.trim() {
            "??" if disposable(&c.dir, path, false) => {}
            "!!" if disposable(&c.dir, path, true) => {}
            "!!" => ignored.push(path),
            _ => changed.push(line),
        }
    }
    if !changed.is_empty() {
        return Ok(Some(format!(
            "uncommitted or untracked files: {}",
            changed.join(", ")
        )));
    }
    if !ignored.is_empty() {
        return Ok(Some(format!(
            "ignored files that removing it would delete: {}",
            ignored.join(", ")
        )));
    }
    let head = git(&c.dir, &["rev-parse", "HEAD"])?;
    let mut contained = git_ok(&c.dir, &["merge-base", "--is-ancestor", &head, base]).is_some();
    if let Some(b) = git_ok(&c.dir, &["symbolic-ref", "--quiet", "--short", "HEAD"])
        && let Some(r) = base_remote(&c.dir, base)?
    {
        let tracking = format!("refs/remotes/{r}/{b}");
        contained |= git_ok(&c.dir, &["merge-base", "--is-ancestor", &head, &tracking]).is_some();
    }
    Ok((!contained).then(|| format!("HEAD {} has unpushed commits", short(&head))))
}

/// Step 4: removes the workspace's checkouts (`git worktree remove` from
/// their sources), their merged local branches (`git branch -d`), `.swl`
/// and the directory. Dirty checkouts and those with unpushed commits are
/// kept, and said so.
fn remove_workspace(report: &Report, ws: &Workspace, config: &Config, dry_run: bool) -> Result<()> {
    report.section(format!("workspace {}", ws.root.display()));
    let mut kept = Vec::new();
    for c in &ws.checkouts {
        let repo = c
            .shared
            .as_ref()
            .map_or(c.name.as_str(), |(r, _)| r.as_str());
        let rc = config.repo(repo)?;
        let base = rc.base();
        if let Some(r) = base_remote(&c.dir, base)? {
            git(&c.dir, &["fetch", "--quiet", &r])?;
        }
        if let Some(why) = keep_reason(c, base)? {
            report.warn(format!("keeping {}: {why}", c.dir.display()));
            kept.push(c.name.clone());
            continue;
        }
        let branch = git_ok(&c.dir, &["symbolic-ref", "--quiet", "--short", "HEAD"]);
        if dry_run {
            report.step(format!(
                "would remove {}{}",
                c.dir.display(),
                branch
                    .as_deref()
                    .map(|b| format!(" and its branch {b}"))
                    .unwrap_or_default()
            ));
            continue;
        }
        let envrc = c.dir.join(".envrc.local");
        if fs::read_to_string(&envrc).is_ok_and(|t| t.starts_with(GENERATED_MARKER)) {
            fs::remove_file(&envrc).with_context(|| format!("removing {}", envrc.display()))?;
        }
        if let Some(b) = &branch {
            // Off the branch, at the base that contains it, so that
            // `git branch -d` sees it merged.
            git(&c.dir, &["switch", "--quiet", "--detach", base])?;
            match git(&c.dir, &["branch", "--quiet", "-d", b]) {
                Ok(_) => report.outcome(
                    Tone::Good,
                    format!("deleted the merged branch {b} of {repo}"),
                ),
                Err(e) => report.warn(format!("kept the branch {b} of {repo}: {e:#}")),
            }
        }
        git(
            &rc.source,
            &["worktree", "remove", &c.dir.to_string_lossy()],
        )
        .with_context(|| format!("removing the worktree {}", c.dir.display()))?;
        report.outcome(Tone::Good, format!("removed {}", c.dir.display()));
    }
    if !kept.is_empty() {
        report.end(
            Tone::Warn,
            format!(
                "kept {} and the workspace {}",
                kept.join(", "),
                ws.root.display()
            ),
        );
        return Ok(());
    }
    if dry_run {
        report.end(Tone::Plain, format!("would remove {}", ws.root.display()));
        return Ok(());
    }
    let dot_swl = ws.root.join(".swl");
    if dot_swl.exists() {
        fs::remove_dir_all(&dot_swl).with_context(|| format!("removing {}", dot_swl.display()))?;
    }
    match fs::remove_dir(&ws.root) {
        Ok(()) => report.end(Tone::Good, format!("removed {}", ws.root.display())),
        Err(e) => report.end(
            Tone::Warn,
            format!("kept {}, which has other files: {e}", ws.root.display()),
        ),
    }
    Ok(())
}

/// Says what the next step waits for, `waits for <who>: <msg>`: the end of
/// the current section.
pub(crate) fn say_waits(report: &Report, waits: Waits, msg: &str) {
    let (who, tone) = match waits {
        Waits::Owner => ("the owner", Tone::Owner),
        Waits::Anyone => ("anyone", Tone::Warn),
        Waits::Nobody => ("nobody", Tone::Nobody),
    };
    report.end(tone, format!("waits for {who}: {msg}"));
}

/// The exit status of a run whose next step waits for `waits`.
pub(crate) fn waits_code(waits: Waits) -> u8 {
    match waits {
        Waits::Owner => EXIT_OWNER,
        Waits::Anyone => EXIT_ANYONE,
        Waits::Nobody => EXIT_NOBODY,
    }
}

/// The end of a run whose next step waits for `waits`: its exit status,
/// success for a dry run.
pub(crate) fn exit(report: &Report, waits: Waits, msg: &str, dry_run: bool) -> ExitCode {
    say_waits(report, waits, msg);
    if dry_run {
        return ExitCode::SUCCESS;
    }
    ExitCode::from(waits_code(waits))
}

/// The end of a dry run that reached a write.
pub(crate) fn planned(report: &Report, then: &str) -> ExitCode {
    report.end(
        Tone::Plain,
        format!("dry run, nothing was written; then: {then}"),
    );
    ExitCode::SUCCESS
}

/// The writer of a dry run, which writes nothing: it needs no fj login.
pub struct DryRunWriter;

impl ForgeWriter for DryRunWriter {
    fn set_title(&self, _: &str, _: u64, _: &str) -> Result<()> {
        bail!("a dry run writes nothing")
    }

    fn set_body(&self, _: &str, _: u64, _: &str) -> Result<()> {
        bail!("a dry run writes nothing")
    }

    fn request_merge(&self, _: &str, _: u64, _: &str, _: MergeWhen) -> Result<MergeOutcome> {
        bail!("a dry run writes nothing")
    }

    fn create_pr(&self, _: &str, _: &str, _: &str, _: &str, _: &str) -> Result<PrInfo> {
        bail!("a dry run writes nothing")
    }

    fn close_pr(&self, _: &str, _: u64) -> Result<CloseOutcome> {
        bail!("a dry run writes nothing")
    }
}

/// Lands the change of the workspace at `root`.
pub fn run(
    root: &Path,
    config: &Config,
    reader: &dyn PrReader,
    writer: &dyn ForgeWriter,
    opts: &Options,
) -> Result<ExitCode> {
    let lander = Lander::new("swl land", config, reader, writer, opts);
    let report = &lander.report;
    for pass in 0..MAX_STEPS {
        let _span = tracing::debug_span!("land", pass).entered();
        let ws = Workspace::scan(root)?;
        let rows = status::collect(&ws, config, reader, &Fetcher::new(true))?;
        let step = match lander.step(&ws, &rows) {
            Err(e) => match e.downcast_ref::<Repeated>() {
                Some(r) => Step::Stop(Waits::Nobody, r.0.clone()),
                None => return Err(e),
            },
            Ok(step) => step,
        };
        tracing::debug!(?step, "the pass ends");
        if step == Step::Done
            && let Some(why) = status::holds_no_change(&ws, &rows)
        {
            let msg = format!(
                "{why}; commit on a topic branch and swl publish the consumer, or join a \
                 published change with swl land --from <consumer>#<MR>"
            );
            report.leave();
            return Ok(exit(report, Waits::Anyone, &msg, opts.dry_run));
        }
        match step {
            Step::Progress => continue,
            Step::Planned(then) => return Ok(planned(report, &then)),
            Step::Stop(waits, msg) => return Ok(exit(report, waits, &msg, opts.dry_run)),
            Step::Done if !lander.landed_by_merge(&ws, &rows)? => {
                report.say(
                    Tone::Plain,
                    format!(
                        "nothing to land: no consumer of {} has a merged MR; the workspace \
                     stays",
                        ws.root.display()
                    ),
                );
                return Ok(ExitCode::SUCCESS);
            }
            Step::Done => {
                report.say(Tone::Good, "every MR of the change is merged");
                if opts.keep {
                    report.say(
                        Tone::Plain,
                        format!("keeping the workspace {} (--keep)", ws.root.display()),
                    );
                } else {
                    remove_workspace(report, &ws, config, opts.dry_run)?;
                }
                return Ok(ExitCode::SUCCESS);
            }
        }
    }
    report.leave();
    Ok(exit(
        report,
        Waits::Nobody,
        "too many steps in one run; rerun swl land",
        opts.dry_run,
    ))
}

/// `swl land --from <consumer>#<MR>`: lands in the workspace `swl start
/// --from` makes (or reuses). A landed MR (as `swl status --from` sees it)
/// has nothing left to do, unless `--workspace` names one to remove. With
/// `--dry-run` and no workspace yet, the plan for the shared MRs from the
/// forge.
#[allow(clippy::too_many_arguments)]
pub fn run_from(
    config: &Config,
    cwd: &Path,
    from: &str,
    workspace: Option<&Path>,
    reader: &dyn PrReader,
    writer: &dyn ForgeWriter,
    swl: &Path,
    opts: &Options,
) -> Result<ExitCode> {
    let (consumer, n) = start::parse_from(from)?;
    let fetcher = Fetcher::new(true);
    let rows = status::collect_from(config, from, reader, &fetcher)?;
    let row = rows
        .iter()
        .find(|r| r.name == consumer && r.follows.is_none());
    let landed = row.is_some_and(|r| r.verdict.stage == Stage::Landed);
    let promoting =
        row.is_some_and(|r| matches!(&r.facts, RowFacts::Consumer(f) if f.promotion.is_some()));
    let resolved = || {
        workspace
            .map(|w| crate::workspace_location::resolve(config, cwd, Some(w)))
            .transpose()
    };
    if promoting && !landed {
        return match resolved()?.as_deref().filter(|w| w.exists()) {
            Some(ws) => run(ws, config, reader, writer, opts),
            None => promote_from(config, from, rows, reader, writer, opts),
        };
    }
    if landed {
        let workspace = resolved()?;
        return match workspace.as_deref().filter(|w| w.exists()) {
            Some(ws) => run(ws, config, reader, writer, opts),
            None => {
                Report::new("swl land")
                    .say(Tone::Good, format!("{from} has landed: nothing left to do"));
                Ok(ExitCode::SUCCESS)
            }
        };
    }
    if opts.dry_run {
        let mr = start::read_consumer_mr(config, &consumer, n, &fetcher)?;
        let dir = crate::workspace_location::for_land_from(config, cwd, &mr.branch, workspace)?;
        if dir.exists() {
            return run(&dir, config, reader, writer, opts);
        }
        let lander = Lander::new("swl land", config, reader, writer, opts);
        let report = &lander.report;
        report.say(
            Tone::Plain,
            format!(
                "would run swl start --from {from} (the workspace {})",
                dir.display()
            ),
        );
        for mr in shared_entries(&[(consumer.clone(), mr.manifest)]) {
            match lander.shared(&rows, None, &mr)? {
                Step::Done => {}
                Step::Progress => unreachable!("a dry run writes nothing"),
                Step::Planned(then) => return Ok(planned(report, &then)),
                Step::Stop(waits, msg) => return Ok(exit(report, waits, &msg, true)),
            }
        }
        report.say(
            Tone::Plain,
            format!("then, in the workspace, finalize, push and merge {from}"),
        );
        return Ok(ExitCode::SUCCESS);
    }
    let mr = start::read_consumer_mr(config, &consumer, n, &fetcher)?;
    let dir = crate::workspace_location::for_land_from(config, cwd, &mr.branch, workspace)?;
    let root = start::run_from(config, cwd, from, Some(&dir), &fetcher, reader, swl)?.root;
    run(&root, config, reader, writer, opts)
}

/// `land --from` common's merged MR without a workspace: its promotion,
/// which needs none, until it has landed. `rows` are the first pass's.
fn promote_from(
    config: &Config,
    from: &str,
    mut rows: Vec<Row>,
    reader: &dyn PrReader,
    writer: &dyn ForgeWriter,
    opts: &Options,
) -> Result<ExitCode> {
    let lander = Lander::new("swl land", config, reader, writer, opts);
    let report = &lander.report;
    report.section(from);
    let (consumer, _) = start::parse_from(from)?;
    for pass in 0..MAX_STEPS {
        let _span = tracing::debug_span!("land", pass).entered();
        if pass > 0 {
            rows = status::collect_from(config, from, reader, &Fetcher::new(true))?;
        }
        let Some(row) = rows
            .iter()
            .find(|r| r.name == consumer && r.follows.is_none())
        else {
            bail!("{from}: no status");
        };
        let step = match &row.facts {
            _ if row.verdict.stage == Stage::Landed => Step::Done,
            RowFacts::Consumer(ConsumerFacts {
                promotion: Some(p), ..
            }) => match lander.promote_consumer(from, p) {
                Err(e) => match e.downcast_ref::<Repeated>() {
                    Some(r) => Step::Stop(Waits::Nobody, r.0.clone()),
                    None => return Err(e),
                },
                Ok(step) => step,
            },
            _ => stop_at(row)?,
        };
        match step {
            Step::Progress => continue,
            Step::Planned(then) => return Ok(planned(report, &then)),
            Step::Stop(waits, msg) => return Ok(exit(report, waits, &msg, opts.dry_run)),
            Step::Done => {
                report.end(Tone::Good, format!("{from} has landed: nothing left to do"));
                return Ok(ExitCode::SUCCESS);
            }
        }
    }
    report.leave();
    Ok(exit(
        report,
        Waits::Nobody,
        "too many steps in one run; rerun swl land",
        opts.dry_run,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(text: &str) -> Manifest {
        Manifest::parse(text).unwrap()
    }

    #[test]
    fn push_rejections() {
        assert_eq!(
            push_rejection(" ! [rejected]        abc -> current (non-fast-forward)"),
            Some(Rejection::Moved)
        );
        assert_eq!(
            push_rejection(" ! [rejected]        abc -> current (fetch first)"),
            Some(Rejection::Moved)
        );
        assert_eq!(
            push_rejection(
                "git push failed: remote: current is protected\n ! [remote rejected] abc -> current (pre-receive hook declined)"
            ),
            Some(Rejection::Refused(
                "remote: current is protected; ! [remote rejected] abc -> current (pre-receive hook declined)".into()
            ))
        );
        assert_eq!(
            push_rejection("fatal: unable to access 'https://x/': Could not resolve host"),
            None
        );
    }

    #[test]
    fn disposable_files() {
        let tmp = crate::tempdir::TempDir::new("swl-land-test").unwrap();
        let d = tmp.path();
        for (path, ignored, ok) in [
            (".direnv/", true, true),
            ("result", true, true),
            ("result-1", true, true),
            (".swl/", true, true),
            ("progress.md", true, false),
            ("sub/result", true, false),
            (".direnv/", false, false),
            (".envrc.local", false, false),
        ] {
            assert_eq!(disposable(d, path, ignored), ok, "{path}");
        }
        fs::write(d.join(".envrc.local"), format!("{GENERATED_MARKER} x\n")).unwrap();
        assert!(disposable(d, ".envrc.local", false));
    }

    #[test]
    fn shared_entries_nested_first_and_once() {
        let a = manifest(
            "[inputs.common]\nrepo = \"swan-lake-common\"\nmr = 24\n\
             [inputs.\"common/zwanenzang\"]\nrepo = \"zwanenzang\"\nmr = 7\n\
             [inputs.common-next]\nrepo = \"swan-lake-common\"\nmr = 30\n",
        );
        let b = manifest("[inputs.common]\nrepo = \"swan-lake-common\"\nmr = 24\n");
        let got: Vec<String> = shared_entries(&[("a".into(), a), ("b".into(), b)])
            .into_iter()
            .map(|(c, p, e)| format!("{c}: {p} {}#{}", e.repo, e.mr))
            .collect();
        assert_eq!(
            got,
            [
                "a: common/zwanenzang zwanenzang#7",
                "a: common swan-lake-common#24",
                "a: common-next swan-lake-common#30",
            ]
        );
    }
}

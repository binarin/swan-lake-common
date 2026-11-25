//! `swl bump`: moves common's inputs forward and the consumers' `common`
//! after them. Stateless, like `land`: each run reads the state from the
//! repos' sources (fetched), the locks, the forge and the mirror, does what it
//! can, and stops at the first step it cannot do, saying what it waits for
//! and for whom. Rerun it to continue.
//!
//! 1. Common, from its `origin/master`: each root input whose declared
//!    branch (the forks' `swan-lake-current…`, the default branch without a
//!    ref) has moved forward of the locked rev is relocked to that branch's
//!    head; an input whose head does not descend from the locked rev is left
//!    and reported. One commit of `flake.lock`, pushed to master directly
//!    (fast-forward only), which only the owner may do.
//! 2. The branch the consumers declare for common (`swan-lake-current`) is
//!    promoted to master by fast-forward, and the declared source (the
//!    GitHub mirror) waited for, as `land` does.
//! 3. Each consumer: a bump MR from the swl-owned branch [`BUMP_BRANCH`]
//!    that moves only `common` to the promoted rev, merged as `land` merges.
//!
//! Everything runs in scratch worktrees of the repos' configured sources,
//! removed at the end. `--dry-run` writes nothing.

use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    process::ExitCode,
};

use anyhow::{Context, Result, anyhow, bail};
use serde_json::Value;

use crate::{
    ancestry::{self, History, Source},
    check,
    config::Config,
    forge::{ForgeWriter, PrInfo, PrReader, PrState},
    git::{git, git_ok, is_rev, short, valid_branch},
    land::{self, Lander, Rejection, Repeated, Step, push_rejection, stop},
    lock_change, lock_rules,
    nix::Nix,
    pin_policy, start,
    status::Waits,
    style::{self, Tone},
    tempdir::TempDir,
    ws_add::{Fetcher, base_remote},
};

const CMD: &str = "swl bump";

/// The shared repo whose inputs are bumped, and which the consumers pin.
pub const COMMON_REPO: &str = "swan-lake-common";

/// The branch of a consumer's bump MR. swl owns it: it updates it (with a
/// lease on the head it read) only while it has nothing but its own commit.
pub const BUMP_BRANCH: &str = "swl-bump-common";

/// The subject of common's bump commit starts so.
const COMMON_SUBJECT: &str = "swl: bump ";
/// The subject (and MR title) of a consumer's bump commit starts so.
const CONSUMER_SUBJECT: &str = "swl: bump common to ";

pub struct Options {
    /// `dry_run`, and how long to wait for the mirror and the forge.
    pub land: land::Options,
    /// `nix flake check --no-build` on each relocked tree before committing.
    pub check: bool,
}

/// A worktree of a repo's source, detached at a revision in a scratch
/// directory; removed on drop.
struct Scratch {
    source: PathBuf,
    dir: PathBuf,
    _tmp: TempDir,
}

impl Scratch {
    fn new(source: &Path, name: &str, rev: &str) -> Result<Self> {
        let tmp = TempDir::new("swl-bump")?;
        let dir = tmp.path().join(name);
        git(
            source,
            &[
                "worktree",
                "add",
                "--quiet",
                "--detach",
                &dir.to_string_lossy(),
                rev,
            ],
        )?;
        Ok(Self {
            source: source.to_path_buf(),
            dir,
            _tmp: tmp,
        })
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let dir = self.dir.to_string_lossy();
        if git(&self.source, &["worktree", "remove", "--force", &dir]).is_err() {
            let _ = fs::remove_dir_all(&self.dir);
            let _ = git(&self.source, &["worktree", "prune"]);
        }
    }
}

fn rev_parse(dir: &Path, refname: &str) -> Option<String> {
    git_ok(
        dir,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{refname}^{{commit}}"),
        ],
    )
}

fn is_ancestor(dir: &Path, a: &str, b: &str) -> bool {
    git_ok(dir, &["merge-base", "--is-ancestor", a, b]).is_some()
}

/// The branch of a base such as `origin/master`.
fn base_branch(base: &str) -> &str {
    base.split_once('/').map_or(base, |(_, b)| b)
}

/// A commit of swl bump's own: one parent, only `flake.lock`, a subject
/// with `prefix`.
fn own_commit(dir: &Path, sha: &str, prefix: &str) -> Result<bool> {
    let line = git(dir, &["log", "-1", "--format=%P%x00%s", sha])?;
    let (parents, subject) = line.split_once('\0').unwrap_or((&line, ""));
    let files = git(
        dir,
        &["diff-tree", "--no-commit-id", "--name-only", "-r", sha],
    )?;
    Ok(parents.split_whitespace().count() == 1
        && subject.starts_with(prefix)
        && files == "flake.lock")
}

/// The locked rev at an input path of a lock.
fn locked_rev(lock: &Value, path: &str) -> Option<String> {
    let key = lock_change::resolve_path(lock, path).ok()?;
    lock.pointer(&format!("/nodes/{key}/locked/rev"))?
        .as_str()
        .map(str::to_string)
}

/// A relock: the lock nix makes for the flake in `dir` from `old` with each
/// moved input `(name, its locked source, the new rev)` locked at that rev,
/// as `swl check` relocks to compare; and the bytes nix wrote. Nothing else
/// may change, and each input must be at its new rev.
fn relock(
    nix: &Nix,
    dir: &Path,
    old: &Value,
    moves: &[(String, Value, String)],
) -> Result<Vec<u8>> {
    let mut reference = old.clone();
    let root = old["root"].as_str().unwrap_or("root").to_string();
    if let Some(inputs) = reference["nodes"][&root]["inputs"].as_object_mut() {
        for (name, _, _) in moves {
            inputs.remove(name);
        }
    }
    let mut overrides = Vec::new();
    for (name, locked, rev) in moves {
        let mut at = locked.clone();
        at["rev"] = rev.as_str().into();
        let flake_ref = lock_rules::locked_ref(&at).map_err(|e| anyhow!("{name}: {e}"))?;
        overrides.push((name.clone(), flake_ref));
    }
    let tmp = TempDir::new("swl-bump-lock")?;
    let (reference_path, output) = (
        tmp.path().join("reference.lock"),
        tmp.path().join("flake.lock"),
    );
    fs::write(&reference_path, serde_json::to_vec_pretty(&reference)?)?;
    nix.lock_from(dir, &reference_path, &overrides, &output)?;
    let bytes = fs::read(&output).context("nix wrote no lock")?;
    let new: Value = serde_json::from_slice(&bytes).context("parsing the lock nix wrote")?;
    for (name, _, rev) in moves {
        let got = locked_rev(&new, name);
        if got.as_deref() != Some(rev.as_str()) {
            bail!(
                "{name} was locked at {}, not at {}",
                got.as_deref().map_or("(nothing)", short),
                short(rev)
            );
        }
    }
    let names: Vec<String> = moves.iter().map(|(n, _, _)| n.clone()).collect();
    lock_change::verify(old, &new, &names)
        .context("the relock changed more than the moved inputs")?;
    Ok(bytes)
}

/// Writes the relocked lock into the worktree, checks it with `nix flake
/// check --no-build` if asked, and commits it; the new HEAD.
fn commit_lock(
    nix: &Nix,
    dir: &Path,
    bytes: &[u8],
    check: bool,
    subject: &str,
    body: &str,
) -> Result<String> {
    fs::write(dir.join("flake.lock"), bytes).context("writing flake.lock")?;
    if check {
        let dir_s = dir.to_string_lossy();
        nix.run(&[
            "flake",
            "check",
            "--no-build",
            "--no-write-lock-file",
            &dir_s,
        ])
        .context("checking the relocked flake")?;
    }
    git(
        dir,
        &[
            "commit",
            "--quiet",
            "-m",
            subject,
            "-m",
            body,
            "--",
            "flake.lock",
        ],
    )?;
    git(dir, &["rev-parse", "HEAD"])
}

/// How one of common's inputs stands against its declared branch.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Judged {
    Current,
    /// The branch's head descends from the locked rev.
    Forward(String),
    /// The branch's head does not descend from the locked rev (a rebased
    /// fork): left.
    NotForward(String),
    /// Not judged, with the reason: left.
    Unknown(String),
}

#[derive(Debug)]
struct InputPlan {
    name: String,
    /// The lock node's `locked`, and its rev.
    locked: Value,
    old: String,
    /// The declared branch, for messages.
    branch: String,
    judged: Judged,
}

/// One of a consumer's root inputs of common.
#[derive(Debug)]
struct CommonInput {
    name: String,
    locked: Value,
    rev: String,
    /// The declared ref; `None` for the default branch.
    branch: Option<String>,
    /// The declared source's git URL (the GitHub mirror).
    declared_source: Option<String>,
}

/// A repo of the config that pins common: its source, the head of its base
/// branch on the forge, and its lock there.
#[derive(Debug)]
struct Consumer {
    name: String,
    source: PathBuf,
    remote: String,
    branch: String,
    head: String,
    lock: Value,
    inputs: Vec<CommonInput>,
}

/// The root inputs of `lock` that are common: direct nodes of the repo by
/// both their sources, other than the experiment inputs (`common-next…`).
fn common_inputs(config: &Config, lock: &Value) -> Result<Vec<CommonInput>> {
    let mut out = Vec::new();
    for (name, edge) in lock_change::root_inputs(lock)? {
        let Some(key) = edge.as_str() else { continue };
        if pin_policy::is_next_name(&name) {
            continue;
        }
        let node = &lock["nodes"][key];
        if !check::node_names_repo(config, node, COMMON_REPO) {
            continue;
        }
        let rev = node
            .pointer("/locked/rev")
            .and_then(Value::as_str)
            .filter(|r| is_rev(r))
            .ok_or_else(|| anyhow!("{name}: no full locked rev"))?;
        let original = &node["original"];
        out.push(CommonInput {
            name,
            locked: node["locked"].clone(),
            rev: rev.to_string(),
            branch: original
                .get("ref")
                .and_then(Value::as_str)
                .map(str::to_string),
            declared_source: ancestry::git_url(original),
        });
    }
    Ok(out)
}

/// How each of common's root inputs stands against the head of its
/// declared ref (from its lock node's `original`; the default branch
/// without one), by `ls-remote`, and whether that is a forward move, asked
/// of its source as `swl check` does.
fn plan_inputs(lock: &Value, history: &dyn History) -> Result<Vec<InputPlan>> {
    let mut plans = Vec::new();
    for (name, edge) in lock_change::root_inputs(lock)? {
        let Some(key) = edge.as_str() else { continue };
        let node = &lock["nodes"][key];
        let locked = node.get("locked").cloned().unwrap_or(Value::Null);
        let original = node.get("original").cloned().unwrap_or(Value::Null);
        let old = locked
            .get("rev")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let declared_ref = original.get("ref").and_then(Value::as_str);
        let branch = declared_ref.unwrap_or("the default branch").to_string();
        let judged = judge(&locked, &original, &old, declared_ref, history);
        tracing::debug!(%name, %old, ?judged, "common's input against its declared branch");
        plans.push(InputPlan {
            name,
            locked,
            old,
            branch,
            judged,
        });
    }
    Ok(plans)
}

fn judge(
    locked: &Value,
    original: &Value,
    old: &str,
    declared_ref: Option<&str>,
    history: &dyn History,
) -> Judged {
    if original.get("rev").is_some() {
        return Judged::Unknown("flake.nix pins it to a rev".into());
    }
    if !is_rev(old) {
        return Judged::Unknown("no full locked rev".into());
    }
    let source = match Source::of(locked) {
        Ok(s) => s,
        Err(e) => return Judged::Unknown(e),
    };
    let Some(url) = ancestry::git_url(locked) else {
        return Judged::Unknown("not a git source".into());
    };
    let refname = match declared_ref {
        None => "HEAD".to_string(),
        Some(r) if r.starts_with("refs/") => r.to_string(),
        Some(r) if valid_branch(r) => format!("refs/heads/{r}"),
        Some(r) => return Judged::Unknown(format!("its ref {r:?} is not a branch name")),
    };
    let tip = match ancestry::remote_head(&url, &refname) {
        Ok(t) => t,
        Err(e) => return Judged::Unknown(format!("cannot read {refname} of {url}: {e}")),
    };
    if tip == old {
        return Judged::Current;
    }
    match history.contains(&source, old, &tip) {
        Ok(true) => Judged::Forward(tip),
        Ok(false) => Judged::NotForward(tip),
        Err(e) => Judged::Unknown(format!(
            "cannot determine whether {} -> {} is a forward move: {e}",
            short(old),
            short(&tip)
        )),
    }
}

/// Says how common's inputs stand.
fn report_plan(plans: &[InputPlan]) {
    let current: Vec<&str> = plans
        .iter()
        .filter(|p| p.judged == Judged::Current)
        .map(|p| p.name.as_str())
        .collect();
    if !current.is_empty() {
        style::say(
            CMD,
            Tone::Plain,
            format!("common: up to date: {}", current.join(", ")),
        );
    }
    for p in plans {
        let (old, name) = (short(&p.old), &p.name);
        match &p.judged {
            Judged::Current => {}
            Judged::Forward(new) => style::say(
                CMD,
                Tone::Plain,
                format!("common: {name} {old} -> {} ({})", short(new), p.branch),
            ),
            Judged::NotForward(new) => style::say(
                CMD,
                Tone::Warn,
                format!(
                    "common: {name}: not bumped: {old} -> {} ({}) is not a forward move, the new \
                     rev does not descend from the locked one; left at {old}",
                    short(new),
                    p.branch
                ),
            ),
            Judged::Unknown(why) => style::say(
                CMD,
                Tone::Warn,
                format!("common: {name}: not bumped: {why}; left at {old}"),
            ),
        }
    }
}

/// Where common stands for the consumers.
enum Common {
    /// Promoted and in the declared sources: the rev to bump them to.
    Ready(String),
    /// The run stops here.
    Stopped(Step),
}

struct Bumper<'a> {
    config: &'a Config,
    reader: &'a dyn PrReader,
    writer: &'a dyn ForgeWriter,
    history: &'a dyn History,
    opts: &'a Options,
    lander: Lander<'a>,
    fetcher: Fetcher,
    nix: std::cell::OnceCell<Nix>,
}

impl Bumper<'_> {
    fn nix(&self) -> Result<&Nix> {
        if self.nix.get().is_none() {
            let _ = self.nix.set(Nix::find()?);
        }
        Ok(self.nix.get().expect("set above"))
    }

    fn dry_run(&self) -> bool {
        self.opts.land.dry_run
    }

    /// The repos of the config other than common whose base branch locks
    /// common, each fetched.
    fn consumers(&self) -> Result<Vec<Consumer>> {
        let mut out = Vec::new();
        for (name, rc) in self.config.repos() {
            if name == COMMON_REPO {
                continue;
            }
            let base = rc.base();
            let Some(remote) = base_remote(&rc.source, base)? else {
                tracing::debug!(%name, %base, "not a consumer: its base names no remote");
                continue;
            };
            self.fetcher.fetch(&rc.source, &remote)?;
            let branch = base_branch(base).to_string();
            let Some(head) = rev_parse(&rc.source, &format!("refs/remotes/{remote}/{branch}"))
            else {
                bail!("{name}: {base} is not known in {}", rc.source.display());
            };
            let Some(lock) = check::lock_at(&rc.source, &head)? else {
                tracing::debug!(%name, "not a consumer: no flake.lock");
                continue;
            };
            let inputs = common_inputs(self.config, &lock)
                .with_context(|| format!("{name}'s flake.lock"))?;
            if inputs.is_empty() {
                tracing::debug!(%name, "not a consumer: no common input");
                continue;
            }
            out.push(Consumer {
                name: name.to_string(),
                source: rc.source.clone(),
                remote,
                branch,
                head,
                lock,
                inputs,
            });
        }
        Ok(out)
    }

    /// Steps 1 and 2: common bumped, promoted to `promoted` and in the
    /// declared sources.
    fn common(&self, promoted: &str, mirrors: &BTreeSet<String>) -> Result<Common> {
        let rc = self.config.repo(COMMON_REPO)?;
        let src = &rc.source;
        let base = rc.base();
        let Some(remote) = base_remote(src, base)? else {
            bail!(
                "{COMMON_REPO}: its base {base} names no remote of {}",
                src.display()
            );
        };
        let master = base_branch(base);
        self.fetcher.fetch(src, &remote)?;
        let tracking = |b: &str| {
            rev_parse(src, &format!("refs/remotes/{remote}/{b}")).ok_or_else(|| {
                anyhow!(
                    "{COMMON_REPO}: {remote}/{b} is not known in {}",
                    src.display()
                )
            })
        };
        let head = tracking(master)?;
        let current = tracking(promoted)?;
        tracing::debug!(%head, %current, "common's master and its promoted branch");
        if !is_ancestor(src, &current, &head) {
            return Ok(Common::Stopped(stop(
                Waits::Owner,
                format!(
                    "common: {promoted} at {} has commits that {master} at {} does not; swl bump \
                     moves {promoted} by fast-forward only",
                    short(&current),
                    short(&head)
                ),
            )?));
        }
        let unpromoted = git(
            src,
            &["rev-list", "--reverse", &format!("{current}..{head}")],
        )?;
        let unpromoted: Vec<&str> = unpromoted.lines().collect();
        let mut foreign = Vec::new();
        for c in &unpromoted {
            if !own_commit(src, c, COMMON_SUBJECT)? {
                foreign.push(git(src, &["log", "-1", "--format=%h %s", c])?);
            }
        }
        if !foreign.is_empty() {
            return Ok(Common::Stopped(stop(
                Waits::Owner,
                format!(
                    "common: {master} at {} has {} commit(s) not promoted to {promoted} that are \
                     not swl bump's ({}); promote them first (git -C {} push {remote} \
                     {head}:refs/heads/{promoted}), then rerun {CMD}",
                    short(&head),
                    foreign.len(),
                    foreign.join("; "),
                    src.display()
                ),
            )?));
        }
        let target = if !unpromoted.is_empty() {
            style::say(
                CMD,
                Tone::Plain,
                format!(
                    "common: {master} at {} has swl bump's commit, not promoted to {promoted} yet",
                    short(&head)
                ),
            );
            head
        } else {
            match self.bump_common(src, &remote, master, &head, promoted, mirrors)? {
                Ok(target) => target,
                Err(step) => return Ok(Common::Stopped(step)),
            }
        };
        let which = format!("{COMMON_REPO} {master}");
        if target != current {
            match self
                .lander
                .promote(&which, COMMON_REPO, &target, promoted)?
            {
                Step::Progress => style::say(
                    CMD,
                    Tone::Good,
                    format!("common: promoted {promoted} to {}", short(&target)),
                ),
                step => return Ok(Common::Stopped(step)),
            }
        }
        for url in mirrors {
            match self.lander.mirror(&which, url, promoted, &target)? {
                Step::Done => {}
                step => return Ok(Common::Stopped(step)),
            }
        }
        Ok(Common::Ready(target))
    }

    /// Step 1 on common's master at `head`: the forward moves relocked,
    /// committed and pushed; the new master, or `head` with nothing to bump.
    fn bump_common(
        &self,
        src: &Path,
        remote: &str,
        master: &str,
        head: &str,
        promoted: &str,
        mirrors: &BTreeSet<String>,
    ) -> Result<Result<String, Step>> {
        let Some(old) = check::lock_at(src, head)? else {
            bail!("{COMMON_REPO} has no flake.lock at {}", short(head));
        };
        let plans = plan_inputs(&old, self.history)?;
        report_plan(&plans);
        let moves: Vec<(String, Value, String)> = plans
            .iter()
            .filter_map(|p| match &p.judged {
                Judged::Forward(new) => Some((p.name.clone(), p.locked.clone(), new.clone())),
                _ => None,
            })
            .collect();
        if moves.is_empty() {
            style::say(
                CMD,
                Tone::Plain,
                format!("common: nothing to bump at {}", short(head)),
            );
            return Ok(Ok(head.to_string()));
        }
        let names: Vec<&str> = moves.iter().map(|(n, _, _)| n.as_str()).collect();
        let subject = format!("{COMMON_SUBJECT}{}", names.join(", "));
        let checked = if self.opts.check {
            ", checked with nix flake check --no-build,"
        } else {
            ""
        };
        let what = format!(
            "relock {} on {master} at {}{checked} commit {subject:?} and push it to {remote}'s \
             {master} (fast-forward)",
            names.join(", "),
            short(head)
        );
        if !self.lander.act("push common", &what)? {
            let then = format!(
                "promote {promoted}{}",
                if mirrors.is_empty() {
                    ""
                } else {
                    ", wait for its declared source"
                }
            );
            return Ok(Err(Step::Planned(then)));
        }
        let nix = self.nix()?;
        let scratch = Scratch::new(src, COMMON_REPO, head)?;
        let bytes = relock(nix, &scratch.dir, &old, &moves)?;
        let body: String = plans
            .iter()
            .filter_map(|p| match &p.judged {
                Judged::Forward(new) => Some(format!(
                    "{}: {} -> {} ({})\n",
                    p.name,
                    short(&p.old),
                    short(new),
                    p.branch
                )),
                Judged::NotForward(new) => Some(format!(
                    "{}: left at {}: {} is not a forward move\n",
                    p.name,
                    short(&p.old),
                    short(new)
                )),
                _ => None,
            })
            .collect();
        let new_head = commit_lock(
            nix,
            &scratch.dir,
            &bytes,
            self.opts.check,
            &subject,
            body.trim_end(),
        )?;
        let refspec = format!("HEAD:refs/heads/{master}");
        match git(&scratch.dir, &["push", "--quiet", remote, &refspec]) {
            Ok(_) => {
                style::say(
                    CMD,
                    Tone::Good,
                    format!("common: pushed {} to {master}: {subject}", short(&new_head)),
                );
                Ok(Ok(new_head))
            }
            Err(e) => match push_rejection(&format!("{e:#}")) {
                None => Err(e.context("pushing common's bump")),
                Some(Rejection::Moved) => Ok(Err(stop(
                    Waits::Nobody,
                    format!("common: {remote}'s {master} moved since it was fetched; rerun {CMD}"),
                )?)),
                Some(Rejection::Refused(reason)) => Ok(Err(stop(
                    Waits::Owner,
                    format!(
                        "push common's bump to {master}: rerun {CMD} as the owner, or git -C {} \
                         push {remote} {new_head}:refs/heads/{master} (the push was refused: \
                         {reason})",
                        src.display()
                    ),
                )?)),
            },
        }
    }

    /// The open bump MR whose head is `sha`, by the forge's pull refs.
    fn open_mr_at(&self, c: &Consumer, sha: &str) -> Result<Option<PrInfo>> {
        let refs = git(&c.source, &["ls-remote", &c.remote, "refs/pull/*/head"])?;
        for (at, refname) in start::parse_ls_remote(&refs) {
            let n = refname
                .strip_prefix("refs/pull/")
                .and_then(|r| r.strip_suffix("/head"))
                .and_then(|n| n.parse::<u64>().ok());
            let (true, Some(n)) = (at == sha, n) else {
                continue;
            };
            if let Some(info) = self.reader.pr(&c.name, n)?
                && info.state == PrState::Open
                && info.head_ref == BUMP_BRANCH
            {
                return Ok(Some(info));
            }
        }
        Ok(None)
    }

    /// Step 3 for one consumer: its bump MR to `target`, pushed, opened and
    /// merged.
    fn consumer(&self, c: &Consumer, target: &str, common_src: &Path) -> Result<Step> {
        let _span = tracing::debug_span!("consumer", name = %c.name).entered();
        let name = &c.name;
        let mut moves = Vec::new();
        for i in &c.inputs {
            if i.rev == target {
                continue;
            }
            if git_ok(
                common_src,
                &["cat-file", "-e", &format!("{}^{{commit}}", i.rev)],
            )
            .is_none()
            {
                style::say(
                    CMD,
                    Tone::Warn,
                    format!(
                        "{name}: {} locks {}, which {COMMON_REPO} does not have; not bumped",
                        i.name,
                        short(&i.rev)
                    ),
                );
            } else if !is_ancestor(common_src, &i.rev, target) {
                style::say(
                    CMD,
                    Tone::Warn,
                    format!(
                        "{name}: not bumped: {} {} -> {} is not a forward move; left at {}",
                        i.name,
                        short(&i.rev),
                        short(target),
                        short(&i.rev)
                    ),
                );
            } else {
                moves.push((i.name.clone(), i.locked.clone(), target.to_string()));
            }
        }
        let remote_tip = git(
            &c.source,
            &["ls-remote", &c.remote, &format!("refs/heads/{BUMP_BRANCH}")],
        )?;
        let tip = remote_tip.split_whitespace().next().map(str::to_string);
        if moves.is_empty() {
            if c.inputs.iter().all(|i| i.rev == target) {
                style::say(
                    CMD,
                    Tone::Good,
                    format!("{name}: common is at {} already", short(target)),
                );
            }
            if let Some(tip) = &tip {
                style::say(
                    CMD,
                    Tone::Plain,
                    format!(
                        "{name}: {BUMP_BRANCH} (at {}) is not needed any more",
                        short(tip)
                    ),
                );
            }
            return Ok(Step::Done);
        }
        let title = format!("{CONSUMER_SUBJECT}{}", short(target));
        let mut mr = None;
        let mut up_to_date = false;
        if let Some(tip) = &tip {
            if rev_parse(&c.source, tip).is_none() {
                let refspec = format!("refs/heads/{BUMP_BRANCH}");
                git(&c.source, &["fetch", "--quiet", &c.remote, &refspec])?;
            }
            if !self.own_branch(c, tip)? {
                return stop(
                    Waits::Anyone,
                    format!(
                        "{name}: {BUMP_BRANCH} at {} has commits other than swl bump's own; swl \
                         bump updates that branch only while it has nothing but its bump commit: \
                         delete it on the forge, then rerun {CMD}",
                        short(tip)
                    ),
                );
            }
            let lock = check::lock_at(&c.source, tip)?;
            up_to_date = git_ok(&c.source, &["rev-parse", &format!("{tip}^")]).as_deref()
                == Some(c.head.as_str())
                && lock.is_some_and(|l| {
                    c.inputs
                        .iter()
                        .all(|i| locked_rev(&l, &i.name).as_deref() == Some(target))
                });
            mr = self.open_mr_at(c, tip)?;
        }
        tracing::debug!(?tip, up_to_date, mr = ?mr.as_ref().map(|m| m.number), "the bump branch");
        let mut head = tip.clone().unwrap_or_default();
        // Kept until the forge has the pushed head, for await_pull_head.
        let mut _scratch = None;
        if !up_to_date {
            let what = format!(
                "push {name}'s bump of {} to {} as {}'s {BUMP_BRANCH}{}",
                moves
                    .iter()
                    .map(|(n, _, _)| n.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
                short(target),
                c.remote,
                match &tip {
                    Some(t) => format!(" (replacing its own commit {})", short(t)),
                    None => String::new(),
                }
            );
            if !self.lander.act(&format!("push {name}"), &what)? {
                return Ok(Step::Planned(format!(
                    "{} the bump MR of {name} and merge it",
                    if mr.is_some() { "update" } else { "open" }
                )));
            }
            let scratch = Scratch::new(&c.source, name, &c.head)?;
            head = self.build(c, &scratch, &moves, &title)?;
            let lease = format!(
                "--force-with-lease=refs/heads/{BUMP_BRANCH}:{}",
                tip.as_deref().unwrap_or("")
            );
            let refspec = format!("HEAD:refs/heads/{BUMP_BRANCH}");
            if let Err(e) = git(
                &scratch.dir,
                &["push", "--quiet", &lease, &c.remote, &refspec],
            ) {
                return match push_rejection(&format!("{e:#}")) {
                    None => Err(e.context(format!("pushing {name}'s bump"))),
                    Some(Rejection::Moved) => stop(
                        Waits::Nobody,
                        format!("{name}: {BUMP_BRANCH} moved since it was read; rerun {CMD}"),
                    ),
                    Some(Rejection::Refused(reason)) => stop(
                        Waits::Owner,
                        format!("{name}: the push to {BUMP_BRANCH} was refused: {reason}"),
                    ),
                };
            }
            if let Some(info) = &mr {
                let which = format!("{name}#{}", info.number);
                match self
                    .lander
                    .await_pull_head(&scratch.dir, &c.remote, &which, info.number)?
                {
                    Step::Progress => {}
                    step => return Ok(step),
                }
            }
            _scratch = Some(scratch);
        }
        let info = match mr {
            Some(info) => match self.reader.pr(name, info.number)? {
                Some(fresh) => fresh,
                None => bail!(
                    "{name}#{}: the forge does not show it any more",
                    info.number
                ),
            },
            None => {
                let body = format!(
                    "Moves {} to {}, the promoted {COMMON_REPO}.\n\nOpened by `{CMD}`, which \
                     updates `{BUMP_BRANCH}` (with a lease on the head it read) while it has \
                     nothing but its own commit.",
                    c.inputs
                        .iter()
                        .map(|i| i.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", "),
                    short(target)
                );
                let what = format!("open the bump MR {title:?} of {name}");
                if !self.lander.act(&format!("open {name}"), &what)? {
                    return Ok(Step::Planned(format!("merge the bump MR of {name}")));
                }
                self.writer
                    .create_pr(name, &c.branch, BUMP_BRANCH, &title, &body)?
            }
        };
        let which = format!("{name}#{}", info.number);
        if info.head_sha != head {
            return stop(
                Waits::Nobody,
                format!(
                    "{which}: the forge has its head at {}, not {} yet; rerun {CMD}",
                    short(&info.head_sha),
                    short(&head)
                ),
            );
        }
        if info.title != title
            && self.lander.act(
                &format!("title {which}"),
                &format!("retitle {which} {title:?}"),
            )?
        {
            self.writer.set_title(name, info.number, &title)?;
        }
        let rebase = format!("rerun {CMD}, which rebuilds {BUMP_BRANCH} on {}", c.branch);
        match self.lander.merge(&which, name, &info, &rebase)? {
            Step::Progress => {
                style::say(
                    CMD,
                    Tone::Good,
                    format!("{name}: common is at {}", short(target)),
                );
                Ok(Step::Done)
            }
            step => Ok(step),
        }
    }

    /// Whether the bump branch at `tip` has nothing but swl bump's own
    /// commit on the consumer's base: it is in the base, or it is one own
    /// commit on a commit of the base.
    fn own_branch(&self, c: &Consumer, tip: &str) -> Result<bool> {
        if is_ancestor(&c.source, tip, &c.head) {
            return Ok(true);
        }
        let commits = git(&c.source, &["rev-list", &format!("{}..{tip}", c.head)])?;
        if commits.lines().count() != 1 || !own_commit(&c.source, tip, CONSUMER_SUBJECT)? {
            return Ok(false);
        }
        Ok(is_ancestor(&c.source, &format!("{tip}^"), &c.head))
    }

    /// The bump commit in the scratch worktree of the consumer's base: its
    /// common inputs relocked to the target as `swl check` relocks, checked
    /// as `finalize` checks a landing; the new HEAD.
    fn build(
        &self,
        c: &Consumer,
        scratch: &Scratch,
        moves: &[(String, Value, String)],
        subject: &str,
    ) -> Result<String> {
        let nix = self.nix()?;
        let bytes = relock(nix, &scratch.dir, &c.lock, moves)
            .with_context(|| format!("relocking {}", c.name))?;
        let new: Value = serde_json::from_slice(&bytes)?;
        let decls = nix.declarations(&scratch.dir)?;
        pin_policy::check(&decls, &new, &|url| nix.parse_flake_ref(url)).with_context(|| {
            format!(
                "{}: the relocked flake.lock breaks the common pin policy",
                c.name
            )
        })?;
        let body: String = moves
            .iter()
            .map(|(name, _, rev)| {
                let old = locked_rev(&c.lock, name).unwrap_or_default();
                format!("{name}: {} -> {}\n", short(&old), short(rev))
            })
            .collect();
        commit_lock(
            nix,
            &scratch.dir,
            &bytes,
            self.opts.check,
            subject,
            body.trim_end(),
        )
    }

    fn run(&self) -> Result<ExitCode> {
        let common_rc = self.config.repo(COMMON_REPO)?;
        let consumers = self.consumers()?;
        if consumers.is_empty() {
            bail!(
                "no repo of the swl config locks {COMMON_REPO}: swl bump needs its consumers, \
                 whose flake.nix declares the branch to promote"
            );
        }
        let master = base_branch(common_rc.base());
        let mut branches = BTreeSet::new();
        let mut mirrors = BTreeSet::new();
        for c in &consumers {
            for i in &c.inputs {
                branches.insert(i.branch.clone().unwrap_or_else(|| master.to_string()));
                mirrors.extend(i.declared_source.clone());
            }
        }
        let promoted = match branches.into_iter().collect::<Vec<_>>().as_slice() {
            [one] => one.clone(),
            many => bail!(
                "the consumers declare {COMMON_REPO} at different branches ({}); swl bump \
                 promotes one",
                many.join(", ")
            ),
        };
        if !valid_branch(&promoted) {
            bail!("the declared branch {promoted:?} is not a branch name");
        }
        let names: Vec<&str> = consumers.iter().map(|c| c.name.as_str()).collect();
        tracing::debug!(%promoted, ?mirrors, consumers = ?names, "what bump works on");
        let target = match self.common(&promoted, &mirrors)? {
            Common::Ready(target) => target,
            Common::Stopped(Step::Planned(then)) => {
                return Ok(land::planned(
                    CMD,
                    &format!("{then}; then the bump MRs of {}", names.join(", ")),
                ));
            }
            Common::Stopped(Step::Stop(waits, msg)) => return Ok(self.end(&[(waits, msg)], &[])),
            Common::Stopped(step) => bail!("common stopped at an unexpected step: {step:?}"),
        };
        let mut stops = Vec::new();
        let mut plans = Vec::new();
        for c in &consumers {
            let step = match self.consumer(c, &target, &common_rc.source) {
                Err(e) => match e.downcast_ref::<Repeated>() {
                    Some(r) => Step::Stop(Waits::Nobody, r.0.clone()),
                    None => return Err(e),
                },
                Ok(step) => step,
            };
            tracing::debug!(consumer = %c.name, ?step, "the consumer's step");
            match step {
                Step::Done | Step::Progress => {}
                Step::Planned(then) => plans.push(then),
                Step::Stop(waits, msg) => stops.push((waits, msg)),
            }
        }
        if stops.is_empty() && plans.is_empty() {
            let msg = if self.lander.wrote() {
                format!(
                    "done: {} lock common at {}",
                    names.join(", "),
                    short(&target)
                )
            } else {
                "nothing to bump".to_string()
            };
            style::say(CMD, Tone::Good, msg);
            return Ok(ExitCode::SUCCESS);
        }
        Ok(self.end(&stops, &plans))
    }

    /// Says what each stopped item waits for, and what a dry run planned.
    /// The exit status is the most urgent wait's: the owner, anyone, then
    /// nobody.
    fn end(&self, stops: &[(Waits, String)], plans: &[String]) -> ExitCode {
        for (waits, msg) in stops {
            land::say_waits(CMD, *waits, msg);
        }
        for then in plans {
            land::planned(CMD, then);
        }
        match stops.iter().map(|(w, _)| land::waits_code(*w)).min() {
            Some(code) if !self.dry_run() => ExitCode::from(code),
            _ => ExitCode::SUCCESS,
        }
    }
}

/// `swl bump`.
pub fn run(
    config: &Config,
    reader: &dyn PrReader,
    writer: &dyn ForgeWriter,
    history: &dyn History,
    opts: &Options,
) -> Result<ExitCode> {
    let bumper = Bumper {
        config,
        reader,
        writer,
        history,
        opts,
        lander: Lander::new(CMD, config, reader, writer, &opts.land),
        fetcher: Fetcher::new(true),
        nix: std::cell::OnceCell::new(),
    };
    bumper.run()
}

//! `swl status`: where one change stands. One row per checkout or MR, with
//! its stage, the next step and who it waits for; the local git facts as
//! detail lines below. From a workspace, or from a published consumer MR
//! (`--from <consumer>#<MR>`) without one.
//!
//! The stages, in order: `local`, `needs publish`, `pushed`, `MR open`, `CI`
//! (pending, failing, green), `merged`, `promoted`, `landing pushed`,
//! `landed`. Facts that cannot be read make a row `unknown (<reason>)`, never
//! an error.

use std::{
    cell::OnceCell,
    collections::BTreeMap,
    fmt, fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, anyhow, bail};
use serde_json::Value;

use crate::{
    ancestry,
    config::Config,
    forge::{CiState, LOGIN_REJECTED, PrInfo, PrReader, PrState},
    git::{git, git_ok, short},
    lock::Lock,
    manifest::{Entry, MANIFEST_FILE, Manifest},
    nix::Nix,
    start::{parse_from, parse_ls_remote, source_remote},
    tempdir::TempDir,
    workspace::{Checkout, Workspace},
    ws_add::{Fetcher, base_remote},
};

/// Who the next step waits for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Waits {
    /// The repo owner: merging, promoting.
    Owner,
    /// Whoever works on the change.
    Anyone,
    /// Nobody: CI, a mirror, or nothing left to do.
    Nobody,
}

impl fmt::Display for Waits {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(match self {
            Waits::Owner => "owner",
            Waits::Anyone => "anyone",
            Waits::Nobody => "nobody",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stage {
    Local,
    NeedsPublish,
    Pushed,
    MrOpen {
        draft: bool,
    },
    Ci(CiState),
    Merged,
    Promoted,
    /// The landing commit is on the open consumer MR's branch.
    LandingPushed {
        ci_failing: bool,
    },
    Landed,
    /// A shared checkout at the locked rev or already in its base.
    NoMrNeeded(String),
    /// A consumer with nothing of its own yet.
    NoChange(String),
    Unknown(String),
}

impl fmt::Display for Stage {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Stage::Local => f.write_str("local"),
            Stage::NeedsPublish => f.write_str("needs publish"),
            Stage::Pushed => f.write_str("pushed"),
            Stage::MrOpen { draft: true } => f.write_str("MR open (draft)"),
            Stage::MrOpen { draft: false } => f.write_str("MR open"),
            Stage::Ci(CiState::None) => f.write_str("CI pending (no status yet)"),
            Stage::Ci(CiState::Pending) => f.write_str("CI pending"),
            Stage::Ci(CiState::Failing(s)) => write!(f, "CI failing ({s})"),
            Stage::Ci(CiState::Green) => f.write_str("CI green"),
            Stage::Merged => f.write_str("merged"),
            Stage::Promoted => f.write_str("promoted"),
            Stage::LandingPushed { ci_failing: false } => f.write_str("landing pushed"),
            Stage::LandingPushed { ci_failing: true } => f.write_str("landing pushed, CI failing"),
            Stage::Landed => f.write_str("landed"),
            Stage::NoMrNeeded(why) => write!(f, "no MR needed ({why})"),
            Stage::NoChange(why) => write!(f, "no change ({why})"),
            Stage::Unknown(why) => write!(f, "unknown ({why})"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verdict {
    pub stage: Stage,
    pub next: String,
    pub waits: Waits,
    /// A shared change the consumer can land now: promoted, and in the
    /// declared source as far as known.
    pub landable: bool,
}

fn verdict(stage: Stage, next: impl Into<String>, waits: Waits) -> Verdict {
    Verdict {
        stage,
        next: next.into(),
        waits,
        landable: false,
    }
}

/// The branch of a checkout against its remote's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteBranch {
    /// Not on the remote (or no remote, or detached).
    Missing,
    Same,
    /// The remote's is behind HEAD: unpushed commits.
    Behind,
    /// Diverged, or the remote's is ahead.
    Other,
}

/// Whether a merged MR's merge commit is in what the consumer's flake.nix
/// declares for the input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Promotion {
    /// The declared branch, e.g. `swan-lake-current`; the base branch when
    /// the input names none.
    pub branch: String,
    /// On the forge: in `<remote>/<branch>`.
    pub on_forge: Result<bool, String>,
    /// The declared source (e.g. the GitHub mirror) and whether it has it;
    /// `None` when not checked.
    pub declared_source: Option<(String, Result<bool, String>)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MrRead {
    /// The forge could not be asked.
    Error(String),
    /// A 404: why, for this reader.
    NotFound(String),
    Open {
        info: PrInfo,
        ci: Result<Option<CiState>, String>,
    },
    /// Merged; the promotion is read for shared repos only.
    Merged {
        merge_commit: String,
        promotion: Option<Promotion>,
    },
    Closed,
}

/// The MR a row follows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MrFacts {
    /// No swl.toml entry (shared) or pull ref (consumer) names one.
    None,
    /// Not looked up, and why.
    NotLookedUp(String),
    /// The consumers' swl.toml entries name different MRs.
    Conflict(Vec<u64>),
    Read {
        number: u64,
        read: MrRead,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SharedFacts {
    pub repo: String,
    /// The base, e.g. `origin/master`.
    pub base: String,
    pub branch: Option<String>,
    /// Uncommitted changes to tracked files.
    pub dirty: bool,
    pub head_in_base: bool,
    /// HEAD is the rev, or in the history of the rev, that every consumer
    /// overriding it locks.
    pub in_locked: bool,
    pub remote: RemoteBranch,
    /// The consumers that override it.
    pub consumers: Vec<String>,
    /// Consumers that use a changed HEAD without a swl.toml entry for it.
    pub missing_entry: Vec<String>,
    pub mr: MrFacts,
}

fn open_verdict(
    which: &str,
    info: &PrInfo,
    ci: &Result<Option<CiState>, String>,
    consumer: &str,
) -> Verdict {
    if info.draft {
        return verdict(
            Stage::MrOpen { draft: true },
            format!("mark {which} ready"),
            Waits::Anyone,
        );
    }
    match ci {
        Ok(Some(CiState::Green)) => verdict(
            Stage::Ci(CiState::Green),
            format!("merge {which}"),
            Waits::Owner,
        ),
        Ok(Some(CiState::Failing(s))) => verdict(
            Stage::Ci(CiState::Failing(s.clone())),
            format!("fix it, then swl publish {consumer}"),
            Waits::Anyone,
        ),
        Ok(Some(state)) => verdict(
            Stage::Ci(state.clone()),
            "wait for CI, or request merge when checks succeed",
            Waits::Nobody,
        ),
        Ok(None) | Err(_) => verdict(Stage::MrOpen { draft: false }, "wait for CI", Waits::Nobody),
    }
}

fn merged_verdict(p: &Promotion, consumer: &str) -> Verdict {
    match (&p.on_forge, &p.declared_source) {
        (Ok(true), Some((url, Ok(false)))) => verdict(
            Stage::Promoted,
            format!("wait for {url} to have it"),
            Waits::Nobody,
        ),
        (Ok(true), _) => Verdict {
            landable: true,
            ..verdict(
                Stage::Promoted,
                format!("swl land {consumer} (not implemented yet)"),
                Waits::Anyone,
            )
        },
        (Ok(false), _) => verdict(Stage::Merged, format!("promote {}", p.branch), Waits::Owner),
        (Err(e), _) => verdict(
            Stage::Merged,
            format!("promote {} (unknown whether done: {e})", p.branch),
            Waits::Owner,
        ),
    }
}

/// A forge read that failed: a rejected fj login is for the user to renew.
fn error_verdict(e: &str) -> Verdict {
    if e == LOGIN_REJECTED {
        verdict(
            Stage::Unknown(e.to_string()),
            "fj auth login",
            Waits::Anyone,
        )
    } else {
        verdict(
            Stage::Unknown(e.to_string()),
            "rerun swl status",
            Waits::Nobody,
        )
    }
}

/// The stage of an MR alone, as far as the forge says.
fn mr_verdict(which: &str, read: &MrRead, consumer: &str) -> Verdict {
    match read {
        MrRead::Error(e) => error_verdict(e),
        MrRead::NotFound(why) => verdict(
            Stage::Unknown(why.clone()),
            format!("check {which} on the forge"),
            Waits::Anyone,
        ),
        MrRead::Open { info, ci } => open_verdict(which, info, ci, consumer),
        MrRead::Merged {
            promotion: Some(p), ..
        } => merged_verdict(p, consumer),
        MrRead::Merged { .. } => verdict(Stage::Merged, "-", Waits::Nobody),
        MrRead::Closed => verdict(
            Stage::NeedsPublish,
            format!("reopen {which}, or swl publish {consumer} for a new MR"),
            Waits::Anyone,
        ),
    }
}

pub fn shared_verdict(f: &SharedFacts) -> Verdict {
    let consumer = f.consumers.first().map_or("<consumer>", String::as_str);
    let publish = |c: &str| {
        verdict(
            Stage::NeedsPublish,
            format!("swl publish {c}"),
            Waits::Anyone,
        )
    };
    if let MrFacts::Read {
        number,
        read: read @ MrRead::Merged { .. },
    } = &f.mr
    {
        return mr_verdict(&format!("{}#{number}", f.repo), read, consumer);
    }
    if f.mr == MrFacts::None {
        if f.dirty && (f.in_locked || f.head_in_base) {
            return verdict(
                Stage::Local,
                format!("commit the changes, then swl publish {consumer}"),
                Waits::Anyone,
            );
        }
        if f.in_locked {
            return verdict(
                Stage::NoMrNeeded(format!("in the rev {} locks", f.consumers.join(", "))),
                "-",
                Waits::Nobody,
            );
        }
        if f.head_in_base {
            return verdict(
                Stage::NoMrNeeded(format!("in {}", f.base)),
                format!("bump the lock in {consumer} by hand"),
                Waits::Anyone,
            );
        }
    }
    let Some(branch) = &f.branch else {
        return verdict(
            Stage::Local,
            format!("check out a branch, then swl publish {consumer}"),
            Waits::Anyone,
        );
    };
    let tracking = format!(
        "{}/{branch}",
        f.base.split_once('/').map_or("origin", |(r, _)| r)
    );
    match f.remote {
        RemoteBranch::Missing => return publish(consumer).with_stage(Stage::Local),
        RemoteBranch::Behind => return publish(consumer),
        RemoteBranch::Other => {
            return verdict(
                Stage::NeedsPublish,
                format!("merge or rebase onto {tracking}, then swl publish {consumer}"),
                Waits::Anyone,
            );
        }
        RemoteBranch::Same => {}
    }
    if let Some(c) = f.missing_entry.first() {
        return publish(c);
    }
    match &f.mr {
        MrFacts::None => publish(consumer),
        MrFacts::NotLookedUp(why) => verdict(Stage::Pushed, format!("({why})"), Waits::Nobody),
        MrFacts::Conflict(ns) => verdict(
            Stage::Unknown(format!(
                "the consumers' {MANIFEST_FILE} name different MRs: {}",
                ns.iter()
                    .map(|n| format!("#{n}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
            "make the swl.toml entries agree",
            Waits::Anyone,
        ),
        MrFacts::Read {
            number,
            read: MrRead::NotFound(why),
        } => verdict(
            Stage::Pushed,
            format!("check {}#{number} on the forge ({why})", f.repo),
            Waits::Anyone,
        ),
        MrFacts::Read { number, read } => {
            mr_verdict(&format!("{}#{number}", f.repo), read, consumer)
        }
    }
}

impl Verdict {
    fn with_stage(self, stage: Stage) -> Self {
        Self { stage, ..self }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsumerFacts {
    pub name: String,
    pub base: String,
    /// The topic branch; `None` when detached or on the base branch.
    pub topic: Option<String>,
    /// Uncommitted changes to tracked files.
    pub dirty: bool,
    pub head_in_base: bool,
    pub remote: RemoteBranch,
    /// The remote branch head is in the base.
    pub remote_in_base: bool,
    /// The unpushed commits change flake.lock (a local `finalize` commit).
    pub local_lock_change: bool,
    /// The remote branch has no pending swl.toml entries, removed some, and
    /// changes flake.lock: the landing commit is pushed.
    pub landing_pushed: bool,
    /// Overridden inputs whose checkout HEAD is changed and has no entry.
    pub unpublished_inputs: Vec<String>,
    /// The swl.toml entries' rows: `repo#n` and their verdicts.
    pub entries: Vec<(String, Verdict)>,
    pub mr: MrFacts,
}

pub fn consumer_verdict(f: &ConsumerFacts) -> Verdict {
    let c = &f.name;
    let landed = || verdict(Stage::Landed, "-", Waits::Nobody);
    if let MrFacts::Read {
        read: MrRead::Merged { .. },
        ..
    } = &f.mr
    {
        return landed();
    }
    let Some(branch) = &f.topic else {
        return if f.head_in_base && !f.dirty {
            verdict(
                Stage::NoChange(format!("HEAD is in {}", f.base)),
                "-",
                Waits::Nobody,
            )
        } else {
            verdict(
                Stage::Local,
                format!("check out a topic branch, then swl publish {c}"),
                Waits::Anyone,
            )
        };
    };
    if f.head_in_base && f.remote_in_base {
        return landed();
    }
    let publish = verdict(
        Stage::NeedsPublish,
        format!("swl publish {c}"),
        Waits::Anyone,
    );
    match f.remote {
        RemoteBranch::Missing if f.head_in_base && !f.dirty => {
            return verdict(
                Stage::NoChange(format!("HEAD is in {}", f.base)),
                "-",
                Waits::Nobody,
            );
        }
        RemoteBranch::Missing => return publish.with_stage(Stage::Local),
        RemoteBranch::Behind if f.local_lock_change => {
            return verdict(
                Stage::NeedsPublish,
                "swl land (not implemented yet): the local landing commit goes to master",
                Waits::Anyone,
            );
        }
        RemoteBranch::Behind => return publish,
        RemoteBranch::Other => {
            let tracking = format!(
                "{}/{branch}",
                f.base.split_once('/').map_or("origin", |(r, _)| r)
            );
            return verdict(
                Stage::NeedsPublish,
                format!("merge or rebase onto {tracking}, then swl publish {c}"),
                Waits::Anyone,
            );
        }
        RemoteBranch::Same => {}
    }
    if !f.unpublished_inputs.is_empty() {
        return publish;
    }
    let which = |n: &u64| format!("{c}#{n}");
    if f.landing_pushed {
        return match &f.mr {
            MrFacts::Read {
                number,
                read: MrRead::Open { info, .. },
            } if info.draft => verdict(
                Stage::MrOpen { draft: true },
                format!("mark {} ready", which(number)),
                Waits::Anyone,
            ),
            MrFacts::Read {
                read: MrRead::Open { ci, .. },
                ..
            } => match ci {
                Ok(Some(CiState::Failing(_))) => verdict(
                    Stage::LandingPushed { ci_failing: true },
                    "fix CI",
                    Waits::Anyone,
                ),
                _ => verdict(
                    Stage::LandingPushed { ci_failing: false },
                    "request merge when checks succeed",
                    Waits::Owner,
                ),
            },
            MrFacts::None => verdict(
                Stage::Pushed,
                "open the MR (the landing commit is pushed)",
                Waits::Anyone,
            ),
            MrFacts::Read {
                number,
                read: MrRead::Closed,
            } => verdict(
                Stage::NeedsPublish,
                format!("reopen {} (the landing commit is pushed)", which(number)),
                Waits::Anyone,
            ),
            MrFacts::Read {
                read: MrRead::Error(e),
                ..
            } => error_verdict(e),
            MrFacts::Read { number, .. } => verdict(
                Stage::Pushed,
                format!(
                    "check {} on the forge (the landing commit is pushed)",
                    which(number)
                ),
                Waits::Anyone,
            ),
            _ => verdict(
                Stage::Pushed,
                format!("check the MR of {c} on the forge (the landing commit is pushed)"),
                Waits::Anyone,
            ),
        };
    }
    let own = match &f.mr {
        MrFacts::None => return publish.with_stage(Stage::Pushed),
        MrFacts::Read { number, read } => match read {
            MrRead::Open { info, ci } => open_verdict(&which(number), info, ci, c),
            MrRead::Closed => {
                return verdict(
                    Stage::NeedsPublish,
                    format!("reopen {}, or swl publish {c} for a new MR", which(number)),
                    Waits::Anyone,
                );
            }
            MrRead::Error(e) => error_verdict(e),
            _ => verdict(Stage::Pushed, "-", Waits::Nobody),
        },
        _ => verdict(Stage::Pushed, "-", Waits::Nobody),
    };
    // A consumer MR with entries waits for them, then lands; but a draft or
    // failing CI is its own step first.
    if own.waits == Waits::Anyone {
        return own;
    }
    if let Some((which, v)) = f.entries.iter().find(|(_, v)| !v.landable) {
        return Verdict {
            next: format!("wait for {which} ({})", v.next),
            waits: v.waits,
            ..own
        };
    }
    if !f.entries.is_empty() {
        return Verdict {
            next: format!("swl land {c} (not implemented yet)"),
            waits: Waits::Anyone,
            ..own
        };
    }
    own
}

pub struct Row {
    pub name: String,
    /// The MR `repo#n` the row follows.
    pub follows: Option<String>,
    pub verdict: Verdict,
    pub details: Vec<String>,
}

impl Row {
    fn unknown(name: &str, e: anyhow::Error, details: Vec<String>) -> Self {
        Self {
            name: name.to_string(),
            follows: None,
            verdict: verdict(Stage::Unknown(format!("{e:#}")), "-", Waits::Nobody),
            details,
        }
    }

    fn print(&self) {
        let v = &self.verdict;
        println!(
            "{}  {}  next: {}  waits: {}",
            self.name, v.stage, v.next, v.waits
        );
        for d in &self.details {
            println!("  {d}");
        }
    }
}

/// Where a flake.nix that declares an input is.
enum FlakeSource {
    Dir(PathBuf),
    Git { dir: PathBuf, rev: String },
}

/// What a flake.nix declares for an input: its ref and git URL.
struct Declared {
    branch: Option<String>,
    git_url: Option<String>,
}

/// For a shared MR's promotion: a repo with the forge's refs (if the repo is
/// configured) and the input's declaration.
type PromotionSource<'a> = (Option<&'a Path>, &'a dyn Fn() -> Result<Declared, String>);

/// Reads the forge, git and nix for the rows.
struct Ctx<'a> {
    config: &'a Config,
    reader: &'a dyn PrReader,
    fetcher: &'a Fetcher,
    nix: OnceCell<Result<Nix, String>>,
}

fn rev(dir: &Path, refname: &str) -> Option<String> {
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

/// `Ok(true)` when `a` is in `b`'s history; an error when either is unknown.
fn ancestor(dir: &Path, a: &str, b: &str) -> Result<bool, String> {
    for r in [a, b] {
        if rev(dir, r).is_none() {
            return Err(format!("{} is not known here", short(r)));
        }
    }
    Ok(git_ok(dir, &["merge-base", "--is-ancestor", a, b]).is_some())
}

fn remote_state(dir: &Path, head: &str, remote_head: Option<&str>) -> RemoteBranch {
    match remote_head {
        None => RemoteBranch::Missing,
        Some(r) if r == head => RemoteBranch::Same,
        Some(r) if ancestor(dir, r, head) == Ok(true) => RemoteBranch::Behind,
        Some(_) => RemoteBranch::Other,
    }
}

impl Ctx<'_> {
    fn nix(&self) -> Result<&Nix, String> {
        self.nix
            .get_or_init(|| Nix::find().map_err(|e| format!("{e:#}")))
            .as_ref()
            .map_err(Clone::clone)
    }

    fn declared(&self, src: &FlakeSource, input: &str) -> Result<Declared, String> {
        let text = match src {
            FlakeSource::Dir(dir) => fs::read_to_string(dir.join("flake.nix"))
                .map_err(|e| format!("reading {}/flake.nix: {e}", dir.display()))?,
            FlakeSource::Git { dir, rev } => {
                git(dir, &["show", &format!("{rev}:flake.nix")]).map_err(|e| format!("{e:#}"))?
            }
        };
        let nix = self.nix()?;
        let tmp = TempDir::new("swl-status").map_err(|e| format!("{e:#}"))?;
        fs::write(tmp.path().join("flake.nix"), text).map_err(|e| e.to_string())?;
        let decls = nix.declarations(tmp.path()).map_err(|e| format!("{e:#}"))?;
        let decl = decls
            .get(input)
            .ok_or_else(|| format!("flake.nix does not declare {input}"))?;
        let attrs = match decl.get("url").and_then(Value::as_str) {
            Some(url) => nix.parse_flake_ref(url).map_err(|e| format!("{e:#}"))?,
            None if decl.get("type").is_some() => decl.clone(),
            None => return Err(format!("{input} has no url in flake.nix")),
        };
        Ok(Declared {
            branch: attrs.get("ref").and_then(Value::as_str).map(str::to_string),
            git_url: ancestry::git_url(&attrs),
        })
    }

    /// Whether `merge_commit` is in the declared branch on the forge (the
    /// remote-tracking ref in `dir` of the base's remote; the local branch
    /// for a base without one) and in the declared source.
    fn promotion(
        &self,
        dir: &Path,
        base: &str,
        merge_commit: &str,
        declared: Result<Declared, String>,
    ) -> Promotion {
        let (remote, base_branch) = match base.split_once('/') {
            Some((r, b)) => (Some(r), b),
            None => (None, base),
        };
        let declared = match declared {
            Ok(d) => d,
            Err(e) => {
                return Promotion {
                    branch: "the declared branch".into(),
                    on_forge: Err(format!("cannot read the declared input: {e}")),
                    declared_source: None,
                };
            }
        };
        let branch = declared.branch.unwrap_or_else(|| base_branch.to_string());
        let tip = match remote {
            Some(r) => format!("refs/remotes/{r}/{branch}"),
            None => format!("refs/heads/{branch}"),
        };
        let on_forge = ancestor(dir, merge_commit, &tip);
        let declared_source = match declared.git_url {
            Some(url) if self.fetcher.enabled() => {
                let has = ancestry::remote_head(&url, &format!("refs/heads/{branch}"))
                    .and_then(|tip| ancestry::is_ancestor(&url, merge_commit, &tip, false));
                Some((url, has))
            }
            _ => None,
        };
        Promotion {
            branch,
            on_forge,
            declared_source,
        }
    }

    /// The MR `repo#n` from the forge, with CI or promotion as its state
    /// needs. For a shared repo, `promote` is a repo with the forge's refs
    /// and the input's declaration, read only for a merged MR.
    fn read_mr(&self, repo: &str, n: u64, promote: Option<PromotionSource>) -> MrRead {
        let info = match self.reader.pr(repo, n) {
            Err(e) => return MrRead::Error(format!("{e:#}")),
            Ok(None) => return MrRead::NotFound(self.reader.not_found().to_string()),
            Ok(Some(info)) => info,
        };
        match &info.state {
            PrState::Open => {
                let ci = self
                    .reader
                    .commit_status(repo, &info.head_sha)
                    .map_err(|e| format!("{e:#}"));
                MrRead::Open { info, ci }
            }
            PrState::Merged { merge_commit } => {
                let promotion = promote.map(|(dir, declared)| match dir {
                    Some(dir) => {
                        self.promotion(dir, self.config.base_for(repo), merge_commit, declared())
                    }
                    None => Promotion {
                        branch: "the declared branch".into(),
                        on_forge: Err(format!("{repo} is not in the swl config")),
                        declared_source: None,
                    },
                });
                MrRead::Merged {
                    merge_commit: merge_commit.clone(),
                    promotion,
                }
            }
            PrState::ClosedUnmerged => MrRead::Closed,
        }
    }

    /// Fetches the remote of `dir`'s base; `Ok(remote)`, `None` without one.
    fn fetch(&self, dir: &Path, base: &str) -> Result<Option<String>> {
        let remote = base_remote(dir, base)?;
        if let Some(r) = &remote {
            self.fetcher.fetch(dir, r)?;
        }
        Ok(remote)
    }

    /// The MR of a consumer branch at `head`: the highest-numbered pull ref
    /// at that head, read with `ls-remote` (works for private repos).
    fn consumer_mr(&self, dir: &Path, remote: &str, repo: &str, head: &str) -> MrFacts {
        if !self.fetcher.enabled() {
            return MrFacts::NotLookedUp("--no-fetch: the MR is not looked up".into());
        }
        let refs = match git(dir, &["ls-remote", remote, "refs/pull/*/head"]) {
            Ok(r) => r,
            Err(e) => return MrFacts::NotLookedUp(format!("{e:#}")),
        };
        let number = parse_ls_remote(&refs)
            .into_iter()
            .filter(|(sha, _)| sha == head)
            .filter_map(|(_, r)| {
                r.strip_prefix("refs/pull/")?
                    .strip_suffix("/head")?
                    .parse()
                    .ok()
            })
            .max();
        match number {
            None => MrFacts::None,
            Some(n) => MrFacts::Read {
                number: n,
                read: self.read_mr(repo, n, None),
            },
        }
    }
}

fn describe_mr(which: &str, read: &MrRead) -> String {
    match read {
        MrRead::Error(e) => format!("{which}: unknown ({e})"),
        MrRead::NotFound(why) => format!("{which}: unknown ({why})"),
        MrRead::Open { info, ci } => {
            let ci = match ci {
                Ok(Some(CiState::None)) => "no CI status yet".to_string(),
                Ok(Some(CiState::Pending)) => "CI pending".into(),
                Ok(Some(CiState::Failing(s))) => format!("CI failing ({s})"),
                Ok(Some(CiState::Green)) => "CI green".into(),
                Ok(None) => "CI unknown (no status readable)".into(),
                Err(e) => format!("CI unknown ({e})"),
            };
            format!(
                "{which}: open{}, {} at {}, {ci}",
                if info.draft { " (draft)" } else { "" },
                info.head_ref,
                short(&info.head_sha)
            )
        }
        MrRead::Merged {
            merge_commit,
            promotion,
        } => {
            let mut s = format!("{which}: merged as {}", short(merge_commit));
            let Some(promotion) = promotion else {
                return s;
            };
            match &promotion.on_forge {
                Ok(true) => s.push_str(&format!(", in {}", promotion.branch)),
                Ok(false) => s.push_str(&format!(", not in {} yet", promotion.branch)),
                Err(e) => s.push_str(&format!(", promotion unknown ({e})")),
            }
            match &promotion.declared_source {
                Some((url, Ok(true))) => s.push_str(&format!("; {url} has it")),
                Some((url, Ok(false))) => s.push_str(&format!("; {url} does not have it yet")),
                Some((url, Err(e))) => s.push_str(&format!("; {url}: unknown ({e})")),
                None => s.push_str("; the declared source is not checked"),
            }
            s
        }
        MrRead::Closed => format!("{which}: closed without merging"),
    }
}

/// Tracked changes only: git+file: sees tracked files only.
fn is_dirty(dir: &Path) -> Result<bool> {
    Ok(!git(dir, &["status", "--porcelain", "--untracked-files=no"])?.is_empty())
}

/// `HEAD <short>  <branch or detached>  clean|dirty`.
fn head_line(dir: &Path) -> Result<String> {
    let head = git(dir, &["rev-parse", "--short", "HEAD"])?;
    let branch = git_ok(dir, &["symbolic-ref", "--quiet", "--short", "HEAD"])
        .unwrap_or_else(|| "detached".into());
    let dirty = if is_dirty(dir)? { "dirty" } else { "clean" };
    Ok(format!("HEAD {head}  {branch}  {dirty}"))
}

fn compare(dir: &Path, locked: &str) -> String {
    let short = locked.get(..7).unwrap_or(locked);
    if git_ok(dir, &["cat-file", "-e", &format!("{locked}^{{commit}}")]).is_none() {
        return format!("locked {short}, not in the local repo");
    }
    match git_ok(
        dir,
        &[
            "rev-list",
            "--left-right",
            "--count",
            &format!("{locked}...HEAD"),
        ],
    ) {
        Some(counts) => {
            let mut it = counts.split_whitespace();
            let behind = it.next().unwrap_or("?");
            let ahead = it.next().unwrap_or("?");
            format!("locked {short}, ahead {ahead}, behind {behind}")
        }
        None => format!("locked {short}"),
    }
}

/// One override of a workspace flake: which flake, the input path, the
/// checkout, and the rev the flake's lock has there.
struct Hop {
    target: String,
    target_is_consumer: bool,
    input_path: String,
    checkout: String,
    locked: Option<String>,
}

fn hops(ws: &Workspace, config: &Config) -> Result<Vec<Hop>> {
    let mut out = Vec::new();
    for target in &ws.checkouts {
        let lock = Lock::read(&target.dir)?;
        for o in ws.overrides_for(config, target)? {
            let path: Vec<String> = o.input_path.split('/').map(str::to_string).collect();
            out.push(Hop {
                target: target.name.clone(),
                target_is_consumer: target.is_consumer(),
                locked: lock.as_ref().and_then(|l| l.locked_rev(&path)),
                input_path: o.input_path,
                checkout: o.checkout,
            });
        }
    }
    Ok(out)
}

/// The flake that declares `input_path` for consumer `target`: the consumer
/// for a root input, else the checkout that overrides the parent path.
fn declaring_flake(
    ws: &Workspace,
    hops: &[Hop],
    target: &str,
    input_path: &str,
) -> Option<FlakeSource> {
    let dir = match input_path.rsplit_once('/') {
        None => ws.checkouts.iter().find(|c| c.name == target)?.dir.clone(),
        Some((parent, _)) => {
            let h = hops
                .iter()
                .find(|h| h.target == target && h.input_path == parent)?;
            ws.checkouts
                .iter()
                .find(|c| c.name == h.checkout)?
                .dir
                .clone()
        }
    };
    Some(FlakeSource::Dir(dir))
}

fn last_segment(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn shared_row(
    ctx: &Ctx,
    ws: &Workspace,
    hops: &[Hop],
    manifests: &BTreeMap<String, Manifest>,
    c: &Checkout,
    repo: &str,
) -> Row {
    let mut details = Vec::new();
    let input = c.shared.as_ref().map_or("", |(_, i)| i.as_str());
    let result = (|| -> Result<(Verdict, Option<String>)> {
        details.push(format!("input {input}  {}", head_line(&c.dir)?));
        let mine: Vec<&Hop> = hops.iter().filter(|h| h.checkout == c.name).collect();
        for h in &mine {
            let state = match &h.locked {
                Some(rev) => compare(&c.dir, rev),
                None => "not locked".into(),
            };
            details.push(format!("{}: {} {state}", h.target, h.input_path));
        }
        let base = ctx.config.base_for(repo).to_string();
        let remote = ctx.fetch(&c.dir, &base)?;
        let head = git(&c.dir, &["rev-parse", "HEAD"])?;
        let branch = git_ok(&c.dir, &["symbolic-ref", "--quiet", "--short", "HEAD"]);
        let remote_head = match (&remote, &branch) {
            (Some(r), Some(b)) => rev(&c.dir, &format!("refs/remotes/{r}/{b}")),
            _ => None,
        };
        let head_in_base = ancestor(&c.dir, &head, &base) == Ok(true);
        let consumer_hops: Vec<&&Hop> = mine.iter().filter(|h| h.target_is_consumer).collect();
        let in_locked = !consumer_hops.is_empty()
            && consumer_hops.iter().all(|h| {
                h.locked
                    .as_deref()
                    .is_some_and(|l| ancestor(&c.dir, &head, l) == Ok(true))
            });
        let mut consumers: Vec<String> = consumer_hops.iter().map(|h| h.target.clone()).collect();
        consumers.dedup();
        let mut numbers: Vec<(u64, &Hop)> = Vec::new();
        let mut missing_entry = Vec::new();
        for h in &consumer_hops {
            match manifests
                .get(&h.target)
                .and_then(|m| m.inputs.get(&h.input_path))
            {
                Some(e) if e.repo == repo => numbers.push((e.mr, h)),
                _ if h.locked.as_deref() != Some(head.as_str()) && !head_in_base => {
                    missing_entry.push(h.target.clone())
                }
                _ => {}
            }
        }
        let mut distinct: Vec<u64> = numbers.iter().map(|(n, _)| *n).collect();
        distinct.sort();
        distinct.dedup();
        let mr = match distinct.as_slice() {
            [] => MrFacts::None,
            [n] => {
                let (_, h) = numbers[0];
                let declared = || match declaring_flake(ws, hops, &h.target, &h.input_path) {
                    Some(src) => ctx.declared(&src, last_segment(&h.input_path)),
                    None => Err(format!(
                        "no flake in the workspace declares {}",
                        h.input_path
                    )),
                };
                let read = ctx.read_mr(repo, *n, Some((Some(&c.dir), &declared)));
                details.push(describe_mr(&format!("{repo}#{n}"), &read));
                MrFacts::Read { number: *n, read }
            }
            many => MrFacts::Conflict(many.to_vec()),
        };
        if let (Some(r), Some(b), Some(rh)) = (&remote, &branch, &remote_head)
            && *rh != head
        {
            details.push(format!("{r}/{b} is at {}", short(rh)));
        }
        let follows = match &mr {
            MrFacts::Read { number, .. } => Some(format!("{repo}#{number}")),
            _ => None,
        };
        let v = shared_verdict(&SharedFacts {
            repo: repo.to_string(),
            base,
            branch,
            dirty: is_dirty(&c.dir)?,
            head_in_base,
            in_locked,
            remote: remote_state(&c.dir, &head, remote_head.as_deref()),
            consumers,
            missing_entry,
            mr,
        });
        Ok((v, follows))
    })();
    match result {
        Ok((verdict, follows)) => Row {
            name: c.name.clone(),
            follows,
            verdict,
            details,
        },
        Err(e) => Row::unknown(&c.name, e, details),
    }
}

/// Whether the commit `at` is a pushed landing: it has no pending swl.toml
/// entries, its branch had some (commits since the merge base with `base`
/// touch swl.toml), and it changes flake.lock against that merge base. So a
/// plain lock bump is not one.
fn landing_at(dir: &Path, at: &str, base: &str) -> Result<bool> {
    let file = format!("{at}:{MANIFEST_FILE}");
    if git_ok(dir, &["cat-file", "-e", &file]).is_some() {
        let m = Manifest::parse(&git(dir, &["show", &file])?)
            .with_context(|| format!("{MANIFEST_FILE} at {}", short(at)))?;
        if !m.is_empty() {
            return Ok(false);
        }
    }
    let mb = git(dir, &["merge-base", at, base])?;
    let range = format!("{mb}..{at}");
    let had_entries = !git(dir, &["log", "--format=%H", &range, "--", MANIFEST_FILE])?.is_empty();
    Ok(had_entries && lock_changed(dir, at, base)?)
}

fn lock_changed(dir: &Path, at: &str, base: &str) -> Result<bool> {
    let mb = git(dir, &["merge-base", at, base])?;
    Ok(!git(dir, &["diff", "--name-only", &mb, at, "--", "flake.lock"])?.is_empty())
}

fn entry_lines(manifest: &Manifest) -> String {
    if manifest.is_empty() {
        return format!("no pending {MANIFEST_FILE} entries");
    }
    let entries: Vec<String> = manifest
        .inputs
        .iter()
        .map(|(p, e)| format!("{p} {}#{}", e.repo, e.mr))
        .collect();
    format!("{MANIFEST_FILE}: {}", entries.join(", "))
}

fn consumer_row(ctx: &Ctx, hops: &[Hop], ws: &Workspace, rows: &[Row], c: &Checkout) -> Row {
    let mut details = Vec::new();
    let result = (|| -> Result<Verdict> {
        details.push(head_line(&c.dir)?);
        let manifest = Manifest::read(&c.dir)?.unwrap_or_default();
        details.push(entry_lines(&manifest));
        let base = ctx.config.base_for(&c.name).to_string();
        let base_branch = base.split_once('/').map_or(base.as_str(), |(_, b)| b);
        let remote = ctx.fetch(&c.dir, &base)?;
        let head = git(&c.dir, &["rev-parse", "HEAD"])?;
        let topic = git_ok(&c.dir, &["symbolic-ref", "--quiet", "--short", "HEAD"])
            .filter(|b| b != base_branch);
        let remote_head = match (&remote, &topic) {
            (Some(r), Some(b)) => rev(&c.dir, &format!("refs/remotes/{r}/{b}")),
            _ => None,
        };
        let head_in_base = ancestor(&c.dir, &head, &base) == Ok(true);
        let state = remote_state(&c.dir, &head, remote_head.as_deref());
        let mut unpublished_inputs = Vec::new();
        for h in hops.iter().filter(|h| h.target == c.name) {
            let checkout = ws.checkouts.iter().find(|x| x.name == h.checkout);
            let Some(checkout) = checkout else { continue };
            let Some((repo, _)) = &checkout.shared else {
                continue;
            };
            let ch = git(&checkout.dir, &["rev-parse", "HEAD"])?;
            if h.locked.as_deref() == Some(ch.as_str())
                || manifest.inputs.contains_key(&h.input_path)
                || ancestor(&checkout.dir, &ch, ctx.config.base_for(repo)) == Ok(true)
            {
                continue;
            }
            unpublished_inputs.push(h.input_path.clone());
        }
        if !unpublished_inputs.is_empty() {
            details.push(format!(
                "changed without a {MANIFEST_FILE} entry: {}",
                unpublished_inputs.join(", ")
            ));
        }
        let (remote_in_base, landing_pushed) = match &remote_head {
            Some(rh) => (
                ancestor(&c.dir, rh, &base) == Ok(true),
                landing_at(&c.dir, rh, &base)?,
            ),
            None => (false, false),
        };
        let local_lock_change =
            state == RemoteBranch::Behind && lock_changed(&c.dir, &head, &base)?;
        // The MR at the remote branch head. Not for a head already in the
        // base: that is landed by git alone, and an older merged MR can have
        // the same head (a new branch at master).
        let mr = match (&remote, &remote_head) {
            (Some(r), Some(rh)) if !remote_in_base => ctx.consumer_mr(&c.dir, r, &c.name, rh),
            _ => MrFacts::None,
        };
        if let (Some(r), Some(b), Some(rh)) = (&remote, &topic, &remote_head)
            && *rh != head
        {
            details.push(format!("{r}/{b} is at {}", short(rh)));
        }
        match &mr {
            MrFacts::Read { number, read } => {
                details.push(describe_mr(&format!("{}#{number}", c.name), read))
            }
            MrFacts::NotLookedUp(why) => details.push(format!("MR: {why}")),
            _ => {}
        }
        Ok(consumer_verdict(&ConsumerFacts {
            name: c.name.clone(),
            base,
            topic,
            dirty: is_dirty(&c.dir)?,
            head_in_base,
            remote: state,
            remote_in_base,
            local_lock_change,
            landing_pushed,
            unpublished_inputs,
            entries: entry_verdicts(&manifest, rows),
            mr,
        }))
    })();
    match result {
        Ok(verdict) => Row {
            name: c.name.clone(),
            follows: None,
            verdict,
            details,
        },
        Err(e) => Row::unknown(&c.name, e, details),
    }
}

/// The verdicts of a manifest's entries, from the rows that follow their MRs.
fn entry_verdicts(manifest: &Manifest, rows: &[Row]) -> Vec<(String, Verdict)> {
    manifest
        .inputs
        .values()
        .map(|e| {
            let which = format!("{}#{}", e.repo, e.mr);
            let v = rows
                .iter()
                .find(|r| r.follows.as_deref() == Some(which.as_str()))
                .map(|r| r.verdict.clone())
                .unwrap_or_else(|| verdict(Stage::Unknown("no row".into()), "-", Waits::Nobody));
            (which, v)
        })
        .collect()
}

/// Marks the rows that follow `which` done, when the consumers that name it
/// have all landed: a merged entry needs nothing more.
fn finish_landed_entry(rows: &mut [Row], which: &str, consumers: &[&str]) {
    let landed = |name: &str| {
        rows.iter()
            .any(|r| r.name == name && r.follows.is_none() && r.verdict.stage == Stage::Landed)
    };
    if consumers.is_empty() || !consumers.iter().all(|c| landed(c)) {
        return;
    }
    for r in rows.iter_mut() {
        if r.follows.as_deref() == Some(which)
            && matches!(r.verdict.stage, Stage::Merged | Stage::Promoted)
        {
            r.verdict = verdict(Stage::Landed, "-", Waits::Nobody);
            r.details
                .push(format!("landed in {}", consumers.join(", ")));
        }
    }
}

/// A row for a manifest entry without a checkout: the MR alone.
fn mr_row(
    ctx: &Ctx,
    consumer: &str,
    path: &str,
    entry: &Entry,
    declared: &dyn Fn() -> Result<Declared, String>,
) -> Row {
    let which = format!("{}#{}", entry.repo, entry.mr);
    let dir = ctx.config.repo(&entry.repo).ok().map(|r| r.source.clone());
    if let Some(dir) = &dir
        && let Err(e) = ctx.fetch(dir, ctx.config.base_for(&entry.repo))
    {
        let mut row = Row::unknown(&which, e, vec![format!("{consumer}: {path}")]);
        row.follows = Some(which);
        return row;
    }
    let read = ctx.read_mr(&entry.repo, entry.mr, Some((dir.as_deref(), declared)));
    Row {
        details: vec![format!("{consumer}: {path}"), describe_mr(&which, &read)],
        verdict: mr_verdict(&which, &read, consumer),
        follows: Some(which.clone()),
        name: which,
    }
}

pub fn run(
    ws: &Workspace,
    config: &Config,
    reader: &dyn PrReader,
    fetcher: &Fetcher,
) -> Result<()> {
    let ctx = Ctx {
        config,
        reader,
        fetcher,
        nix: OnceCell::new(),
    };
    let hops = hops(ws, config)?;
    let mut manifests = BTreeMap::new();
    for c in ws.checkouts.iter().filter(|c| c.is_consumer()) {
        if let Ok(Some(m)) = Manifest::read(&c.dir) {
            manifests.insert(c.name.clone(), m);
        }
    }
    let mut rows = Vec::new();
    for c in &ws.checkouts {
        if let Some((repo, _)) = &c.shared {
            rows.push(shared_row(&ctx, ws, &hops, &manifests, c, repo));
        }
    }
    // Entries whose MR no checkout follows.
    for (consumer, m) in &manifests {
        for (path, e) in &m.inputs {
            let which = format!("{}#{}", e.repo, e.mr);
            if rows
                .iter()
                .any(|r| r.follows.as_deref() == Some(which.as_str()))
            {
                continue;
            }
            let declared = || match declaring_flake(ws, &hops, consumer, path) {
                Some(src) => ctx.declared(&src, last_segment(path)),
                None => Err(format!("no flake in the workspace declares {path}")),
            };
            rows.push(mr_row(&ctx, consumer, path, e, &declared));
        }
    }
    for c in ws.checkouts.iter().filter(|c| c.is_consumer()) {
        let row = consumer_row(&ctx, &hops, ws, &rows, c);
        rows.push(row);
    }
    let mut named: BTreeMap<String, Vec<&str>> = BTreeMap::new();
    for (consumer, m) in &manifests {
        for e in m.inputs.values() {
            named
                .entry(format!("{}#{}", e.repo, e.mr))
                .or_default()
                .push(consumer);
        }
    }
    for (which, consumers) in &named {
        finish_landed_entry(&mut rows, which, consumers);
    }
    for r in &rows {
        r.print();
    }
    Ok(())
}

/// `swl status --from <consumer>#<MR>`: the published change, without a
/// workspace: the consumer MR from git and the forge, and a row per
/// swl.toml entry from the forge.
pub fn run_from(
    config: &Config,
    from: &str,
    reader: &dyn PrReader,
    fetcher: &Fetcher,
) -> Result<()> {
    let ctx = Ctx {
        config,
        reader,
        fetcher,
        nix: OnceCell::new(),
    };
    let (consumer, n) = parse_from(from)?;
    let (source, remote) = source_remote(config, &consumer)?;
    let base = config.base_for(&consumer).to_string();
    let pull = format!("refs/pull/{n}/head");
    let refs = git(&source, &["ls-remote", &remote, &pull, "refs/heads/*"])
        .with_context(|| format!("reading {consumer}#{n} from {remote}"))?;
    let refs = parse_ls_remote(&refs);
    let head = refs
        .iter()
        .find(|(_, r)| *r == pull)
        .map(|(sha, _)| sha.clone())
        .ok_or_else(|| anyhow!("{consumer}#{n}: no {pull} on the forge: no such MR"))?;
    let branches: Vec<&str> = refs
        .iter()
        .filter(|(sha, _)| *sha == head)
        .filter_map(|(_, r)| r.strip_prefix("refs/heads/"))
        .collect();
    fetcher.fetch(&source, &remote)?;
    if fetcher.enabled() {
        git(&source, &["fetch", "--quiet", &remote, &pull])?;
    }
    if rev(&source, &head).is_none() {
        bail!(
            "{consumer}#{n}: its head {} is not in {}; rerun without --no-fetch",
            short(&head),
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

    let mut rows = Vec::new();
    for (path, e) in &manifest.inputs {
        // The flake that declares the input: the consumer at the MR head for a
        // root input; for a nested one, the parent entry's MR head.
        let declared = || -> Result<Declared, String> {
            match path.rsplit_once('/') {
                None => ctx.declared(
                    &FlakeSource::Git {
                        dir: source.clone(),
                        rev: head.clone(),
                    },
                    path,
                ),
                Some((parent, name)) => {
                    let pe = manifest.inputs.get(parent).ok_or_else(|| {
                        format!("{parent} has no entry to read its flake.nix from")
                    })?;
                    let dir = config
                        .repo(&pe.repo)
                        .map_err(|e| format!("{e:#}"))?
                        .source
                        .clone();
                    let info = reader
                        .pr(&pe.repo, pe.mr)
                        .map_err(|e| format!("{e:#}"))?
                        .ok_or_else(|| format!("{}#{}: {}", pe.repo, pe.mr, reader.not_found()))?;
                    let at = match info.state {
                        PrState::Merged { merge_commit } => merge_commit,
                        _ => info.head_sha,
                    };
                    ctx.declared(&FlakeSource::Git { dir, rev: at }, name)
                }
            }
        };
        rows.push(mr_row(&ctx, &consumer, path, e, &declared));
    }

    let which = format!("{consumer}#{n}");
    let mut details = vec![
        format!(
            "{pull} at {}, {}",
            short(&head),
            match branches.as_slice() {
                [] => "the head of no branch".to_string(),
                bs => format!("the head of {}", bs.join(", ")),
            }
        ),
        entry_lines(&manifest),
    ];
    let read = ctx.read_mr(&consumer, n, None);
    details.push(describe_mr(&which, &read));
    let head_in_base = ancestor(&source, &head, &base) == Ok(true);
    let row = (|| -> Result<Verdict> {
        let landing_pushed = landing_at(&source, &head, &base)?;
        Ok(consumer_verdict(&ConsumerFacts {
            name: consumer.clone(),
            base: base.clone(),
            topic: Some(branches.first().map_or("(gone)", |b| b).to_string()),
            dirty: false,
            head_in_base,
            remote: if branches.is_empty() {
                RemoteBranch::Other
            } else {
                RemoteBranch::Same
            },
            remote_in_base: head_in_base,
            local_lock_change: false,
            landing_pushed,
            unpublished_inputs: Vec::new(),
            entries: entry_verdicts(&manifest, &rows),
            mr: MrFacts::Read { number: n, read },
        }))
    })();
    rows.push(match row {
        Ok(v) if branches.is_empty() && v.stage != Stage::Landed => Row {
            name: consumer.clone(),
            follows: None,
            verdict: verdict(
                Stage::Unknown(format!(
                    "the MR head {} is the head of no branch: the branch moved on (Forgejo has not updated the MR yet), or its branch is gone",
                    short(&head)
                )),
                format!("check {which} on the forge"),
                Waits::Anyone,
            ),
            details,
        },
        Ok(verdict) => Row {
            name: consumer.clone(),
            follows: None,
            verdict,
            details,
        },
        Err(e) => Row::unknown(&consumer, e, details),
    });
    let entries: Vec<String> = manifest
        .inputs
        .values()
        .map(|e| format!("{}#{}", e.repo, e.mr))
        .collect();
    for which in &entries {
        finish_landed_entry(&mut rows, which, &[consumer.as_str()]);
    }
    for r in &rows {
        r.print();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pr(draft: bool) -> PrInfo {
        PrInfo {
            number: 1,
            state: PrState::Open,
            head_sha: "h".into(),
            head_ref: "sl-1-x".into(),
            title: "t".into(),
            draft,
        }
    }

    fn shared() -> SharedFacts {
        SharedFacts {
            repo: "swan-lake-common".into(),
            base: "origin/master".into(),
            branch: Some("sl-1-x".into()),
            dirty: false,
            head_in_base: false,
            in_locked: false,
            remote: RemoteBranch::Same,
            consumers: vec!["swan-lake".into()],
            missing_entry: Vec::new(),
            mr: MrFacts::None,
        }
    }

    fn read(read: MrRead) -> MrFacts {
        MrFacts::Read { number: 1, read }
    }

    fn open(draft: bool, ci: Option<CiState>) -> MrFacts {
        read(MrRead::Open {
            info: pr(draft),
            ci: Ok(ci),
        })
    }

    fn merged(on_forge: Result<bool, String>, source: Option<Result<bool, String>>) -> MrFacts {
        read(MrRead::Merged {
            merge_commit: "m".into(),
            promotion: Some(Promotion {
                branch: "swan-lake-current".into(),
                on_forge,
                declared_source: source.map(|s| ("https://github.com/x".into(), s)),
            }),
        })
    }

    fn sv(f: SharedFacts) -> (String, String, Waits) {
        let v = shared_verdict(&f);
        (v.stage.to_string(), v.next, v.waits)
    }

    #[test]
    fn shared_stages_in_order() {
        let s = |f: SharedFacts| sv(f).0;
        assert_eq!(
            s(SharedFacts {
                remote: RemoteBranch::Missing,
                ..shared()
            }),
            "local"
        );
        assert_eq!(
            s(SharedFacts {
                branch: None,
                ..shared()
            }),
            "local"
        );
        assert_eq!(
            sv(SharedFacts {
                remote: RemoteBranch::Behind,
                mr: open(false, None),
                ..shared()
            }),
            (
                "needs publish".into(),
                "swl publish swan-lake".into(),
                Waits::Anyone
            )
        );
        assert_eq!(
            s(SharedFacts {
                missing_entry: vec!["nixos-config".into()],
                mr: open(false, None),
                ..shared()
            }),
            "needs publish"
        );
        assert_eq!(s(shared()), "needs publish");
        assert_eq!(
            sv(SharedFacts {
                mr: open(true, Some(CiState::Green)),
                ..shared()
            }),
            (
                "MR open (draft)".into(),
                "mark swan-lake-common#1 ready".into(),
                Waits::Anyone
            )
        );
        assert_eq!(
            sv(SharedFacts {
                mr: open(false, Some(CiState::Pending)),
                ..shared()
            })
            .2,
            Waits::Nobody
        );
        assert_eq!(
            sv(SharedFacts {
                mr: open(false, Some(CiState::Green)),
                ..shared()
            }),
            (
                "CI green".into(),
                "merge swan-lake-common#1".into(),
                Waits::Owner
            )
        );
        assert_eq!(
            sv(SharedFacts {
                mr: merged(Ok(false), None),
                ..shared()
            }),
            (
                "merged".into(),
                "promote swan-lake-current".into(),
                Waits::Owner
            )
        );
        let mirror_pending = shared_verdict(&SharedFacts {
            mr: merged(Ok(true), Some(Ok(false))),
            ..shared()
        });
        assert_eq!(mirror_pending.stage, Stage::Promoted);
        assert_eq!(mirror_pending.waits, Waits::Nobody);
        assert!(!mirror_pending.landable);
        let promoted = shared_verdict(&SharedFacts {
            mr: merged(Ok(true), Some(Ok(true))),
            ..shared()
        });
        assert!(promoted.landable);
        assert!(promoted.next.starts_with("swl land swan-lake"));
        // A merged MR wins over a detached or deleted branch.
        assert_eq!(
            s(SharedFacts {
                branch: None,
                remote: RemoteBranch::Missing,
                mr: merged(Ok(true), None),
                ..shared()
            }),
            "promoted"
        );
    }

    #[test]
    fn shared_without_an_mr_or_readable_forge() {
        let s = |f: SharedFacts| sv(f).0;
        assert_eq!(
            s(SharedFacts {
                in_locked: true,
                ..shared()
            }),
            "no MR needed (in the rev swan-lake locks)"
        );
        assert_eq!(
            s(SharedFacts {
                in_locked: true,
                dirty: true,
                ..shared()
            }),
            "local"
        );
        assert_eq!(
            sv(SharedFacts {
                head_in_base: true,
                ..shared()
            }),
            (
                "no MR needed (in origin/master)".into(),
                "bump the lock in swan-lake by hand".into(),
                Waits::Anyone
            )
        );
        assert_eq!(
            s(SharedFacts {
                mr: read(MrRead::NotFound("not readable".into())),
                ..shared()
            }),
            "pushed"
        );
        assert_eq!(
            s(SharedFacts {
                mr: read(MrRead::Error("forge API unreachable".into())),
                ..shared()
            }),
            "unknown (forge API unreachable)"
        );
        assert!(
            s(SharedFacts {
                mr: MrFacts::Conflict(vec![1, 2]),
                ..shared()
            })
            .starts_with("unknown (")
        );
        assert_eq!(
            s(SharedFacts {
                mr: read(MrRead::Closed),
                ..shared()
            }),
            "needs publish"
        );
    }

    fn consumer() -> ConsumerFacts {
        ConsumerFacts {
            name: "swan-lake".into(),
            base: "origin/master".into(),
            topic: Some("sl-1-x".into()),
            dirty: false,
            head_in_base: false,
            remote: RemoteBranch::Same,
            remote_in_base: false,
            local_lock_change: false,
            landing_pushed: false,
            unpublished_inputs: Vec::new(),
            entries: Vec::new(),
            mr: MrFacts::None,
        }
    }

    fn cv(f: ConsumerFacts) -> (String, String, Waits) {
        let v = consumer_verdict(&f);
        (v.stage.to_string(), v.next, v.waits)
    }

    #[test]
    fn consumer_stages_in_order() {
        let s = |f: ConsumerFacts| cv(f).0;
        assert_eq!(
            s(ConsumerFacts {
                topic: None,
                ..consumer()
            }),
            "local"
        );
        assert_eq!(
            s(ConsumerFacts {
                topic: None,
                head_in_base: true,
                ..consumer()
            }),
            "no change (HEAD is in origin/master)"
        );
        assert_eq!(
            s(ConsumerFacts {
                remote: RemoteBranch::Missing,
                ..consumer()
            }),
            "local"
        );
        assert_eq!(
            s(ConsumerFacts {
                remote: RemoteBranch::Behind,
                ..consumer()
            }),
            "needs publish"
        );
        assert!(
            cv(ConsumerFacts {
                remote: RemoteBranch::Behind,
                local_lock_change: true,
                ..consumer()
            })
            .1
            .starts_with("swl land")
        );
        assert_eq!(
            s(ConsumerFacts {
                unpublished_inputs: vec!["common".into()],
                ..consumer()
            }),
            "needs publish"
        );
        assert_eq!(
            cv(consumer()),
            (
                "pushed".into(),
                "swl publish swan-lake".into(),
                Waits::Anyone
            )
        );
        assert_eq!(
            s(ConsumerFacts {
                mr: open(false, Some(CiState::Green)),
                ..consumer()
            }),
            "CI green"
        );
        assert_eq!(
            cv(ConsumerFacts {
                landing_pushed: true,
                mr: open(false, Some(CiState::Pending)),
                ..consumer()
            }),
            (
                "landing pushed".into(),
                "request merge when checks succeed".into(),
                Waits::Owner
            )
        );
        // The landing commit is pushed: only an open MR is `landing pushed`.
        assert_eq!(
            cv(ConsumerFacts {
                landing_pushed: true,
                ..consumer()
            }),
            (
                "pushed".into(),
                "open the MR (the landing commit is pushed)".into(),
                Waits::Anyone
            )
        );
        assert_eq!(
            cv(ConsumerFacts {
                landing_pushed: true,
                mr: open(false, Some(CiState::Failing("failure".into()))),
                ..consumer()
            }),
            (
                "landing pushed, CI failing".into(),
                "fix CI".into(),
                Waits::Anyone
            )
        );
        assert_eq!(
            cv(ConsumerFacts {
                landing_pushed: true,
                mr: open(true, Some(CiState::Green)),
                ..consumer()
            }),
            (
                "MR open (draft)".into(),
                "mark swan-lake#1 ready".into(),
                Waits::Anyone
            )
        );
        for (e, next, waits) in [
            (LOGIN_REJECTED, "fj auth login", Waits::Anyone),
            ("forge API unreachable", "rerun swl status", Waits::Nobody),
        ] {
            assert_eq!(
                cv(ConsumerFacts {
                    landing_pushed: true,
                    mr: read(MrRead::Error(e.into())),
                    ..consumer()
                }),
                (format!("unknown ({e})"), next.into(), waits)
            );
        }
        assert_eq!(
            cv(ConsumerFacts {
                landing_pushed: true,
                mr: read(MrRead::NotFound("private".into())),
                ..consumer()
            })
            .0,
            "pushed"
        );
        assert_eq!(
            s(ConsumerFacts {
                mr: read(MrRead::Merged {
                    merge_commit: "m".into(),
                    promotion: None
                }),
                ..consumer()
            }),
            "landed"
        );
        // A local HEAD in the base does not land an open MR whose branch is
        // not.
        assert_eq!(
            s(ConsumerFacts {
                head_in_base: true,
                remote: RemoteBranch::Other,
                mr: open(false, Some(CiState::Green)),
                ..consumer()
            }),
            "needs publish"
        );
        assert_eq!(
            cv(ConsumerFacts {
                mr: read(MrRead::Error(LOGIN_REJECTED.into())),
                ..consumer()
            }),
            (
                format!("unknown ({LOGIN_REJECTED})"),
                "fj auth login".into(),
                Waits::Anyone
            )
        );
        // Without a readable MR, from git: the branch head is in master.
        assert_eq!(
            s(ConsumerFacts {
                head_in_base: true,
                remote_in_base: true,
                mr: read(MrRead::NotFound("private".into())),
                ..consumer()
            }),
            "landed"
        );
    }

    #[test]
    fn a_consumer_mr_waits_for_its_entries_then_lands() {
        let waiting = verdict(Stage::Merged, "promote swan-lake-current", Waits::Owner);
        let f = ConsumerFacts {
            mr: open(false, Some(CiState::Green)),
            entries: vec![("swan-lake-common#1".into(), waiting)],
            ..consumer()
        };
        assert_eq!(
            cv(f.clone()),
            (
                "CI green".into(),
                "wait for swan-lake-common#1 (promote swan-lake-current)".into(),
                Waits::Owner
            )
        );
        let ready = Verdict {
            landable: true,
            ..verdict(Stage::Promoted, "swl land", Waits::Anyone)
        };
        let f = ConsumerFacts {
            entries: vec![("swan-lake-common#1".into(), ready)],
            ..f
        };
        assert_eq!(cv(f.clone()).1, "swl land swan-lake (not implemented yet)");
        // CI pending waits for nobody: deferred too.
        let pending = ConsumerFacts {
            mr: open(false, Some(CiState::Pending)),
            ..f.clone()
        };
        assert_eq!(cv(pending).1, "swl land swan-lake (not implemented yet)");
        // A draft or failing CI is the consumer's own step first.
        let draft = ConsumerFacts {
            mr: open(true, Some(CiState::Green)),
            entries: vec![(
                "swan-lake-common#1".into(),
                verdict(Stage::Merged, "promote swan-lake-current", Waits::Owner),
            )],
            ..f.clone()
        };
        assert_eq!(
            cv(draft),
            (
                "MR open (draft)".into(),
                "mark swan-lake#1 ready".into(),
                Waits::Anyone
            )
        );
        let failing = ConsumerFacts {
            mr: open(false, Some(CiState::Failing("failure".into()))),
            ..f
        };
        assert_eq!(
            cv(failing),
            (
                "CI failing (failure)".into(),
                "fix it, then swl publish swan-lake".into(),
                Waits::Anyone
            )
        );
    }
}

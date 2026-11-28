//! `swl views`: the umbrella's read-only checkouts on `master`, one per repo
//! that sets `view` in the config, fast-forwarded to the repo's base. Never
//! a rebase: a view with uncommitted changes, on another branch or with
//! commits of its own is skipped with a warning. `land` and `bump` run it
//! quietly at the end of a successful run.

use std::{path::Path, process::ExitCode};

use anyhow::Result;

use crate::{
    bump::{is_ancestor, rev_parse},
    config::Config,
    git::{git, git_ok, short},
    report::Report,
    style::{self, Tone},
    ws_add::{Fetcher, base_remote},
};

const CMD: &str = "swl views";

/// The branch a view is on.
const VIEW_BRANCH: &str = "master";

/// What became of one view.
#[derive(Debug)]
enum Outcome {
    FastForwarded {
        from: String,
        to: String,
    },
    UpToDate(String),
    /// Left as it is, and why.
    Skipped(String),
}

/// The view at `view` fast-forwarded to `base`: its repository's base
/// remote fetched first (the source's, for a worktree of it).
fn update(view: &Path, base: &str, fetcher: &Fetcher) -> Result<Outcome> {
    let skip = |why: String| {
        Ok(Outcome::Skipped(format!(
            "skipped {}: {why}",
            view.display()
        )))
    };
    if !view.exists() {
        return skip("it does not exist".into());
    }
    let top = git_ok(view, &["rev-parse", "--show-toplevel"]);
    let is_top = top.is_some_and(|t| Path::new(&t).canonicalize().ok() == view.canonicalize().ok());
    if !is_top {
        return skip("it is not a git checkout".into());
    }
    match git_ok(view, &["symbolic-ref", "--quiet", "--short", "HEAD"]) {
        Some(b) if b == VIEW_BRANCH => {}
        Some(b) => return skip(format!("it is on the branch {b}, not {VIEW_BRANCH}")),
        None => {
            let head = git(view, &["rev-parse", "HEAD"])?;
            return skip(format!(
                "it is detached at {}, not on {VIEW_BRANCH}",
                short(&head)
            ));
        }
    }
    if !git(view, &["status", "--porcelain", "--untracked-files=no"])?.is_empty() {
        return skip("it has uncommitted changes".into());
    }
    if let Some(remote) = base_remote(view, base)? {
        fetcher.fetch(view, &remote)?;
    }
    let Some(target) = rev_parse(view, base) else {
        return skip(format!("{base} is not known there"));
    };
    let head = git(view, &["rev-parse", "HEAD"])?;
    if head == target {
        return Ok(Outcome::UpToDate(target));
    }
    if !is_ancestor(view, &head, &target) {
        let own = git(view, &["rev-list", "--count", &format!("{target}..{head}")])?;
        return skip(format!(
            "{VIEW_BRANCH} has {own} commit(s) not on {base} (fast-forward only)"
        ));
    }
    git(view, &["merge", "--ff-only", "--quiet", base])?;
    let to = git(view, &["rev-parse", "HEAD"])?;
    Ok(Outcome::FastForwarded { from: head, to })
}

/// The configured views, by repo name: `(repo, view, base)`.
fn views(config: &Config) -> Vec<(&str, &Path, &str)> {
    config
        .repos()
        .filter_map(|(name, rc)| Some((name, rc.view.as_deref()?, rc.base())))
        .collect()
}

fn ff_line(view: &Path, base: &str, from: &str, to: &str) -> String {
    format!(
        "fast-forwarded {} from {} to {} ({base})",
        view.display(),
        short(from),
        short(to)
    )
}

/// `swl views`: a section per view, closed by its outcome. A view skipped
/// is a warning; a view that git fails to fetch or fast-forward is an error
/// (exit 2), after the others are done.
pub fn run(config: &Config, fetcher: &Fetcher) -> Result<ExitCode> {
    let report = Report::new(CMD);
    let views = views(config);
    if views.is_empty() {
        report.say(Tone::Plain, "no repo of the config sets view");
        return Ok(ExitCode::SUCCESS);
    }
    let mut errors = Vec::new();
    for (repo, view, base) in views {
        report.section(repo);
        match update(view, base, fetcher) {
            Ok(Outcome::FastForwarded { from, to }) => {
                report.end(Tone::Good, ff_line(view, base, &from, &to));
            }
            Ok(Outcome::UpToDate(at)) => report.end(
                Tone::Good,
                format!("{} is up to date at {}", view.display(), short(&at)),
            ),
            Ok(Outcome::Skipped(why)) => report.end(Tone::Warn, why),
            Err(e) => {
                tracing::debug!(repo, "the view failed: {e:#}");
                errors.push(format!("{repo}: {e:#}"));
            }
        }
    }
    report.close();
    for e in &errors {
        style::tell(CMD, Tone::Error, format!("error: {e}"));
    }
    Ok(if errors.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(2)
    })
}

/// The views after a successful `land` or `bump` (`cmd`): one section,
/// `views`, with what moved or was skipped; failures are warnings too.
pub fn quietly(cmd: &'static str, config: &Config) {
    let report = Report::new(cmd);
    report.section("views");
    let fetcher = Fetcher::new(true);
    for (repo, view, base) in views(config) {
        match update(view, base, &fetcher) {
            Ok(Outcome::FastForwarded { from, to }) => report.outcome(
                Tone::Good,
                format!("{repo}: {}", ff_line(view, base, &from, &to)),
            ),
            Ok(Outcome::UpToDate(_)) => {}
            Ok(Outcome::Skipped(why)) => report.warn(format!("{repo}: {why}")),
            Err(e) => report.warn(format!(
                "{repo}: {e:#}; rerun swl views (or swl views --no-fetch)"
            )),
        }
    }
}

//! `swl promote [<rev>]`: fast-forwards common's promotion branch
//! (`swan-lake-current`) to a commit of its master that reached it without
//! a consumer, and waits for the declared sources (the GitHub mirror), as
//! `land` promotes a merged MR. The branch and the sources are those that
//! `bump` promotes ([`bump::promotion_target`]). Needs no workspace: it
//! works in common's configured source. `--dry-run` writes nothing.

use std::process::ExitCode;

use anyhow::{Result, anyhow, bail};

use crate::{
    bump::{self, COMMON_REPO, base_branch, is_ancestor, rev_parse},
    config::Config,
    forge::AnonApi,
    git::short,
    land::{self, DryRunWriter, Lander, Step, stop},
    status::Waits,
    style::{self, Tone},
    ws_add::{Fetcher, base_remote},
};

const CMD: &str = "swl promote";

/// `swl promote`: `rev` (default common's master on its remote) promoted
/// and in the declared sources.
pub fn run(config: &Config, rev: Option<&str>, opts: &land::Options) -> Result<ExitCode> {
    let rc = config.repo(COMMON_REPO)?;
    let src = &rc.source;
    let base = rc.base();
    let Some(remote) = base_remote(src, base)? else {
        bail!(
            "{COMMON_REPO}: its base {base} names no remote of {}",
            src.display()
        );
    };
    let master = base_branch(base);
    let fetcher = Fetcher::new(true);
    fetcher.fetch(src, &remote)?;
    let target = bump::promotion_target(config, &fetcher, CMD)?;
    let branch = &target.branch;
    let tracking = |b: &str| {
        rev_parse(src, &format!("refs/remotes/{remote}/{b}")).ok_or_else(|| {
            anyhow!(
                "{COMMON_REPO}: {remote}/{b} is not known in {}",
                src.display()
            )
        })
    };
    let head = tracking(master)?;
    let current = tracking(branch)?;
    let (name, commit) = match rev {
        None => (format!("{remote}/{master}"), head.clone()),
        Some(r) => match rev_parse(src, r) {
            Some(c) => (r.to_string(), c),
            None => bail!("{r} is not a commit of {COMMON_REPO} ({})", src.display()),
        },
    };
    if !is_ancestor(src, &commit, &head) {
        bail!(
            "{name} ({}) is not {remote}/{master} ({}) or in its history: {CMD} promotes what is \
             on {master} only",
            short(&commit),
            short(&head)
        );
    }
    let which = format!("{COMMON_REPO} {name}");
    // The promotion is a git push: nothing is written on the forge's API.
    let (reader, writer) = (AnonApi::from_env(), DryRunWriter);
    let lander = Lander::new(CMD, config, &reader, &writer, opts);
    let moved = if is_ancestor(src, &commit, &current) {
        style::say(
            CMD,
            Tone::Plain,
            format!(
                "{branch} has {} already (at {})",
                short(&commit),
                short(&current)
            ),
        );
        false
    } else if !is_ancestor(src, &current, &commit) {
        return Ok(end(
            stop(
                Waits::Owner,
                format!(
                    "promote {branch}: {remote}/{branch} at {} has commits that {name} at {} does \
                     not, so it cannot be fast-forwarded (swl never force-pushes a promotion \
                     branch)",
                    short(&current),
                    short(&commit)
                ),
            )?,
            opts.dry_run,
        ));
    } else {
        match lander.promote(&which, COMMON_REPO, &commit, branch)? {
            Step::Progress => {
                style::say(
                    CMD,
                    Tone::Good,
                    format!(
                        "promoted {branch} to {}; next: swl bump, to move the consumers",
                        short(&commit)
                    ),
                );
                true
            }
            step => return Ok(end(step, opts.dry_run)),
        }
    };
    for url in &target.mirrors {
        match lander.mirror(&which, url, branch, &commit)? {
            Step::Done => {}
            step => return Ok(end(step, opts.dry_run)),
        }
    }
    let msg = if moved {
        format!("done: {branch} at {}", short(&commit))
    } else {
        "nothing to promote".to_string()
    };
    style::say(CMD, Tone::Good, msg);
    Ok(ExitCode::SUCCESS)
}

/// The end of a run that stopped at `step`: the exit status of what it
/// waits for, as `land`'s.
fn end(step: Step, dry_run: bool) -> ExitCode {
    match step {
        Step::Planned(then) => land::planned(CMD, &then),
        Step::Stop(waits, msg) => land::exit(CMD, waits, &msg, dry_run),
        Step::Done | Step::Progress => ExitCode::SUCCESS,
    }
}

//! `swl ls`: all work in progress. One line per workspace below the repos
//! config's `workspaces` directory, with where its change stands (from
//! `swl status`'s rows) and whether `swl ws prune` would remove it; then the
//! forge's open MRs that no workspace holds, grouped by change, each group
//! with the `swl start --from` that joins it.

use std::{io::Write, path::Path};

use anyhow::{Result, bail};

use crate::{
    bump::consumer_names,
    config::Config,
    forge::{CiState, ForgeReader, PrInfo, PrReader},
    git::git_ok,
    manifest::Manifest,
    open_mrs::{self, Held},
    status::{self, Row, Stage, Verdict, Waits},
    status_tree::verdict_style,
    style::{BOLD, DIM, OWNER, pad, paint, width},
    workspace::Workspace,
    workspace_location::directories,
    ws_add::Fetcher,
    ws_prune,
};

/// What `--no-fetch` answers instead of the forge.
const OFFLINE: &str = "--no-fetch: the forge is not read";

/// The reader of `--no-fetch`: no forge request.
struct Offline;

impl PrReader for Offline {
    fn pr(&self, _: &str, _: u64) -> Result<Option<PrInfo>> {
        bail!(OFFLINE)
    }

    fn commit_status(&self, _: &str, _: &str) -> Result<Option<CiState>> {
        bail!(OFFLINE)
    }
}

/// One workspace's line.
struct WorkspaceLine {
    name: String,
    verdict: Verdict,
    removable: bool,
}

fn unknown(e: &anyhow::Error) -> Verdict {
    Verdict {
        stage: Stage::Unknown(format!("{e:#}")),
        next: "rerun swl status".to_string(),
        waits: Waits::Nobody,
        landable: false,
    }
}

/// How far a stage is along the change; `unknown` first.
fn rank(stage: &Stage) -> u8 {
    match stage {
        Stage::Unknown(_) => 0,
        Stage::Local => 1,
        Stage::NeedsPublish => 2,
        Stage::Pushed => 3,
        Stage::MrOpen { .. } | Stage::Obsolete(_) => 4,
        Stage::Ci(_) => 5,
        Stage::Merged => 6,
        Stage::Promoted => 7,
        Stage::LandingPushed { .. } => 8,
        Stage::Landed => 9,
        Stage::NoMrNeeded(_) => 10,
        Stage::NoChange(_) => 11,
        Stage::NotAChange(_) => 12,
    }
}

/// The workspace's one line from its rows: of the roots of `status`'s tree
/// (the consumers, and rows under no consumer), whose verdicts already wait
/// for the rows below them, an unknown one first, else the least advanced
/// that has a next step, else the least advanced. Its next step ends with
/// the other roots that have a next step at another stage, `(+ <row>:
/// <stage>, …)`.
fn summary(ws: &Workspace, rows: &[Row]) -> Verdict {
    if ws.checkouts.is_empty() {
        return Verdict {
            stage: Stage::NoChange("no checkouts".to_string()),
            next: "-".to_string(),
            waits: Waits::Nobody,
            landable: false,
        };
    }
    let roots: Vec<&Row> = rows.iter().filter(|r| r.under.is_empty()).collect();
    let Some((chosen, _)) = roots.iter().enumerate().min_by_key(|(i, r)| {
        let v = &r.verdict;
        let class = match (&v.stage, v.next == "-") {
            (Stage::Unknown(_), _) => 0,
            (_, false) => 1,
            (_, true) => 2,
        };
        (class, rank(&v.stage), *i)
    }) else {
        return unknown(&anyhow::anyhow!("no rows"));
    };
    let mut verdict = roots[chosen].verdict.clone();
    let others: Vec<String> = roots
        .iter()
        .enumerate()
        .filter(|(i, r)| *i != chosen && r.verdict.next != "-" && r.verdict.stage != verdict.stage)
        .map(|(_, r)| format!("{}: {}", r.name, r.verdict.stage))
        .collect();
    if !others.is_empty() {
        verdict.next = format!("{} (+ {})", verdict.next, others.join(", "));
    }
    verdict
}

/// What the workspace holds of the forge's MRs: its checkouts' branches,
/// the workspace's branch (its name lower-cased, as `swl start` names
/// branches) in its consumers' repos, also with the checkout detached or on
/// another branch, and its consumers' `swl.toml` entries.
fn hold(ws: &Workspace, held: &mut Held) {
    let ws_branch = ws
        .root
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase());
    for c in &ws.checkouts {
        let repo = c
            .shared
            .as_ref()
            .map_or(c.name.as_str(), |(r, _)| r.as_str());
        let branches = held.branches.entry(repo.to_string()).or_default();
        if let Some(b) = git_ok(&c.dir, &["symbolic-ref", "--quiet", "--short", "HEAD"]) {
            branches.push(b);
        }
        if !c.is_consumer() {
            continue;
        }
        branches.extend(ws_branch.clone());
        match Manifest::read(&c.dir) {
            Ok(Some(m)) => held
                .entries
                .extend(m.inputs.values().map(|e| format!("{}#{}", e.repo, e.mr))),
            Ok(None) => {}
            Err(e) => {
                tracing::debug!(
                    target: "swl::ls",
                    checkout = %c.name,
                    error = %format!("{e:#}"),
                    "its entries hold nothing"
                );
            }
        }
    }
}

fn workspace_line(
    dir: &Path,
    config: &Config,
    reader: &dyn PrReader,
    fetcher: &Fetcher,
    held: &mut Held,
) -> WorkspaceLine {
    let name = dir.file_name().map_or_else(
        || dir.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    );
    let _span = tracing::debug_span!("workspace", %name).entered();
    let ws = match Workspace::scan(dir) {
        Ok(ws) => ws,
        Err(e) => {
            return WorkspaceLine {
                name,
                verdict: unknown(&e),
                removable: false,
            };
        }
    };
    hold(&ws, held);
    let verdict = match status::collect(&ws, config, reader, fetcher) {
        Ok(rows) => summary(&ws, &rows),
        Err(e) => unknown(&e),
    };
    let removable = match ws_prune::keep_reason(&ws, config) {
        Ok(None) => true,
        Ok(Some(why)) => {
            tracing::debug!(target: "swl::ls", %why, "kept");
            false
        }
        Err(e) => {
            tracing::debug!(target: "swl::ls", error = %format!("{e:#}"), "kept");
            false
        }
    };
    WorkspaceLine {
        name,
        verdict,
        removable,
    }
}

/// `<name>  [removable]  <stage>  waits: <who>  next: <step>`, the columns
/// aligned and coloured as `status` colours a row.
fn local_lines(lines: &[WorkspaceLine]) -> Vec<String> {
    let name_w = lines.iter().map(|l| width(&l.name)).max().unwrap_or(0);
    let any_removable = lines.iter().any(|l| l.removable);
    let stage_w = lines
        .iter()
        .map(|l| width(&l.verdict.stage.to_string()))
        .max()
        .unwrap_or(0);
    let waits_w = lines
        .iter()
        .map(|l| width(&format!("waits: {}", l.verdict.waits)))
        .max()
        .unwrap_or(0);
    let mut out = Vec::new();
    for l in lines {
        let v = &l.verdict;
        let (color, owner) = verdict_style(v);
        let stage = v.stage.to_string();
        let waits = format!("waits: {}", v.waits);
        let marker = match (any_removable, l.removable) {
            (false, _) => String::new(),
            (true, true) => "removable  ".to_string(),
            (true, false) => " ".repeat(width("removable  ")),
        };
        out.push(format!(
            "  {}{}  {marker}{}{}  {}{}  next: {}",
            paint(BOLD, &l.name),
            pad(name_w, width(&l.name)),
            paint(color, &stage),
            pad(stage_w, width(&stage)),
            paint(color, &waits),
            pad(waits_w, width(&waits)),
            if owner {
                paint(OWNER, &v.next)
            } else {
                v.next.clone()
            },
        ));
    }
    out
}

/// `swl ls [--no-fetch]`. Each workspace's rows come from `swl status`'s
/// computation, one workspace after another, with one fetcher, so that
/// each source is fetched once. `--no-fetch` neither fetches nor asks the
/// forge, and lists no open MRs.
pub fn run(config: &Config, no_fetch: bool) -> Result<()> {
    let root = config.workspaces()?;
    let fetcher = Fetcher::new(!no_fetch);
    let forge = (!no_fetch).then(ForgeReader::from_env);
    let reader: &dyn PrReader = match &forge {
        Some(r) => r,
        None => &Offline,
    };
    let mut held = Held::default();
    let lines: Vec<WorkspaceLine> = directories(&root)?
        .iter()
        .map(|dir| workspace_line(dir, config, reader, &fetcher, &mut held))
        .collect();
    let mut stdout = anstream::stdout();
    if lines.is_empty() {
        let _ = writeln!(stdout, "no workspaces in {}", root.display());
    } else {
        let _ = writeln!(stdout, "workspaces in {}:", root.display());
        for l in local_lines(&lines) {
            let _ = writeln!(stdout, "{l}");
        }
    }
    let Some(reader) = &forge else {
        let _ = writeln!(
            stdout,
            "\n{}",
            paint(DIM, format!("{OFFLINE}: its open MRs are not listed"))
        );
        return Ok(());
    };
    let listing = open_mrs::list(config, reader);
    let groups = open_mrs::groups(listing.mrs.into_iter().filter(|m| !held.holds(m)));
    let _ = writeln!(stdout);
    if groups.is_empty() && listing.unread.is_empty() {
        let _ = writeln!(stdout, "no open MR on the forge that no workspace holds");
        return Ok(());
    }
    let _ = writeln!(stdout, "open on the forge, in no workspace:");
    let consumers = if groups.is_empty() {
        Ok(Vec::new())
    } else {
        consumer_names(config, &fetcher).map_err(|e| format!("{e:#}"))
    };
    for l in open_mrs::lines(&groups, &consumers) {
        let _ = writeln!(stdout, "{l}");
    }
    for (repo, why) in &listing.unread {
        let _ = writeln!(stdout, "  {}: not read ({why})", paint(BOLD, repo));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::{status::RowFacts, workspace::Checkout};

    fn row(name: &str, under: &[(&str, &str)], stage: Stage, next: &str) -> Row {
        Row {
            name: name.into(),
            follows: None,
            under: under
                .iter()
                .map(|(c, p)| (c.to_string(), p.to_string()))
                .collect(),
            verdict: Verdict {
                stage,
                next: next.into(),
                waits: Waits::Anyone,
                landable: false,
            },
            details: Vec::new(),
            facts: RowFacts::None,
        }
    }

    fn workspace() -> Workspace {
        Workspace {
            root: PathBuf::from("/ws"),
            checkouts: vec![Checkout {
                name: "swan-lake".into(),
                dir: PathBuf::from("/ws/swan-lake"),
                shared: None,
            }],
        }
    }

    #[test]
    fn a_workspace_line_is_its_least_advanced_root_with_a_step() {
        let ws = workspace();
        let landed = || row("a", &[], Stage::Landed, "-");
        let open = || row("b", &[], Stage::MrOpen { draft: false }, "wait for CI");
        let local = || row("c", &[], Stage::Local, "swl publish c");
        let pick = |rows: &[Row]| summary(&ws, rows).next;
        assert_eq!(pick(&[landed(), open()]), "wait for CI");
        // The other roots with a step at another stage are named.
        assert_eq!(
            pick(&[landed(), open(), local()]),
            "swl publish c (+ b: MR open)"
        );
        let merged = row("n", &[], Stage::Merged, "remove the workspace");
        let ci = |name| row(name, &[], Stage::Ci(CiState::Green), "swl land");
        assert_eq!(pick(&[ci("s"), merged]), "swl land (+ n: merged)");
        assert_eq!(pick(&[ci("s"), ci("t")]), "swl land");
        // Rows below a consumer count through the consumer's verdict only.
        let child = row("x@lib", &[("a", "lib")], Stage::Local, "swl publish a");
        assert_eq!(pick(&[child, landed(), open()]), "wait for CI");
        let unknown = row("d", &[], Stage::Unknown("x".into()), "-");
        assert_eq!(
            summary(&ws, &[local(), unknown]).stage,
            Stage::Unknown("x".into())
        );
        let no_change = row(
            "e",
            &[],
            Stage::NoChange("HEAD is in origin/master".into()),
            "-",
        );
        assert_eq!(summary(&ws, &[no_change, landed()]).stage, Stage::Landed);
        let empty = Workspace {
            root: PathBuf::from("/ws"),
            checkouts: Vec::new(),
        };
        assert_eq!(
            summary(&empty, &[]).stage,
            Stage::NoChange("no checkouts".into())
        );
    }
}

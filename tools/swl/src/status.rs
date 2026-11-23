//! `swl status`: local git state of the checkouts against the revs the
//! workspace's flakes lock. No forge calls.

use anyhow::Result;

use crate::{
    config::Config,
    git::{git, git_ok},
    lock::Lock,
    workspace::{Checkout, Workspace},
};

fn head_line(c: &Checkout) -> Result<String> {
    let head = git(&c.dir, &["rev-parse", "--short", "HEAD"])?;
    let branch = git_ok(&c.dir, &["symbolic-ref", "--quiet", "--short", "HEAD"])
        .unwrap_or_else(|| "detached".into());
    // git+file: sees tracked files only, so untracked ones do not count.
    let dirty = !git(&c.dir, &["status", "--porcelain", "--untracked-files=no"])?.is_empty();
    Ok(format!(
        "{head}  {branch}  {}",
        if dirty { "dirty" } else { "clean" }
    ))
}

fn compare(c: &Checkout, locked: &str) -> String {
    let short = &locked[..locked.len().min(7)];
    if git_ok(&c.dir, &["cat-file", "-e", &format!("{locked}^{{commit}}")]).is_none() {
        return format!("locked {short}, not in the local repo");
    }
    match git_ok(
        &c.dir,
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

pub fn run(ws: &Workspace, config: &Config) -> Result<()> {
    // (consumer of the hop, input path, checkout) for every hop in the
    // workspace, with the flake's own lock.
    let mut hops = Vec::new();
    for target in &ws.checkouts {
        let lock = Lock::read(&target.dir)?;
        for o in ws.overrides_for(config, target)? {
            let path: Vec<String> = o.input_path.split('/').map(str::to_string).collect();
            let rev = lock.as_ref().and_then(|l| l.locked_rev(&path));
            hops.push((target.name.as_str(), o.input_path, o.checkout, rev));
        }
    }
    for c in &ws.checkouts {
        let Some((_, input)) = &c.shared else {
            println!("{}  consumer  {}", c.name, head_line(c)?);
            continue;
        };
        println!("{}  input {input}  {}", c.name, head_line(c)?);
        for (target, input_path, _, rev) in hops.iter().filter(|h| h.2 == c.name) {
            let state = match rev {
                Some(rev) => compare(c, rev),
                None => "not locked".into(),
            };
            println!("  {target}: {input_path} {state}");
        }
    }
    Ok(())
}

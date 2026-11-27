//! The forge's open MRs across the configured repos, grouped by change:
//! by the ticket ID in their branch (`SL-NNNN`), else by branch, and their
//! listing's lines. `swl ls` lists those that no workspace holds.

use std::{cmp::Ordering, collections::BTreeMap};

use crate::{
    bump::{BUMP_BRANCH, COMMON_REPO},
    config::Config,
    forge::{PrInfo, PrReader},
    style::{BOLD, DIM, pad, paint, width},
    workspace_location::{ticket_in, ticket_spelling},
};

/// An open MR of a configured repo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenMr {
    pub repo: String,
    pub info: PrInfo,
}

impl OpenMr {
    /// `repo#n`.
    pub fn which(&self) -> String {
        format!("{}#{}", self.repo, self.info.number)
    }

    /// The branch of `swl bump`'s MRs.
    pub fn is_bump(&self) -> bool {
        self.info.head_ref == BUMP_BRANCH
    }
}

/// The open MRs of every configured repo, and the repos that could not be
/// read, with why.
#[derive(Debug, Default)]
pub struct Listing {
    pub mrs: Vec<OpenMr>,
    pub unread: Vec<(String, String)>,
}

/// Every open MR (drafts and every author's) of every repo of the config,
/// from the forge's list of open MRs. A repo that cannot be read is in
/// [`Listing::unread`], not an error.
pub fn list(config: &Config, reader: &dyn PrReader) -> Listing {
    let mut listing = Listing::default();
    for (repo, _) in config.repos() {
        match reader.open_prs(repo) {
            Ok(Some(prs)) => listing.mrs.extend(prs.into_iter().map(|info| OpenMr {
                repo: repo.to_string(),
                info,
            })),
            Ok(None) => listing
                .unread
                .push((repo.to_string(), reader.not_found().to_string())),
            Err(e) => listing.unread.push((repo.to_string(), format!("{e:#}"))),
        }
    }
    listing
}

/// What the MRs of one change share.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GroupKey {
    /// The ticket ID in their branches, normalised (`sl-94`).
    Ticket(String),
    /// Their branch, without a ticket ID in it.
    Branch(String),
}

impl GroupKey {
    pub fn of(mr: &OpenMr) -> Self {
        match ticket_in(&mr.info.head_ref) {
            Some(t) => GroupKey::Ticket(t),
            None => GroupKey::Branch(mr.info.head_ref.clone()),
        }
    }
}

/// Tickets first, by number; then branches, by name.
impl Ord for GroupKey {
    fn cmp(&self, other: &Self) -> Ordering {
        let number = |t: &str| {
            let n = t.rsplit('-').next().unwrap_or("").to_string();
            (n.len(), n)
        };
        match (self, other) {
            (GroupKey::Ticket(a), GroupKey::Ticket(b)) => number(a).cmp(&number(b)),
            (GroupKey::Ticket(_), GroupKey::Branch(_)) => Ordering::Less,
            (GroupKey::Branch(_), GroupKey::Ticket(_)) => Ordering::Greater,
            (GroupKey::Branch(a), GroupKey::Branch(b)) => a.cmp(b),
        }
    }
}

impl PartialOrd for GroupKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// The open MRs of one change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    pub key: GroupKey,
    /// By repo, then number.
    pub mrs: Vec<OpenMr>,
}

impl Group {
    /// The ticket ID as the first MR's branch spells it, upper-cased
    /// (`SL-0094`), or the branch.
    pub fn label(&self) -> String {
        match &self.key {
            GroupKey::Branch(b) => b.clone(),
            GroupKey::Ticket(t) => self
                .mrs
                .first()
                .and_then(|mr| ticket_spelling(&mr.info.head_ref))
                .unwrap_or_else(|| t.to_uppercase()),
        }
    }

    /// The MR to join the change from with `swl start --from`: the first of
    /// a repo in `consumers`, else common's (a change of common alone);
    /// with the other consumer MRs of the group.
    pub fn start_from<'a>(&'a self, consumers: &[String]) -> Option<(&'a OpenMr, Vec<&'a OpenMr>)> {
        let of_consumers: Vec<&OpenMr> = self
            .mrs
            .iter()
            .filter(|mr| consumers.contains(&mr.repo))
            .collect();
        if let Some((first, rest)) = of_consumers.split_first() {
            return Some((first, rest.to_vec()));
        }
        self.mrs
            .iter()
            .find(|mr| mr.repo == COMMON_REPO)
            .map(|mr| (mr, Vec::new()))
    }
}

/// The MRs grouped by [`GroupKey`], in its order.
pub fn groups(mrs: impl IntoIterator<Item = OpenMr>) -> Vec<Group> {
    let mut by_key: Vec<(GroupKey, Vec<OpenMr>)> = Vec::new();
    for mr in mrs {
        let key = GroupKey::of(&mr);
        match by_key.iter_mut().find(|(k, _)| *k == key) {
            Some((_, v)) => v.push(mr),
            None => by_key.push((key, vec![mr])),
        }
    }
    let mut out: Vec<Group> = by_key
        .into_iter()
        .map(|(key, mut mrs)| {
            mrs.sort_by(|a, b| (&a.repo, a.info.number).cmp(&(&b.repo, b.info.number)));
            Group { key, mrs }
        })
        .collect();
    out.sort_by(|a, b| a.key.cmp(&b.key));
    out
}

/// The open MRs that no workspace holds: no checkout of their repo is on
/// their branch, and no consumer's `swl.toml` in a workspace names them.
#[derive(Debug, Default)]
pub struct Held {
    /// The branches of the workspaces' checkouts, by repo.
    pub branches: BTreeMap<String, Vec<String>>,
    /// `repo#n` that the workspaces' `swl.toml` entries name.
    pub entries: Vec<String>,
}

impl Held {
    pub fn holds(&self, mr: &OpenMr) -> bool {
        self.branches
            .get(&mr.repo)
            .is_some_and(|bs| bs.contains(&mr.info.head_ref))
            || self.entries.contains(&mr.which())
    }
}

/// The `swl start --from` line of a group.
pub fn start_line(group: &Group, consumers: &Result<Vec<String>, String>) -> String {
    let consumers = match consumers {
        Ok(c) => c,
        Err(e) => return format!("unknown which repos are consumers ({e})"),
    };
    match group.start_from(consumers) {
        Some((from, _)) if from.repo == COMMON_REPO && !consumers.contains(&from.repo) => {
            format!(
                "swl start --from {} (only shared-repo MRs: common alone, as the consumer)",
                from.which()
            )
        }
        Some((from, others)) if others.is_empty() => {
            format!("swl start --from {}", from.which())
        }
        Some((from, others)) => format!(
            "swl start --from {} (also {})",
            from.which(),
            others
                .iter()
                .map(|m| m.which())
                .collect::<Vec<_>>()
                .join(", ")
        ),
        None => "only shared-repo MRs: no consumer MR to swl start --from".to_string(),
    }
}

/// The groups' lines: the group, then per MR `<repo>#<n>[ (bump)]  open
/// [(draft)]  <branch>  <title>`, then the `swl start --from` line.
pub fn lines(groups: &[Group], consumers: &Result<Vec<String>, String>) -> Vec<String> {
    let mrs: Vec<&OpenMr> = groups.iter().flat_map(|g| &g.mrs).collect();
    let which = |m: &OpenMr| {
        if m.is_bump() {
            format!("{} (bump)", m.which())
        } else {
            m.which()
        }
    };
    let state = |m: &OpenMr| if m.info.draft { "open (draft)" } else { "open" };
    let which_w = mrs.iter().map(|m| width(&which(m))).max().unwrap_or(0);
    let state_w = mrs.iter().map(|m| width(state(m))).max().unwrap_or(0);
    let branch_w = mrs
        .iter()
        .map(|m| width(&m.info.head_ref))
        .max()
        .unwrap_or(0);
    let mut out = Vec::new();
    for g in groups {
        out.push(format!("  {}", paint(BOLD, g.label())));
        for m in &g.mrs {
            let w = which(m);
            let s = state(m);
            let b = &m.info.head_ref;
            out.push(
                format!(
                    "    {w}{}  {s}{}  {b}{}  {}",
                    pad(which_w, width(&w)),
                    pad(state_w, width(s)),
                    pad(branch_w, width(b)),
                    m.info.title
                )
                .trim_end()
                .to_string(),
            );
        }
        out.push(paint(DIM, format!("    {}", start_line(g, consumers))));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::forge::PrState;

    fn mr(repo: &str, number: u64, branch: &str) -> OpenMr {
        OpenMr {
            repo: repo.to_string(),
            info: PrInfo {
                number,
                state: PrState::Open,
                head_sha: "0".repeat(40),
                head_ref: branch.to_string(),
                title: "t".to_string(),
                draft: false,
                base_ref: Some("master".to_string()),
                head_repo: None,
            },
        }
    }

    fn whiches(g: &Group) -> Vec<String> {
        g.mrs.iter().map(OpenMr::which).collect()
    }

    #[test]
    fn mrs_group_by_ticket_then_branch() {
        let gs = groups([
            mr("swan-lake", 3, "feature-x"),
            mr("swan-lake-common", 24, "SL-94-other"),
            mr("swan-lake", 12, "sl-0094-swl-ls"),
            mr("zwanenzang", 2, "feature-x"),
            mr("swan-lake", 13, "swl-bump-common"),
            mr("nixos-config", 4, "swl-bump-common"),
            mr("swan-lake", 20, "sl-102-x"),
        ]);
        let keys: Vec<String> = gs.iter().map(Group::label).collect();
        assert_eq!(
            keys,
            ["SL-0094", "SL-102", "feature-x", "swl-bump-common"],
            "{gs:?}"
        );
        assert_eq!(whiches(&gs[0]), ["swan-lake#12", "swan-lake-common#24"]);
        assert_eq!(whiches(&gs[2]), ["swan-lake#3", "zwanenzang#2"]);
        assert!(gs[3].mrs.iter().all(OpenMr::is_bump));
        assert!(!gs[2].mrs.iter().any(OpenMr::is_bump));
    }

    #[test]
    fn the_start_hint_is_a_consumer_mr_else_commons() {
        let consumers = ["nixos-config".to_string(), "swan-lake".to_string()];
        let g = &groups([
            mr("swan-lake-common", 24, "sl-94-x"),
            mr("swan-lake", 12, "sl-94-x"),
            mr("nixos-config", 4, "sl-94-y"),
        ])[0];
        let (from, others) = g.start_from(&consumers).unwrap();
        assert_eq!(from.which(), "nixos-config#4");
        assert_eq!(
            others.iter().map(|m| m.which()).collect::<Vec<_>>(),
            ["swan-lake#12"]
        );
        let common = &groups([mr("swan-lake-common", 24, "x"), mr("zwanenzang", 2, "x")])[0];
        assert_eq!(
            common.start_from(&consumers).unwrap().0.which(),
            "swan-lake-common#24"
        );
        let zw = &groups([mr("zwanenzang", 2, "x")])[0];
        assert!(zw.start_from(&consumers).is_none());
    }

    #[test]
    fn held_mrs_are_on_a_checked_out_branch_or_named_by_an_entry() {
        let mut held = Held::default();
        held.branches
            .entry("swan-lake".into())
            .or_default()
            .push("sl-0010-x".into());
        held.entries.push("swan-lake-common#7".into());
        assert!(held.holds(&mr("swan-lake", 4, "sl-0010-x")));
        assert!(!held.holds(&mr("swan-lake-common", 5, "sl-0010-x")));
        assert!(held.holds(&mr("swan-lake-common", 7, "other")));
        assert!(!held.holds(&mr("swan-lake", 8, "other")));
    }
}

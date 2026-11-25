//! The `flake.lock` changes `swl check` accepts on an MR, against the merge
//! base. Per root input whose locked graph changed, one of:
//!
//! - (i) a landing: the input is the root of a removed `swl.toml` entry (named
//!   at the merge base or a commit since, and not at HEAD) whose MR is merged;
//!   each such merge commit is in the rev now locked at its input path; and
//!   the input moved as in (ii), or is new and names the entry's repo;
//! - (ii) a forward move: the input's source is unchanged (its `original`,
//!   and its `locked` but for the rev and its metadata), and the new rev
//!   descends from the old one and is in the branch `flake.nix` declares
//!   (the default branch without a ref).
//!
//! Besides: no root input was removed, only a landing adds one, nothing else
//! in the lock changed (`lock_change::verify`), and the lock below the moved
//! inputs is exactly what nix locks for them at their new revisions (the
//! caller's `relock`). The pin policy of HEAD's lock is checked by the
//! caller.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result};
use serde_json::Value;

use crate::{
    ancestry::{History, Source, valid_github_name},
    forge::{PrReader, PrState},
    git::{git, git_ok, is_rev, short},
    lock_change,
    manifest::{Entry, MANIFEST_FILE, Manifest},
};

fn root_of(path: &str) -> &str {
    path.split('/').next().unwrap_or(path)
}

/// What the rules found: failures and informational lines.
#[derive(Debug, Default)]
pub struct Judgement {
    pub errors: Vec<String>,
    pub notes: Vec<String>,
}

/// The manifest at `rev`, `None` without one.
fn manifest_at(root: &std::path::Path, rev: &str) -> Result<Option<Manifest>> {
    let file = format!("{rev}:{MANIFEST_FILE}");
    if git_ok(root, &["cat-file", "-e", &file]).is_none() {
        return Ok(None);
    }
    Manifest::parse(&git(root, &["show", &file])?)
        .map(Some)
        .with_context(|| format!("{MANIFEST_FILE} at {}", short(rev)))
}

/// The entries named in `swl.toml` at the merge base `mb` or at any commit in
/// `mb..HEAD`, minus the input paths HEAD's manifest `head` still has. For an
/// entry named with different MRs over time, the latest. A manifest that does
/// not parse at some commit is skipped, with a note.
pub fn removed_entries(
    root: &std::path::Path,
    mb: &str,
    head: &Manifest,
) -> Result<(BTreeMap<String, Entry>, Vec<String>)> {
    let range = format!("{mb}..HEAD");
    let mut commits = vec![mb.to_string()];
    commits.extend(
        git(
            root,
            &[
                "rev-list",
                "--reverse",
                "--topo-order",
                "--full-history",
                &range,
                "--",
                MANIFEST_FILE,
            ],
        )?
        .lines()
        .map(str::to_string),
    );
    let mut named = BTreeMap::new();
    let mut notes = Vec::new();
    for c in &commits {
        match manifest_at(root, c) {
            Ok(Some(m)) => named.extend(m.inputs),
            Ok(None) => {}
            Err(e) => notes.push(format!("skipping {e:#}")),
        }
    }
    named.retain(|path, _| !head.inputs.contains_key(path));
    Ok((named, notes))
}

/// What `flake.nix` declares for a root input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Declaration {
    /// The ref; `None` for the default branch.
    pub branch: Option<String>,
    /// HEAD's declaration differs from the merge base's: also absent there,
    /// but for a new input.
    pub changed: bool,
}

/// The declaration of a root input in HEAD's `flake.nix`, for an input new
/// in the lock or not, or why it cannot be read.
pub type Declared<'a> = &'a dyn Fn(&str, bool) -> Result<Declaration, String>;

/// The lock nix makes from a reference lock with `(input, flake ref)`
/// overrides, or why it cannot.
pub type Relock<'a> = &'a dyn Fn(&Value, &[(String, String)]) -> Result<Value, String>;

pub struct Rules<'a> {
    pub old: &'a Value,
    pub new: &'a Value,
    /// The removed entries by input path.
    pub removed: &'a BTreeMap<String, Entry>,
    pub declared: Declared<'a>,
    pub api: &'a dyn PrReader,
    pub history: &'a dyn History,
    /// Whether a lock node's URLs name a repo (`Config::url_names_repo`).
    pub names_repo: &'a dyn Fn(&Value, &str) -> bool,
    pub relock: Relock<'a>,
}

/// The locked source and rev of the node at an input path.
fn locked_at(lock: &Value, path: &str) -> Result<(Source, String), String> {
    let key = lock_change::resolve_path(lock, path).map_err(|e| format!("{e:#}"))?;
    let locked = lock
        .pointer(&format!("/nodes/{key}/locked"))
        .ok_or_else(|| format!("{path} has no locked source"))?;
    let source = Source::of(locked).map_err(|e| format!("{path}: {e}"))?;
    let rev = locked
        .get("rev")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{path} has no locked rev"))?;
    if !is_rev(rev) {
        return Err(format!(
            "{path}: its locked rev {rev:?} is not a full revision"
        ));
    }
    Ok((source, rev.to_string()))
}

/// The lock node of a root input.
fn root_node<'v>(lock: &'v Value, name: &str) -> Option<&'v Value> {
    let key = lock_change::resolve_path(lock, name).ok()?;
    lock.pointer(&format!("/nodes/{key}"))
}

/// The `locked` attributes that may change when an input moves: the rev and
/// what nix derives from it.
const MOVING: [&str; 4] = ["rev", "narHash", "lastModified", "revCount"];

fn without_moving(locked: Option<&Value>) -> Value {
    let mut v = locked.cloned().unwrap_or(Value::Null);
    if let Some(m) = v.as_object_mut() {
        for k in MOVING {
            m.remove(k);
        }
    }
    v
}

/// A flake reference to the source `locked` names, at its rev: what nix
/// fetches to lock the input as HEAD's lock has it.
pub fn locked_ref(locked: &Value) -> Result<String, String> {
    let obj = locked.as_object().ok_or("no locked attributes")?;
    let s = |k: &str| obj.get(k).and_then(Value::as_str);
    let rev = s("rev").filter(|r| is_rev(r)).ok_or("no full locked rev")?;
    let encode = |v: &str| {
        v.bytes()
            .map(|b| {
                if b.is_ascii_alphanumeric() || b"._/-".contains(&b) {
                    (b as char).to_string()
                } else {
                    format!("%{b:02X}")
                }
            })
            .collect::<String>()
    };
    let (base, params): (String, &[&str]) = match s("type") {
        Some("github") => {
            let (Some(owner), Some(repo)) = (s("owner"), s("repo")) else {
                return Err("a GitHub source without owner or repo".into());
            };
            if !valid_github_name(owner) || !valid_github_name(repo) {
                return Err(format!(
                    "{owner:?}/{repo:?} is not a GitHub repository name"
                ));
            }
            (format!("github:{owner}/{repo}/{rev}"), &["host", "dir"])
        }
        Some("git") => {
            let url = s("url").ok_or("a git source without a url")?;
            if url.contains(['?', '#']) {
                return Err(format!("{url:?} has a query"));
            }
            (
                format!("git+{url}?rev={rev}"),
                &["ref", "dir", "submodules", "shallow"],
            )
        }
        _ => return Err("not a github or git source".into()),
    };
    let mut query = Vec::new();
    for (k, v) in obj {
        if k == "type"
            || MOVING.contains(&k.as_str())
            || ["owner", "repo", "url"].contains(&k.as_str())
        {
            continue;
        }
        if !params.contains(&k.as_str()) {
            return Err(format!("the locked attribute {k:?} cannot be reproduced"));
        }
        let v = match v {
            Value::String(v) => encode(v),
            Value::Bool(b) => (if *b { "1" } else { "0" }).to_string(),
            _ => {
                return Err(format!(
                    "the locked attribute {k:?} is not a string or a boolean"
                ));
            }
        };
        query.push(format!("{k}={v}"));
    }
    if query.is_empty() {
        return Ok(base);
    }
    let sep = if base.contains('?') { '&' } else { '?' };
    Ok(format!("{base}{sep}{}", query.join("&")))
}

impl Rules<'_> {
    pub fn judge(&self) -> Result<Judgement> {
        let mut out = Judgement::default();
        if self.old == self.new {
            out.errors.push(
                "flake.lock differs from the merge base only in its formatting: an MR must not \
                 change flake.lock except by a landing or forward moves"
                    .into(),
            );
            return Ok(out);
        }
        let changes = lock_change::root_changes(self.old, self.new)?;
        let moved: Vec<String> = changes
            .changed
            .iter()
            .chain(&changes.added)
            .cloned()
            .collect();
        if changes.all().is_empty() {
            out.errors.push(
                "flake.lock changes without moving a root input: an MR must not change \
                 flake.lock except by a landing or forward moves"
                    .into(),
            );
            return Ok(out);
        }
        for name in &changes.removed {
            out.errors.push(format!(
                "{name}: the root input was removed from flake.lock; an MR may only move inputs"
            ));
        }
        let landed = self.landed_entries(&moved, &mut out);
        let landing_roots: BTreeSet<&str> = landed.keys().map(|p| root_of(p)).collect();
        for name in &changes.added {
            if landing_roots.contains(name.as_str()) {
                self.added_landing(name, &landed, &mut out);
            } else {
                out.errors.push(format!(
                    "{name}: the root input was added to flake.lock without a landed {MANIFEST_FILE} \
                     entry; an MR may only move inputs"
                ));
            }
        }
        for name in &changes.changed {
            if !self.direct(name) {
                out.errors.push(format!(
                    "{name}: its follows changed in flake.lock; not a landing or a forward move"
                ));
            } else if self.same_source(name, &mut out) {
                if landing_roots.contains(name.as_str()) {
                    self.merged_in(name, &landed, &mut out);
                }
                self.forward(name, &mut out);
            }
        }
        if !out.errors.is_empty() {
            return Ok(out);
        }
        if let Err(e) = lock_change::verify(self.old, self.new, &moved) {
            out.errors.push(format!(
                "flake.lock changes more than the moved inputs {}: {e:#}",
                moved.join(", ")
            ));
            return Ok(out);
        }
        self.compare_relock(&moved, &mut out);
        Ok(out)
    }

    /// The removed entries whose root moved and whose MR is merged, with
    /// their merge commits. Others are notes: their root is a forward move.
    fn landed_entries(&self, moved: &[String], out: &mut Judgement) -> BTreeMap<String, String> {
        let mut landed = BTreeMap::new();
        for (path, e) in self.removed {
            let which = format!("{path}: {}#{}", e.repo, e.mr);
            if !moved.iter().any(|r| r == root_of(path)) {
                out.notes.push(format!(
                    "{path}: the {MANIFEST_FILE} entry {}#{} was removed; the input is unchanged",
                    e.repo, e.mr
                ));
                continue;
            }
            let state = match self.api.pr(&e.repo, e.mr) {
                Err(err) => {
                    out.errors
                        .push(format!("{which}: cannot read the MR state: {err:#}"));
                    continue;
                }
                Ok(None) => crate::forge::NOT_FOUND.to_string(),
                Ok(Some(pr)) => match pr.state {
                    PrState::Merged { merge_commit } if is_rev(&merge_commit) => {
                        landed.insert(path.clone(), merge_commit);
                        continue;
                    }
                    state => state.describe(),
                },
            };
            out.notes.push(format!(
                "{which} was removed from {MANIFEST_FILE} but is {state}: not a landing; {} must \
                 be a forward move",
                root_of(path)
            ));
        }
        landed
    }

    /// Whether the root input is a direct node in both locks (or new).
    fn direct(&self, name: &str) -> bool {
        let edge = |lock: &Value| {
            lock_change::root_inputs(lock)
                .ok()
                .and_then(|i| i.get(name).cloned())
        };
        [edge(self.old), edge(self.new)]
            .iter()
            .all(|e| e.as_ref().is_none_or(Value::is_string))
    }

    /// A moved input keeps its source: the same `original` and `flake`, and
    /// the same `locked` but for the rev and its metadata.
    fn same_source(&self, name: &str, out: &mut Judgement) -> bool {
        let (Some(old), Some(new)) = (root_node(self.old, name), root_node(self.new, name)) else {
            out.errors.push(format!("{name}: not a node in both locks"));
            return false;
        };
        let what = if old.get("original") != new.get("original") {
            "original"
        } else if old.get("flake") != new.get("flake") {
            "flake attribute"
        } else if without_moving(old.get("locked")) != without_moving(new.get("locked")) {
            "locked source (beyond its rev, narHash, lastModified and revCount)"
        } else {
            return true;
        };
        out.errors.push(format!(
            "{name}: its {what} changed in flake.lock; an MR may only move an input along its \
             source"
        ));
        false
    }

    /// The new rev of `root` is in the branch its `flake.nix` declaration
    /// names.
    fn declared_branch(
        &self,
        root: &str,
        new: bool,
        rev: &str,
        source: &Source,
        out: &mut Judgement,
    ) {
        let decl = match (self.declared)(root, new) {
            Ok(d) => d,
            Err(e) => {
                out.errors.push(format!(
                    "{root}: cannot read its flake.nix declaration: {e}"
                ));
                return;
            }
        };
        if decl.changed {
            out.errors.push(format!(
                "flake.nix changes the declaration of {root}; change the declaration and the lock \
                 in separate MRs"
            ));
            return;
        }
        let which = decl
            .branch
            .as_deref()
            .map_or_else(|| "the default branch".to_string(), str::to_string);
        match self.history.in_branch(source, rev, decl.branch.as_deref()) {
            Ok(true) => out
                .notes
                .push(format!("{root}: {} is in {which} of {source}", short(rev))),
            Ok(false) => out.errors.push(format!(
                "{root}: {} is not in {which} of {source}, which flake.nix declares; promote it \
                 first",
                short(rev)
            )),
            Err(e) => out.errors.push(format!(
                "{root}: cannot determine whether {} is in {which} of {source}: {e}",
                short(rev)
            )),
        }
    }

    /// Rule (i): each landed entry below `root` is in the rev now locked at
    /// its input path.
    fn merged_in(&self, root: &str, landed: &BTreeMap<String, String>, out: &mut Judgement) {
        for (path, merge_commit) in landed.iter().filter(|(p, _)| root_of(p) == root) {
            let e = &self.removed[path];
            let which = format!("{path}: {}#{}", e.repo, e.mr);
            let (source, rev) = match locked_at(self.new, path) {
                Ok(x) => x,
                Err(err) => {
                    out.errors.push(format!("{which}: {err}"));
                    continue;
                }
            };
            match self.history.contains(&source, merge_commit, &rev) {
                Ok(true) => out.notes.push(format!(
                    "{which} lands: merged as {}, in {} now locked",
                    short(merge_commit),
                    short(&rev)
                )),
                Ok(false) => out.errors.push(format!(
                    "{which}: its merge commit {} is not in {}, which flake.lock now locks there",
                    short(merge_commit),
                    short(&rev)
                )),
                Err(err) => out.errors.push(format!(
                    "{which}: cannot determine whether its merge commit {} is in {}: {err}",
                    short(merge_commit),
                    short(&rev)
                )),
            }
        }
    }

    /// Rule (i) for a new root input: its entry names its repo, and it is in
    /// its declared branch.
    fn added_landing(&self, root: &str, landed: &BTreeMap<String, String>, out: &mut Judgement) {
        let node = root_node(self.new, root);
        match (landed.contains_key(root), node) {
            (true, Some(node)) if (self.names_repo)(node, &self.removed[root].repo) => {}
            (true, _) => {
                out.errors.push(format!(
                    "{root}: the new input is not the repo {:?} its {MANIFEST_FILE} entry names",
                    self.removed[root].repo
                ));
                return;
            }
            (false, _) => {
                out.errors.push(format!(
                    "{root}: the root input was added without a landed {MANIFEST_FILE} entry for \
                     it"
                ));
                return;
            }
        }
        self.merged_in(root, landed, out);
        match locked_at(self.new, root) {
            Ok((source, rev)) => self.declared_branch(root, true, &rev, &source, out),
            Err(e) => out.errors.push(format!("{root}: {e}")),
        }
    }

    /// Rule (ii) for the root input `root`, and the forward part of (i).
    fn forward(&self, root: &str, out: &mut Judgement) {
        let (source, new_rev) = match locked_at(self.new, root) {
            Ok(x) => x,
            Err(e) => {
                out.errors.push(format!("{root}: {e}"));
                return;
            }
        };
        let old_rev = match locked_at(self.old, root) {
            Ok((_, r)) => r,
            Err(e) => {
                out.errors
                    .push(format!("{root}: in the merge base's lock, {e}"));
                return;
            }
        };
        let moved = format!("{} -> {}", short(&old_rev), short(&new_rev));
        if old_rev != new_rev {
            match self.history.contains(&source, &old_rev, &new_rev) {
                Ok(true) => out.notes.push(format!("{root}: forward move {moved}")),
                Ok(false) => {
                    out.errors.push(format!(
                        "{root}: {moved} is not a forward move: the new rev does not descend \
                         from the old one"
                    ));
                    return;
                }
                Err(e) => {
                    out.errors.push(format!(
                        "{root}: cannot determine whether {moved} is a forward move: {e}"
                    ));
                    return;
                }
            }
        }
        self.declared_branch(root, false, &new_rev, &source, out);
    }

    /// The lock below the moved inputs is what nix locks for them at their
    /// new revisions: the merge base's lock without them, relocked with each
    /// overridden to its new locked source, equals HEAD's.
    fn compare_relock(&self, moved: &[String], out: &mut Judgement) {
        let mut reference = self.old.clone();
        let root = self.old["root"].as_str().unwrap_or("root").to_string();
        if let Some(inputs) = reference["nodes"][&root]["inputs"].as_object_mut() {
            for name in moved {
                inputs.remove(name);
            }
        }
        let mut overrides = Vec::new();
        for name in moved {
            let locked = root_node(self.new, name).and_then(|n| n.get("locked"));
            match locked.map(locked_ref) {
                Some(Ok(r)) => overrides.push((name.clone(), r)),
                Some(Err(e)) => {
                    out.errors.push(format!("{name}: cannot relock it: {e}"));
                    return;
                }
                None => {
                    out.errors.push(format!("{name}: no locked source"));
                    return;
                }
            }
        }
        let names = moved.join(", ");
        match (self.relock)(&reference, &overrides) {
            Err(e) => out.errors.push(format!(
                "cannot relock {names} to compare with flake.lock: {e}"
            )),
            Ok(relocked) => match lock_change::first_difference(&relocked, self.new) {
                Err(e) => out.errors.push(format!("{e:#}")),
                Ok(Some(at)) => out.errors.push(format!(
                    "flake.lock is not what nix locks for {names} at the new revisions: it \
                     differs at {at}"
                )),
                Ok(None) => out.notes.push(format!(
                    "below {names}, flake.lock is what nix locks at the new revisions"
                )),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use serde_json::json;

    use super::*;
    use crate::forge::{CiState, PrInfo};

    const HASH: &str = "sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";

    fn rev(c: char) -> String {
        c.to_string().repeat(40)
    }

    /// common (github, with a nested zwanenzang on git) and nixpkgs.
    fn lock() -> Value {
        json!({"version": 7, "root": "root", "nodes": {
            "root": {"inputs": {"common": "common", "nixpkgs": "nixpkgs", "zw": ["common", "zwanenzang"]}},
            "common": {"locked": {"type": "github", "owner": "binarin", "repo": "swan-lake-common",
                                  "rev": rev('a'), "narHash": HASH},
                       "original": {"type": "github", "owner": "binarin", "repo": "swan-lake-common", "ref": "swan-lake-current"},
                       "inputs": {"zwanenzang": "zwanenzang"}},
            "zwanenzang": {"locked": {"type": "git", "url": "https://forgejo.example/binarin/zwanenzang",
                                      "rev": rev('d'), "narHash": HASH},
                           "original": {"type": "git", "url": "https://forgejo.example/binarin/zwanenzang"}},
            "nixpkgs": {"locked": {"type": "github", "owner": "NixOS", "repo": "nixpkgs",
                                   "rev": rev('1'), "narHash": HASH},
                        "original": {"type": "github", "owner": "NixOS", "repo": "nixpkgs", "ref": "nixos-unstable"}},
        }})
    }

    fn set_rev(lock: &mut Value, node: &str, r: char) {
        lock["nodes"][node]["locked"]["rev"] = json!(rev(r));
    }

    /// Ancestry by explicit `(ancestor, descendant)` pairs; branch tips by
    /// `(source, branch)`. Anything else cannot be determined.
    #[derive(Default)]
    struct FakeHistory {
        pairs: Vec<(String, String)>,
        tips: BTreeMap<(String, String), String>,
        asked: RefCell<Vec<String>>,
    }

    impl FakeHistory {
        fn known(&self, a: &str, d: &str) -> Result<bool, String> {
            if a == d || self.pairs.iter().any(|(x, y)| x == a && y == d) {
                return Ok(true);
            }
            let revs: BTreeSet<&str> = self
                .pairs
                .iter()
                .flat_map(|(x, y)| [x.as_str(), y.as_str()])
                .collect();
            if revs.contains(a) && revs.contains(d) {
                Ok(false)
            } else {
                Err("offline".into())
            }
        }
    }

    impl History for FakeHistory {
        fn contains(&self, source: &Source, a: &str, d: &str) -> Result<bool, String> {
            self.asked
                .borrow_mut()
                .push(format!("contains {source} {} {}", short(a), short(d)));
            self.known(a, d)
        }

        fn in_branch(
            &self,
            source: &Source,
            r: &str,
            branch: Option<&str>,
        ) -> Result<bool, String> {
            let branch = branch.unwrap_or("HEAD").to_string();
            self.asked
                .borrow_mut()
                .push(format!("in_branch {source} {} {branch}", short(r)));
            let tip = self
                .tips
                .get(&(source.to_string(), branch))
                .ok_or("rate limited")?;
            self.known(r, tip)
        }
    }

    struct Api(BTreeMap<u64, PrState>);

    impl PrReader for Api {
        fn pr(&self, _: &str, n: u64) -> Result<Option<PrInfo>> {
            Ok(self.0.get(&n).map(|state| PrInfo {
                number: n,
                state: state.clone(),
                head_sha: "h".into(),
                head_ref: "b".into(),
                title: String::new(),
                draft: false,
            }))
        }

        fn commit_status(&self, _: &str, _: &str) -> Result<Option<CiState>> {
            Ok(None)
        }
    }

    fn merged(c: char) -> PrState {
        PrState::Merged {
            merge_commit: rev(c),
        }
    }

    fn entry(repo: &str, mr: u64) -> Entry {
        Entry {
            repo: repo.into(),
            mr,
        }
    }

    fn history() -> FakeHistory {
        let mut h = FakeHistory::default();
        // common: a -> c (the merge of #24) -> b; current is at b. nixpkgs
        // 1 -> 2, unstable at 2; 3 diverged. zwanenzang d -> e.
        for (a, d) in [('a', 'c'), ('c', 'b'), ('a', 'b'), ('1', '2'), ('d', 'e')] {
            h.pairs.push((rev(a), rev(d)));
        }
        h.pairs.push((rev('3'), rev('3')));
        h.tips.insert(
            (
                "github:binarin/swan-lake-common".into(),
                "swan-lake-current".into(),
            ),
            rev('b'),
        );
        h.tips.insert(
            ("github:NixOS/nixpkgs".into(), "nixos-unstable".into()),
            rev('2'),
        );
        h
    }

    fn declared(name: &str, _new: bool) -> Result<Declaration, String> {
        let branch = match name {
            "common" | "common-next" => "swan-lake-current",
            "nixpkgs" => "nixos-unstable",
            _ => return Err(format!("{name} is not declared")),
        };
        Ok(Declaration {
            branch: Some(branch.into()),
            changed: false,
        })
    }

    /// Like `check`'s: both `original` and `locked` name the repo.
    fn names_repo(node: &Value, repo: &str) -> bool {
        ["/original/repo", "/locked/repo"]
            .iter()
            .all(|p| node.pointer(p).and_then(Value::as_str) == Some(repo))
    }

    /// The rules with `relocked` as what nix locks (HEAD's lock when
    /// `None`: a faithful lock), and what relock was asked.
    struct Case<'a> {
        old: &'a Value,
        new: &'a Value,
        removed: Vec<(&'a str, Entry)>,
        api: Vec<(u64, PrState)>,
        relocked: Option<Value>,
    }

    /// The reference locks and overrides relock was asked for.
    type Asked = Vec<(Value, Vec<(String, String)>)>;

    fn run(c: Case, h: &FakeHistory) -> (Judgement, Asked) {
        let removed: BTreeMap<String, Entry> = c
            .removed
            .iter()
            .map(|(p, e)| (p.to_string(), e.clone()))
            .collect();
        let api = Api(c.api.into_iter().collect());
        let asked = RefCell::new(Vec::new());
        let relock = |reference: &Value, overrides: &[(String, String)]| {
            asked
                .borrow_mut()
                .push((reference.clone(), overrides.to_vec()));
            Ok(c.relocked.clone().unwrap_or_else(|| c.new.clone()))
        };
        let j = Rules {
            old: c.old,
            new: c.new,
            removed: &removed,
            declared: &declared,
            api: &api,
            history: h,
            names_repo: &names_repo,
            relock: &relock,
        }
        .judge()
        .unwrap();
        (j, asked.into_inner())
    }

    fn judge(
        old: &Value,
        new: &Value,
        removed: &[(&str, Entry)],
        api: &[(u64, PrState)],
        h: &FakeHistory,
    ) -> Judgement {
        run(
            Case {
                old,
                new,
                removed: removed.to_vec(),
                api: api.to_vec(),
                relocked: None,
            },
            h,
        )
        .0
    }

    fn ok(j: &Judgement) {
        assert!(j.errors.is_empty(), "{:?}", j.errors);
    }

    fn fails(j: &Judgement, needle: &str) {
        assert!(
            j.errors.iter().any(|e| e.contains(needle)),
            "want {needle:?} in {:?}",
            j.errors
        );
    }

    #[test]
    fn a_landing_moves_the_removed_entries_roots() {
        let old = lock();
        let mut new = lock();
        set_rev(&mut new, "common", 'b');
        let h = history();
        let removed = [("common", entry("swan-lake-common", 24))];
        let j = judge(&old, &new, &removed, &[(24, merged('c'))], &h);
        ok(&j);
        assert!(
            j.notes
                .iter()
                .any(|n| n.contains("swan-lake-common#24 lands")),
            "{:?}",
            j.notes
        );
        // Merged elsewhere, or not promoted.
        let j = judge(&old, &new, &removed, &[(24, merged('3'))], &h);
        fails(&j, "its merge commit 333333333333 is not in bbbbbbbbbbbb");
        let mut unpromoted = lock();
        set_rev(&mut unpromoted, "common", 'c');
        let mut h2 = history();
        h2.tips.insert(
            (
                "github:binarin/swan-lake-common".into(),
                "swan-lake-current".into(),
            ),
            rev('a'),
        );
        let j = judge(&old, &unpromoted, &removed, &[(24, merged('c'))], &h2);
        fails(
            &j,
            "is not in swan-lake-current of github:binarin/swan-lake-common",
        );
    }

    #[test]
    fn a_landing_must_move_forward() {
        // The merge base is past the merge commit: going back to it is not a
        // landing, even with the entry named somewhere in the branch.
        let mut old = lock();
        set_rev(&mut old, "common", 'b');
        let mut new = lock();
        set_rev(&mut new, "common", 'c');
        let removed = [("common", entry("swan-lake-common", 24))];
        let j = judge(&old, &new, &removed, &[(24, merged('c'))], &history());
        fails(&j, "bbbbbbbbbbbb -> cccccccccccc is not a forward move");
    }

    #[test]
    fn an_abandoned_entry_leaves_its_root_to_a_forward_move() {
        let old = lock();
        let mut new = lock();
        set_rev(&mut new, "common", 'b');
        let h = history();
        for state in [
            vec![(24, PrState::Open)],
            vec![(24, PrState::ClosedUnmerged)],
            vec![],
        ] {
            let removed = [("common", entry("swan-lake-common", 24))];
            let j = judge(&old, &new, &removed, &state, &h);
            ok(&j);
            assert!(
                j.notes
                    .iter()
                    .any(|n| n.contains("not a landing; common must be a forward move")),
                "{:?}",
                j.notes
            );
        }
        // Backwards is still refused.
        let removed = [("common", entry("swan-lake-common", 24))];
        let j = judge(&new, &old, &removed, &[(24, PrState::Open)], &h);
        fails(&j, "is not a forward move");
    }

    #[test]
    fn a_nested_entry_resolves_through_the_parents_new_lock() {
        let old = lock();
        let mut new = lock();
        set_rev(&mut new, "common", 'b');
        set_rev(&mut new, "zwanenzang", 'e');
        let h = history();
        let removed = [("common/zwanenzang", entry("zwanenzang", 7))];
        let j = judge(&old, &new, &removed, &[(7, merged('e'))], &h);
        ok(&j);
        assert!(
            h.asked.borrow().contains(&format!(
                "contains https://forgejo.example/binarin/zwanenzang {} {}",
                short(&rev('e')),
                short(&rev('e'))
            )),
            "{:?}",
            h.asked.borrow()
        );
        let j = judge(&old, &new, &removed, &[(7, merged('3'))], &h);
        fails(&j, "common/zwanenzang: zwanenzang#7: its merge commit");
    }

    #[test]
    fn landing_and_forward_moves_mix_per_root() {
        let old = lock();
        let mut new = lock();
        set_rev(&mut new, "common", 'b');
        set_rev(&mut new, "nixpkgs", '2');
        let h = history();
        let removed = [("common", entry("swan-lake-common", 24))];
        let j = judge(&old, &new, &removed, &[(24, merged('c'))], &h);
        ok(&j);
        assert!(
            j.notes
                .iter()
                .any(|n| n.starts_with("nixpkgs: forward move")),
            "{:?}",
            j.notes
        );
        // Without the removed entry, common is a forward move too.
        ok(&judge(&old, &new, &[], &[], &h));
    }

    #[test]
    fn forward_moves_only() {
        let old = lock();
        let h = history();
        // Backwards and diverged are refused.
        let mut back = lock();
        set_rev(&mut back, "common", 'b');
        let j = judge(&back, &old, &[], &[], &h);
        fails(
            &j,
            "common: bbbbbbbbbbbb -> aaaaaaaaaaaa is not a forward move",
        );
        let mut diverged = lock();
        set_rev(&mut diverged, "nixpkgs", '3');
        let j = judge(&old, &diverged, &[], &[], &h);
        fails(&j, "nixpkgs: 111111111111 -> 333333333333");
        // Forward, but not in the declared branch.
        let mut h2 = history();
        h2.tips.insert(
            ("github:NixOS/nixpkgs".into(), "nixos-unstable".into()),
            rev('1'),
        );
        let mut fwd = lock();
        set_rev(&mut fwd, "nixpkgs", '2');
        fails(
            &judge(&old, &fwd, &[], &[], &h2),
            "is not in nixos-unstable of github:NixOS/nixpkgs",
        );
    }

    /// Review repro: a forward rev with another source must not pass.
    #[test]
    fn a_moved_input_keeps_its_source() {
        let old = lock();
        let h = history();
        let moved = || {
            let mut l = lock();
            set_rev(&mut l, "nixpkgs", '2');
            l
        };
        let mut host = moved();
        host["nodes"]["nixpkgs"]["locked"]["host"] = json!("evil.example");
        fails(
            &judge(&old, &host, &[], &[], &h),
            "nixpkgs: its locked source",
        );
        let mut fork = moved();
        fork["nodes"]["nixpkgs"]["locked"]["owner"] = json!("evil");
        fails(
            &judge(&old, &fork, &[], &[], &h),
            "nixpkgs: its locked source",
        );
        let mut original = moved();
        original["nodes"]["nixpkgs"]["original"]["ref"] = json!("nixos-24.05");
        fails(
            &judge(&old, &original, &[], &[], &h),
            "nixpkgs: its original changed",
        );
        let mut not_flake = moved();
        not_flake["nodes"]["nixpkgs"]["flake"] = json!(false);
        fails(
            &judge(&old, &not_flake, &[], &[], &h),
            "its flake attribute",
        );
        // The metadata nix writes for a rev may change.
        let mut meta = moved();
        meta["nodes"]["nixpkgs"]["locked"]["lastModified"] = json!(1);
        meta["nodes"]["nixpkgs"]["locked"]["narHash"] = json!("sha256-B");
        ok(&judge(&old, &meta, &[], &[], &h));
        // A rev that is not a full revision is refused before any fetch.
        let mut short_rev = lock();
        short_rev["nodes"]["nixpkgs"]["locked"]["rev"] = json!("--upload-pack=x");
        fails(
            &judge(&old, &short_rev, &[], &[], &h),
            "is not a full revision",
        );
    }

    /// Review repro: churn below a forward-moved input must not pass.
    #[test]
    fn the_lock_below_a_moved_input_is_what_nix_locks() {
        let old = lock();
        let mut legit = lock();
        set_rev(&mut legit, "common", 'b');
        set_rev(&mut legit, "zwanenzang", 'e');
        let mut tampered = legit.clone();
        tampered["nodes"]["zwanenzang"]["locked"]["url"] = json!("https://evil.example/x");
        set_rev(&mut tampered, "zwanenzang", '9');
        let h = history();
        let case = |new| Case {
            old: &old,
            new,
            removed: vec![],
            api: vec![],
            relocked: Some(legit.clone()),
        };
        let (j, asked) = run(case(&tampered), &h);
        fails(
            &j,
            "flake.lock is not what nix locks for common at the new revisions: it differs at \
             common/zwanenzang (its locked)",
        );
        // nix was asked to lock the merge base's lock without common, with
        // common at its new rev.
        let (reference, overrides) = &asked[0];
        assert!(reference.pointer("/nodes/root/inputs/common").is_none());
        assert_eq!(
            reference.pointer("/nodes/root/inputs/nixpkgs"),
            Some(&json!("nixpkgs"))
        );
        assert_eq!(
            overrides,
            &[(
                "common".to_string(),
                format!("github:binarin/swan-lake-common/{}", rev('b'))
            )]
        );
        // A narHash under common, as in the owner's repro.
        let mut hash = legit.clone();
        hash["nodes"]["zwanenzang"]["locked"]["narHash"] = json!("sha256-B");
        fails(&run(case(&hash), &h).0, "differs at common/zwanenzang");
        let (j, _) = run(case(&legit), &h);
        ok(&j);
        assert!(
            j.notes.iter().any(|n| n.contains("what nix locks")),
            "{:?}",
            j.notes
        );
        // No relock when an earlier rule already failed.
        let mut back = lock();
        set_rev(&mut back, "common", 'b');
        let (_, asked) = run(
            Case {
                old: &back,
                new: &old,
                removed: vec![],
                api: vec![],
                relocked: None,
            },
            &h,
        );
        assert!(asked.is_empty());
    }

    #[test]
    fn a_moved_input_keeps_its_declaration() {
        let old = lock();
        let mut new = lock();
        set_rev(&mut new, "nixpkgs", '2');
        let changed = |name: &str, new: bool| {
            declared(name, new).map(|d| Declaration {
                changed: name == "nixpkgs",
                ..d
            })
        };
        let api = Api(BTreeMap::new());
        let relock = |_: &Value, _: &[(String, String)]| Ok(new.clone());
        let j = Rules {
            old: &old,
            new: &new,
            removed: &BTreeMap::new(),
            declared: &changed,
            api: &api,
            history: &history(),
            names_repo: &names_repo,
            relock: &relock,
        }
        .judge()
        .unwrap();
        fails(
            &j,
            "flake.nix changes the declaration of nixpkgs; change the declaration and the lock in \
             separate MRs",
        );
    }

    /// Only an input new in the lock is asked for as new: a moved one that
    /// the merge base did not declare must count as changed.
    #[test]
    fn declarations_are_asked_for_as_new_only_for_added_inputs() {
        let old = lock();
        let mut new = lock();
        set_rev(&mut new, "nixpkgs", '2');
        new["nodes"]["root"]["inputs"]["common-next"] = json!("next");
        new["nodes"]["next"] = new["nodes"]["common"].clone();
        new["nodes"]["next"]["locked"]["rev"] = json!(rev('b'));
        let asked = RefCell::new(Vec::new());
        let declared = |name: &str, is_new: bool| {
            asked.borrow_mut().push(format!("{name} {is_new}"));
            declared(name, is_new)
        };
        let removed: BTreeMap<String, Entry> =
            [("common-next".to_string(), entry("swan-lake-common", 24))].into();
        let api = Api([(24, merged('c'))].into());
        let relock = |_: &Value, _: &[(String, String)]| Ok(new.clone());
        let j = Rules {
            old: &old,
            new: &new,
            removed: &removed,
            declared: &declared,
            api: &api,
            history: &history(),
            names_repo: &names_repo,
            relock: &relock,
        }
        .judge()
        .unwrap();
        ok(&j);
        assert_eq!(*asked.borrow(), ["common-next true", "nixpkgs false"]);
    }

    #[test]
    fn cannot_determine_is_a_failure() {
        let old = lock();
        let mut new = lock();
        set_rev(&mut new, "nixpkgs", '9');
        let j = judge(&old, &new, &[], &[], &history());
        fails(
            &j,
            "cannot determine whether 111111111111 -> 999999999999 is a forward move: offline",
        );
        let mut new = lock();
        set_rev(&mut new, "nixpkgs", '2');
        let mut h = history();
        h.tips.clear();
        fails(
            &judge(&old, &new, &[], &[], &h),
            "cannot determine whether 222222222222 is in nixos-unstable",
        );
    }

    #[test]
    fn inputs_added_or_removed_are_refused() {
        let old = lock();
        let h = history();
        let mut added = lock();
        added["nodes"]["root"]["inputs"]["extra"] = json!("nixpkgs");
        fails(
            &judge(&old, &added, &[], &[], &h),
            "extra: the root input was added",
        );
        let mut removed = lock();
        removed["nodes"]["root"]["inputs"]
            .as_object_mut()
            .unwrap()
            .remove("nixpkgs");
        removed["nodes"].as_object_mut().unwrap().remove("nixpkgs");
        fails(
            &judge(&old, &removed, &[], &[], &h),
            "nixpkgs: the root input was removed",
        );
        // A landing may add the input its entry names, of that repo.
        let mut landed = lock();
        landed["nodes"]["root"]["inputs"]["common-next"] = json!("next");
        landed["nodes"]["next"] = landed["nodes"]["common"].clone();
        landed["nodes"]["next"]["locked"]["rev"] = json!(rev('b'));
        let removed = [("common-next", entry("swan-lake-common", 24))];
        ok(&judge(&old, &landed, &removed, &[(24, merged('c'))], &h));
        // Locked elsewhere, though its original is the repo.
        let mut elsewhere = landed.clone();
        elsewhere["nodes"]["next"]["locked"]["repo"] = json!("evil");
        fails(
            &judge(&old, &elsewhere, &removed, &[(24, merged('c'))], &h),
            "common-next: the new input is not the repo",
        );
        let other = [("common-next", entry("zwanenzang", 24))];
        fails(
            &judge(&old, &landed, &other, &[(24, merged('c'))], &h),
            "common-next: the new input is not the repo \"zwanenzang\"",
        );
        fails(
            &judge(&old, &landed, &removed, &[(24, PrState::Open)], &h),
            "common-next: the root input was added",
        );
    }

    #[test]
    fn other_churn_and_formatting_are_refused() {
        let old = lock();
        let h = history();
        fails(&judge(&old, &old, &[], &[], &h), "only in its formatting");
        let mut stray = lock();
        stray["nodes"]["stray"] = json!({"locked": {"rev": "s"}});
        fails(
            &judge(&old, &stray, &[], &[], &h),
            "without moving a root input",
        );
        // A follows path that changes is neither rule.
        let mut follows = lock();
        follows["nodes"]["root"]["inputs"]["zw"] = json!(["nixpkgs"]);
        fails(
            &judge(&old, &follows, &[], &[], &h),
            "zw: its follows changed",
        );
        // A shared node moved under common, which keeps its rev.
        let mut shared = lock();
        set_rev(&mut shared, "zwanenzang", 'e');
        fails(&judge(&old, &shared, &[], &[], &h), "same revision changed");
    }

    #[test]
    fn a_removed_entry_with_an_unchanged_input_is_a_note() {
        let old = lock();
        let mut new = lock();
        set_rev(&mut new, "nixpkgs", '2');
        let removed = [("common", entry("swan-lake-common", 24))];
        let j = judge(&old, &new, &removed, &[], &history());
        ok(&j);
        assert!(
            j.notes.iter().any(|n| n.contains("the input is unchanged")),
            "{:?}",
            j.notes
        );
    }

    #[test]
    fn locked_refs_reproduce_the_source() {
        let r = rev('a');
        let gh = json!({"type": "github", "owner": "NixOS", "repo": "nixpkgs", "rev": r,
                        "narHash": HASH, "lastModified": 1});
        assert_eq!(
            locked_ref(&gh).unwrap(),
            format!("github:NixOS/nixpkgs/{r}")
        );
        let git = json!({"type": "git", "url": "https://f.example/binarin/zw", "ref": "current",
                         "rev": r, "revCount": 3, "narHash": HASH, "lastModified": 1});
        assert_eq!(
            locked_ref(&git).unwrap(),
            format!("git+https://f.example/binarin/zw?rev={r}&ref=current")
        );
        let mut sub = git.clone();
        sub["submodules"] = json!(true);
        sub["ref"] = json!("a&b");
        assert_eq!(
            locked_ref(&sub).unwrap(),
            format!("git+https://f.example/binarin/zw?rev={r}&ref=a%26b&submodules=1")
        );
        for bad in [
            json!({"type": "github", "owner": "..", "repo": "x", "rev": r}),
            json!({"type": "github", "owner": "o", "repo": "x", "rev": "abc"}),
            json!({"type": "github", "owner": "o", "repo": "x", "rev": r, "extra": "1"}),
            json!({"type": "git", "url": "https://f/x?ref=y", "rev": r}),
            json!({"type": "tarball", "url": "https://f/x.tar.gz", "rev": r}),
        ] {
            assert!(locked_ref(&bad).is_err(), "{bad}");
        }
    }
}

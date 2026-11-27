//! The common pin policy for master, ported from `check-common-pin`
//! (`tools/common-workflow/workflow.py`): `common` is strictly the canonical
//! promoted `swan-lake-current`; each experiment input `common-next` or
//! `common-next-<label>` may be at any ref or rev of swan-lake-common, and is
//! reported as based on the locked `common` or not.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Result, bail};
use serde_json::{Value, json};

use crate::{ancestry, git::is_rev};

pub const CANONICAL: &str = "github:binarin/swan-lake-common/swan-lake-current";
/// A temporary Forgejo branch pin, as the SL-34 workflow used.
const TEMP_PREFIX: &str = "git+https://forgejo.lynx-lizard.ts.net/binarin/swan-lake-common?ref=";
const FORGEJO_URL: &str = "https://forgejo.lynx-lizard.ts.net/binarin/swan-lake-common";
const GITHUB_URL: &str = "https://github.com/binarin/swan-lake-common";

fn canonical_original() -> Value {
    json!({"type": "github", "owner": "binarin", "repo": "swan-lake-common", "ref": "swan-lake-current"})
}

/// `common-next` or `common-next-<label>`.
pub fn is_next_name(name: &str) -> bool {
    name == "common-next"
        || name
            .strip_prefix("common-next-")
            .is_some_and(|l| !l.is_empty())
}

/// What a passing check found: the locked common revision and the
/// experiment inputs' locked sources.
#[derive(Debug)]
pub struct Pins {
    pub common_rev: String,
    pub next: Vec<(String, Value)>,
}

fn is_nar_hash(s: &str) -> bool {
    s.strip_prefix("sha256-")
        .and_then(|h| h.strip_suffix('='))
        .is_some_and(|h| {
            h.len() == 43
                && h.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/')
        })
}

fn str_of<'a>(v: &'a Value, k: &str) -> Option<&'a str> {
    v.get(k).and_then(Value::as_str)
}

fn keys(v: &Value) -> BTreeSet<&str> {
    v.as_object()
        .map(|m| m.keys().map(String::as_str).collect())
        .unwrap_or_default()
}

fn root_inputs(lock: &Value) -> Option<&serde_json::Map<String, Value>> {
    let root = lock.get("root")?.as_str()?;
    lock.pointer(&format!("/nodes/{root}/inputs"))?.as_object()
}

/// The node of a direct root input.
fn direct_node<'a>(lock: &'a Value, name: &str) -> Result<&'a Value> {
    let Some(edge) = root_inputs(lock).and_then(|i| i.get(name)) else {
        bail!("{name} is not in flake.lock; run nix flake lock");
    };
    let Some(key) = edge.as_str() else {
        bail!("{name} must be a direct input, not follows");
    };
    match lock.pointer("/nodes").and_then(|n| n.get(key)) {
        Some(node) => Ok(node),
        None => bail!("flake.lock has no node {key:?} for {name}"),
    }
}

fn require_flake(name: &str, node: &Value) -> Result<()> {
    if node.get("flake").is_some_and(|f| f != &Value::Bool(true)) {
        bail!("{name} must be a flake");
    }
    Ok(())
}

/// Checks the pins of the flake whose `flake.nix` declares `decls` and whose
/// lock is `lock`. `parse` gives the attribute form of a flake reference
/// (`builtins.parseFlakeRef`). `Ok(None)` when the flake has neither a
/// `common` input nor an experiment input: the policy does not apply.
pub fn check(
    decls: &BTreeMap<String, Value>,
    lock: &Value,
    parse: &dyn Fn(&str) -> Result<Value>,
) -> Result<Option<Pins>> {
    let locked_names: BTreeSet<&str> = root_inputs(lock)
        .map(|i| i.keys().map(String::as_str).collect())
        .unwrap_or_default();
    let next_names: BTreeSet<&str> = decls
        .keys()
        .map(String::as_str)
        .chain(locked_names.iter().copied())
        .filter(|n| is_next_name(n))
        .collect();
    if !decls.contains_key("common") && !locked_names.contains("common") && next_names.is_empty() {
        return Ok(None);
    }
    let common_rev = check_common(decls.get("common"), lock)?;
    let mut next = Vec::new();
    for name in next_names {
        next.push((
            name.to_string(),
            check_next(name, decls.get(name), lock, parse)?,
        ));
    }
    Ok(Some(Pins { common_rev, next }))
}

fn check_common(decl: Option<&Value>, lock: &Value) -> Result<String> {
    let Some(decl) = decl.filter(|d| d.is_object()) else {
        bail!("common input declaration must be an attribute set");
    };
    let url = str_of(decl, "url").unwrap_or("");
    if url.starts_with(TEMP_PREFIX) {
        bail!(
            "temporary pin present; keep common at {CANONICAL} and name the MR in swl.toml instead"
        );
    }
    if decl != &json!({ "url": CANONICAL }) {
        bail!("common declaration must be exactly {CANONICAL}");
    }
    let node = direct_node(lock, "common")?;
    require_flake("common", node)?;
    if node.get("original") != Some(&canonical_original()) {
        bail!("common lock original must be canonical GitHub owner/repo/ref/type");
    }
    let locked = node.get("locked").cloned().unwrap_or(Value::Null);
    for (k, v) in [
        ("type", "github"),
        ("owner", "binarin"),
        ("repo", "swan-lake-common"),
    ] {
        if str_of(&locked, k) != Some(v) {
            bail!("common locked source must be canonical GitHub owner/repo/type");
        }
    }
    let allowed: BTreeSet<&str> = [
        "type",
        "owner",
        "repo",
        "rev",
        "narHash",
        "lastModified",
        "ref",
    ]
    .into();
    if !keys(&locked).is_subset(&allowed) {
        bail!("unexpected common locked source options");
    }
    if locked
        .get("ref")
        .is_some_and(|r| r.as_str() != Some("swan-lake-current"))
    {
        bail!("noncanonical locked ref");
    }
    let rev = str_of(&locked, "rev").unwrap_or("");
    if !is_rev(rev) {
        bail!("common requires a full locked revision");
    }
    if !is_nar_hash(str_of(&locked, "narHash").unwrap_or("")) {
        bail!("common requires a SHA256 NAR hash");
    }
    Ok(rev.to_string())
}

fn check_next(
    name: &str,
    decl: Option<&Value>,
    lock: &Value,
    parse: &dyn Fn(&str) -> Result<Value>,
) -> Result<Value> {
    let in_lock = root_inputs(lock).is_some_and(|i| i.contains_key(name));
    let Some(decl) = decl.filter(|_| in_lock) else {
        bail!("{name} declaration/lock mismatch; run nix flake lock");
    };
    let url = match str_of(decl, "url") {
        Some(u) if decl == &json!({ "url": u }) => u,
        _ => bail!("{name} declaration must be exactly a url (no follows or options)"),
    };
    let original = parse(url)?;
    check_next_original(name, &original)?;
    let node = direct_node(lock, name)?;
    require_flake(name, node)?;
    if node.get("original") != Some(&original) {
        bail!("{name} declaration/lock mismatch; run nix flake lock");
    }
    let locked = node.get("locked").cloned().unwrap_or(Value::Null);
    check_next_locked(name, &original, &locked)?;
    Ok(locked)
}

/// Any ref or rev of the swan-lake-common repository, nothing else.
pub fn check_next_original(name: &str, original: &Value) -> Result<()> {
    if !original.is_object() {
        bail!("{name} original must be an attribute set");
    }
    let wrong_repo = || anyhow::anyhow!("{name} must point at the swan-lake-common repository");
    let allowed: BTreeSet<&str> = match str_of(original, "type") {
        Some("github") => {
            if str_of(original, "owner") != Some("binarin")
                || str_of(original, "repo") != Some("swan-lake-common")
            {
                return Err(wrong_repo());
            }
            ["type", "owner", "repo", "ref", "rev"].into()
        }
        Some("git") => {
            let url = str_of(original, "url").unwrap_or("");
            let base = url.strip_suffix(".git").unwrap_or(url);
            if base != FORGEJO_URL && base != GITHUB_URL {
                return Err(wrong_repo());
            }
            ["type", "url", "ref", "rev"].into()
        }
        _ => bail!("{name} must be a github: or git+https: reference to swan-lake-common"),
    };
    let extra: Vec<&str> = keys(original).difference(&allowed).copied().collect();
    if !extra.is_empty() {
        bail!("unexpected {name} source options: {extra:?}");
    }
    if let Some(r) = original.get("ref")
        && r.as_str().is_none_or(str::is_empty)
    {
        bail!("{name} ref must be a nonempty string");
    }
    if let Some(r) = original.get("rev")
        && !r.as_str().is_some_and(is_rev)
    {
        bail!("{name} rev must be a full revision");
    }
    Ok(())
}

pub fn check_next_locked(name: &str, original: &Value, locked: &Value) -> Result<()> {
    let (same, allowed): (&[&str], BTreeSet<&str>) = if str_of(original, "type") == Some("github") {
        (
            &["type", "owner", "repo"],
            [
                "type",
                "owner",
                "repo",
                "rev",
                "narHash",
                "lastModified",
                "ref",
            ]
            .into(),
        )
    } else {
        (
            &["type", "url"],
            [
                "type",
                "url",
                "ref",
                "rev",
                "narHash",
                "lastModified",
                "revCount",
            ]
            .into(),
        )
    };
    if same.iter().any(|k| locked.get(k) != original.get(k)) {
        bail!("{name} locked source must match its original repository");
    }
    if !keys(locked).is_subset(&allowed) {
        bail!("unexpected {name} locked source options");
    }
    for k in ["ref", "rev"] {
        if let (Some(o), Some(l)) = (original.get(k), locked.get(k))
            && o != l
        {
            bail!("{name} locked {k} differs from its original");
        }
    }
    if !is_rev(str_of(locked, "rev").unwrap_or("")) {
        bail!("{name} requires a full locked revision");
    }
    if !is_nar_hash(str_of(locked, "narHash").unwrap_or("")) {
        bail!("{name} requires a SHA256 NAR hash");
    }
    Ok(())
}

/// Whether an experiment input's locked revision descends from common's.
/// Informational: offline is "cannot determine", not a failure.
pub fn next_report(name: &str, locked: &Value, common_rev: &str) -> String {
    let rev = str_of(locked, "rev").unwrap_or("");
    let subject = format!("{name} {}", crate::git::short(rev));
    let common = crate::git::short(common_rev);
    if rev == common_rev {
        return format!("{subject} equals the locked common revision");
    }
    let result = match ancestry::git_url(locked) {
        Some(url) => ancestry::is_ancestor(&url, common_rev, rev, true),
        None => Err("not a git source".into()),
    };
    match result {
        Ok(true) => format!("{subject} is based on common {common}"),
        Ok(false) => format!("{subject} is NOT based on common {common} (not a descendant)"),
        Err(reason) => {
            format!("cannot determine whether {subject} is based on common {common}: {reason}")
        }
    }
}

#[cfg(test)]
mod tests {
    //! Ported from test_workflow.py's `Guard` and `NextGuard`.

    use super::*;

    const REV: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const NEXT_REV: &str = "cccccccccccccccccccccccccccccccccccccccc";
    const HASH: &str = "sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";

    fn lock() -> Value {
        json!({"version": 7, "root": "root", "nodes": {
            "root": {"inputs": {"common": "renamed", "dep": ["common", "dep"], "other": "other"}},
            "renamed": {"original": canonical_original(), "locked": {
                "type": "github", "owner": "binarin", "repo": "swan-lake-common",
                "rev": REV, "narHash": HASH}, "inputs": {"dep": "dep"}},
            "dep": {"locked": {"rev": "dependency"}},
            "other": {"locked": {"rev": "unrelated"}},
        }})
    }

    /// Adds an experiment node (and a root follows edge into it).
    fn add_next(mut lock: Value, name: &str, original: Value, locked: Option<Value>) -> Value {
        let locked = locked.unwrap_or_else(|| {
            let mut l = original.clone();
            l.as_object_mut().unwrap().remove("ref");
            l["rev"] = json!(NEXT_REV);
            l["narHash"] = json!(HASH);
            l
        });
        let key = format!("{name}-node");
        lock["nodes"]["root"]["inputs"][name] = json!(key);
        lock["nodes"]["root"]["inputs"][format!("dep-of-{name}")] = json!([name, "dep"]);
        lock["nodes"][&key] =
            json!({"original": original, "inputs": {"dep": "next-dep"}, "locked": locked});
        lock["nodes"]["next-dep"] = json!({"locked": {"rev": "next dependency"}});
        lock
    }

    fn forgejo(rest: &str) -> String {
        format!("git+{FORGEJO_URL}{rest}")
    }

    /// Declarations and the attribute form builtins.parseFlakeRef gives.
    fn accepted() -> Vec<(String, Value)> {
        vec![
            (CANONICAL.into(), canonical_original()),
            (
                "github:binarin/swan-lake-common/sl-0034-x".into(),
                json!({"type": "github", "owner": "binarin", "repo": "swan-lake-common", "ref": "sl-0034-x"}),
            ),
            (
                format!("github:binarin/swan-lake-common/{NEXT_REV}"),
                json!({"type": "github", "owner": "binarin", "repo": "swan-lake-common", "rev": NEXT_REV}),
            ),
            (
                format!("{TEMP_PREFIX}sl-0034-x"),
                json!({"type": "git", "url": FORGEJO_URL, "ref": "sl-0034-x"}),
            ),
            (
                forgejo(".git?ref=a/b"),
                json!({"type": "git", "url": format!("{FORGEJO_URL}.git"), "ref": "a/b"}),
            ),
            (
                forgejo(&format!("?rev={NEXT_REV}")),
                json!({"type": "git", "url": FORGEJO_URL, "rev": NEXT_REV}),
            ),
            (
                format!("git+{GITHUB_URL}?ref=x"),
                json!({"type": "git", "url": GITHUB_URL, "ref": "x"}),
            ),
        ]
    }

    fn parsed(url: &str) -> Value {
        accepted()
            .into_iter()
            .find(|(u, _)| u == url)
            .map(|(_, v)| v)
            .unwrap_or_else(|| panic!("no parse for {url}"))
    }

    fn rejected() -> Vec<Value> {
        vec![
            json!({"type": "github", "owner": "other", "repo": "swan-lake-common", "ref": "x"}),
            json!({"type": "github", "owner": "binarin", "repo": "swan-lake", "ref": "x"}),
            json!({"type": "github", "owner": "binarin", "repo": "swan-lake-common", "host": "example.org"}),
            json!({"type": "github", "owner": "binarin", "repo": "swan-lake-common", "dir": "sub"}),
            json!({"type": "github", "owner": "binarin", "repo": "swan-lake-common", "rev": "abc"}),
            json!({"type": "github", "owner": "Binarin", "repo": "swan-lake-common"}),
            json!({"type": "git", "url": "https://forgejo.lynx-lizard.ts.net/binarin/swan-lake", "ref": "x"}),
            json!({"type": "git", "url": "https://example.org/binarin/swan-lake-common", "ref": "x"}),
            json!({"type": "git", "url": "http://forgejo.lynx-lizard.ts.net/binarin/swan-lake-common", "ref": "x"}),
            json!({"type": "git", "url": "file:///tmp/swan-lake-common"}),
            json!({"type": "git", "url": "ssh://git@forgejo.lynx-lizard.ts.net/binarin/swan-lake-common"}),
            json!({"type": "git", "url": FORGEJO_URL, "ref": "x", "shallow": true}),
            json!({"type": "git", "url": FORGEJO_URL, "ref": "x", "submodules": true}),
            json!({"type": "git", "url": FORGEJO_URL, "ref": ""}),
            json!({"type": "path", "path": "/tmp/swan-lake-common"}),
            json!({"type": "indirect", "id": "swan-lake-common"}),
            json!({"type": "tarball", "url": format!("{FORGEJO_URL}/archive/master.tar.gz")}),
        ]
    }

    fn decls(common: Option<Value>, next: &[(&str, Value)]) -> BTreeMap<String, Value> {
        let mut d = BTreeMap::new();
        if let Some(c) = common {
            d.insert("common".to_string(), c);
        }
        for (n, v) in next {
            d.insert(n.to_string(), v.clone());
        }
        d
    }

    fn canonical_decl() -> Option<Value> {
        Some(json!({ "url": CANONICAL }))
    }

    fn run(d: &BTreeMap<String, Value>, lock: &Value) -> Result<Option<Pins>> {
        check(d, lock, &|u| Ok(parsed(u)))
    }

    fn err(r: Result<Option<Pins>>) -> String {
        format!("{:#}", r.expect_err("the check should fail"))
    }

    #[test]
    fn canonical_renamed_and_stale() {
        let pins = run(&decls(canonical_decl(), &[]), &lock())
            .unwrap()
            .unwrap();
        assert_eq!(pins.common_rev, REV);
        assert!(pins.next.is_empty());
        let mut l = lock();
        l["nodes"]["renamed"]["locked"]["rev"] = json!("b".repeat(40));
        run(&decls(canonical_decl(), &[]), &l).unwrap();
    }

    #[test]
    fn declarations() {
        for url in [
            &format!("{TEMP_PREFIX}feature")[..],
            "path:/tmp/common",
            "git+file:/tmp/common",
            "github:other/repo/ref",
        ] {
            assert!(
                run(&decls(Some(json!({ "url": url })), &[]), &lock()).is_err(),
                "{url}"
            );
        }
        assert!(
            err(run(
                &decls(Some(json!({"url": format!("{TEMP_PREFIX}feature")})), &[]),
                &lock()
            ))
            .contains("temporary pin present")
        );
        let with_follows = json!({"url": CANONICAL, "inputs": {"dep": {"follows": "other"}}});
        assert!(run(&decls(Some(with_follows), &[]), &lock()).is_err());
        assert!(err(run(&decls(None, &[]), &lock())).contains("attribute set"));
    }

    #[test]
    fn bad_fields() {
        for (section, fields) in [
            ("original", &["type", "owner", "repo", "ref"][..]),
            ("locked", &["type", "owner", "repo", "rev", "narHash"]),
        ] {
            for field in fields {
                for value in [None, Some("bad")] {
                    let mut l = lock();
                    let s = l["nodes"]["renamed"][section].as_object_mut().unwrap();
                    match value {
                        None => drop(s.remove(*field)),
                        Some(v) => drop(s.insert(field.to_string(), json!(v))),
                    }
                    assert!(
                        run(&decls(canonical_decl(), &[]), &l).is_err(),
                        "{section}.{field} = {value:?}"
                    );
                }
            }
        }
        let mut l = lock();
        l["nodes"]["renamed"]["locked"]["dir"] = json!("sub");
        assert!(err(run(&decls(canonical_decl(), &[]), &l)).contains("unexpected"));
        let mut l = lock();
        l["nodes"]["renamed"]["flake"] = json!(false);
        assert!(err(run(&decls(canonical_decl(), &[]), &l)).contains("must be a flake"));
    }

    #[test]
    fn follows_and_missing() {
        for edge in [json!(["other"]), json!("missing"), Value::Null] {
            let mut l = lock();
            if edge.is_null() {
                l["nodes"]["root"]["inputs"]
                    .as_object_mut()
                    .unwrap()
                    .remove("common");
            } else {
                l["nodes"]["root"]["inputs"]["common"] = edge.clone();
            }
            assert!(run(&decls(canonical_decl(), &[]), &l).is_err(), "{edge}");
        }
    }

    #[test]
    fn no_common_input_means_no_policy() {
        let mut l = lock();
        l["nodes"]["root"]["inputs"] = json!({"other": "other"});
        assert!(run(&BTreeMap::new(), &l).unwrap().is_none());
    }

    #[test]
    fn accepted_experiment_refs() {
        for (url, original) in accepted() {
            for name in ["common-next", "common-next-disko"] {
                let l = add_next(lock(), name, original.clone(), None);
                let d = decls(canonical_decl(), &[(name, json!({ "url": url }))]);
                let pins = run(&d, &l)
                    .unwrap_or_else(|e| panic!("{url}: {e:#}"))
                    .unwrap();
                assert_eq!(pins.common_rev, REV);
                assert_eq!(pins.next.len(), 1);
                assert_eq!(pins.next[0].0, name);
                assert_eq!(pins.next[0].1["rev"], NEXT_REV);
            }
        }
    }

    #[test]
    fn several_experiments() {
        let (url_a, orig_a) = accepted()[1].clone();
        let (url_b, orig_b) = accepted()[3].clone();
        let l = add_next(lock(), "common-next", orig_a, None);
        let l = add_next(l, "common-next-disko", orig_b, None);
        let d = decls(
            canonical_decl(),
            &[
                ("common-next", json!({ "url": url_a })),
                ("common-next-disko", json!({ "url": url_b })),
            ],
        );
        let pins = run(&d, &l).unwrap().unwrap();
        let names: Vec<&str> = pins.next.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["common-next", "common-next-disko"]);
        assert!(!is_next_name("common-next-"));
        assert!(!is_next_name("common-nextish"));
    }

    #[test]
    fn git_locked_with_rev_count() {
        let url = format!("{TEMP_PREFIX}sl-0034-x");
        let l = add_next(
            lock(),
            "common-next",
            parsed(&url),
            Some(
                json!({"type": "git", "url": FORGEJO_URL, "ref": "sl-0034-x", "rev": NEXT_REV,
                        "narHash": HASH, "lastModified": 1, "revCount": 3}),
            ),
        );
        run(
            &decls(canonical_decl(), &[("common-next", json!({ "url": url }))]),
            &l,
        )
        .unwrap();
    }

    #[test]
    fn rejected_repositories_and_options() {
        for original in rejected() {
            assert!(
                check_next_original("common-next", &original).is_err(),
                "{original}"
            );
            let l = add_next(lock(), "common-next", original.clone(), None);
            let d = decls(canonical_decl(), &[("common-next", json!({"url": "x"}))]);
            assert!(
                check(&d, &l, &|_| Ok(original.clone())).is_err(),
                "{original}"
            );
        }
        for (_, original) in accepted() {
            check_next_original("common-next", &original).unwrap();
        }
    }

    #[test]
    fn declaration_lock_mismatch() {
        let url = format!("{TEMP_PREFIX}sl-0034-x");
        let next_decl = |d: Value| decls(canonical_decl(), &[("common-next", d)]);
        assert!(err(run(&next_decl(json!({ "url": url })), &lock())).contains("mismatch"));
        let l = add_next(lock(), "common-next", parsed(&url), None);
        assert!(err(run(&decls(canonical_decl(), &[]), &l)).contains("mismatch"));
        let mut stale = parsed(&url);
        stale["ref"] = json!("other");
        let l = add_next(lock(), "common-next", stale, None);
        assert!(err(run(&next_decl(json!({ "url": url })), &l)).contains("mismatch"));
        let l = add_next(lock(), "common-next", parsed(&url), None);
        for d in [
            json!({"url": url, "inputs": {"nixpkgs": {"follows": "nixpkgs"}}}),
            json!({"url": url, "flake": false}),
            json!({"follows": "common"}),
        ] {
            assert!(
                err(run(&next_decl(d.clone()), &l)).contains("exactly a url"),
                "{d}"
            );
        }
    }

    #[test]
    fn follows_and_not_flake() {
        let url = format!("{TEMP_PREFIX}sl-0034-x");
        let d = decls(canonical_decl(), &[("common-next", json!({ "url": url }))]);
        let mut l = add_next(lock(), "common-next", parsed(&url), None);
        l["nodes"]["root"]["inputs"]["common-next"] = json!(["common"]);
        assert!(err(run(&d, &l)).contains("direct input"));
        let mut l = add_next(lock(), "common-next", parsed(&url), None);
        l["nodes"]["common-next-node"]["flake"] = json!(false);
        assert!(err(run(&d, &l)).contains("must be a flake"));
    }

    #[test]
    fn bad_locked() {
        let url = format!("{TEMP_PREFIX}sl-0034-x");
        let d = decls(canonical_decl(), &[("common-next", json!({ "url": url }))]);
        let good = json!({"type": "git", "url": FORGEJO_URL, "ref": "sl-0034-x", "rev": NEXT_REV, "narHash": HASH});
        for (field, value) in [
            ("url", Some(json!(format!("{FORGEJO_URL}.git")))),
            ("url", Some(json!("https://example.org/x"))),
            ("type", Some(json!("github"))),
            ("ref", Some(json!("other"))),
            ("rev", Some(json!("abc"))),
            ("rev", None),
            ("narHash", None),
            ("narHash", Some(json!("sha1-x"))),
            ("dir", Some(json!("sub"))),
            ("shallow", Some(json!(true))),
        ] {
            let mut locked = good.clone();
            match value.clone() {
                None => drop(locked.as_object_mut().unwrap().remove(field)),
                Some(v) => locked[field] = v,
            }
            let l = add_next(lock(), "common-next", parsed(&url), Some(locked));
            assert!(run(&d, &l).is_err(), "{field} = {value:?}");
        }
        let pinned = forgejo(&format!("?rev={NEXT_REV}"));
        let locked =
            json!({"type": "git", "url": FORGEJO_URL, "rev": "d".repeat(40), "narHash": HASH});
        let l = add_next(lock(), "common-next", parsed(&pinned), Some(locked));
        let d = decls(
            canonical_decl(),
            &[("common-next", json!({ "url": pinned }))],
        );
        assert!(err(run(&d, &l)).contains("locked rev differs"));
    }

    #[test]
    fn common_stays_canonical() {
        let url = format!("{TEMP_PREFIX}sl-0034-x");
        let l = add_next(lock(), "common-next", parsed(&url), None);
        let next = ("common-next", json!({ "url": url }));
        let temp = json!({"url": format!("{TEMP_PREFIX}feature")});
        assert!(
            err(run(&decls(Some(temp), std::slice::from_ref(&next)), &l))
                .contains("temporary pin present")
        );
        let other = json!({"url": "github:binarin/swan-lake-common/other"});
        assert!(run(&decls(Some(other), &[next]), &l).is_err());
    }

    #[test]
    fn report_equal_and_offline() {
        let locked = json!({"type": "git", "url": "file:///nonexistent/swl-test", "rev": NEXT_REV});
        assert!(next_report("common-next", &locked, NEXT_REV).contains("equals"));
        assert!(next_report("common-next", &locked, REV).starts_with("cannot determine"));
    }
}

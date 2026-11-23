//! Checking that relocking some root inputs changed only their part of a
//! `flake.lock`. Generalises finalize-common's `validate_conversion`
//! (`tools/common-workflow/workflow.py`) to several inputs. Works on the
//! lock JSON as written, so that no field is lost.

use std::collections::BTreeSet;

use anyhow::{Result, anyhow, bail};
use serde_json::{Map, Value, json};

fn nodes(lock: &Value) -> Result<&Map<String, Value>> {
    lock.get("nodes")
        .and_then(Value::as_object)
        .ok_or_else(|| anyhow!("flake.lock has no nodes"))
}

fn root_key(lock: &Value) -> Result<&str> {
    lock.get("root")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("flake.lock has no root"))
}

fn node_inputs(lock: &Value, key: &str) -> Result<Map<String, Value>> {
    Ok(nodes(lock)?
        .get(key)
        .ok_or_else(|| anyhow!("flake.lock has no node {key:?}"))?
        .get("inputs")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default())
}

pub fn root_inputs(lock: &Value) -> Result<Map<String, Value>> {
    node_inputs(lock, root_key(lock)?)
}

/// The node key an input edge (a key, or a `follows` path) resolves to.
pub fn resolve(lock: &Value, edge: &Value) -> Result<String> {
    resolve_depth(lock, edge, 0)
}

fn resolve_depth(lock: &Value, edge: &Value, depth: usize) -> Result<String> {
    if depth > 64 {
        bail!("malformed or cyclic follows in flake.lock");
    }
    match edge {
        Value::String(k) => Ok(k.clone()),
        Value::Array(path) => {
            let mut key = root_key(lock)?.to_string();
            for name in path {
                let name = name
                    .as_str()
                    .ok_or_else(|| anyhow!("malformed follows in flake.lock"))?;
                let next = node_inputs(lock, &key)?
                    .get(name)
                    .cloned()
                    .ok_or_else(|| anyhow!("follows path {path:?} does not resolve"))?;
                key = resolve_depth(lock, &next, depth + 1)?;
            }
            Ok(key)
        }
        _ => bail!("malformed input edge in flake.lock"),
    }
}

/// The node key of an input path such as `common/zwanenzang`.
pub fn resolve_path(lock: &Value, path: &str) -> Result<String> {
    let edge = Value::Array(path.split('/').map(|s| json!(s)).collect());
    resolve(lock, &edge)
}

/// Every node key reachable from `key`, following `follows` too.
fn closure(lock: &Value, key: &str, out: &mut BTreeSet<String>) -> Result<()> {
    if !out.insert(key.to_string()) {
        return Ok(());
    }
    for edge in node_inputs(lock, key)?.values() {
        closure(lock, &resolve(lock, edge)?, out)?;
    }
    Ok(())
}

/// The graph below `key` through direct edges, sources included. A
/// `follows` edge is kept as its path: it may legitimately lead into a
/// relocked input.
fn graph(lock: &Value, key: &str, active: &mut Vec<String>) -> Result<Value> {
    if active.iter().any(|k| k == key) {
        bail!("cyclic dependency graph in flake.lock");
    }
    let node = nodes(lock)?
        .get(key)
        .ok_or_else(|| anyhow!("flake.lock has no node {key:?}"))?;
    active.push(key.to_string());
    let mut inputs = Map::new();
    for (name, edge) in node_inputs(lock, key)? {
        let v = match &edge {
            Value::String(k) => graph(lock, k, active)?,
            follows => json!({ "follows": follows }),
        };
        inputs.insert(name, v);
    }
    active.pop();
    Ok(json!({
        "locked": node.get("locked"),
        "original": node.get("original"),
        "flake": node.get("flake").unwrap_or(&Value::Bool(true)),
        "inputs": inputs,
    }))
}

fn locked_field<'a>(lock: &'a Value, key: &str, field: &str) -> Option<&'a Value> {
    lock.pointer(&format!("/nodes/{key}/locked/{field}"))
}

/// Checks that `new` differs from `old` only below the root inputs in
/// `updated`: every other node and root edge is unchanged, the other root
/// inputs keep their resolved graphs (shared nodes included), and new nodes
/// are reachable only from the updated inputs. An updated input whose
/// revision stayed the same must keep its whole graph.
pub fn verify(old: &Value, new: &Value, updated: &[String]) -> Result<()> {
    if old.get("version") != new.get("version") {
        bail!("the lock file version changed");
    }
    let (old_root, new_root) = (root_key(old)?, root_key(new)?);
    let (old_inputs, new_inputs) = (root_inputs(old)?, root_inputs(new)?);
    let is_updated = |n: &str| updated.iter().any(|u| u == n);

    let mut old_reach = BTreeSet::new();
    let mut new_reach = BTreeSet::new();
    for name in updated {
        if let Some(edge) = old_inputs.get(name) {
            closure(old, &resolve(old, edge)?, &mut old_reach)?;
        }
        let edge = new_inputs
            .get(name)
            .ok_or_else(|| anyhow!("relocking dropped the input {name}"))?;
        closure(new, &resolve(new, edge)?, &mut new_reach)?;
    }
    for (key, node) in nodes(old)? {
        if key != old_root && !old_reach.contains(key) && nodes(new)?.get(key) != Some(node) {
            bail!("unexpected unrelated lock churn: {key}");
        }
    }
    for (name, edge) in &old_inputs {
        if !is_updated(name) && new_inputs.get(name) != Some(edge) {
            bail!("unexpected lock churn: root input {name} changed");
        }
    }
    for name in new_inputs.keys() {
        if !is_updated(name) && !old_inputs.contains_key(name) {
            bail!("unexpected lock churn: new root input {name}");
        }
    }
    for (key, _) in nodes(new)? {
        if key != new_root && !nodes(old)?.contains_key(key) && !new_reach.contains(key) {
            bail!("unexpected new lock node {key}");
        }
    }
    for (name, edge) in &old_inputs {
        if is_updated(name) || !edge.is_string() {
            continue;
        }
        let before = graph(old, &resolve(old, edge)?, &mut Vec::new())?;
        let after = graph(new, &resolve(new, &new_inputs[name])?, &mut Vec::new())?;
        if before != after {
            bail!("unexpected lock churn: resolved graph of {name} changed");
        }
    }
    for name in updated {
        let Some(edge) = old_inputs.get(name) else {
            continue;
        };
        let (old_key, new_key) = (resolve(old, edge)?, resolve(new, &new_inputs[name])?);
        if locked_field(old, &old_key, "rev") != locked_field(new, &new_key, "rev") {
            continue;
        }
        if locked_field(old, &old_key, "narHash") != locked_field(new, &new_key, "narHash") {
            bail!("{name}: same revision has a different NAR hash");
        }
        let mut a = graph(old, &old_key, &mut Vec::new())?;
        let mut b = graph(new, &new_key, &mut Vec::new())?;
        for g in [&mut a, &mut b] {
            g["locked"] = Value::Null;
            g["original"] = Value::Null;
        }
        if a != b {
            bail!("{name}: same revision changed its resolved dependency graph");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    //! Ported from test_workflow.py's `Finalization` cases.

    use super::*;

    const HASH: &str = "sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";

    fn lock() -> Value {
        json!({"version": 7, "root": "root", "nodes": {
            "root": {"inputs": {"common": "renamed", "dep": ["common", "dep"], "other": "other"}},
            "renamed": {"original": {"type": "github", "owner": "binarin", "repo": "swan-lake-common", "ref": "swan-lake-current"},
                        "locked": {"type": "github", "owner": "binarin", "repo": "swan-lake-common",
                                   "rev": "a".repeat(40), "narHash": HASH}, "inputs": {"dep": "dep"}},
            "dep": {"locked": {"rev": "dependency"}},
            "other": {"locked": {"rev": "unrelated"}},
        }})
    }

    fn with_next(mut l: Value) -> Value {
        l["nodes"]["root"]["inputs"]["common-next"] = json!("next");
        l["nodes"]["root"]["inputs"]["next-dep"] = json!(["common-next", "dep"]);
        l["nodes"]["next"] = json!({"locked": {"rev": "c".repeat(40), "narHash": HASH}, "inputs": {"dep": "next-dep"}});
        l["nodes"]["next-dep"] = json!({"locked": {"rev": "next dependency"}});
        l
    }

    fn common() -> Vec<String> {
        vec!["common".into()]
    }

    #[test]
    fn equivalent_and_newer() {
        let (old, mut new) = (lock(), lock());
        verify(&old, &new, &common()).unwrap();
        new["nodes"]["renamed"]["locked"]["rev"] = json!("b".repeat(40));
        new["nodes"]["dep"]["locked"]["rev"] = json!("new dependency");
        verify(&old, &new, &common()).unwrap();
    }

    #[test]
    fn bad_changes() {
        for (node, field, value) in [
            ("renamed", "narHash", "bad"),
            ("dep", "rev", "changed"),
            ("other", "rev", "changed"),
        ] {
            let (old, mut new) = (lock(), lock());
            new["nodes"][node]["locked"][field] = json!(value);
            assert!(verify(&old, &new, &common()).is_err(), "{node}");
        }
        let (old, mut new) = (lock(), lock());
        new["nodes"]["other"]["locked"]["rev"] = json!("changed");
        new["nodes"]["renamed"]["locked"]["rev"] = json!("b".repeat(40));
        let e = verify(&old, &new, &common()).unwrap_err().to_string();
        assert!(e.contains("unrelated lock churn: other"), "{e}");
    }

    #[test]
    fn shared_dependency_of_another_input() {
        let shared = || {
            let mut l = with_next(lock());
            l["nodes"]["next"]["inputs"]["dep"] = json!("dep");
            l["nodes"].as_object_mut().unwrap().remove("next-dep");
            l
        };
        for (name, key, other) in [
            ("common", "renamed", "common-next"),
            ("common-next", "next", "common"),
        ] {
            let (old, mut new) = (shared(), shared());
            new["nodes"][key]["locked"]["rev"] = json!("b".repeat(40));
            verify(&old, &new, &[name.to_string()]).unwrap();
            new["nodes"]["dep"]["locked"]["rev"] = json!("changed");
            let e = verify(&old, &new, &[name.to_string()])
                .unwrap_err()
                .to_string();
            assert!(
                e.contains(&format!("resolved graph of {other} changed")),
                "{e}"
            );
        }
    }

    #[test]
    fn same_revision_must_keep_its_graph() {
        let (old, mut new) = (lock(), lock());
        new["nodes"]["dep"]["original"] = json!({"type": "path", "path": "/tmp/other"});
        let e = verify(&old, &new, &common()).unwrap_err().to_string();
        assert!(e.contains("dependency graph"), "{e}");
    }

    #[test]
    fn follows_into_the_relocked_input_may_change() {
        // swan-lake's nixpkgs follows common/nixpkgs: relocking common moves it.
        let (mut old, mut new) = (lock(), lock());
        for l in [&mut old, &mut new] {
            l["nodes"]["root"]["inputs"]["x"] = json!("x");
            l["nodes"]["x"] = json!({"inputs": {"dep": ["dep"]}, "locked": {"rev": "x"}});
        }
        new["nodes"]["renamed"]["locked"]["rev"] = json!("b".repeat(40));
        new["nodes"]["dep"]["locked"]["rev"] = json!("new dependency");
        verify(&old, &new, &common()).unwrap();
    }

    #[test]
    fn new_nodes_only_below_relocked_inputs() {
        let (old, mut new) = (lock(), lock());
        new["nodes"]["stray"] = json!({"locked": {"rev": "s"}});
        assert!(
            verify(&old, &new, &common())
                .unwrap_err()
                .to_string()
                .contains("stray")
        );
        // A new input in flake.nix, landed through the manifest.
        let mut new = lock();
        new["nodes"]["root"]["inputs"]["common-next-x"] = json!("nx");
        new["nodes"]["nx"] = json!({"locked": {"rev": "n"}});
        verify(&old, &new, &["common-next-x".into()]).unwrap();
        assert!(verify(&old, &new, &common()).is_err());
    }

    #[test]
    fn resolves_paths() {
        let l = lock();
        assert_eq!(resolve_path(&l, "common/dep").unwrap(), "dep");
        assert_eq!(resolve_path(&l, "dep").unwrap(), "dep");
        assert!(resolve_path(&l, "nope").is_err());
    }
}

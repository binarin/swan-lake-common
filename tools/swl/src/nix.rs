//! Calls to the real `nix` (never a swl wrapper): evaluating a flake's input
//! declarations, parsing flake references, and relocking.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use anyhow::{Context, Result, bail};
use serde_json::Value;

use crate::shim::find_real_tool;

pub struct Nix {
    bin: PathBuf,
}

/// swl's own secrets, which nix (evaluating the MR's flake.nix, fetching its
/// inputs) never sees.
const SECRET_ENV: [&str; 2] = [crate::github::TOKEN_ENV, crate::forge::FJ_KEYS_ENV];

impl Nix {
    pub fn find() -> Result<Self> {
        Ok(Self {
            bin: find_real_tool("nix", None)?,
        })
    }

    fn command(&self) -> Command {
        let mut c = Command::new(&self.bin);
        c.args(["--extra-experimental-features", "nix-command flakes"]);
        for var in SECRET_ENV {
            c.env_remove(var);
        }
        c.stdin(Stdio::null());
        c
    }

    /// Runs nix with `args`; stdout, or an error with nix's stderr.
    pub fn run(&self, args: &[&str]) -> Result<String> {
        let out = self
            .command()
            .args(args)
            .output()
            .with_context(|| format!("running {}", self.bin.display()))?;
        if !out.status.success() {
            bail!(
                "nix {} failed:\n{}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    fn eval_json(&self, expr: &str, extra: &[&str]) -> Result<Value> {
        let mut args = vec!["eval", "--impure", "--json"];
        args.extend(extra);
        args.extend(["--expr", expr]);
        let out = self.run(&args)?;
        serde_json::from_str(&out).context("nix eval printed no JSON")
    }

    /// The `inputs` attribute set of the flake at `root`, evaluated from its
    /// `flake.nix` alone, without fetching any input. Missing `inputs` is
    /// empty. In restricted mode: only `root` may be read, nothing fetched,
    /// and `builtins.getEnv` is empty.
    pub fn declarations(&self, root: &Path) -> Result<BTreeMap<String, Value>> {
        let file = root.join("flake.nix");
        let expr = format!(
            "(import {}).inputs or {{}}",
            serde_json::to_string(&file.to_string_lossy())?
        );
        let root_s = root.to_string_lossy();
        let v = self
            .eval_json(&expr, &["--option", "restrict-eval", "true", "-I", &root_s])
            .with_context(|| format!("reading the inputs of {}", file.display()))?;
        match v {
            Value::Object(m) => Ok(m.into_iter().collect()),
            _ => bail!("{}: inputs is not an attribute set", file.display()),
        }
    }

    /// The attribute form of a flake reference, as `nix flake lock` records
    /// it in a node's `original`.
    pub fn parse_flake_ref(&self, url: &str) -> Result<Value> {
        self.eval_json(
            &format!("builtins.parseFlakeRef {}", serde_json::to_string(url)?),
            &[],
        )
        .with_context(|| format!("parsing the flake reference {url:?}"))
    }

    /// `nix flake lock ROOT` from the lock `reference`, with `(input, flake
    /// ref)` overrides, into `output`; the flake's own lock is not touched.
    /// Inputs missing from `reference` are locked as nix does for a new
    /// input: an overridden one at that flake ref, its own inputs from its
    /// lock at that revision (honouring the root's follows).
    pub fn lock_from(
        &self,
        root: &Path,
        reference: &Path,
        overrides: &[(String, String)],
        output: &Path,
    ) -> Result<()> {
        let (root_s, reference_s, output_s) = (
            root.to_string_lossy(),
            reference.to_string_lossy(),
            output.to_string_lossy(),
        );
        let mut args = vec![
            "flake",
            "lock",
            &*root_s,
            "--reference-lock-file",
            &*reference_s,
            "--output-lock-file",
            &*output_s,
        ];
        for (input, flake_ref) in overrides {
            args.extend(["--override-input", input, flake_ref]);
        }
        self.run(&args).map(drop)
    }

    /// `nix flake update INPUTS --flake ROOT`, writing the new lock to
    /// `output` instead of the flake's `flake.lock`. `--refresh` so that a
    /// moved branch, such as a fresh promotion, is seen.
    pub fn update_to(&self, root: &Path, inputs: &[String], output: &Path) -> Result<()> {
        let root_s = root.to_string_lossy();
        let output_s = output.to_string_lossy();
        let mut args = vec!["flake", "update"];
        args.extend(inputs.iter().map(String::as_str));
        args.extend([
            "--flake",
            &root_s,
            "--output-lock-file",
            &output_s,
            "--refresh",
        ]);
        self.run(&args).map(drop)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nix_never_sees_swls_secrets() {
        let nix = Nix { bin: "nix".into() };
        let c = nix.command();
        let removed: Vec<_> = c
            .get_envs()
            .filter(|(_, v)| v.is_none())
            .map(|(k, _)| k.to_string_lossy().into_owned())
            .collect();
        assert_eq!(removed, ["SWL_FJ_KEYS", "SWL_GITHUB_TOKEN"]);
    }
}

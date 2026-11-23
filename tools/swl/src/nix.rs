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

impl Nix {
    pub fn find() -> Result<Self> {
        Ok(Self {
            bin: find_real_tool("nix", None)?,
        })
    }

    fn command(&self) -> Command {
        let mut c = Command::new(&self.bin);
        c.args(["--extra-experimental-features", "nix-command flakes"]);
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

    fn eval_json(&self, expr: &str) -> Result<Value> {
        let out = self.run(&["eval", "--impure", "--json", "--expr", expr])?;
        serde_json::from_str(&out).context("nix eval printed no JSON")
    }

    /// The `inputs` attribute set of the flake at `root`, evaluated from its
    /// `flake.nix` alone, without fetching any input. Missing `inputs` is
    /// empty.
    pub fn declarations(&self, root: &Path) -> Result<BTreeMap<String, Value>> {
        let file = root.join("flake.nix");
        let expr = format!(
            "(import {}).inputs or {{}}",
            serde_json::to_string(&file.to_string_lossy())?
        );
        let v = self
            .eval_json(&expr)
            .with_context(|| format!("reading the inputs of {}", file.display()))?;
        match v {
            Value::Object(m) => Ok(m.into_iter().collect()),
            _ => bail!("{}: inputs is not an attribute set", file.display()),
        }
    }

    /// The attribute form of a flake reference, as `nix flake lock` records
    /// it in a node's `original`.
    pub fn parse_flake_ref(&self, url: &str) -> Result<Value> {
        self.eval_json(&format!(
            "builtins.parseFlakeRef {}",
            serde_json::to_string(url)?
        ))
        .with_context(|| format!("parsing the flake reference {url:?}"))
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

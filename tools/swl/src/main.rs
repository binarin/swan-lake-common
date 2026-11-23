mod ancestry;
mod check;
mod config;
mod env;
mod forge;
mod git;
mod lock;
mod manifest;
mod nix;
mod nix_args;
mod pin_policy;
mod shim;
mod status;
mod tempdir;
mod workspace;
mod ws_add;

use std::{ffi::OsString, path::PathBuf, process::ExitCode};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

use crate::{check::Event, config::Config, forge::AnonApi, shim::Tool, workspace::Workspace};

#[derive(Parser)]
#[command(
    name = "swl",
    version,
    about = "Swan Lake cross-repo workspaces: checkouts, input overrides, the nix/nh/deploy shims, manifests and CI"
)]
struct Cli {
    /// Repos config [default: $SWL_CONFIG, else $XDG_CONFIG_HOME/swl/repos.toml]
    #[arg(long, global = true, value_name = "FILE")]
    config: Option<PathBuf>,
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Manage the workspace's checkouts
    Ws {
        #[command(subcommand)]
        command: WsCommands,
    },
    /// Write each consumer's .envrc.local and the workspace's nix, nh and deploy wrappers
    Env {
        /// Workspace directory [default: current directory]
        #[arg(long, value_name = "DIR")]
        workspace: Option<PathBuf>,
        /// Overwrite a .envrc.local that swl did not write
        #[arg(long)]
        force: bool,
    },
    /// Show the shared checkouts against the revs the workspace's flakes lock
    Status {
        /// Workspace directory [default: current directory]
        #[arg(long, value_name = "DIR")]
        workspace: Option<PathBuf>,
    },
    /// CI gate: on an MR, flake.lock unchanged and swl.toml valid; on master, no swl.toml and the pin policy
    Check {
        /// The repo [default: current directory]
        dir: Option<PathBuf>,
        /// [default: from GITHUB_EVENT_NAME and GITHUB_REF]
        #[arg(long)]
        event: Option<Event>,
        /// Revision an MR is compared against [default: origin/$GITHUB_BASE_REF, fetched]
        #[arg(long, value_name = "REV")]
        base: Option<String>,
    },
    /// Run the real tool with the workspace's overrides and guards (used by the wrappers)
    Shim {
        tool: Tool,
        #[arg(long, value_name = "DIR")]
        workspace: PathBuf,
        /// Arguments for the tool
        #[arg(last = true, allow_hyphen_values = true)]
        args: Vec<OsString>,
    },
}

#[derive(Subcommand)]
enum WsCommands {
    // In `about`, not a doc comment, so that rustdoc does not read the
    // placeholders as HTML tags.
    #[command(about = "Add a worktree checkout: <consumer> or <repo>@<input>")]
    Add {
        name: String,
        /// Check out this branch, creating it at the repo's base if missing [default: detached at the base]
        #[arg(long)]
        branch: Option<String>,
        /// Workspace directory [default: current directory]
        #[arg(long, value_name = "DIR")]
        workspace: Option<PathBuf>,
    },
}

fn workspace_dir(flag: Option<PathBuf>) -> Result<PathBuf> {
    match flag {
        Some(dir) => Ok(dir),
        None => std::env::current_dir().context("current directory"),
    }
}

fn run(cli: Cli) -> Result<ExitCode> {
    let config_flag = cli.config.as_deref();
    match cli.command {
        Commands::Ws {
            command:
                WsCommands::Add {
                    name,
                    branch,
                    workspace,
                },
        } => {
            let config = Config::load(config_flag)?;
            ws_add::run(
                &config,
                &workspace_dir(workspace)?,
                &name,
                branch.as_deref(),
            )?;
        }
        Commands::Env { workspace, force } => {
            let config = Config::load(config_flag)?;
            let ws = Workspace::scan(&workspace_dir(workspace)?)?;
            let swl = std::env::current_exe()
                .and_then(|p| p.canonicalize())
                .context("locating the swl executable")?;
            env::run(&ws, &config, &swl, force)?;
        }
        Commands::Status { workspace } => {
            let config = Config::load(config_flag)?;
            status::run(&Workspace::scan(&workspace_dir(workspace)?)?, &config)?;
        }
        Commands::Check { dir, event, base } => {
            let config = Config::load(config_flag)?;
            return check::run(
                &workspace_dir(dir)?,
                event,
                base.as_deref(),
                &config,
                &AnonApi::from_env(),
            );
        }
        Commands::Shim {
            tool,
            workspace,
            args,
        } => {
            return shim::run(tool, &workspace, config_flag, args);
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("swl: {e:#}");
            ExitCode::from(2)
        }
    }
}

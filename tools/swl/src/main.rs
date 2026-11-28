mod ancestry;
mod bump;
mod check;
mod ci;
mod config;
mod env;
mod finalize;
mod forge;
mod git;
mod github;
mod hook;
mod land;
mod lock;
mod lock_change;
mod lock_rules;
mod logging;
mod ls;
mod manifest;
mod nix;
mod nix_args;
mod open_mrs;
mod package;
mod pin_policy;
mod promote;
mod publish;
mod refresh;
mod report;
mod shim;
mod start;
mod status;
mod status_tree;
mod style;
mod tempdir;
mod version;
mod views;
mod workspace;
mod workspace_location;
mod ws_add;
mod ws_prune;
mod ws_remove;

use std::{ffi::OsString, path::PathBuf, process::ExitCode, time::Duration};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

use crate::{check::Event, config::Config, forge::AnonApi, shim::Tool, workspace::Workspace};

#[derive(Parser)]
#[command(
    name = "swl",
    version = version::VERSION.as_str(),
    about = "Swan Lake cross-repo workspaces: checkouts, input overrides, the nix/nh/deploy shims, manifests and CI"
)]
struct Cli {
    /// Repos config [default: $SWL_CONFIG, else $XDG_CONFIG_HOME/swl/repos.toml]
    #[arg(long, global = true, value_name = "FILE")]
    config: Option<PathBuf>,
    /// Colour the output: auto is on a terminal, unless NO_COLOR is set (CLICOLOR_FORCE forces it)
    #[arg(long, global = true, value_name = "WHEN", default_value = "auto")]
    color: style::ColorWhen,
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
    // In `about`, not a doc comment, so that rustdoc does not read the
    // placeholders as HTML tags.
    #[command(
        about = "Create or finish a workspace for one change: <name> <consumer>... <repo>@<input>..., or --from <consumer>#<MR>"
    )]
    Start {
        /// Workspace name, e.g. SL-0060-foo; the branch is its lower-case form
        #[arg(required_unless_present = "from", conflicts_with = "from")]
        name: Option<String>,
        /// Checkouts: at least one consumer, and <repo>@<input> shared checkouts
        #[arg(conflicts_with = "from")]
        checkouts: Vec<String>,
        /// Join a published consumer MR, e.g. swan-lake#12, with the shared MRs its swl.toml names
        #[arg(long, value_name = "CONSUMER#MR")]
        from: Option<String>,
        /// Workspace directory [default: configured workspaces/<name or consumer branch>]
        #[arg(long, value_name = "DIR")]
        workspace: Option<PathBuf>,
        /// Branch of every checkout [default: <name> lower-cased]
        #[arg(long, conflicts_with = "from")]
        branch: Option<String>,
        /// Do not fetch the sources; use the refs they already have
        #[arg(long)]
        no_fetch: bool,
    },
    /// Write each consumer's .envrc.local and the workspace's nix, nh and deploy wrappers
    Env {
        /// Workspace name, ticket ID or path (alternative to --workspace)
        #[arg(value_name = "WORKSPACE", conflicts_with = "workspace")]
        workspace_name: Option<PathBuf>,
        /// Workspace name, ticket ID or path [default: enclosing workspace]
        #[arg(long, value_name = "WORKSPACE")]
        workspace: Option<PathBuf>,
        /// Overwrite a .envrc.local that swl did not write
        #[arg(long)]
        force: bool,
    },
    /// Where the change stands: per checkout or MR its stage, the next step and who it waits for
    Status {
        /// Workspace name, ticket ID or path (alternative to --workspace)
        #[arg(
            value_name = "WORKSPACE",
            conflicts_with = "workspace",
            conflicts_with = "from"
        )]
        workspace_name: Option<PathBuf>,
        /// Workspace name, ticket ID or path [default: enclosing workspace]
        #[arg(long, value_name = "WORKSPACE", conflicts_with = "from")]
        workspace: Option<PathBuf>,
        /// A published consumer MR, e.g. swan-lake#12, without a workspace
        #[arg(long, value_name = "CONSUMER#MR")]
        from: Option<String>,
        /// Do not fetch; use the refs already known
        #[arg(long)]
        no_fetch: bool,
    },
    /// All work in progress: each workspace's stage, waits and next step, and the forge's open MRs that no workspace holds
    Ls {
        /// Do not fetch or ask the forge: the workspaces only, from the refs already known
        #[arg(long)]
        no_fetch: bool,
    },
    /// Land the workspace's change: merge the shared MRs, promote, finalize, push and merge each consumer MR, then remove the workspace
    #[command(
        after_help = "Exit status: 0 when everything has landed or there is nothing to land (and for a dry run), 3 when the next step waits for the owner, 4 for anyone, 5 for nobody (CI, a mirror, the forge: rerun later), 2 on errors."
    )]
    Land {
        /// Print what would be done; write nothing
        #[arg(long)]
        dry_run: bool,
        /// Keep the workspace when everything has landed
        #[arg(long)]
        keep: bool,
        /// Workspace name, ticket ID or path (alternative to --workspace)
        #[arg(value_name = "WORKSPACE", conflicts_with = "workspace")]
        workspace_name: Option<PathBuf>,
        /// Workspace name, ticket ID or path [default: enclosing workspace; with --from, configured workspaces/<consumer branch>]
        #[arg(long, value_name = "WORKSPACE")]
        workspace: Option<PathBuf>,
        /// A published consumer MR, e.g. swan-lake#12: swl start --from it first (or reuse its workspace)
        #[arg(long, value_name = "CONSUMER#MR")]
        from: Option<String>,
        /// Seconds to wait for the mirror and the forge
        #[arg(long, default_value_t = 300, value_name = "SECONDS")]
        timeout: u64,
        /// Seconds between checks while waiting
        #[arg(long, default_value_t = 10, value_name = "SECONDS")]
        interval: u64,
    },
    /// Rebase the change's open MR branches onto their bases and push them with a lease, shared MRs first, then the consumers
    #[command(
        after_help = "Exit status: 0 when every MR branch contains its base or was rebased (and for a dry run), 3 when the next step waits for the owner, 4 for anyone, 5 for nobody (the forge: rerun later), 2 on errors."
    )]
    Refresh {
        /// Print what would be rebased and pushed; write nothing
        #[arg(long)]
        dry_run: bool,
        /// Workspace name, ticket ID or path (alternative to --workspace)
        #[arg(value_name = "WORKSPACE", conflicts_with = "workspace")]
        workspace_name: Option<PathBuf>,
        /// Workspace name, ticket ID or path [default: enclosing workspace]
        #[arg(long, value_name = "WORKSPACE")]
        workspace: Option<PathBuf>,
        /// Seconds to wait for the forge to show a pushed head
        #[arg(long, default_value_t = 300, value_name = "SECONDS")]
        timeout: u64,
        /// Seconds between checks while waiting
        #[arg(long, default_value_t = 10, value_name = "SECONDS")]
        interval: u64,
    },
    /// Bump common's inputs forward and push that to its master, promote it, then a bump MR per consumer moving common there, merged
    #[command(
        after_help = "Exit status: 0 when everything is bumped or there is nothing to bump (and for a dry run), 3 when a step waits for the owner, 4 for anyone, 5 for nobody (CI, a mirror, the forge: rerun later), 2 on errors."
    )]
    Bump {
        /// Print the plan; write nothing
        #[arg(long)]
        dry_run: bool,
        /// Run nix flake check --no-build on each relocked tree before committing it
        #[arg(long)]
        check: bool,
        /// Seconds to wait for the mirror and the forge
        #[arg(long, default_value_t = 300, value_name = "SECONDS")]
        timeout: u64,
        /// Seconds between checks while waiting
        #[arg(long, default_value_t = 10, value_name = "SECONDS")]
        interval: u64,
    },
    /// Fast-forward each repo's view, its umbrella checkout on master, to the repo's base
    #[command(
        after_help = "Exit status: 0 when every view is up to date, fast-forwarded or skipped with a warning, 2 on errors."
    )]
    Views {
        /// Do not fetch; use the refs already known
        #[arg(long)]
        no_fetch: bool,
    },
    // In `about`, not a doc comment, so that rustdoc does not read the
    // placeholder as an HTML tag.
    #[command(
        about = "Fast-forward common's promotion branch to <rev> of its master, then wait for its declared sources",
        after_help = "Exit status: 0 when promoted and mirrored, also when it was already (and for a dry run), 3 when the next step waits for the owner, 5 for nobody (the mirror: rerun later), 2 on errors."
    )]
    Promote {
        /// A commit of common's master [default: its origin/master]
        rev: Option<String>,
        /// Print what would be pushed; write nothing
        #[arg(long)]
        dry_run: bool,
        /// Seconds to wait for the mirror
        #[arg(long, default_value_t = 300, value_name = "SECONDS")]
        timeout: u64,
        /// Seconds between checks while waiting
        #[arg(long, default_value_t = 10, value_name = "SECONDS")]
        interval: u64,
    },
    /// Push a consumer's changed shared checkouts and the consumer, open their MRs, and record them in swl.toml
    Publish {
        /// The consumer checkout, e.g. swan-lake
        consumer: String,
        /// Workspace name, ticket ID or path [default: enclosing workspace]
        #[arg(long, value_name = "WORKSPACE")]
        workspace: Option<PathBuf>,
    },
    /// Commit, on the consumer's branch, the relock of the swl.toml entries that have landed
    Finalize {
        /// The consumer repo [default: current directory]
        dir: Option<PathBuf>,
        /// Wait until every merged entry is promoted (and mirrored), up to --timeout
        #[arg(long)]
        wait: bool,
        /// Seconds to wait with --wait
        #[arg(long, default_value_t = 300, value_name = "SECONDS")]
        timeout: u64,
        /// Seconds between checks with --wait
        #[arg(long, default_value_t = 10, value_name = "SECONDS")]
        interval: u64,
    },
    /// CI gate: on an MR, flake.lock unchanged or changed by a landing or forward moves, and swl.toml valid; on master, no swl.toml and the pin policy
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
    /// Commands for CI jobs
    Ci {
        #[command(subcommand)]
        command: CiCommands,
    },
    /// Print the shell function `swl`, which changes to the new workspace after `swl start`: eval "$(swl hook zsh)"
    Hook { shell: hook::Shell },
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
enum CiCommands {
    /// Put a nix wrapper on $GITHUB_PATH that overrides the swl.toml inputs with the MRs' heads
    Env {
        /// The repo CI checked out [default: current directory]
        dir: Option<PathBuf>,
    },
    /// What the wrapper of `ci env` runs
    #[command(hide = true)]
    Shim {
        #[arg(long, value_name = "DIR")]
        flake: PathBuf,
        #[arg(long = "override", num_args = 2, value_names = ["INPUT", "URL"])]
        overrides: Vec<String>,
        /// Arguments for nix
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
        /// Check out this branch: the local one, else one tracking the remote's, else a new one at the base [default: detached at the base]
        #[arg(long)]
        branch: Option<String>,
        /// Workspace name, ticket ID or path [default: enclosing workspace]
        #[arg(long, value_name = "WORKSPACE")]
        workspace: Option<PathBuf>,
        /// Do not fetch the source first; use the refs it already has
        #[arg(long)]
        no_fetch: bool,
        /// Do not run `swl env` afterwards
        #[arg(long)]
        no_env: bool,
    },
    /// Remove a workspace: each checkout's worktree, its branch when the branch is pushed, then the directory
    Remove {
        /// Workspace name, ticket ID or path (alternative to --workspace)
        #[arg(value_name = "WORKSPACE", conflicts_with = "workspace")]
        workspace_name: Option<PathBuf>,
        /// Workspace name, ticket ID or path [default: enclosing workspace]
        #[arg(long, value_name = "WORKSPACE")]
        workspace: Option<PathBuf>,
        /// Remove checkouts with uncommitted or untracked files, or commits on no remote branch, too
        #[arg(long)]
        force: bool,
        /// Do not fetch the sources first; use the refs they already have
        #[arg(long)]
        no_fetch: bool,
    },
}

/// The swl that the workspace's wrappers and CI's nix wrapper exec, and
/// that `status` reports as running: the package's wrapper when it set
/// `SWL_SELF`, else this executable.
fn swl_exe() -> Result<PathBuf> {
    if let Some(path) = &package::get().self_path {
        return Ok(path.clone());
    }
    std::env::current_exe()
        .and_then(|p| p.canonicalize())
        .context("locating the swl executable")
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
                    no_fetch,
                    no_env,
                },
        } => {
            let config = Config::load(config_flag)?;
            let swl = if no_env { None } else { Some(swl_exe()?) };
            ws_add::run(
                &config,
                &workspace_location::resolve(
                    &config,
                    &std::env::current_dir()?,
                    workspace.as_deref(),
                )?,
                &name,
                branch.as_deref(),
                &ws_add::Fetcher::new(!no_fetch),
                swl.as_deref(),
            )?;
        }
        Commands::Ws {
            command:
                WsCommands::Remove {
                    workspace_name,
                    workspace,
                    force,
                    no_fetch,
                },
        } => {
            let config = Config::load(config_flag)?;
            let cwd = std::env::current_dir().context("current directory")?;
            let workspace = workspace.or(workspace_name);
            ws_remove::run(
                &config,
                &workspace_location::resolve(&config, &cwd, workspace.as_deref())?,
                &cwd,
                force,
                &ws_add::Fetcher::new(!no_fetch),
            )?;
        }
        Commands::Start {
            name,
            checkouts,
            from,
            workspace,
            branch,
            no_fetch,
        } => {
            let config = Config::load(config_flag)?;
            let cwd = std::env::current_dir().context("current directory")?;
            let fetcher = ws_add::Fetcher::new(!no_fetch);
            let swl = swl_exe()?;
            let started = match from {
                Some(from) => start::run_from(
                    &config,
                    &cwd,
                    &from,
                    workspace.as_deref(),
                    &fetcher,
                    &forge::ForgeReader::from_env(),
                    &swl,
                )?,
                None => start::run_new(
                    &config,
                    &cwd,
                    name.as_deref().expect("required by clap without --from"),
                    &checkouts,
                    workspace.as_deref(),
                    branch.as_deref(),
                    &fetcher,
                    &swl,
                )?,
            };
            if let Some(dir) = &started.cd {
                hook::write_cd_file("swl start", dir);
            }
        }
        Commands::Env {
            workspace,
            workspace_name,
            force,
        } => {
            let config = Config::load(config_flag)?;
            let workspace = workspace.or(workspace_name);
            let ws = Workspace::scan(&workspace_location::resolve(
                &config,
                &std::env::current_dir()?,
                workspace.as_deref(),
            )?)?;
            env::run(&ws, &config, &swl_exe()?, force)?;
        }
        Commands::Status {
            workspace_name,
            workspace,
            from,
            no_fetch,
        } => {
            let config = Config::load(config_flag)?;
            let workspace = workspace.or(workspace_name);
            let fetcher = ws_add::Fetcher::new(!no_fetch);
            let reader = forge::ForgeReader::from_env();
            match from {
                Some(from) => status::run_from(&config, &from, &reader, &fetcher)?,
                None => status::run(
                    &Workspace::scan(&workspace_location::resolve(
                        &config,
                        &std::env::current_dir()?,
                        workspace.as_deref(),
                    )?)?,
                    &config,
                    &reader,
                    &fetcher,
                )?,
            }
        }
        Commands::Ls { no_fetch } => {
            ls::run(&Config::load(config_flag)?, no_fetch)?;
        }
        Commands::Land {
            workspace_name,
            dry_run,
            keep,
            workspace,
            from,
            timeout,
            interval,
        } => {
            let config = Config::load(config_flag)?;
            let workspace = workspace.or(workspace_name);
            let workspace = if from.is_none() {
                Some(workspace_location::resolve(
                    &config,
                    &std::env::current_dir()?,
                    workspace.as_deref(),
                )?)
            } else {
                workspace
            };
            let opts = land::Options {
                dry_run,
                keep,
                timeout: Duration::from_secs(timeout),
                interval: Duration::from_secs(interval),
            };
            let reader = forge::ForgeReader::from_env();
            let writer: Box<dyn forge::ForgeWriter> = if dry_run {
                Box::new(land::DryRunWriter)
            } else {
                Box::new(forge::FjWriter::from_env()?)
            };
            let writer = writer.as_ref();
            let code = match from {
                Some(from) => land::run_from(
                    &config,
                    &std::env::current_dir().context("current directory")?,
                    &from,
                    workspace.as_deref(),
                    &reader,
                    writer,
                    &swl_exe()?,
                    &opts,
                ),
                None => land::run(
                    workspace.as_deref().expect("resolved above without --from"),
                    &config,
                    &reader,
                    writer,
                    &opts,
                ),
            }?;
            if code == ExitCode::SUCCESS && !dry_run {
                views::quietly("swl land", &config);
            }
            return Ok(code);
        }
        Commands::Refresh {
            workspace_name,
            dry_run,
            workspace,
            timeout,
            interval,
        } => {
            let config = Config::load(config_flag)?;
            let workspace = workspace.or(workspace_name);
            let opts = land::Options {
                dry_run,
                keep: false,
                timeout: Duration::from_secs(timeout),
                interval: Duration::from_secs(interval),
            };
            return refresh::run(
                &workspace_location::resolve(
                    &config,
                    &std::env::current_dir()?,
                    workspace.as_deref(),
                )?,
                &config,
                &forge::ForgeReader::from_env(),
                &opts,
            );
        }
        Commands::Bump {
            dry_run,
            check,
            timeout,
            interval,
        } => {
            let config = Config::load(config_flag)?;
            let opts = bump::Options {
                land: land::Options {
                    dry_run,
                    keep: false,
                    timeout: Duration::from_secs(timeout),
                    interval: Duration::from_secs(interval),
                },
                check,
            };
            let writer: Box<dyn forge::ForgeWriter> = if dry_run {
                Box::new(land::DryRunWriter)
            } else {
                Box::new(forge::FjWriter::from_env()?)
            };
            let code = bump::run(
                &config,
                &forge::ForgeReader::from_env(),
                writer.as_ref(),
                &ancestry::RemoteHistory::from_env(),
                &opts,
            )?;
            if code == ExitCode::SUCCESS && !dry_run {
                views::quietly("swl bump", &config);
            }
            return Ok(code);
        }
        Commands::Views { no_fetch } => {
            return views::run(
                &Config::load(config_flag)?,
                &ws_add::Fetcher::new(!no_fetch),
            );
        }
        Commands::Promote {
            rev,
            dry_run,
            timeout,
            interval,
        } => {
            let config = Config::load(config_flag)?;
            let opts = land::Options {
                dry_run,
                keep: false,
                timeout: Duration::from_secs(timeout),
                interval: Duration::from_secs(interval),
            };
            return promote::run(&config, rev.as_deref(), &opts);
        }
        Commands::Publish {
            consumer,
            workspace,
        } => {
            let config = Config::load(config_flag)?;
            let ws = Workspace::scan(&workspace_location::resolve(
                &config,
                &std::env::current_dir()?,
                workspace.as_deref(),
            )?)?;
            // fj's login, before anything is pushed.
            let writer = forge::FjWriter::from_env()?;
            publish::run(
                &ws,
                &config,
                &consumer,
                &writer,
                &forge::ForgeReader::from_env(),
            )?;
        }
        Commands::Finalize {
            dir,
            wait,
            timeout,
            interval,
        } => {
            let wait = wait.then(|| finalize::Wait {
                timeout: Duration::from_secs(timeout),
                interval: Duration::from_secs(interval),
            });
            return finalize::run(&workspace_dir(dir)?, wait, &AnonApi::from_env());
        }
        Commands::Check { dir, event, base } => {
            let config = Config::load(config_flag)?;
            return check::run(
                &workspace_dir(dir)?,
                event,
                base.as_deref(),
                &config,
                &AnonApi::from_env(),
                &ancestry::RemoteHistory::from_env(),
            );
        }
        Commands::Ci {
            command: CiCommands::Env { dir },
        } => {
            return ci::run_env(&workspace_dir(dir)?, &AnonApi::from_env(), &swl_exe()?);
        }
        Commands::Ci {
            command:
                CiCommands::Shim {
                    flake,
                    overrides,
                    args,
                },
        } => {
            let pairs = overrides
                .chunks(2)
                .map(|p| (p[0].clone(), p[1].clone()))
                .collect();
            return shim::run_ci(&flake, pairs, args);
        }
        Commands::Hook { shell } => print!("{}", hook::script(shell)),
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
    // SAFETY: first in main, before any thread is started.
    unsafe { package::take() };
    let cli = Cli::parse();
    // The shims stay silent on stderr unless SWL_LOG asks for diagnostics.
    let shim = matches!(
        cli.command,
        Commands::Shim { .. }
            | Commands::Ci {
                command: CiCommands::Shim { .. }
            }
    );
    style::init(cli.color);
    logging::init(shim, style::stderr_colored());
    match run(cli) {
        Ok(code) => code,
        Err(e) => {
            style::tell("swl", style::Tone::Error, format!("{e:#}"));
            ExitCode::from(2)
        }
    }
}

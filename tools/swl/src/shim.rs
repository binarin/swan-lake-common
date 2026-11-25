//! `swl shim`: what the generated `nix`, `nh` and `deploy` wrappers run.

use std::{
    env,
    ffi::{OsStr, OsString},
    fs,
    io::{Read, Write},
    os::unix::{fs::PermissionsExt, process::CommandExt},
    path::{Path, PathBuf},
    process::{Command, ExitCode},
};

use anyhow::{Context, Result, anyhow};
use clap::ValueEnum;

use crate::{
    ci::CI_MARKER,
    config::Config,
    env::{GENERATED_MARKER, store_dir},
    nix_args::{InstallablesCall, NixCall, classify_nix, nh_lock_flag},
    workspace::{Override, Workspace, find_flake_dir},
};

/// Set to a file path to append one line per shim call: the tool, the action
/// taken and the arguments.
pub const LOG_ENV: &str = "SWL_SHIM_LOG";

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum Tool {
    Nix,
    Nh,
    Deploy,
}

impl Tool {
    pub const ALL: [Tool; 3] = [Tool::Nix, Tool::Nh, Tool::Deploy];

    pub fn name(self) -> &'static str {
        match self {
            Tool::Nix => "nix",
            Tool::Nh => "nh",
            Tool::Deploy => "deploy",
        }
    }
}

/// What the shim does with one call.
#[derive(Debug, PartialEq, Eq)]
enum Action {
    /// Not a flake command, or a tool without guards.
    Pass,
    /// Every installable is a store path.
    StorePaths,
    /// The flake is outside the workspace, or no checkout overrides it.
    NoOverrides,
    /// The caller already passes every override and `--no-write-lock-file`.
    Present,
    Override {
        insert_at: usize,
        extra: Vec<String>,
    },
    Refuse(String),
}

impl Action {
    fn log_name(&self) -> &'static str {
        match self {
            Action::Pass => "pass",
            Action::StorePaths => "store-paths",
            Action::NoOverrides => "no-overrides",
            Action::Present => "present",
            Action::Override { .. } => "override",
            Action::Refuse(_) => "refuse",
        }
    }
}

/// `(input path, flake URL)` pairs for `--override-input`.
pub type OverrideArgs = Vec<(String, String)>;

pub fn run(
    tool: Tool,
    workspace: &Path,
    config_flag: Option<&Path>,
    args: Vec<OsString>,
) -> Result<ExitCode> {
    let wrapper_dir = workspace.join(crate::workspace::WRAPPER_DIR);
    let real = find_real_tool(tool.name(), Some(&wrapper_dir))?;
    let text = args_text(&args);
    let cwd = env::current_dir().context("current directory")?;
    let action = match tool {
        Tool::Nix => nix_action(&text, &cwd, &|dirs| {
            workspace_overrides(workspace, config_flag, dirs)
        })?,
        Tool::Nh => match nh_lock_flag(&text) {
            Some(flag) => Action::Refuse(format!(
                "nh {flag} updates flake inputs; in a swl workspace, lock changes are the lander's"
            )),
            None => Action::Pass,
        },
        Tool::Deploy => Action::Pass,
    };
    finish(tool, &real, action, &args, &text)
}

/// The `nix` wrapper of `swl ci env`: the fixed `overrides` for the flake
/// in `flake_dir` only, with the same classification and refusals as the
/// workspace shim. The real nix is the first on PATH without a swl marker.
pub fn run_ci(flake_dir: &Path, overrides: OverrideArgs, args: Vec<OsString>) -> Result<ExitCode> {
    let real = find_real_tool("nix", None)?;
    let flake_dir = flake_dir
        .canonicalize()
        .with_context(|| format!("flake {}", flake_dir.display()))?;
    let text = args_text(&args);
    let cwd = env::current_dir().context("current directory")?;
    let action = nix_action(&text, &cwd, &|dirs| {
        Ok(if dirs.contains(&flake_dir) {
            overrides.clone()
        } else {
            Vec::new()
        })
    })?;
    finish(Tool::Nix, &real, action, &args, &text)
}

fn args_text(args: &[OsString]) -> Vec<String> {
    args.iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect()
}

fn finish(
    tool: Tool,
    real: &Path,
    action: Action,
    args: &[OsString],
    text: &[String],
) -> Result<ExitCode> {
    log_call(tool, &action, text);
    match action {
        Action::Refuse(why) => {
            eprintln!(
                "swl shim: refusing `{} {}`: {why} (run {} directly to bypass)",
                tool.name(),
                text.join(" "),
                real.display()
            );
            Ok(ExitCode::from(1))
        }
        Action::Override { insert_at, extra } => {
            let mut full: Vec<OsString> = args[..insert_at].to_vec();
            full.extend(extra.into_iter().map(OsString::from));
            full.extend(args[insert_at..].iter().cloned());
            Err(exec(real, &full))
        }
        _ => Err(exec(real, args)),
    }
}

fn exec(real: &Path, args: &[OsString]) -> anyhow::Error {
    let mut cmd = Command::new(real);
    cmd.args(args);
    // Keep argv[0] the tool name, as when called from PATH.
    if let Some(name) = real.file_name() {
        cmd.arg0(name);
    }
    tracing::debug!(target: "swl::cmd", "exec {}", crate::logging::command_line(&cmd));
    anyhow!(cmd.exec()).context(format!("running {}", real.display()))
}

/// The workspace overrides for the flakes in `dirs`; the workspace is read
/// only here, so store-path calls stay cheap.
fn workspace_overrides(
    workspace: &Path,
    config_flag: Option<&Path>,
    dirs: &[PathBuf],
) -> Result<OverrideArgs> {
    let ws = Workspace::scan(workspace)?;
    let config = Config::load(config_flag)?;
    Ok(overrides_for_dirs(&ws, &config, dirs)?
        .into_iter()
        .map(|o| (o.input_path.clone(), o.url()))
        .collect())
}

/// Gives the overrides for the local flake directories an installable names.
type OverridesFor<'a> = dyn Fn(&[PathBuf]) -> Result<OverrideArgs> + 'a;

/// The decision for a `nix` call made in `cwd`. `overrides_for` is called
/// only when a flake installable needs it.
fn nix_action(args: &[String], cwd: &Path, overrides_for: &OverridesFor) -> Result<Action> {
    match classify_nix(args) {
        NixCall::Other => Ok(Action::Pass),
        NixCall::LockCommand { flake } => {
            let Some(dir) = flake_ref_dir(&flake, cwd) else {
                return Ok(Action::NoOverrides);
            };
            if overrides_for(&[dir])?.is_empty() {
                return Ok(Action::NoOverrides);
            }
            Ok(Action::Refuse(
                "it writes flake.lock while swl overrides apply; lock changes are the lander's"
                    .into(),
            ))
        }
        NixCall::Installables(call) => installables_action(call, cwd, overrides_for),
    }
}

fn installables_action(
    call: InstallablesCall,
    cwd: &Path,
    overrides_for: &OverridesFor,
) -> Result<Action> {
    if call.not_flake {
        return Ok(Action::Pass);
    }
    let installables = if call.installables.is_empty() {
        if !call.default_installable {
            return Ok(Action::Pass);
        }
        vec![".".to_string()]
    } else {
        call.installables
    };
    let flake_refs: Vec<&String> = installables
        .iter()
        .filter(|i| !is_store_path(i, cwd))
        .collect();
    if flake_refs.is_empty() {
        return Ok(Action::StorePaths);
    }
    let dirs: Vec<PathBuf> = flake_refs
        .iter()
        .filter_map(|r| flake_ref_dir(r, cwd))
        .collect();
    if dirs.is_empty() {
        return Ok(Action::NoOverrides);
    }
    let overrides = overrides_for(&dirs)?;
    if overrides.is_empty() {
        return Ok(Action::NoOverrides);
    }
    if let Some(flag) = call.lock_writing_flags.first() {
        return Ok(Action::Refuse(format!(
            "{flag} writes a lock file while swl overrides apply; lock changes are the lander's"
        )));
    }
    let mut extra = Vec::new();
    for (input_path, url) in overrides {
        if !call.given_overrides.contains(&input_path) {
            extra.extend(["--override-input".to_string(), input_path, url]);
        }
    }
    if !call.no_write_lock_file {
        extra.push("--no-write-lock-file".into());
    }
    if extra.is_empty() {
        return Ok(Action::Present);
    }
    Ok(Action::Override {
        insert_at: call.insert_at,
        extra,
    })
}

/// The union of the overrides of the workspace checkouts among `dirs`.
fn overrides_for_dirs(ws: &Workspace, config: &Config, dirs: &[PathBuf]) -> Result<Vec<Override>> {
    let mut out: Vec<Override> = Vec::new();
    for dir in dirs {
        let Some(checkout) = ws.checkout_at(dir) else {
            continue;
        };
        for o in ws.overrides_for(config, checkout)? {
            if !out.iter().any(|p| p.input_path == o.input_path) {
                out.push(o);
            }
        }
    }
    Ok(out)
}

/// A store path installable, such as `/nix/store/…-x.drv^out` or a `result`
/// symlink into the store.
fn is_store_path(installable: &str, cwd: &Path) -> bool {
    if installable.contains('#') {
        return false;
    }
    let path = installable.split('^').next().unwrap_or(installable);
    if !(path.starts_with('/') || path.starts_with('.')) {
        return false;
    }
    let path = cwd.join(path);
    let resolved = path
        .canonicalize()
        .ok()
        .or_else(|| {
            fs::read_link(&path)
                .ok()
                .map(|t| path.parent().unwrap_or(cwd).join(t))
        })
        .unwrap_or(path);
    resolved.starts_with(store_dir())
}

/// The local flake directory a flake reference names: path references (`.`,
/// `./x`, `/x`), `path:` and `git+file:`; `None` for remote and indirect
/// references.
fn flake_ref_dir(flake_ref: &str, cwd: &Path) -> Option<PathBuf> {
    let flake_ref = flake_ref.split('#').next().unwrap_or("");
    let (path, query) = match flake_ref.split_once('?') {
        Some((p, q)) => (p, Some(q)),
        None => (flake_ref, None),
    };
    let path = if let Some(rest) = path
        .strip_prefix("git+file:")
        .or_else(|| path.strip_prefix("path:"))
    {
        rest.strip_prefix("//").unwrap_or(rest)
    } else if path.starts_with('.') || path.starts_with('/') {
        path
    } else {
        return None;
    };
    let mut dir = cwd.join(path);
    if let Some(sub) = query
        .into_iter()
        .flat_map(|q| q.split('&'))
        .find_map(|kv| kv.strip_prefix("dir="))
    {
        dir.push(sub);
    }
    find_flake_dir(&dir)
}

/// The first `name` on PATH that is neither in `wrapper_dir` nor a swl
/// wrapper: the real tool, for the shims and for the commands that must not
/// go through them.
pub fn find_real_tool(name: &str, wrapper_dir: Option<&Path>) -> Result<PathBuf> {
    let wrapper_dir = wrapper_dir.map(|w| w.canonicalize().unwrap_or_else(|_| w.to_path_buf()));
    let path = env::var_os("PATH").unwrap_or_default();
    for dir in env::split_paths(&path) {
        if dir.as_os_str().is_empty()
            || wrapper_dir
                .as_ref()
                .is_some_and(|w| dir.canonicalize().is_ok_and(|d| &d == w))
        {
            continue;
        }
        let candidate = dir.join(name);
        let executable = fs::metadata(&candidate)
            .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0);
        if executable && !is_swl_wrapper(&candidate) {
            return Ok(candidate);
        }
    }
    Err(match wrapper_dir {
        Some(w) => anyhow!("no {name} on PATH outside {}", w.display()),
        None => anyhow!("no {name} on PATH besides swl wrappers"),
    })
}

fn is_swl_wrapper(path: &Path) -> bool {
    let mut head = [0u8; 256];
    let n = fs::File::open(path)
        .and_then(|mut f| f.read(&mut head))
        .unwrap_or(0);
    [GENERATED_MARKER, CI_MARKER].iter().any(|marker| {
        let marker = marker.as_bytes();
        head[..n].windows(marker.len()).any(|w| w == marker)
    })
}

fn log_call(tool: Tool, action: &Action, args: &[String]) {
    let Some(path) = env::var_os(LOG_ENV).filter(|p| !p.is_empty()) else {
        return;
    };
    let line = format!(
        "{} {} {}\n",
        tool.name(),
        action.log_name(),
        shell_words(args)
    );
    let _ = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .and_then(|mut f| f.write_all(line.as_bytes()));
}

fn shell_words(args: &[String]) -> String {
    args.iter()
        .map(|a| crate::env::shell_quote(OsStr::new(a)))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &str) -> Vec<String> {
        s.split_whitespace().map(str::to_string).collect()
    }

    fn no_workspace(_: &[PathBuf]) -> Result<OverrideArgs> {
        panic!("the workspace must not be loaded for this call")
    }

    #[test]
    fn store_path_calls_do_not_load_the_workspace() {
        let cwd = Path::new("/");
        for s in [
            "build --out-link /tmp/r /nix/store/aaaa-x /nix/store/bbbb-y",
            "--experimental-features nix-command path-info /nix/store/aaaa-x.drv",
            "build /nix/store/aaaa-x.drv^out --print-out-paths --no-link",
            "copy --to ssh://h /nix/store/aaaa-x",
        ] {
            assert_eq!(
                nix_action(&args(s), cwd, &no_workspace).unwrap(),
                Action::StorePaths,
                "{s}"
            );
        }
    }

    #[test]
    fn non_flake_and_remote_calls_pass() {
        let cwd = Path::new("/");
        assert_eq!(
            nix_action(&args("--version"), cwd, &no_workspace).unwrap(),
            Action::Pass
        );
        assert_eq!(
            nix_action(
                &args("eval --raw --expr builtins.currentSystem"),
                cwd,
                &no_workspace
            )
            .unwrap(),
            Action::Pass
        );
        assert_eq!(
            nix_action(
                &args("build nixpkgs#hello github:o/r#x"),
                cwd,
                &no_workspace
            )
            .unwrap(),
            Action::NoOverrides
        );
        assert_eq!(
            nix_action(&args("repl"), cwd, &no_workspace).unwrap(),
            Action::Pass
        );
    }

    #[test]
    fn flake_refs_name_local_directories() {
        let tmp = env::temp_dir().join(format!("swl-flake-ref-{}", std::process::id()));
        fs::create_dir_all(tmp.join("f/sub/deeper")).unwrap();
        fs::create_dir_all(tmp.join("f/.git")).unwrap();
        fs::write(tmp.join("f/flake.nix"), "{}").unwrap();
        fs::write(tmp.join("f/sub/flake.nix"), "{}").unwrap();
        let f = tmp.join("f").canonicalize().unwrap();
        let cwd = f.join("sub/deeper");
        assert_eq!(flake_ref_dir(".", &cwd), Some(f.join("sub")));
        assert_eq!(flake_ref_dir("../..#x", &cwd), Some(f.clone()));
        assert_eq!(
            flake_ref_dir(&format!("git+file://{}?ref=main#x", f.display()), &cwd),
            Some(f.clone())
        );
        assert_eq!(
            flake_ref_dir(&format!("path:{}?dir=sub", f.display()), &cwd),
            Some(f.join("sub"))
        );
        assert_eq!(flake_ref_dir("github:binarin/swan-lake", &cwd), None);
        assert_eq!(flake_ref_dir("nixpkgs#hello", &cwd), None);
        fs::remove_dir_all(&tmp).unwrap();
    }
}

//! `swl hook <shell>`: the shell function `swl`, for interactive shells, and
//! the `SWL_CD_FILE` protocol through which `swl start` tells it where to
//! change to.

use std::{os::unix::ffi::OsStrExt, path::Path};

use crate::style::{self, Tone};

/// The shells `swl hook` prints the function for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum Shell {
    Zsh,
    Bash,
}

/// The file in which a command that leaves a directory to change to (`swl
/// start`) writes its path, when set. The function sets it for each call.
pub const CD_FILE_ENV: &str = "SWL_CD_FILE";

/// The same function for zsh and bash: it runs `swl` from PATH with
/// `SWL_CD_FILE` set to a new temporary file, then removes the file and, when
/// swl succeeded and wrote a directory there, changes to it. It returns swl's
/// exit status, or that of a failed `cd`. When `mktemp` fails it runs swl
/// with `SWL_CD_FILE` empty, which swl takes as unset. The commands that may
/// fail are on the left of `||`, so that `set -e`, zsh's `ERR_RETURN` or an
/// ERR trap in the user's shell do not stop the function before it removes
/// the file.
const FUNCTION: &str = r#"swl() {
  local __swl_cd_file __swl_status=0 __swl_dir=
  __swl_cd_file=$(mktemp "${TMPDIR:-/tmp}/swl-cd.XXXXXXXX") || __swl_cd_file=
  SWL_CD_FILE=$__swl_cd_file command swl "$@" || __swl_status=$?
  if [ -n "$__swl_cd_file" ]; then
    if [ "$__swl_status" -eq 0 ]; then
      __swl_dir=$(<"$__swl_cd_file") || __swl_dir=
    fi
    rm -f -- "$__swl_cd_file"
  fi
  if [ -n "$__swl_dir" ]; then
    builtin cd -- "$__swl_dir" || return
  fi
  return "$__swl_status"
}
"#;

/// What `swl hook <shell>` prints, for `eval "$(swl hook <shell>)"`.
pub fn script(shell: Shell) -> String {
    let name = match shell {
        Shell::Zsh => "zsh",
        Shell::Bash => "bash",
    };
    format!(
        "# The swl function for {name}, from `swl hook {name}`: it runs swl from PATH\n# and, after `swl start`, changes to the new workspace's consumer.\n{FUNCTION}"
    )
}

/// Writes `dir` to `$SWL_CD_FILE` when it is set, for the function to change
/// to. A failure is a warning: the command itself succeeded.
pub fn write_cd_file(command: &str, dir: &Path) {
    let Some(file) = std::env::var_os(CD_FILE_ENV).filter(|f| !f.is_empty()) else {
        return;
    };
    let path = Path::new(&file);
    if let Err(e) = std::fs::write(path, dir.as_os_str().as_bytes()) {
        style::tell(
            command,
            Tone::Warn,
            format!(
                "warning: could not write {} to ${CD_FILE_ENV} ({}): {e}",
                dir.display(),
                path.display()
            ),
        );
    }
}

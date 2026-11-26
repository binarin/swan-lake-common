//! Diagnostics with `tracing`, on stderr, filtered by `SWL_LOG` (an
//! `EnvFilter`: `debug`, `swl::forge=trace`, …). The default is `warn`; the
//! shims log nothing unless `SWL_LOG` is set. External commands are logged
//! here, at debug, with their arguments, exit status and duration; words
//! that look like credentials are scrubbed.

use std::{
    borrow::Cow,
    env,
    io::{self, Read},
    process::{Command, ExitStatus, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

use tracing_subscriber::{EnvFilter, fmt::time::uptime};

/// The filter of the diagnostics.
pub const LOG_ENV: &str = "SWL_LOG";

/// Installs the subscriber, unless `only_when_asked` (the shims) and
/// `SWL_LOG` is not set. A filter that does not parse is a warning, and
/// `warn` is used.
pub fn init(only_when_asked: bool, ansi: bool) {
    let asked = env::var(LOG_ENV).ok().filter(|v| !v.trim().is_empty());
    if only_when_asked && asked.is_none() {
        return;
    }
    let (filter, bad) = match &asked {
        Some(v) => match EnvFilter::try_new(v) {
            Ok(f) => (f, None),
            Err(e) => (EnvFilter::new("warn"), Some((v.clone(), e))),
        },
        None => (EnvFilter::new("warn"), None),
    };
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(io::stderr)
        .with_timer(uptime())
        .with_ansi(ansi)
        .try_init();
    if let Some((v, e)) = bad {
        tracing::warn!("{LOG_ENV}={v:?} is not a filter ({e}); using warn");
    }
}

/// Whether a word looks like it carries a credential: an Authorization
/// header, a `token` value.
fn secret_like(word: &str) -> bool {
    let lower = word.to_ascii_lowercase();
    ["authorization", "token ", "token=", "token:", "bearer "]
        .iter()
        .any(|m| lower.contains(m))
}

/// `word` without credentials: the user info of a URL (`scheme://user:pw@`)
/// and words that look like credentials are replaced.
pub fn scrub(word: &str) -> Cow<'_, str> {
    if secret_like(word) {
        return Cow::Borrowed("<scrubbed>");
    }
    if let Some((scheme, rest)) = word.split_once("://") {
        let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
        if let Some(at) = rest[..authority_end].rfind('@')
            && rest[..at].contains(':')
        {
            return Cow::Owned(format!("{scheme}://<scrubbed>@{}", &rest[at + 1..]));
        }
    }
    Cow::Borrowed(word)
}

/// A word of a command line, quoted (as a Rust string) when it has
/// whitespace, quotes or control characters.
fn quoted(word: &str) -> String {
    if word.is_empty()
        || word
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || matches!(c, '"' | '\'' | '\\'))
    {
        format!("{word:?}")
    } else {
        word.to_string()
    }
}

/// The command line of `cmd` for the log: program and arguments, scrubbed.
/// The argument after one named like a token option (nix's
/// `access-tokens`) is scrubbed too.
pub fn command_line(cmd: &Command) -> String {
    let mut words = vec![quoted(&cmd.get_program().to_string_lossy())];
    let mut hide_next = false;
    for a in cmd.get_args() {
        let a = a.to_string_lossy();
        words.push(if hide_next {
            "<scrubbed>".to_string()
        } else {
            quoted(&scrub(&a))
        });
        let lower = a.to_ascii_lowercase();
        hide_next = !lower.contains([' ', '=', ':'])
            && (lower.ends_with("token") || lower.ends_with("tokens"));
    }
    words.join(" ")
}

/// Logs the end of a command started at `started`.
pub fn finished(line: &str, started: Instant, status: Result<&ExitStatus, &io::Error>) {
    let ms = started.elapsed().as_millis();
    match status {
        Ok(s) => match s.code() {
            Some(exit) => tracing::debug!(target: "swl::cmd", exit, ms, "{line}"),
            None => tracing::debug!(target: "swl::cmd", status = %s, ms, "{line}"),
        },
        Err(e) => tracing::debug!(target: "swl::cmd", error = %e, ms, "{line}"),
    }
}

/// `cmd.output()`, logged.
pub fn output(cmd: &mut Command) -> io::Result<Output> {
    let line = command_line(cmd);
    let started = Instant::now();
    let out = cmd.output();
    finished(&line, started, out.as_ref().map(|o| &o.status));
    out
}

/// `cmd.output()` with stdin null, logged, but `None` when the command has
/// not exited and closed its stdout and stderr within `timeout`; it is then
/// killed.
pub fn output_within(cmd: &mut Command, timeout: Duration) -> io::Result<Option<Output>> {
    let line = command_line(cmd);
    let started = Instant::now();
    let spawned = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();
    let mut child = match spawned {
        Ok(child) => child,
        Err(e) => {
            finished(&line, started, Err(&e));
            return Err(e);
        }
    };
    let stdout = read_all(child.stdout.take());
    let stderr = read_all(child.stderr.take());
    let deadline = started + timeout;
    loop {
        // Read to the end too: a child it left running may hold the pipes.
        let exited = child.try_wait()?;
        if let Some(status) = exited.filter(|_| stdout.is_finished() && stderr.is_finished()) {
            finished(&line, started, Ok(&status));
            return Ok(Some(Output {
                status,
                stdout: stdout.join().unwrap_or_default(),
                stderr: stderr.join().unwrap_or_default(),
            }));
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            tracing::debug!(target: "swl::cmd", ms = started.elapsed().as_millis(), "timed out: {line}");
            return Ok(None);
        }
        thread::sleep(Duration::from_millis(10));
    }
}

/// The bytes a child's pipe gives, read on a thread.
fn read_all<R: Read + Send + 'static>(pipe: Option<R>) -> thread::JoinHandle<Vec<u8>> {
    thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(mut p) = pipe {
            let _ = p.read_to_end(&mut bytes);
        }
        bytes
    })
}

/// `cmd.status()`, logged.
pub fn status(cmd: &mut Command) -> io::Result<ExitStatus> {
    let line = command_line(cmd);
    let started = Instant::now();
    let status = cmd.status();
    finished(&line, started, status.as_ref());
    status
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credentials_are_scrubbed() {
        assert_eq!(scrub("--location"), "--location");
        assert_eq!(scrub("Authorization: token abc"), "<scrubbed>");
        assert_eq!(scrub("header = \"Authorization: token x\""), "<scrubbed>");
        assert_eq!(
            scrub("https://u:secret@example.org/x?y@z"),
            "https://<scrubbed>@example.org/x?y@z"
        );
        // A user without a password is no secret (`ssh://git@host`).
        assert_eq!(
            scrub("git+ssh://forgejo@host/x"),
            "git+ssh://forgejo@host/x"
        );
        let mut c = Command::new("nix");
        c.args([
            "--option",
            "access-tokens",
            "github.com=ghp_x",
            "--header",
            "Authorization: token t",
            "build",
            "Accept: a b",
            "\n%{http_code}",
        ]);
        assert_eq!(
            command_line(&c),
            r#"nix --option access-tokens <scrubbed> --header <scrubbed> build "Accept: a b" "\n%{http_code}""#
        );
    }
}

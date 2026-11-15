use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    env,
    ffi::OsString,
    fs,
    io::{BufRead, BufReader},
    path::{Component, Path, PathBuf},
    process::{Command, ExitCode},
};

use anyhow::{Result, anyhow, bail};
use clap::{Parser, Subcommand};
use serde_json::Value;

#[derive(Parser)]
#[command(
    name = "secretctl",
    version,
    about = "Recipient-policy driven sops encryption for a secrets tree",
    after_help = EXIT_STATUS_HELP
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Encrypt plaintext files and re-key sops files whose recipients differ from the policy
    #[command(after_help = EXIT_STATUS_HELP)]
    Encrypt {
        #[arg(long, help = "Print the planned action per file; never run sops")]
        dry_run: bool,
        #[arg(
            long,
            value_name = "PATH",
            help = "Secrets root directory [default: <git toplevel>/secrets]"
        )]
        root: Option<PathBuf>,
        directory: Option<PathBuf>,
    },
}

const EXIT_STATUS_HELP: &str = "Exit status: 0 = nothing pending (or all actions applied), \
                                1 = --dry-run found pending actions, 2 = error.";
/// Exit status of `encrypt --dry-run` when at least one file needs an action.
const EXIT_PENDING: u8 = 1;
/// Exit status for every error, matching clap's usage-error status.
const EXIT_ERROR: u8 = 2;

/// SOPS_* variables passed through to sops. They only select decryption
/// identities (age keys, ssh keys, the gpg binary) or the order in which they
/// are tried. Every other SOPS_* variable is removed because it could add
/// recipients (SOPS_AGE_RECIPIENTS, SOPS_PGP_FP, SOPS_KMS_ARN, ...), load a
/// different config (SOPS_CONFIG) or route key operations to a remote key
/// service (SOPS_KEYSERVICE). Non-SOPS variables such as GNUPGHOME, HOME,
/// XDG_CONFIG_HOME (default age key file), GPG_AGENT_INFO and PATH (gpg, age
/// plugins) are kept as they are.
const SOPS_ENV_KEPT: [&str; 7] = [
    "SOPS_AGE_KEY",
    "SOPS_AGE_KEY_FILE",
    "SOPS_AGE_KEY_CMD",
    "SOPS_AGE_SSH_PRIVATE_KEY_FILE",
    "SOPS_AGE_SSH_PRIVATE_KEY_CMD",
    "SOPS_GPG_EXEC",
    "SOPS_DECRYPTION_ORDER",
];
/// Master-key kinds that the recipient policy cannot express; a sops file using
/// any of them is rejected rather than silently re-keyed. Together with `age`
/// and `pgp` these are all master-key fields of the sops 3.13 metadata block.
/// SSH keys have no field of their own: sops stores them as age recipients
/// (`ssh-ed25519 ...`) under `age`, where they show up as policy drift.
const UNSUPPORTED_MASTER_KEYS: [&str; 6] = [
    "kms",
    "gcp_kms",
    "azure_kv",
    "hc_vault",
    "hckms",
    "key_groups",
];

fn error(path: &Path, reason: impl std::fmt::Display) -> anyhow::Error {
    anyhow!("{}: {reason}", path.display())
}
fn metadata(path: &Path) -> Result<fs::Metadata> {
    fs::symlink_metadata(path).map_err(|e| error(path, e))
}
fn boundary(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path.join(".git")) {
        Ok(_) => Err(error(path, "nested Git repository/worktree boundary")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(error(path, e)),
    }
}

enum ResolutionStep {
    Component(OsString),
    EndLink(PathBuf),
}

fn resolve(root: &Path, relative: &Path, links: bool) -> Result<PathBuf> {
    let mut pending: VecDeque<ResolutionStep> = relative
        .components()
        .map(|c| ResolutionStep::Component(c.as_os_str().to_owned()))
        .collect();
    let mut current = root.to_owned();
    let mut active_links = BTreeSet::new();
    while let Some(step) = pending.pop_front() {
        let part = match step {
            ResolutionStep::Component(part) => part,
            ResolutionStep::EndLink(path) => {
                active_links.remove(&path);
                continue;
            }
        };
        if part == "." {
            continue;
        }
        if part == ".." {
            if current == root {
                return Err(error(&current, "path escapes secrets root"));
            }
            current.pop();
            continue;
        }
        if Path::new(&part)
            .components()
            .any(|c| matches!(c, Component::RootDir | Component::Prefix(_)))
        {
            return Err(error(&current, "absolute link/path is forbidden"));
        }
        current.push(&part);
        let meta = metadata(&current)?;
        if meta.file_type().is_symlink() {
            if !links {
                return Err(error(&current, "unexpected content symlink"));
            }
            if !active_links.insert(current.clone()) {
                return Err(error(&current, "cyclic marker link chain"));
            }
            let target = fs::read_link(&current).map_err(|e| error(&current, e))?;
            if target.is_absolute() {
                return Err(error(&current, "absolute marker link is forbidden"));
            }
            pending.push_front(ResolutionStep::EndLink(current.clone()));
            current.pop();
            for c in target.components().rev() {
                pending.push_front(ResolutionStep::Component(c.as_os_str().to_owned()));
            }
        } else if meta.is_dir() {
            boundary(&current)?;
        } else if pending
            .iter()
            .any(|s| matches!(s, ResolutionStep::Component(_)))
        {
            return Err(error(&current, "non-directory path component"));
        }
    }
    Ok(current)
}

fn age(value: &str) -> Result<()> {
    if value.bytes().any(|b| b.is_ascii_uppercase()) {
        bail!("age recipient must be lowercase");
    }
    let (hrp, data) = value
        .rsplit_once('1')
        .ok_or_else(|| anyhow!("missing Bech32 separator"))?;
    if hrp == "age1pq" {
        bail!("hybrid age recipients are unsupported");
    }
    if hrp != "age" {
        let name = hrp
            .strip_prefix("age1")
            .ok_or_else(|| anyhow!("invalid age HRP"))?;
        if name.is_empty()
            || !name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"+._-".contains(&b))
        {
            bail!("invalid age plugin name");
        }
    }
    let alphabet = b"qpzry9x8gf2tvdw0s3jn54khce6mua7l";
    let words: Vec<u8> = data
        .bytes()
        .map(|b| {
            alphabet
                .iter()
                .position(|&x| x == b)
                .map(|n| n as u8)
                .ok_or_else(|| anyhow!("invalid Bech32 character"))
        })
        .collect::<std::result::Result<_, _>>()?;
    if words.len() < 6 {
        bail!("missing Bech32 checksum");
    }
    let mut chk = 1u32;
    for v in hrp
        .bytes()
        .map(|b| b >> 5)
        .chain(std::iter::once(0))
        .chain(hrp.bytes().map(|b| b & 31))
        .chain(words.iter().copied())
    {
        let top = chk >> 25;
        chk = ((chk & 0x1ffffff) << 5) ^ u32::from(v);
        for (i, g) in [0x3b6a57b2, 0x26508e6d, 0x1ea119fa, 0x3d4233dd, 0x2a1462b3]
            .iter()
            .enumerate()
        {
            if (top >> i) & 1 != 0 {
                chk ^= g;
            }
        }
    }
    if chk != 1 {
        bail!("invalid Bech32 checksum");
    }
    let payload = &words[..words.len() - 6];
    let bits = payload.len() * 5;
    let remainder = bits % 8;
    if remainder >= 5 || (remainder > 0 && payload.last().unwrap() & ((1 << remainder) - 1) != 0) {
        bail!("invalid Bech32 padding");
    }
    if hrp == "age" && bits / 8 != 32 {
        bail!("native age recipient must encode 32 bytes");
    }
    Ok(())
}

fn recipient(path: &Path, pgp: bool) -> Result<String> {
    let file = fs::File::open(path).map_err(|e| error(path, e))?;
    let mut bytes = Vec::new();
    BufReader::new(file)
        .read_until(b'\n', &mut bytes)
        .map_err(|e| error(path, e))?;
    if bytes.last() == Some(&b'\n') {
        bytes.pop();
        if bytes.last() == Some(&b'\r') {
            bytes.pop();
        }
    }
    let value = std::str::from_utf8(&bytes)
        .map_err(|_| error(path, "non-ASCII recipient"))?
        .trim_matches([' ', '\t']);
    if value.is_empty()
        || !value.is_ascii()
        || value.bytes().any(|b| b.is_ascii_whitespace() || b == 0)
    {
        return Err(error(path, "empty or malformed recipient"));
    }
    if pgp {
        if ![40, 64].contains(&value.len()) || !value.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(error(path, "expected full 40- or 64-digit PGP fingerprint"));
        }
        Ok(format!("pgp:{}", value.to_ascii_uppercase()))
    } else {
        age(value).map_err(|e| error(path, e))?;
        Ok(format!("age:{value}"))
    }
}
fn marker(path: &Path) -> Option<bool> {
    let name = path.file_name()?.as_encoded_bytes();
    if name.ends_with(b".pgp-key") {
        Some(true)
    } else if name.ends_with(b".age-key") {
        Some(false)
    } else {
        None
    }
}
fn entries(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut paths = fs::read_dir(dir)
        .map_err(|e| error(dir, e))?
        .map(|e| e.map(|e| e.path()).map_err(|e| error(dir, e)))
        .collect::<Result<Vec<_>>>()?;
    paths.sort();
    Ok(paths)
}
fn policy(root: &Path, dir: &Path, inherited: &BTreeSet<String>) -> Result<BTreeSet<String>> {
    let mut recipients = inherited.clone();
    for path in entries(dir)? {
        if let Some(pgp) = marker(&path) {
            let meta = metadata(&path)?;
            if meta.file_type().is_symlink() {
                let target = resolve(root, path.strip_prefix(root).unwrap(), true)
                    .map_err(|e| error(&path, e))?;
                if !metadata(&target)?.is_file() {
                    return Err(error(&path, "marker target is not a regular file"));
                }
                recipients.insert(recipient(&target, pgp)?);
            }
        }
    }
    Ok(recipients)
}
fn walk(
    root: &Path,
    dir: &Path,
    inherited: &BTreeSet<String>,
    output: &mut Vec<(PathBuf, BTreeSet<String>)>,
) -> Result<()> {
    let recipients = policy(root, dir, inherited)?;
    for path in entries(dir)? {
        let meta = metadata(&path)?;
        if meta.file_type().is_symlink() {
            if marker(&path).is_some() {
                continue;
            }
            return Err(error(&path, "unexpected content symlink"));
        }
        if meta.is_dir() {
            boundary(&path)?;
            walk(root, &path, &recipients, output)?;
        } else if meta.is_file() {
            if marker(&path).is_some() && path.starts_with(root.join("values")) {
                eprintln!(
                    "secretctl: warning: {}: regular file with a marker name is ignored \
                     as a recipient; a marker must be a symlink to the key file",
                    path.display()
                );
            }
            if marker(&path).is_some()
                || path
                    .file_name()
                    .unwrap()
                    .as_encoded_bytes()
                    .ends_with(b".public")
            {
                continue;
            }
            if recipients.is_empty() {
                return Err(error(&path, "empty effective recipient set"));
            }
            output.push((path, recipients.clone()));
        } else {
            return Err(error(&path, "unsupported filesystem entry"));
        }
    }
    Ok(())
}

/// Recipients currently listed in a sops file: normalised policy form
/// (`age:<recipient>`, `pgp:<UPPERHEX>`) mapped to the value as stored, which
/// is what `sops rotate --rm-*` must be given.
type Recipients = BTreeMap<String, String>;

#[derive(PartialEq)]
enum Action {
    Encrypt,
    Rotate {
        add: Vec<String>,
        remove: Vec<(String, String)>,
    },
    UpToDate,
}
impl Action {
    fn describe(&self) -> String {
        match self {
            Action::Encrypt => "encrypt".to_owned(),
            Action::UpToDate => "ok".to_owned(),
            Action::Rotate { add, remove } => std::iter::once("rotate".to_owned())
                .chain(add.iter().map(|r| format!("+{r}")))
                .chain(remove.iter().map(|(r, _)| format!("-{r}")))
                .collect::<Vec<_>>()
                .join(" "),
        }
    }
}

struct Candidate {
    path: PathBuf,
    policy: BTreeSet<String>,
    action: Action,
}

fn malformed(path: &Path, reason: impl std::fmt::Display) -> anyhow::Error {
    error(path, format!("malformed sops file: {reason}"))
}
fn stored_recipients(
    path: &Path,
    sops: &serde_json::Map<String, Value>,
    kind: &str,
    field: &str,
) -> Result<Vec<String>> {
    match sops.get(kind) {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(entries)) => entries
            .iter()
            .map(|entry| match entry.get(field).and_then(Value::as_str) {
                Some(value) if !value.is_empty() && !value.contains(',') => Ok(value.to_owned()),
                _ => Err(malformed(
                    path,
                    format!("`sops.{kind}` entry without a valid `{field}`"),
                )),
            })
            .collect(),
        Some(_) => Err(malformed(path, format!("`sops.{kind}` is not a list"))),
    }
}
/// Classifies a candidate: `None` means plaintext, otherwise the file is a
/// sops binary-format file (`{"data": ..., "sops": {...}}`) with these
/// recipients. A JSON object with a top-level `sops` key that does not fit is
/// an error, never plaintext. Contents never appear in messages.
fn sops_recipients(path: &Path) -> Result<Option<Recipients>> {
    let bytes = fs::read(path).map_err(|e| error(path, e))?;
    let Ok(Value::Object(document)) = serde_json::from_slice::<Value>(&bytes) else {
        return Ok(None);
    };
    let Some(sops) = document.get("sops") else {
        return Ok(None);
    };
    let Value::Object(sops) = sops else {
        return Err(malformed(path, "`sops` is not an object"));
    };
    if !sops.get("mac").is_some_and(Value::is_string) {
        return Err(malformed(path, "missing string `sops.mac`"));
    }
    for kind in UNSUPPORTED_MASTER_KEYS {
        match sops.get(kind) {
            None | Some(Value::Null) => {}
            Some(Value::Array(entries)) if entries.is_empty() => {}
            Some(_) => {
                return Err(malformed(
                    path,
                    format!("unsupported master key type `sops.{kind}`"),
                ));
            }
        }
    }
    if document.len() != 2 || !document.get("data").is_some_and(Value::is_string) {
        return Err(malformed(
            path,
            "not sops binary format (expected only a string `data` and `sops`)",
        ));
    }
    let mut recipients = Recipients::new();
    for recipient in stored_recipients(path, sops, "age", "recipient")? {
        recipients.insert(format!("age:{recipient}"), recipient);
    }
    for fingerprint in stored_recipients(path, sops, "pgp", "fp")? {
        recipients.insert(
            format!("pgp:{}", fingerprint.to_ascii_uppercase()),
            fingerprint,
        );
    }
    if recipients.is_empty() {
        return Err(malformed(path, "no age or pgp recipients"));
    }
    Ok(Some(recipients))
}
fn plan(path: &Path, policy: &BTreeSet<String>) -> Result<Action> {
    let Some(current) = sops_recipients(path)? else {
        return Ok(Action::Encrypt);
    };
    let add: Vec<String> = policy
        .iter()
        .filter(|r| !current.contains_key(*r))
        .cloned()
        .collect();
    let remove: Vec<(String, String)> = current
        .into_iter()
        .filter(|(r, _)| !policy.contains(r))
        .collect();
    Ok(if add.is_empty() && remove.is_empty() {
        Action::UpToDate
    } else {
        Action::Rotate { add, remove }
    })
}

fn push_list<'a>(args: &mut Vec<String>, flag: &str, values: impl Iterator<Item = &'a str>) {
    let values: Vec<&str> = values.collect();
    if !values.is_empty() {
        args.push(flag.to_owned());
        args.push(values.join(","));
    }
}
/// Values of normalised recipients of one kind, without the `age:`/`pgp:` prefix.
fn of_kind<'a>(
    recipients: impl IntoIterator<Item = &'a String>,
    kind: &'static str,
) -> impl Iterator<Item = &'a str> {
    recipients
        .into_iter()
        .filter_map(move |r| r.strip_prefix(kind))
}
/// The sops invocation for an action. `--config /dev/null` stops sops from
/// searching for a `.sops.yaml` whose creation rules or store settings could
/// change recipients or output format; keys always come from the flags.
fn sops_command(candidate: &Candidate) -> Option<Command> {
    let mut args = vec!["--config".to_owned(), "/dev/null".to_owned()];
    match &candidate.action {
        Action::UpToDate => return None,
        Action::Encrypt => {
            args.extend(["encrypt", "-i"].map(str::to_owned));
            push_list(&mut args, "--age", of_kind(&candidate.policy, "age:"));
            push_list(&mut args, "--pgp", of_kind(&candidate.policy, "pgp:"));
        }
        Action::Rotate { add, remove } => {
            args.extend(["rotate", "-i"].map(str::to_owned));
            push_list(&mut args, "--add-age", of_kind(add, "age:"));
            push_list(&mut args, "--add-pgp", of_kind(add, "pgp:"));
            let stored = |kind: &'static str| {
                remove
                    .iter()
                    .filter(move |(r, _)| r.starts_with(kind))
                    .map(|(_, stored)| stored.as_str())
            };
            push_list(&mut args, "--rm-age", stored("age:"));
            push_list(&mut args, "--rm-pgp", stored("pgp:"));
        }
    }
    args.extend(["--input-type", "binary", "--output-type", "binary"].map(str::to_owned));
    let mut command = Command::new("sops");
    for (key, _) in env::vars_os() {
        let bytes = key.as_encoded_bytes();
        if bytes.starts_with(b"SOPS_") && !SOPS_ENV_KEPT.iter().any(|k| k.as_bytes() == bytes) {
            command.env_remove(&key);
        }
    }
    // Candidate paths are absolute, so they cannot be mistaken for options.
    command.args(args).arg(&candidate.path);
    Some(command)
}

fn real_directory(root: &Path) -> Result<()> {
    let meta = metadata(root)?;
    if !meta.is_dir() || meta.file_type().is_symlink() {
        return Err(error(root, "secrets root must be a real directory"));
    }
    Ok(())
}
fn git_secrets_root(cwd: &Path) -> Result<PathBuf> {
    let mut git = Command::new("git");
    for (key, _) in env::vars_os() {
        if key.as_encoded_bytes().starts_with(b"GIT_") {
            git.env_remove(key);
        }
    }
    let result = git
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .map_err(|e| anyhow!("Git discovery: {e}"))?;
    if !result.status.success() {
        return Err(error(cwd, "not inside a Git checkout/worktree"));
    }
    use std::os::unix::ffi::OsStringExt;
    let mut bytes = result.stdout;
    if bytes.last() == Some(&b'\n') {
        bytes.pop();
    }
    let root = PathBuf::from(OsString::from_vec(bytes)).join("secrets");
    real_directory(&root)?;
    Ok(root)
}
fn run() -> Result<u8> {
    let Commands::Encrypt {
        dry_run,
        root,
        directory: target,
    } = Cli::parse().command;
    let cwd = env::current_dir()?;
    let root = match root {
        Some(root) => {
            let root = cwd.join(root);
            real_directory(&root)?;
            fs::canonicalize(&root).map_err(|e| error(&root, e))?
        }
        None => git_secrets_root(&cwd)?,
    };
    boundary(&root)?;
    let requested = match target {
        Some(p) => cwd.join(p),
        None => {
            if !cwd.starts_with(root.join("values")) {
                return Err(error(
                    &cwd,
                    "implicit target requires a directory inside secrets/values",
                ));
            }
            cwd
        }
    };
    let relative = requested
        .strip_prefix(&root)
        .map_err(|_| error(&requested, "target is outside secrets root"))?;
    let target = resolve(&root, relative, false)?;
    if !metadata(&target)?.is_dir() {
        return Err(error(&target, "target must be a directory"));
    }
    let mut inherited = BTreeSet::new();
    let mut dir = root.clone();
    for component in target.strip_prefix(&root).unwrap().components() {
        inherited = policy(&root, &dir, &inherited)?;
        dir.push(component);
    }
    let mut discovered = Vec::new();
    walk(&root, &target, &inherited, &mut discovered)?;
    discovered.sort_by(|a, b| a.0.cmp(&b.0));
    // Classify everything before printing or running sops, so that any error
    // leaves both the output and the tree untouched.
    let candidates = discovered
        .into_iter()
        .map(|(path, policy)| {
            let action = plan(&path, &policy)?;
            Ok(Candidate {
                path,
                policy,
                action,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let pending = candidates
        .iter()
        .filter(|c| !matches!(c.action, Action::UpToDate))
        .count();
    let display_base = root.parent().unwrap_or(&root);
    let mut applied = 0;
    for candidate in &candidates {
        let display = candidate.path.strip_prefix(display_base).unwrap().display();
        println!(
            "{display}: {}\n  {}",
            candidate.action.describe(),
            candidate
                .policy
                .iter()
                .cloned()
                .collect::<Vec<_>>()
                .join("\n  ")
        );
        if dry_run {
            continue;
        }
        let Some(mut command) = sops_command(candidate) else {
            continue;
        };
        // The file may have changed since classification (e.g. it was
        // encrypted or re-keyed by another process); running the planned sops
        // command on different content could double-encrypt or mis-rotate it.
        let current = plan(&candidate.path, &candidate.policy)?;
        if current != candidate.action {
            bail!(
                "{display}: changed since classification (now: {}); stopped after \
                 {applied} of {pending} action(s), sops was not run for it",
                current.describe()
            );
        }
        std::io::Write::flush(&mut std::io::stdout())?;
        let status = command
            .status()
            .map_err(|e| error(&candidate.path, format!("cannot run sops: {e}")))?;
        if !status.success() {
            bail!(
                "{display}: sops failed ({status}); stopped after {applied} of {pending} \
                 action(s), remaining files were not touched"
            );
        }
        if !matches!(plan(&candidate.path, &candidate.policy)?, Action::UpToDate) {
            bail!(
                "{display}: sops succeeded but the result does not match the recipient \
                 policy; stopped after {applied} of {pending} action(s)"
            );
        }
        applied += 1;
    }
    if dry_run && pending > 0 {
        eprintln!("secretctl: {pending} file(s) need action; run without --dry-run to apply");
        return Ok(EXIT_PENDING);
    }
    Ok(0)
}
fn main() -> ExitCode {
    match run() {
        Ok(code) => ExitCode::from(code),
        Err(e) => {
            eprintln!("secretctl: {e}");
            ExitCode::from(EXIT_ERROR)
        }
    }
}

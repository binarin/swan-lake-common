use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    env,
    ffi::{CString, OsStr, OsString},
    fs,
    io::{BufRead, BufReader, ErrorKind},
    os::unix::{
        ffi::OsStrExt,
        fs::{DirBuilderExt, symlink},
    },
    path::{Component, Path, PathBuf},
    process::{Command, ExitCode, Output},
    sync::atomic::{AtomicI32, Ordering},
    time::{SystemTime, UNIX_EPOCH},
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
    /// Manage machines in the secrets tree
    Machine {
        #[command(subcommand)]
        command: MachineCommands,
    },
}

#[derive(Subcommand)]
enum MachineCommands {
    /// Generate a machine's age identity and ed25519 SSH host key into the secrets tree
    #[command(after_help = MACHINE_ADD_HELP)]
    Add {
        #[arg(
            long,
            value_name = "PATH",
            help = "Secrets root directory inside a Git worktree [default: <git toplevel>/secrets]"
        )]
        root: Option<PathBuf>,
        /// Machine name, matching [a-zA-Z0-9][a-zA-Z0-9_-]* like `provision`
        name: String,
    },
}

const EXIT_STATUS_HELP: &str = "Exit status: 0 = nothing pending (or all actions applied), \
                                1 = --dry-run found pending actions, 2 = error.";
const MACHINE_ADD_HELP: &str = "\
Creates exactly these files below the secrets root:
  keys/machines/<name>.age-key                         public age recipient
  keys/machines/<name>.ssh_host_ed25519-key.public     public SSH host key
  values/machines/<name>/<name>.age-key                marker symlink to the recipient
  values/machines/<name>/openssh/ssh_host_ed25519_key  sops binary SSH host key
  values/roots/<name>.private                          sops binary age identity

Keys are generated in a private /dev/shm directory (tmpfs required, active swap
refused, core dumps disabled); only ciphertext and public files reach the tree,
each written atomically. Recipients come only from the marker policy, as for
`encrypt`. Refuses if any of the files already exists. The new files are
recorded with `git add --intent-to-add`; no content is staged or committed.
If a step fails or the run is interrupted (SIGINT, SIGTERM, SIGHUP), the files,
directories and index entries this run created are removed again.

Exit status: 0 = machine added, 2 = error.";
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

/// Entries of `dir`, or none if it does not exist; it must be a real directory.
fn entries_if_dir(dir: &Path) -> Result<Vec<PathBuf>> {
    match fs::symlink_metadata(dir) {
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(error(dir, e)),
        Ok(meta) if meta.is_dir() => entries(dir),
        Ok(_) => Err(error(dir, "expected a real directory")),
    }
}
/// A machine is `keys/machines/<m>.age-key` plus its marker
/// `values/machines/<m>/<m>.age-key` (as `machine add` creates them); either
/// half without the other is an error.
fn check_machines(root: &Path) -> Result<()> {
    let keys = root.join("keys/machines");
    let values = root.join("values/machines");
    for path in entries_if_dir(&keys)? {
        let file_name = path.file_name().unwrap();
        let Some(machine) = file_name.as_bytes().strip_suffix(b".age-key") else {
            continue;
        };
        let marker = values.join(OsStr::from_bytes(machine)).join(file_name);
        match fs::symlink_metadata(&marker) {
            Ok(meta) if meta.file_type().is_symlink() => {}
            Ok(_) => return Err(error(&marker, "machine marker is not a symlink")),
            Err(e) if matches!(e.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) => {
                return Err(error(
                    &path,
                    format!("machine recipient without marker {}", marker.display()),
                ));
            }
            Err(e) => return Err(error(&marker, e)),
        }
    }
    for path in entries_if_dir(&values)? {
        if !metadata(&path)?.is_dir() {
            continue;
        }
        let mut file_name = path.file_name().unwrap().to_owned();
        file_name.push(".age-key");
        let recipient = keys.join(file_name);
        match fs::symlink_metadata(&recipient) {
            Ok(_) => {}
            Err(e) if matches!(e.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) => {
                return Err(error(
                    &path,
                    format!("machine directory without {}", recipient.display()),
                ));
            }
            Err(e) => return Err(error(&recipient, e)),
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
/// `sops --config /dev/null` with the SOPS_* environment reduced to
/// `SOPS_ENV_KEPT`. `--config /dev/null` stops sops from searching for a
/// `.sops.yaml` whose creation rules or store settings could change recipients
/// or output format; keys always come from the flags.
fn sops() -> Command {
    let mut command = Command::new("sops");
    for (key, _) in env::vars_os() {
        let bytes = key.as_encoded_bytes();
        if bytes.starts_with(b"SOPS_") && !SOPS_ENV_KEPT.iter().any(|k| k.as_bytes() == bytes) {
            command.env_remove(&key);
        }
    }
    command.args(["--config", "/dev/null"]);
    command
}
/// `--age`/`--pgp` arguments of `sops encrypt` for exactly `policy`.
fn encrypt_recipient_args(args: &mut Vec<String>, policy: &BTreeSet<String>) {
    push_list(args, "--age", of_kind(policy, "age:"));
    push_list(args, "--pgp", of_kind(policy, "pgp:"));
}
const SOPS_BINARY_ARGS: [&str; 4] = ["--input-type", "binary", "--output-type", "binary"];
/// The sops invocation for an action.
fn sops_command(candidate: &Candidate) -> Option<Command> {
    let mut args = Vec::new();
    match &candidate.action {
        Action::UpToDate => return None,
        Action::Encrypt => {
            args.extend(["encrypt", "-i"].map(str::to_owned));
            encrypt_recipient_args(&mut args, &candidate.policy);
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
    args.extend(SOPS_BINARY_ARGS.map(str::to_owned));
    let mut command = sops();
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
/// `git` run in `dir` with every GIT_* variable removed, so that only `dir`
/// selects the repository. Pathspecs are literal: paths are passed verbatim.
fn git(dir: &Path) -> Command {
    let mut git = Command::new("git");
    for (key, _) in env::vars_os() {
        if key.as_encoded_bytes().starts_with(b"GIT_") {
            git.env_remove(key);
        }
    }
    git.current_dir(dir).arg("--literal-pathspecs");
    git
}
fn git_secrets_root(cwd: &Path) -> Result<PathBuf> {
    let result = git(cwd)
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
fn secrets_root(cwd: &Path, root: Option<PathBuf>) -> Result<PathBuf> {
    let root = match root {
        Some(root) => {
            let root = cwd.join(root);
            real_directory(&root)?;
            fs::canonicalize(&root).map_err(|e| error(&root, e))?
        }
        None => git_secrets_root(cwd)?,
    };
    boundary(&root)?;
    Ok(root)
}
fn run() -> Result<u8> {
    let cwd = env::current_dir()?;
    match Cli::parse().command {
        Commands::Encrypt {
            dry_run,
            root,
            directory,
        } => encrypt(&cwd, dry_run, root, directory),
        Commands::Machine {
            command: MachineCommands::Add { root, name },
        } => machine_add(&cwd, root, &name),
    }
}
fn encrypt(
    cwd: &Path,
    dry_run: bool,
    root: Option<PathBuf>,
    target: Option<PathBuf>,
) -> Result<u8> {
    let root = secrets_root(cwd, root)?;
    check_machines(&root)?;
    let requested = match target {
        Some(p) => cwd.join(p),
        None => {
            if !cwd.starts_with(root.join("values")) {
                return Err(error(
                    cwd,
                    "implicit target requires a directory inside secrets/values",
                ));
            }
            cwd.to_owned()
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
/// Signal number caught during `machine add`, 0 if none. The handler only
/// stores it; `check_interrupt` turns it into an error between steps and after
/// every child process, so that staging cleanup and rollback run as normal code.
static CAUGHT_SIGNAL: AtomicI32 = AtomicI32::new(0);
extern "C" fn catch_signal(signal: libc::c_int) {
    CAUGHT_SIGNAL.store(signal, Ordering::SeqCst);
}
fn catch_signals() {
    let handler: extern "C" fn(libc::c_int) = catch_signal;
    for signal in [libc::SIGHUP, libc::SIGINT, libc::SIGTERM] {
        // SAFETY: the handler only does an atomic store, which is
        // async-signal-safe. Children get default dispositions back on exec.
        unsafe { libc::signal(signal, handler as libc::sighandler_t) };
    }
}
fn check_interrupt() -> Result<()> {
    match CAUGHT_SIGNAL.load(Ordering::SeqCst) {
        0 => Ok(()),
        signal => bail!("interrupted by signal {signal}"),
    }
}
/// Disables core dumps for this process and its children, like `ulimit -c 0`.
fn disable_core_dumps() -> Result<()> {
    let limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: `limit` is a valid rlimit that setrlimit only reads.
    if unsafe { libc::setrlimit(libc::RLIMIT_CORE, &limit) } != 0 {
        bail!(
            "cannot disable core dumps: {}",
            std::io::Error::last_os_error()
        );
    }
    Ok(())
}
/// Sets the process umask (inherited by children) and returns the previous one.
fn set_umask(mask: libc::mode_t) -> libc::mode_t {
    // SAFETY: umask has no preconditions and cannot fail.
    unsafe { libc::umask(mask) }
}
fn is_tmpfs(path: &Path) -> Result<bool> {
    let c_path = CString::new(path.as_os_str().as_bytes())?;
    let mut stat = std::mem::MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: `c_path` is NUL-terminated and `stat` is writable statfs storage.
    if unsafe { libc::statfs(c_path.as_ptr(), stat.as_mut_ptr()) } != 0 {
        return Err(error(path, std::io::Error::last_os_error()));
    }
    // SAFETY: statfs succeeded, so it initialised `stat`.
    let stat = unsafe { stat.assume_init() };
    Ok(stat.f_type == libc::TMPFS_MAGIC)
}

/// Runs `command` to completion (stdout and stderr are captured unless the
/// caller set them); failure or a caught signal is an error.
fn run_checked(command: &mut Command, what: &str) -> Result<Output> {
    check_interrupt()?;
    let output = command
        .output()
        .map_err(|e| anyhow!("cannot run {what}: {e}"))?;
    check_interrupt()?;
    if !output.status.success() {
        bail!(
            "{what} failed ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(output)
}

fn machine_name(name: &str) -> Result<()> {
    let mut bytes = name.bytes();
    let valid = bytes.next().is_some_and(|b| b.is_ascii_alphanumeric())
        && bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    if !valid {
        bail!("invalid machine name {name:?}: expected [a-zA-Z0-9][a-zA-Z0-9_-]*");
    }
    Ok(())
}

/// The files `machine add` creates, in creation order.
struct MachineFiles {
    /// `keys/machines/<m>.age-key`: the public age recipient.
    recipient: PathBuf,
    /// `keys/machines/<m>.ssh_host_ed25519-key.public`.
    ssh_public: PathBuf,
    /// `values/machines/<m>/<m>.age-key` -> `../../../keys/machines/<m>.age-key`.
    marker: PathBuf,
    /// `values/machines/<m>/openssh/ssh_host_ed25519_key`, sops binary.
    host_key: PathBuf,
    /// `values/roots/<m>.private`, sops binary `age-keygen` output.
    identity: PathBuf,
}
impl MachineFiles {
    fn new(root: &Path, name: &str) -> Self {
        Self {
            recipient: root.join(format!("keys/machines/{name}.age-key")),
            ssh_public: root.join(format!("keys/machines/{name}.ssh_host_ed25519-key.public")),
            marker: root.join(format!("values/machines/{name}/{name}.age-key")),
            host_key: root.join(format!(
                "values/machines/{name}/openssh/ssh_host_ed25519_key"
            )),
            identity: root.join(format!("values/roots/{name}.private")),
        }
    }
    fn all(&self) -> [&Path; 5] {
        [
            &self.recipient,
            &self.ssh_public,
            &self.marker,
            &self.host_key,
            &self.identity,
        ]
    }
}

/// Directories from `root` down to `dir` that do not exist yet, outermost
/// first. The existing ones must be real directories inside the worktree.
fn missing_dirs(root: &Path, dir: &Path) -> Result<Vec<PathBuf>> {
    let mut current = root.to_owned();
    let mut missing = Vec::new();
    for component in dir.strip_prefix(root).unwrap().components() {
        current.push(component);
        if !missing.is_empty() {
            missing.push(current.clone());
            continue;
        }
        match fs::symlink_metadata(&current) {
            Ok(meta) if meta.is_dir() => boundary(&current)?,
            Ok(_) => return Err(error(&current, "expected a real directory")),
            Err(e) if e.kind() == ErrorKind::NotFound => missing.push(current.clone()),
            Err(e) => return Err(error(&current, e)),
        }
    }
    Ok(missing)
}
/// The recipients `encrypt` would give a file directly in `dir`: the markers
/// of `root` and of every directory down to `dir`. Directories that do not
/// exist yet contribute nothing.
fn effective_policy(root: &Path, dir: &Path) -> Result<BTreeSet<String>> {
    let missing = missing_dirs(root, dir)?;
    let mut recipients = policy(root, root, &BTreeSet::new())?;
    let mut current = root.to_owned();
    for component in dir.strip_prefix(root).unwrap().components() {
        current.push(component);
        if missing.contains(&current) {
            break;
        }
        recipients = policy(root, &current, &recipients)?;
    }
    Ok(recipients)
}

/// A private directory in memory-backed /dev/shm for generated private keys,
/// staged the way `provision` stages a decrypted identity. Removed on drop.
struct Staging(PathBuf);
impl Staging {
    fn new() -> Result<Self> {
        let shm = Path::new("/dev/shm");
        if !is_tmpfs(shm)? {
            bail!("/dev/shm must be memory-backed tmpfs");
        }
        // tmpfs can swap; refuse active swap rather than risk a plaintext disk copy.
        let swaps = fs::read_to_string("/proc/swaps").map_err(|e| anyhow!("/proc/swaps: {e}"))?;
        if swaps.lines().count() != 1 {
            bail!("disable swap before generating machine keys");
        }
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos());
        for attempt in 0..100 {
            let dir = shm.join(format!(
                "secretctl-machine-{}-{nanos}-{attempt}",
                std::process::id()
            ));
            match fs::DirBuilder::new().mode(0o700).create(&dir) {
                Ok(()) => return Ok(Self(dir)),
                Err(e) if e.kind() == ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(error(&dir, e)),
            }
        }
        bail!("cannot create a staging directory in /dev/shm");
    }
}
impl Drop for Staging {
    fn drop(&mut self) {
        if let Err(e) = fs::remove_dir_all(&self.0) {
            eprintln!("secretctl: warning: {}: {e}", self.0.display());
        }
    }
}

/// Freshly generated keys: public parts in memory, private parts staged.
struct MachineKeys {
    /// `age1...` recipient of `identity`.
    recipient: String,
    /// Full `age-keygen` output.
    identity: PathBuf,
    ssh_public: Vec<u8>,
    host_key: PathBuf,
}
fn generate_keys(staging: &Staging, name: &str) -> Result<MachineKeys> {
    let identity = staging.0.join("age-identity");
    let host_key = staging.0.join("ssh_host_ed25519_key");
    let previous = set_umask(0o077);
    let generated = (|| {
        run_checked(
            Command::new("age-keygen").arg("-o").arg(&identity),
            "age-keygen",
        )?;
        let output = run_checked(
            Command::new("age-keygen").arg("-y").arg(&identity),
            "age-keygen -y",
        )?;
        run_checked(
            Command::new("ssh-keygen")
                .args(["-q", "-t", "ed25519", "-N", "", "-C"])
                .arg(format!("root@{name}"))
                .arg("-f")
                .arg(&host_key)
                .stdin(std::process::Stdio::null()),
            "ssh-keygen",
        )?;
        Ok::<_, anyhow::Error>(output.stdout)
    })();
    set_umask(previous);
    let recipient = String::from_utf8(generated?)
        .map_err(|_| anyhow!("age-keygen -y printed a non-UTF-8 recipient"))?
        .trim()
        .to_owned();
    age(&recipient).map_err(|e| anyhow!("age-keygen -y: {e}"))?;
    let ssh_public = host_key.with_extension("pub");
    let ssh_public = fs::read(&ssh_public).map_err(|e| error(&ssh_public, e))?;
    Ok(MachineKeys {
        recipient,
        identity,
        ssh_public,
        host_key,
    })
}

/// Everything one `machine add` run created, so that a failure removes
/// exactly that and nothing that existed before.
#[derive(Default)]
struct Created {
    dirs: Vec<PathBuf>,
    /// Includes temporary files that are not renamed into place yet.
    files: Vec<PathBuf>,
    /// Index entries for `MachineFiles` may exist (from `git add`).
    indexed: bool,
}
impl Created {
    /// Undoes the run; returns the problems it could not undo.
    fn rollback(&self, root: &Path, files: &MachineFiles) -> Vec<String> {
        let mut problems = Vec::new();
        if self.indexed {
            let result = git(root)
                .args(["rm", "--cached", "--quiet", "--ignore-unmatch", "--"])
                .args(files.all())
                .output();
            match result {
                Ok(o) if o.status.success() => {}
                Ok(o) => problems.push(format!(
                    "git rm --cached failed: {}",
                    String::from_utf8_lossy(&o.stderr).trim()
                )),
                Err(e) => problems.push(format!("cannot run git rm --cached: {e}")),
            }
        }
        for path in self.files.iter().rev() {
            match fs::remove_file(path) {
                Err(e) if e.kind() != ErrorKind::NotFound => {
                    problems.push(format!("{}: {e}", path.display()))
                }
                _ => {}
            }
        }
        for dir in self.dirs.iter().rev() {
            if let Err(e) = fs::remove_dir(dir) {
                problems.push(format!("{}: {e}", dir.display()));
            }
        }
        problems
    }
}

/// Writes `path` atomically: `fill` writes a temporary file next to it, which
/// is then renamed into place.
fn write_new(
    path: &Path,
    created: &mut Created,
    fill: impl FnOnce(&Path, &mut fs::File) -> Result<()>,
) -> Result<()> {
    let mut temporary = OsString::from(".");
    temporary.push(path.file_name().unwrap());
    temporary.push(format!(".secretctl-{}.tmp", std::process::id()));
    let temporary = path.with_file_name(temporary);
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(|e| error(&temporary, e))?;
    created.files.push(temporary.clone());
    fill(&temporary, &mut file)?;
    file.sync_all().map_err(|e| error(&temporary, e))?;
    fs::rename(&temporary, path).map_err(|e| error(path, e))?;
    *created.files.last_mut().unwrap() = path.to_owned();
    check_interrupt()
}
/// Encrypts the staged `plaintext` for exactly `policy` into `path`; the
/// result is checked with the same classification `encrypt` uses.
fn write_encrypted(
    plaintext: &Path,
    policy: &BTreeSet<String>,
    path: &Path,
    created: &mut Created,
) -> Result<()> {
    if policy.is_empty() {
        return Err(error(path, "empty effective recipient set"));
    }
    write_new(path, created, |temporary, file| {
        let mut args = vec!["encrypt".to_owned()];
        encrypt_recipient_args(&mut args, policy);
        args.extend(SOPS_BINARY_ARGS.map(str::to_owned));
        let stdout = file.try_clone().map_err(|e| error(temporary, e))?;
        // The staged path is absolute, so it cannot be mistaken for an option.
        run_checked(
            sops().args(args).arg(plaintext).stdout(stdout),
            &format!("sops for {}", path.display()),
        )?;
        if plan(temporary, policy)? != Action::UpToDate {
            return Err(error(
                path,
                "sops succeeded but the result does not match the recipient policy",
            ));
        }
        Ok(())
    })
}
fn write_machine(
    root: &Path,
    files: &MachineFiles,
    keys: &MachineKeys,
    name: &str,
    created: &mut Created,
) -> Result<()> {
    for file in files.all() {
        for dir in missing_dirs(root, file.parent().unwrap())? {
            fs::create_dir(&dir).map_err(|e| error(&dir, e))?;
            created.dirs.push(dir);
        }
    }
    write_new(&files.recipient, created, |temporary, file| {
        std::io::Write::write_all(file, format!("{}\n", keys.recipient).as_bytes())
            .map_err(|e| error(temporary, e))
    })?;
    write_new(&files.ssh_public, created, |temporary, file| {
        std::io::Write::write_all(file, &keys.ssh_public).map_err(|e| error(temporary, e))
    })?;
    // A symlink cannot be filled in place: replace the empty temporary file.
    write_new(&files.marker, created, |temporary, _| {
        fs::remove_file(temporary).map_err(|e| error(temporary, e))?;
        symlink(format!("../../../keys/machines/{name}.age-key"), temporary)
            .map_err(|e| error(temporary, e))
    })?;
    // The host key policy now includes the machine through its new marker.
    let host_key_policy = effective_policy(root, files.host_key.parent().unwrap())?;
    if !host_key_policy.contains(&format!("age:{}", keys.recipient)) {
        return Err(error(&files.marker, "new machine marker is not effective"));
    }
    write_encrypted(&keys.host_key, &host_key_policy, &files.host_key, created)?;
    let identity_policy = effective_policy(root, files.identity.parent().unwrap())?;
    write_encrypted(&keys.identity, &identity_policy, &files.identity, created)?;
    created.indexed = true;
    run_checked(
        git(root)
            .args(["add", "--intent-to-add", "--"])
            .args(files.all()),
        "git add --intent-to-add",
    )?;
    Ok(())
}
fn machine_add(cwd: &Path, root: Option<PathBuf>, name: &str) -> Result<u8> {
    machine_name(name)?;
    let root = secrets_root(cwd, root)?;
    let toplevel = git(&root)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .map_err(|e| anyhow!("Git discovery: {e}"))?;
    if !toplevel.status.success() {
        return Err(error(&root, "secrets root is not inside a Git worktree"));
    }
    let display_base = root.parent().unwrap_or(&root).to_owned();
    let shown = |path: &Path| {
        path.strip_prefix(&display_base)
            .unwrap()
            .display()
            .to_string()
    };
    let files = MachineFiles::new(&root, name);
    for file in files.all() {
        missing_dirs(&root, file.parent().unwrap())?;
    }
    // An index entry without a file is also an existing artefact; rollback
    // must only ever drop index entries this run added.
    let indexed = run_checked(
        git(&root)
            .args(["ls-files", "--cached", "-z", "--"])
            .args(files.all()),
        "git ls-files",
    )?;
    let indexed: BTreeSet<PathBuf> = indexed
        .stdout
        .split(|&b| b == 0)
        .filter(|entry| !entry.is_empty())
        .map(|entry| root.join(OsStr::from_bytes(entry)))
        .collect();
    let mut existing = Vec::new();
    for path in files.all() {
        match fs::symlink_metadata(path) {
            Ok(_) => existing.push(shown(path)),
            Err(e) if e.kind() == ErrorKind::NotFound => {
                if indexed.contains(path) {
                    existing.push(format!("{} (only in the Git index)", shown(path)));
                }
            }
            Err(e) => return Err(error(path, e)),
        }
    }
    if !existing.is_empty() {
        bail!(
            "machine {name} already exists; refusing to touch:\n  {}",
            existing.join("\n  ")
        );
    }
    if effective_policy(&root, files.identity.parent().unwrap())?.is_empty() {
        return Err(error(
            files.identity.parent().unwrap(),
            "empty effective recipient set",
        ));
    }
    catch_signals();
    disable_core_dumps()?;
    let staging = Staging::new()?;
    let mut created = Created::default();
    let result = generate_keys(&staging, name)
        .and_then(|keys| write_machine(&root, &files, &keys, name, &mut created));
    drop(staging);
    if let Err(e) = result {
        let problems = created.rollback(&root, &files);
        if problems.is_empty() {
            bail!("{e}\nmachine {name} not added; this run's changes were undone");
        }
        bail!(
            "{e}\nmachine {name} not added; could not undo:\n  {}",
            problems.join("\n  ")
        );
    }
    println!("added machine {name}:");
    for path in files.all() {
        println!("  {}", shown(path));
    }
    println!(
        "The new files are recorded with `git add --intent-to-add`; review, stage and commit them.\n\
         Next steps:\n  \
         1. write machines/{name}.nix\n  \
         2. provision {name} --target-host USER@HOST --dry-run"
    );
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

use std::{
    collections::{BTreeSet, VecDeque},
    env,
    ffi::OsString,
    fs,
    io::{BufRead, BufReader},
    path::{Component, Path, PathBuf},
    process::{Command, ExitCode},
};

use anyhow::{Result, anyhow, bail};
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "secretctl",
    version,
    about = "Discovery-only recipient-policy preview"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    Encrypt {
        #[arg(
            long,
            required = true,
            help = "Discover candidates; do not encrypt or reconcile"
        )]
        dry_run: bool,
        directory: Option<PathBuf>,
    },
}

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
fn run() -> Result<()> {
    let Commands::Encrypt {
        directory: target, ..
    } = Cli::parse().command;
    let cwd = env::current_dir()?;
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
        return Err(error(&cwd, "not inside a Git checkout/worktree"));
    }
    use std::os::unix::ffi::OsStringExt;
    let mut bytes = result.stdout;
    if bytes.last() == Some(&b'\n') {
        bytes.pop();
    }
    let checkout = PathBuf::from(OsString::from_vec(bytes));
    let root = checkout.join("secrets");
    let meta = metadata(&root)?;
    if !meta.is_dir() || meta.file_type().is_symlink() {
        return Err(error(&root, "secrets root must be a real directory"));
    }
    boundary(&root)?;
    let requested = match target {
        Some(p) => {
            if p.is_absolute() {
                p
            } else {
                cwd.join(p)
            }
        }
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
    let mut output = Vec::new();
    walk(&root, &target, &inherited, &mut output)?;
    output.sort_by(|a, b| a.0.cmp(&b.0));
    println!(
        "DISCOVERY ONLY: ciphertext classification and reconciliation checks are not implemented."
    );
    for (path, recipients) in output {
        println!(
            "{}\n  {}",
            path.strip_prefix(&checkout).unwrap().display(),
            recipients.into_iter().collect::<Vec<_>>().join("\n  ")
        );
    }
    Ok(())
}
fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("secretctl: {e}");
            ExitCode::FAILURE
        }
    }
}

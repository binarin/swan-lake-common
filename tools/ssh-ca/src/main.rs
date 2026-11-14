mod agent;
mod issue;
mod policy;
mod targets;

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::fs::OpenOptions;
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::Ordering;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand};
use ssh_key::private::Ed25519Keypair;
use ssh_key::public::{Ed25519PublicKey, KeyData};
use ssh_key::{HashAlg, PublicKey};

use crate::issue::{Request, Signer};
use crate::policy::{CaIdentity, Manifest, Registry};
use crate::targets::{Resolution, Target};

/// Clock skew allowance: certificates start this many seconds in the past.
const BACKDATE_SECS: u64 = 300;
const MIN_VALIDITY_SECS: u64 = 60;
const MAX_VALIDITY_SECS: u64 = 366 * 86400;

#[derive(Parser)]
#[command(
    name = "ssh-ca",
    version,
    about = "Issue OpenSSH user certificates from trusted CAs",
    arg_required_else_help = true
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Generate a fresh in-memory Ed25519 key, certify it and add it to an
    /// existing SSH agent. Nothing is written to persistent storage.
    Session {
        #[command(flatten)]
        common: Common,

        /// Destination agent for the certified identity.
        #[arg(long, value_name = "SOCKET", env = "SSH_AUTH_SOCK")]
        agent: Option<PathBuf>,

        /// How long the agent keeps the identity; at most the remaining
        /// certificate validity, which is also the default.
        #[arg(long, value_name = "DURATION", value_parser = parse_duration)]
        agent_lifetime: Option<u64>,

        /// Keep older certificates from the same CA in the agent. By default
        /// they are removed once the new identity is installed.
        #[arg(long)]
        keep_existing: bool,
    },

    /// Certify an existing public key and write the certificate next to it.
    Sign {
        #[command(flatten)]
        common: Common,

        /// Subject public key (OpenSSH format).
        #[arg(value_name = "PUBLIC_KEY")]
        public_key: PathBuf,

        /// Certificate path; defaults to OpenSSH's `NAME-cert.pub`.
        #[arg(long, short, value_name = "FILE")]
        output: Option<PathBuf>,

        /// Replace an existing certificate file.
        #[arg(long)]
        force: bool,
    },
}

#[derive(Args)]
struct Common {
    /// Key registry exported by swan-lake-common.
    #[arg(long, value_name = "FILE", env = "SSH_CA_REGISTRY")]
    registry: PathBuf,

    /// Registry id of the signing CA.
    #[arg(long, value_name = "ID")]
    ca: String,

    /// Sign through an SSH agent holding the CA identity.
    #[arg(
        long,
        value_name = "SOCKET",
        required_unless_present = "ca_key",
        conflicts_with = "ca_key"
    )]
    ca_agent: Option<PathBuf>,

    /// Sign with a CA private key or FIDO resident-key handle file.
    #[arg(long, value_name = "FILE")]
    ca_key: Option<PathBuf>,

    /// Require exactly this YubiKey to be the one connected.
    #[arg(long, value_name = "SERIAL", requires = "ca_key")]
    yubikey_serial: Option<String>,

    /// Target manifest: which principals each machine/account accepts.
    #[arg(long, value_name = "FILE", conflicts_with = "default_principals")]
    policy: Option<PathBuf>,

    /// Without a manifest, assume every target accepts MACHINE--ACCOUNT.
    #[arg(long)]
    default_principals: bool,

    /// A target account, as MACHINE/ACCOUNT. Repeatable.
    #[arg(long = "target", value_name = "MACHINE/ACCOUNT")]
    targets: Vec<String>,

    /// Every MACHINE with every ACCOUNT of one group, both comma-separated.
    /// Repeatable; groups are expanded separately and never multiplied together.
    /// Each occurrence takes exactly two values, so consecutive pairs of this
    /// list are the groups.
    #[arg(long, num_args = 2, value_names = ["MACHINES", "ACCOUNTS"], action = clap::ArgAction::Append)]
    cartesian: Vec<String>,

    /// Certify exactly these principals, inferring nothing. Repeatable.
    #[arg(
        long = "principal",
        value_name = "NAME",
        conflicts_with_all = ["targets", "cartesian", "default_principals"]
    )]
    principals: Vec<String>,

    /// Accept principals that other machines or accounts also accept.
    #[arg(long)]
    allow_shared_principals: bool,

    /// Certificate validity, such as 1w, 12h or 30m.
    #[arg(long, value_name = "DURATION", default_value = "1w", value_parser = parse_duration)]
    valid_for: u64,

    /// Print the plan and exit: no device, agent or file is touched.
    #[arg(long)]
    plan: bool,

    /// Issue without interactive confirmation.
    #[arg(long, conflicts_with = "plan")]
    yes: bool,
}

fn parse_duration(s: &str) -> Result<u64, String> {
    let (num, unit) = s.split_at(s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len()));
    let n: u64 = num.parse().map_err(|_| format!("invalid duration `{s}`"))?;
    let mult = match unit {
        "s" => 1,
        "m" => 60,
        "h" => 3600,
        "d" => 86400,
        "w" => 7 * 86400,
        _ => {
            return Err(format!(
                "invalid duration `{s}`: use a number with s, m, h, d or w"
            ));
        }
    };
    n.checked_mul(mult)
        .filter(|v| *v > 0)
        .ok_or_else(|| format!("invalid duration `{s}`"))
}

fn format_utc(secs: u64) -> String {
    // Civil-from-days (Howard Hinnant), UTC.
    let days = (secs / 86400) as i64;
    let rem = secs % 86400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02} UTC",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

fn format_duration(secs: u64) -> String {
    let (d, h, m) = (secs / 86400, secs % 86400 / 3600, secs % 3600 / 60);
    match (d, h, m) {
        (0, 0, 0) => format!("{secs}s"),
        (0, 0, m) => format!("{m}m"),
        (0, h, m) => format!("{h}h{m:02}m"),
        (d, h, _) => format!("{d}d{h:02}h"),
    }
}

enum Mode {
    Session {
        agent: PathBuf,
        lifetime: u64,
        keep_existing: bool,
    },
    Sign {
        subject: PublicKey,
        subject_path: PathBuf,
        output: PathBuf,
        force: bool,
    },
}

/// A manifest target accepting a principal, with its server-side options.
type Acceptor = (Target, Option<String>);

struct Plan {
    ca: CaIdentity,
    signer: Signer,
    resolution: Resolution,
    principals: Vec<String>,
    /// For each principal, known manifest targets accepting it beyond the request.
    shared: Vec<(String, Vec<Acceptor>)>,
    manifest: bool,
    request: Request,
    mode: Mode,
    notes: Vec<String>,
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before 1970")
        .as_secs()
}

fn default_cert_path(public_key: &Path) -> PathBuf {
    let s = public_key.to_string_lossy();
    match s.strip_suffix(".pub") {
        Some(stem) => PathBuf::from(format!("{stem}-cert.pub")),
        None => PathBuf::from(format!("{s}-cert.pub")),
    }
}

fn build_plan(common: &Common, mode_args: ModeArgs) -> Result<Plan> {
    let registry = Registry::load(&common.registry)?;
    let ca = registry.ca(&common.ca)?;
    let manifest = common.policy.as_deref().map(Manifest::load).transpose()?;
    if let Some(m) = &manifest
        && !m.trusts_ca(&ca)
    {
        bail!(
            "hosts in the manifest do not trust CA {} ({})",
            ca.id,
            ca.fingerprint()
        );
    }

    let signer = match (&common.ca_agent, &common.ca_key) {
        (Some(socket), None) => Signer::Agent {
            socket: socket.clone(),
        },
        (None, Some(path)) => Signer::KeyFile {
            path: path.clone(),
            yubikey_serial: common.yubikey_serial.clone(),
        },
        _ => bail!("choose exactly one of --ca-agent and --ca-key"),
    };

    let mut notes = Vec::new();
    let requested: BTreeSet<Target>;
    let resolution = if !common.principals.is_empty() {
        requested = BTreeSet::new();
        targets::resolve_raw(&common.principals)?
    } else {
        let groups: Vec<Vec<String>> = common.cartesian.chunks(2).map(<[String]>::to_vec).collect();
        requested = targets::expand(&common.targets, &groups)?;
        if requested.is_empty() {
            bail!("nothing to certify: give --target, --cartesian or --principal");
        }
        match (&manifest, common.default_principals) {
            (Some(m), _) => targets::resolve_manifest(m, &requested)?,
            (None, true) => {
                notes.push(
                    "no manifest: assuming each target accepts MACHINE--ACCOUNT; custom or legacy server policy is unknown"
                        .to_string(),
                );
                targets::resolve_default_principals(&requested)?
            }
            (None, false) => {
                bail!("target mode needs --policy MANIFEST or an explicit --default-principals")
            }
        }
    };
    let principals = resolution.principals();

    let mut shared = Vec::new();
    match &manifest {
        Some(m) => {
            for (principal, accepted) in targets::acceptance(m, &principals) {
                let beyond: Vec<_> = accepted
                    .into_iter()
                    .filter(|(t, _)| !requested.contains(t))
                    .collect();
                if !beyond.is_empty() {
                    shared.push((principal, beyond));
                }
            }
        }
        None if matches!(resolution, Resolution::Raw { .. }) => {
            notes.push("no manifest: which servers accept these principals is unknown".to_string())
        }
        None => {}
    }
    let legacy: Vec<String> = match &resolution {
        Resolution::Targets { grants, .. } => grants
            .iter()
            .filter(|g| g.legacy)
            .map(|g| g.target.to_string())
            .collect(),
        Resolution::Raw { .. } => Vec::new(),
    };
    if (!legacy.is_empty() || !shared.is_empty()) && !common.allow_shared_principals {
        let why = if legacy.is_empty() {
            let names: Vec<&str> = shared.iter().map(|(p, _)| p.as_str()).collect();
            format!(
                "principals {} are also accepted elsewhere",
                names.join(", ")
            )
        } else {
            format!("{} only accept shared legacy principals", legacy.join(", "))
        };
        bail!(
            "{why}; the certificate would authorize more than the requested targets. \
             Review with --plan --allow-shared-principals and pass --allow-shared-principals to accept"
        );
    }

    if !(MIN_VALIDITY_SECS..=MAX_VALIDITY_SECS).contains(&common.valid_for) {
        bail!(
            "--valid-for must be between {} and {}",
            format_duration(MIN_VALIDITY_SECS),
            format_duration(MAX_VALIDITY_SECS)
        );
    }
    let start = now();
    let valid_after = start - BACKDATE_SECS;
    let valid_before = start + common.valid_for;

    let (mode, subject_fp) = match mode_args {
        ModeArgs::Session {
            agent,
            agent_lifetime,
            keep_existing,
        } => {
            let Some(agent) = agent else {
                bail!(
                    "no destination agent: SSH_AUTH_SOCK is unset; start an agent or pass --agent"
                );
            };
            let lifetime = agent_lifetime.unwrap_or(common.valid_for);
            if lifetime > common.valid_for {
                bail!(
                    "--agent-lifetime {} exceeds the certificate validity {}",
                    format_duration(lifetime),
                    format_duration(common.valid_for)
                );
            }
            if Some(&agent) == common.ca_agent.as_ref() {
                notes.push("the destination agent is also the CA agent".to_string());
            }
            (
                Mode::Session {
                    agent,
                    lifetime,
                    keep_existing,
                },
                None,
            )
        }
        ModeArgs::Sign {
            public_key,
            output,
            force,
        } => {
            let subject = PublicKey::read_openssh_file(&public_key).with_context(|| {
                format!("cannot read subject public key {}", public_key.display())
            })?;
            let fp = subject.fingerprint(HashAlg::Sha256).to_string();
            let output = output.unwrap_or_else(|| default_cert_path(&public_key));
            if output.exists() && !force {
                bail!("{} exists; pass --force to replace it", output.display());
            }
            (
                Mode::Sign {
                    subject,
                    subject_path: public_key,
                    output,
                    force,
                },
                Some(fp),
            )
        }
    };

    let key_id = format!(
        "ssh-ca:{}:{}:{}",
        ca.id,
        subject_fp.as_deref().unwrap_or("session"),
        valid_after
    );
    Ok(Plan {
        ca,
        signer,
        resolution,
        request: Request {
            key_id,
            principals: principals.clone(),
            valid_after,
            valid_before,
        },
        principals,
        shared,
        manifest: manifest.is_some(),
        mode,
        notes,
    })
}

enum ModeArgs {
    Session {
        agent: Option<PathBuf>,
        agent_lifetime: Option<u64>,
        keep_existing: bool,
    },
    Sign {
        public_key: PathBuf,
        output: Option<PathBuf>,
        force: bool,
    },
}

fn render(plan: &Plan, subject_fp: Option<&str>) -> String {
    let mut s = String::new();
    let w = &mut s;
    let _ = writeln!(w, "CA:          {} {}", plan.ca.id, plan.ca.fingerprint());
    let _ = writeln!(w, "Signer:      {}", plan.signer.describe());
    match (&plan.mode, subject_fp) {
        (Mode::Sign { subject_path, .. }, Some(fp)) => {
            let _ = writeln!(w, "Subject:     {fp} ({})", subject_path.display());
        }
        (Mode::Session { .. }, Some(fp)) => {
            let _ = writeln!(w, "Subject:     {fp} (fresh in-memory Ed25519 key)");
        }
        _ => {
            let _ = writeln!(
                w,
                "Subject:     fresh in-memory Ed25519 key, generated at issuance"
            );
        }
    }
    match &plan.resolution {
        Resolution::Raw { .. } => {
            let _ = writeln!(w, "Targets:     none inferred (explicit principals)");
        }
        Resolution::Targets {
            grants,
            from_manifest,
        } => {
            let _ = writeln!(
                w,
                "Targets:     {} ({})",
                grants.len(),
                if *from_manifest {
                    "manifest"
                } else {
                    "default principals"
                }
            );
            for g in grants {
                let _ = write!(w, "  {:<32} -> {}", g.target.to_string(), g.principal);
                if g.legacy {
                    let _ = write!(w, "  [shared legacy principal]");
                }
                if let Some(o) = &g.options {
                    let _ = write!(w, "  [server options: {o}]");
                }
                let _ = writeln!(w);
            }
        }
    }
    let _ = writeln!(w, "Principals:  {}", plan.principals.join(","));
    if !plan.shared.is_empty() {
        let _ = writeln!(
            w,
            "Also accepted by (from the manifest; unlisted hosts may accept them too):"
        );
        for (p, targets) in &plan.shared {
            let list: Vec<String> = targets
                .iter()
                .map(|(t, o)| match o {
                    Some(o) => format!("{t} [{o}]"),
                    None => t.to_string(),
                })
                .collect();
            let _ = writeln!(w, "  {p}: {}", list.join(", "));
        }
    } else if plan.manifest {
        let _ = writeln!(
            w,
            "Sharing:     no other manifest target accepts these principals"
        );
    }
    let r = &plan.request;
    let _ = writeln!(
        w,
        "Valid:       {} .. {} ({})",
        format_utc(r.valid_after),
        format_utc(r.valid_before),
        format_duration(r.valid_before - r.valid_after)
    );
    let _ = writeln!(w, "Key id:      {}", r.key_id);
    match &plan.mode {
        Mode::Session {
            agent,
            lifetime,
            keep_existing,
        } => {
            let _ = writeln!(w, "Agent:       {}", agent.display());
            let _ = writeln!(w, "Agent TTL:   {}", format_duration(*lifetime));
            let _ = writeln!(
                w,
                "Replaces:    {}",
                if *keep_existing {
                    "nothing (--keep-existing)"
                } else {
                    "older certificates from this CA in the agent, after the new one is added"
                }
            );
        }
        Mode::Sign { output, force, .. } => {
            let _ = writeln!(
                w,
                "Output:      {}{}",
                output.display(),
                if *force { " (replacing)" } else { "" }
            );
        }
    }
    for n in &plan.notes {
        let _ = writeln!(w, "Note:        {n}");
    }
    s
}

fn confirm() -> Result<()> {
    let tty = OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")
        .context("confirmation needs a terminal; pass --yes to issue non-interactively")?;
    let mut out = tty.try_clone()?;
    write!(out, "Issue this certificate? [y/N] ")?;
    out.flush()?;
    let mut line = String::new();
    BufReader::new(tty).read_line(&mut line)?;
    if !matches!(line.trim(), "y" | "Y" | "yes") {
        bail!("cancelled; nothing was issued");
    }
    Ok(())
}

fn run(cli: Cli) -> Result<()> {
    let (common, mode_args) = match cli.command {
        Command::Session {
            common,
            agent,
            agent_lifetime,
            keep_existing,
        } => (
            common,
            ModeArgs::Session {
                agent,
                agent_lifetime,
                keep_existing,
            },
        ),
        Command::Sign {
            common,
            public_key,
            output,
            force,
        } => (
            common,
            ModeArgs::Sign {
                public_key,
                output,
                force,
            },
        ),
    };
    let plan = build_plan(&common, mode_args)?;

    if common.plan {
        print!("{}", render(&plan, plan_subject_fp(&plan).as_deref()));
        println!("Not checked in plan mode: CA availability, device presence, destination agent.");
        return Ok(());
    }

    // The session key exists only from here on, in this process.
    let session_key = match plan.mode {
        Mode::Session { .. } => Some(Ed25519Keypair::random(&mut rand_core::OsRng)),
        Mode::Sign { .. } => None,
    };
    let subject = match (&plan.mode, &session_key) {
        (Mode::Sign { subject, .. }, _) => subject.clone(),
        (Mode::Session { .. }, Some(k)) => PublicKey::new(
            KeyData::Ed25519(Ed25519PublicKey(k.public.0)),
            "ssh-ca session",
        ),
        _ => unreachable!(),
    };
    let fp = subject.fingerprint(HashAlg::Sha256).to_string();
    let mut request = plan.request.clone();
    if matches!(plan.mode, Mode::Session { .. }) {
        request.key_id = request.key_id.replace(":session:", &format!(":{fp}:"));
    }
    let shown = Plan {
        request: request.clone(),
        ..plan
    };
    eprint!("{}", render(&shown, Some(&fp)));
    if !common.yes {
        confirm()?;
    }

    shown.signer.preflight(&shown.ca)?;
    if let Mode::Session { agent, .. } = &shown.mode {
        agent::probe(agent)?;
    }

    let staging = issue::staging_dir()?;
    let cert = issue::issue(staging.path(), &shown.signer, &shown.ca, &subject, &request)?;

    match &shown.mode {
        Mode::Sign { output, force, .. } => {
            let mut opts = OpenOptions::new();
            opts.write(true).mode(0o644);
            if *force {
                opts.create(true).truncate(true);
            } else {
                opts.create_new(true);
            }
            let mut f = opts
                .open(output)
                .with_context(|| format!("cannot write {}", output.display()))?;
            f.write_all((cert.to_openssh()? + "\n").as_bytes())?;
            eprintln!("Wrote {}", output.display());
        }
        Mode::Session {
            agent,
            lifetime,
            keep_existing,
        } => {
            let remaining = request.valid_before.saturating_sub(now());
            let ttl = (*lifetime).min(remaining);
            let ttl = u32::try_from(ttl).context("agent lifetime too large")?;
            let key = session_key.expect("session key");
            agent::install(agent, &cert, key, &request.key_id, ttl)?;
            eprintln!(
                "Added the certified identity to {} for {} (certificate valid until {})",
                agent.display(),
                format_duration(u64::from(ttl)),
                format_utc(request.valid_before)
            );
            if !*keep_existing {
                match agent::remove_superseded(agent, shown.ca.public_key.key_data(), &cert) {
                    Ok(removed) => {
                        for id in &removed {
                            eprintln!("Removed older certificate from {}: {id}", shown.ca.id);
                        }
                    }
                    Err(e) => eprintln!(
                        "ssh-ca: warning: the new identity is installed, but older certificates were not all removed: {e:#}"
                    ),
                }
            }
        }
    }
    drop(staging);
    Ok(())
}

fn plan_subject_fp(plan: &Plan) -> Option<String> {
    match &plan.mode {
        Mode::Sign { subject, .. } => Some(subject.fingerprint(HashAlg::Sha256).to_string()),
        Mode::Session { .. } => None,
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    // Interrupting the confirmation prompt exits immediately; while ssh-keygen
    // runs the child gets the signal and its failure unwinds normally, which
    // removes the staging directory.
    let _ = ctrlc::set_handler(|| {
        if !issue::CHILD_RUNNING.load(Ordering::SeqCst) {
            std::process::exit(130);
        }
    });
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            let _ = writeln!(io::stderr(), "ssh-ca: {e:#}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations() {
        assert_eq!(parse_duration("1w"), Ok(604_800));
        assert_eq!(parse_duration("90m"), Ok(5400));
        assert!(parse_duration("0h").is_err());
        assert!(parse_duration("5").is_err());
        assert!(parse_duration("h").is_err());
        assert!(parse_duration("1y").is_err());
    }

    #[test]
    fn utc_format() {
        assert_eq!(format_utc(0), "1970-01-01 00:00:00 UTC");
        assert_eq!(format_utc(1_709_210_096), "2024-02-29 12:34:56 UTC");
    }

    #[test]
    fn cert_path() {
        assert_eq!(
            default_cert_path(Path::new("/k/id_ed25519.pub")),
            PathBuf::from("/k/id_ed25519-cert.pub")
        );
        assert_eq!(
            default_cert_path(Path::new("key")),
            PathBuf::from("key-cert.pub")
        );
    }
}

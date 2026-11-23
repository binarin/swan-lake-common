//! Classifying `nix` and `nh` command lines for the shim, without touching
//! the filesystem.

/// What a `nix` command line does, as far as overrides are concerned.
#[derive(Debug, PartialEq, Eq)]
pub enum NixCall {
    /// Not a command that evaluates a flake: pass it through.
    Other,
    /// `nix flake lock` or `nix flake update`, which write the lock of
    /// `flake` (`.` when not given).
    LockCommand { flake: String },
    /// A command that takes installables and the flake lock flags.
    Installables(InstallablesCall),
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct InstallablesCall {
    /// Index after the subcommand words, where overrides are inserted.
    pub insert_at: usize,
    /// Positional installables; when empty, the command uses `.` unless
    /// `default_installable` is false.
    pub installables: Vec<String>,
    pub default_installable: bool,
    /// `--expr` or `--file`: installables are not flakes.
    pub not_flake: bool,
    /// Lock-writing flags given, e.g. `--commit-lock-file`.
    pub lock_writing_flags: Vec<String>,
    /// Input paths the caller already overrides with `--override-input`.
    pub given_overrides: Vec<String>,
    pub no_write_lock_file: bool,
}

/// Flags that make Nix write a lock file.
const LOCK_WRITING_FLAGS: &[&str] = &[
    "--commit-lock-file",
    "--recreate-lock-file",
    "--update-input",
    "--output-lock-file",
];

/// Number of values an option takes, for options before or after the
/// subcommand; unknown options are taken as flags.
fn option_arity(subcommand: &str, opt: &str) -> usize {
    let env_command = matches!(subcommand, "shell" | "develop" | "run" | "env shell");
    match opt {
        "--option" | "--override-input" | "--override-flake" | "--arg" | "--argstr"
        | "--arg-from-file" | "--redirect" | "--set-env-var" => 2,
        "-s" if env_command => 2,
        "--update-input"
        | "--output-lock-file"
        | "--reference-lock-file"
        | "--inputs-from"
        | "--expr"
        | "--file"
        | "-f"
        | "--apply"
        | "--out-link"
        | "-o"
        | "--profile"
        | "--log-format"
        | "--store"
        | "--eval-store"
        | "--to"
        | "--from"
        | "--include"
        | "-I"
        | "--max-jobs"
        | "-j"
        | "--cores"
        | "--builders"
        | "--system"
        | "--extra-experimental-features"
        | "--experimental-features"
        | "--substituters"
        | "--extra-substituters"
        | "--trusted-public-keys"
        | "--extra-trusted-public-keys"
        | "--arg-from-stdin"
        | "--phase"
        | "--write-to"
        | "--json-format"
        | "--keep-env-var"
        | "--unset-env-var"
        | "--exclude"
        | "-e"
        | "--flake"
        | "--log-lines"
        | "--timeout"
        | "--max-silent-time"
        | "--system-features"
        | "--extra-system-features" => 1,
        "-k" | "-u" if env_command => 1,
        _ => 0,
    }
}

/// Subcommands taking installables, as the number of words they span.
fn installables_subcommand(args: &[String], i: usize) -> Option<(&'static str, usize)> {
    let next = args.get(i + 1).map(String::as_str);
    Some(match (args[i].as_str(), next) {
        ("build", _) => ("build", 1),
        ("eval", _) => ("eval", 1),
        ("develop", _) => ("develop", 1),
        ("print-dev-env", _) => ("print-dev-env", 1),
        ("run", _) => ("run", 1),
        ("shell", _) => ("shell", 1),
        ("path-info", _) => ("path-info", 1),
        ("log", _) => ("log", 1),
        ("why-depends", _) => ("why-depends", 1),
        ("copy", _) => ("copy", 1),
        ("repl", _) => ("repl", 1),
        ("bundle", _) => ("bundle", 1),
        ("search", _) => ("search", 1),
        ("edit", _) => ("edit", 1),
        ("env", Some("shell")) => ("env shell", 2),
        ("derivation", Some("show")) => ("derivation show", 2),
        ("flake", Some("check")) => ("flake check", 2),
        ("flake", Some("show")) => ("flake show", 2),
        ("flake", Some("archive")) => ("flake archive", 2),
        ("flake", Some("metadata")) => ("flake metadata", 2),
        ("flake", Some("info")) => ("flake info", 2),
        ("flake", Some("prefetch-inputs")) => ("flake prefetch-inputs", 2),
        ("flake", Some("lock")) => ("flake lock", 2),
        ("flake", Some("update")) => ("flake update", 2),
        _ => return None,
    })
}

const NIX_TOP_COMMANDS: &[&str] = &[
    "build",
    "eval",
    "develop",
    "print-dev-env",
    "run",
    "shell",
    "path-info",
    "log",
    "why-depends",
    "copy",
    "repl",
    "bundle",
    "search",
    "edit",
    "env",
    "derivation",
    "flake",
    "help",
    "help-stores",
    "profile",
    "fmt",
    "formatter",
    "registry",
    "config",
    "daemon",
    "hash",
    "key",
    "nar",
    "realisation",
    "store",
    "upgrade-nix",
];

pub fn classify_nix(args: &[String]) -> NixCall {
    // Find the subcommand: skip global options with their values, and words
    // that are no known subcommand (values of options not in the table).
    let mut i = 0;
    let found = loop {
        let Some(word) = args.get(i) else {
            return NixCall::Other;
        };
        if word == "--" {
            return NixCall::Other;
        }
        if word.starts_with('-') {
            i += 1 + option_arity("", word);
        } else if NIX_TOP_COMMANDS.contains(&word.as_str()) {
            break installables_subcommand(args, i);
        } else {
            i += 1;
        }
    };
    let Some((subcommand, words)) = found else {
        return NixCall::Other;
    };
    let insert_at = i + words;
    let mut call = InstallablesCall {
        insert_at,
        default_installable: !matches!(subcommand, "repl" | "search"),
        ..Default::default()
    };
    let mut flake_flag = None;
    let mut j = insert_at;
    while j < args.len() {
        let word = args[j].as_str();
        if word == "--" {
            if subcommand != "run" {
                call.installables.extend(args[j + 1..].iter().cloned());
            }
            break;
        }
        if !word.starts_with('-') || word == "-" {
            call.installables.push(word.to_string());
            // `nix run INSTALLABLE ARGS...`: the rest belongs to the program.
            if subcommand == "run" {
                break;
            }
            j += 1;
            continue;
        }
        let arity = option_arity(subcommand, word);
        let value = |k: usize| args.get(j + k).cloned().unwrap_or_default();
        match word {
            "--command" | "-c" => break,
            "--expr" | "--file" | "-f" => call.not_flake = true,
            "--override-input" => call.given_overrides.push(value(1)),
            "--no-write-lock-file" => call.no_write_lock_file = true,
            "--flake" => flake_flag = Some(value(1)),
            w if LOCK_WRITING_FLAGS.contains(&w) => call.lock_writing_flags.push(w.to_string()),
            _ => {}
        }
        j += 1 + arity;
    }
    match subcommand {
        "flake lock" => NixCall::LockCommand {
            flake: call
                .installables
                .first()
                .cloned()
                .unwrap_or_else(|| ".".into()),
        },
        // `nix flake update [INPUT...] [--flake FLAKE]`.
        "flake update" => NixCall::LockCommand {
            flake: flake_flag.unwrap_or_else(|| ".".into()),
        },
        _ => NixCall::Installables(call),
    }
}

/// The first nh lock-update flag in `args`, if any: nh 4.4.2's `-u/--update`,
/// `-U/--update-input`, `--recreate-lock-file` and `--commit-lock-file`.
/// Arguments after `--` go to `nix build`, which the nix shim guards.
pub fn nh_lock_flag(args: &[String]) -> Option<String> {
    for word in args {
        if word == "--" {
            return None;
        }
        let long_refused = matches!(
            word.as_str(),
            "--update" | "--update-input" | "--recreate-lock-file" | "--commit-lock-file"
        ) || word.starts_with("--update-input=");
        if long_refused {
            return Some(word.clone());
        }
        if let Some(cluster) = word.strip_prefix('-').filter(|w| !w.starts_with('-')) {
            for c in cluster.chars() {
                match c {
                    'u' | 'U' => return Some(word.clone()),
                    // Short options with a value: the rest of the word is it.
                    'b' | 'c' | 'd' | 'e' | 'f' | 'H' | 'I' | 'j' | 'k' | 'K' | 'l' | 'o' | 'P'
                    | 's' => break,
                    _ => {}
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &str) -> Vec<String> {
        s.split_whitespace().map(str::to_string).collect()
    }

    fn installables(s: &str) -> InstallablesCall {
        match classify_nix(&args(s)) {
            NixCall::Installables(c) => c,
            other => panic!("{s}: {other:?}"),
        }
    }

    #[test]
    fn plain_commands() {
        let c = installables(
            "eval --raw .#nixosConfigurations.focalor.config.system.build.toplevel.drvPath",
        );
        assert_eq!(c.insert_at, 1);
        assert_eq!(
            c.installables,
            [".#nixosConfigurations.focalor.config.system.build.toplevel.drvPath"]
        );
        let c = installables("build");
        assert!(c.installables.is_empty() && c.default_installable);
        assert!(!installables("repl").default_installable);
    }

    #[test]
    fn global_options_before_the_subcommand() {
        // deploy-rs and nom.
        let c = installables("--experimental-features nix-command path-info /nix/store/abc-x.drv");
        assert_eq!(
            (c.insert_at, c.installables.as_slice()),
            (3, &["/nix/store/abc-x.drv".to_string()][..])
        );
        let c = installables("--option eval-cache false --log-format bar build .#x");
        assert_eq!(c.insert_at, 6);
        let c = installables(
            "eval --extra-experimental-features nix-command --impure --raw --expr builtins.currentSystem",
        );
        assert!(c.not_flake);
    }

    #[test]
    fn option_values_are_not_installables() {
        let c = installables("build --out-link /tmp/gcroot /nix/store/abc-x --print-out-paths");
        assert_eq!(c.installables, ["/nix/store/abc-x"]);
        let c = installables("eval --json .#deploy --apply f");
        assert_eq!(c.installables, [".#deploy"]);
        let c = installables(
            "print-dev-env --profile /p . --override-input common git+file:///ws/c --no-write-lock-file",
        );
        assert_eq!(c.installables, ["."]);
        assert_eq!(c.given_overrides, ["common"]);
        assert!(c.no_write_lock_file);
        let c = installables(
            "copy --substitute-on-destination --no-check-sigs --to ssh://root@h /nix/store/abc-x",
        );
        assert_eq!(c.installables, ["/nix/store/abc-x"]);
    }

    #[test]
    fn after_double_dash_and_program_arguments() {
        let c = installables("flake archive --json --no-write-lock-file -- .");
        assert_eq!(
            (c.insert_at, c.installables.as_slice()),
            (2, &[".".to_string()][..])
        );
        let c = installables("run .#app -- --commit-lock-file");
        assert_eq!(c.installables, [".#app"]);
        assert!(c.lock_writing_flags.is_empty());
        let c = installables("run .#app arg --recreate-lock-file");
        assert_eq!(c.installables, [".#app"]);
        assert!(c.lock_writing_flags.is_empty());
        let c = installables("shell .#a .#b --command nix flake lock");
        assert_eq!(c.installables, [".#a", ".#b"]);
        let c = installables("develop -s K V -k X . -c --commit-lock-file");
        assert_eq!(c.installables, ["."]);
    }

    #[test]
    fn lock_writing_flags() {
        let c = installables(
            "build .#x --commit-lock-file --update-input common --output-lock-file /tmp/l --recreate-lock-file",
        );
        assert_eq!(
            c.lock_writing_flags,
            [
                "--commit-lock-file",
                "--update-input",
                "--output-lock-file",
                "--recreate-lock-file"
            ]
        );
        assert_eq!(c.installables, [".#x"]);
    }

    #[test]
    fn lock_commands() {
        assert_eq!(
            classify_nix(&args("flake lock")),
            NixCall::LockCommand { flake: ".".into() }
        );
        assert_eq!(
            classify_nix(&args("flake lock ../x --update-input common")),
            NixCall::LockCommand {
                flake: "../x".into()
            }
        );
        assert_eq!(
            classify_nix(&args("flake update common --flake /ws/c")),
            NixCall::LockCommand {
                flake: "/ws/c".into()
            }
        );
        assert_eq!(
            classify_nix(&args("flake update")),
            NixCall::LockCommand { flake: ".".into() }
        );
    }

    #[test]
    fn other_commands_pass() {
        for s in [
            "--version",
            "config show experimental-features",
            "store gc",
            "flake init",
            "profile list",
            "",
            "-- build",
        ] {
            assert_eq!(classify_nix(&args(s)), NixCall::Other, "{s}");
        }
    }

    #[test]
    fn subcommand_insert_positions() {
        assert_eq!(installables("flake check .").insert_at, 2);
        assert_eq!(installables("derivation show .#x").insert_at, 2);
        assert_eq!(installables("env shell .#x").insert_at, 2);
        assert_eq!(installables("-L --verbose flake show").insert_at, 4);
    }

    #[test]
    fn nh_lock_flags() {
        for s in [
            "os build -U common .",
            "os switch --update",
            "home switch -u",
            "os build --update-input=common",
            "os build -vu",
            "os build --commit-lock-file",
            "os build --recreate-lock-file",
            "os build -Ucommon",
        ] {
            assert!(nh_lock_flag(&args(s)).is_some(), "{s}");
        }
        for s in [
            "os build -H focalor .",
            "os build . -- --update-input common",
            "os switch -e sudo",
            "os build -sfoo-u",
            "search --update-input-like hello",
            "clean all -k 3",
        ] {
            assert_eq!(nh_lock_flag(&args(s)), None, "{s}");
        }
    }
}

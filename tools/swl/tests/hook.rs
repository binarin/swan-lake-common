//! `swl hook <shell>`: the function it prints, run in zsh and bash against
//! `swl start` on a disposable forge, and the shells it refuses.

mod support;

use std::{fs, process::Command};

use support::{Forge, failed, find_on_path, stdout};

#[test]
fn hook_refuses_a_shell_other_than_zsh_and_bash() {
    let out = Command::new(env!("CARGO_BIN_EXE_swl"))
        .args(["hook", "fish"])
        .output()
        .unwrap();
    failed(&out, 2, "invalid value 'fish'");
    assert!(out.stdout.is_empty());
}

/// Runs `script` in `shell` without its rc files, in the forge's `work`,
/// with this swl first on PATH and the forge's `tmp` as TMPDIR.
fn run_in(f: &Forge, shell: &str, script: &str) -> String {
    let bin = f.root.join("swlbin");
    if !bin.exists() {
        fs::create_dir_all(&bin).unwrap();
        std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_swl"), bin.join("swl")).unwrap();
    }
    let tmp = f.root.join("tmp");
    fs::create_dir_all(&tmp).unwrap();
    let path = format!(
        "{}:{}:{}",
        bin.display(),
        f.root.join("fakebin").display(),
        f.base_path
    );
    let mut c = Command::new(find_on_path(&f.base_path, shell));
    f.env(&mut c, &path);
    match shell {
        "zsh" => c.arg("-f"),
        _ => c.args(["--norc", "--noprofile"]),
    };
    let work = f.root.join("work");
    stdout(
        &c.arg("-c")
            .arg(script)
            .current_dir(&work)
            .env("TMPDIR", &tmp)
            .output()
            .unwrap(),
    )
}

/// The temporary files the function left in the forge's `tmp`.
fn left_in_tmp(f: &Forge) -> Vec<std::path::PathBuf> {
    fs::read_dir(f.root.join("tmp"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect()
}

#[test]
fn hook_function_changes_to_the_new_workspace_and_passes_the_exit_status_through() {
    for shell in ["zsh", "bash"] {
        let f = Forge::new(&format!("hook-{shell}"));
        let work = f.root.join("work");
        fs::create_dir_all(&work).unwrap();
        let consumer = work.join("SL-0001-Foo/swan-lake");
        let spaced = work.join("with space/swan-lake");
        let script = format!(
            r#"eval "$(swl hook {shell})" || exit 9
typeset -f swl >/dev/null || exit 8
spaced_ws="$PWD/with space"
swl start SL-0002 swan-lake-common@lib 2>/dev/null
echo "status=$? pwd=$PWD"
swl start SL-0001-Foo swan-lake >/dev/null
echo "status=$? pwd=$PWD cd_file=${{SWL_CD_FILE-unset}}"
swl --version >/dev/null
echo "status=$? pwd=$PWD"
swl start SL-0003-Bar swan-lake --workspace "$spaced_ws" >/dev/null
echo "status=$? pwd=$PWD"
"#
        );
        let out = run_in(&f, shell, &script);
        assert_eq!(
            out.lines().collect::<Vec<_>>(),
            [
                format!("status=2 pwd={}", work.display()),
                format!("status=0 pwd={} cd_file=unset", consumer.display()),
                format!("status=0 pwd={}", consumer.display()),
                format!("status=0 pwd={}", spaced.display()),
            ],
            "{shell}: {out}"
        );
        assert!(consumer.join("flake.nix").is_file());
        assert!(spaced.join("flake.nix").is_file());
        // Each call removed its temporary file.
        let left = left_in_tmp(&f);
        assert!(left.is_empty(), "{shell}: {left:?}");
    }
}

/// A failing swl does not stop the function before it removes its temporary
/// file when the shell has `set -e`, an ERR trap that exits (inherited by
/// functions: bash's `set -E`) or, in zsh, `ERR_RETURN`: the function
/// returns swl's status, which then stops the shell as usual.
#[test]
fn hook_function_cleans_up_under_set_e_err_traps_and_err_return() {
    for shell in ["zsh", "bash"] {
        let f = Forge::new(&format!("hook-errexit-{shell}"));
        fs::create_dir_all(f.root.join("work")).unwrap();
        let errtrace = if shell == "bash" { "set -E; " } else { "" };
        let mut script = format!(
            r#"eval "$(swl hook {shell})" || exit 9
(set -e; swl start SL-0002 swan-lake-common@lib 2>/dev/null; echo not stopped)
echo "set -e status=$?"
({errtrace}trap 'exit 3' ERR; swl start SL-0002 swan-lake-common@lib 2>/dev/null; echo not stopped)
echo "ERR trap status=$?"
"#
        );
        let mut expected = vec!["set -e status=2", "ERR trap status=3"];
        if shell == "zsh" {
            script.push_str(
                r#"(setopt err_return; swl start SL-0002 swan-lake-common@lib 2>/dev/null; echo not stopped)
echo "err_return status=$?"
"#,
            );
            expected.push("err_return status=2");
        }
        let out = run_in(&f, shell, &script);
        assert_eq!(out.lines().collect::<Vec<_>>(), expected, "{shell}: {out}");
        let left = left_in_tmp(&f);
        assert!(left.is_empty(), "{shell}: {left:?}");
    }
}

/// When `mktemp` fails (here a shell function in its place), the function
/// still runs swl, without a file to read, and passes its status through.
#[test]
fn hook_function_runs_swl_when_mktemp_fails() {
    for shell in ["zsh", "bash"] {
        let f = Forge::new(&format!("hook-mktemp-{shell}"));
        let work = f.root.join("work");
        fs::create_dir_all(&work).unwrap();
        let script = format!(
            r#"eval "$(swl hook {shell})" || exit 9
mktemp() {{ return 1; }}
swl start SL-0002 swan-lake-common@lib 2>/dev/null
echo "status=$? pwd=$PWD"
swl start SL-0001-Foo swan-lake >/dev/null 2>&1
echo "status=$? pwd=$PWD"
"#
        );
        let out = run_in(&f, shell, &script);
        assert_eq!(
            out.lines().collect::<Vec<_>>(),
            [
                format!("status=2 pwd={}", work.display()),
                format!("status=0 pwd={}", work.display()),
            ],
            "{shell}: {out}"
        );
        assert!(work.join("SL-0001-Foo/swan-lake/flake.nix").is_file());
    }
}

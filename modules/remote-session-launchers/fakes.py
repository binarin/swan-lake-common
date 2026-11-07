"""Fake local and remote executables for the launch-chain checks.

The role is the name the script is invoked as. Every fake appends its argv
as one JSON line to $FAKE_LOG.
"""

import json
import os
import sys

role = os.path.basename(sys.argv[0])
args = sys.argv[1:]


def log(record):
    with open(os.environ["FAKE_LOG"], "a") as f:
        f.write(json.dumps(record) + "\n")


def exec_after(marker, **env):
    rest = args[args.index(marker) + 1 :]
    os.execve(rest[0], rest, dict(os.environ, **env))


def ssh():
    # OpenSSH: options, then the host; options after the host are parsed
    # again unless "--" came first; the remaining words are joined with
    # spaces and given to the remote login shell.
    with_value = set("BbcDEeFIiJLlmOoPpQRSWw")
    i = 0
    terminated = False

    def parse_options():
        nonlocal i, terminated
        while i < len(args):
            a = args[i]
            if a == "--":
                terminated = True
                i += 1
                return
            if not a.startswith("-") or a == "-":
                return
            for j, c in enumerate(a[1:], 1):
                if c in with_value:
                    if j == len(a) - 1:
                        i += 1
                    break
            i += 1

    parse_options()
    host = args[i]
    i += 1
    if not terminated:
        parse_options()
    command = " ".join(args[i:])
    log(["ssh", host] + ([command] if command else []))
    if not command:
        return
    shell = os.environ["FAKE_REMOTE_SHELL"]
    env = dict(os.environ, PATH=os.environ["FAKE_REMOTE_PATH"], HOME="/remote-home")
    os.execve(shell, [shell, "-c", command], env)


def niri():
    if args[:2] == ["msg", "--json"]:
        windows = os.environ.get("FAKE_NIRI_WINDOWS")
        if windows is None:
            sys.exit("niri: not running")
        print(windows)
        return
    log(["niri"] + args)
    sys.exit(int(os.environ.get("FAKE_NIRI_FOCUS_STATUS", "0")))


if role == "ssh":
    ssh()
elif role == "niri":
    niri()
elif role == "foot":
    log(["foot"] + args)
    exec_after("-e", TERM="foot")
elif role == "uwsm":
    log(["uwsm"] + args)
    exec_after("--")
else:
    # Remote programs: tmux, herdr, nix, prog, mosh-server.
    log([role] + args)

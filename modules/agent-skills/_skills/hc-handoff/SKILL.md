---
name: hc-handoff
description: Hand this conversation off to an independent agent session in a new Herdr tab, e.g. to continue in another directory or worktree. Use when the user asks to hand off, move or continue the session elsewhere, or fork the conversation into a new tab or terminal.
---

A handoff starts an independent agent in a visible tab of the user's Herdr
desktop window. It is not a hidden child or a background-only terminal.

## Find the caller's core and space

```sh
printf 'socket=%s terminal=%s\n' "${HC_TERMINAL_SOCKET:-}" "${HC_TERMINAL_ID:-}"
hc get "$HC_TERMINAL_ID" --json
hc schema
```

`HC_TERMINAL_SOCKET` identifies the local core. The caller's terminal labels
`herdr.space` and `herdr.tab` identify its desktop location. This is enough
for the local-core recipe below, even on a host reached over SSH. An absent
`HC_SESSION` does **not** mean the desktop is unavailable. Do not require
SSH back to the desktop or an app control socket when the core holds the
layout.

The desktop stores its topology in the core's `herdr-app` document. The
bundled helper reads it with `doc.get`, confirms the caller's space and tab,
and uses version-checked `doc.set` to preserve concurrent desktop edits.
Inspect the installed `hc schema` for compatibility; do not directly edit
the core's files on disk.

## Launch and register a visible tab

Resolve a requested model with `pi --list-models` on the execution host.
Choose a unique terminal/tab name, target directory and first message. The
message states that this is an independent continuation, its assigned work,
and its next step. It must not ask the new session to report to the launcher.

Use `scripts/handoff.py`, resolved relative to this skill's directory (not
the project working directory). It requires Python 3, `hc` and `pi`, uses
the caller's local core, and creates the pi session on the same host.
Set these environment variables before invoking it:

- `HANDOFF_NAME`: unique terminal/tab name.
- `HANDOFF_DIR`: absolute target working directory.
- `HANDOFF_TASK`: the first message.
- `HANDOFF_MODEL`: resolved provider/model ID.

It also reads the inherited `HC_TERMINAL_SOCKET`, `HC_TERMINAL_ID` and
`PI_SESSION_FILE`. Run:

```sh
python3 <absolute-skill-directory>/scripts/handoff.py
```

The helper forks the conversation with `pi --fork`, prints the created
terminal/tab/space IDs, then registers and verifies the tab. Labels alone
do not create a tab: both terminal labels and the layout entry are required.
The app observes the document update and shows the new tab in the caller's
space; this does not require switching the user's focus.

A failure after terminal creation can leave a hidden live agent. Retain the
printed terminal/tab IDs, inspect `doc.get` and `hc get`, and either finish
registering that same terminal or stop it and report the failure. A transport
timeout can occur after a successful write: inspect before retrying. Never
launch a duplicate agent just because its tab has not appeared yet.

If these APIs or the expected layout are unavailable, inspect the installed
interface. A verified desktop instance's `tab.new` command is another option,
but do not guess `herdr-<instance>` or use an unrelated `herdr` TUI binary.
Ask for missing connection information only after checking the local core.
Never start another server/window to work around a failed handoff.

## Verify startup and report

1. Confirm the layout entry points to the created terminal in the intended
   space. Inspect `hc get <terminal> --json`: the desktop labels must match,
   `parent` must be null and `die_with_parent` false.
2. Use `hc wait <terminal> --timeout 120`, then
   `hc read <terminal> --lines 40` on the execution host. Startup idle is not
   proof the task ran: verify the delivered message and the agent's response
   or active investigation. A timeout is not permission to duplicate it.
3. Tell the human the tab name. If visibility remains uncertain, distinguish
   verified core topology from confirmation that the desktop rendered it.

For an explicitly requested batch, verify the first tab before launching
the rest, then register and verify each agreed group. Keep ownership
disjoint. When replacing old sessions, stop and confirm those specified
sessions before relaunching. Do not close unrelated tabs or the smoke test.

After launching/verifying the requested handoff or batch, stop working on
the handed-off tasks here. If the human also explicitly requested a skill
fix or other distinct launcher work, finish that work without touching the
new sessions' tickets or product files.

## Context and fallback boundaries

- Never use `pi --session "$PI_SESSION_FILE"`: two live processes would write
  the same session. Never silently substitute a fresh context for a fork.
- For a different agent, use its native fork mechanism and session ID
  (Claude Code: `claude --resume <session-id> --fork-session`).
- `hc run --bg --no-parent` without registering a desktop tab is **hidden**.
  Use that only when the human explicitly requests or accepts hidden
  topology; agreement to a ticket split or model is not such acceptance.
  State plainly that it created no desktop tab and give `hc attach <name>`.

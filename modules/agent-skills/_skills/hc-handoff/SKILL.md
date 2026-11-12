---
name: hc-handoff
description: Hand this conversation off to an independent agent session in a new Herdr tab, e.g. to continue in another directory or worktree. Use when the user asks to hand off, move or continue the session elsewhere, or fork the conversation into a new tab or terminal.
---

A handoff starts a new agent that carries this conversation and then runs on its own in a tab of the user's Herdr window. It is not your child: it does not report to you, and it does not end when you end. This is the opposite of delegation (`hc --skill`), where the helper is your hidden child and reports back.

Ask the window for a new tab in your own space, running a fork of your session:

```sh
space=$(hc get "$HC_TERMINAL_ID" --json | jq -r '.labels["herdr.space"]')
herdr-"$HC_SESSION" do tab.new --space "$space" --name <tab-name> \
  --folder <target-dir> \
  --argv "$(jq -nc --arg f "$PI_SESSION_FILE" '["pi","--fork",$f]')" \
  --task "<first message>"
```

- **Pick the right window.** Several Herdr instances can run on one machine, each with its own hc session. Instance `<name>` is the `herdr-<name>` binary and sets `HC_SESSION=<name>`, so `herdr-"$HC_SESSION"` is the window you run in. Never use plain `herdr-app`: it targets a different home, so it talks to the wrong window or opens a new one.
- **Your space** is the `herdr.space` label on your own terminal. Without `--space`, the tab goes to whichever space is selected, which may not be yours.
- **Independence:** tabs the window creates have no parent (`hc get` shows `parent: null`, `die_with_parent: false`), so the new agent outlives you and sends you no notes.
- **Context:** `pi --fork` copies the conversation, up to and including this launch, into a new session. The fork lands in `--folder`, and your own session file stays untouched.
- **First message:** `--task` is delivered once pi is ready. Say that it continues this conversation, where it now works, and the next step. Don't ask it to report to you; there is no "you" after the handoff.

The reply prints `{space, tab, terminal}`, and the new tab becomes the active one. Its terminal is a shell that runs pi, so when pi exits the tab drops to a shell instead of closing.

Not a handoff:

- `hc run` without `--no-parent`: that makes a hidden child, which ends when you end and reports to you.
- `hc run --no-parent` alone: the agent is independent but hidden. A terminal appears in the window only with a `herdr.space` label, and `tab.new` sets that label for you.
- `pi --session "$PI_SESSION_FILE"`: two live processes then write the same session file.
- `pi` without `--fork`: the new agent starts without the conversation.

After launching:

1. Confirm it started: `hc wait <terminal> --timeout 120` until it finishes the first turn, then `hc read <terminal> --lines 40` to see that pi took the task.
2. Tell the user the tab name. It is already on screen.
3. Stop working in this session. Don't edit files, run commands, or follow up on the handed-off work here; the user continues exclusively in the new tab. Two agents working on the same thing get in each other's way.

If you don't run inside a Herdr window (no `herdr.space` label or no `$HC_SESSION`), start it hidden and let the user attach:

```sh
hc run --bg --no-parent --name <name> --cwd <target-dir> \
  -- pi --fork "$PI_SESSION_FILE" "<first message>"
```

Then tell the user to run `hc attach <name>`.

The recipe is for pi, which sets `$PI_SESSION_FILE`. Another agent needs its own CLI's fork option and session ID in `--argv` (Claude Code: `["claude","--resume","<session-id>","--fork-session"]`). If you can't find your session ID, tell the user instead of starting a new agent without the context.

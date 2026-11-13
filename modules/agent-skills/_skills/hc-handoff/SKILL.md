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

The desktop stores its topology in the core's `herdr-app` document. Read it
through the documented `doc.get` RPC. Confirm that the caller's space and tab
exist there before launching. Inspect the installed `hc schema` for the RPC
contract; do not directly edit the core's files on disk. Use version-checked
`doc.set` so a concurrent desktop edit is not overwritten.

## Launch and register a visible tab

Resolve a requested model with `pi --list-models` on the execution host.
Choose a unique terminal/tab name, target directory and first message. The
message states that this is an independent continuation, its assigned work,
and its next step. It must not ask the new session to report to the launcher.

The following recipe uses the caller's local core and creates the pi session
on the same host. Set `HANDOFF_NAME`, `HANDOFF_DIR`, `HANDOFF_TASK`, and
`HANDOFF_MODEL` in the environment first (model is a resolved provider/id).
It preserves the conversation with `pi --fork`, not a shared writable session.

```sh
python3 - <<'PY'
import json
import os
import socket
import subprocess
import uuid

name = os.environ['HANDOFF_NAME']
directory = os.environ['HANDOFF_DIR']
task = os.environ['HANDOFF_TASK']
model = os.environ['HANDOFF_MODEL']
session = os.environ['PI_SESSION_FILE']
assert os.path.isfile(session), 'Missing session context; do not launch fresh'

sock = socket.socket(socket.AF_UNIX)
sock.settimeout(15)
sock.connect(os.environ['HC_TERMINAL_SOCKET'])
stream = sock.makefile('rwb')
sequence = 0

def rpc(method, params):
    global sequence
    sequence += 1
    request = dict(jsonrpc='2.0', id=sequence, method=method, params=params)
    stream.write((json.dumps(request) + '\n').encode())
    stream.flush()
    while True:
        line = stream.readline()
        if not line:
            raise RuntimeError('Core disconnected; inspect state before retrying')
        reply = json.loads(line)
        if reply.get('id') == sequence:
            if 'error' in reply:
                raise RuntimeError(reply['error'])
            return reply['result']

caller = json.loads(subprocess.check_output(
    ['hc', 'get', os.environ['HC_TERMINAL_ID'], '--json']))
space_id = caller['labels']['herdr.space']
caller_tab = caller['labels']['herdr.tab']
layout = rpc('doc.get', {'name': 'herdr-app'})
space = next(s for s in layout['value']['spaces'] if s['id'] == space_id)
assert any(t['id'] == caller_tab for t in space['tabs'])

tab_id = 'tb' + uuid.uuid4().hex
pane_id = 'p' + uuid.uuid4().hex
created = json.loads(subprocess.check_output([
    'hc', 'run', '--bg', '--no-parent', '--keep', '--name', name,
    '--cwd', directory,
    '--label', 'herdr.space=' + space_id,
    '--label', 'herdr.tab=' + tab_id,
    '--', 'pi', '--fork', session, '--model', model, '--name', name, task,
], text=True))
terminal = created['address']
print(json.dumps(dict(terminal=terminal, tab=tab_id, space=space_id)), flush=True)
new_tab = dict(id=tab_id, name=name, root=dict(
    id=tab_id, type='row', children=[dict(
        id=pane_id, type='block', content=dict(terminal=terminal))]))

# Retry only explicit version conflicts, always against a fresh document.
for attempt in range(5):
    layout = rpc('doc.get', {'name': 'herdr-app'})
    space = next(s for s in layout['value']['spaces'] if s['id'] == space_id)
    if any(t['id'] == tab_id for t in space['tabs']):
        break
    space['tabs'].append(new_tab)
    try:
        rpc('doc.set', dict(name='herdr-app', value=layout['value'],
                            version=layout['version']))
        break
    except RuntimeError as error:
        if 'conflict' not in str(error) or attempt == 4:
            raise

layout = rpc('doc.get', {'name': 'herdr-app'})
space = next(s for s in layout['value']['spaces'] if s['id'] == space_id)
assert next(t for t in space['tabs'] if t['id'] == tab_id) == new_tab
print('Verified desktop tab:', name, terminal, flush=True)
PY
```

Labels alone do not create a tab: both terminal labels and the layout entry
are required. The app observes the document update and shows the new tab
in the caller's space; this does not require switching the user's focus.

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

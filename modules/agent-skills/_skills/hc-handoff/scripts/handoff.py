#!/usr/bin/env python3
"""Fork pi into a visible tab using the caller's local Herdr core."""

import json
import os
import socket
import subprocess
import sys
import time
import uuid

STARTUP_TIMEOUT = 60


def verify_startup(terminal):
    """Poll until the agent has started its first turn, then show its screen.

    Never waits for the turn to end: a working agent is the success case.
    """
    deadline = time.monotonic() + STARTUP_TIMEOUT
    while True:
        info = json.loads(subprocess.check_output(
            ['hc', 'get', terminal, '--json']))
        state = (info.get('status') or {}).get('state')
        started = (info.get('turn') or 0) > 0 or state in ('working', 'done',
                                                       'blocked')
        if info['state'] != 'live' or started or state == 'error':
            break
        if time.monotonic() > deadline:
            break
        time.sleep(1)
    print(subprocess.check_output(
        ['hc', 'read', terminal, '--lines', '40'], text=True), flush=True)
    summary = 'terminal=%s state=%s status=%s turn=%s' % (
        terminal, info['state'], state, info.get('turn'))
    if info['state'] == 'live' and started:
        print('Startup confirmed:', summary, flush=True)
        return 0
    print('Startup NOT confirmed:', summary, '- inspect it; do not launch'
          ' a duplicate', flush=True)
    return 3


if len(sys.argv) == 3 and sys.argv[1] == '--verify':
    sys.exit(verify_startup(sys.argv[2]))

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
sys.exit(verify_startup(terminal))

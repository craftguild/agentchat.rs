#!/usr/bin/env python3
"""Observe real hook stages without advancing any AgentChat delivery cursor."""
import fcntl
import json
import os
from pathlib import Path
import subprocess
import sys
import time
import uuid

ROOT = Path(sys.argv[1]).resolve()
ACTION = sys.argv[2]


def record(kind, data):
    with (ROOT / 'events.jsonl').open('a') as stream:
        fcntl.flock(stream, fcntl.LOCK_EX)
        stream.write(json.dumps({'time': time.time(), 'kind': kind, 'data': data}) + '\n')
        stream.flush()


raw = sys.stdin.buffer.read(1_048_577)
if len(raw) > 1_048_576:
    raise SystemExit(1)
event = json.loads(raw)
if ACTION == 'log':
    record(event['kind'], event['data'])
    print('{}')
    raise SystemExit(0)

manifest = json.loads((ROOT / 'manifest.json').read_text())


def original_owner():
    try:
        captured = json.loads((ROOT / 'owner.json').read_text())
        result = subprocess.run(
            [manifest['identity_helper'], 'identity', str(captured['pid'])],
            capture_output=True, timeout=3, check=True,
        )
        return json.loads(result.stdout) == captured
    except (OSError, ValueError, subprocess.SubprocessError):
        return False


record('hook_enter', event)
permit = event.get('agentchat_body_probe', {})
if event.get('session_id') != manifest['session'] or permit.get('root') != manifest['session']:
    record('hook_refused_root', {})
    raise SystemExit(0)
if not permit.get('allowed') or not original_owner():
    record('hook_refused_controls_or_owner', {})
    raise SystemExit(0)

with (ROOT / 'offer.lock').open('a') as lock:
    try:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except BlockingIOError:
        record('hook_coalesced_busy', {})
        raise SystemExit(0)
    control = json.loads((ROOT / 'control.json').read_text())
    sequence = control.get('head')
    if sequence is None:
        record('hook_empty', {})
        raise SystemExit(0)
    payload = json.loads((ROOT / 'payloads' / f'{sequence:020d}.json').read_text())
    attempt = str(uuid.uuid4())
    record('hook_read', {'payload': payload, 'attempt': attempt, 'permit': permit})
    if control.get('hold'):
        until = time.monotonic() + 45
        while not (ROOT / 'release').exists():
            if time.monotonic() > until or not original_owner():
                record('hook_hold_abandoned', {'attempt': attempt})
                raise SystemExit(0)
            time.sleep(0.05)
        mode = (ROOT / 'release').read_text().strip()
    else:
        mode = control.get('mode', 'deliver')
    if mode == 'fail':
        record('hook_failed_before_handoff', {'attempt': attempt})
        raise SystemExit(1)
    if not original_owner():
        record('hook_refused_lost_owner', {'attempt': attempt})
        raise SystemExit(0)
    text = (
        f'BODY_PROBE_NOTICE root={manifest["session"]} seq={sequence} attempt={attempt}\n'
        'This is an isolated full-body handoff test. Read the following complete body directly. '
        'Do not fetch it with tools. Follow its requested output only; do not restart previous work.\n'
        + json.dumps(payload, ensure_ascii=False)
    )
    record('hook_stderr_before_exit2', {'attempt': attempt, 'text': text})
    print(text, file=sys.stderr, flush=True)
    # No payload removal, consumer receipt or delivery cursor update here.
    raise SystemExit(2)

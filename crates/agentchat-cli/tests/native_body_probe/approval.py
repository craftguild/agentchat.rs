#!/usr/bin/env python3
"""Observe refusal and Esc while a real native foreground approval is pending."""
import json
from pathlib import Path
import sys
import time
import urllib.request

root = Path(sys.argv[1]).resolve(strict=True)
server = json.loads((root / 'terminal-control.json').read_text())


def terminal(action, **fields):
    data = json.dumps({'op': action, 'name': 'recipient', **fields}).encode()
    request = urllib.request.Request(f'http://127.0.0.1:{server["port"]}', data=data,
                                     headers={'Content-Type': 'application/json'})
    with urllib.request.urlopen(request, timeout=5) as response:
        return json.load(response)


def events():
    return [json.loads(line) for line in (root / 'events.jsonl').read_text().splitlines()]


def accepted_four(entries):
    return any(entry['kind'] == 'prompt_after' and
               'seq=4 ' in entry['data'].get('result', {}).get('text', '') for entry in entries)


assert 'Do you want to create approval-fence.txt?' in terminal('screen')['screen']
assert not (root / 'workspace/approval-fence.txt').exists()
started = time.time()
(root / 'control.json').write_text(json.dumps({'head': 4, 'hold': False, 'mode': 'deliver'}))
(root / 'workspace/.agentchat-body-probe-wake').write_text('8\n')
deadline = time.monotonic() + 5
while time.monotonic() < deadline:
    if any(entry['time'] >= started and entry['kind'] == 'hook_refused_controls_or_owner'
           for entry in events()):
        break
    time.sleep(.05)
else:
    raise SystemExit('No actual hook refusal during native approval')
window = time.time()
requests = sum(entry['kind'] == 'model_request' for entry in events())
deadline = time.monotonic() + 18
while time.monotonic() < deadline:
    entries = events()
    assert not accepted_four(entries)
    assert not (root / 'workspace/approval-fence.txt').exists()
    assert sum(entry['kind'] == 'model_request' for entry in entries) == requests
    time.sleep(.1)
assert 'Do you want to create approval-fence.txt?' in terminal('screen')['screen']
esc_time = time.time()
terminal('input', text='\x1b')
deadline = time.monotonic() + 8
while time.monotonic() < deadline:
    aborted = [entry for entry in events() if entry['time'] >= esc_time and
               ((entry['kind'] == 'tool_after' and entry['data'].get('aborted')) or
                (entry['kind'] == 'turn_complete' and entry['data']['event'].get('isAborted')))]
    if aborted:
        break
    time.sleep(.05)
else:
    raise SystemExit('Native Esc did not produce an observed abort')
pause_signal = time.time()
(root / 'workspace/.agentchat-body-probe-wake').write_text('9\n')
deadline = time.monotonic() + 5
while time.monotonic() < deadline:
    refused = [entry for entry in events() if entry['time'] >= pause_signal and
               entry['kind'] == 'hook_refused_controls_or_owner']
    if refused:
        break
    time.sleep(.05)
else:
    raise SystemExit('No actual hook refusal after Esc')
assert not accepted_four(events())
assert not (root / 'workspace/approval-fence.txt').exists()
report = {'started': started, 'approval_refusal_window': esc_time - window,
          'esc_time': esc_time, 'aborted': aborted, 'paused_refused': refused,
          'body4_accepted': False, 'target_created': False,
          'inflight_release_case': 'Earlier held hook expired before release; not verified'}
(root / 'approval-esc.json').write_text(json.dumps(report, indent=2) + '\n')
print(json.dumps(report, indent=2))
